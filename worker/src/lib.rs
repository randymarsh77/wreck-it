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
fn is_trusted_issue_author(issue: &types::Issue, authenticated_login: Option<&str>) -> bool {
    let user_type = issue.user.as_ref().and_then(|u| u.user_type.as_deref());
    let user_login = issue.user.as_ref().map(|u| u.login.as_str());
    wreck_it_core::types::is_trusted_issue_author(user_type, user_login, authenticated_login)
}

/// Check whether a pull request was opened by a known coding agent or the
/// authenticated user.
///
/// Delegates to the shared [`wreck_it_core::types::is_trusted_pr_author`]
/// function.
fn is_trusted_pr_author(pr: &types::PullRequest, authenticated_login: Option<&str>) -> bool {
    let login = pr.user.as_ref().map(|u| u.login.as_str());
    wreck_it_core::types::is_trusted_pr_author(login, authenticated_login)
}

/// Determine whether a pull request webhook event should be processed.
///
/// Returns `true` for:
///   - `closed` + `merged == true` (task completion signal)
///   - `opened`, `ready_for_review`, `synchronize` (workflow approval)
///
/// In all cases the PR must be from a trusted author.
fn should_process_pr_event(
    action: &str,
    pr: &types::PullRequest,
    authenticated_login: Option<&str>,
) -> bool {
    if !is_trusted_pr_author(pr, authenticated_login) {
        return false;
    }
    let merged = pr.merged.unwrap_or(false);
    // Workflow-approval actions (approve pending runs + enable auto-merge).
    ["opened", "ready_for_review", "synchronize"].contains(&action)
        // Task-completion action (merged PR).
        || (action == "closed" && merged)
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

    // Quick filter: immediately handle or reject events we never process.
    match &event {
        WebhookEvent::Other(name) if name == "ping" => {
            return Response::ok("pong");
        }
        WebhookEvent::Issues | WebhookEvent::Push | WebhookEvent::PullRequest => {
            // These may need further processing — continue below.
        }
        _ => {
            return Response::ok("event ignored");
        }
    }

    // Extract repository info (needed for token resolution and processing).
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

    // Resolve the authenticated user's login for trust checks.
    //
    // In the App flow, issues are created as the App bot (user.type == "Bot")
    // and the authenticated_login is not needed.  In the PAT flow, issues are
    // created as the PAT owner and we need their login to recognise them as
    // trusted.
    let authenticated_login = fetch_authenticated_login(&github_token).await;
    let auth_login_ref = authenticated_login.as_deref();

    // Only process events we care about.
    let should_process = match &event {
        WebhookEvent::Issues => {
            // Process when an issue is opened or labeled with "wreck-it",
            // but only if the issue was created by our App bot or the
            // authenticated user.  This prevents external users from
            // triggering task processing by creating or labeling issues
            // with the "wreck-it" label.
            let action = payload.action.as_deref().unwrap_or("");
            let has_label = payload
                .issue
                .as_ref()
                .map(|i| i.labels.iter().any(|l| l.name == "wreck-it"))
                .unwrap_or(false);
            let trusted = payload
                .issue
                .as_ref()
                .map(|i| is_trusted_issue_author(i, auth_login_ref))
                .unwrap_or(false);
            (action == "opened" && has_label || action == "labeled" && has_label) && trusted
        }
        WebhookEvent::Push => {
            // Process pushes to the state branch (external state updates).
            true
        }
        WebhookEvent::PullRequest => {
            let action = payload.action.as_deref().unwrap_or("");
            payload
                .pull_request
                .as_ref()
                .map(|pr| should_process_pr_event(action, pr, auth_login_ref))
                .unwrap_or(false)
        }
        _ => false,
    };

    if !should_process {
        return Response::ok("event ignored");
    }

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

    // For PR events from trusted authors, approve pending workflow runs
    // and enable auto-merge when required checks are detected.  This
    // handles the case where workflow runs need explicit approval before
    // they can execute (e.g. first-time contributors, outside
    // collaborators, or fork PRs).
    if event == WebhookEvent::PullRequest {
        if let Some(pr) = &payload.pull_request {
            let action = payload.action.as_deref().unwrap_or("");
            let merged = pr.merged.unwrap_or(false);

            if action == "closed" && merged {
                // Merged PR — handle task completion.
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
            } else {
                // Non-merged PR event (opened, ready_for_review, synchronize)
                // — approve pending workflow runs so required checks can
                // execute, then enable auto-merge.
                let pr_number = pr.number;
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    console_warn!(
                        "Failed to approve workflow runs for PR #{}: {}",
                        pr_number,
                        e,
                    );
                }

                // Enable auto-merge when the base branch has required checks.
                match client.has_required_checks_for_pr(pr_number).await {
                    Ok(true) => {
                        if let Err(e) = client.enable_auto_merge(pr_number).await {
                            console_warn!(
                                "Failed to enable auto-merge for PR #{}: {}",
                                pr_number,
                                e,
                            );
                        }
                    }
                    Ok(false) => {}
                    Err(e) => {
                        console_warn!(
                            "Failed to check required checks for PR #{}: {}",
                            pr_number,
                            e,
                        );
                    }
                }

                return Response::ok(format!(
                    "pr-workflow-approval: processed PR #{} ({})",
                    pr_number, action,
                ));
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

/// Fetch the login of the user (or bot) authenticated by `token`.
///
/// Calls `GET /user` and returns the `login` field.  Returns `None` on any
/// error (network, parse, missing field) so that callers can fall back to
/// the stricter Bot-only check.
async fn fetch_authenticated_login(token: &str) -> Option<String> {
    let url = "https://api.github.com/user";

    let mut headers = worker::Headers::new();
    headers.set("Accept", "application/vnd.github+json").ok();
    headers
        .set("Authorization", &format!("Bearer {}", token))
        .ok();
    headers.set("User-Agent", "wreck-it-worker").ok();
    headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

    let request = match worker::Request::new_with_init(
        url,
        worker::RequestInit::new()
            .with_method(worker::Method::Get)
            .with_headers(headers),
    ) {
        Ok(r) => r,
        Err(e) => {
            console_warn!("Failed to build /user request: {e}");
            return None;
        }
    };

    let mut response = match Fetch::Request(request).send().await {
        Ok(r) => r,
        Err(e) => {
            console_warn!("GET /user failed: {e}");
            return None;
        }
    };

    if response.status_code() != 200 {
        console_warn!(
            "GET /user returned status {}; falling back to Bot-only trust check",
            response.status_code(),
        );
        return None;
    }

    let body: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            console_warn!("Failed to parse /user response: {e}");
            return None;
        }
    };
    let login = body["login"].as_str().map(|s| s.to_string());
    if login.is_none() {
        console_warn!("GET /user response has no login field");
    }
    login
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
        assert!(is_trusted_issue_author(&issue, None));
    }

    #[test]
    fn trusted_issue_author_matching_login() {
        let issue = make_issue("my-user", "User");
        assert!(is_trusted_issue_author(&issue, Some("my-user")));
    }

    #[test]
    fn untrusted_issue_author_user() {
        let issue = make_issue("attacker", "User");
        assert!(!is_trusted_issue_author(&issue, None));
    }

    #[test]
    fn untrusted_issue_author_login_mismatch() {
        let issue = make_issue("attacker", "User");
        assert!(!is_trusted_issue_author(&issue, Some("my-user")));
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
        assert!(!is_trusted_issue_author(&issue, None));
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
        assert!(!is_trusted_issue_author(&issue, None));
    }

    // ---- is_trusted_pr_author ----

    #[test]
    fn trusted_pr_author_copilot_swe_agent_bot() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn trusted_pr_author_copilot_bare() {
        let pr = make_pr("copilot", "Bot");
        assert!(is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn trusted_pr_author_claude() {
        let pr = make_pr("claude", "Bot");
        assert!(is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn trusted_pr_author_codex() {
        let pr = make_pr("codex", "Bot");
        assert!(is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn trusted_pr_author_codex_bot_suffix() {
        let pr = make_pr("codex[bot]", "Bot");
        assert!(is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn trusted_pr_author_matching_login() {
        let pr = make_pr("my-user", "User");
        assert!(is_trusted_pr_author(&pr, Some("my-user")));
    }

    #[test]
    fn untrusted_pr_author_random_user() {
        let pr = make_pr("attacker", "User");
        assert!(!is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn untrusted_pr_author_no_user() {
        let pr = types::PullRequest {
            number: 1,
            state: "open".into(),
            merged: None,
            user: None,
        };
        assert!(!is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn untrusted_pr_author_unknown_bot() {
        let pr = make_pr("evil-bot[bot]", "Bot");
        assert!(!is_trusted_pr_author(&pr, None));
    }

    #[test]
    fn untrusted_pr_author_login_mismatch() {
        let pr = make_pr("attacker", "User");
        assert!(!is_trusted_pr_author(&pr, Some("my-user")));
    }

    // ---- should_process_pr_event ----

    #[test]
    fn process_pr_merged_trusted() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.merged = Some(true);
        assert!(should_process_pr_event("closed", &pr, None));
    }

    #[test]
    fn reject_pr_closed_not_merged_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(!should_process_pr_event("closed", &pr, None));
    }

    #[test]
    fn process_pr_opened_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(should_process_pr_event("opened", &pr, None));
    }

    #[test]
    fn process_pr_ready_for_review_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(should_process_pr_event("ready_for_review", &pr, None));
    }

    #[test]
    fn process_pr_synchronize_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(should_process_pr_event("synchronize", &pr, None));
    }

    #[test]
    fn reject_pr_opened_untrusted() {
        let pr = make_pr("attacker", "User");
        assert!(!should_process_pr_event("opened", &pr, None));
    }

    #[test]
    fn reject_pr_ready_for_review_untrusted() {
        let pr = make_pr("attacker", "User");
        assert!(!should_process_pr_event("ready_for_review", &pr, None));
    }

    #[test]
    fn reject_pr_synchronize_untrusted() {
        let pr = make_pr("attacker", "User");
        assert!(!should_process_pr_event("synchronize", &pr, None));
    }

    #[test]
    fn reject_pr_unknown_action_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(!should_process_pr_event("edited", &pr, None));
    }

    #[test]
    fn process_pr_opened_authenticated_user() {
        let pr = make_pr("my-user", "User");
        assert!(should_process_pr_event("opened", &pr, Some("my-user")));
    }

    #[test]
    fn reject_pr_no_user() {
        let pr = types::PullRequest {
            number: 1,
            state: "open".into(),
            merged: None,
            user: None,
        };
        assert!(!should_process_pr_event("opened", &pr, None));
    }
}
