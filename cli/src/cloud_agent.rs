use anyhow::{bail, Context, Result};

const GITHUB_API_BASE: &str = "https://api.github.com";
/// Known coding agent logins — re-exported from `wreck-it-core` for local use.
const KNOWN_AGENT_LOGINS: &[&str] = wreck_it_core::types::KNOWN_AGENT_LOGINS;

/// Status of a cloud agent session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudAgentStatus {
    /// The agent is still working on the task.
    Working,
    /// The agent has created a PR.
    PrCreated { pr_number: u64, pr_url: String },
    /// The agent has created a PR but is still actively working on it.
    ///
    /// This is detected when the coding agent is still assigned to the
    /// triggering issue, indicating it has not finished pushing changes.
    PrCreatedAgentWorking { pr_number: u64, pr_url: String },
    /// The agent session completed or the issue was closed without a PR.
    CompletedNoPr,
}

/// Review status of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewStatus {
    /// Reviews are still pending (not all reviewers have submitted).
    Pending,
    /// All submitted reviews are approved (or dismissed).
    Approved,
    /// At least one reviewer has requested changes.
    ChangesRequested {
        /// Logins of reviewers who requested changes.
        reviewers: Vec<String>,
    },
}

/// Merge readiness of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PrMergeStatus {
    /// The PR is a draft and must be marked ready for review first.
    Draft,
    /// The coding agent is still actively working on the PR.
    ///
    /// Detected via Copilot agent session events (GraphQL) or by the
    /// `[wip]` title prefix heuristic as a fallback.  The runner should
    /// wait until the agent finishes before attempting to merge.
    AgentWorkInProgress,
    /// The PR is not yet mergeable (checks pending, conflicts, etc.).
    NotMergeable,
    /// The PR is ready to be merged.
    Mergeable,
    /// The PR has already been merged.
    AlreadyMerged,
    /// The PR was closed without being merged.
    ClosedNotMerged,
}

/// Summary of an open pull request, returned by [`CloudAgentClient::list_open_prs`].
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct OpenPr {
    pub number: u64,
    pub title: String,
    /// Whether the PR is currently a draft.
    #[allow(dead_code)]
    pub draft: bool,
}

/// Result of triggering a cloud agent.
#[derive(Debug, Clone)]
pub struct TriggerResult {
    pub issue_number: u64,
    pub issue_url: String,
}

/// Build the GitHub issue body for a cloud agent trigger.
///
/// Appends a "Previous Context" section when `memory` is non-empty so that
/// the coding agent has visibility into earlier iterations.  Each memory
/// entry is formatted as a bullet point and should be a complete, self-
/// contained description (e.g. "iteration 3: merged PR #7 for task setup").
///
/// When `branch` is provided the body includes explicit instructions for the
/// agent to base its work on that branch and target PRs to it.
pub(crate) fn build_issue_body(
    task_id: &str,
    task_description: &str,
    memory: &[String],
    branch: Option<&str>,
    system_prompt: Option<&str>,
) -> String {
    let system_prompt_section = match system_prompt {
        Some(sp) if !sp.is_empty() => format!(
            "<!-- system-prompt -->\n```\n{}\n```\n<!-- /system-prompt -->\n\n",
            sp
        ),
        _ => String::new(),
    };
    let branch_section = match branch {
        Some(b) => format!(
            "\n\n## Branch\n\nBase your work on the `{}` branch and target your pull request to `{}`.",
            b, b,
        ),
        None => String::new(),
    };
    let memory_section = if memory.is_empty() {
        String::new()
    } else {
        let bullets = memory
            .iter()
            .map(|m| format!("- {}", m))
            .collect::<Vec<_>>()
            .join("\n");
        format!("\n\n## Previous Context\n\n{}", bullets)
    };
    format!(
        "{}{}{}{}\n\n---\n*Triggered by wreck-it cloud agent orchestrator (task `{}`)*",
        system_prompt_section, task_description, branch_section, memory_section, task_id,
    )
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
    /// Login of the user authenticated by `github_token`.  Populated lazily
    /// via [`fetch_authenticated_login`].  When set, trust checks also accept
    /// issues/PRs created by this user (PAT flow).
    authenticated_login: Option<String>,
    /// When set, this agent login is preferred over the default
    /// [`KNOWN_AGENT_LOGINS`] list when assigning an agent to an issue.
    preferred_agent: Option<String>,
}

/// Check whether any known coding agent appears in an issue's assignees array.
fn is_copilot_in_assignees(issue: &serde_json::Value) -> bool {
    issue["assignees"]
        .as_array()
        .map(|arr| {
            arr.iter().any(|a| {
                a["login"]
                    .as_str()
                    .map(wreck_it_core::types::is_known_agent_login)
                    .unwrap_or(false)
            })
        })
        .unwrap_or(false)
}

/// Returns `true` if the PR title starts with `[wip]` (case-insensitive),
/// indicating the coding agent is still actively working on the PR.
fn is_wip_title(title: &str) -> bool {
    let trimmed = title.trim_start();
    // `get(..5)` safely returns `None` for short or non-ASCII-boundary strings.
    matches!(trimmed.get(..5), Some(prefix) if prefix.eq_ignore_ascii_case("[wip]"))
}

/// Parse Copilot agent session completion status from a GraphQL response.
///
/// The expected response shape is produced by a `timelineItems` query
/// filtered to `COPILOT_PULL_REQUEST_SESSION_EVENT` items:
///
/// ```json
/// { "data": { "repository": { "pullRequest": { "timelineItems": {
///     "nodes": [ { "copilotSession": { "completedAt": "..." } } ]
/// } } } } }
/// ```
///
/// Returns `Some(true)` if the latest session has a `completedAt` timestamp,
/// `Some(false)` if a session exists but is not completed, and `None` if no
/// session data is present or the response structure is unexpected.
fn parse_copilot_session_status(graphql_response: &serde_json::Value) -> Option<bool> {
    // GraphQL-level errors indicate the feature flag may not be available.
    if graphql_response.get("errors").is_some() {
        return None;
    }

    let nodes = graphql_response
        .pointer("/data/repository/pullRequest/timelineItems/nodes")
        .and_then(|v| v.as_array())?;

    if nodes.is_empty() {
        return None;
    }

    // The query uses `last: N`, so the final element is the most recent session.
    let latest = nodes.last()?;
    let completed_at = latest
        .pointer("/copilotSession/completedAt")
        .and_then(|v| v.as_str());

    if completed_at.is_some() {
        Some(true)
    } else {
        Some(false)
    }
}

/// Partition an issue's assignees into agent and non-agent logins.
///
/// Returns `(agent_logins, non_agent_logins)`.
fn partition_assignees(issue: &serde_json::Value) -> (Vec<String>, Vec<String>) {
    let mut agents = Vec::new();
    let mut others = Vec::new();
    if let Some(arr) = issue["assignees"].as_array() {
        for a in arr {
            if let Some(login) = a["login"].as_str() {
                if wreck_it_core::types::is_known_agent_login(login) {
                    agents.push(login.to_string());
                } else {
                    others.push(login.to_string());
                }
            }
        }
    }
    (agents, others)
}

impl CloudAgentClient {
    pub fn new(github_token: String, repo_owner: String, repo_name: String) -> Self {
        Self {
            github_token,
            repo_owner,
            repo_name,
            http: reqwest::Client::new(),
            authenticated_login: None,
            preferred_agent: None,
        }
    }

    /// Set a preferred agent login for issue assignment.
    ///
    /// When set, [`get_agent_from_suggested_actors`] searches for this login
    /// first before falling back to the default known agent list.
    pub fn set_preferred_agent(&mut self, agent: Option<String>) {
        self.preferred_agent = agent;
    }

    /// Fetch the login of the user authenticated by our token and cache it.
    ///
    /// Calls `GET /user` and stores the result so subsequent trust checks can
    /// recognise issues/PRs created by the PAT owner as trusted.  Safe to
    /// call multiple times — the API call is only made once.
    pub async fn resolve_authenticated_login(&mut self) {
        if self.authenticated_login.is_some() {
            return;
        }
        let url = format!("{}/user", GITHUB_API_BASE);
        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await;
        match resp {
            Ok(resp) => {
                if !resp.status().is_success() {
                    tracing::warn!(
                        "GET /user returned status {}; falling back to Bot/agent-only trust check",
                        resp.status(),
                    );
                    return;
                }
                match resp.json::<serde_json::Value>().await {
                    Ok(body) => {
                        if let Some(login) = body["login"].as_str() {
                            tracing::info!("Authenticated as GitHub user '{}'", login);
                            self.authenticated_login = Some(login.to_string());
                        } else {
                            tracing::warn!("GET /user response has no login field");
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Failed to parse GET /user response: {}", e);
                    }
                }
            }
            Err(e) => {
                tracing::warn!("GET /user request failed: {}", e);
            }
        }
    }

    /// Trigger a cloud coding agent for the given task.
    ///
    /// Creates a GitHub issue with the task description and assigns Copilot to
    /// it, which triggers the Copilot coding agent to work on the task
    /// autonomously and create a pull request.
    ///
    /// `memory_context` is a slice of freeform notes from previous iterations
    /// that is appended to the issue body so the agent has historical context.
    ///
    /// When `branch` is provided the issue body instructs the agent to base its
    /// work on that branch and target PRs to it.
    pub async fn trigger_agent(
        &self,
        ralph_name: &str,
        task_id: &str,
        task_description: &str,
        memory_context: &[String],
        branch: Option<&str>,
        system_prompt: Option<&str>,
    ) -> Result<TriggerResult> {
        let issue_body = build_issue_body(
            task_id,
            task_description,
            memory_context,
            branch,
            system_prompt,
        );

        let create_body = serde_json::json!({
            "title": format!("[wreck-it] {} {}", ralph_name, task_id),
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
            .assign_copilot(issue_number, issue_node_id.as_deref(), branch)
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

    /// Create a GitHub issue for cloud-based plan generation and assign a
    /// coding agent to it.
    ///
    /// This is used by `wreck-it plan --cloud` to delegate plan creation to a
    /// cloud agent (e.g. Copilot).  The issue body should contain instructions
    /// for the agent to write a task plan file to `.wreck-it/plans/`.
    pub async fn create_plan_issue(&self, title: &str, body: &str) -> Result<TriggerResult> {
        let create_body = serde_json::json!({
            "title": title,
            "body": body,
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
            .context("Failed to create GitHub issue for cloud plan")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to create plan issue ({}): {}", status, body);
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

        // Assign a cloud agent to the issue.
        if !self
            .assign_copilot(issue_number, issue_node_id.as_deref(), None)
            .await
        {
            tracing::warn!(
                "Agent assignment failed for plan issue #{}; the issue was created but \
                 the agent may need to be assigned manually",
                issue_number,
            );
        }

        Ok(TriggerResult {
            issue_number,
            issue_url,
        })
    }

    /// Assign the Copilot bot to an issue, triggering the coding agent.
    ///
    /// When `branch` is provided it is passed as `agentAssignment.baseRef` in
    /// the GraphQL mutation so the coding agent starts from (and targets PRs
    /// to) that branch instead of the repository default.
    ///
    /// Returns `true` if the assignment succeeded.
    async fn assign_copilot(
        &self,
        issue_number: u64,
        issue_node_id: Option<&str>,
        branch: Option<&str>,
    ) -> bool {
        self.try_assign_copilot(issue_number, issue_node_id, branch)
            .await
    }

    /// Find an available coding agent via the `suggestedActors` GraphQL query.
    ///
    /// Returns `(agent_node_id, agent_login)` of the first known agent found,
    /// or `None` if no agent could be discovered.
    async fn get_agent_from_suggested_actors(&self) -> Option<(String, String)> {
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": r#"query($owner: String!, $name: String!) {
                repository(owner: $owner, name: $name) {
                    suggestedActors(capabilities: [CAN_BE_ASSIGNED], first: 100) {
                        nodes {
                            login
                            __typename
                            ... on Bot { id }
                            ... on User { id }
                        }
                    }
                }
            }"#,
            "variables": {
                "owner": self.repo_owner,
                "name": self.repo_name,
            },
        });

        let resp = match self
            .http
            .post(&graphql_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&query)
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("HTTP error querying suggestedActors: {}", e);
                return None;
            }
        };

        if !resp.status().is_success() {
            tracing::warn!("suggestedActors query failed ({})", resp.status(),);
            return None;
        }

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!("Failed to parse suggestedActors response: {}", e);
                return None;
            }
        };

        if let Some(errors) = body.get("errors") {
            tracing::warn!("GraphQL errors in suggestedActors query: {}", errors);
            return None;
        }

        let nodes = body
            .pointer("/data/repository/suggestedActors/nodes")
            .and_then(|v| v.as_array());

        let nodes = match nodes {
            Some(n) => n,
            None => {
                tracing::warn!("No suggestedActors nodes returned");
                return None;
            }
        };

        // When a preferred agent is configured, search for it first.
        if let Some(ref preferred) = self.preferred_agent {
            for node in nodes {
                let node_login = node["login"].as_str().unwrap_or_default();
                if node_login.eq_ignore_ascii_case(preferred) {
                    if let Some(id) = node["id"].as_str() {
                        tracing::info!(
                            "Found preferred agent '{}' (id: {}) via suggestedActors",
                            node_login,
                            id,
                        );
                        return Some((id.to_string(), node_login.to_string()));
                    }
                }
            }
            tracing::warn!(
                "Preferred agent '{}' not found in suggestedActors; falling back to default list",
                preferred,
            );
        }

        // Search for the first known agent login in priority order.
        for &known_login in KNOWN_AGENT_LOGINS {
            for node in nodes {
                let node_login = node["login"].as_str().unwrap_or_default();
                if node_login.eq_ignore_ascii_case(known_login) {
                    if let Some(id) = node["id"].as_str() {
                        tracing::info!(
                            "Found coding agent '{}' (id: {}) via suggestedActors",
                            node_login,
                            id,
                        );
                        return Some((id.to_string(), node_login.to_string()));
                    }
                }
            }
        }

        tracing::warn!(
            "No known coding agent found in suggestedActors (searched for {:?}). \
             This feature may require a GitHub Enterprise account or Copilot for \
             Pull Requests and Issues to be enabled in the repository settings.",
            KNOWN_AGENT_LOGINS,
        );
        None
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

    /// Attempt to assign a coding agent using the GraphQL
    /// `addAssigneesToAssignable` mutation. Discovers the agent via
    /// `suggestedActors` and falls back to fetching the issue node ID if it was
    /// not provided.
    ///
    /// When `branch` is `Some`, the mutation includes an `agentAssignment`
    /// input with `baseRef` set to the given branch name so the coding agent
    /// starts from that branch and targets PRs to it.
    ///
    /// Returns `true` when the mutation succeeds.
    async fn try_assign_copilot(
        &self,
        issue_number: u64,
        issue_node_id: Option<&str>,
        branch: Option<&str>,
    ) -> bool {
        // Resolve the issue's GraphQL node ID.
        let owned_node_id;
        let assignable_id = match issue_node_id {
            Some(id) => id,
            None => match self.get_issue_node_id(issue_number).await {
                Some(id) => {
                    owned_node_id = id;
                    owned_node_id.as_str()
                }
                None => {
                    tracing::warn!("Could not resolve node_id for issue #{}", issue_number,);
                    return false;
                }
            },
        };

        // Discover a coding agent via the suggestedActors GraphQL query.
        let (agent_id, agent_login) = match self.get_agent_from_suggested_actors().await {
            Some(pair) => pair,
            None => return false,
        };

        // Use the GraphQL addAssigneesToAssignable mutation.
        //
        // When a branch is specified, include `agentAssignment.baseRef` so
        // the coding agent starts from (and targets PRs to) that branch.
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let mut variables = serde_json::json!({
            "assignableId": assignable_id,
            "assigneeIds": [agent_id],
        });
        if let Some(base_ref) = branch {
            variables["agentAssignment"] = serde_json::json!({ "baseRef": base_ref });
        }
        let query = serde_json::json!({
            "query": r#"mutation($assignableId: ID!, $assigneeIds: [ID!]!, $agentAssignment: AgentAssignmentInput) {
                addAssigneesToAssignable(input: {
                    assignableId: $assignableId,
                    assigneeIds: $assigneeIds,
                    agentAssignment: $agentAssignment
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
            "variables": variables,
        });

        match self
            .http
            .post(&graphql_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .header(
                "GraphQL-Features",
                "issues_copilot_assignment_api_support,coding_agent_model_selection",
            )
            .json(&query)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(gql_resp) = resp.json::<serde_json::Value>().await {
                    // Check for GraphQL-level errors.
                    if let Some(errors) = gql_resp.get("errors") {
                        tracing::warn!(
                            "GraphQL errors assigning '{}' to issue #{}: {}",
                            agent_login,
                            issue_number,
                            errors,
                        );
                        return false;
                    }

                    // Verify the agent appears in the assignees from the
                    // mutation response.
                    let has_agent = gql_resp
                        .pointer("/data/addAssigneesToAssignable/assignable/assignees/nodes")
                        .and_then(|v| v.as_array())
                        .map(|arr| {
                            arr.iter().any(|a| {
                                a["login"]
                                    .as_str()
                                    .map(wreck_it_core::types::is_known_agent_login)
                                    .unwrap_or(false)
                            })
                        })
                        .unwrap_or(false);

                    if has_agent {
                        tracing::info!(
                            "Assigned '{}' to issue #{} via GraphQL – coding agent triggered",
                            agent_login,
                            issue_number,
                        );
                        return true;
                    }
                }

                // The mutation response didn't include the agent in
                // assignees, but there can be a propagation delay. Poll
                // the REST API a few times before giving up.
                tracing::debug!(
                    "Agent not immediately visible in mutation response for issue #{}; \
                     polling REST API to confirm",
                    issue_number,
                );
                if self
                    .poll_assignee_with_retries(issue_number, 3, std::time::Duration::from_secs(2))
                    .await
                {
                    tracing::info!(
                        "Confirmed '{}' assigned to issue #{} after polling REST API",
                        agent_login,
                        issue_number,
                    );
                    return true;
                }

                tracing::warn!(
                    "Agent not found in assignees for issue #{} after GraphQL mutation \
                     and REST polling; the token may lack permission or a GitHub \
                     Enterprise account may be required",
                    issue_number,
                );
                false
            }
            Ok(resp) => {
                tracing::warn!(
                    "Failed to assign '{}' to issue #{} via GraphQL ({}); \
                     agent may need manual trigger",
                    agent_login,
                    issue_number,
                    resp.status(),
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    "HTTP error assigning '{}' to issue #{} via GraphQL: {}",
                    agent_login,
                    issue_number,
                    e,
                );
                false
            }
        }
    }

    /// Poll the REST API to check whether a known coding agent appears in an
    /// issue's assignees. Retries up to `max_retries` times, sleeping
    /// `delay` between each attempt.
    async fn poll_assignee_with_retries(
        &self,
        issue_number: u64,
        max_retries: u32,
        delay: std::time::Duration,
    ) -> bool {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, issue_number,
        );
        for attempt in 1..=max_retries {
            tokio::time::sleep(delay).await;
            match self
                .http
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.github_token))
                .header("User-Agent", "wreck-it")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    match resp.json::<serde_json::Value>().await {
                        Ok(issue) if is_copilot_in_assignees(&issue) => return true,
                        Ok(_) => {}
                        Err(e) => {
                            tracing::debug!(
                                "REST poll attempt {}/{} for issue #{}: JSON parse error: {}",
                                attempt,
                                max_retries,
                                issue_number,
                                e,
                            );
                        }
                    }
                }
                Ok(resp) => {
                    tracing::debug!(
                        "REST poll attempt {}/{} for issue #{} returned {}",
                        attempt,
                        max_retries,
                        issue_number,
                        resp.status(),
                    );
                }
                Err(e) => {
                    tracing::debug!(
                        "REST poll attempt {}/{} for issue #{} failed: {}",
                        attempt,
                        max_retries,
                        issue_number,
                        e,
                    );
                }
            }
        }
        false
    }

    /// Check the status of an agent session by looking for PRs that reference
    /// the triggering issue.
    pub async fn check_agent_status(&self, issue_number: u64) -> Result<CloudAgentStatus> {
        // Look for open PRs that reference this issue.
        if let Some(status) = self.find_linked_pr(issue_number).await? {
            // A linked PR was found.  Check whether the coding agent is still
            // assigned to the triggering issue — if so, it *may* be still
            // actively pushing changes.  However, some agents (e.g. Copilot)
            // remain assigned after finishing, so also consult PR-level
            // signals before declaring the agent is still working.
            if let CloudAgentStatus::PrCreated { pr_number, pr_url } = status {
                if self.is_agent_assigned_to_issue(issue_number).await?
                    && !self.is_pr_work_completed(pr_number).await
                {
                    return Ok(CloudAgentStatus::PrCreatedAgentWorking { pr_number, pr_url });
                }
                return Ok(CloudAgentStatus::PrCreated { pr_number, pr_url });
            }
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
                if self.assign_copilot(issue_number, None, None).await {
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

    /// Check whether a known coding agent is still assigned to the given issue.
    ///
    /// Returns `true` when one of the [`KNOWN_AGENT_LOGINS`] appears in the
    /// issue's assignee list, indicating the agent is still actively working.
    /// Network or permission errors are treated as `false` (not assigned) so
    /// that the caller can proceed conservatively.
    pub async fn is_agent_assigned_to_issue(&self, issue_number: u64) -> Result<bool> {
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
            .context("Failed to fetch issue for agent assignment check")?;

        if resp.status().is_success() {
            let issue: serde_json::Value = resp.json().await?;
            Ok(is_copilot_in_assignees(&issue))
        } else {
            Ok(false)
        }
    }

    /// Check whether PR-level signals indicate the coding agent has finished
    /// working on the PR, even though it may still appear as an issue assignee.
    ///
    /// Uses the same two-tier strategy as [`check_pr_merge_status`]:
    ///   1. **Primary**: Copilot agent session completion via GraphQL.
    ///   2. **Fallback**: absence of a `[wip]` title prefix.
    ///
    /// Returns `true` when the agent appears to have finished, `false` when it
    /// is still working or the status cannot be determined.
    async fn is_pr_work_completed(&self, pr_number: u64) -> bool {
        // Primary signal: Copilot session completion via GraphQL.
        match self.check_copilot_session_completed(pr_number).await {
            Some(true) => return true,
            Some(false) => return false,
            None => {}
        }

        // Fallback: check whether the PR title starts with [wip].
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
            .await;

        if let Ok(resp) = resp {
            if resp.status().is_success() {
                if let Ok(pr) = resp.json::<serde_json::Value>().await {
                    let title = pr["title"].as_str().unwrap_or("");
                    return !is_wip_title(title);
                }
            }
        }

        // Cannot determine; conservatively assume still working.
        false
    }

    /// Fetch issue assignee details for diagnostic logging.
    ///
    /// Returns `(agent_logins, non_agent_logins)` so callers can determine
    /// whether the coding agent is the sole assignee (still working) or
    /// whether human reviewers have also been assigned (agent likely done).
    pub async fn get_issue_assignee_summary(
        &self,
        issue_number: u64,
    ) -> Result<(Vec<String>, Vec<String>)> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, issue_number,
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to fetch issue for assignee summary")?;

        if resp.status().is_success() {
            let issue: serde_json::Value = resp.json().await?;
            Ok(partition_assignees(&issue))
        } else {
            Ok((Vec::new(), Vec::new()))
        }
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
                                    // Supply-chain protection: only accept PRs
                                    // opened by a known coding agent or the
                                    // authenticated user.
                                    let pr_author = issue_obj
                                        .and_then(|i| i.pointer("/user/login"))
                                        .and_then(|v| v.as_str());
                                    if !wreck_it_core::types::is_trusted_pr_author(
                                        pr_author,
                                        self.authenticated_login.as_deref(),
                                    ) {
                                        tracing::warn!(
                                            "Ignoring cross-referenced PR by {:?} (not a known agent or authenticated user)",
                                            pr_author.unwrap_or("<unknown>"),
                                        );
                                        continue;
                                    }
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

    /// Check whether a Copilot agent has finished working on a pull request
    /// by querying for agent session events via the GraphQL API.
    ///
    /// Returns `Some(true)` when a completed agent session is found,
    /// `Some(false)` when only in-progress sessions exist, and `None`
    /// when the query fails or no session data is available (e.g. the
    /// API does not support the required feature flag).
    pub async fn check_copilot_session_completed(&self, pr_number: u64) -> Option<bool> {
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": r#"query($owner: String!, $name: String!, $number: Int!) {
                repository(owner: $owner, name: $name) {
                    pullRequest(number: $number) {
                        timelineItems(last: 10, itemTypes: [COPILOT_PULL_REQUEST_SESSION_EVENT]) {
                            nodes {
                                __typename
                                ... on CopilotPullRequestSessionEvent {
                                    createdAt
                                    copilotSession {
                                        completedAt
                                    }
                                }
                            }
                        }
                    }
                }
            }"#,
            "variables": {
                "owner": self.repo_owner,
                "name": self.repo_name,
                "number": pr_number,
            },
        });

        let resp = self
            .http
            .post(&graphql_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .header("GraphQL-Features", "copilot_pull_request_agent_session")
            .json(&query)
            .send()
            .await
            .ok()?;

        if !resp.status().is_success() {
            return None;
        }

        let body: serde_json::Value = resp.json().await.ok()?;
        parse_copilot_session_status(&body)
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
            let status = resp.status();
            println!(
                "[wreck-it] PR #{} fetch failed with status {}",
                pr_number, status,
            );
            return Ok(PrMergeStatus::NotMergeable);
        }

        let pr: serde_json::Value = resp.json().await?;
        let title = pr["title"].as_str().unwrap_or("");
        let state = pr["state"].as_str().unwrap_or("unknown");
        let draft = pr["draft"].as_bool().unwrap_or(false);
        let mergeable_raw = &pr["mergeable"];
        let mergeable = mergeable_raw.as_bool().unwrap_or(false);
        let merged = pr["merged"].as_bool().unwrap_or(false);
        let mergeable_state = pr["mergeable_state"].as_str().unwrap_or("unknown");
        let pr_author = pr.pointer("/user/login").and_then(|v| v.as_str());

        println!(
            "[wreck-it] PR #{} details: title={:?}, state={}, draft={}, \
             mergeable(raw)={}, mergeable_state={}, merged={}, author={:?}",
            pr_number, title, state, draft, mergeable_raw, mergeable_state, merged, pr_author,
        );

        // Supply-chain protection: reject PRs not opened by a known agent
        // or the authenticated user.
        if !wreck_it_core::types::is_trusted_pr_author(
            pr_author,
            self.authenticated_login.as_deref(),
        ) {
            println!(
                "[wreck-it] PR #{} was opened by {:?}, not a known agent or authenticated user — refusing to process",
                pr_number,
                pr_author.unwrap_or("<unknown>"),
            );
            return Ok(PrMergeStatus::NotMergeable);
        }

        if merged {
            return Ok(PrMergeStatus::AlreadyMerged);
        }
        if state != "open" {
            return Ok(PrMergeStatus::ClosedNotMerged);
        }
        if draft {
            return Ok(PrMergeStatus::Draft);
        }
        // Detect agent work-in-progress.
        //
        // Primary signal: query the Copilot agent session status via GraphQL.
        // Fallback: check whether the title starts with [wip].
        match self.check_copilot_session_completed(pr_number).await {
            Some(true) => {
                // Agent session completed; skip the [wip] title heuristic.
                println!(
                    "[wreck-it] PR #{} has a completed Copilot agent session",
                    pr_number,
                );
            }
            Some(false) => {
                // Agent session is still active.
                println!(
                    "[wreck-it] PR #{} has an active Copilot agent session — still working",
                    pr_number,
                );
                return Ok(PrMergeStatus::AgentWorkInProgress);
            }
            None => {
                // Could not determine from GraphQL; fall back to title heuristic.
                if is_wip_title(title) {
                    return Ok(PrMergeStatus::AgentWorkInProgress);
                }
            }
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

    /// Approve any pending workflow runs for a pull request.
    ///
    /// When a repo requires approval for Actions (e.g. first-time contributors,
    /// outside collaborators, or fork PRs), workflow runs may sit in
    /// `action_required`, `pending`, or `waiting` status until explicitly
    /// approved.  This method fetches the PR's head SHA, lists workflow runs
    /// for that commit across all three statuses, and approves every run that
    /// is waiting.
    pub async fn approve_pending_workflow_runs(&self, pr_number: u64) -> Result<()> {
        // Fetch the PR to obtain the head SHA.
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
            .context("Failed to fetch PR for head SHA")?;

        if !pr_resp.status().is_success() {
            let status = pr_resp.status();
            let body = pr_resp.text().await.unwrap_or_default();
            bail!(
                "Failed to fetch PR #{} for head SHA ({}): {}",
                pr_number,
                status,
                body,
            );
        }

        let pr: serde_json::Value = pr_resp.json().await?;
        let head_sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .context("Missing head SHA in PR response")?;

        // Query workflow runs across all statuses that may indicate a run is
        // waiting for approval.  Depending on the repository configuration,
        // GitHub may place pending runs in `action_required` (fork PRs),
        // `pending` (first-time contributors), or `waiting` (outside
        // collaborators / deployment protection rules).
        let mut all_run_ids: Vec<u64> = Vec::new();

        for status_filter in &["action_required", "pending", "waiting"] {
            let runs_url = format!(
                "{}/repos/{}/{}/actions/runs?head_sha={}&status={}",
                GITHUB_API_BASE, self.repo_owner, self.repo_name, head_sha, status_filter,
            );

            let runs_resp = match self
                .http
                .get(&runs_url)
                .header("Authorization", format!("Bearer {}", self.github_token))
                .header("User-Agent", "wreck-it")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
            {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Failed to list {} workflow runs for PR #{}: {}",
                        status_filter,
                        pr_number,
                        e,
                    );
                    continue;
                }
            };

            if !runs_resp.status().is_success() {
                let status = runs_resp.status();
                let body = runs_resp.text().await.unwrap_or_default();
                tracing::warn!(
                    "Failed to list {} workflow runs for PR #{} ({}): {}",
                    status_filter,
                    pr_number,
                    status,
                    body,
                );
                continue;
            }

            let runs: serde_json::Value = match runs_resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(
                        "Failed to parse {} workflow runs response for PR #{}: {}",
                        status_filter,
                        pr_number,
                        e,
                    );
                    continue;
                }
            };

            if let Some(arr) = runs["workflow_runs"].as_array() {
                for run in arr {
                    if let Some(id) = run["id"].as_u64() {
                        if !all_run_ids.contains(&id) {
                            let name = run["name"].as_str().unwrap_or("unknown");
                            tracing::info!(
                                "Found {} workflow run {} (name={}) for PR #{}",
                                status_filter,
                                id,
                                name,
                                pr_number,
                            );
                            all_run_ids.push(id);
                        }
                    }
                }
            }
        }

        if all_run_ids.is_empty() {
            tracing::debug!("No workflow runs awaiting approval for PR #{}", pr_number);
            return Ok(());
        }

        let mut approved_count: usize = 0;

        for run_id in &all_run_ids {
            // Attempt the REST approval endpoint.  This is the standard
            // mechanism for approving workflow runs that require manual
            // approval (fork PRs, first-time contributors, etc.).
            let approve_url = format!(
                "{}/repos/{}/{}/actions/runs/{}/approve",
                GITHUB_API_BASE, self.repo_owner, self.repo_name, run_id,
            );

            match self
                .http
                .post(&approve_url)
                .header("Authorization", format!("Bearer {}", self.github_token))
                .header("User-Agent", "wreck-it")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
            {
                Ok(resp) if resp.status().is_success() => {
                    tracing::info!(
                        "Approved workflow run {} via REST for PR #{}",
                        run_id,
                        pr_number,
                    );
                    approved_count += 1;
                    continue;
                }
                Ok(resp) => {
                    let status = resp.status();
                    let body = resp.text().await.unwrap_or_default();
                    tracing::debug!(
                        "REST approve returned {} for workflow run {} on PR #{}: {}",
                        status,
                        run_id,
                        pr_number,
                        body,
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        "HTTP error on REST approve for workflow run {} on PR #{}: {}",
                        run_id,
                        pr_number,
                        e,
                    );
                }
            }

            // Fall back to the pending_deployments endpoint.  This handles
            // runs in `waiting` status (deployment protection rules) as well
            // as other cases where the `/approve` endpoint is not sufficient.
            if self.approve_pending_deployments(*run_id, pr_number).await {
                approved_count += 1;
            }
        }

        if approved_count > 0 {
            tracing::info!(
                "Approved {} of {} pending workflow run(s) for PR #{}",
                approved_count,
                all_run_ids.len(),
                pr_number,
            );
        } else {
            tracing::warn!(
                "Found {} pending workflow run(s) for PR #{} but could not approve any",
                all_run_ids.len(),
                pr_number,
            );
        }
        Ok(())
    }

    /// Attempt to approve pending deployments for a workflow run.
    ///
    /// When a workflow run is in `waiting` status due to deployment protection
    /// rules, the standard `/approve` endpoint does not work.  Instead we
    /// must query the pending deployments and approve each environment.
    async fn approve_pending_deployments(&self, run_id: u64, pr_number: u64) -> bool {
        let pending_url = format!(
            "{}/repos/{}/{}/actions/runs/{}/pending_deployments",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, run_id,
        );

        let pending_resp = match self
            .http
            .get(&pending_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(
                    "Failed to fetch pending deployments for run {} (PR #{}): {}",
                    run_id,
                    pr_number,
                    e,
                );
                return false;
            }
        };

        if !pending_resp.status().is_success() {
            tracing::debug!(
                "Pending deployments request failed for run {} (PR #{}) ({})",
                run_id,
                pr_number,
                pending_resp.status(),
            );
            return false;
        }

        let deployments: serde_json::Value = match pending_resp.json().await {
            Ok(v) => v,
            Err(e) => {
                tracing::warn!(
                    "Failed to parse pending deployments for run {} (PR #{}): {}",
                    run_id,
                    pr_number,
                    e,
                );
                return false;
            }
        };

        let env_ids: Vec<u64> = deployments
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|d| d.pointer("/environment/id").and_then(|v| v.as_u64()))
                    .collect()
            })
            .unwrap_or_default();

        if env_ids.is_empty() {
            return false;
        }

        let approve_body = serde_json::json!({
            "environment_ids": env_ids,
            "state": "approved",
            "comment": "Approved by wreck-it",
        });

        match self
            .http
            .post(&pending_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&approve_body)
            .send()
            .await
        {
            Ok(resp) if resp.status().is_success() => {
                tracing::info!(
                    "Approved pending deployments for workflow run {} (PR #{})",
                    run_id,
                    pr_number,
                );
                true
            }
            Ok(resp) => {
                tracing::warn!(
                    "Failed to approve pending deployments for run {} (PR #{}) ({})",
                    run_id,
                    pr_number,
                    resp.status(),
                );
                false
            }
            Err(e) => {
                tracing::warn!(
                    "HTTP error approving pending deployments for run {} (PR #{}): {}",
                    run_id,
                    pr_number,
                    e,
                );
                false
            }
        }
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

    /// Check whether the base branch of a pull request has required status
    /// checks configured via branch protection rules.
    ///
    /// Returns `true` when the branch has required status checks (meaning CI
    /// must pass before merging) and `false` otherwise (no branch protection,
    /// or no required checks).
    pub async fn has_required_checks_for_pr(&self, pr_number: u64) -> Result<bool> {
        // Fetch the PR to determine the base branch.
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
            .context("Failed to fetch PR for base branch")?;

        if !pr_resp.status().is_success() {
            // If we can't determine the base branch, assume no required checks.
            return Ok(false);
        }

        let pr: serde_json::Value = pr_resp.json().await?;
        let base_branch = pr
            .pointer("/base/ref")
            .and_then(|v| v.as_str())
            .unwrap_or("main");

        // Check legacy branch protection for required status checks.
        let protection_url = format!(
            "{}/repos/{}/{}/branches/{}/protection/required_status_checks",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, base_branch,
        );

        let resp = self
            .http
            .get(&protection_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to check branch protection")?;

        if resp.status().is_success() {
            // Legacy branch protection has required status checks.
            return Ok(true);
        }

        // Legacy branch protection didn't report required checks.  Fall back
        // to the repository rulesets API which covers the newer GitHub
        // rulesets that are not reflected by the legacy endpoint.
        let rules_url = format!(
            "{}/repos/{}/{}/rules/branches/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, base_branch,
        );

        let rules_resp = self
            .http
            .get(&rules_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to check repository rulesets")?;

        if !rules_resp.status().is_success() {
            return Ok(false);
        }

        let rules: serde_json::Value = rules_resp.json().await?;
        if let Some(arr) = rules.as_array() {
            for rule in arr {
                if rule["type"].as_str() == Some("required_status_checks") {
                    return Ok(true);
                }
            }
        }

        Ok(false)
    }

    /// Check whether the head commit of a pull request has any check runs
    /// that are still queued or in progress.
    ///
    /// Returns `true` when at least one check run is pending (not yet
    /// completed), `false` otherwise.
    pub async fn has_pending_checks_for_pr(&self, pr_number: u64) -> Result<bool> {
        // Fetch the PR to obtain the head SHA.
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
            .context("Failed to fetch PR for head SHA")?;

        if !pr_resp.status().is_success() {
            return Ok(false);
        }

        let pr: serde_json::Value = pr_resp.json().await?;
        let head_sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .context("Missing head SHA in PR response")?;

        // Check for queued check runs.
        for status in &["queued", "in_progress"] {
            let checks_url = format!(
                "{}/repos/{}/{}/commits/{}/check-runs?status={}&per_page=1",
                GITHUB_API_BASE, self.repo_owner, self.repo_name, head_sha, status,
            );

            let resp = self
                .http
                .get(&checks_url)
                .header("Authorization", format!("Bearer {}", self.github_token))
                .header("User-Agent", "wreck-it")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
                .context("Failed to list check runs")?;

            if !resp.status().is_success() {
                continue;
            }

            let body: serde_json::Value = resp.json().await?;
            let count = body["total_count"].as_u64().unwrap_or(0);
            if count > 0 {
                return Ok(true);
            }
        }

        Ok(false)
    }

    /// Check whether the head commit of a pull request has any failing
    /// check runs (i.e. runs with `conclusion` set to `"failure"`).
    ///
    /// Returns `true` when at least one check run has failed, `false`
    /// otherwise (all passing, still pending, or unable to determine).
    pub async fn has_failing_checks_for_pr(&self, pr_number: u64) -> Result<bool> {
        // Fetch the PR to obtain the head SHA.
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
            .context("Failed to fetch PR for head SHA")?;

        if !pr_resp.status().is_success() {
            return Ok(false);
        }

        let pr: serde_json::Value = pr_resp.json().await?;
        let head_sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .context("Missing head SHA in PR response")?;

        // Query check runs for the head SHA, filtering by status=completed.
        // Note: fetches up to 100 results (one page).  In practice, a single
        // commit rarely exceeds 100 completed check runs.
        let checks_url = format!(
            "{}/repos/{}/{}/commits/{}/check-runs?status=completed&per_page=100",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, head_sha,
        );

        let resp = self
            .http
            .get(&checks_url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to list check runs")?;

        if !resp.status().is_success() {
            return Ok(false);
        }

        let body: serde_json::Value = resp.json().await?;
        let has_failure = body["check_runs"]
            .as_array()
            .map(|runs| {
                runs.iter()
                    .any(|r| r["conclusion"].as_str() == Some("failure"))
            })
            .unwrap_or(false);

        Ok(has_failure)
    }

    /// Post a comment on a pull request (via the issues comments API).
    pub async fn comment_on_pr(&self, pr_number: u64, body: &str) -> Result<()> {
        let url = format!(
            "{}/repos/{}/{}/issues/{}/comments",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, pr_number,
        );

        let payload = serde_json::json!({ "body": body });

        let resp = self
            .http
            .post(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&payload)
            .send()
            .await
            .context("Failed to post PR comment")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let resp_body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to comment on PR #{} ({}): {}",
                pr_number,
                status,
                resp_body,
            );
        }

        tracing::info!("Posted comment on PR #{}", pr_number);
        Ok(())
    }

    /// Enable auto-merge on a pull request using the squash merge method.
    ///
    /// This uses the GraphQL `enablePullRequestAutoMerge` mutation so that
    /// GitHub automatically merges the PR once all required checks pass.
    pub async fn enable_auto_merge(&self, pr_number: u64) -> Result<()> {
        // Fetch the PR to obtain the GraphQL node_id.
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

        // Use the GraphQL API to enable auto-merge.
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": concat!(
                "mutation($prId: ID!) { ",
                  "enablePullRequestAutoMerge(input: { ",
                    "pullRequestId: $prId, ",
                    "mergeMethod: SQUASH ",
                  "}) { ",
                    "pullRequest { autoMergeRequest { enabledAt } } ",
                  "} ",
                "}"
            ),
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
            .context("Failed to call GraphQL enablePullRequestAutoMerge")?;

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
                "GraphQL errors enabling auto-merge for PR #{}: {}",
                pr_number,
                errors,
            );
        }

        tracing::info!("Enabled auto-merge for PR #{}", pr_number);
        Ok(())
    }

    /// Disable auto-merge on a pull request.
    ///
    /// Uses the GraphQL `disablePullRequestAutoMerge` mutation so that
    /// GitHub stops waiting for checks to pass before merging.  This is
    /// used in "brute mode" to clear any previously-enabled auto-merge
    /// before performing a direct merge.
    pub async fn disable_auto_merge(&self, pr_number: u64) -> Result<()> {
        // Fetch the PR to obtain the GraphQL node_id.
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

        // Use the GraphQL API to disable auto-merge.
        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": concat!(
                "mutation($prId: ID!) { ",
                  "disablePullRequestAutoMerge(input: { ",
                    "pullRequestId: $prId ",
                  "}) { ",
                    "pullRequest { autoMergeRequest { enabledAt } } ",
                  "} ",
                "}"
            ),
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
            .context("Failed to call GraphQL disablePullRequestAutoMerge")?;

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
                "GraphQL errors disabling auto-merge for PR #{}: {}",
                pr_number,
                errors,
            );
        }

        tracing::info!("Disabled auto-merge for PR #{}", pr_number);
        Ok(())
    }

    /// Fetch the GraphQL node ID of a GitHub user by their login.
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
                tracing::warn!(
                    "Failed to fetch user '{}' for node_id ({})",
                    login,
                    resp.status(),
                );
                None
            }
            Err(e) => {
                tracing::warn!("HTTP error fetching user '{}' for node_id: {}", login, e);
                None
            }
        }
    }

    /// Fetch the GraphQL node ID for a pull request.
    async fn get_pr_node_id(&self, pr_number: u64) -> Option<String> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, pr_number,
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
                    "Failed to fetch PR #{} for node_id ({})",
                    pr_number,
                    resp.status(),
                );
                None
            }
            Err(e) => {
                tracing::warn!("HTTP error fetching PR #{} for node_id: {}", pr_number, e,);
                None
            }
        }
    }

    /// Request reviews on a pull request from the specified logins.
    ///
    /// Uses the GraphQL `requestReviews` mutation to assign reviewers.
    /// Logins that cannot be resolved to node IDs are skipped with a warning.
    pub async fn request_reviewers(
        &self,
        pr_number: u64,
        reviewer_logins: &[String],
    ) -> Result<()> {
        if reviewer_logins.is_empty() {
            return Ok(());
        }

        let pr_node_id = self
            .get_pr_node_id(pr_number)
            .await
            .context("Could not resolve PR node_id for review request")?;

        let mut user_ids: Vec<String> = Vec::new();
        for login in reviewer_logins {
            match self.get_user_node_id(login).await {
                Some(id) => user_ids.push(id),
                None => {
                    tracing::warn!(
                        "Could not resolve node_id for reviewer '{}'; skipping",
                        login,
                    );
                }
            }
        }

        if user_ids.is_empty() {
            bail!(
                "None of the configured reviewers ({:?}) could be resolved",
                reviewer_logins,
            );
        }

        let graphql_url = format!("{}/graphql", GITHUB_API_BASE);
        let query = serde_json::json!({
            "query": r#"mutation($prId: ID!, $userIds: [ID!]!) {
                requestReviews(input: {
                    pullRequestId: $prId,
                    userIds: $userIds
                }) {
                    pullRequest {
                        reviewRequests(first: 20) {
                            nodes {
                                requestedReviewer {
                                    ... on User { login }
                                    ... on Bot { login }
                                }
                            }
                        }
                    }
                }
            }"#,
            "variables": {
                "prId": pr_node_id,
                "userIds": user_ids,
            },
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
            .context("Failed to call GraphQL requestReviews")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "GraphQL request failed for PR #{} requestReviews ({}): {}",
                pr_number,
                status,
                body,
            );
        }

        let gql_resp: serde_json::Value = resp.json().await?;
        if let Some(errors) = gql_resp.get("errors") {
            bail!(
                "GraphQL errors requesting reviews for PR #{}: {}",
                pr_number,
                errors,
            );
        }

        tracing::info!(
            "Requested reviews on PR #{} from {:?}",
            pr_number,
            reviewer_logins,
        );
        Ok(())
    }

    /// Check the review status of a pull request.
    ///
    /// Queries the latest review from each expected reviewer and returns an
    /// aggregate status.  Reviews that are `APPROVED` or `DISMISSED` are
    /// treated as passing; `CHANGES_REQUESTED` causes the overall status
    /// to be [`ReviewStatus::ChangesRequested`]; any missing or `COMMENTED`
    /// reviews leave the status as [`ReviewStatus::Pending`].
    pub async fn check_reviews_complete(
        &self,
        pr_number: u64,
        expected_reviewers: &[String],
    ) -> Result<ReviewStatus> {
        if expected_reviewers.is_empty() {
            return Ok(ReviewStatus::Approved);
        }

        let url = format!(
            "{}/repos/{}/{}/pulls/{}/reviews",
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
            .context("Failed to fetch PR reviews")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to fetch reviews for PR #{} ({}): {}",
                pr_number,
                status,
                body,
            );
        }

        let reviews: Vec<serde_json::Value> = resp.json().await?;

        // Build a map of the latest review state per reviewer login.
        let mut latest_states: std::collections::HashMap<String, String> =
            std::collections::HashMap::new();
        for review in &reviews {
            let login = review
                .pointer("/user/login")
                .and_then(|v| v.as_str())
                .unwrap_or_default()
                .to_lowercase();
            let state = review["state"].as_str().unwrap_or_default().to_string();
            // Later entries override earlier ones (the list is chronological).
            latest_states.insert(login, state);
        }

        let mut changes_requested_by: Vec<String> = Vec::new();
        let mut all_approved = true;

        for reviewer in expected_reviewers {
            let key = reviewer.to_lowercase();
            match latest_states.get(&key).map(|s| s.as_str()) {
                Some("APPROVED") | Some("DISMISSED") => {}
                Some("CHANGES_REQUESTED") => {
                    changes_requested_by.push(reviewer.clone());
                    all_approved = false;
                }
                _ => {
                    // No review yet or only COMMENTED.
                    all_approved = false;
                }
            }
        }

        if !changes_requested_by.is_empty() {
            return Ok(ReviewStatus::ChangesRequested {
                reviewers: changes_requested_by,
            });
        }

        if all_approved {
            Ok(ReviewStatus::Approved)
        } else {
            Ok(ReviewStatus::Pending)
        }
    }

    /// Get the login of a pull request's author.
    pub async fn get_pr_author(&self, pr_number: u64) -> Result<Option<String>> {
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
            .context("Failed to fetch PR for author")?;

        if !resp.status().is_success() {
            return Ok(None);
        }

        let pr: serde_json::Value = resp.json().await?;
        Ok(pr
            .pointer("/user/login")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string()))
    }

    /// List all open pull requests in the repository.
    pub async fn list_open_prs(&self) -> Result<Vec<OpenPr>> {
        let mut prs = Vec::new();
        let mut page = 1u32;

        loop {
            let url = format!(
                "{}/repos/{}/{}/pulls?state=open&per_page=100&page={}",
                GITHUB_API_BASE, self.repo_owner, self.repo_name, page,
            );

            let resp = self
                .http
                .get(&url)
                .header("Authorization", format!("Bearer {}", self.github_token))
                .header("User-Agent", "wreck-it")
                .header("Accept", "application/vnd.github+json")
                .send()
                .await
                .context("Failed to list open PRs")?;

            if !resp.status().is_success() {
                let status = resp.status();
                let body = resp.text().await.unwrap_or_default();
                bail!("Failed to list open PRs ({}): {}", status, body);
            }

            let items: Vec<serde_json::Value> = resp.json().await?;
            if items.is_empty() {
                break;
            }

            for item in &items {
                if let Some(number) = item["number"].as_u64() {
                    prs.push(OpenPr {
                        number,
                        title: item["title"].as_str().unwrap_or("").to_string(),
                        draft: item["draft"].as_bool().unwrap_or(false),
                    });
                }
            }

            if items.len() < 100 {
                break;
            }
            page += 1;
        }

        Ok(prs)
    }

    /// Fetch the raw JSON representation of a pull request.
    pub async fn fetch_pr_json(&self, pr_number: u64) -> Result<serde_json::Value> {
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
            .context("Failed to fetch PR JSON")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Failed to fetch PR #{} ({}): {}", pr_number, status, body);
        }

        Ok(resp.json().await?)
    }

    /// Fetch the most recent commit messages on a branch.
    ///
    /// Returns a newline-separated string of `<sha_short> <message>` lines.
    pub async fn fetch_recent_commits(&self, branch: &str, count: u32) -> Result<String> {
        let url = format!(
            "{}/repos/{}/{}/commits?sha={}&per_page={}",
            GITHUB_API_BASE, self.repo_owner, self.repo_name, branch, count,
        );

        let resp = self
            .http
            .get(&url)
            .header("Authorization", format!("Bearer {}", self.github_token))
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .send()
            .await
            .context("Failed to fetch recent commits")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to fetch commits for branch {} ({}): {}",
                branch,
                status,
                body,
            );
        }

        let items: Vec<serde_json::Value> = resp.json().await?;
        let lines: Vec<String> = items
            .iter()
            .filter_map(|c| {
                let sha = c["sha"].as_str().unwrap_or("").get(..7).unwrap_or("");
                let msg = c
                    .pointer("/commit/message")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                let first_line = msg.lines().next().unwrap_or("");
                if sha.is_empty() {
                    None
                } else {
                    Some(format!("{} {}", sha, first_line))
                }
            })
            .collect();

        Ok(lines.join("\n"))
    }

    /// Fetch a summary of files changed in a pull request.
    ///
    /// Returns a newline-separated `<filename> | <changes> <status>` summary.
    pub async fn fetch_pr_files_summary(&self, pr_number: u64) -> Result<String> {
        let url = format!(
            "{}/repos/{}/{}/pulls/{}/files?per_page=100",
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
            .context("Failed to fetch PR files")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!(
                "Failed to fetch files for PR #{} ({}): {}",
                pr_number,
                status,
                body,
            );
        }

        let items: Vec<serde_json::Value> = resp.json().await?;
        let lines: Vec<String> = items
            .iter()
            .filter_map(|f| {
                let filename = f["filename"].as_str().unwrap_or("");
                let changes = f["changes"].as_u64().unwrap_or(0);
                let status = f["status"].as_str().unwrap_or("modified");
                if filename.is_empty() {
                    None
                } else {
                    Some(format!("{} | {} {}", filename, changes, status))
                }
            })
            .collect();

        Ok(lines.join("\n"))
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
        assert_eq!(client.github_token, "test-token");
        assert_eq!(client.repo_owner, "owner");
        assert_eq!(client.repo_name, "repo");
        assert!(client.authenticated_login.is_none());
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

        let pr_working = CloudAgentStatus::PrCreatedAgentWorking {
            pr_number: 7,
            pr_url: "https://github.com/o/r/pull/7".to_string(),
        };
        assert!(matches!(
            pr_working,
            CloudAgentStatus::PrCreatedAgentWorking { pr_number: 7, .. }
        ));
        // PrCreatedAgentWorking is distinct from PrCreated.
        assert_ne!(
            CloudAgentStatus::PrCreated {
                pr_number: 7,
                pr_url: "https://github.com/o/r/pull/7".to_string(),
            },
            CloudAgentStatus::PrCreatedAgentWorking {
                pr_number: 7,
                pr_url: "https://github.com/o/r/pull/7".to_string(),
            },
        );

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
        assert_eq!(
            PrMergeStatus::AgentWorkInProgress,
            PrMergeStatus::AgentWorkInProgress
        );
        assert_eq!(PrMergeStatus::NotMergeable, PrMergeStatus::NotMergeable);
        assert_eq!(PrMergeStatus::Mergeable, PrMergeStatus::Mergeable);
        assert_eq!(PrMergeStatus::AlreadyMerged, PrMergeStatus::AlreadyMerged);
        assert_ne!(PrMergeStatus::Draft, PrMergeStatus::Mergeable);
        assert_ne!(PrMergeStatus::AlreadyMerged, PrMergeStatus::NotMergeable);
        assert_ne!(
            PrMergeStatus::AgentWorkInProgress,
            PrMergeStatus::NotMergeable
        );
    }

    #[test]
    fn is_wip_title_detects_prefix() {
        assert!(is_wip_title("[wip] some feature"));
        assert!(is_wip_title("[WIP] another feature"));
        assert!(is_wip_title("[Wip] mixed case"));
        assert!(is_wip_title("[WIP]no space"));
    }

    #[test]
    fn is_wip_title_rejects_non_wip() {
        assert!(!is_wip_title("some feature"));
        assert!(!is_wip_title("fix: [wip] embedded"));
        assert!(!is_wip_title(""));
        assert!(!is_wip_title("[wi"));
    }

    #[test]
    fn is_wip_title_leading_whitespace() {
        assert!(is_wip_title("  [wip] with leading space"));
    }

    // ---- parse_copilot_session_status tests ----

    #[test]
    fn copilot_session_completed() {
        let resp = serde_json::json!({
            "data": { "repository": { "pullRequest": { "timelineItems": {
                "nodes": [{
                    "__typename": "CopilotPullRequestSessionEvent",
                    "createdAt": "2025-01-01T00:00:00Z",
                    "copilotSession": { "completedAt": "2025-01-01T01:00:00Z" }
                }]
            }}}}
        });
        assert_eq!(parse_copilot_session_status(&resp), Some(true));
    }

    #[test]
    fn copilot_session_in_progress() {
        let resp = serde_json::json!({
            "data": { "repository": { "pullRequest": { "timelineItems": {
                "nodes": [{
                    "__typename": "CopilotPullRequestSessionEvent",
                    "createdAt": "2025-01-01T00:00:00Z",
                    "copilotSession": { "completedAt": null }
                }]
            }}}}
        });
        assert_eq!(parse_copilot_session_status(&resp), Some(false));
    }

    #[test]
    fn copilot_session_no_nodes() {
        let resp = serde_json::json!({
            "data": { "repository": { "pullRequest": { "timelineItems": {
                "nodes": []
            }}}}
        });
        assert_eq!(parse_copilot_session_status(&resp), None);
    }

    #[test]
    fn copilot_session_graphql_errors() {
        let resp = serde_json::json!({
            "errors": [{"message": "Field not found"}]
        });
        assert_eq!(parse_copilot_session_status(&resp), None);
    }

    #[test]
    fn copilot_session_unexpected_shape() {
        let resp = serde_json::json!({ "data": {} });
        assert_eq!(parse_copilot_session_status(&resp), None);
    }

    #[test]
    fn copilot_session_multiple_takes_latest() {
        let resp = serde_json::json!({
            "data": { "repository": { "pullRequest": { "timelineItems": {
                "nodes": [
                    {
                        "copilotSession": { "completedAt": "2025-01-01T01:00:00Z" }
                    },
                    {
                        "copilotSession": { "completedAt": null }
                    }
                ]
            }}}}
        });
        // The last node (most recent) is in-progress.
        assert_eq!(parse_copilot_session_status(&resp), Some(false));
    }

    #[test]
    fn partition_assignees_mixed() {
        let issue = serde_json::json!({
            "assignees": [
                {"login": "copilot"},
                {"login": "user1"},
                {"login": "copilot-swe-agent"},
                {"login": "reviewer"},
            ]
        });
        let (agents, others) = partition_assignees(&issue);
        assert_eq!(agents, vec!["copilot", "copilot-swe-agent"]);
        assert_eq!(others, vec!["user1", "reviewer"]);
    }

    #[test]
    fn partition_assignees_agent_only() {
        let issue = serde_json::json!({
            "assignees": [{"login": "copilot"}]
        });
        let (agents, others) = partition_assignees(&issue);
        assert_eq!(agents, vec!["copilot"]);
        assert!(others.is_empty());
    }

    #[test]
    fn partition_assignees_empty() {
        let issue = serde_json::json!({ "assignees": [] });
        let (agents, others) = partition_assignees(&issue);
        assert!(agents.is_empty());
        assert!(others.is_empty());
    }

    #[test]
    fn partition_assignees_no_field() {
        let issue = serde_json::json!({});
        let (agents, others) = partition_assignees(&issue);
        assert!(agents.is_empty());
        assert!(others.is_empty());
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

    #[test]
    fn is_copilot_in_assignees_copilot_swe_agent() {
        let issue = serde_json::json!({
            "assignees": [{"login": "copilot-swe-agent"}]
        });
        assert!(is_copilot_in_assignees(&issue));
    }

    #[test]
    fn is_copilot_in_assignees_claude() {
        let issue = serde_json::json!({
            "assignees": [{"login": "claude"}]
        });
        assert!(is_copilot_in_assignees(&issue));
    }

    #[test]
    fn is_copilot_in_assignees_codex() {
        let issue = serde_json::json!({
            "assignees": [{"login": "codex"}]
        });
        assert!(is_copilot_in_assignees(&issue));
    }

    #[test]
    fn known_agent_logins_contains_expected_entries() {
        assert!(KNOWN_AGENT_LOGINS.contains(&"copilot-swe-agent"));
        assert!(KNOWN_AGENT_LOGINS.contains(&"copilot"));
        assert!(KNOWN_AGENT_LOGINS.contains(&"claude"));
        assert!(KNOWN_AGENT_LOGINS.contains(&"codex"));
    }

    // ---- build_issue_body tests ----

    #[test]
    fn build_issue_body_without_memory() {
        let body = build_issue_body("task-1", "Implement feature X", &[], None, None);
        assert!(body.contains("Implement feature X"));
        assert!(body.contains("task `task-1`"));
        assert!(!body.contains("Previous Context"));
        assert!(!body.contains("## Branch"));
    }

    #[test]
    fn build_issue_body_with_memory() {
        let memory = vec![
            "iteration 1: triggered cloud agent for task setup (issue #10)".to_string(),
            "iteration 2: agent created PR #5 for task setup".to_string(),
        ];
        let body = build_issue_body("task-2", "Add test coverage", &memory, None, None);
        assert!(body.contains("Add test coverage"));
        assert!(body.contains("task `task-2`"));
        assert!(body.contains("Previous Context"));
        assert!(body.contains("iteration 1: triggered cloud agent for task setup (issue #10)"));
        assert!(body.contains("iteration 2: agent created PR #5 for task setup"));
    }

    #[test]
    fn build_issue_body_memory_placed_before_footer() {
        let memory = vec!["some context".to_string()];
        let body = build_issue_body("t", "desc", &memory, None, None);
        let context_pos = body.find("Previous Context").unwrap();
        let footer_pos = body.find("Triggered by wreck-it").unwrap();
        assert!(
            context_pos < footer_pos,
            "memory context should appear before the footer"
        );
    }

    #[test]
    fn build_issue_body_with_branch() {
        let body = build_issue_body(
            "task-1",
            "Implement feature X",
            &[],
            Some("feature/my-branch"),
            None,
        );
        assert!(body.contains("Implement feature X"));
        assert!(body.contains("## Branch"));
        assert!(body.contains("Base your work on the `feature/my-branch` branch"));
        assert!(body.contains("target your pull request to `feature/my-branch`"));
    }

    #[test]
    fn build_issue_body_with_branch_and_memory() {
        let memory = vec!["iteration 1: something".to_string()];
        let body = build_issue_body("task-1", "desc", &memory, Some("dev"), None);
        // Branch section should come before memory section
        let branch_pos = body.find("## Branch").unwrap();
        let memory_pos = body.find("Previous Context").unwrap();
        let footer_pos = body.find("Triggered by wreck-it").unwrap();
        assert!(branch_pos < memory_pos);
        assert!(memory_pos < footer_pos);
    }

    #[test]
    fn build_issue_body_with_system_prompt() {
        let body = build_issue_body(
            "task-1",
            "Implement feature X",
            &[],
            None,
            Some("You are a Rust expert."),
        );
        assert!(body.contains("<!-- system-prompt -->"));
        assert!(body.contains("You are a Rust expert."));
        assert!(body.contains("<!-- /system-prompt -->"));
        // System prompt should appear before description
        let sp_pos = body.find("<!-- system-prompt -->").unwrap();
        let desc_pos = body.find("Implement feature X").unwrap();
        assert!(sp_pos < desc_pos);
    }

    #[test]
    fn build_issue_body_without_system_prompt_has_no_marker() {
        let body = build_issue_body("task-1", "desc", &[], None, None);
        assert!(!body.contains("<!-- system-prompt -->"));
    }

    #[test]
    fn open_pr_struct_stores_fields() {
        let pr = OpenPr {
            number: 42,
            title: "Fix the thing".to_string(),
            draft: true,
        };
        assert_eq!(pr.number, 42);
        assert_eq!(pr.title, "Fix the thing");
        assert!(pr.draft);
    }

    #[test]
    fn open_pr_clone() {
        let pr = OpenPr {
            number: 1,
            title: "PR title".to_string(),
            draft: false,
        };
        let cloned = pr.clone();
        assert_eq!(cloned.number, pr.number);
        assert_eq!(cloned.title, pr.title);
        assert_eq!(cloned.draft, pr.draft);
    }

    // ---- ReviewStatus tests ----

    #[test]
    fn review_status_pending() {
        let status = ReviewStatus::Pending;
        assert_eq!(status, ReviewStatus::Pending);
    }

    #[test]
    fn review_status_approved() {
        let status = ReviewStatus::Approved;
        assert_eq!(status, ReviewStatus::Approved);
        assert_ne!(status, ReviewStatus::Pending);
    }

    #[test]
    fn review_status_changes_requested() {
        let status = ReviewStatus::ChangesRequested {
            reviewers: vec!["alice".to_string()],
        };
        assert!(matches!(
            status,
            ReviewStatus::ChangesRequested { ref reviewers } if reviewers == &["alice"]
        ));
    }

    #[test]
    fn review_status_changes_requested_multiple() {
        let status = ReviewStatus::ChangesRequested {
            reviewers: vec!["alice".to_string(), "bob".to_string()],
        };
        if let ReviewStatus::ChangesRequested { reviewers } = status {
            assert_eq!(reviewers.len(), 2);
        } else {
            panic!("expected ChangesRequested");
        }
    }

    #[test]
    fn review_status_variants_are_distinct() {
        assert_ne!(ReviewStatus::Pending, ReviewStatus::Approved);
        assert_ne!(
            ReviewStatus::Approved,
            ReviewStatus::ChangesRequested { reviewers: vec![] },
        );
    }

    // ---- preferred_agent tests ----

    #[test]
    fn preferred_agent_defaults_to_none() {
        let client =
            CloudAgentClient::new("token".to_string(), "owner".to_string(), "repo".to_string());
        assert!(client.preferred_agent.is_none());
    }

    #[test]
    fn set_preferred_agent_stores_value() {
        let mut client =
            CloudAgentClient::new("token".to_string(), "owner".to_string(), "repo".to_string());
        client.set_preferred_agent(Some("claude".to_string()));
        assert_eq!(client.preferred_agent.as_deref(), Some("claude"));
    }

    #[test]
    fn set_preferred_agent_can_clear() {
        let mut client =
            CloudAgentClient::new("token".to_string(), "owner".to_string(), "repo".to_string());
        client.set_preferred_agent(Some("claude".to_string()));
        client.set_preferred_agent(None);
        assert!(client.preferred_agent.is_none());
    }
}
