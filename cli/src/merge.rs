//! "Merge" command: scan open PRs for merge conflicts with the base branch
//! and resolve them.
//!
//! When `backend = "copilot_cli"` (the default), the command clones the
//! repository into a subdirectory, checks out the PR branch, merges the
//! base branch into it, invokes the Copilot CLI to resolve any conflicts,
//! commits the result, and pushes to the PR branch.
//!
//! When `backend = "cloud_agent"`, the command posts a `@copilot` comment
//! directly on the conflicting PR, asking the coding agent to reapply the
//! PR's changes on top of the latest base branch.
//!
//! When `backend = "cli"`, the command performs a local `git merge` of the
//! base branch into the PR branch and pushes the result directly.
//!
//! This can be used as a standalone CLI command (`wreck-it merge`) or as a
//! ralph command (`command = "merge"` in `[[ralphs]]`).

use crate::cloud_agent::{resolve_repo_info, CloudAgentClient, CloudAgentStatus};
use crate::headless;
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::headless_state::{load_headless_state, save_headless_state, TrackedPr};
use crate::repo_config::RalphConfig;
use crate::state_worktree::{commit_and_push_state, ensure_state_worktree};
use crate::types::Config;
use anyhow::{Context, Result};
use std::path::Path;
use wreck_it_core::state::PendingMergeIssue;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Supported backend values.
const BACKEND_COPILOT_CLI: &str = "copilot_cli";
const BACKEND_CLOUD_AGENT: &str = "cloud_agent";
const BACKEND_CLI: &str = "cli";

/// Run the merge logic: find open PRs with merge conflicts and resolve them.
///
/// `backend` selects how conflicts are resolved – `"copilot_cli"` (default)
/// clones the repo into a subdirectory, merges the base branch into the PR
/// branch, invokes the Copilot CLI to resolve conflicts, and pushes;
/// `"cloud_agent"` posts a `@copilot` comment on the PR asking the agent to
/// reapply changes; `"cli"` merges locally and pushes.
///
/// When `ralph` is provided, the merge command also loads/saves persistent
/// state so that PRs created by previous runs are tracked and advanced
/// through the merge pipeline (mark ready → merge).
pub async fn run_merge(
    config: &Config,
    backend: Option<&str>,
    ralph: Option<&RalphConfig>,
) -> Result<()> {
    let work_dir = &config.work_dir;
    let backend = backend.unwrap_or(BACKEND_COPILOT_CLI);

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required for merge command")?;

    let headless_cfg = load_headless_cfg(work_dir)?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    println!(
        "[wreck-it] merge: scanning {}/{} for PRs with merge conflicts (backend={})",
        repo_owner, repo_name, backend,
    );

    let mut client =
        CloudAgentClient::new(github_token.clone(), repo_owner.clone(), repo_name.clone());
    client.resolve_authenticated_login().await;

    // ── State loading ──────────────────────────────────────────────────
    let repo_cfg = crate::repo_config::load_repo_config(work_dir)
        .ok()
        .flatten();
    let state_branch = repo_cfg
        .as_ref()
        .map(|c| c.state_branch.clone())
        .unwrap_or_else(|| headless_cfg.state_branch.clone());

    let state_dir = ensure_state_worktree(work_dir, &state_branch).ok();

    // Determine the state file name from the ralph config (e.g.
    // `.merge-state.json`); fall back to the headless config default.
    let state_file_name = ralph
        .map(|r| r.state_file.clone())
        .unwrap_or_else(|| headless_cfg.state_file.to_string_lossy().into_owned());

    let mut state = state_dir
        .as_ref()
        .map(|d| load_headless_state(&d.join(&state_file_name)))
        .transpose()
        .context("Failed to load merge state")?
        .unwrap_or_default();

    // ── Conflict resolution ────────────────────────────────────────────
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

        // Guard: skip if we already have a pending issue or tracked PR for
        // this conflict resolution to avoid creating duplicate work.
        let task_id = format!("merge-pr-{}", pr.number);
        if has_existing_work_for_task(&state, &task_id) {
            println!(
                "[wreck-it] merge: PR #{} ({}) already has an outstanding issue/PR for \
                 conflict resolution ({}), skipping",
                pr.number, pr.title, task_id,
            );
            continue;
        }

        println!(
            "[wreck-it] merge: PR #{} ({}) has merge conflicts — resolving via {}",
            pr.number, pr.title, backend,
        );

        let result = match backend {
            BACKEND_COPILOT_CLI => {
                resolve_via_copilot_cli(
                    work_dir,
                    &pr_detail,
                    &github_token,
                    &repo_owner,
                    &repo_name,
                )
                .await
            }
            BACKEND_CLI => {
                resolve_via_cli(work_dir, &pr_detail, &github_token, &repo_owner, &repo_name).await
            }
            BACKEND_CLOUD_AGENT => resolve_via_cloud_agent(&client, &pr_detail, &mut state).await,
            other => {
                anyhow::bail!(
                    "unknown merge backend '{}'; expected one of: copilot_cli, cloud_agent, cli",
                    other,
                );
            }
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

    // ── Promote pending issues → tracked PRs ───────────────────────────
    promote_pending_merge_issues(&client, &mut state).await;

    // ── Advance tracked PRs (mark ready, merge, etc.) ──────────────────
    if let Some(ref sd) = state_dir {
        // Build a HeadlessConfig with the correct state_file for this ralph.
        let mut merge_headless_cfg = headless_cfg.clone();
        if let Some(rc) = ralph {
            merge_headless_cfg.task_file = rc.task_file.clone().into();
            merge_headless_cfg.state_file = state_file_name.clone().into();
        }

        if let Err(e) = headless::advance_tracked_prs(
            config,
            &merge_headless_cfg,
            ralph,
            &mut state,
            work_dir,
            sd,
        )
        .await
        {
            println!("[wreck-it] merge: failed to advance tracked PRs: {}", e);
        }
    }

    // ── Save state ─────────────────────────────────────────────────────
    if let Some(ref sd) = state_dir {
        let state_path = sd.join(&state_file_name);
        if let Err(e) = save_headless_state(&state_path, &state) {
            println!("[wreck-it] merge: failed to save state: {}", e);
        }
        if let Err(e) =
            commit_and_push_state(work_dir, &state_branch, "wreck-it: update merge state")
        {
            println!("[wreck-it] merge: failed to commit/push state: {}", e);
        }
    }

    Ok(())
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

/// Resolve merge conflicts by posting a `@copilot` comment on the PR.
///
/// Instead of creating a separate GitHub issue (which would start the
/// coding agent on its own branch, unable to perform a real `git merge`),
/// we comment directly on the conflicting PR.  The agent will work within
/// the PR context and can reapply the changes on the latest base branch.
///
/// The PR number is recorded in `state.pending_merge_issues` (with
/// `comment_only = true`) as a deduplication guard so that subsequent
/// invocations do not post redundant comments.
async fn resolve_via_cloud_agent(
    client: &CloudAgentClient,
    detail: &PrDetail,
    state: &mut crate::headless_state::HeadlessState,
) -> Result<()> {
    // Gather context: recent base commits and diff.
    let recent_base_commits = fetch_recent_base_commits_via_api(client, &detail.base_ref).await;
    let diff_summary = fetch_diff_summary_via_api(client, detail.number).await;

    let context = build_merge_context(detail, &recent_base_commits, &diff_summary);

    let comment = format!(
        "@copilot This PR has merge conflicts with `{}`. Please resolve \
         the conflicts by reapplying the changes in this PR on top of the \
         latest `{}` branch.\n\n\
         {}\n\n\
         ---\n\
         *Triggered by wreck-it merge ralph*",
        detail.base_ref, detail.base_ref, context,
    );

    client.comment_on_pr(detail.number, &comment).await?;

    let task_id = format!("merge-pr-{}", detail.number);

    println!(
        "[wreck-it] merge: posted @copilot comment on PR #{} for conflict resolution",
        detail.number,
    );

    // Record the comment so we don't re-post on the next invocation.
    if !state
        .pending_merge_issues
        .iter()
        .any(|p| p.task_id == task_id)
    {
        state.pending_merge_issues.push(PendingMergeIssue {
            issue_number: detail.number,
            task_id,
            comment_only: true,
        });
    }

    Ok(())
}

/// Resolve merge conflicts by cloning the repo into a subdirectory, merging
/// the base branch into the PR branch, invoking the Copilot CLI to resolve
/// any conflicts, committing the result, and pushing to the PR branch.
async fn resolve_via_copilot_cli(
    work_dir: &Path,
    detail: &PrDetail,
    github_token: &str,
    repo_owner: &str,
    repo_name: &str,
) -> Result<()> {
    use std::process::Command;

    let clone_dir = work_dir.join(format!(".merge-pr-{}", detail.number));

    // Clean up any leftover subdirectory from a previous attempt.
    if clone_dir.exists() {
        std::fs::remove_dir_all(&clone_dir).context("Failed to remove stale merge subdirectory")?;
    }

    // Clone the repository into the subdirectory.
    // Pass the token via a temporary git credential store file so it does not
    // appear in the clone URL (which can leak in process listings and logs).
    let clone_url = format!("https://github.com/{}/{}.git", repo_owner, repo_name,);

    // Write an ephemeral credential store that git can read.
    let cred_file = work_dir.join(format!(".merge-pr-{}-cred", detail.number));
    std::fs::write(
        &cred_file,
        format!("https://x-access-token:{}@github.com\n", github_token,),
    )
    .context("Failed to write temporary credential file")?;

    let cred_helper = format!(
        "credential.helper=store --file={}",
        cred_file.to_string_lossy(),
    );

    let clone_status = Command::new("git")
        .args([
            "-c",
            &cred_helper,
            "clone",
            "--depth=50",
            &clone_url,
            &clone_dir.to_string_lossy(),
        ])
        .status()
        .context("Failed to run `git clone`")?;
    if !clone_status.success() {
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("git clone failed");
    }

    // Persist the credential helper in the clone's local git config so that
    // subsequent fetch/push commands are authenticated.
    let set_config_status = Command::new("git")
        .args([
            "config",
            "--local",
            "credential.helper",
            &format!("store --file={}", cred_file.to_string_lossy()),
        ])
        .current_dir(&clone_dir)
        .status()
        .context("Failed to configure git credentials in clone")?;
    if !set_config_status.success() {
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("git config for credential helper failed");
    }

    // Fetch the PR branch explicitly — the shallow clone only fetches the
    // default branch (--depth implies --single-branch), so the PR's head ref
    // won't be available locally without this step.
    let fetch_head_status = Command::new("git")
        .args(["fetch", "origin", &detail.head_ref])
        .current_dir(&clone_dir)
        .status()
        .context("Failed to fetch PR head branch")?;
    if !fetch_head_status.success() {
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("git fetch origin {} failed", detail.head_ref);
    }

    // Check out the PR branch.
    let checkout_status = Command::new("git")
        .args(["checkout", &detail.head_ref])
        .current_dir(&clone_dir)
        .status()
        .context("Failed to checkout PR branch")?;
    if !checkout_status.success() {
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("git checkout {} failed", detail.head_ref);
    }

    // Fetch the base branch.
    let fetch_status = Command::new("git")
        .args(["fetch", "origin", &detail.base_ref])
        .current_dir(&clone_dir)
        .status()
        .context("Failed to fetch base branch")?;
    if !fetch_status.success() {
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("git fetch origin {} failed", detail.base_ref);
    }

    // Merge the base branch into the PR branch.
    let merge_output = Command::new("git")
        .args(["merge", &format!("origin/{}", detail.base_ref), "--no-edit"])
        .current_dir(&clone_dir)
        .output()
        .context("Failed to run git merge")?;

    if merge_output.status.success() {
        // Merge succeeded without conflicts — push and clean up.
        let push_status = Command::new("git")
            .args(["push", "origin", &detail.head_ref])
            .current_dir(&clone_dir)
            .status()
            .context("Failed to push merged branch")?;
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        if !push_status.success() {
            anyhow::bail!("git push origin {} failed", detail.head_ref);
        }
        println!(
            "[wreck-it] merge: cleanly merged {} into {} and pushed",
            detail.base_ref, detail.head_ref,
        );
        return Ok(());
    }

    // Merge produced conflicts — invoke the Copilot CLI to resolve them.
    println!("[wreck-it] merge: merge conflicts detected, invoking Copilot CLI to resolve…",);

    let cli_path = crate::agent::resolve_copilot_cli_path().context(
        "Could not find the 'copilot' binary on PATH. \
         Install GitHub Copilot CLI (https://gh.io/copilot-install) \
         or ensure it is available in your shell environment.",
    )?;

    let conflict_prompt = format!(
        "There are merge conflicts in this repository after merging `origin/{}` into `{}`. \
         Please resolve all merge conflicts in every file. For each conflicted file, \
         pick the correct resolution that preserves the intent of both branches. \
         After resolving, stage all changes with `git add .`.",
        detail.base_ref, detail.head_ref,
    );

    use copilot_sdk_supercharged::*;

    let config = SessionConfig {
        request_permission: Some(false),
        request_user_input: Some(false),
        working_directory: Some(clone_dir.to_string_lossy().to_string()),
        ..Default::default()
    };

    let result = crate::agent::copilot_oneshot(
        cli_path,
        config,
        conflict_prompt,
        300_000, // 5 minute timeout for conflict resolution
        "",
    )
    .await;

    if let Err(e) = result {
        // Abort the merge and clean up on failure.
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&clone_dir)
            .status();
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("Copilot CLI conflict resolution failed: {}", e);
    }

    // Check if all conflicts have been resolved (no conflict markers remain).
    let diff_check = Command::new("git")
        .args(["diff", "--check"])
        .current_dir(&clone_dir)
        .output()
        .context("Failed to run git diff --check")?;

    if !diff_check.status.success() {
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(&clone_dir)
            .status();
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!(
            "Copilot CLI did not fully resolve all merge conflicts between {} and {}",
            detail.base_ref,
            detail.head_ref,
        );
    }

    // Commit the merge resolution.
    let commit_status = Command::new("git")
        .args([
            "commit",
            "--no-edit",
            "-m",
            &format!(
                "Merge {} into {} (resolved by Copilot CLI)",
                detail.base_ref, detail.head_ref,
            ),
        ])
        .current_dir(&clone_dir)
        .status()
        .context("Failed to commit merge resolution")?;
    if !commit_status.success() {
        let _ = std::fs::remove_dir_all(&clone_dir);
        let _ = std::fs::remove_file(&cred_file);
        anyhow::bail!("git commit for merge resolution failed");
    }

    // Push to the PR branch.
    let push_status = Command::new("git")
        .args(["push", "origin", &detail.head_ref])
        .current_dir(&clone_dir)
        .status()
        .context("Failed to push resolved merge")?;

    let _ = std::fs::remove_dir_all(&clone_dir);
    let _ = std::fs::remove_file(&cred_file);

    if !push_status.success() {
        anyhow::bail!("git push origin {} failed", detail.head_ref);
    }

    println!(
        "[wreck-it] merge: resolved conflicts between {} and {} via Copilot CLI and pushed",
        detail.base_ref, detail.head_ref,
    );

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

/// Poll pending merge entries.
///
/// For **issue-based** entries (`comment_only = false`), poll the coding
/// agent status and promote to tracked PRs when a pull request is created.
///
/// For **comment-based** entries (`comment_only = true`), check whether the
/// PR still has merge conflicts.  Once the conflicts are resolved (or the
/// PR is closed), the entry is removed so that a fresh comment can be
/// posted if new conflicts appear in the future.
async fn promote_pending_merge_issues(
    client: &CloudAgentClient,
    state: &mut crate::headless_state::HeadlessState,
) {
    if state.pending_merge_issues.is_empty() {
        return;
    }

    println!(
        "[wreck-it] merge: checking {} pending merge entries",
        state.pending_merge_issues.len(),
    );

    let mut promoted: Vec<u64> = Vec::new();

    for pending in &state.pending_merge_issues {
        // ── Comment-only entries: check PR conflict status ──────────
        if pending.comment_only {
            match client.fetch_pr_json(pending.issue_number).await {
                Ok(pr_json) => {
                    let is_closed = pr_json["state"].as_str() == Some("closed");
                    // When `mergeable` is null (GitHub still computing),
                    // treat conservatively as "not yet mergeable" so we
                    // keep the deduplication guard in place.
                    let mergeable = pr_json["mergeable"].as_bool().unwrap_or(false);
                    let mergeable_state = pr_json["mergeable_state"].as_str().unwrap_or("unknown");
                    let still_conflicting = !mergeable || mergeable_state == "dirty";

                    if is_closed {
                        println!(
                            "[wreck-it] merge: PR #{} is closed — removing comment guard",
                            pending.issue_number,
                        );
                        promoted.push(pending.issue_number);
                    } else if !still_conflicting {
                        println!(
                            "[wreck-it] merge: PR #{} conflicts resolved — removing comment guard",
                            pending.issue_number,
                        );
                        promoted.push(pending.issue_number);
                    } else {
                        println!(
                            "[wreck-it] merge: PR #{} still has conflicts — keeping comment guard",
                            pending.issue_number,
                        );
                    }
                }
                Err(e) => {
                    println!(
                        "[wreck-it] merge: failed to check PR #{} status: {}",
                        pending.issue_number, e,
                    );
                }
            }
            continue;
        }

        // ── Issue-based entries: poll agent status ──────────────────
        match client.check_agent_status(pending.issue_number).await {
            Ok(CloudAgentStatus::PrCreated { pr_number, .. }) => {
                if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_number) {
                    println!(
                        "[wreck-it] merge: issue #{} produced PR #{} — now tracking",
                        pending.issue_number, pr_number,
                    );
                    state.tracked_prs.push(TrackedPr {
                        pr_number,
                        task_id: pending.task_id.clone(),
                        issue_number: Some(pending.issue_number),
                        review_requested: None,
                    });
                }
                promoted.push(pending.issue_number);
            }
            Ok(CloudAgentStatus::PrCreatedAgentWorking { pr_number, .. }) => {
                // PR exists but agent is still working — track it now so
                // that advance_tracked_prs can see the issue_number and
                // defer marking-ready until the agent finishes.
                if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_number) {
                    println!(
                        "[wreck-it] merge: issue #{} produced PR #{} (agent still working) — now tracking",
                        pending.issue_number, pr_number,
                    );
                    state.tracked_prs.push(TrackedPr {
                        pr_number,
                        task_id: pending.task_id.clone(),
                        issue_number: Some(pending.issue_number),
                        review_requested: None,
                    });
                }
                promoted.push(pending.issue_number);
            }
            Ok(CloudAgentStatus::CompletedNoPr) => {
                println!(
                    "[wreck-it] merge: issue #{} completed without a PR — removing",
                    pending.issue_number,
                );
                promoted.push(pending.issue_number);
            }
            Ok(CloudAgentStatus::Working) => {
                println!(
                    "[wreck-it] merge: issue #{} — agent still working",
                    pending.issue_number,
                );
            }
            Err(e) => {
                println!(
                    "[wreck-it] merge: failed to check status of issue #{}: {}",
                    pending.issue_number, e,
                );
            }
        }
    }

    // Remove promoted/completed entries from the pending list.
    state
        .pending_merge_issues
        .retain(|p| !promoted.contains(&p.issue_number));
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

/// Check whether we already have a pending merge issue or tracked PR for the
/// given `task_id`.  This prevents creating duplicate issues when a conflict
/// resolution is already in progress.
fn has_existing_work_for_task(state: &crate::headless_state::HeadlessState, task_id: &str) -> bool {
    state
        .pending_merge_issues
        .iter()
        .any(|p| p.task_id == task_id)
        || state.tracked_prs.iter().any(|tp| tp.task_id == task_id)
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
        assert_eq!(BACKEND_COPILOT_CLI, "copilot_cli");
        assert_eq!(BACKEND_CLOUD_AGENT, "cloud_agent");
        assert_eq!(BACKEND_CLI, "cli");
    }

    #[test]
    fn pending_merge_issue_serde_roundtrip() {
        let issue = PendingMergeIssue {
            issue_number: 99,
            task_id: "merge-pr-42".to_string(),
            comment_only: false,
        };
        let json = serde_json::to_string(&issue).unwrap();
        let loaded: PendingMergeIssue = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.issue_number, 99);
        assert_eq!(loaded.task_id, "merge-pr-42");
        assert!(!loaded.comment_only);
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
        let dir = tempfile::tempdir().unwrap();
        let state_file = dir.path().join(".merge-state.json");
        let mut state = HeadlessState::default();
        state.pending_merge_issues.push(PendingMergeIssue {
            issue_number: 100,
            task_id: "merge-pr-55".to_string(),
            comment_only: false,
        });
        state.tracked_prs.push(TrackedPr {
            pr_number: 200,
            task_id: "merge-pr-33".to_string(),
            issue_number: Some(88),
            review_requested: None,
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

    #[test]
    fn has_existing_work_returns_false_for_empty_state() {
        let state = crate::headless_state::HeadlessState::default();
        assert!(!has_existing_work_for_task(&state, "merge-pr-42"));
    }

    #[test]
    fn has_existing_work_detects_pending_merge_issue() {
        let mut state = crate::headless_state::HeadlessState::default();
        state.pending_merge_issues.push(PendingMergeIssue {
            issue_number: 100,
            task_id: "merge-pr-42".to_string(),
            comment_only: false,
        });
        assert!(has_existing_work_for_task(&state, "merge-pr-42"));
        assert!(!has_existing_work_for_task(&state, "merge-pr-99"));
    }

    #[test]
    fn has_existing_work_detects_tracked_pr() {
        let mut state = crate::headless_state::HeadlessState::default();
        state.tracked_prs.push(TrackedPr {
            pr_number: 200,
            task_id: "merge-pr-55".to_string(),
            issue_number: Some(100),
            review_requested: None,
        });
        assert!(has_existing_work_for_task(&state, "merge-pr-55"));
        assert!(!has_existing_work_for_task(&state, "merge-pr-42"));
    }

    #[test]
    fn has_existing_work_detects_both_pending_and_tracked() {
        let mut state = crate::headless_state::HeadlessState::default();
        state.pending_merge_issues.push(PendingMergeIssue {
            issue_number: 100,
            task_id: "merge-pr-42".to_string(),
            comment_only: false,
        });
        state.tracked_prs.push(TrackedPr {
            pr_number: 200,
            task_id: "merge-pr-55".to_string(),
            issue_number: Some(101),
            review_requested: None,
        });
        assert!(has_existing_work_for_task(&state, "merge-pr-42"));
        assert!(has_existing_work_for_task(&state, "merge-pr-55"));
        assert!(!has_existing_work_for_task(&state, "merge-pr-99"));
    }

    #[test]
    fn has_existing_work_detects_comment_only_entry() {
        let mut state = crate::headless_state::HeadlessState::default();
        state.pending_merge_issues.push(PendingMergeIssue {
            issue_number: 42,
            task_id: "merge-pr-42".to_string(),
            comment_only: true,
        });
        assert!(has_existing_work_for_task(&state, "merge-pr-42"));
        assert!(!has_existing_work_for_task(&state, "merge-pr-99"));
    }

    #[test]
    fn comment_only_serde_roundtrip() {
        let issue = PendingMergeIssue {
            issue_number: 42,
            task_id: "merge-pr-42".to_string(),
            comment_only: true,
        };
        let json = serde_json::to_string(&issue).unwrap();
        assert!(json.contains("comment_only"));
        let loaded: PendingMergeIssue = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.issue_number, 42);
        assert_eq!(loaded.task_id, "merge-pr-42");
        assert!(loaded.comment_only);
    }

    #[test]
    fn comment_only_defaults_to_false_on_deserialize() {
        // Legacy entries without comment_only should deserialize with false.
        let json = r#"{"issue_number":99,"task_id":"merge-pr-99"}"#;
        let loaded: PendingMergeIssue = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.issue_number, 99);
        assert!(!loaded.comment_only);
    }

    #[test]
    fn comment_only_false_omitted_from_json() {
        // When comment_only is false, it should be omitted from JSON output.
        let issue = PendingMergeIssue {
            issue_number: 99,
            task_id: "merge-pr-99".to_string(),
            comment_only: false,
        };
        let json = serde_json::to_string(&issue).unwrap();
        assert!(!json.contains("comment_only"));
    }
}
