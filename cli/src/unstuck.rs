//! "Unstuck" command: scan open PRs for failing CI checks and ask
//! `@copilot` to fix them.
//!
//! This can be used as a standalone CLI command (`wreck-it unstuck`) or as a
//! ralph command (`command = "unstuck"` in `[[ralphs]]`).

use crate::cloud_agent::{resolve_repo_info, CloudAgentClient};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::state_worktree::ensure_state_worktree;
use crate::types::Config;
use anyhow::{Context, Result};
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Run the unstuck logic: find open PRs with failing checks and comment
/// `@copilot` to request fixes.
pub async fn run_unstuck(config: &Config) -> Result<()> {
    let work_dir = &config.work_dir;

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required for unstuck command")?;

    let headless_cfg = load_headless_cfg(work_dir)?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    println!(
        "[wreck-it] unstuck: scanning {}/{} for PRs with failing checks",
        repo_owner, repo_name,
    );

    let mut client = CloudAgentClient::new(github_token, repo_owner, repo_name);
    client.resolve_authenticated_login().await;

    let prs = client.list_open_prs().await?;
    if prs.is_empty() {
        println!("[wreck-it] unstuck: no open PRs found");
        return Ok(());
    }

    println!("[wreck-it] unstuck: found {} open PR(s)", prs.len());

    let mut requested = 0u32;
    for pr in &prs {
        match client.has_failing_checks_for_pr(pr.number).await {
            Ok(true) => {
                println!(
                    "[wreck-it] unstuck: PR #{} ({}) has failing checks — requesting fix",
                    pr.number, pr.title,
                );
                if let Err(e) = client
                    .comment_on_pr(
                        pr.number,
                        "@copilot The CI checks on this PR are failing. \
                         Please investigate the failures and push a fix. \
                         Use available tools (e.g. `cargo fmt`, `cargo clippy`, \
                         test runners) as needed.",
                    )
                    .await
                {
                    println!(
                        "[wreck-it] unstuck: failed to comment on PR #{}: {}",
                        pr.number, e,
                    );
                } else {
                    requested += 1;
                }
            }
            Ok(false) => {
                println!(
                    "[wreck-it] unstuck: PR #{} ({}) — checks OK, skipping",
                    pr.number, pr.title,
                );
            }
            Err(e) => {
                println!(
                    "[wreck-it] unstuck: failed to check PR #{}: {}",
                    pr.number, e,
                );
            }
        }
    }

    println!(
        "[wreck-it] unstuck: done — requested fixes on {} PR(s) with failing checks",
        requested,
    );
    Ok(())
}

/// Load the headless config, preferring the state worktree copy when the state
/// branch can be determined.
fn load_headless_cfg(work_dir: &Path) -> Result<HeadlessConfig> {
    // Try the main checkout first.
    let bootstrap_path = work_dir.join(DEFAULT_CONFIG_FILE);
    let bootstrap = if bootstrap_path.exists() {
        load_headless_config(&bootstrap_path)?
    } else {
        HeadlessConfig::default()
    };

    // Attempt to set up the state worktree so we can read a more up-to-date
    // config from there (mirrors the pattern in run_headless).
    let repo_cfg = crate::repo_config::load_repo_config(work_dir)
        .ok()
        .flatten();
    let state_branch = repo_cfg
        .as_ref()
        .map(|c| c.state_branch.clone())
        .unwrap_or_else(|| bootstrap.state_branch.clone());

    if let Ok(state_dir) = ensure_state_worktree(work_dir, &state_branch) {
        let state_cfg_path = state_dir.join(DEFAULT_CONFIG_FILE);
        if state_cfg_path.exists() {
            return load_headless_config(&state_cfg_path)
                .context("Failed to load .wreck-it.toml from state worktree");
        }
    }

    Ok(bootstrap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the default headless config is returned when no config file
    /// exists (no panic).
    #[test]
    fn load_headless_cfg_defaults_without_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_headless_cfg(dir.path()).unwrap();
        assert_eq!(cfg.state_branch, "wreck-it-state");
    }
}
