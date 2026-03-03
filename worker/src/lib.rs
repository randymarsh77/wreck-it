//! Cloudflare Worker entry point for the wreck-it GitHub App webhook.
//!
//! This worker receives webhook deliveries from GitHub, verifies the
//! payload signature, and triggers an iteration of the wreck-it processing
//! loop.  The Rust code compiles to WASM via `wasm32-unknown-unknown` and
//! runs on the Cloudflare Workers runtime.
//!
//! # Required secrets (set via `wrangler secret put`):
//!
//! - `GITHUB_WEBHOOK_SECRET` — webhook secret for HMAC-SHA256 verification.
//! - `GITHUB_APP_TOKEN` — a GitHub token (installation token or PAT) with
//!   read/write access to repository contents on the state branch.

mod github;
mod processor;
mod types;
mod webhook;

use webhook::{verify_signature, WebhookEvent};
use worker::*;

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

    let github_token = env
        .secret("GITHUB_APP_TOKEN")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("Missing GITHUB_APP_TOKEN secret".into()))?;

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
            // Process when an issue is opened or labeled with "wreck-it".
            let action = payload.action.as_deref().unwrap_or("");
            let has_label = payload
                .issue
                .as_ref()
                .map(|i| i.labels.iter().any(|l| l.name == "wreck-it"))
                .unwrap_or(false);
            action == "opened" && has_label || action == "labeled" && has_label
        }
        WebhookEvent::Push => {
            // Process pushes to the state branch (external state updates).
            true
        }
        WebhookEvent::PullRequest => {
            // Process when a PR is merged (task completion signal).
            let action = payload.action.as_deref().unwrap_or("");
            let merged = payload
                .pull_request
                .as_ref()
                .and_then(|pr| pr.merged)
                .unwrap_or(false);
            action == "closed" && merged
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
