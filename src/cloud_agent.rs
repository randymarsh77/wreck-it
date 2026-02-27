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

impl CloudAgentClient {
    pub fn new(github_token: String, repo_owner: String, repo_name: String) -> Self {
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

        // Assign Copilot to the issue to trigger the coding agent.
        if !self.assign_copilot(issue_number).await {
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
    async fn assign_copilot(&self, issue_number: u64) -> bool {
        let url = format!(
            "{}/repos/{}/{}/issues/{}/assignees",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, issue_number,
        );

        let body = serde_json::json!({
            "assignees": [COPILOT_LOGIN],
        });

        match self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                // Verify that Copilot actually appears in the assignees list.
                // The API can return 200 without actually assigning Copilot
                // when the token lacks sufficient permissions.
                if let Ok(issue) = resp.json::<serde_json::Value>().await {
                    let assigned = issue["assignees"]
                        .as_array()
                        .map(|arr| {
                            arr.iter()
                                .any(|a| a["login"].as_str() == Some(COPILOT_LOGIN))
                        })
                        .unwrap_or(false);
                    if assigned {
                        tracing::info!(
                            "Assigned Copilot to issue #{} – coding agent triggered",
                            issue_number
                        );
                        return true;
                    }
                }
                tracing::warn!(
                    "Copilot not found in assignees for issue #{} after assignment; \
                     the token may lack permission to assign Copilot (a PAT may be required)",
                    issue_number,
                );
                false
            }
            Ok(resp) => {
                tracing::warn!(
                    "Failed to assign Copilot to issue #{} ({}); agent may need manual trigger",
                    issue_number,
                    resp.status(),
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    "HTTP error assigning Copilot to issue #{}: {}",
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

    /// Check whether a PR is open and in a mergeable state.
    pub async fn is_pr_mergeable(&self, pr_number: u64) -> Result<bool> {
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
            return Ok(false);
        }

        let pr: serde_json::Value = resp.json().await?;
        let state = pr["state"].as_str().unwrap_or("unknown");
        let draft = pr["draft"].as_bool().unwrap_or(false);
        let mergeable = pr["mergeable"].as_bool().unwrap_or(false);

        Ok(state == "open" && !draft && mergeable)
    }

    /// Check whether a PR is currently in draft mode.
    pub async fn is_pr_draft(&self, pr_number: u64) -> Result<bool> {
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
            bail!("Failed to fetch PR #{} ({})", pr_number, resp.status());
        }

        let pr: serde_json::Value = resp.json().await?;
        Ok(pr["draft"].as_bool().unwrap_or(false))
    }

    /// Mark a draft PR as ready for review.
    pub async fn mark_pr_ready_for_review(&self, pr_number: u64) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, pr_number,
        );

        let body = serde_json::json!({
            "draft": false,
        });

        let resp = self
            .http
            .patch(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&body)
            .send()
            .await
            .context("Failed to update PR draft status")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to mark PR #{} as ready for review ({}): {}",
                pr_number,
                status,
                body,
            );
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
}
