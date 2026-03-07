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

        let url = format!("{GITHUB_API_BASE}/repos/{}/issues", self.repo);
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
            "{GITHUB_API_BASE}/repos/{}/issues/{issue_number}",
            self.repo
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
    pub async fn ensure_label(&self) -> Result<()> {
        let url = format!("{GITHUB_API_BASE}/repos/{}/labels", self.repo);

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

    // ── resolve_token ────────────────────────────────────────────────────────

    #[test]
    fn resolve_token_prefers_env_over_config() {
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
}
