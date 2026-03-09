//! GitHub API client for reading and writing repository contents.
//!
//! Uses the Cloudflare Worker Fetch API (via the `worker` crate) to
//! communicate with the GitHub REST API.  This avoids pulling in `reqwest`
//! or other HTTP clients that do not compile to WASM.

use serde::{Deserialize, Serialize};
use worker::Fetch;

/// A lightweight GitHub API client scoped to a single repository.
pub struct GitHubClient {
    owner: String,
    repo: String,
    token: String,
}

// ---------------------------------------------------------------------------
// GitHub REST API response types
// ---------------------------------------------------------------------------

/// Represents a file returned by the Contents API.
#[derive(Debug, Deserialize)]
pub struct ContentFile {
    /// Base64-encoded file content (present for files, absent for dirs).
    pub content: Option<String>,
    /// Git blob SHA — required when updating an existing file.
    pub sha: String,
}

/// Payload sent to the Contents API to create or update a file.
#[derive(Serialize)]
struct UpsertFileRequest<'a> {
    message: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha: Option<&'a str>,
    branch: &'a str,
}

/// Represents a Git reference (branch pointer).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub object: GitRefObject,
}

/// Merge readiness of a pull request.
#[derive(Debug, Clone, PartialEq, Eq)]
#[allow(dead_code)]
pub enum PrMergeStatus {
    /// The PR is a draft.
    Draft,
    /// The PR is not yet mergeable (checks pending, conflicts, etc.).
    NotMergeable,
    /// The PR is ready to be merged.
    Mergeable,
    /// The PR has already been merged.
    AlreadyMerged,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitRefObject {
    pub sha: String,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl GitHubClient {
    /// Create a new client for the given repository.
    pub fn new(
        owner: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            token: token.into(),
        }
    }

    /// Read a file from the repository.  Returns `None` if the file does not
    /// exist (404).
    pub async fn get_file(&self, path: &str, branch: &str) -> Result<Option<ContentFile>, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(path),
            url_encode(branch),
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() == 404 {
            return Ok(None);
        }

        if response.status_code() != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "GitHub API returned {}: {}",
                response.status_code(),
                body
            ));
        }

        let file: ContentFile = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse GitHub response: {e}"))?;

        Ok(Some(file))
    }

    /// Decode the base64 content from a [`ContentFile`].
    pub fn decode_content(file: &ContentFile) -> Result<String, String> {
        let encoded = file.content.as_deref().unwrap_or("");
        // GitHub returns base64 with newlines; strip them before decoding.
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64_decode(&cleaned)?;
        String::from_utf8(bytes).map_err(|e| format!("Content is not valid UTF-8: {e}"))
    }

    /// Create or update a file on the given branch.
    ///
    /// If `sha` is `Some`, this is an update to an existing file; otherwise a
    /// new file is created.
    pub async fn put_file(
        &self,
        path: &str,
        branch: &str,
        content: &str,
        message: &str,
        sha: Option<&str>,
    ) -> Result<(), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(path),
        );

        let encoded = base64_encode(content.as_bytes());
        let body = UpsertFileRequest {
            message,
            content: &encoded,
            sha,
            branch,
        };
        let body_json =
            serde_json::to_string(&body).map_err(|e| format!("Failed to serialize body: {e}"))?;

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Put)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        let status = response.status_code();
        if status != 200 && status != 201 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("GitHub API returned {status}: {body}"));
        }

        Ok(())
    }

    /// Check whether a branch exists on the remote.
    #[allow(dead_code)]
    pub async fn branch_exists(&self, branch: &str) -> Result<bool, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/ref/heads/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(branch),
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        Ok(response.status_code() == 200)
    }

    #[allow(dead_code)]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    #[allow(dead_code)]
    pub fn repo(&self) -> &str {
        &self.repo
    }

    // -----------------------------------------------------------------------
    // Issue creation
    // -----------------------------------------------------------------------

    /// Create a GitHub issue with the given title, body, and labels.
    ///
    /// Returns `(issue_number, Option<node_id>)`.
    pub async fn create_issue(
        &self,
        title: &str,
        body: &str,
        labels: &[&str],
    ) -> Result<(u64, Option<String>), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues",
            url_encode(&self.owner),
            url_encode(&self.repo),
        );

        let payload = serde_json::json!({
            "title": title,
            "body": body,
            "labels": labels,
        });
        let body_json =
            serde_json::to_string(&payload).map_err(|e| format!("Failed to serialize: {e}"))?;

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Post)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        let status = response.status_code();
        if status != 201 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Failed to create issue ({status}): {body}"));
        }

        let issue: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse issue response: {e}"))?;

        let number = issue["number"]
            .as_u64()
            .ok_or_else(|| "Missing issue number in response".to_string())?;
        let node_id = issue["node_id"].as_str().map(|s| s.to_string());

        Ok((number, node_id))
    }

    // -----------------------------------------------------------------------
    // GraphQL helpers
    // -----------------------------------------------------------------------

    /// Execute a GraphQL query/mutation against the GitHub API.
    async fn graphql(
        &self,
        query: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = "https://api.github.com/graphql";
        let body_json = serde_json::to_string(query)
            .map_err(|e| format!("Failed to serialize GraphQL query: {e}"))?;

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            url,
            worker::RequestInit::new()
                .with_method(worker::Method::Post)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        )
        .map_err(|e| format!("Failed to create GraphQL request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GraphQL request failed: {e}"))?;

        if response.status_code() != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "GraphQL returned {}: {body}",
                response.status_code()
            ));
        }

        response
            .json()
            .await
            .map_err(|e| format!("Failed to parse GraphQL response: {e}"))
    }

    // -----------------------------------------------------------------------
    // Agent discovery and assignment
    // -----------------------------------------------------------------------

    /// Known coding agent logins, tried in priority order.
    const KNOWN_AGENT_LOGINS: &[&str] =
        &["copilot-swe-agent", "copilot", "claude", "codex"];

    /// Discover a coding agent via the `suggestedActors` GraphQL query.
    ///
    /// Returns `(agent_node_id, agent_login)` of the first known agent, or
    /// `None` if none could be found.
    pub async fn get_suggested_agent(&self) -> Option<(String, String)> {
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
                "owner": self.owner,
                "name": self.repo,
            },
        });

        let body = match self.graphql(&query).await {
            Ok(v) => v,
            Err(e) => {
                worker::console_warn!("suggestedActors query failed: {e}");
                return None;
            }
        };

        if body.get("errors").is_some() {
            worker::console_warn!("GraphQL errors in suggestedActors: {}", body["errors"]);
            return None;
        }

        let nodes = body
            .pointer("/data/repository/suggestedActors/nodes")
            .and_then(|v| v.as_array())?;

        for &known in Self::KNOWN_AGENT_LOGINS {
            for node in nodes {
                let node_login = node["login"].as_str().unwrap_or_default();
                if node_login.eq_ignore_ascii_case(known) {
                    if let Some(id) = node["id"].as_str() {
                        return Some((id.to_string(), node_login.to_string()));
                    }
                }
            }
        }

        worker::console_warn!(
            "No known coding agent found in suggestedActors (searched {:?})",
            Self::KNOWN_AGENT_LOGINS,
        );
        None
    }

    /// Fetch the GraphQL node ID for an issue.
    async fn get_issue_node_id(&self, issue_number: u64) -> Option<String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            issue_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .ok()?;

        let mut response = Fetch::Request(request).send().await.ok()?;

        if response.status_code() != 200 {
            return None;
        }

        let body: serde_json::Value = response.json().await.ok()?;
        body["node_id"].as_str().map(|s| s.to_string())
    }

    /// Assign a coding agent to an issue, triggering autonomous work.
    ///
    /// Discovers an agent via `suggestedActors`, then uses the
    /// `addAssigneesToAssignable` GraphQL mutation.  Returns `true` on success.
    pub async fn assign_agent(
        &self,
        issue_number: u64,
        issue_node_id: Option<&str>,
    ) -> bool {
        // Resolve the issue node ID if not provided.
        let owned_node_id;
        let assignable_id = match issue_node_id {
            Some(id) => id,
            None => match self.get_issue_node_id(issue_number).await {
                Some(id) => {
                    owned_node_id = id;
                    owned_node_id.as_str()
                }
                None => {
                    worker::console_warn!(
                        "Could not resolve node_id for issue #{}",
                        issue_number,
                    );
                    return false;
                }
            },
        };

        // Discover the agent.
        let (agent_id, agent_login) = match self.get_suggested_agent().await {
            Some(pair) => pair,
            None => return false,
        };

        // Assign via GraphQL mutation.
        let query = serde_json::json!({
            "query": r#"mutation($assignableId: ID!, $assigneeIds: [ID!]!) {
                addAssigneesToAssignable(input: {
                    assignableId: $assignableId,
                    assigneeIds: $assigneeIds
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
                "assigneeIds": [agent_id],
            },
        });

        let mut gql_headers = worker::Headers::new();
        gql_headers
            .set(
                "GraphQL-Features",
                "issues_copilot_assignment_api_support,coding_agent_model_selection",
            )
            .ok();

        // Use the graphql helper but with the extra header.
        let url = "https://api.github.com/graphql";
        let body_json = match serde_json::to_string(&query) {
            Ok(j) => j,
            Err(_) => return false,
        };

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();
        headers
            .set(
                "GraphQL-Features",
                "issues_copilot_assignment_api_support,coding_agent_model_selection",
            )
            .ok();

        let request = match worker::Request::new_with_init(
            url,
            worker::RequestInit::new()
                .with_method(worker::Method::Post)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        ) {
            Ok(r) => r,
            Err(_) => return false,
        };

        let mut response = match Fetch::Request(request).send().await {
            Ok(r) => r,
            Err(e) => {
                worker::console_warn!("Agent assignment request failed: {e}");
                return false;
            }
        };

        if response.status_code() != 200 {
            worker::console_warn!(
                "Agent assignment returned {}",
                response.status_code(),
            );
            return false;
        }

        let gql_resp: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(_) => return false,
        };

        if gql_resp.get("errors").is_some() {
            worker::console_warn!(
                "GraphQL errors assigning '{}' to issue #{}: {}",
                agent_login,
                issue_number,
                gql_resp["errors"],
            );
            return false;
        }

        // Verify the agent appears in the assignees.
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
            worker::console_log!(
                "Assigned '{}' to issue #{} — agent triggered",
                agent_login,
                issue_number,
            );
        }

        has_agent
    }

    // -----------------------------------------------------------------------
    // Pull request management
    // -----------------------------------------------------------------------

    /// Check the merge readiness of a pull request.
    pub async fn check_pr_merge_status(&self, pr_number: u64) -> Result<PrMergeStatus, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if !matches!(response.status_code(), 200) {
            // Treat API errors as not-mergeable to avoid blocking the loop.
            // Matches the CLI's conservative approach.
            return Ok(PrMergeStatus::NotMergeable);
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let merged = pr["merged"].as_bool().unwrap_or(false);
        if merged {
            return Ok(PrMergeStatus::AlreadyMerged);
        }
        let state = pr["state"].as_str().unwrap_or("unknown");
        if state != "open" {
            return Ok(PrMergeStatus::NotMergeable);
        }
        if pr["draft"].as_bool().unwrap_or(false) {
            return Ok(PrMergeStatus::Draft);
        }
        if pr["mergeable"].as_bool().unwrap_or(false) {
            Ok(PrMergeStatus::Mergeable)
        } else {
            Ok(PrMergeStatus::NotMergeable)
        }
    }

    /// Merge a pull request using a squash merge.
    pub async fn merge_pr(&self, pr_number: u64) -> Result<(), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}/merge",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let body_json = r#"{"merge_method":"squash"}"#;

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Put)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(body_json))),
        )
        .map_err(|e| format!("Failed to create merge request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("Merge request failed: {e}"))?;

        let status = response.status_code();
        if status != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("Failed to merge PR #{pr_number} ({status}): {body}"));
        }

        worker::console_log!("Merged PR #{}", pr_number);
        Ok(())
    }

    /// Enable auto-merge on a pull request (squash method).
    pub async fn enable_auto_merge(&self, pr_number: u64) -> Result<(), String> {
        // Fetch the PR node_id.
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &pr_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to fetch PR #{pr_number} ({}): {body}",
                response.status_code(),
            ));
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let node_id = pr["node_id"]
            .as_str()
            .ok_or_else(|| "Missing node_id in PR response".to_string())?;

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

        let gql_resp = self.graphql(&query).await?;

        if let Some(errors) = gql_resp.get("errors") {
            return Err(format!(
                "GraphQL errors enabling auto-merge for PR #{pr_number}: {errors}"
            ));
        }

        worker::console_log!("Enabled auto-merge for PR #{}", pr_number);
        Ok(())
    }

    /// Mark a draft PR as ready for review.
    pub async fn mark_pr_ready_for_review(&self, pr_number: u64) -> Result<(), String> {
        // Fetch the PR node_id.
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &pr_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to fetch PR #{pr_number} ({}): {body}",
                response.status_code(),
            ));
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let node_id = pr["node_id"]
            .as_str()
            .ok_or_else(|| "Missing node_id in PR response".to_string())?;

        let query = serde_json::json!({
            "query": "mutation($prId: ID!) { markPullRequestReadyForReview(input: { pullRequestId: $prId }) { pullRequest { isDraft } } }",
            "variables": { "prId": node_id },
        });

        let gql_resp = self.graphql(&query).await?;

        if let Some(errors) = gql_resp.get("errors") {
            return Err(format!(
                "GraphQL errors marking PR #{pr_number} ready: {errors}"
            ));
        }

        worker::console_log!("Marked PR #{} as ready for review", pr_number);
        Ok(())
    }

    /// Check whether the base branch of a PR has required status checks.
    pub async fn has_required_checks_for_pr(&self, pr_number: u64) -> Result<bool, String> {
        // Fetch the PR to determine the base branch.
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &pr_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() != 200 {
            return Ok(false);
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let base_branch = pr
            .pointer("/base/ref")
            .and_then(|v| v.as_str())
            .unwrap_or("main");

        // Check legacy branch protection.
        let protection_url = format!(
            "https://api.github.com/repos/{}/{}/branches/{}/protection/required_status_checks",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(base_branch),
        );

        let mut prot_headers = worker::Headers::new();
        prot_headers.set("Accept", "application/vnd.github+json").ok();
        prot_headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        prot_headers.set("User-Agent", "wreck-it-worker").ok();
        prot_headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let prot_request = worker::Request::new_with_init(
            &protection_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(prot_headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let prot_response = Fetch::Request(prot_request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if prot_response.status_code() == 200 {
            return Ok(true);
        }

        // Legacy branch protection didn't report required checks.  Fall back
        // to the repository rulesets API which covers the newer GitHub
        // rulesets that are not reflected by the legacy endpoint.
        let rules_url = format!(
            "https://api.github.com/repos/{}/{}/rules/branches/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(base_branch),
        );

        let mut rules_headers = worker::Headers::new();
        rules_headers
            .set("Accept", "application/vnd.github+json")
            .ok();
        rules_headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        rules_headers.set("User-Agent", "wreck-it-worker").ok();
        rules_headers
            .set("X-GitHub-Api-Version", "2022-11-28")
            .ok();

        let rules_request = worker::Request::new_with_init(
            &rules_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(rules_headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut rules_response = Fetch::Request(rules_request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if rules_response.status_code() != 200 {
            return Ok(false);
        }

        let rules: serde_json::Value = rules_response
            .json()
            .await
            .map_err(|e| format!("Failed to parse rules response: {e}"))?;

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
    pub async fn has_pending_checks_for_pr(&self, pr_number: u64) -> Result<bool, String> {
        // Fetch the PR to obtain the head SHA.
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &pr_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() != 200 {
            return Ok(false);
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let head_sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing head SHA in PR response".to_string())?;

        for status in &["queued", "in_progress"] {
            let checks_url = format!(
                "https://api.github.com/repos/{}/{}/commits/{}/check-runs?status={}&per_page=1",
                url_encode(&self.owner),
                url_encode(&self.repo),
                url_encode(head_sha),
                status,
            );

            let mut chk_headers = worker::Headers::new();
            chk_headers.set("Accept", "application/vnd.github+json").ok();
            chk_headers
                .set("Authorization", &format!("Bearer {}", self.token))
                .ok();
            chk_headers.set("User-Agent", "wreck-it-worker").ok();
            chk_headers
                .set("X-GitHub-Api-Version", "2022-11-28")
                .ok();

            let chk_request = worker::Request::new_with_init(
                &checks_url,
                worker::RequestInit::new()
                    .with_method(worker::Method::Get)
                    .with_headers(chk_headers),
            )
            .map_err(|e| format!("Failed to create request: {e}"))?;

            let mut chk_response = Fetch::Request(chk_request)
                .send()
                .await
                .map_err(|e| format!("GitHub API request failed: {e}"))?;

            if chk_response.status_code() != 200 {
                continue;
            }

            let body: serde_json::Value = chk_response
                .json()
                .await
                .map_err(|e| format!("Failed to parse check runs response: {e}"))?;

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
    pub async fn has_failing_checks_for_pr(&self, pr_number: u64) -> Result<bool, String> {
        // Fetch the PR to obtain the head SHA.
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &pr_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() != 200 {
            return Ok(false);
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let head_sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing head SHA in PR response".to_string())?;

        // Query check runs for the head SHA, filtering by status=completed.
        // Note: fetches up to 100 results (one page).  In practice, a single
        // commit rarely exceeds 100 completed check runs.
        let checks_url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}/check-runs?status=completed&per_page=100",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(head_sha),
        );

        let mut chk_headers = worker::Headers::new();
        chk_headers.set("Accept", "application/vnd.github+json").ok();
        chk_headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        chk_headers.set("User-Agent", "wreck-it-worker").ok();
        chk_headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let chk_request = worker::Request::new_with_init(
            &checks_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(chk_headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut chk_response = Fetch::Request(chk_request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if chk_response.status_code() != 200 {
            return Ok(false);
        }

        let body: serde_json::Value = chk_response
            .json()
            .await
            .map_err(|e| format!("Failed to parse check runs response: {e}"))?;

        let has_failure = body["check_runs"]
            .as_array()
            .map(|runs| {
                runs.iter().any(|r| {
                    r["conclusion"].as_str() == Some("failure")
                })
            })
            .unwrap_or(false);

        Ok(has_failure)
    }

    /// Post a comment on a pull request (via the issues comments API).
    pub async fn comment_on_pr(&self, pr_number: u64, body: &str) -> Result<(), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/issues/{}/comments",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let payload = serde_json::json!({ "body": body });
        let body_json =
            serde_json::to_string(&payload).map_err(|e| format!("Failed to serialize: {e}"))?;

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Post)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        )
        .map_err(|e| format!("Failed to create comment request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("Comment request failed: {e}"))?;

        let status = response.status_code();
        if status != 201 {
            let resp_body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to comment on PR #{pr_number} ({status}): {resp_body}"
            ));
        }

        worker::console_log!("Posted comment on PR #{}", pr_number);
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
    pub async fn approve_pending_workflow_runs(&self, pr_number: u64) -> Result<(), String> {
        // Fetch the PR to obtain the head SHA.
        let pr_url = format!(
            "https://api.github.com/repos/{}/{}/pulls/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            pr_number,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &pr_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "Failed to fetch PR #{pr_number} ({}): {body}",
                response.status_code(),
            ));
        }

        let pr: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse PR response: {e}"))?;

        let head_sha = pr
            .pointer("/head/sha")
            .and_then(|v| v.as_str())
            .ok_or_else(|| "Missing head SHA in PR response".to_string())?;

        // Query workflow runs across all statuses that may indicate a run is
        // waiting for approval.  Depending on the repository configuration,
        // GitHub may place pending runs in `action_required` (fork PRs),
        // `pending` (first-time contributors), or `waiting` (outside
        // collaborators / deployment protection rules).
        let mut all_run_ids: Vec<u64> = Vec::new();

        for status_filter in &["action_required", "pending", "waiting"] {
            let runs_url = format!(
                "https://api.github.com/repos/{}/{}/actions/runs?head_sha={}&status={}",
                url_encode(&self.owner),
                url_encode(&self.repo),
                url_encode(head_sha),
                status_filter,
            );

            let mut run_headers = worker::Headers::new();
            run_headers.set("Accept", "application/vnd.github+json").ok();
            run_headers
                .set("Authorization", &format!("Bearer {}", self.token))
                .ok();
            run_headers.set("User-Agent", "wreck-it-worker").ok();
            run_headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

            let run_request = match worker::Request::new_with_init(
                &runs_url,
                worker::RequestInit::new()
                    .with_method(worker::Method::Get)
                    .with_headers(run_headers),
            ) {
                Ok(r) => r,
                Err(e) => {
                    worker::console_warn!(
                        "Failed to create {} workflow runs request for PR #{}: {}",
                        status_filter,
                        pr_number,
                        e,
                    );
                    continue;
                }
            };

            let mut run_response = match Fetch::Request(run_request).send().await {
                Ok(r) => r,
                Err(e) => {
                    worker::console_warn!(
                        "Failed to list {} workflow runs for PR #{}: {}",
                        status_filter,
                        pr_number,
                        e,
                    );
                    continue;
                }
            };

            if run_response.status_code() != 200 {
                let body = run_response.text().await.unwrap_or_default();
                worker::console_warn!(
                    "Failed to list {} workflow runs for PR #{} ({}): {}",
                    status_filter,
                    pr_number,
                    run_response.status_code(),
                    body,
                );
                continue;
            }

            let runs: serde_json::Value = match run_response.json().await {
                Ok(v) => v,
                Err(e) => {
                    worker::console_warn!(
                        "Failed to parse {} workflow runs for PR #{}: {}",
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
                            worker::console_log!(
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
            return Ok(());
        }

        let mut approved_count: usize = 0;

        for run_id in &all_run_ids {
            // Attempt the REST approval endpoint.  This is the standard
            // mechanism for approving workflow runs that require manual
            // approval (fork PRs, first-time contributors, etc.).
            let approve_url = format!(
                "https://api.github.com/repos/{}/{}/actions/runs/{}/approve",
                url_encode(&self.owner),
                url_encode(&self.repo),
                run_id,
            );

            let mut approve_headers = worker::Headers::new();
            approve_headers
                .set("Accept", "application/vnd.github+json")
                .ok();
            approve_headers
                .set("Authorization", &format!("Bearer {}", self.token))
                .ok();
            approve_headers.set("User-Agent", "wreck-it-worker").ok();
            approve_headers
                .set("X-GitHub-Api-Version", "2022-11-28")
                .ok();

            let approve_request = match worker::Request::new_with_init(
                &approve_url,
                worker::RequestInit::new()
                    .with_method(worker::Method::Post)
                    .with_headers(approve_headers),
            ) {
                Ok(r) => r,
                Err(_) => continue,
            };

            match Fetch::Request(approve_request).send().await {
                Ok(r) if r.status_code() < 300 => {
                    worker::console_log!(
                        "Approved workflow run {} via REST for PR #{}",
                        run_id,
                        pr_number,
                    );
                    approved_count += 1;
                    continue;
                }
                Ok(mut r) => {
                    let status = r.status_code();
                    let body = r.text().await.unwrap_or_default();
                    worker::console_log!(
                        "REST approve returned {} for workflow run {} on PR #{}: {}",
                        status,
                        run_id,
                        pr_number,
                        body,
                    );
                }
                Err(e) => {
                    worker::console_warn!(
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
            if self
                .approve_pending_deployments(*run_id, pr_number)
                .await
            {
                approved_count += 1;
            }
        }

        if approved_count > 0 {
            worker::console_log!(
                "Approved {} of {} pending workflow run(s) for PR #{}",
                approved_count,
                all_run_ids.len(),
                pr_number,
            );
        } else {
            worker::console_warn!(
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
            "https://api.github.com/repos/{}/{}/actions/runs/{}/pending_deployments",
            url_encode(&self.owner),
            url_encode(&self.repo),
            run_id,
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = match worker::Request::new_with_init(
            &pending_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        ) {
            Ok(r) => r,
            Err(_) => return false,
        };

        let mut response = match Fetch::Request(request).send().await {
            Ok(r) => r,
            Err(_) => return false,
        };

        if response.status_code() != 200 {
            return false;
        }

        let deployments: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(_) => return false,
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
        let body_json = match serde_json::to_string(&approve_body) {
            Ok(s) => s,
            Err(_) => return false,
        };

        let mut post_headers = worker::Headers::new();
        post_headers
            .set("Accept", "application/vnd.github+json")
            .ok();
        post_headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        post_headers.set("User-Agent", "wreck-it-worker").ok();
        post_headers
            .set("X-GitHub-Api-Version", "2022-11-28")
            .ok();
        post_headers.set("Content-Type", "application/json").ok();

        let post_request = match worker::Request::new_with_init(
            &pending_url,
            worker::RequestInit::new()
                .with_method(worker::Method::Post)
                .with_headers(post_headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        ) {
            Ok(r) => r,
            Err(_) => return false,
        };

        match Fetch::Request(post_request).send().await {
            Ok(r) if r.status_code() < 300 => {
                worker::console_log!(
                    "Approved pending deployments for workflow run {} (PR #{})",
                    run_id,
                    pr_number,
                );
                true
            }
            _ => false,
        }
    }
}

// ---------------------------------------------------------------------------
// URL encoding helper
// ---------------------------------------------------------------------------

/// Percent-encode a string for use in a URL path segment or query value.
/// Encodes all characters except unreserved ones (RFC 3986 §2.3): A-Z a-z 0-9 - . _ ~
/// Also preserves `/` in path segments since the GitHub Contents API expects paths like `dir/file`.
fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                result.push(byte as char)
            }
            _ => {
                result.push('%');
                result.push(HEX_CHARS[(byte >> 4) as usize] as char);
                result.push(HEX_CHARS[(byte & 0x0F) as usize] as char);
            }
        }
    }
    result
}

const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";

// ---------------------------------------------------------------------------
// Base64 helpers (no external crate needed for this minimal subset)
// ---------------------------------------------------------------------------

const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);
    let mut accum: u32 = 0;
    let mut bits: u32 = 0;

    for ch in input.bytes() {
        let val = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            _ => continue,
        };
        accum = (accum << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push((accum >> bits) as u8);
            accum &= (1 << bits) - 1;
        }
    }
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let original = "Hello, wreck-it!";
        let encoded = base64_encode(original.as_bytes());
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), original);
    }

    #[test]
    fn base64_encode_known() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b"Hi"), "SGk=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn base64_decode_with_newlines() {
        // GitHub API returns base64 with embedded newlines.
        let encoded = "SGVs\nbG8=";
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let decoded = base64_decode(&cleaned).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "Hello");
    }

    #[test]
    fn url_encode_preserves_safe_chars() {
        assert_eq!(url_encode("simple"), "simple");
        assert_eq!(url_encode("dir/file.json"), "dir/file.json");
        assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn url_encode_encodes_special_chars() {
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a?b=c"), "a%3Fb%3Dc");
        assert_eq!(url_encode("a#b"), "a%23b");
        assert_eq!(url_encode("a&b"), "a%26b");
    }
}
