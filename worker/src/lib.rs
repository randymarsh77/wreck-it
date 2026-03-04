//! Cloudflare Worker entry point for the wreck-it GitHub App webhook.
//!
//! This worker receives webhook deliveries from GitHub, verifies the
//! payload signature, and triggers an iteration of the wreck-it processing
//! loop.  The Rust code compiles to WASM via `wasm32-unknown-unknown` and
//! runs on the Cloudflare Workers runtime.
//!
//! # Authentication
//!
//! The worker supports two authentication modes:
//!
//! **App credentials (recommended)** — set `GITHUB_APP_ID` and
//! `GITHUB_APP_PRIVATE_KEY`.  The worker generates a JWT and exchanges it
//! for an installation access token using the `installation.id` from each
//! webhook payload.  This token has the full permissions granted to the
//! GitHub App, enabling the worker to create issues, assign agents, merge
//! PRs, and manage the complete task lifecycle.
//!
//! **Static token (legacy)** — set `GITHUB_APP_TOKEN` to a pre-generated
//! installation token or PAT.  The worker can read/write repository
//! contents on the state branch but **cannot** trigger agents or merge PRs
//! unless the token has sufficient scopes.
//!
//! # Required secrets (set via `wrangler secret put`):
//!
//! - `GITHUB_WEBHOOK_SECRET` — webhook secret for HMAC-SHA256 verification.
//! - `GITHUB_APP_ID` — numeric App ID (recommended).
//! - `GITHUB_APP_PRIVATE_KEY` — PEM-encoded RSA private key (recommended).
//! - `GITHUB_APP_TOKEN` — fallback static token (legacy).

mod github;
mod github_app;
mod processor;
mod types;
mod webhook;

use webhook::{verify_signature, WebhookEvent};
use worker::*;

/// Check whether an issue was opened by a trusted author.
///
/// Delegates to the shared [`wreck_it_core::types::is_trusted_issue_author`]
/// function.
fn is_trusted_issue_author(issue: &types::Issue) -> bool {
    let user_type = issue.user.as_ref().and_then(|u| u.user_type.as_deref());
    wreck_it_core::types::is_trusted_issue_author(user_type)
}

/// Check whether a pull request was opened by a known coding agent.
///
/// Delegates to the shared [`wreck_it_core::types::is_trusted_pr_author`]
/// function.
fn is_trusted_pr_author(pr: &types::PullRequest) -> bool {
    let login = pr.user.as_ref().map(|u| u.login.as_str());
    wreck_it_core::types::is_trusted_pr_author(login)
}

#[event(fetch)]
async fn main(mut req: Request, env: Env, _ctx: Context) -> Result<Response> {
    // Only accept POST requests at the webhook endpoint.
    if req.method() != Method::Post {
        return Response::ok("wreck-it webhook worker is running");
    }

    // Read required secrets.
    let webhook_secret = env
        .secret("GITHUB_WEBHOOK_SECRET")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("Missing GITHUB_WEBHOOK_SECRET secret".into()))?;

    // Read the raw body for signature verification.
    let body_bytes = req.bytes().await?;

    // Verify the webhook signature.
    let signature = req
        .headers()
        .get("X-Hub-Signature-256")
        .ok()
        .flatten()
        .unwrap_or_default();

    if !verify_signature(&signature, &webhook_secret, &body_bytes) {
        return Response::error("Invalid signature", 401);
    }

    // Determine the event type.
    let event_header = req
        .headers()
        .get("X-GitHub-Event")
        .ok()
        .flatten()
        .unwrap_or_default();
    let event = WebhookEvent::from_header(&event_header);

    // Parse the payload.
    let payload: types::WebhookPayload = serde_json::from_slice(&body_bytes)
        .map_err(|e| Error::RustError(format!("Failed to parse payload: {e}")))?;

    // Only process events we care about.
    let should_process = match &event {
        WebhookEvent::Issues => {
            // Process when an issue is opened or labeled with "wreck-it",
            // but only if the issue was created by our App bot (type "Bot").
            // This prevents external users from triggering task processing
            // by creating or labeling issues with the "wreck-it" label.
            let action = payload.action.as_deref().unwrap_or("");
            let has_label = payload
                .issue
                .as_ref()
                .map(|i| i.labels.iter().any(|l| l.name == "wreck-it"))
                .unwrap_or(false);
            let trusted = payload
                .issue
                .as_ref()
                .map(|i| is_trusted_issue_author(i))
                .unwrap_or(false);
            (action == "opened" && has_label || action == "labeled" && has_label) && trusted
        }
        WebhookEvent::Push => {
            // Process pushes to the state branch (external state updates).
            true
        }
        WebhookEvent::PullRequest => {
            // Process when a PR is merged (task completion signal), but
            // only if the PR was opened by a known coding agent.  This
            // prevents an attacker from marking tasks as complete by
            // getting an unauthorized PR merged.
            let action = payload.action.as_deref().unwrap_or("");
            let merged = payload
                .pull_request
                .as_ref()
                .and_then(|pr| pr.merged)
                .unwrap_or(false);
            let trusted = payload
                .pull_request
                .as_ref()
                .map(|pr| is_trusted_pr_author(pr))
                .unwrap_or(false);
            action == "closed" && merged && trusted
        }
        WebhookEvent::Other(name) if name == "ping" => {
            // Respond to GitHub's ping event during app setup.
            return Response::ok("pong");
        }
        _ => false,
    };

    if !should_process {
        return Response::ok("event ignored");
    }

    // Extract repository info.
    let repo = payload
        .repository
        .as_ref()
        .ok_or_else(|| Error::RustError("Missing repository in payload".into()))?;

    let owner = &repo.owner.login;
    let repo_name = &repo.name;
    let default_branch = repo.default_branch.as_deref().unwrap_or("main");

    // Resolve the GitHub API token.
    //
    // Prefer App credentials (GITHUB_APP_ID + GITHUB_APP_PRIVATE_KEY) which
    // allow the worker to vend a scoped installation token with full
    // permissions.  Fall back to the static GITHUB_APP_TOKEN for backward
    // compatibility.
    let github_token = resolve_github_token(&env, &payload).await?;

    // Create GitHub client and run the iteration.
    let client = github::GitHubClient::new(owner, repo_name, &github_token);

    // Verify the state branch exists before processing.
    let config_file = client
        .get_file(".wreck-it/config.toml", default_branch)
        .await
        .map_err(|e| Error::RustError(e))?;

    if config_file.is_none() {
        return Response::ok("no wreck-it configuration found; skipping");
    }

    // For merged PR events, handle task completion and PR management.
    // Note: the `should_process` check above already filters PullRequest events
    // to only those with action == "closed" && merged == true.
    if event == WebhookEvent::PullRequest {
        if let Some(pr) = &payload.pull_request {
            match processor::process_merged_pr(&client, default_branch, pr.number).await {
                Ok(result) => {
                    let status = if result.changed { "processed" } else { "no-op" };
                    return Response::ok(format!("pr-merged {status}: {}", result.summary));
                }
                Err(e) => {
                    console_error!("PR merge handling failed: {e}");
                    // Fall through to the normal iteration processing.
                }
            }
        }
    }

    match processor::process_iteration(&client, default_branch).await {
        Ok(result) => {
            let status = if result.changed { "processed" } else { "no-op" };
            Response::ok(format!("{status}: {}", result.summary))
        }
        Err(e) => {
            console_error!("Iteration failed: {e}");
            Response::error(format!("Processing failed: {e}"), 500)
        }
    }
}

/// Resolve the GitHub API token from environment secrets.
///
/// Prefers GitHub App credentials (GITHUB_APP_ID + GITHUB_APP_PRIVATE_KEY)
/// which produce a scoped installation token.  Falls back to the static
/// GITHUB_APP_TOKEN secret.
async fn resolve_github_token(
    env: &Env,
    payload: &types::WebhookPayload,
) -> Result<String> {
    // Try App credentials first.
    let app_id = env.secret("GITHUB_APP_ID").map(|s| s.to_string()).ok();
    let private_key = env
        .secret("GITHUB_APP_PRIVATE_KEY")
        .map(|s| s.to_string())
        .ok();

    if let (Some(app_id), Some(private_key)) = (app_id, private_key) {
        let installation_id = payload
            .installation
            .as_ref()
            .map(|i| i.id)
            .ok_or_else(|| {
                Error::RustError(
                    "GITHUB_APP_ID and GITHUB_APP_PRIVATE_KEY are set but webhook \
                     payload has no installation.id"
                        .into(),
                )
            })?;

        let now_secs = js_sys_now_secs();
        let jwt = github_app::generate_jwt(&app_id, &private_key, now_secs)
            .map_err(|e| Error::RustError(format!("JWT generation failed: {e}")))?;

        let token = github_app::vend_installation_token(installation_id, &jwt)
            .await
            .map_err(|e| Error::RustError(format!("Token vending failed: {e}")))?;

        return Ok(token);
    }

    // Fall back to static token.
    env.secret("GITHUB_APP_TOKEN")
        .map(|s| s.to_string())
        .map_err(|_| {
            Error::RustError(
                "Missing GitHub credentials: set either GITHUB_APP_ID + \
                 GITHUB_APP_PRIVATE_KEY (recommended) or GITHUB_APP_TOKEN"
                    .into(),
            )
        })
}

/// Current Unix timestamp in seconds.
fn js_sys_now_secs() -> u64 {
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_issue(login: &str, user_type: &str) -> types::Issue {
        types::Issue {
            number: 1,
            title: "test".into(),
            body: None,
            labels: vec![],
            user: Some(types::User {
                login: login.into(),
                user_type: Some(user_type.into()),
            }),
        }
    }

    fn make_pr(login: &str, user_type: &str) -> types::PullRequest {
        types::PullRequest {
            number: 1,
            state: "open".into(),
            merged: None,
            user: Some(types::User {
                login: login.into(),
                user_type: Some(user_type.into()),
            }),
        }
    }

    // ---- is_trusted_issue_author ----

    #[test]
    fn trusted_issue_author_bot() {
        let issue = make_issue("my-app[bot]", "Bot");
        assert!(is_trusted_issue_author(&issue));
    }

    #[test]
    fn untrusted_issue_author_user() {
        let issue = make_issue("attacker", "User");
        assert!(!is_trusted_issue_author(&issue));
    }

    #[test]
    fn untrusted_issue_author_no_user() {
        let issue = types::Issue {
            number: 1,
            title: "test".into(),
            body: None,
            labels: vec![],
            user: None,
        };
        assert!(!is_trusted_issue_author(&issue));
    }

    #[test]
    fn untrusted_issue_author_no_type() {
        let issue = types::Issue {
            number: 1,
            title: "test".into(),
            body: None,
            labels: vec![],
            user: Some(types::User {
                login: "someone".into(),
                user_type: None,
            }),
        };
        assert!(!is_trusted_issue_author(&issue));
    }

    // ---- is_trusted_pr_author ----

    #[test]
    fn trusted_pr_author_copilot_swe_agent_bot() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(is_trusted_pr_author(&pr));
    }

    #[test]
    fn trusted_pr_author_copilot_bare() {
        let pr = make_pr("copilot", "Bot");
        assert!(is_trusted_pr_author(&pr));
    }

    #[test]
    fn trusted_pr_author_claude() {
        let pr = make_pr("claude", "Bot");
        assert!(is_trusted_pr_author(&pr));
    }

    #[test]
    fn trusted_pr_author_codex() {
        let pr = make_pr("codex", "Bot");
        assert!(is_trusted_pr_author(&pr));
    }

    #[test]
    fn trusted_pr_author_codex_bot_suffix() {
        let pr = make_pr("codex[bot]", "Bot");
        assert!(is_trusted_pr_author(&pr));
    }

    #[test]
    fn untrusted_pr_author_random_user() {
        let pr = make_pr("attacker", "User");
        assert!(!is_trusted_pr_author(&pr));
    }

    #[test]
    fn untrusted_pr_author_no_user() {
        let pr = types::PullRequest {
            number: 1,
            state: "open".into(),
            merged: None,
            user: None,
        };
        assert!(!is_trusted_pr_author(&pr));
    }

    #[test]
    fn untrusted_pr_author_unknown_bot() {
        let pr = make_pr("evil-bot[bot]", "Bot");
        assert!(!is_trusted_pr_author(&pr));
    }
}
