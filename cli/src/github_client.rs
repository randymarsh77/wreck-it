//! Minimal GitHub REST API client for the wreck-it GitHub Issues integration.
//!
//! When `github_issues_enabled` is set in the config a [`GitHubIssueClient`]
//! is used to:
//!
//! * **Open** a GitHub Issue (titled `[<task_id>] <description>`) when a task
//!   transitions to `InProgress`.
//! * **Close** that issue when the task later reaches `Completed` or `Failed`.
//!
//! The client always adds a `wreck-it` label to every issue it creates so that
//! automation can easily filter wreck-it–managed issues.
//!
//! ## Authentication
//!
//! The token is resolved in the following order:
//!
//! 1. `GITHUB_TOKEN` environment variable.
//! 2. `github_token` field from the wreck-it config file.
//!
//! If neither source provides a token the client is constructed but every
//! operation will fail with a descriptive error.

use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use tracing::warn;

/// Label attached to every issue created by wreck-it.
pub const WRECK_IT_LABEL: &str = "wreck-it";

/// GitHub REST API base URL.
const GITHUB_API_BASE: &str = "https://api.github.com";

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

// ── Request / response shapes ────────────────────────────────────────────────

#[derive(Debug, Serialize)]
struct CreateIssueRequest<'a> {
    title: &'a str,
    body: &'a str,
    labels: Vec<&'a str>,
}

#[derive(Debug, Deserialize)]
struct CreateIssueResponse {
    pub number: u64,
}

#[derive(Debug, Serialize)]
struct UpdateIssueRequest<'a> {
    state: &'a str,
}

// ── Client ───────────────────────────────────────────────────────────────────

/// Thin wrapper around the GitHub Issues REST API.
///
/// Construct via [`GitHubIssueClient::new`].  All API methods are `async` and
/// return `anyhow::Result`.  Errors are surfaced as `Err` values rather than
/// panics; callers should log and continue rather than aborting.
pub struct GitHubIssueClient {
    /// `owner/repo` string, e.g. `"acme/my-project"`.
    repo: String,
    /// Bearer token used for every request.
    token: String,
    /// Base URL for the GitHub API.  Defaults to [`GITHUB_API_BASE`].
    /// Overridable in tests to point at a local mock server.
    api_base: String,
}

impl GitHubIssueClient {
    /// Create a new client.
    ///
    /// `repo` must be in `owner/repo` format.  `token` is a GitHub personal
    /// access token or fine-grained token with `issues: write` permission.
    pub fn new(repo: impl Into<String>, token: impl Into<String>) -> Self {
        Self {
            repo: repo.into(),
            token: token.into(),
            api_base: GITHUB_API_BASE.to_string(),
        }
    }

    /// Create a client that sends requests to `base_url` instead of the
    /// real GitHub API.  Used in tests to point at a local mock server.
    #[cfg(test)]
    fn new_with_base_url(
        repo: impl Into<String>,
        token: impl Into<String>,
        base_url: impl Into<String>,
    ) -> Self {
        Self {
            repo: repo.into(),
            token: token.into(),
            api_base: base_url.into(),
        }
    }

    /// Resolve a GitHub token from the environment or config, returning `None`
    /// when neither source provides a non-empty value.
    pub fn resolve_token(config_token: Option<&str>) -> Option<String> {
        // 1. Environment variable.
        if let Ok(t) = std::env::var("GITHUB_TOKEN") {
            if !t.is_empty() {
                return Some(t);
            }
        }
        // 2. Config field.
        if let Some(t) = config_token {
            if !t.is_empty() {
                return Some(t.to_string());
            }
        }
        None
    }

    /// Open a GitHub Issue for the given task.
    ///
    /// The issue title is `[<task_id>] <description>` and the body is a
    /// short note indicating which task is being tracked.
    ///
    /// Returns the new issue number on success.
    pub async fn create_issue(&self, task_id: &str, description: &str) -> Result<u64> {
        let title = format!("[{task_id}] {description}");
        let body = format!(
            "This issue was automatically created by **wreck-it** to track the progress of task `{task_id}`.\n\n\
             **Task:** {description}\n\n\
             The issue will be closed automatically when the task completes or fails."
        );

        let url = format!("{}/repos/{}/issues", self.api_base, self.repo);
        let payload = CreateIssueRequest {
            title: &title,
            body: &body,
            labels: vec![WRECK_IT_LABEL],
        };

        let resp = http_client()
            .post(&url)
            .bearer_auth(&self.token)
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send create-issue request to GitHub")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("GitHub create-issue failed ({status}): {body}");
        }

        let created: CreateIssueResponse = resp
            .json()
            .await
            .context("Failed to parse create-issue response from GitHub")?;

        Ok(created.number)
    }

    /// Close an existing GitHub Issue by number.
    ///
    /// Failures are returned as `Err` but callers are encouraged to log them
    /// as warnings rather than aborting the main task loop.
    pub async fn close_issue(&self, issue_number: u64) -> Result<()> {
        let url = format!(
            "{}/repos/{}/issues/{issue_number}",
            self.api_base, self.repo
        );
        let payload = UpdateIssueRequest { state: "closed" };

        let resp = http_client()
            .patch(&url)
            .bearer_auth(&self.token)
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send close-issue request to GitHub")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("GitHub close-issue failed ({status}): {body}");
        }

        Ok(())
    }

    /// Ensure the `wreck-it` label exists in the repository.
    ///
    /// Creates the label with a default colour (`#0075ca`) if it does not
    /// already exist.  If the label already exists (HTTP 422) the error is
    /// silently ignored.
    #[allow(dead_code)]
    pub async fn ensure_label(&self) -> Result<()> {
        let url = format!("{}/repos/{}/labels", self.api_base, self.repo);

        #[derive(Serialize)]
        struct CreateLabelRequest<'a> {
            name: &'a str,
            color: &'a str,
            description: &'a str,
        }

        let payload = CreateLabelRequest {
            name: WRECK_IT_LABEL,
            color: "0075ca",
            description: "Managed by wreck-it",
        };

        let resp = http_client()
            .post(&url)
            .bearer_auth(&self.token)
            .header("User-Agent", "wreck-it")
            .header("Accept", "application/vnd.github+json")
            .json(&payload)
            .send()
            .await
            .context("Failed to send ensure-label request to GitHub")?;

        // 422 means the label already exists – that is fine.
        if resp.status() == reqwest::StatusCode::UNPROCESSABLE_ENTITY {
            return Ok(());
        }

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("GitHub ensure-label failed ({status}): {body}");
        }

        Ok(())
    }
}

// ── Helper used by ralph_loop.rs ─────────────────────────────────────────────

/// Build a [`GitHubIssueClient`] from the wreck-it config when GitHub Issues
/// integration is enabled.
///
/// Returns `None` when the feature is disabled or when required configuration
/// (`github_repo`, authentication token) is missing.  Warnings are emitted
/// for misconfiguration so that operators know the feature is not active.
pub fn client_from_config(
    enabled: bool,
    repo: Option<&str>,
    config_token: Option<&str>,
) -> Option<GitHubIssueClient> {
    if !enabled {
        return None;
    }

    let repo = match repo {
        Some(r) if !r.is_empty() => r,
        _ => {
            warn!(
                "github_issues_enabled is true but github_repo is not set; \
                 GitHub Issues integration disabled"
            );
            return None;
        }
    };

    match GitHubIssueClient::resolve_token(config_token) {
        Some(token) => Some(GitHubIssueClient::new(repo, token)),
        None => {
            warn!(
                "github_issues_enabled is true but no GitHub token found \
                 (set GITHUB_TOKEN or github_token in config); \
                 GitHub Issues integration disabled"
            );
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::ENV_LOCK;

    // ── resolve_token ────────────────────────────────────────────────────────

    #[test]
    fn resolve_token_prefers_env_over_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::set_var("GITHUB_TOKEN", "env-token");

        let token = GitHubIssueClient::resolve_token(Some("config-token"));
        assert_eq!(token.as_deref(), Some("env-token"));

        // Restore
        match had_env {
            Some(v) => std::env::set_var("GITHUB_TOKEN", v),
            None => std::env::remove_var("GITHUB_TOKEN"),
        }
    }

    #[test]
    fn resolve_token_falls_back_to_config() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");

        let token = GitHubIssueClient::resolve_token(Some("config-token"));
        assert_eq!(token.as_deref(), Some("config-token"));

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    #[test]
    fn resolve_token_returns_none_when_no_sources() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");

        let token = GitHubIssueClient::resolve_token(None);
        assert!(token.is_none());

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    #[test]
    fn resolve_token_ignores_empty_env() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::set_var("GITHUB_TOKEN", "");

        let token = GitHubIssueClient::resolve_token(Some("config-token"));
        assert_eq!(token.as_deref(), Some("config-token"));

        match had_env {
            Some(v) => std::env::set_var("GITHUB_TOKEN", v),
            None => std::env::remove_var("GITHUB_TOKEN"),
        }
    }

    // ── client_from_config ───────────────────────────────────────────────────

    #[test]
    fn client_from_config_returns_none_when_disabled() {
        let client = client_from_config(false, Some("owner/repo"), Some("token"));
        assert!(client.is_none());
    }

    #[test]
    fn client_from_config_returns_none_when_repo_missing() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");

        let client = client_from_config(true, None, Some("token"));
        assert!(client.is_none());

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    #[test]
    fn client_from_config_returns_none_when_no_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");

        let client = client_from_config(true, Some("owner/repo"), None);
        assert!(client.is_none());

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    #[test]
    fn client_from_config_returns_client_when_configured() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");

        let client = client_from_config(true, Some("owner/repo"), Some("mytoken"));
        assert!(client.is_some());
        let c = client.unwrap();
        assert_eq!(c.repo, "owner/repo");
        assert_eq!(c.token, "mytoken");

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    // ── Issue title format ───────────────────────────────────────────────────

    #[test]
    fn issue_title_format() {
        // Verify the title format used by create_issue without making network calls.
        let task_id = "impl-auth";
        let description = "Implement the authentication module";
        let expected_title = "[impl-auth] Implement the authentication module";
        let actual_title = format!("[{task_id}] {description}");
        assert_eq!(actual_title, expected_title);
    }

    // ── HTTP integration tests (mock TCP server) ─────────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spin up a minimal HTTP/1.1 server on a random localhost port.
    /// Returns the base URL and a future that accepts one connection, reads
    /// the raw request bytes, sends back `status_line` with `resp_body`, and
    /// returns the captured request bytes.
    async fn mock_github_server(
        status_line: &'static str,
        resp_body: &'static str,
    ) -> (String, impl std::future::Future<Output = Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let base_url = format!("http://127.0.0.1:{port}");

        let fut = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 16384];
            let n = stream.read(&mut buf).await.unwrap();
            buf.truncate(n);
            let response = format!(
                "{}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                status_line,
                resp_body.len(),
                resp_body
            );
            let _ = stream.write_all(response.as_bytes()).await;
            buf
        };

        (base_url, fut)
    }

    /// Extract the request line (first line) from a raw HTTP request.
    fn request_line(raw: &[u8]) -> String {
        let text = std::str::from_utf8(raw).expect("request is not valid UTF-8");
        text.lines().next().unwrap_or("").to_string()
    }

    /// Extract and parse the JSON body from a raw HTTP request.
    fn extract_body(raw: &[u8]) -> serde_json::Value {
        let text = std::str::from_utf8(raw).expect("request is not valid UTF-8");
        let body_start = text
            .find("\r\n\r\n")
            .expect("no header/body separator in request");
        let body = &text[body_start + 4..];
        serde_json::from_str(body).expect("body is not valid JSON")
    }

    /// (1) A POST to /repos/{owner}/{repo}/issues is made when create_issue is called
    ///     (which corresponds to a task transitioning to InProgress).
    #[tokio::test]
    async fn create_issue_sends_post_to_issues_endpoint() {
        let resp_body = r#"{"number": 42, "html_url": "https://github.com/owner/repo/issues/42"}"#;
        let (base_url, req_fut) = mock_github_server("HTTP/1.1 201 Created", resp_body).await;

        let client = GitHubIssueClient::new_with_base_url("owner/repo", "fake-token", &base_url);

        let (result, raw_req) = tokio::join!(
            client.create_issue("impl-auth", "Implement the auth module"),
            req_fut,
        );

        // The call must succeed and return the issue number from the response.
        assert_eq!(
            result.unwrap(),
            42,
            "create_issue should return the issue number"
        );

        // Verify the HTTP method and path.
        let req_line = request_line(&raw_req);
        assert!(
            req_line.starts_with("POST /repos/owner/repo/issues"),
            "expected POST to /repos/owner/repo/issues, got: {req_line}"
        );

        // Verify the payload contains the task id in the title and the wreck-it label.
        let body = extract_body(&raw_req);
        assert!(
            body["title"].as_str().unwrap_or("").contains("impl-auth"),
            "issue title should contain the task id"
        );
        let labels = body["labels"]
            .as_array()
            .expect("labels should be a JSON array");
        assert!(!labels.is_empty(), "labels array should not be empty");
        assert_eq!(labels[0], "wreck-it", "issue should be labeled 'wreck-it'");
    }

    /// (2) A PATCH request to close the issue is made when close_issue is called
    ///     (which corresponds to a task reaching Completed or Failed).
    #[tokio::test]
    async fn close_issue_sends_patch_to_issue_endpoint() {
        let resp_body = r#"{"number": 42, "state": "closed"}"#;
        let (base_url, req_fut) = mock_github_server("HTTP/1.1 200 OK", resp_body).await;

        let client = GitHubIssueClient::new_with_base_url("owner/repo", "fake-token", &base_url);

        let (result, raw_req) = tokio::join!(client.close_issue(42), req_fut);

        result.unwrap();

        let req_line = request_line(&raw_req);
        assert!(
            req_line.starts_with("PATCH /repos/owner/repo/issues/42"),
            "expected PATCH to /repos/owner/repo/issues/42, got: {req_line}"
        );

        // Verify that the payload sets state to "closed".
        let body = extract_body(&raw_req);
        assert_eq!(
            body["state"], "closed",
            "close_issue should set state to 'closed'"
        );
    }

    /// (3) When github_issues_enabled is false, no HTTP calls are made.
    ///     client_from_config returns None, so the caller never constructs a
    ///     client and therefore no network I/O happens.
    #[test]
    fn no_http_calls_when_github_issues_disabled() {
        // client_from_config must return None regardless of other settings.
        let client = client_from_config(false, Some("owner/repo"), Some("some-token"));
        assert!(
            client.is_none(),
            "client_from_config should return None when github_issues_enabled is false"
        );
        // Because the client is None, the callers in ralph_loop.rs will skip
        // all GitHub API calls – no HTTP requests are made.
    }

    /// (4) A GitHub API error (e.g. 401 Unauthorized) causes create_issue to
    ///     return Err.  Callers log this as a warning and continue the loop
    ///     without aborting.
    #[tokio::test]
    async fn create_issue_api_error_returns_err_without_panicking() {
        let resp_body = r#"{"message": "Requires authentication", "documentation_url": "https://docs.github.com/rest"}"#;
        let (base_url, req_fut) = mock_github_server("HTTP/1.1 401 Unauthorized", resp_body).await;

        let client = GitHubIssueClient::new_with_base_url("owner/repo", "bad-token", &base_url);

        let (result, _raw_req) = tokio::join!(
            client.create_issue("task-1", "Do something important"),
            req_fut,
        );

        // Must return an Err – not panic – so that callers can log a warning
        // and continue the main task loop.
        assert!(result.is_err(), "create_issue should return Err on 401");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("401"),
            "error message should mention the 401 status, got: {err_msg}"
        );
    }

    /// Companion to (4): close_issue also returns Err on API failure.
    #[tokio::test]
    async fn close_issue_api_error_returns_err_without_panicking() {
        let resp_body = r#"{"message": "Requires authentication"}"#;
        let (base_url, req_fut) = mock_github_server("HTTP/1.1 401 Unauthorized", resp_body).await;

        let client = GitHubIssueClient::new_with_base_url("owner/repo", "bad-token", &base_url);

        let (result, _raw_req) = tokio::join!(client.close_issue(7), req_fut);

        assert!(result.is_err(), "close_issue should return Err on 401");
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("401"),
            "error message should mention the 401 status, got: {err_msg}"
        );
    }
}
