//! GitHub OAuth device flow authentication.
//!
//! When no GitHub token is available from environment variables or user config,
//! this module implements the [OAuth device flow][df] to obtain a token via
//! browser-based sign-in.
//!
//! The flow requires a GitHub OAuth App client ID.  Set the
//! `WRECK_IT_OAUTH_CLIENT_ID` environment variable or pass it via CLI.  If no
//! client ID is configured the user is prompted to create a GitHub OAuth App.
//!
//! [df]: https://docs.github.com/en/apps/oauth-apps/building-oauth-apps/authorizing-oauth-apps#device-flow

use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// GitHub device code endpoint.
const DEVICE_CODE_URL: &str = "https://github.com/login/device/code";

/// GitHub token exchange endpoint.
const TOKEN_URL: &str = "https://github.com/login/oauth/access_token";

/// OAuth scopes needed for creating issues and managing the repo.
const OAUTH_SCOPES: &str = "repo";

/// Response from the device code request.
#[derive(Debug, Deserialize)]
struct DeviceCodeResponse {
    device_code: String,
    user_code: String,
    verification_uri: String,
    #[allow(dead_code)]
    expires_in: u64,
    interval: u64,
}

/// Response from the token polling request.
#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: Option<String>,
    error: Option<String>,
}

/// Resolve a GitHub token, checking multiple sources in order.
///
/// 1. `GITHUB_TOKEN` environment variable
/// 2. Previously stored token from user config (`github_token` field)
/// 3. Interactive OAuth device flow (opens the browser)
///
/// The `config_token` parameter is the stored `github_token` from the user
/// config file, if any.
pub async fn resolve_github_token(config_token: Option<&str>) -> Result<String> {
    // 1. Environment variable.
    if let Ok(token) = std::env::var("GITHUB_TOKEN") {
        if !token.is_empty() {
            return Ok(token);
        }
    }

    // 2. Stored config token.
    if let Some(token) = config_token {
        if !token.is_empty() {
            return Ok(token.to_string());
        }
    }

    // 3. Device flow.
    device_flow_auth().await
}

/// Run the GitHub OAuth device flow to obtain an access token.
///
/// 1. Request a device code from GitHub.
/// 2. Display the verification URL and user code; open the browser.
/// 3. Poll for the access token until the user completes authorisation.
async fn device_flow_auth() -> Result<String> {
    let client_id = std::env::var("WRECK_IT_OAUTH_CLIENT_ID")
        .ok()
        .filter(|s| !s.is_empty());

    let client_id = match client_id {
        Some(id) => id,
        None => {
            bail!(
                "No GitHub token found and no OAuth client ID configured.\n\n\
                 To use `--cloud` you need a GitHub token.  You can either:\n\
                 1. Set the GITHUB_TOKEN environment variable, or\n\
                 2. Set WRECK_IT_OAUTH_CLIENT_ID to a GitHub OAuth App client ID\n\
                    to authenticate via the browser.\n\n\
                 To create a GitHub OAuth App:\n\
                   • Go to https://github.com/settings/applications/new\n\
                   • Set \"Application name\" to something like \"wreck-it\"\n\
                   • Set \"Homepage URL\" to your repo URL\n\
                   • Check \"Enable Device Flow\"\n\
                   • After creation, copy the Client ID and set WRECK_IT_OAUTH_CLIENT_ID"
            );
        }
    };

    let http = reqwest::Client::new();

    // Step 1: Request device code.
    let resp = http
        .post(DEVICE_CODE_URL)
        .header("Accept", "application/json")
        .form(&[("client_id", client_id.as_str()), ("scope", OAUTH_SCOPES)])
        .send()
        .await
        .context("Failed to request GitHub device code")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        bail!(
            "GitHub device code request failed ({}): {}\n\
             Make sure the OAuth App has device flow enabled.",
            status,
            body,
        );
    }

    let device: DeviceCodeResponse = resp
        .json()
        .await
        .context("Failed to parse device code response")?;

    // Step 2: Show the user code and try to open the browser.
    println!();
    println!("To authenticate with GitHub, open this URL in your browser:");
    println!();
    println!("  {}", device.verification_uri);
    println!();
    println!("Then enter the code:  {}", device.user_code);
    println!();

    if let Err(e) = open::that(&device.verification_uri) {
        tracing::debug!("Could not open browser automatically: {}", e);
    }

    // Step 3: Poll for the token.
    let interval = std::time::Duration::from_secs(device.interval.max(5));
    let max_polls = 120; // generous upper bound
    println!("Waiting for authorisation…");

    for _ in 0..max_polls {
        tokio::time::sleep(interval).await;

        let resp = http
            .post(TOKEN_URL)
            .header("Accept", "application/json")
            .form(&[
                ("client_id", client_id.as_str()),
                ("device_code", device.device_code.as_str()),
                ("grant_type", "urn:ietf:params:oauth:grant-type:device_code"),
            ])
            .send()
            .await
            .context("Failed to poll for access token")?;

        let token_resp: TokenResponse = resp
            .json()
            .await
            .context("Failed to parse token exchange response")?;

        if let Some(ref token) = token_resp.access_token {
            if !token.is_empty() {
                println!("GitHub authentication successful!");
                return Ok(token.clone());
            }
        }

        match token_resp.error.as_deref() {
            Some("authorization_pending") => continue,
            Some("slow_down") => {
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
            Some("expired_token") => bail!("Device code expired before authorisation completed."),
            Some("access_denied") => bail!("Authorisation was denied by the user."),
            Some(err) => bail!("GitHub OAuth error: {}", err),
            None => continue,
        }
    }

    bail!("Timed out waiting for GitHub authorisation")
}

/// Build a GitHub issue body that instructs a cloud agent to generate a
/// structured task plan for the given goal.
///
/// The agent is expected to create a JSON plan file at the path
/// `.wreck-it/plans/{plan_filename}` on the default branch.
pub fn build_plan_issue_body(goal: &str, plan_filename: &str) -> String {
    format!(
        "## Goal\n\n\
         {goal}\n\n\
         ## Instructions\n\n\
         Analyse the goal above and break it down into a structured list of \
         development tasks.  Write the result as a JSON file at:\n\n\
         ```\n\
         .wreck-it/plans/{plan_filename}\n\
         ```\n\n\
         The file must contain a JSON array of task objects.  Each object must \
         have exactly these fields:\n\n\
         | Field | Type | Description |\n\
         |-------|------|-------------|\n\
         | `id` | string | Unique task identifier (e.g. `\"1\"`, `\"impl-auth\"`) |\n\
         | `description` | string | Clear, actionable description of the task |\n\
         | `status` | string | Must be `\"pending\"` |\n\
         | `phase` | integer | Execution phase (≥ 1); lower phases run first |\n\
         | `depends_on` | string[] | IDs of tasks that must complete first (may be empty) |\n\n\
         ### Example\n\n\
         ```json\n\
         [\n\
           {{\"id\": \"1\", \"description\": \"Set up project structure\", \"status\": \"pending\", \"phase\": 1, \"depends_on\": []}},\n\
           {{\"id\": \"2\", \"description\": \"Implement core logic\", \"status\": \"pending\", \"phase\": 2, \"depends_on\": [\"1\"]}},\n\
           {{\"id\": \"3\", \"description\": \"Add tests\", \"status\": \"pending\", \"phase\": 2, \"depends_on\": [\"1\"]}}\n\
         ]\n\
         ```\n\n\
         Commit the file directly to the default branch.\n\n\
         ---\n\
         *Triggered by `wreck-it plan --cloud`*",
        goal = goal,
        plan_filename = plan_filename,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_helpers::ENV_LOCK;

    // ---- resolve_github_token tests ----

    #[tokio::test]
    async fn resolve_from_config_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        // Clear env to avoid interference from CI environment.
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");

        let token = resolve_github_token(Some("cfg-token-123")).await.unwrap();
        assert_eq!(token, "cfg-token-123");

        // Restore env.
        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
    }

    #[tokio::test]
    async fn resolve_skips_empty_config_token() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");
        let had_oauth = std::env::var("WRECK_IT_OAUTH_CLIENT_ID").ok();
        std::env::remove_var("WRECK_IT_OAUTH_CLIENT_ID");

        let result = resolve_github_token(Some("")).await;
        // Should fail because no env, empty config, and no OAuth client ID.
        assert!(result.is_err());

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
        if let Some(v) = had_oauth {
            std::env::set_var("WRECK_IT_OAUTH_CLIENT_ID", v);
        }
    }

    #[tokio::test]
    async fn resolve_falls_through_when_no_sources() {
        let _guard = ENV_LOCK.lock().unwrap();
        let had_env = std::env::var("GITHUB_TOKEN").ok();
        std::env::remove_var("GITHUB_TOKEN");
        let had_oauth = std::env::var("WRECK_IT_OAUTH_CLIENT_ID").ok();
        std::env::remove_var("WRECK_IT_OAUTH_CLIENT_ID");

        let result = resolve_github_token(None).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("GITHUB_TOKEN"),
            "error should mention GITHUB_TOKEN: {msg}"
        );

        if let Some(v) = had_env {
            std::env::set_var("GITHUB_TOKEN", v);
        }
        if let Some(v) = had_oauth {
            std::env::set_var("WRECK_IT_OAUTH_CLIENT_ID", v);
        }
    }

    // ---- build_plan_issue_body tests ----

    #[test]
    fn plan_issue_body_contains_goal() {
        let body =
            build_plan_issue_body("Build a REST API", "rest-api-tasks.json--cloud-plan.json");
        assert!(body.contains("Build a REST API"));
    }

    #[test]
    fn plan_issue_body_contains_filename() {
        let body = build_plan_issue_body("anything", "feature-tasks.json--cloud-plan.json");
        assert!(body.contains("feature-tasks.json--cloud-plan.json"));
    }

    #[test]
    fn plan_issue_body_contains_json_schema_fields() {
        let body = build_plan_issue_body("anything", "plan.json");
        assert!(body.contains("\"id\""));
        assert!(body.contains("\"description\""));
        assert!(body.contains("\"status\""));
        assert!(body.contains("\"phase\""));
        assert!(body.contains("\"depends_on\""));
    }

    #[test]
    fn plan_issue_body_contains_example() {
        let body = build_plan_issue_body("anything", "plan.json");
        assert!(body.contains("Set up project structure"));
        assert!(body.contains("\"pending\""));
    }

    #[test]
    fn plan_issue_body_contains_plans_path() {
        let body = build_plan_issue_body("goal", "my-plan.json");
        assert!(body.contains(".wreck-it/plans/my-plan.json"));
    }
}
