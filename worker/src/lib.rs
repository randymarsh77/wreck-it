//! Cloudflare Worker entry point for the wreck-it GitHub App webhook and
//! pulse trigger system.
//!
//! This worker receives webhook deliveries from GitHub, verifies the
//! payload signature, and triggers an iteration of the wreck-it processing
//! loop.  It also supports scheduled (cron) triggers that iterate over all
//! registered repositories, injecting entropy so that tasks with expired
//! cooldowns can be picked up.
//!
//! The Rust code compiles to WASM via `wasm32-unknown-unknown` and runs on
//! the Cloudflare Workers runtime.
//!
//! # Authentication
//!
//! The worker authenticates as a GitHub App using a private key.  Set
//! `GITHUB_APP_ID` and `GITHUB_APP_PRIVATE_KEY` as Cloudflare secrets.
//! On each webhook delivery the worker generates a short-lived JWT and
//! exchanges it for an installation access token **scoped to the specific
//! repository** from the webhook payload.  This token has the full
//! permissions granted to the GitHub App, enabling the worker to create
//! issues, assign agents, merge PRs, and manage the complete task
//! lifecycle.
//!
//! # Pulse trigger (scheduled / cron)
//!
//! Some iterations need to happen without an incoming webhook event — for
//! example, when a ralph has a cooldown that has expired.  The pulse system
//! solves this by:
//!
//! 1. **Auto-registering** repositories in a KV-backed pulse registry
//!    whenever a webhook event is processed.
//! 2. **Iterating** over all registered repos when a Cloudflare cron
//!    trigger fires, running `process_iteration` for each.
//!
//! # Required secrets (set via `wrangler secret put`):
//!
//! - `GITHUB_WEBHOOK_SECRET` — webhook secret for HMAC-SHA256 verification.
//! - `GITHUB_APP_ID` — numeric App ID.
//! - `GITHUB_APP_PRIVATE_KEY` — PEM-encoded RSA private key.

mod api;
mod durable_object;
mod github;
mod github_app;
mod kv_store;
mod portal_api;
mod processor;
mod pulse;
mod scheduler;
mod types;
mod webhook;

pub use durable_object::RalphAgent;
pub use scheduler::SchedulerAgent;

use webhook::{verify_signature, WebhookEvent};
use worker::*;

/// Comment posted by the unstuck ralph when a workflow run fails on a PR.
const UNSTUCK_COMMENT: &str = "@copilot The CI checks on this PR are failing. \
    Please investigate the failures and push a fix.";

/// Default pulse interval in seconds (30 minutes) used when the cron
/// expression cannot be parsed.
const DEFAULT_PULSE_INTERVAL_SECS: u64 = 30 * 60;

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

/// Check whether a pull request appears to still be a work in progress.
///
/// Returns `true` when:
///   - The PR is a draft.
///   - The title starts with `[WIP]` (case-insensitive).
///   - The body contains incomplete GitHub checklist items (`- [ ]`).
///
/// This is used as a guard to prevent enabling auto-merge before the
/// agent has finished working on the PR.
fn is_pr_work_in_progress(pr: &types::PullRequest) -> bool {
    if pr.draft.unwrap_or(false) {
        return true;
    }
    if let Some(title) = &pr.title {
        let trimmed = title.trim_start();
        if matches!(trimmed.get(..5), Some(prefix) if prefix.eq_ignore_ascii_case("[wip]")) {
            return true;
        }
    }
    if let Some(body) = &pr.body {
        if body.contains("- [ ]") {
            return true;
        }
    }
    false
}

/// Determine whether a pull request webhook event should be processed.
///
/// Returns `true` for:
///   - `closed` + `merged == true` (task completion signal)
///   - `opened`, `ready_for_review`, `synchronize` (workflow approval)
///   - `review_requested`, `edited` (agent-finished-work signals)
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
        // Agent-finished-work signals — trigger a full iteration so the
        // orchestrator can react to completed agent work (e.g. Copilot
        // requesting a review or editing a PR title).
        || ["review_requested", "edited"].contains(&action)
}

#[event(fetch)]
async fn main(req: Request, env: Env, _ctx: Context) -> Result<Response> {
    let url = req.url()?;
    let path = url.path();

    // Route API requests through the Router.
    if path.starts_with("/api/") {
        let router = Router::new();
        let router = api::register_routes(router);
        let router = portal_api::register_portal_routes(router);
        return router.run(req, env).await;
    }

    // Everything else goes to the webhook handler.
    match handle_webhook(req, env).await {
        Ok(resp) => {
            console_log!("[wreck-it] → {} response", resp.status_code());
            Ok(resp)
        }
        Err(e) => {
            console_error!("[wreck-it] ✗ unhandled error: {e}");
            Response::error(format!("Internal error: {e}"), 500)
        }
    }
}

#[event(scheduled)]
async fn scheduled(_event: ScheduledEvent, env: Env, _ctx: ScheduleContext) {
    console_log!("[wreck-it][pulse] cron trigger fired");
    match pulse::run_pulse(&env).await {
        Ok(summary) => {
            console_log!("[wreck-it][pulse] {}", summary);
        }
        Err(e) => {
            console_error!("[wreck-it][pulse] ✗ pulse failed: {e}");
        }
    }
}

async fn handle_webhook(mut req: Request, env: Env) -> Result<Response> {
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
        console_warn!("[wreck-it] ✗ invalid webhook signature");
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
    let payload: types::WebhookPayload = match serde_json::from_slice(&body_bytes) {
        Ok(p) => p,
        Err(e) => {
            console_error!(
                "[wreck-it] ✗ failed to parse {} payload ({} bytes): {e}",
                event_header,
                body_bytes.len(),
            );
            return Err(Error::RustError(format!("Failed to parse payload: {e}")));
        }
    };

    let action = payload.action.as_deref().unwrap_or("-");
    let repo_name_log = payload
        .repository
        .as_ref()
        .map(|r| r.full_name.as_str())
        .unwrap_or("unknown");
    console_log!(
        "[wreck-it] ← event={} action={} repo={}",
        event_header,
        action,
        repo_name_log,
    );

    // Quick filter: immediately handle or reject events we never process.
    match &event {
        WebhookEvent::Other(name) if name == "ping" => {
            return Response::ok("pong");
        }
        WebhookEvent::Installation => {
            // Handle installation created/new_permissions_accepted events
            // by registering all repos and initializing the scheduler.
            return handle_installation_event(&payload, &env).await;
        }
        WebhookEvent::Issues
        | WebhookEvent::Push
        | WebhookEvent::PullRequest
        | WebhookEvent::WorkflowRun => {
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

    // Check installation-level events_enabled setting.  When disabled, all
    // webhook processing is short-circuited for this installation.
    if let Some(installation) = &payload.installation {
        if let Ok(kv) = env.kv(kv_store::KV_BINDING) {
            match kv_store::load_installation_settings(&kv, installation.id).await {
                Ok(settings) if !settings.events_enabled => {
                    console_log!(
                        "[wreck-it] events disabled for installation {} — ignoring",
                        installation.id,
                    );
                    return Response::ok("events disabled for this installation");
                }
                Err(e) => {
                    console_warn!(
                        "[wreck-it] failed to load installation settings: {e} — proceeding",
                    );
                }
                _ => {}
            }
        }
    }

    // Resolve the GitHub API token.
    //
    // Use App credentials (GITHUB_APP_ID + GITHUB_APP_PRIVATE_KEY) to vend
    // an installation token scoped to this specific repository.
    console_log!("[wreck-it] resolving app token for {}/{}", owner, repo_name);
    let github_token = match resolve_github_token(&env, &payload, repo_name).await {
        Ok(t) => {
            console_log!(
                "[wreck-it] ✓ app token resolved for {}/{}",
                owner,
                repo_name
            );
            t
        }
        Err(e) => {
            console_error!(
                "[wreck-it] ✗ token resolution failed for {}/{}: {e}",
                owner,
                repo_name,
            );
            return Err(e);
        }
    };

    // Resolve the authenticated user's login for trust checks.
    //
    // In the App flow, issues are created as the App bot (user.type == "Bot")
    // and the authenticated_login is not needed for the trust check.
    let authenticated_login = fetch_authenticated_login(&github_token).await;
    let auth_login_ref = authenticated_login.as_deref();
    console_log!(
        "[wreck-it] authenticated_login={}",
        auth_login_ref.unwrap_or("(none)"),
    );

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
            let issue_user = payload
                .issue
                .as_ref()
                .and_then(|i| i.user.as_ref())
                .map(|u| format!("{}({})", u.login, u.user_type.as_deref().unwrap_or("?")))
                .unwrap_or_else(|| "(no user)".into());
            console_log!(
                "[wreck-it] issue filter: action={} has_label={} trusted={} user={}",
                action,
                has_label,
                trusted,
                issue_user,
            );
            (action == "opened" || action == "labeled") && has_label && trusted
        }
        WebhookEvent::Push => {
            // Process pushes to the state branch (external state updates).
            console_log!("[wreck-it] push event — will process");
            true
        }
        WebhookEvent::PullRequest => {
            let action = payload.action.as_deref().unwrap_or("");
            let result = payload
                .pull_request
                .as_ref()
                .map(|pr| should_process_pr_event(action, pr, auth_login_ref))
                .unwrap_or(false);
            let pr_user = payload
                .pull_request
                .as_ref()
                .and_then(|pr| pr.user.as_ref())
                .map(|u| u.login.as_str())
                .unwrap_or("(no user)");
            console_log!(
                "[wreck-it] PR filter: action={} user={} should_process={}",
                action,
                pr_user,
                result,
            );
            result
        }
        WebhookEvent::WorkflowRun => {
            // Process completed workflow runs that have a failure conclusion
            // and at least one associated pull request.
            let action = payload.action.as_deref().unwrap_or("");
            let conclusion = payload
                .workflow_run
                .as_ref()
                .and_then(|wr| wr.conclusion.as_deref())
                .unwrap_or("");
            let pr_count = payload
                .workflow_run
                .as_ref()
                .map(|wr| wr.pull_requests.len())
                .unwrap_or(0);
            console_log!(
                "[wreck-it] workflow_run filter: action={} conclusion={} pull_requests={}",
                action,
                conclusion,
                pr_count,
            );
            action == "completed" && conclusion == "failure" && pr_count > 0
        }
        _ => false,
    };

    if !should_process {
        console_log!("[wreck-it] event filtered out — ignoring");
        return Response::ok("event ignored");
    }
    console_log!("[wreck-it] event accepted — processing");

    // Auto-register this repository in the pulse registry so that
    // scheduled (cron) triggers can iterate it even when no webhook
    // events arrive.
    if let Some(installation) = &payload.installation {
        if let Ok(kv) = env.kv(kv_store::KV_BINDING) {
            let reg = types::PulseRegistration {
                owner: owner.to_string(),
                repo: repo_name.to_string(),
                installation_id: installation.id,
                default_branch: default_branch.to_string(),
            };
            if let Err(e) = kv_store::upsert_pulse_registration(&kv, &reg).await {
                console_warn!("[wreck-it] pulse registration upsert failed: {e}");
            }
        }
    }

    // Create GitHub client and run the iteration.
    let client = github::GitHubClient::new(owner, repo_name, &github_token);

    // Verify the state branch exists before processing.
    let config_file = client
        .get_file(".wreck-it/config.toml", default_branch)
        .await
        .map_err(Error::RustError)?;

    if config_file.is_none() {
        console_log!(
            "[wreck-it] no .wreck-it/config.toml found on {} — skipping",
            default_branch
        );
        return Response::ok("no wreck-it configuration found; skipping");
    }

    console_log!("[wreck-it] config found — proceeding with event handling");

    // For PR events from trusted authors, approve pending workflow runs
    // and enable auto-merge when required checks are detected.  This
    // handles the case where workflow runs need explicit approval before
    // they can execute (e.g. first-time contributors, outside
    // collaborators, or fork PRs).
    //
    // Some PR actions signal that an agent has finished its work (e.g.
    // Copilot requesting a review or editing a PR title).  For these
    // "iteration-triggering" actions we perform workflow approval **and**
    // fall through to run a full iteration so the orchestrator can react.
    if event == WebhookEvent::PullRequest {
        if let Some(pr) = &payload.pull_request {
            let action = payload.action.as_deref().unwrap_or("");
            let merged = pr.merged.unwrap_or(false);

            if action == "closed" && merged {
                // Merged PR — handle task completion.
                console_log!("[wreck-it] handling merged PR #{}", pr.number);
                match processor::process_merged_pr(&client, default_branch, pr.number).await {
                    Ok(result) => {
                        let status = if result.changed { "processed" } else { "no-op" };
                        console_log!("[wreck-it] pr-merged {}: {}", status, result.summary);
                        return Response::ok(format!("pr-merged {status}: {}", result.summary));
                    }
                    Err(e) => {
                        console_error!("[wreck-it] ✗ PR merge handling failed: {e}");
                        // Fall through to the normal iteration processing.
                    }
                }
            } else if ["review_requested", "edited", "ready_for_review"].contains(&action) {
                // Agent-finished-work signals — approve workflow runs,
                // then fall through to run a full iteration.
                let pr_number = pr.number;
                console_log!(
                    "[wreck-it] PR #{} action={} — will approve workflows and run iteration",
                    pr_number,
                    action,
                );
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    console_warn!(
                        "Failed to approve workflow runs for PR #{}: {}",
                        pr_number,
                        e,
                    );
                }

                // Enable auto-merge when the base branch has required checks
                // and the PR is not still a work in progress.
                if !is_pr_work_in_progress(pr) {
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
                } else {
                    console_log!(
                        "[wreck-it] PR #{} is work-in-progress — skipping auto-merge",
                        pr_number,
                    );
                }

                // Fall through to the iteration below.
            } else {
                // Workflow-only PR events (opened, synchronize) — approve
                // pending workflow runs and enable auto-merge, then return.
                let pr_number = pr.number;
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    console_warn!(
                        "Failed to approve workflow runs for PR #{}: {}",
                        pr_number,
                        e,
                    );
                }

                // Enable auto-merge when the base branch has required checks
                // and the PR is not still a work in progress.
                if !is_pr_work_in_progress(pr) {
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
                } else {
                    console_log!(
                        "[wreck-it] PR #{} is work-in-progress — skipping auto-merge",
                        pr_number,
                    );
                }

                return Response::ok(format!(
                    "pr-workflow-approval: processed PR #{} ({})",
                    pr_number, action,
                ));
            }
        }
    }

    // For workflow_run events with a failure conclusion, check if the repo
    // has an "unstuck" ralph configured and, if so, comment `@copilot` on
    // each associated PR to request fixes.
    if event == WebhookEvent::WorkflowRun {
        if let Some(workflow_run) = &payload.workflow_run {
            let config_content = config_file
                .as_ref()
                .and_then(|f| github::GitHubClient::decode_content(f).ok());
            let has_unstuck = config_content
                .as_deref()
                .and_then(|c| toml::from_str::<types::RepoConfig>(c).ok())
                .map(|cfg| {
                    cfg.ralphs
                        .iter()
                        .any(|r| r.command.as_deref() == Some("unstuck"))
                })
                .unwrap_or(false);

            if !has_unstuck {
                console_log!(
                    "[wreck-it] no unstuck ralph configured — ignoring workflow_run failure"
                );
                return Response::ok("workflow_run failure ignored: no unstuck ralph configured");
            }

            let mut commented = 0u32;
            for wr_pr in &workflow_run.pull_requests {
                console_log!(
                    "[wreck-it] workflow_run failure — commenting on PR #{}",
                    wr_pr.number,
                );
                match client.comment_on_pr(wr_pr.number, UNSTUCK_COMMENT).await {
                    Ok(()) => {
                        commented += 1;
                    }
                    Err(e) => {
                        console_warn!(
                            "[wreck-it] failed to comment on PR #{}: {}",
                            wr_pr.number,
                            e,
                        );
                    }
                }
            }

            return Response::ok(format!(
                "workflow_run failure: commented on {} PR(s)",
                commented,
            ));
        }
    }

    console_log!("[wreck-it] running iteration for {}/{}", owner, repo_name);
    match processor::process_iteration(&client, default_branch).await {
        Ok(result) => {
            let status = if result.changed { "processed" } else { "no-op" };
            console_log!("[wreck-it] iteration {}: {}", status, result.summary);
            Response::ok(format!("{status}: {}", result.summary))
        }
        Err(e) => {
            console_error!("[wreck-it] ✗ iteration failed: {e}");
            Response::error(format!("Processing failed: {e}"), 500)
        }
    }
}

/// Handle `installation` webhook events.
///
/// When a GitHub App is installed (`created`) or its permissions are
/// accepted (`new_permissions_accepted`), this registers all repositories
/// from the installation in the pulse registry, saves default settings to
/// KV, and initialises the `SchedulerAgent` Durable Object.
///
/// This ensures the cron-based pulse system picks up repositories without
/// waiting for an individual webhook event from each repo.
async fn handle_installation_event(payload: &types::WebhookPayload, env: &Env) -> Result<Response> {
    let action = payload.action.as_deref().unwrap_or("");

    // Only bootstrap on install / permission-grant events.
    if !["created", "new_permissions_accepted"].contains(&action) {
        console_log!(
            "[wreck-it] installation event action='{}' — ignoring",
            action,
        );
        return Response::ok("installation event ignored");
    }

    let installation = payload
        .installation
        .as_ref()
        .ok_or_else(|| Error::RustError("Missing installation in payload".into()))?;

    let installation_id = installation.id;

    console_log!(
        "[wreck-it] installation {} action='{}' — bootstrapping {} repo(s)",
        installation_id,
        action,
        payload.repositories.len(),
    );

    let kv = env
        .kv(kv_store::KV_BINDING)
        .map_err(|e| Error::RustError(format!("KV binding failed: {e}")))?;

    // Determine the owner from the installation account or the first repo.
    let owner = installation
        .account
        .as_ref()
        .map(|a| a.login.clone())
        .or_else(|| {
            payload
                .repositories
                .first()
                .and_then(|r| r.full_name.split('/').next().map(|s| s.to_string()))
        })
        .unwrap_or_default();

    // Register every repository from the payload in the pulse registry.
    let mut registered = 0u32;
    for repo in &payload.repositories {
        // Extract owner from the repo's full_name (owner/repo format) to
        // handle the unlikely case of repos from different owners.
        let repo_owner = repo.full_name.split('/').next().unwrap_or(&owner);
        let repo_name = &repo.name;
        let reg = types::PulseRegistration {
            owner: repo_owner.to_string(),
            repo: repo_name.clone(),
            installation_id,
            default_branch: repo.default_branch.clone(),
        };
        if let Err(e) = kv_store::upsert_pulse_registration(&kv, &reg).await {
            console_warn!(
                "[wreck-it] pulse registration upsert failed for {}/{}: {e}",
                repo_owner,
                repo_name,
            );
        } else {
            registered += 1;
        }
    }

    // Save default settings so that pulse_enabled and events_enabled are
    // persisted, and sync the SchedulerAgent Durable Object.
    let settings = kv_store::load_installation_settings(&kv, installation_id)
        .await
        .unwrap_or_default();

    kv_store::save_installation_settings(&kv, installation_id, &settings)
        .await
        .unwrap_or_else(|e| {
            console_warn!(
                "[wreck-it] failed to save installation settings for {}: {e}",
                installation_id,
            );
        });

    // Sync the SchedulerAgent DO.
    if let Err(e) = sync_scheduler_do_from_env(env, installation_id, &settings).await {
        console_warn!(
            "[wreck-it] scheduler sync failed for installation {}: {e}",
            installation_id,
        );
    }

    Response::ok(format!(
        "installation bootstrapped: registered {registered} repo(s) for installation {installation_id}",
    ))
}

/// Sync the `SchedulerAgent` Durable Object from an [`Env`] reference.
///
/// This is the same logic as [`portal_api::sync_scheduler_do`] but works
/// outside the Router context (e.g. from a webhook handler).
async fn sync_scheduler_do_from_env(
    env: &Env,
    installation_id: u64,
    settings: &types::InstallationSettings,
) -> std::result::Result<(), String> {
    let namespace = env
        .durable_object("SCHEDULER_AGENT")
        .map_err(|e| format!("DO binding failed: {e}"))?;
    let do_name = scheduler::scheduler_name(installation_id);
    let id = namespace
        .id_from_name(&do_name)
        .map_err(|e| format!("DO id failed: {e}"))?;
    let stub = id.get_stub().map_err(|e| format!("DO stub failed: {e}"))?;

    if settings.pulse_enabled {
        let interval_secs = portal_api::cron_to_interval_secs(&settings.pulse_cron)
            .unwrap_or(DEFAULT_PULSE_INTERVAL_SECS);

        let body = serde_json::json!({
            "installation_id": installation_id,
            "interval_secs": interval_secs,
        });
        let body_str =
            serde_json::to_string(&body).map_err(|e| format!("JSON serialization failed: {e}"))?;

        let mut init = worker::RequestInit::new();
        init.with_method(worker::Method::Post);
        init.with_body(Some(wasm_bindgen::JsValue::from_str(&body_str)));
        let do_req = worker::Request::new_with_init("https://do/schedule", &init)
            .map_err(|e| format!("DO request failed: {e}"))?;
        stub.fetch_with_request(do_req)
            .await
            .map_err(|e| format!("DO fetch failed: {e}"))?;
    } else {
        let do_req = worker::Request::new("https://do/disable", worker::Method::Post)
            .map_err(|e| format!("DO request failed: {e}"))?;
        stub.fetch_with_request(do_req)
            .await
            .map_err(|e| format!("DO fetch failed: {e}"))?;
    }

    Ok(())
}

/// Resolve the GitHub API token from environment secrets.
///
/// Generates a JWT from the GitHub App credentials (GITHUB_APP_ID +
/// GITHUB_APP_PRIVATE_KEY), then exchanges it for an installation access
/// token scoped to the given repository.
async fn resolve_github_token(
    env: &Env,
    payload: &types::WebhookPayload,
    repo_name: &str,
) -> Result<String> {
    let app_id = env
        .secret("GITHUB_APP_ID")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("Missing GITHUB_APP_ID secret".into()))?;
    let private_key = env
        .secret("GITHUB_APP_PRIVATE_KEY")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("Missing GITHUB_APP_PRIVATE_KEY secret".into()))?;

    let installation_id = payload.installation.as_ref().map(|i| i.id).ok_or_else(|| {
        Error::RustError(
            "Webhook payload has no installation.id — is the \
                 GitHub App installed on this repository?"
                .into(),
        )
    })?;

    let now_secs = js_sys_now_secs();
    let jwt = github_app::generate_jwt(&app_id, &private_key, now_secs)
        .map_err(|e| Error::RustError(format!("JWT generation failed: {e}")))?;

    let token = github_app::vend_installation_token(installation_id, &jwt, repo_name)
        .await
        .map_err(|e| Error::RustError(format!("Token vending failed: {e}")))?;

    Ok(token)
}

/// Fetch the login of the user (or bot) authenticated by `token`.
///
/// Calls `GET /user` and returns the `login` field.  Returns `None` on any
/// error (network, parse, missing field) so that callers can fall back to
/// the stricter Bot-only check.
async fn fetch_authenticated_login(token: &str) -> Option<String> {
    let url = "https://api.github.com/user";

    let headers = worker::Headers::new();
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
            title: Some("test".into()),
            body: None,
            draft: None,
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
            title: None,
            body: None,
            draft: None,
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
    fn process_pr_edited_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(should_process_pr_event("edited", &pr, None));
    }

    #[test]
    fn process_pr_review_requested_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(should_process_pr_event("review_requested", &pr, None));
    }

    #[test]
    fn reject_pr_review_requested_untrusted() {
        let pr = make_pr("attacker", "User");
        assert!(!should_process_pr_event("review_requested", &pr, None));
    }

    #[test]
    fn reject_pr_edited_untrusted() {
        let pr = make_pr("attacker", "User");
        assert!(!should_process_pr_event("edited", &pr, None));
    }

    #[test]
    fn reject_pr_unknown_action_trusted() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(!should_process_pr_event("labeled", &pr, None));
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
            title: None,
            body: None,
            draft: None,
            state: "open".into(),
            merged: None,
            user: None,
        };
        assert!(!should_process_pr_event("opened", &pr, None));
    }

    // ---- is_pr_work_in_progress ----

    #[test]
    fn wip_draft_pr() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.draft = Some(true);
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn wip_title_prefix_lower() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("[wip] some feature".into());
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn wip_title_prefix_upper() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("[WIP] Fix auto merge issue".into());
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn wip_title_prefix_mixed() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("[Wip] mixed case".into());
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn wip_title_no_space() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("[WIP]no space".into());
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn wip_title_leading_whitespace() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("  [wip] with leading space".into());
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn wip_incomplete_checklist() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.body = Some("- [x] done\n- [ ] not done yet".into());
        assert!(is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_complete_checklist() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.body = Some("- [x] all done\n- [x] also done".into());
        assert!(!is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_normal_pr() {
        let pr = make_pr("copilot-swe-agent[bot]", "Bot");
        assert!(!is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_embedded_wip() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("fix: [wip] embedded".into());
        assert!(!is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_empty_title() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("".into());
        assert!(!is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_short_title() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = Some("[wi".into());
        assert!(!is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_no_title_no_body() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.title = None;
        pr.body = None;
        assert!(!is_pr_work_in_progress(&pr));
    }

    #[test]
    fn not_wip_draft_false() {
        let mut pr = make_pr("copilot-swe-agent[bot]", "Bot");
        pr.draft = Some(false);
        assert!(!is_pr_work_in_progress(&pr));
    }

    // ---- cron_to_interval_secs ----

    #[test]
    fn cron_every_30_minutes() {
        assert_eq!(
            portal_api::cron_to_interval_secs("*/30 * * * *"),
            Some(1800)
        );
    }

    #[test]
    fn cron_every_15_minutes() {
        assert_eq!(portal_api::cron_to_interval_secs("*/15 * * * *"), Some(900));
    }

    #[test]
    fn cron_every_1_minute() {
        assert_eq!(portal_api::cron_to_interval_secs("*/1 * * * *"), Some(60));
    }

    #[test]
    fn cron_unsupported_expression() {
        assert_eq!(portal_api::cron_to_interval_secs("0 */2 * * *"), None);
    }

    #[test]
    fn cron_invalid_expression() {
        assert_eq!(portal_api::cron_to_interval_secs("bad"), None);
    }
}
