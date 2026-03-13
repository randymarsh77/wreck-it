//! "Unstuck" command: scan open PRs **and the main branch** for failing CI
//! checks.  For PRs it comments `@copilot` to request fixes.  For the main
//! branch it opens a GitHub issue (if one doesn't already exist) and assigns
//! a coding agent to fix the build.  When an issue already exists and the
//! agent has created a PR, the command advances that PR toward merge —
//! converting from draft, approving workflows, enabling auto-merge, and
//! commenting `@copilot` when checks are failing.
//!
//! This can be used as a standalone CLI command (`wreck-it unstuck`) or as a
//! ralph command (`command = "unstuck"` in `[[ralphs]]`).

use crate::cloud_agent::{resolve_repo_info, CloudAgentClient, CloudAgentStatus, PrMergeStatus};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::state_worktree::{detect_default_branch, ensure_state_worktree};
use crate::types::Config;
use anyhow::{Context, Result};
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Title prefix used for issues created when the main branch has failing
/// checks.  This is also used to search for existing issues so that we don't
/// duplicate work.
const MAIN_BRANCH_ISSUE_TITLE: &str = "[wreck-it] Fix failing checks on";

/// Run the unstuck logic: find open PRs with failing checks and comment
/// `@copilot` to request fixes.  Additionally, check the main/default branch
/// and open a fix-the-build issue when its checks are failing.
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

    // ── Check open PRs ──────────────────────────────────────────────
    let prs = client.list_open_prs().await?;
    if prs.is_empty() {
        println!("[wreck-it] unstuck: no open PRs found");
    } else {
        println!("[wreck-it] unstuck: found {} open PR(s)", prs.len());
    }

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

    // ── Check the main / default branch ─────────────────────────────
    // Errors are logged inside check_main_branch; we intentionally avoid
    // propagating them so that a transient API failure doesn't prevent the
    // overall unstuck command from succeeding.
    check_main_branch(&client, work_dir).await;

    Ok(())
}

/// Detect the default branch, check its latest check-runs, and open an issue
/// (with an assigned agent) when the build is failing – unless an issue
/// already exists.
async fn check_main_branch(client: &CloudAgentClient, work_dir: &Path) {
    let default_branch = match detect_default_branch(work_dir) {
        Ok(b) => b,
        Err(e) => {
            println!(
                "[wreck-it] unstuck: could not detect default branch: {} — skipping main-branch check",
                e,
            );
            return;
        }
    };

    println!(
        "[wreck-it] unstuck: checking default branch '{}' for failing checks",
        default_branch,
    );

    let has_failures = match client.has_failing_checks_for_ref(&default_branch).await {
        Ok(v) => v,
        Err(e) => {
            println!(
                "[wreck-it] unstuck: failed to check '{}': {}",
                default_branch, e,
            );
            return;
        }
    };

    if !has_failures {
        println!(
            "[wreck-it] unstuck: '{}' checks are passing — nothing to do",
            default_branch,
        );
        return;
    }

    println!(
        "[wreck-it] unstuck: '{}' has failing checks — checking for existing issue",
        default_branch,
    );

    // Build the exact title we would use for the issue so the search is
    // precise.
    let issue_title = format!("{} `{}`", MAIN_BRANCH_ISSUE_TITLE, default_branch);

    match client.find_open_issue_by_title(&issue_title).await {
        Ok(Some(existing)) => {
            println!(
                "[wreck-it] unstuck: issue #{} already tracks the failing '{}' build — checking for linked PR",
                existing, default_branch,
            );
            // The agent may have created a PR from this issue.  Try to
            // find and advance it toward merge.
            advance_issue_pr(client, existing).await;
        }
        Ok(None) => {
            // No existing issue – create one and assign an agent.
            let body = format!(
                "The CI checks on the `{}` branch are currently failing.\n\n\
                 Please investigate the failures and push a fix.\n\
                 Use available tools (e.g. `cargo fmt`, `cargo clippy`, test runners) as needed.",
                default_branch,
            );

            match client
                .trigger_agent("unstuck", &issue_title, &body, &[], None, None)
                .await
            {
                Ok(result) => {
                    println!(
                        "[wreck-it] unstuck: opened issue #{} ({}) and assigned agent to fix '{}'",
                        result.issue_number, result.issue_url, default_branch,
                    );
                }
                Err(e) => {
                    println!(
                        "[wreck-it] unstuck: failed to create issue for '{}': {}",
                        default_branch, e,
                    );
                }
            }
        }
        Err(e) => {
            println!(
                "[wreck-it] unstuck: failed to search for existing issues: {}",
                e,
            );
        }
    }
}

/// Given an existing issue created by the unstuck workflow, check whether the
/// agent has created a linked PR and advance it toward merge — mirroring the
/// behaviour of the normal ralph lifecycle (convert from draft, approve
/// workflows, enable auto-merge, comment `@copilot` on failures).
async fn advance_issue_pr(client: &CloudAgentClient, issue_number: u64) {
    let status = match client.check_agent_status(issue_number).await {
        Ok(s) => s,
        Err(e) => {
            println!(
                "[wreck-it] unstuck: failed to check agent status for issue #{}: {}",
                issue_number, e,
            );
            return;
        }
    };

    let pr_number = match status {
        CloudAgentStatus::PrCreated { pr_number, .. } => pr_number,
        CloudAgentStatus::PrCreatedAgentWorking { pr_number, .. } => {
            println!(
                "[wreck-it] unstuck: issue #{} — agent created PR #{} but is still working",
                issue_number, pr_number,
            );
            return;
        }
        CloudAgentStatus::Working => {
            println!(
                "[wreck-it] unstuck: issue #{} — agent is still working, no PR yet",
                issue_number,
            );
            return;
        }
        CloudAgentStatus::CompletedNoPr => {
            println!(
                "[wreck-it] unstuck: issue #{} — agent completed without creating a PR",
                issue_number,
            );
            return;
        }
    };

    println!(
        "[wreck-it] unstuck: issue #{} has linked PR #{} — advancing",
        issue_number, pr_number,
    );

    let merge_status = match client.check_pr_merge_status(pr_number).await {
        Ok(s) => s,
        Err(e) => {
            println!(
                "[wreck-it] unstuck: failed to check merge status for PR #{}: {}",
                pr_number, e,
            );
            return;
        }
    };

    match merge_status {
        PrMergeStatus::Draft => {
            println!(
                "[wreck-it] unstuck: PR #{} is a draft — marking ready for review",
                pr_number,
            );
            if let Err(e) = client.mark_pr_ready_for_review(pr_number).await {
                println!(
                    "[wreck-it] unstuck: failed to mark PR #{} ready: {}",
                    pr_number, e,
                );
            }
        }
        PrMergeStatus::AgentWorkInProgress => {
            println!(
                "[wreck-it] unstuck: PR #{} — agent is still working, skipping",
                pr_number,
            );
        }
        PrMergeStatus::NotMergeable => {
            // Check for failing CI checks first.
            let has_failures = match client.has_failing_checks_for_pr(pr_number).await {
                Ok(v) => v,
                Err(e) => {
                    println!(
                        "[wreck-it] unstuck: failed to check failing checks for PR #{}: {}",
                        pr_number, e,
                    );
                    false
                }
            };
            if has_failures {
                println!(
                    "[wreck-it] unstuck: PR #{} has failing checks — requesting @copilot fix",
                    pr_number,
                );
                if let Err(e) = client
                    .comment_on_pr(
                        pr_number,
                        "@copilot The CI checks on this PR are failing. \
                         Please investigate the failures and push a fix.",
                    )
                    .await
                {
                    println!(
                        "[wreck-it] unstuck: failed to comment on PR #{}: {}",
                        pr_number, e,
                    );
                }
            } else {
                println!(
                    "[wreck-it] unstuck: PR #{} not yet mergeable — approving workflows and enabling auto-merge",
                    pr_number,
                );
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    println!(
                        "[wreck-it] unstuck: failed to approve workflows for PR #{}: {}",
                        pr_number, e,
                    );
                }
                if let Err(e) = client.enable_auto_merge(pr_number).await {
                    println!(
                        "[wreck-it] unstuck: failed to enable auto-merge for PR #{}: {}",
                        pr_number, e,
                    );
                }
            }
        }
        PrMergeStatus::Mergeable => {
            // Approve any pending workflows and enable auto-merge so that
            // required checks (if any) can complete before merge.
            println!(
                "[wreck-it] unstuck: PR #{} is mergeable — approving workflows and enabling auto-merge",
                pr_number,
            );
            if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                println!(
                    "[wreck-it] unstuck: failed to approve workflows for PR #{}: {}",
                    pr_number, e,
                );
            }
            if let Err(e) = client.enable_auto_merge(pr_number).await {
                println!(
                    "[wreck-it] unstuck: failed to enable auto-merge for PR #{}: {}",
                    pr_number, e,
                );
            }
        }
        PrMergeStatus::AlreadyMerged => {
            println!(
                "[wreck-it] unstuck: PR #{} already merged — fix is in",
                pr_number,
            );
        }
        PrMergeStatus::ClosedNotMerged => {
            println!(
                "[wreck-it] unstuck: PR #{} was closed without merging",
                pr_number,
            );
        }
    }
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

    /// The issue title built for the main branch should contain the branch
    /// name so duplicate-detection searches can match it precisely.
    #[test]
    fn main_branch_issue_title_contains_branch() {
        let title = format!("{} `{}`", MAIN_BRANCH_ISSUE_TITLE, "main");
        assert!(title.contains("main"));
        assert!(title.starts_with("[wreck-it]"));
    }

    /// Different branch names should produce different titles.
    #[test]
    fn main_branch_issue_title_varies_by_branch() {
        let title_main = format!("{} `{}`", MAIN_BRANCH_ISSUE_TITLE, "main");
        let title_master = format!("{} `{}`", MAIN_BRANCH_ISSUE_TITLE, "master");
        assert_ne!(title_main, title_master);
    }
}
