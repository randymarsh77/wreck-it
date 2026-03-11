//! "Merge" command: scan open PRs for merge conflicts with the base branch
//! and resolve them.
//!
//! When `backend = "cloud_agent"` (the default), the command creates a GitHub
//! issue describing the conflict with full context (PR body, conflicting
//! commit messages, and code diff) and assigns a coding agent to fix it.
//!
//! When `backend = "cli"`, the command performs a local `git merge` of the
//! base branch into the PR branch and pushes the result directly.
//!
//! This can be used as a standalone CLI command (`wreck-it merge`) or as a
//! ralph command (`command = "merge"` in `[[ralphs]]`).

use crate::cloud_agent::{
    resolve_repo_info, CloudAgentClient, CloudAgentStatus, PrMergeStatus,
};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::headless_state::{load_headless_state, save_headless_state, HeadlessState, PendingIssue, TrackedPr};
use crate::repo_config::RalphConfig;
use crate::state_worktree::{commit_and_push_state, ensure_state_worktree};
use crate::types::Config;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Supported backend values.
const BACKEND_CLOUD_AGENT: &str = "cloud_agent";
const BACKEND_CLI: &str = "cli";

/// Run the merge logic: find open PRs with merge conflicts and resolve them.
///
/// `ralph` supplies the ralph-specific config (state file, task file, backend,
/// etc.).  `backend` selects how conflicts are resolved – `"cloud_agent"`
/// (default) assigns a coding agent via a new issue, while `"cli"` merges
/// locally and pushes.
///
/// When the backend is `"cloud_agent"`, the command tracks the issues it
/// creates in persistent state.  Subsequent invocations poll those issues for
/// PRs, then manage and merge those PRs using a merge commit (since they are
/// true merges of the base branch into the feature branch).
pub async fn run_merge(
    config: &Config,
    ralph: Option<&RalphConfig>,
    backend: Option<&str>,
) -> Result<()> {
    let work_dir = &config.work_dir;
    let backend = backend.unwrap_or(BACKEND_CLOUD_AGENT);

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required for merge command")?;

    let mut headless_cfg = load_headless_cfg(work_dir)?;

    // Override state/task file from the ralph config (mirrors run_headless).
    if let Some(rc) = ralph {
        headless_cfg.task_file = rc.task_file.clone().into();
        headless_cfg.state_file = rc.state_file.clone().into();
    }

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    // Set up state worktree and load persistent state.
    let repo_cfg = crate::repo_config::load_repo_config(work_dir)
        .ok()
        .flatten();
    let state_branch = repo_cfg
        .as_ref()
        .map(|c| c.state_branch.clone())
        .unwrap_or_else(|| headless_cfg.state_branch.clone());

    let state_dir = ensure_state_worktree(work_dir, &state_branch)
        .context("Failed to set up state worktree")?;
    let state_path = state_dir.join(&headless_cfg.state_file);
    let mut state = load_headless_state(&state_path).context("Failed to load merge state")?;

    println!(
        "[wreck-it] merge: scanning {}/{} for PRs with merge conflicts (backend={})",
        repo_owner, repo_name, backend,
    );

    let mut client =
        CloudAgentClient::new(github_token.clone(), repo_owner.clone(), repo_name.clone());
    client.resolve_authenticated_login().await;

    // --- Phase 1: promote pending issues whose agents have created PRs ---
    promote_pending_issues(&client, &mut state).await;

    // --- Phase 2: advance tracked PRs (merge when ready) ---
    advance_merge_tracked_prs(&client, &mut state).await;

    // --- Phase 3: scan for new PRs with merge conflicts ---
    let prs = client.list_open_prs().await?;
    if prs.is_empty() {
        println!("[wreck-it] merge: no open PRs found");
    } else {
        println!("[wreck-it] merge: found {} open PR(s)", prs.len());
    }

    let mut fixed = 0u32;
    for pr in &prs {
        let pr_detail = fetch_pr_detail(&client, &repo_owner, &repo_name, pr.number).await;
        let pr_detail = match pr_detail {
            Ok(d) => d,
            Err(e) => {
                println!(
                    "[wreck-it] merge: failed to fetch details for PR #{}: {}",
                    pr.number, e,
                );
                continue;
            }
        };

        if !pr_detail.has_conflicts {
            println!(
                "[wreck-it] merge: PR #{} ({}) — no conflicts, skipping",
                pr.number, pr.title,
            );
            continue;
        }

        // Skip if we already have a pending issue or tracked PR for this
        // conflict (avoid creating duplicate issues).
        let task_id = format!("merge-pr-{}", pr.number);
        if state
            .pending_issues
            .iter()
            .any(|pi| pi.task_id == task_id)
            || state.tracked_prs.iter().any(|tp| tp.task_id == task_id)
        {
            println!(
                "[wreck-it] merge: PR #{} already tracked, skipping",
                pr.number,
            );
            continue;
        }

        println!(
            "[wreck-it] merge: PR #{} ({}) has merge conflicts — resolving via {}",
            pr.number, pr.title, backend,
        );

        let result = match backend {
            BACKEND_CLI => {
                resolve_via_cli(work_dir, &pr_detail, &github_token, &repo_owner, &repo_name).await
            }
            _ => resolve_via_cloud_agent(&client, &pr_detail, &mut state).await,
        };

        match result {
            Ok(()) => {
                fixed += 1;
            }
            Err(e) => {
                println!(
                    "[wreck-it] merge: failed to resolve conflicts on PR #{}: {}",
                    pr.number, e,
                );
            }
        }
    }

    println!(
        "[wreck-it] merge: done — initiated conflict resolution on {} PR(s)",
        fixed,
    );

    // Persist state so the next invocation picks up where we left off.
    save_headless_state(&state_path, &state).context("Failed to save merge state")?;
    if let Err(e) = commit_and_push_state(work_dir, &state_branch, "wreck-it: update merge state")
    {
        println!("[wreck-it] merge: warning: failed to commit state: {}", e);
    }

    Ok(())
}

/// Poll pending issues and promote any that have produced a PR to tracked PRs.
async fn promote_pending_issues(client: &CloudAgentClient, state: &mut HeadlessState) {
    if state.pending_issues.is_empty() {
        return;
    }

    println!(
        "[wreck-it] merge: checking {} pending issue(s) for PRs",
        state.pending_issues.len(),
    );

    let mut promoted: Vec<u64> = Vec::new();

    for pi in &state.pending_issues {
        match client.check_agent_status(pi.issue_number).await {
            Ok(CloudAgentStatus::PrCreated { pr_number, pr_url })
            | Ok(CloudAgentStatus::PrCreatedAgentWorking { pr_number, pr_url }) => {
                println!(
                    "[wreck-it] merge: issue #{} produced PR #{} ({})",
                    pi.issue_number, pr_number, pr_url,
                );
                if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_number) {
                    state.tracked_prs.push(TrackedPr {
                        pr_number,
                        task_id: pi.task_id.clone(),
                        issue_number: Some(pi.issue_number),
                        review_requested: None,
                        merge_method: pi.merge_method.clone(),
                    });
                }
                promoted.push(pi.issue_number);
            }
            Ok(CloudAgentStatus::CompletedNoPr) => {
                println!(
                    "[wreck-it] merge: issue #{} (task {}) completed without a PR, removing",
                    pi.issue_number, pi.task_id,
                );
                promoted.push(pi.issue_number);
            }
            Ok(CloudAgentStatus::Working) => {
                println!(
                    "[wreck-it] merge: issue #{} (task {}) — agent still working",
                    pi.issue_number, pi.task_id,
                );
            }
            Err(e) => {
                println!(
                    "[wreck-it] merge: failed to check issue #{}: {}",
                    pi.issue_number, e,
                );
            }
        }
    }

    let promoted_set: HashSet<u64> = promoted.into_iter().collect();
    state
        .pending_issues
        .retain(|pi| !promoted_set.contains(&pi.issue_number));
}

/// Advance tracked PRs created by the merge ralph: merge when ready.
///
/// Uses merge commits (not squash) because these PRs represent true merges of
/// the base branch into the feature branch.
async fn advance_merge_tracked_prs(client: &CloudAgentClient, state: &mut HeadlessState) {
    if state.tracked_prs.is_empty() {
        return;
    }

    println!(
        "[wreck-it] merge: advancing {} tracked PR(s)",
        state.tracked_prs.len(),
    );

    let mut resolved: Vec<u64> = Vec::new();
    let snapshot: Vec<TrackedPr> = state.tracked_prs.clone();

    for tracked in &snapshot {
        let pr_number = tracked.pr_number;
        let merge_method = tracked.merge_method.as_deref();

        // If the agent is still assigned, skip merging for now.
        if let Some(issue_num) = tracked.issue_number {
            match client.is_agent_assigned_to_issue(issue_num).await {
                Ok(true) => {
                    println!(
                        "[wreck-it] merge: PR #{} — agent still assigned to issue #{}, skipping",
                        pr_number, issue_num,
                    );
                    continue;
                }
                Ok(false) => {}
                Err(e) => {
                    println!(
                        "[wreck-it] merge: failed to check agent assignment for issue #{}: {}",
                        issue_num, e,
                    );
                }
            }
        }

        match client.check_pr_merge_status(pr_number).await {
            Ok(PrMergeStatus::Draft) => {
                println!(
                    "[wreck-it] merge: PR #{} is still a draft, marking ready",
                    pr_number,
                );
                if let Err(e) = client.mark_pr_ready_for_review(pr_number).await {
                    println!(
                        "[wreck-it] merge: failed to mark PR #{} as ready: {}",
                        pr_number, e,
                    );
                }
            }
            Ok(PrMergeStatus::NotMergeable) | Ok(PrMergeStatus::Mergeable) => {
                // Approve any pending workflow runs first.
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    println!(
                        "[wreck-it] merge: failed to approve workflows for PR #{}: {}",
                        pr_number, e,
                    );
                }
                // Try to merge directly; fall back to auto-merge if not yet
                // mergeable.
                match client.merge_pr(pr_number, merge_method).await {
                    Ok(()) => {
                        println!("[wreck-it] merge: merged PR #{}", pr_number);
                        resolved.push(pr_number);
                    }
                    Err(e) => {
                        println!(
                            "[wreck-it] merge: PR #{} not yet mergeable ({}), enabling auto-merge",
                            pr_number, e,
                        );
                        if let Err(e2) =
                            client.enable_auto_merge(pr_number, merge_method).await
                        {
                            println!(
                                "[wreck-it] merge: failed to enable auto-merge for PR #{}: {}",
                                pr_number, e2,
                            );
                        }
                    }
                }
            }
            Ok(PrMergeStatus::AlreadyMerged) => {
                println!(
                    "[wreck-it] merge: PR #{} already merged",
                    pr_number,
                );
                resolved.push(pr_number);
            }
            Ok(PrMergeStatus::ClosedNotMerged) => {
                println!(
                    "[wreck-it] merge: PR #{} was closed without merging",
                    pr_number,
                );
                resolved.push(pr_number);
            }
            Err(e) => {
                println!(
                    "[wreck-it] merge: error checking PR #{}: {}",
                    pr_number, e,
                );
            }
        }
    }

    let resolved_set: HashSet<u64> = resolved.into_iter().collect();
    state
        .tracked_prs
        .retain(|tp| !resolved_set.contains(&tp.pr_number));
}

/// Detailed information about a PR needed for conflict resolution.
#[derive(Debug)]
struct PrDetail {
    number: u64,
    title: String,
    body: String,
    head_ref: String,
    base_ref: String,
    has_conflicts: bool,
}

/// Fetch PR details from the GitHub REST API.
async fn fetch_pr_detail(
    client: &CloudAgentClient,
    repo_owner: &str,
    repo_name: &str,
    pr_number: u64,
) -> Result<PrDetail> {
    let pr_json = client.fetch_pr_json(pr_number).await?;

    let title = pr_json["title"].as_str().unwrap_or("").to_string();
    let body = pr_json["body"].as_str().unwrap_or("").to_string();
    let head_ref = pr_json
        .pointer("/head/ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let base_ref = pr_json
        .pointer("/base/ref")
        .and_then(|v| v.as_str())
        .unwrap_or("main")
        .to_string();

    // `mergeable` is null while GitHub is still computing it; treat null as
    // "no conflicts detected yet".
    let mergeable = pr_json["mergeable"].as_bool().unwrap_or(true);
    let mergeable_state = pr_json["mergeable_state"].as_str().unwrap_or("unknown");
    let has_conflicts = !mergeable || mergeable_state == "dirty";

    let _ = (repo_owner, repo_name); // used by the caller context

    Ok(PrDetail {
        number: pr_number,
        title,
        body,
        head_ref,
        base_ref,
        has_conflicts,
    })
}

/// Build the context string for a merge conflict resolution prompt.
fn build_merge_context(detail: &PrDetail, recent_base_commits: &str, diff_summary: &str) -> String {
    let mut ctx = String::new();

    ctx.push_str(&format!("## PR #{}: {}\n\n", detail.number, detail.title,));

    if !detail.body.is_empty() {
        ctx.push_str("### PR Description\n\n");
        ctx.push_str(&detail.body);
        ctx.push_str("\n\n");
    }

    ctx.push_str(&format!(
        "### Branches\n\n- Head: `{}`\n- Base: `{}`\n\n",
        detail.head_ref, detail.base_ref,
    ));

    if !recent_base_commits.is_empty() {
        ctx.push_str(&format!(
            "### Recent commits on `{}` that may conflict\n\n```\n{}\n```\n\n",
            detail.base_ref, recent_base_commits,
        ));
    }

    if !diff_summary.is_empty() {
        ctx.push_str(&format!(
            "### Diff summary (base vs head)\n\n```\n{}\n```\n\n",
            diff_summary,
        ));
    }

    ctx
}

/// Resolve merge conflicts by creating an issue and assigning a cloud agent.
///
/// On success, the issue is recorded in `state.pending_issues` so the next
/// invocation can track the resulting PR.
async fn resolve_via_cloud_agent(
    client: &CloudAgentClient,
    detail: &PrDetail,
    state: &mut HeadlessState,
) -> Result<()> {
    // Gather context: recent base commits and diff.
    let recent_base_commits = fetch_recent_base_commits_via_api(client, &detail.base_ref).await;
    let diff_summary = fetch_diff_summary_via_api(client, detail.number).await;

    let context = build_merge_context(detail, &recent_base_commits, &diff_summary);

    let issue_body = format!(
        "PR #{} (`{}` → `{}`) has merge conflicts that need to be resolved.\n\n\
         Please check out the `{}` branch, merge `{}` into it, resolve all \
         conflicts, and push the result.\n\n\
         {}\n\n\
         ---\n\
         *Triggered by wreck-it merge ralph*",
        detail.number, detail.head_ref, detail.base_ref, detail.head_ref, detail.base_ref, context,
    );

    let task_id = format!("merge-pr-{}", detail.number);

    let result = client
        .trigger_agent(
            "merge",
            &task_id,
            &issue_body,
            &[],
            Some(&detail.head_ref),
            None,
        )
        .await?;

    println!(
        "[wreck-it] merge: created issue #{} for PR #{} conflict resolution ({})",
        result.issue_number, detail.number, result.issue_url,
    );

    // Track the issue so we can pick up the resulting PR on the next run.
    state.pending_issues.push(PendingIssue {
        issue_number: result.issue_number,
        task_id,
        merge_method: Some("merge".to_string()),
    });

    Ok(())
}

/// Resolve merge conflicts locally via git and push the fix.
async fn resolve_via_cli(
    work_dir: &Path,
    detail: &PrDetail,
    _github_token: &str,
    _repo_owner: &str,
    _repo_name: &str,
) -> Result<()> {
    use std::process::Command;

    // Fetch latest refs.
    let fetch_status = Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(work_dir)
        .status()
        .context("Failed to run `git fetch origin`")?;
    if !fetch_status.success() {
        anyhow::bail!("git fetch origin failed");
    }

    // Check out the PR branch.
    let checkout_status = Command::new("git")
        .args(["checkout", &detail.head_ref])
        .current_dir(work_dir)
        .status()
        .context("Failed to checkout PR branch")?;
    if !checkout_status.success() {
        anyhow::bail!("git checkout {} failed", detail.head_ref);
    }

    // Attempt the merge.
    let merge_output = Command::new("git")
        .args(["merge", &format!("origin/{}", detail.base_ref), "--no-edit"])
        .current_dir(work_dir)
        .output()
        .context("Failed to run git merge")?;

    if merge_output.status.success() {
        // Merge succeeded without conflicts — push.
        let push_status = Command::new("git")
            .args(["push", "origin", &detail.head_ref])
            .current_dir(work_dir)
            .status()
            .context("Failed to push merged branch")?;
        if !push_status.success() {
            anyhow::bail!("git push origin {} failed", detail.head_ref);
        }
        println!(
            "[wreck-it] merge: cleanly merged {} into {} and pushed",
            detail.base_ref, detail.head_ref,
        );
    } else {
        // Merge has conflicts.  Abort and report.
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(work_dir)
            .status();

        anyhow::bail!(
            "git merge of {} into {} produced conflicts that require manual resolution; \
             consider using backend = \"cloud_agent\" to assign a coding agent",
            detail.base_ref,
            detail.head_ref,
        );
    }

    Ok(())
}

/// Fetch recent commit messages on the base branch via the GitHub REST API.
async fn fetch_recent_base_commits_via_api(client: &CloudAgentClient, base_ref: &str) -> String {
    match client.fetch_recent_commits(base_ref, 10).await {
        Ok(commits) => commits,
        Err(e) => {
            println!(
                "[wreck-it] merge: could not fetch recent base commits: {}",
                e,
            );
            String::new()
        }
    }
}

/// Fetch a summary of the PR diff via the GitHub REST API.
async fn fetch_diff_summary_via_api(client: &CloudAgentClient, pr_number: u64) -> String {
    match client.fetch_pr_files_summary(pr_number).await {
        Ok(summary) => summary,
        Err(e) => {
            println!(
                "[wreck-it] merge: could not fetch diff summary for PR #{}: {}",
                pr_number, e,
            );
            String::new()
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

    #[test]
    fn build_merge_context_includes_all_sections() {
        let detail = PrDetail {
            number: 42,
            title: "Add feature X".to_string(),
            body: "This PR adds feature X.".to_string(),
            head_ref: "feature-x".to_string(),
            base_ref: "main".to_string(),
            has_conflicts: true,
        };
        let commits = "abc1234 fix: update config\ndef5678 feat: add new module";
        let diff = "src/main.rs | 10 +++++-----\nsrc/lib.rs  |  3 +++";

        let ctx = build_merge_context(&detail, commits, diff);

        assert!(ctx.contains("PR #42: Add feature X"));
        assert!(ctx.contains("This PR adds feature X."));
        assert!(ctx.contains("Head: `feature-x`"));
        assert!(ctx.contains("Base: `main`"));
        assert!(ctx.contains("abc1234 fix: update config"));
        assert!(ctx.contains("src/main.rs | 10"));
    }

    #[test]
    fn build_merge_context_omits_empty_sections() {
        let detail = PrDetail {
            number: 7,
            title: "Small fix".to_string(),
            body: String::new(),
            head_ref: "fix-typo".to_string(),
            base_ref: "main".to_string(),
            has_conflicts: true,
        };

        let ctx = build_merge_context(&detail, "", "");

        assert!(ctx.contains("PR #7: Small fix"));
        assert!(!ctx.contains("PR Description"));
        assert!(!ctx.contains("Recent commits"));
        assert!(!ctx.contains("Diff summary"));
    }

    #[test]
    fn backend_constants_are_valid() {
        assert_eq!(BACKEND_CLOUD_AGENT, "cloud_agent");
        assert_eq!(BACKEND_CLI, "cli");
    }

    #[test]
    fn state_pending_merge_issues_backward_compat() {
        // Existing state files without pending_merge_issues should still load.
        use crate::headless_state::load_headless_state;
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join(".merge-state.json");
        std::fs::write(&state_file, r#"{"phase":"needs_trigger","iteration":0}"#).unwrap();

        let loaded = load_headless_state(&state_file).unwrap();
        assert!(loaded.pending_merge_issues.is_empty());
        assert!(loaded.tracked_prs.is_empty());
    }

    #[test]
    fn state_pending_merge_issues_roundtrip() {
        use crate::headless_state::{load_headless_state, save_headless_state, HeadlessState};
        use wreck_it_core::state::PendingMergeIssue;
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join(".merge-state.json");
        let mut state = HeadlessState::default();
        state.pending_merge_issues.push(PendingMergeIssue {
            issue_number: 100,
            task_id: "merge-pr-55".to_string(),
        });
        state.tracked_prs.push(TrackedPr {
            pr_number: 200,
            task_id: "merge-pr-33".to_string(),
            issue_number: Some(88),
            review_requested: None,
            merge_method: None,
        });

        save_headless_state(&state_file, &state).unwrap();
        let loaded = load_headless_state(&state_file).unwrap();

        assert_eq!(loaded.pending_merge_issues.len(), 1);
        assert_eq!(loaded.pending_merge_issues[0].issue_number, 100);
        assert_eq!(loaded.pending_merge_issues[0].task_id, "merge-pr-55");
        assert_eq!(loaded.tracked_prs.len(), 1);
        assert_eq!(loaded.tracked_prs[0].pr_number, 200);
    }

    #[test]
    fn state_pending_merge_issues_omitted_when_empty() {
        // When pending_merge_issues is empty, it should not appear in JSON.
        use crate::headless_state::{save_headless_state, HeadlessState};
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join(".merge-state.json");
        let state = HeadlessState::default();

        save_headless_state(&state_file, &state).unwrap();
        let content = std::fs::read_to_string(&state_file).unwrap();
        assert!(!content.contains("pending_merge_issues"));
    }
}
