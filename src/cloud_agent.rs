use anyhow::{bail, Context, Result};

const GITHUB_API_BASE: &str = "https://api.github.com";
const COPILOT_LOGIN: &str = "copilot";

/// Status of a cloud agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudAgentStatus {
    /// The agent is still working on the task.
    Working,
    /// The agent has created a PR.
    PrCreated { pr_number: u64, pr_url: String },
    /// The agent session completed or the issue was closed without a PR.
    CompletedNoPr,
}

/// Merge readiness of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrMergeStatus {
    /// The PR is a draft and must be marked ready for review first.
    Draft,
    /// The PR is not yet mergeable (checks pending, conflicts, etc.).
    NotMergeable,
    /// The PR is ready to be merged.
    Mergeable,
    /// The PR has already been merged.
    AlreadyMerged,
}

/// Result of triggering a cloud agent.
#[derive(Debug, Clone)]
pub struct TriggerResult {
    pub issue_number: u64,
    pub issue_url: String,
}

/// Client for interacting with cloud coding agents via the GitHub API.
///
/// Instead of running AI chat completions locally and trying to interpret text
/// responses as code changes, this client triggers a cloud coding agent (e.g.
/// GitHub Copilot Coding Agent) which can autonomously make code changes and
/// create pull requests.
pub struct CloudAgentClient {
    github_token: String,
    repo_owner: String,
    repo_name: String,
    http: reqwest::Client,
}

/// Check whether Copilot appears in an issue's assignees array.
fn is_copilot_in_assignees(issue: &serde_json::Value) -> bool {
    issue["assignees"]
        .as_array()
        .map(|arr| {
            arr.iter()
                .any(|a| a["login"].as_str() == Some(COPILOT_LOGIN))
        })
        .unwrap_or(false)
}

impl CloudAgentClient {
    pub fn new(
        github_token: String,
        repo_owner: String,
        repo_name: String,
    ) -> Self {
        Self {
            github_token,
            repo_owner,
            repo_name,
            http: reqwest::Client::new(),
        }
    }

    /// Trigger a cloud coding agent for the given task.
    ///
    /// Creates a GitHub issue with the task description and assigns Copilot to
    /// it, which triggers the Copilot coding agent to work on the task
    /// autonomously and create a pull request.
    pub async fn trigger_agent(
        &self,
        task_id: &str,
        task_description: &str,
    ) -> Result<TriggerResult> {
        let issue_body = format!(
            "{}\n\n---\n*Triggered by wreck-it cloud agent orchestrator (task `{}`)*",
            task_description, task_id,
        );

        let create_body = serde_json::json!({
            "title": format!("[wreck-it] {}", task_id),
            "body": issue_body,
            "labels": ["wreck-it", "copilot"],
        });

        let url = format!(
            "{}/repos/{}/{}/issues",
            GITHUB_API_BASE, self.repo_owner, self.repo_name,
        );

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&create_body)
            .send()
            .await
            .context("Failed to create GitHub issue")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create issue ({}): {}", status, body);
        }

        let issue: serde_json::Value = resp.json().await?;
        let issue_number = issue["number"]
            .as_u64()
            .context("Missing issue number in response")?;
        let issue_url = issue["html_url"]
            .as_str()
            .context("Missing issue URL in response")?
            .to_string();
        let issue_node_id = issue["node_id"].as_str().map(|s| s.to_string());
        if issue_node_id.is_none() {
            tracing::warn!(
                "Issue creation response for #{} did not include node_id; \
                 will need an extra API call to resolve it",
                issue_number,
            );
        }

        // Assign Copilot to the issue to trigger the coding agent.
        if !self
            .assign_copilot(issue_number, issue_node_id.as_deref())
            .await
        {
            tracing::warn!(
                "Copilot assignment failed for issue #{}; the issue was created but the \
                 agent may need to be triggered manually",
                issue_number,
            );
        }

        Ok(TriggerResult {
            issue_number,
            issue_url,
        })
    }

    /// Assign the Copilot bot to an issue, triggering the coding agent.
    /// Returns `true` if the assignment succeeded.
    async fn assign_copilot(&self, issue_number: u64, issue_node_id: Option<&str>) -> bool {
        self.try_assign_copilot(issue_number, issue_node_id).await
    }

    /// Look up a GitHub user's GraphQL node ID by login.
    async fn get_user_node_id(&self, login: &str) -> Option<String> {
        let url = format!("{}/users/{}", GITHUB_API_BASE, login);
        match self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["node_id"].as_str().map(|s| s.to_string())),
            Ok(resp) => {
                tracing::warn!("Failed to look up user '{}' ({})", login, resp.status());
                None
            }
            Err(e) => {
                tracing::warn!("HTTP error looking up user '{}': {}", login, e);
                None
            }
        }
    }

    /// Fetch the GraphQL node ID for an issue.
    async fn get_issue_node_id(&self, issue_number: u64) -> Option<String> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, issue_number,
        );
        match self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => resp
                .json::<serde_json::Value>()
                .await
                .ok()
                .and_then(|v| v["node_id"].as_str().map(|s| s.to_string())),
            Ok(resp) => {
                tracing::warn!(
                    "Failed to fetch issue #{} for node_id ({})",
                    issue_number,
                    resp.status(),
                );
                None
            }
            Err(e) => {
                tracing::warn!(
                    "HTTP error fetching issue #{} for node_id: {}",
                    issue_number,
                    e,
                );
                None
            }
        }
    }

    /// Attempt to assign Copilot using the GraphQL `replaceActorsForAssignable`
    /// mutation. Falls back to fetching the issue node ID if it was not provided.
    /// Returns `true` when the mutation succeeds.
    async fn try_assign_copilot(
        &self,
        issue_number: u64,
        issue_node_id: Option<&str>,
    ) -> bool {
        // Resolve the issue's GraphQL node ID.
        let owned_node_id;
        let assignable_id = match issue_node_id {
            Some(id) => id,
            None => {
                match self.get_issue_node_id(issue_number).await {
                    Some(id) => {
                        owned_node_id = id;
                        owned_node_id.as_str()
                    }
                    None => {
                        tracing::warn!(
                            "Could not resolve node_id for issue #{}",
                            issue_number,
                        );
                        return false;
                    }
                }
            }
        };

        // Look up the Copilot bot's GraphQL node ID.
        let actor_id = match self.get_user_node_id(COPILOT_LOGIN).await {
            Some(id) => id,
            None => {
                tracing::warn!(
                    "Could not look up node_id for user '{}'; \
                     unable to assign via GraphQL",
                    COPILOT_LOGIN,
                );
                return false;
            }
        };

        // Use the GraphQL replaceActorsForAssignable mutation.
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": r#"mutation($assignableId: ID!, $actorIds: [ID!]!) {
                replaceActorsForAssignable(input: {
                    assignableId: $assignableId,
                    actorIds: $actorIds
                }) {
                    assignable {
                        ... on Issue {
                            assignees(first: 10) {
                                nodes { login }
                            }
                        }
                    }
                }
            }"#,
            "variables": {
                "assignableId": assignable_id,
                "actorIds": [actor_id],
            },
        });

        match self
            .http
            .post(&graphql_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&query)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(gql_resp) = resp.json::<serde_json::Value>().await {
                    // Check for GraphQL-level errors.
                    if let Some(errors) = gql_resp.get("errors") {
                        tracing::warn!(
                            "GraphQL errors assigning Copilot to issue #{}: {}",
                            issue_number,
                            errors,
                        );
                        return false;
                    }

                    // Verify Copilot appears in the assignees from the mutation
                    // response.
                    let has_copilot = gql_resp
                        .pointer(
                            "/data/replaceActorsForAssignable/assignable/assignees/nodes",
                        )
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter()
                                .any(|a| a["login"].as_str() == Some(COPILOT_LOGIN))
                        })
                        .unwrap_or(false);

                    if has_copilot {
                        tracing::info!(
                            "Assigned Copilot to issue #{} via GraphQL – coding agent triggered",
                            issue_number,
                        );
                        return true;
                    }
                }
                tracing::warn!(
                    "Copilot not found in assignees for issue #{} after GraphQL mutation; \
                     the token may lack permission to assign Copilot (a PAT may be required)",
                    issue_number,
                );
                false
            }
            Ok(resp) => {
                tracing::warn!(
                    "Failed to assign Copilot to issue #{} via GraphQL ({}); \
                     agent may need manual trigger",
                    issue_number,
                    resp.status(),
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    "HTTP error assigning Copilot to issue #{} via GraphQL: {}",
                    issue_number,
                    e,
                );
                false
            }
        }
    }

    /// Check the status of an agent session by looking for PRs that reference
    /// the triggering issue.
    pub async fn check_agent_status(&self, issue_number: u64) -> Result<CloudAgentStatus> {
        // Look for open PRs that reference this issue.
        if let Some(status) = self.find_linked_pr(issue_number).await? {
            return Ok(status);
        }

        // Check if the issue has been closed (agent completed without PR).
        let issue_url = format!(
            "{}/repos/{}/{}/issues/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, issue_number,
        );

        let resp = self
            .http
            .get(&issue_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to fetch issue status")?;

        if resp.status().is_success() {
            let issue: serde_json::Value = resp.json().await?;
            if issue["state"].as_str() == Some("closed") {
                return Ok(CloudAgentStatus::CompletedNoPr);
            }

            // Verify Copilot is actually assigned to the issue.  If the token
            // changed or lacks permissions the assignment may have been lost,
            // leaving the issue open but with no agent working on it.
            if !is_copilot_in_assignees(&issue) {
                tracing::warn!(
                    "Copilot is not assigned to issue #{}; attempting to reassign",
                    issue_number,
                );
                if self.assign_copilot(issue_number, None).await {
                    tracing::info!("Successfully reassigned Copilot to issue #{}", issue_number,);
                } else {
                    tracing::warn!(
                        "Failed to reassign Copilot to issue #{}; \
                         the agent may need to be triggered manually",
                        issue_number,
                    );
                }
            }
        }

        Ok(CloudAgentStatus::Working)
    }

    /// Search for open PRs that reference the given issue number.
    async fn find_linked_pr(&self, issue_number: u64) -> Result<Option<CloudAgentStatus>> {
        // Use the issue timeline to find cross-referenced PRs.
        let url = format!(
            "{}/repos/{}/{}/issues/{}/timeline",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, issue_number,
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github.mockingbird-preview+json")
            .send()
            .await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                if let Ok(events) = resp.json::<Vec<serde_json::Value>>().await {
                    // Walk events in reverse to find the most recent PR reference.
                    for event in events.iter().rev() {
                        if event["event"].as_str() == Some("cross-referenced") {
                            if let Some(source) = event.get("source") {
                                let issue_obj = source.get("issue");
                                let is_pr = issue_obj.and_then(|i| i.get("pull_request")).is_some();
                                if is_pr {
                                    if let Some(pr_number) =
                                        issue_obj.and_then(|i| i["number"].as_u64())
                                    {
                                        let pr_url = issue_obj
                                            .and_then(|i| i["html_url"].as_str())
                                            .unwrap_or("")
                                            .to_string();
                                        return Ok(Some(CloudAgentStatus::PrCreated {
                                            pr_number,
                                            pr_url,
                                        }));
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        Ok(None)
    }

    /// Check the merge readiness of a PR in a single API call.
    ///
    /// Returns [`PrMergeStatus::Draft`] if the PR is still a draft,
    /// [`PrMergeStatus::NotMergeable`] if it is not yet mergeable (e.g. checks
    /// pending or conflicts), and [`PrMergeStatus::Mergeable`] when it is ready.
    pub async fn check_pr_merge_status(&self, pr_number: u64) -> Result<PrMergeStatus> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, pr_number,
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to fetch PR details")?;

        if !resp.status().is_success() {
            return Ok(PrMergeStatus::NotMergeable);
        }

        let pr: serde_json::Value = resp.json().await?;
        let state = pr["state"].as_str().unwrap_or("unknown");
        let draft = pr["draft"].as_bool().unwrap_or(false);
        let mergeable = pr["mergeable"].as_bool().unwrap_or(false);
        let merged = pr["merged"].as_bool().unwrap_or(false);

        if merged {
            return Ok(PrMergeStatus::AlreadyMerged);
        }
        if state != "open" {
            return Ok(PrMergeStatus::NotMergeable);
        }
        if draft {
            return Ok(PrMergeStatus::Draft);
        }
        if mergeable {
            Ok(PrMergeStatus::Mergeable)
        } else {
            Ok(PrMergeStatus::NotMergeable)
        }
    }

    /// Mark a draft PR as ready for review.
    ///
    /// The REST API does not support unsetting `draft`; the GraphQL mutation
    /// `markPullRequestReadyForReview` is required instead.
    pub async fn mark_pr_ready_for_review(&self, pr_number: u64) -> Result<()> {
        // First, fetch the PR to obtain the GraphQL node_id.
        let pr_url = format!(
            "{}/repos/{}/{}/pulls/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, pr_number,
        );

        let pr_resp = self
            .http
            .get(&pr_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to fetch PR for node_id")?;

        if !pr_resp.status().is_success() {
            let status = pr_resp.status();
            let body = pr_resp.text().await.unwrap_or_default();
            bail!(
                "Failed to fetch PR #{} for node_id ({}): {}",
                pr_number,
                status,
                body,
            );
        }

        let pr: serde_json::Value = pr_resp.json().await?;
        let node_id = pr["node_id"]
            .as_str()
            .context("Missing node_id in PR response")?;

        // Use the GraphQL API to mark the PR as ready for review.
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": "mutation($prId: ID!) { markPullRequestReadyForReview(input: { pullRequestId: $prId }) { pullRequest { isDraft } } }",
            "variables": { "prId": node_id },
        });

        let resp = self
            .http
            .post(&graphql_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&query)
            .send()
            .await
            .context("Failed to call GraphQL markPullRequestReadyForReview")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "GraphQL request failed for PR #{} ({}): {}",
                pr_number,
                status,
                body,
            );
        }

        let gql_resp: serde_json::Value = resp.json().await?;

        // Check for GraphQL-level errors.
        if let Some(errors) = gql_resp.get("errors") {
            bail!(
                "GraphQL errors marking PR #{} as ready for review: {}",
                pr_number,
                errors,
            );
        }

        // Verify the mutation result.
        let is_draft = gql_resp
            .pointer("/data/markPullRequestReadyForReview/pullRequest/isDraft")
            .and_then(|v| v.as_bool());
        match is_draft {
            Some(false) => {} // success
            Some(true) => bail!(
                "PR #{} is still a draft after markPullRequestReadyForReview",
                pr_number,
            ),
            None => bail!(
                "Unexpected GraphQL response for PR #{}: could not verify draft status: {}",
                pr_number,
                gql_resp,
            ),
        }

        tracing::info!("Marked PR #{} as ready for review", pr_number);
        Ok(())
    }

    /// Merge a pull request using a squash merge.
    pub async fn merge_pr(&self, pr_number: u64) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/merge",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, pr_number,
        );

        let body = serde_json::json!({
            "merge_method": "squash",
        });

        let resp = self
            .http
            .put(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
            .context("Failed to merge PR")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to merge PR #{} ({}): {}", pr_number, status, body);
        }

        tracing::info!("Merged PR #{}", pr_number);
        Ok(())
    }
}

/// Parse a GitHub remote URL into (owner, repo).
///
/// Handles both HTTPS (`https://github.com/owner/repo.git`) and SSH
/// (`git@github.com:owner/repo.git`) formats.
pub fn parse_github_remote(url: &str) -> Option<(String, String)> {
    let path = url
        .strip_prefix("https://github.com/")
        .or_else(|| url.strip_prefix("git@github.com:"))?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    let path = path.trim_end_matches('/');
    let mut parts = path.splitn(2, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    // Ignore any trailing path segments (e.g. ".../tree/main").
    let repo = repo.split('/').next().unwrap_or(repo);
    Some((owner.to_string(), repo.to_string()))
}

/// Resolve the GitHub repository owner and name.
///
/// Prefers explicit config values; falls back to parsing the `origin` git
/// remote in the given working directory.
pub fn resolve_repo_info(
    repo_owner: Option<&str>,
    repo_name: Option<&str>,
    work_dir: &std::path::Path,
) -> Result<(String, String)> {
    if let (Some(owner), Some(name)) = (repo_owner, repo_name) {
        return Ok((owner.to_string(), name.to_string()));
    }

    let output = std::process::Command::new("git")
        .args(["remote", "get-url", "origin"])
        .current_dir(work_dir)
        .output()
        .context("Failed to run `git remote get-url origin`")?;

    let remote_url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    parse_github_remote(&remote_url)
        .context("Could not determine repo owner/name from git remote URL")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_github_remote_https() {
        let (owner, repo) = parse_github_remote("https://github.com/octocat/hello-world.git")
            .expect("should parse HTTPS URL");
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "hello-world");
    }

    #[test]
    fn parse_github_remote_https_no_git_suffix() {
        let (owner, repo) = parse_github_remote("https://github.com/octocat/hello-world")
            .expect("should parse HTTPS URL without .git");
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "hello-world");
    }

    #[test]
    fn parse_github_remote_ssh() {
        let (owner, repo) = parse_github_remote("git@github.com:octocat/hello-world.git")
            .expect("should parse SSH URL");
        assert_eq!(owner, "octocat");
        assert_eq!(repo, "hello-world");
    }

    #[test]
    fn parse_github_remote_invalid() {
        assert!(parse_github_remote("https://gitlab.com/foo/bar").is_none());
        assert!(parse_github_remote("not-a-url").is_none());
        assert!(parse_github_remote("https://github.com/").is_none());
    }

    #[test]
    fn cloud_agent_client_constructs() {
        let client = CloudAgentClient::new(
            "test-token".to_string(),
            "owner".to_string(),
            "repo".to_string(),
        );
        assert_eq!(client.repo_owner, "owner");
        assert_eq!(client.repo_name, "repo");
    }

    #[test]
    fn cloud_agent_status_variants() {
        let working = CloudAgentStatus::Working;
        assert_eq!(working, CloudAgentStatus::Working);

        let pr = CloudAgentStatus::PrCreated {
            pr_number: 42,
            pr_url: "https://github.com/o/r/pull/42".to_string(),
        };
        assert!(matches!(
            pr,
            CloudAgentStatus::PrCreated { pr_number: 42, .. }
        ));

        let no_pr = CloudAgentStatus::CompletedNoPr;
        assert_eq!(no_pr, CloudAgentStatus::CompletedNoPr);
    }

    #[test]
    fn resolve_repo_info_prefers_explicit_config() {
        let (owner, name) = resolve_repo_info(
            Some("explicit-owner"),
            Some("explicit-repo"),
            std::path::Path::new("."),
        )
        .unwrap();
        assert_eq!(owner, "explicit-owner");
        assert_eq!(name, "explicit-repo");
    }

    #[test]
    fn pr_merge_status_variants() {
        assert_eq!(PrMergeStatus::Draft, PrMergeStatus::Draft);
        assert_eq!(PrMergeStatus::NotMergeable, PrMergeStatus::NotMergeable);
        assert_eq!(PrMergeStatus::Mergeable, PrMergeStatus::Mergeable);
        assert_eq!(PrMergeStatus::AlreadyMerged, PrMergeStatus::AlreadyMerged);
        assert_ne!(PrMergeStatus::Draft, PrMergeStatus::Mergeable);
        assert_ne!(PrMergeStatus::AlreadyMerged, PrMergeStatus::NotMergeable);
    }

    #[test]
    fn is_copilot_in_assignees_present() {
        let issue = serde_json::json!({
            "assignees": [{"login": "copilot"}]
        });
        assert!(is_copilot_in_assignees(&issue));
    }

    #[test]
    fn is_copilot_in_assignees_among_others() {
        let issue = serde_json::json!({
            "assignees": [{"login": "user1"}, {"login": "copilot"}, {"login": "user2"}]
        });
        assert!(is_copilot_in_assignees(&issue));
    }

    #[test]
    fn is_copilot_in_assignees_missing() {
        let issue = serde_json::json!({
            "assignees": [{"login": "other-user"}]
        });
        assert!(!is_copilot_in_assignees(&issue));
    }

    #[test]
    fn is_copilot_in_assignees_empty() {
        let issue = serde_json::json!({
            "assignees": []
        });
        assert!(!is_copilot_in_assignees(&issue));
    }

    #[test]
    fn is_copilot_in_assignees_no_field() {
        let issue = serde_json::json!({});
        assert!(!is_copilot_in_assignees(&issue));
    }
}
