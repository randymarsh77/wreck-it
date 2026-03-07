use crate::cloud_agent::{resolve_repo_info, CloudAgentClient, CloudAgentStatus, PrMergeStatus};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::headless_state::{
    load_headless_state, save_headless_state, AgentPhase, HeadlessState, TrackedPr,
};
use crate::plan_migration::migrate_pending_plans;
use crate::repo_config::{load_repo_config, RalphConfig};
use crate::state_worktree::{commit_and_push_state, ensure_feature_branch, ensure_state_worktree};
use crate::task_manager::{load_tasks, reset_recurring_tasks, save_tasks};
use crate::types::Config;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Maximum number of synchronous steps executed in a single invocation before
/// forcing a yield.  This prevents infinite loops when the state machine
/// repeatedly transitions through synchronous phases.
const MAX_SYNC_STEPS: usize = 20;

/// Maximum number of progress rounds in the outer loop.  Each round consists
/// of advancing tracked PRs and running the inner state-machine loop.  When a
/// round makes progress (any item transitions state) the loop re-runs
/// immediately instead of waiting for the next cron / event trigger.  This
/// cap prevents runaway loops.
const MAX_PROGRESS_ROUNDS: usize = 10;

/// Sentinel value returned by [`infer_task_id_from_title`] when no task ID
/// can be determined from the PR title.
const UNKNOWN_TASK_ID: &str = "unknown";

/// Known task-ID prefixes used by the wreck-it agent swarm.
#[cfg(test)]
const TASK_ID_PREFIXES: &[&str] = &["ideas-", "impl-", "eval-"];

/// Outcome of a single headless phase step.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StepOutcome {
    /// The step was synchronous/instant; the loop may continue immediately.
    Continue,
    /// An async/external operation was performed or there is nothing more to
    /// do; save state and return.
    Yield,
}

/// Run wreck-it in headless mode.
///
/// This is designed for CI environments (e.g. a cron-triggered GitHub Actions
/// workflow).  Instead of running a local AI chat loop, each invocation drives
/// the cloud-agent state machine forward:
///
///   NeedsTrigger → create a GitHub issue and assign Copilot (triggers the
///                  cloud coding agent)
///   AgentWorking → poll the issue for a linked PR created by the agent
///   NeedsVerification → check PR mergeability and merge it
///   Completed → mark the task done, advance to the next one
///
/// Synchronous/instant phase transitions (e.g. Completed → NeedsTrigger) are
/// executed in a tight loop so that a single invocation can chain through
/// multiple steps without sleeping.  The loop yields once an async/external
/// operation is performed (e.g. triggering an agent, polling for a PR) or when
/// there is nothing left to do.
///
/// State is persisted between invocations so subsequent cron runs pick up
/// where the previous one left off.
pub async fn run_headless(config: Config, ralph: Option<&RalphConfig>) -> Result<()> {
    // `work_dir` is the main checkout – agents work here.
    let work_dir = config.work_dir.clone();

    // Try to load the repo-level config (.wreck-it/config.toml on the main
    // branch).  When available it is the canonical source for the state branch
    // name.  Fall back to the bootstrap headless config or defaults.
    let repo_cfg = load_repo_config(&work_dir).ok().flatten();

    // Set up the state worktree.  First, try loading a headless config from
    // the main checkout (it may have been placed there before migration, or
    // the user may prefer it there for bootstrapping).  Then set up the
    // worktree and prefer the config from there if it exists.
    let bootstrap_cfg_path = work_dir.join(DEFAULT_CONFIG_FILE);
    let bootstrap_cfg = if bootstrap_cfg_path.exists() {
        load_headless_config(&bootstrap_cfg_path)
            .context("Failed to load .wreck-it.toml from work dir")?
    } else {
        HeadlessConfig::default()
    };

    let state_branch = repo_cfg
        .as_ref()
        .map(|c| c.state_branch.clone())
        .unwrap_or_else(|| bootstrap_cfg.state_branch.clone());
    let state_dir = ensure_state_worktree(&work_dir, &state_branch)
        .context("Failed to set up state worktree")?;

    // Prefer the config from the state worktree when available.
    let headless_cfg_path = state_dir.join(DEFAULT_CONFIG_FILE);
    let mut headless_cfg = if headless_cfg_path.exists() {
        load_headless_config(&headless_cfg_path)
            .context("Failed to load .wreck-it.toml from state worktree")?
    } else {
        bootstrap_cfg
    };

    // When a named ralph was requested, override the task and state file
    // paths from the ralph config so each loop operates independently.
    if let Some(rc) = ralph {
        headless_cfg.task_file = rc.task_file.clone().into();
        headless_cfg.state_file = rc.state_file.clone().into();
        println!(
            "[wreck-it] using ralph '{}' (tasks={}, state={})",
            rc.name, rc.task_file, rc.state_file
        );
    }

    let ralph_name = ralph.map(|r| r.name.as_str()).unwrap_or("default");
    let ralph_branch = ralph.and_then(|r| r.branch.as_deref());

    let state_path = state_dir.join(&headless_cfg.state_file);
    let mut state = load_headless_state(&state_path).context("Failed to load headless state")?;

    // Migrate pending plans from the main branch into the state branch.
    // Cloud agents write new/revised task plans as JSON files in
    // `.wreck-it/plans/` on the main branch; this step merges them into the
    // active task list on the state branch.
    let config_dir = work_dir.join(wreck_it_core::config::CONFIG_DIR);
    let task_file_for_migration = state_dir.join(&headless_cfg.task_file);
    match migrate_pending_plans(&config_dir, &state_dir, &task_file_for_migration) {
        Ok(0) => {}
        Ok(n) => {
            println!("[wreck-it] migrated {} task(s) from pending plans", n);
        }
        Err(e) => {
            println!("[wreck-it] warning: plan migration failed: {}", e);
        }
    }

    // Outer progress loop: re-run advance + state-machine as long as any item
    // transitions state.  This lets a single invocation chain through multiple
    // synchronous progressions (e.g. mark-ready → merge → complete) without
    // waiting for the next cron trigger.
    let mut progress_rounds: usize = 0;

    loop {
        let mut made_progress = false;

        // Advance tracked PRs before entering the main state machine.  Only PRs
        // that are already tracked in state (created by wreck-it) are processed.
        match advance_tracked_prs(
            &config,
            &headless_cfg,
            ralph,
            &mut state,
            &work_dir,
            &state_dir,
        )
        .await
        {
            Ok(progressed) => {
                if progressed {
                    made_progress = true;
                }
            }
            Err(e) => {
                println!("[wreck-it] warning: advance_tracked_prs failed: {}", e);
            }
        }

        let mut sync_steps: usize = 0;

        loop {
            println!(
                "[wreck-it] headless iteration {} | phase: {:?}",
                state.iteration, state.phase
            );

            let outcome = match state.phase {
                AgentPhase::NeedsTrigger => {
                    run_needs_trigger(
                        &config,
                        &headless_cfg,
                        ralph_name,
                        ralph_branch,
                        ralph,
                        &mut state,
                        &work_dir,
                        &state_dir,
                    )
                    .await?
                }
                AgentPhase::AgentWorking => {
                    run_agent_working(&config, &headless_cfg, &mut state, &work_dir).await?
                }
                AgentPhase::NeedsVerification => {
                    run_needs_verification(
                        &config,
                        &headless_cfg,
                        ralph,
                        &mut state,
                        &work_dir,
                        &state_dir,
                    )
                    .await?
                }
                AgentPhase::AwaitingReview => {
                    run_awaiting_review(
                        &config,
                        &headless_cfg,
                        ralph,
                        &mut state,
                        &work_dir,
                        &state_dir,
                    )
                    .await?
                }
                AgentPhase::Completed => {
                    println!("[wreck-it] previous task completed, advancing to next trigger");
                    state.phase = AgentPhase::NeedsTrigger;
                    state.current_task_id = None;
                    state.issue_number = None;
                    state.pr_number = None;
                    state.pr_url = None;
                    state.last_prompt = None;
                    state.review_requested = None;
                    StepOutcome::Continue
                }
            };

            if outcome == StepOutcome::Yield {
                break;
            }

            sync_steps += 1;
            if sync_steps >= MAX_SYNC_STEPS {
                println!(
                    "[wreck-it] reached max synchronous steps ({}), yielding",
                    MAX_SYNC_STEPS
                );
                break;
            }
        }

        made_progress |= sync_steps > 0;

        if !made_progress {
            break;
        }

        progress_rounds += 1;
        if progress_rounds >= MAX_PROGRESS_ROUNDS {
            println!(
                "[wreck-it] reached max progress rounds ({}), yielding",
                MAX_PROGRESS_ROUNDS
            );
            break;
        }

        println!(
            "[wreck-it] progress detected, re-running advance loop (round {})",
            progress_rounds
        );
    }

    save_headless_state(&state_path, &state).context("Failed to save headless state")?;

    // Commit state changes in the worktree and push if anything was committed.
    if let Err(e) =
        commit_and_push_state(&work_dir, &state_branch, "wreck-it: update headless state")
    {
        println!("[wreck-it] warning: failed to commit state changes: {}", e);
    }

    println!(
        "[wreck-it] saved state: phase={:?} iteration={}",
        state.phase, state.iteration
    );

    Ok(())
}

/// Log issue assignee details for diagnostics.
///
/// Fetches the assignees of `issue_number` and prints agent vs non-agent
/// breakdowns.  Emits a hint when only agent(s) are assigned (agent may
/// still be working).
async fn log_issue_assignees(client: &CloudAgentClient, issue_number: u64, prefix: &str) {
    match client.get_issue_assignee_summary(issue_number).await {
        Ok((agents, others)) => {
            println!(
                "{} issue #{} assignees: agents={:?}, others={:?}",
                prefix, issue_number, agents, others,
            );
            if !agents.is_empty() && others.is_empty() {
                println!(
                    "{} only agent(s) assigned to issue #{} — agent may still be working",
                    prefix, issue_number,
                );
            }
        }
        Err(e) => {
            println!(
                "{} failed to fetch assignees for issue #{}: {}",
                prefix, issue_number, e,
            );
        }
    }
}

/// Advance all tracked PRs in the state.
///
/// Only PRs that are already recorded in `state.tracked_prs` (i.e. PRs
/// created by wreck-it for known tasks) are processed.  This does **not**
/// discover or adopt untracked PRs from the repository.
///
/// For each tracked PR the progression is:
/// - Draft → mark it ready for review.
/// - Ready with required status checks → approve pending workflow runs and
///   enable auto-merge so GitHub merges automatically once checks pass.
/// - Ready without required checks → merge directly.
/// - Already merged → mark the associated task complete.
///
/// When a merged PR corresponds to the currently tracked task in `state`, the
/// headless state is advanced to [`AgentPhase::Completed`] so the next
/// invocation can pick up a new task.
///
/// Returns `true` when at least one tracked PR transitioned state (e.g. was
/// marked ready, merged, or resolved), signalling that another progress round
/// may be worthwhile.
async fn advance_tracked_prs(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    ralph: Option<&RalphConfig>,
    state: &mut HeadlessState,
    work_dir: &Path,
    state_dir: &Path,
) -> Result<bool> {
    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required to advance tracked PRs")?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    let mut client = CloudAgentClient::new(github_token, repo_owner, repo_name);
    client.resolve_authenticated_login().await;

    // Ensure the current task's PR is tracked.
    if let (Some(pr_num), Some(task_id)) = (state.pr_number, state.current_task_id.clone()) {
        if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_num) {
            state.tracked_prs.push(TrackedPr {
                pr_number: pr_num,
                task_id,
                issue_number: state.issue_number,
                review_requested: None,
            });
        }
    }

    if state.tracked_prs.is_empty() {
        println!("[wreck-it] advance: no tracked PRs");
        return Ok(false);
    }

    println!(
        "[wreck-it] advance: processing {} tracked PR(s)",
        state.tracked_prs.len(),
    );

    let mut made_progress = false;
    let task_file = state_dir.join(&headless_cfg.task_file);

    // Process each tracked PR.  Collect PR numbers that have been merged or
    // closed (to remove from the tracked list after the loop).
    let mut resolved_pr_numbers: Vec<u64> = Vec::new();

    // Iterate over a snapshot of tracked_prs to avoid borrow issues.
    let tracked_snapshot: Vec<TrackedPr> = state.tracked_prs.clone();

    for tracked in &tracked_snapshot {
        let pr_number = tracked.pr_number;

        println!(
            "[wreck-it] advance: PR #{} (task {})",
            pr_number, tracked.task_id
        );

        match client.check_pr_merge_status(pr_number).await {
            Ok(PrMergeStatus::Draft) => {
                // If we know the triggering issue, check whether the coding
                // agent is still assigned — a draft PR while the agent is
                // assigned means it is still pushing changes and should not
                // be marked ready yet.
                if let Some(issue_num) = tracked.issue_number {
                    match client.is_agent_assigned_to_issue(issue_num).await {
                        Ok(true) => {
                            println!(
                                "[wreck-it] advance: PR #{} is a draft and agent is still \
                                 assigned to issue #{}; skipping",
                                pr_number, issue_num,
                            );
                            continue;
                        }
                        Ok(false) => { /* agent finished; mark ready below */ }
                        Err(e) => {
                            println!(
                                "[wreck-it] advance: failed to check agent assignment for \
                                 issue #{}: {}; proceeding to mark ready",
                                issue_num, e,
                            );
                        }
                    }
                }
                println!(
                    "[wreck-it] advance: PR #{} is a draft, marking ready for review",
                    pr_number
                );
                if let Err(e) = client.mark_pr_ready_for_review(pr_number).await {
                    println!(
                        "[wreck-it] advance: failed to mark PR #{} ready: {}",
                        pr_number, e
                    );
                } else {
                    made_progress = true;
                    // Request reviews if configured and not yet requested.
                    if let Some(reviewers) = ralph.and_then(|r| r.reviewers.as_ref()) {
                        if !reviewers.is_empty() && tracked.review_requested != Some(true) {
                            if let Err(e) = client.request_reviewers(pr_number, reviewers).await {
                                println!(
                                    "[wreck-it] advance: failed to request reviews on PR #{}: {}",
                                    pr_number, e,
                                );
                            } else {
                                println!(
                                    "[wreck-it] advance: requested reviews on PR #{} from {:?}",
                                    pr_number, reviewers,
                                );
                                // Mark review_requested on the tracked PR.
                                if let Some(tp) = state
                                    .tracked_prs
                                    .iter_mut()
                                    .find(|tp| tp.pr_number == pr_number)
                                {
                                    tp.review_requested = Some(true);
                                }
                                if state.pr_number == Some(pr_number) {
                                    state.review_requested = Some(true);
                                    state.phase = AgentPhase::AwaitingReview;
                                }
                            }
                        }
                    }
                }
                state.memory.push(format!(
                    "advance: marked PR #{} (task {}) as ready for review",
                    pr_number, tracked.task_id,
                ));
            }
            Ok(PrMergeStatus::AgentWorkInProgress) => {
                println!(
                    "[wreck-it] advance: PR #{} — agent is still working, skipping",
                    pr_number,
                );
                if let Some(issue_num) = tracked.issue_number {
                    log_issue_assignees(&client, issue_num, "[wreck-it] advance:").await;
                }
            }
            Ok(PrMergeStatus::NotMergeable) => {
                // Check whether the base branch requires status checks.
                let has_checks = match client.has_required_checks_for_pr(pr_number).await {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "[wreck-it] advance: failed to check required checks for PR #{}: {}",
                            pr_number, e
                        );
                        false
                    }
                };
                if has_checks {
                    // Check if any checks are actively failing.
                    let has_failures = match client.has_failing_checks_for_pr(pr_number).await {
                        Ok(v) => v,
                        Err(e) => {
                            println!(
                                "[wreck-it] advance: failed to check failing checks for PR #{}: {}",
                                pr_number, e
                            );
                            false
                        }
                    };
                    if has_failures {
                        println!(
                            "[wreck-it] advance: PR #{} has failing checks, requesting @copilot fix",
                            pr_number
                        );
                        if let Err(e) = client
                            .comment_on_pr(
                                pr_number,
                                "@copilot The CI checks on this PR are failing. Please fix the failing checks.",
                            )
                            .await
                        {
                            println!(
                                "[wreck-it] advance: failed to comment on PR #{}: {}",
                                pr_number, e
                            );
                        }
                    } else {
                        println!(
                            "[wreck-it] advance: PR #{} not yet mergeable, approving workflows and enabling auto-merge",
                            pr_number
                        );
                        if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                            println!(
                                "[wreck-it] advance: failed to approve workflows for PR #{}: {}",
                                pr_number, e
                            );
                        }
                        if let Err(e) = client.enable_auto_merge(pr_number).await {
                            println!(
                                "[wreck-it] advance: failed to enable auto-merge for PR #{}: {}",
                                pr_number, e
                            );
                        }
                    }
                } else {
                    println!(
                        "[wreck-it] advance: PR #{} not yet mergeable (no required checks), will retry",
                        pr_number
                    );
                }
                // Log issue assignee details for diagnostics.
                if let Some(issue_num) = tracked.issue_number {
                    log_issue_assignees(&client, issue_num, "[wreck-it] advance:").await;
                }
            }
            Ok(PrMergeStatus::Mergeable) => {
                // If reviewers are configured and not yet requested, request them
                // and skip merge for now.
                if let Some(reviewers) = ralph.and_then(|r| r.reviewers.as_ref()) {
                    if !reviewers.is_empty() && tracked.review_requested != Some(true) {
                        if let Err(e) = client.request_reviewers(pr_number, reviewers).await {
                            println!(
                                "[wreck-it] advance: failed to request reviews on PR #{}: {}",
                                pr_number, e,
                            );
                        } else {
                            println!(
                                "[wreck-it] advance: requested reviews on PR #{} from {:?}",
                                pr_number, reviewers,
                            );
                            if let Some(tp) = state
                                .tracked_prs
                                .iter_mut()
                                .find(|tp| tp.pr_number == pr_number)
                            {
                                tp.review_requested = Some(true);
                            }
                            if state.pr_number == Some(pr_number) {
                                state.review_requested = Some(true);
                                state.phase = AgentPhase::AwaitingReview;
                            }
                            made_progress = true;
                        }
                        continue;
                    }
                }
                // Check whether the base branch requires status checks.
                let has_checks = match client.has_required_checks_for_pr(pr_number).await {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "[wreck-it] advance: failed to check required checks for PR #{}: {}",
                            pr_number, e
                        );
                        false
                    }
                };
                if has_checks {
                    // Required checks exist and pass; enable auto-merge and
                    // approve workflows in case any are still pending.
                    println!(
                        "[wreck-it] advance: PR #{} is mergeable with required checks, enabling auto-merge",
                        pr_number
                    );
                    if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                        println!(
                            "[wreck-it] advance: failed to approve workflows for PR #{}: {}",
                            pr_number, e
                        );
                    }
                    if let Err(e) = client.enable_auto_merge(pr_number).await {
                        println!(
                            "[wreck-it] advance: failed to enable auto-merge for PR #{}: {}",
                            pr_number, e
                        );
                    }
                } else {
                    // No required checks detected.  As a safety net, check
                    // for pending (queued/in-progress) check runs before
                    // merging directly.
                    let has_pending = match client.has_pending_checks_for_pr(pr_number).await {
                        Ok(v) => v,
                        Err(e) => {
                            println!(
                                "[wreck-it] advance: failed to check pending checks for PR #{}: {}",
                                pr_number, e
                            );
                            false
                        }
                    };
                    if has_pending {
                        println!(
                            "[wreck-it] advance: PR #{} has pending check runs, enabling auto-merge instead of merging directly",
                            pr_number
                        );
                        if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                            println!(
                                "[wreck-it] advance: failed to approve workflows for PR #{}: {}",
                                pr_number, e
                            );
                        }
                        if let Err(e) = client.enable_auto_merge(pr_number).await {
                            println!(
                                "[wreck-it] advance: failed to enable auto-merge for PR #{}: {}",
                                pr_number, e
                            );
                        }
                    } else {
                        // No required checks and no pending check runs — merge directly.
                        println!(
                            "[wreck-it] advance: PR #{} is mergeable, merging directly",
                            pr_number
                        );
                        match client.merge_pr(pr_number).await {
                            Ok(()) => {
                                println!("[wreck-it] advance: merged PR #{}", pr_number);
                                state
                                    .memory
                                    .push(format!("advance: merged PR #{}", pr_number));
                                mark_task_complete_by_id(&tracked.task_id, &task_file)?;
                                if state.pr_number == Some(pr_number) {
                                    state.phase = AgentPhase::Completed;
                                }
                                resolved_pr_numbers.push(pr_number);
                                made_progress = true;
                            }
                            Err(e) => {
                                println!(
                                    "[wreck-it] advance: failed to merge PR #{}: {}",
                                    pr_number, e
                                );
                            }
                        }
                    }
                }
            }
            Ok(PrMergeStatus::AlreadyMerged) => {
                println!(
                    "[wreck-it] advance: PR #{} (task {}) already merged",
                    pr_number, tracked.task_id
                );
                mark_task_complete_by_id(&tracked.task_id, &task_file)?;
                if state.pr_number == Some(pr_number) {
                    state.phase = AgentPhase::Completed;
                }
                resolved_pr_numbers.push(pr_number);
                made_progress = true;
            }
            Err(e) => {
                println!(
                    "[wreck-it] advance: error checking PR #{}: {}",
                    pr_number, e
                );
            }
        }
    }

    // Remove merged/closed PRs from the tracked list.
    let resolved_set: HashSet<u64> = resolved_pr_numbers.into_iter().collect();
    state
        .tracked_prs
        .retain(|tp| !resolved_set.contains(&tp.pr_number));

    Ok(made_progress)
}

/// Try to infer a task ID from a PR title.
///
/// Agent-created PRs often reference the triggering issue whose title is
/// `[wreck-it] <ralph_name> <task_id>` (current format) or the legacy
/// `[wreck-it] <task_id>` (without a ralph name).  PR titles may contain
/// the issue number (e.g. "Fixes #42") or quote the task ID directly.
/// This is a best-effort extraction; returns [`UNKNOWN_TASK_ID`] when no
/// ID can be inferred.
#[cfg(test)]
fn infer_task_id_from_title(title: &str) -> String {
    // Look for "[wreck-it] <ralph_name> <task_id>" or "[wreck-it] <task_id>" pattern.
    if let Some(rest) = title.strip_prefix("[wreck-it] ") {
        let trimmed = rest.trim();
        // Task IDs are single-token identifiers (e.g. "impl-3", "1"),
        // so the task ID is the last space-separated token.  This handles
        // both the current "[wreck-it] my-ralph task-1" and the legacy
        // "[wreck-it] task-1" formats.
        return trimmed
            .rsplit_once(' ')
            .map(|(_, id)| id)
            .unwrap_or(trimmed)
            .to_string();
    }
    // Check if the title itself is a known wreck-it task ID pattern.
    let trimmed = title.trim();
    if TASK_ID_PREFIXES.iter().any(|p| trimmed.starts_with(p)) {
        return trimmed.to_string();
    }
    UNKNOWN_TASK_ID.to_string()
}

/// Mark a task as completed by its ID in the task file.
fn mark_task_complete_by_id(task_id: &str, task_file: &Path) -> Result<()> {
    if task_id == UNKNOWN_TASK_ID {
        return Ok(());
    }
    let mut tasks = load_tasks(task_file)?;
    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        if task.status != crate::types::TaskStatus::Completed {
            task.status = crate::types::TaskStatus::Completed;
            task.last_attempt_at = Some(
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs(),
            );
            save_tasks(task_file, &tasks)?;
        }
    }
    Ok(())
}

/// Phase: NeedsTrigger – pick the next task and trigger a cloud coding agent.
///
/// Instead of running a local AI chat loop, this creates a GitHub issue with
/// the task description and assigns Copilot to it, which triggers the cloud
/// coding agent to autonomously make changes and create a pull request.
///
/// Returns [`StepOutcome::Yield`] because triggering the agent is an async
/// external operation (the agent needs time to work).
#[allow(clippy::too_many_arguments)]
async fn run_needs_trigger(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    ralph_name: &str,
    ralph_branch: Option<&str>,
    ralph: Option<&RalphConfig>,
    state: &mut HeadlessState,
    work_dir: &Path,
    state_dir: &Path,
) -> Result<StepOutcome> {
    let task_file = state_dir.join(&headless_cfg.task_file);
    let mut tasks = load_tasks(&task_file)?;

    // Reset any recurring tasks whose cooldown has elapsed.
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let reset_count = reset_recurring_tasks(&mut tasks, now);
    if reset_count > 0 {
        println!(
            "[wreck-it] reset {} recurring task(s) back to pending",
            reset_count
        );
        save_tasks(&task_file, &tasks)?;
    }

    // Build the set of completed task IDs for dependency checking.
    let completed_ids: HashSet<&str> = tasks
        .iter()
        .filter(|t| t.status == crate::types::TaskStatus::Completed)
        .map(|t| t.id.as_str())
        .collect();

    // Find the first pending task whose dependencies are all satisfied.
    let pending = tasks.iter().find(|t| {
        t.status == crate::types::TaskStatus::Pending
            && t.depends_on
                .iter()
                .all(|dep| completed_ids.contains(dep.as_str()))
    });
    let pending_task = match pending {
        Some(t) => t.clone(),
        None => {
            println!("[wreck-it] no ready tasks (all pending tasks have unmet dependencies)");
            return Ok(StepOutcome::Yield);
        }
    };

    if state.iteration >= headless_cfg.max_iterations {
        println!(
            "[wreck-it] max iterations ({}) reached",
            headless_cfg.max_iterations
        );
        return Ok(StepOutcome::Yield);
    }

    state.iteration += 1;
    state.current_task_id = Some(pending_task.id.clone());
    state.last_prompt = Some(pending_task.description.clone());

    // Resolve GitHub token and repo info.
    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required to trigger cloud agent")?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    let mut client = CloudAgentClient::new(github_token, repo_owner, repo_name);
    client.resolve_authenticated_login().await;

    // If a preferred agent is configured in the ralph config, set it on the
    // client so the assignment mutation targets that specific agent.
    if let Some(rc) = ralph {
        client.set_preferred_agent(rc.agent.clone());
    }

    // If a per-ralph feature branch is configured, ensure it exists on the
    // remote before triggering the cloud agent.  This creates the branch from
    // the repository default when it does not exist yet.
    if let Some(branch) = ralph_branch {
        if let Err(e) = ensure_feature_branch(work_dir, branch) {
            println!(
                "[wreck-it] warning: failed to ensure branch '{}': {}",
                branch, e,
            );
        }
    }

    println!(
        "[wreck-it] triggering cloud agent for task {}: {}",
        pending_task.id, pending_task.description
    );

    let result = client
        .trigger_agent(
            ralph_name,
            &pending_task.id,
            &pending_task.description,
            &state.memory,
            ralph_branch,
        )
        .await?;

    state.issue_number = Some(result.issue_number);
    state.pr_number = None;
    state.pr_url = None;
    state.phase = AgentPhase::AgentWorking;
    state.memory.push(format!(
        "iteration {}: triggered cloud agent for task {} (issue #{})",
        state.iteration, pending_task.id, result.issue_number,
    ));

    // Mark task as InProgress and save.
    let mut updated_tasks = load_tasks(&task_file)?;
    if let Some(task) = updated_tasks.iter_mut().find(|t| t.id == pending_task.id) {
        task.status = crate::types::TaskStatus::InProgress;
    }
    save_tasks(&task_file, &updated_tasks)?;

    println!(
        "[wreck-it] cloud agent triggered – issue: {}",
        result.issue_url,
    );

    Ok(StepOutcome::Yield)
}

/// Phase: AgentWorking – the cloud agent is still processing; check whether it
/// has created a PR yet.
///
/// Returns [`StepOutcome::Continue`] when the phase transitions without an
/// external wait (e.g. PR already found or error recovery), and
/// [`StepOutcome::Yield`] when we need to wait for the agent.
async fn run_agent_working(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<StepOutcome> {
    let issue_number = match state.issue_number {
        Some(n) => n,
        None => {
            println!("[wreck-it] no issue number in state, going back to trigger");
            state.phase = AgentPhase::NeedsTrigger;
            return Ok(StepOutcome::Continue);
        }
    };

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required to check cloud agent status")?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    let mut client = CloudAgentClient::new(github_token, repo_owner, repo_name);
    client.resolve_authenticated_login().await;

    println!(
        "[wreck-it] checking cloud agent status for issue #{}",
        issue_number
    );

    match client.check_agent_status(issue_number).await? {
        CloudAgentStatus::Working => {
            println!(
                "[wreck-it] agent is still working on issue #{}, will check again next run",
                issue_number
            );
            // Stay in AgentWorking phase.
            Ok(StepOutcome::Yield)
        }
        CloudAgentStatus::PrCreatedAgentWorking { pr_number, pr_url } => {
            println!(
                "[wreck-it] agent created PR #{} but is still working: {}",
                pr_number, pr_url
            );
            // Record the PR so it is tracked, but stay in AgentWorking
            // because the agent is still pushing changes.
            state.pr_number = Some(pr_number);
            state.pr_url = Some(pr_url.clone());
            state.memory.push(format!(
                "iteration {}: agent opened PR #{} for task {:?} (still working)",
                state.iteration, pr_number, state.current_task_id,
            ));
            if let Some(task_id) = state.current_task_id.clone() {
                if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_number) {
                    state.tracked_prs.push(TrackedPr {
                        pr_number,
                        task_id,
                        issue_number: Some(issue_number),
                        review_requested: None,
                    });
                }
            }
            Ok(StepOutcome::Yield)
        }
        CloudAgentStatus::PrCreated { pr_number, pr_url } => {
            println!("[wreck-it] agent created PR #{}: {}", pr_number, pr_url);
            state.pr_number = Some(pr_number);
            state.pr_url = Some(pr_url);
            state.phase = AgentPhase::NeedsVerification;
            state.memory.push(format!(
                "iteration {}: agent created PR #{} for task {:?}",
                state.iteration, pr_number, state.current_task_id,
            ));
            // Track this PR so it is managed across invocations.
            if let Some(task_id) = state.current_task_id.clone() {
                if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_number) {
                    state.tracked_prs.push(TrackedPr {
                        pr_number,
                        task_id,
                        issue_number: Some(issue_number),
                        review_requested: None,
                    });
                }
            }
            Ok(StepOutcome::Continue)
        }
        CloudAgentStatus::CompletedNoPr => {
            println!("[wreck-it] agent completed without creating a PR");
            state.phase = AgentPhase::NeedsTrigger;
            state.memory.push(format!(
                "iteration {}: agent completed without PR for task {:?}",
                state.iteration, state.current_task_id,
            ));
            Ok(StepOutcome::Continue)
        }
    }
}

/// Phase: NeedsVerification – the agent created a PR; advance it toward merge.
///
/// Progresses the PR through its lifecycle:
/// - Draft → mark ready for review.
/// - Not yet mergeable with required checks → approve workflows and enable
///   auto-merge so GitHub merges once checks pass.
/// - Mergeable without required checks → merge directly.
/// - Already merged → mark the task complete.
///
/// Returns [`StepOutcome::Continue`] when the phase transitions without an
/// external wait, and [`StepOutcome::Yield`] when waiting for an external
/// event (e.g. CI checks to pass).
async fn run_needs_verification(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    ralph: Option<&RalphConfig>,
    state: &mut HeadlessState,
    work_dir: &Path,
    state_dir: &Path,
) -> Result<StepOutcome> {
    let pr_number = match state.pr_number {
        Some(n) => n,
        None => {
            println!("[wreck-it] no PR to verify, going back to trigger");
            state.phase = AgentPhase::NeedsTrigger;
            return Ok(StepOutcome::Continue);
        }
    };

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required to merge PR")?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    let mut client = CloudAgentClient::new(github_token, repo_owner, repo_name);
    client.resolve_authenticated_login().await;

    println!("[wreck-it] checking PR #{} for merge readiness", pr_number);

    // Check PR status: draft, not-yet-mergeable, or ready.
    match client.check_pr_merge_status(pr_number).await {
        Ok(PrMergeStatus::Draft) => {
            println!(
                "[wreck-it] PR #{} is a draft, marking as ready for review",
                pr_number
            );
            if let Err(e) = client.mark_pr_ready_for_review(pr_number).await {
                println!(
                    "[wreck-it] failed to mark PR #{} as ready for review: {}",
                    pr_number, e
                );
                state.memory.push(format!(
                    "iteration {}: PR #{} is a draft; failed to mark ready: {}",
                    state.iteration, pr_number, e,
                ));
                return Ok(StepOutcome::Yield);
            }
            println!("[wreck-it] PR #{} marked as ready for review", pr_number);
            state.memory.push(format!(
                "iteration {}: marked PR #{} as ready for review",
                state.iteration, pr_number,
            ));
            // If reviewers are configured and not yet requested, request them
            // now and transition to AwaitingReview.
            if let Some(reviewers) = ralph.and_then(|r| r.reviewers.as_ref()) {
                if !reviewers.is_empty() && state.review_requested != Some(true) {
                    if let Err(e) = client.request_reviewers(pr_number, reviewers).await {
                        println!(
                            "[wreck-it] failed to request reviews on PR #{}: {}",
                            pr_number, e,
                        );
                    } else {
                        println!(
                            "[wreck-it] requested reviews on PR #{} from {:?}",
                            pr_number, reviewers,
                        );
                        state.review_requested = Some(true);
                        state.phase = AgentPhase::AwaitingReview;
                        return Ok(StepOutcome::Yield);
                    }
                }
            }
            // We made progress — loop again so the PR can be re-checked
            // immediately (it may now be mergeable).
            return Ok(StepOutcome::Continue);
        }
        Ok(PrMergeStatus::AgentWorkInProgress) => {
            println!(
                "[wreck-it] PR #{} — agent is still working, \
                 will retry next run",
                pr_number
            );
            // Log issue assignee details for diagnostics.
            if let Some(issue_num) = state.issue_number {
                log_issue_assignees(&client, issue_num, "[wreck-it]").await;
            }
            state.memory.push(format!(
                "iteration {}: PR #{} agent still working",
                state.iteration, pr_number,
            ));
            return Ok(StepOutcome::Yield);
        }
        Ok(PrMergeStatus::NotMergeable) => {
            // Check whether the base branch requires status checks.
            let has_checks = match client.has_required_checks_for_pr(pr_number).await {
                Ok(v) => v,
                Err(e) => {
                    println!(
                        "[wreck-it] failed to check required checks for PR #{}: {}",
                        pr_number, e
                    );
                    false
                }
            };
            if has_checks {
                // Check if any checks are actively failing.
                let has_failures = match client.has_failing_checks_for_pr(pr_number).await {
                    Ok(v) => v,
                    Err(e) => {
                        println!(
                            "[wreck-it] failed to check failing checks for PR #{}: {}",
                            pr_number, e
                        );
                        false
                    }
                };
                if has_failures {
                    println!(
                        "[wreck-it] PR #{} has failing checks, requesting @copilot fix",
                        pr_number
                    );
                    if let Err(e) = client
                        .comment_on_pr(
                            pr_number,
                            "@copilot The CI checks on this PR are failing. Please fix the failing checks.",
                        )
                        .await
                    {
                        println!(
                            "[wreck-it] failed to comment on PR #{}: {}",
                            pr_number, e
                        );
                    }
                } else {
                    println!(
                        "[wreck-it] PR #{} is not yet mergeable, approving workflows and enabling auto-merge",
                        pr_number
                    );
                    if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                        println!(
                            "[wreck-it] failed to approve workflow runs for PR #{}: {}",
                            pr_number, e
                        );
                    }
                    if let Err(e) = client.enable_auto_merge(pr_number).await {
                        println!(
                            "[wreck-it] failed to enable auto-merge for PR #{}: {}",
                            pr_number, e
                        );
                    }
                }
            } else {
                println!(
                    "[wreck-it] PR #{} is not yet mergeable (no required checks), will retry next run",
                    pr_number
                );
            }
            // Log issue assignee details for diagnostics.
            if let Some(issue_num) = state.issue_number {
                log_issue_assignees(&client, issue_num, "[wreck-it]").await;
            }
            state.memory.push(format!(
                "iteration {}: PR #{} not yet mergeable",
                state.iteration, pr_number,
            ));
            return Ok(StepOutcome::Yield);
        }
        Ok(PrMergeStatus::AlreadyMerged) => {
            println!(
                "[wreck-it] PR #{} is already merged, marking task complete",
                pr_number
            );
            state.phase = AgentPhase::Completed;
            state.memory.push(format!(
                "iteration {}: PR #{} already merged for task {:?}",
                state.iteration, pr_number, state.current_task_id,
            ));

            // Mark task as completed.
            let task_file = state_dir.join(&headless_cfg.task_file);
            if let Some(task_id) = &state.current_task_id {
                mark_task_complete_by_id(task_id, &task_file)?;
            }
            // Remove from tracked list.
            state.tracked_prs.retain(|tp| tp.pr_number != pr_number);

            return Ok(StepOutcome::Continue);
        }
        Ok(PrMergeStatus::Mergeable) => {
            // If reviewers are configured and reviews not yet requested,
            // request them now and transition to AwaitingReview.
            if let Some(reviewers) = ralph.and_then(|r| r.reviewers.as_ref()) {
                if !reviewers.is_empty() && state.review_requested != Some(true) {
                    if let Err(e) = client.request_reviewers(pr_number, reviewers).await {
                        println!(
                            "[wreck-it] failed to request reviews on PR #{}: {}",
                            pr_number, e,
                        );
                    } else {
                        println!(
                            "[wreck-it] requested reviews on PR #{} from {:?}",
                            pr_number, reviewers,
                        );
                        state.review_requested = Some(true);
                        state.phase = AgentPhase::AwaitingReview;
                        return Ok(StepOutcome::Yield);
                    }
                }
            }
            /* proceed to merge logic below */
        }
        Err(e) => {
            println!(
                "[wreck-it] error checking PR #{} merge status: {}",
                pr_number, e
            );
            return Ok(StepOutcome::Yield);
        }
    }

    // PR is mergeable.  Check for required checks to decide the strategy.
    let has_checks = match client.has_required_checks_for_pr(pr_number).await {
        Ok(v) => v,
        Err(e) => {
            println!(
                "[wreck-it] failed to check required checks for PR #{}: {}",
                pr_number, e
            );
            false
        }
    };

    if has_checks {
        // Required checks exist and pass; enable auto-merge and approve
        // workflows in case any are still pending.
        println!(
            "[wreck-it] PR #{} is mergeable with required checks, enabling auto-merge",
            pr_number
        );
        if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
            println!(
                "[wreck-it] failed to approve workflow runs for PR #{}: {}",
                pr_number, e
            );
        }
        if let Err(e) = client.enable_auto_merge(pr_number).await {
            println!(
                "[wreck-it] failed to enable auto-merge for PR #{}: {}",
                pr_number, e
            );
        }
        state.memory.push(format!(
            "iteration {}: enabled auto-merge for PR #{}",
            state.iteration, pr_number,
        ));
        return Ok(StepOutcome::Yield);
    }

    // No required checks detected via branch protection / rulesets.
    // As a safety net, check whether there are any pending (queued or
    // in-progress) check runs on the head commit.  If so, enable
    // auto-merge instead of merging directly to avoid merging before
    // checks complete.
    let has_pending = match client.has_pending_checks_for_pr(pr_number).await {
        Ok(v) => v,
        Err(e) => {
            println!(
                "[wreck-it] failed to check pending checks for PR #{}: {}",
                pr_number, e
            );
            false
        }
    };
    if has_pending {
        println!(
            "[wreck-it] PR #{} has pending check runs, enabling auto-merge instead of merging directly",
            pr_number
        );
        if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
            println!(
                "[wreck-it] failed to approve workflow runs for PR #{}: {}",
                pr_number, e
            );
        }
        if let Err(e) = client.enable_auto_merge(pr_number).await {
            println!(
                "[wreck-it] failed to enable auto-merge for PR #{}: {}",
                pr_number, e
            );
        }
        state.memory.push(format!(
            "iteration {}: PR #{} has pending checks, enabled auto-merge",
            state.iteration, pr_number,
        ));
        return Ok(StepOutcome::Yield);
    }

    // No required checks and no pending check runs — merge directly.
    match client.merge_pr(pr_number).await {
        Ok(()) => {
            println!("[wreck-it] PR #{} merged successfully", pr_number);
            state.phase = AgentPhase::Completed;
            state.memory.push(format!(
                "iteration {}: merged PR #{} for task {:?}",
                state.iteration, pr_number, state.current_task_id,
            ));

            // Mark task as completed.
            let task_file = state_dir.join(&headless_cfg.task_file);
            if let Some(task_id) = &state.current_task_id {
                mark_task_complete_by_id(task_id, &task_file)?;
            }
            // Remove from tracked list.
            state.tracked_prs.retain(|tp| tp.pr_number != pr_number);

            Ok(StepOutcome::Continue)
        }
        Err(e) => {
            println!("[wreck-it] failed to merge PR #{}: {}", pr_number, e);
            state.memory.push(format!(
                "iteration {}: merge failed for PR #{}: {}",
                state.iteration, pr_number, e,
            ));
            // Stay in NeedsVerification to retry on the next invocation.
            Ok(StepOutcome::Yield)
        }
    }
}

/// Phase: AwaitingReview – reviews have been requested on the PR; wait for all
/// reviewers to submit their reviews.
///
/// When all reviews are approved, transitions to [`AgentPhase::NeedsVerification`]
/// so the PR can proceed to merge.  When at least one reviewer requests changes,
/// the PR author (the coding agent) is at-mentioned to address the feedback.
/// While the agent is working, the phase yields.
async fn run_awaiting_review(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    ralph: Option<&RalphConfig>,
    state: &mut HeadlessState,
    work_dir: &Path,
    _state_dir: &Path,
) -> Result<StepOutcome> {
    let pr_number = match state.pr_number {
        Some(n) => n,
        None => {
            println!("[wreck-it] no PR to review, going back to trigger");
            state.phase = AgentPhase::NeedsTrigger;
            return Ok(StepOutcome::Continue);
        }
    };

    let reviewers = match ralph.and_then(|r| r.reviewers.as_ref()) {
        Some(r) if !r.is_empty() => r.clone(),
        _ => {
            // No reviewers configured; skip review and proceed to merge.
            println!(
                "[wreck-it] no reviewers configured, proceeding to verification for PR #{}",
                pr_number,
            );
            state.phase = AgentPhase::NeedsVerification;
            return Ok(StepOutcome::Continue);
        }
    };

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required to check reviews")?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    let mut client = CloudAgentClient::new(github_token, repo_owner, repo_name);
    client.resolve_authenticated_login().await;

    // Check if the coding agent is still pushing changes (e.g. addressing
    // review feedback from a previous round).
    if let Some(false) = client.check_copilot_session_completed(pr_number).await {
        println!(
            "[wreck-it] PR #{} — agent is still working on review feedback, will retry",
            pr_number,
        );
        return Ok(StepOutcome::Yield);
    }

    println!(
        "[wreck-it] checking review status for PR #{} (reviewers: {:?})",
        pr_number, reviewers,
    );

    use crate::cloud_agent::ReviewStatus;
    match client.check_reviews_complete(pr_number, &reviewers).await {
        Ok(ReviewStatus::Approved) => {
            println!(
                "[wreck-it] all reviews on PR #{} are approved, proceeding to verification",
                pr_number,
            );
            state.memory.push(format!(
                "iteration {}: reviews approved on PR #{}",
                state.iteration, pr_number,
            ));
            state.phase = AgentPhase::NeedsVerification;
            // Clear review_requested so a fresh cycle can be started if needed.
            state.review_requested = None;
            Ok(StepOutcome::Continue)
        }
        Ok(ReviewStatus::ChangesRequested {
            reviewers: requested_by,
        }) => {
            println!(
                "[wreck-it] PR #{} has changes requested by {:?}, notifying author",
                pr_number, requested_by,
            );
            // At-mention the PR author to address the review feedback.
            match client.get_pr_author(pr_number).await {
                Ok(Some(author)) => {
                    let comment = format!(
                        "@{} Review feedback has been submitted requesting changes. \
                         Please address the review comments.",
                        author,
                    );
                    if let Err(e) = client.comment_on_pr(pr_number, &comment).await {
                        println!("[wreck-it] failed to comment on PR #{}: {}", pr_number, e,);
                    } else {
                        println!(
                            "[wreck-it] notified @{} on PR #{} to address review feedback",
                            author, pr_number,
                        );
                    }
                }
                Ok(None) => {
                    println!(
                        "[wreck-it] could not determine PR #{} author; skipping @mention",
                        pr_number,
                    );
                }
                Err(e) => {
                    println!("[wreck-it] failed to fetch PR #{} author: {}", pr_number, e,);
                }
            }
            state.memory.push(format!(
                "iteration {}: changes requested on PR #{} by {:?}",
                state.iteration, pr_number, requested_by,
            ));
            // Reset review_requested so that after the agent addresses the
            // feedback, reviews can be re-requested on the next cycle.
            state.review_requested = None;
            // Stay in AwaitingReview; on the next invocation, if the agent
            // pushes new changes, we will detect it and re-request reviews
            // once it finishes.
            Ok(StepOutcome::Yield)
        }
        Ok(ReviewStatus::Pending) => {
            println!(
                "[wreck-it] PR #{} reviews are still pending, will check again",
                pr_number,
            );
            Ok(StepOutcome::Yield)
        }
        Err(e) => {
            println!(
                "[wreck-it] failed to check review status for PR #{}: {}",
                pr_number, e,
            );
            Ok(StepOutcome::Yield)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn step_outcome_variants() {
        assert_eq!(StepOutcome::Continue, StepOutcome::Continue);
        assert_eq!(StepOutcome::Yield, StepOutcome::Yield);
        assert_ne!(StepOutcome::Continue, StepOutcome::Yield);
    }

    #[test]
    fn completed_phase_is_synchronous() {
        // The Completed phase handler in run_headless resets state and returns
        // StepOutcome::Continue.  Verify the state reset logic directly.
        let mut state = HeadlessState {
            phase: AgentPhase::Completed,
            iteration: 5,
            current_task_id: Some("task-1".to_string()),
            issue_number: Some(42),
            pr_number: Some(10),
            pr_url: Some("https://github.com/o/r/pull/10".to_string()),
            last_prompt: Some("do something".to_string()),
            memory: vec![],
            tracked_prs: vec![],
            review_requested: None,
        };

        // Simulate the Completed branch of the loop.
        state.phase = AgentPhase::NeedsTrigger;
        state.current_task_id = None;
        state.issue_number = None;
        state.pr_number = None;
        state.pr_url = None;
        state.last_prompt = None;

        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert!(state.current_task_id.is_none());
        assert!(state.issue_number.is_none());
        assert!(state.pr_number.is_none());
        assert!(state.pr_url.is_none());
        assert!(state.last_prompt.is_none());
        // Iteration counter is preserved.
        assert_eq!(state.iteration, 5);
    }

    #[test]
    fn max_sync_steps_is_bounded() {
        // Ensure the constant exists and is reasonable.
        assert!(MAX_SYNC_STEPS > 0);
        assert!(MAX_SYNC_STEPS <= 100);
    }

    #[test]
    fn max_progress_rounds_is_bounded() {
        // Ensure the progress-loop cap exists and is reasonable.
        assert!(MAX_PROGRESS_ROUNDS > 0);
        assert!(MAX_PROGRESS_ROUNDS <= 100);
    }

    #[test]
    fn infer_task_id_from_wreck_it_prefix() {
        assert_eq!(infer_task_id_from_title("[wreck-it] impl-3"), "impl-3");
        assert_eq!(infer_task_id_from_title("[wreck-it] eval-2"), "eval-2");
    }

    #[test]
    fn infer_task_id_from_bare_id() {
        assert_eq!(infer_task_id_from_title("ideas-1"), "ideas-1");
        assert_eq!(infer_task_id_from_title("impl-5"), "impl-5");
        assert_eq!(infer_task_id_from_title("eval-7"), "eval-7");
    }

    #[test]
    fn infer_task_id_unknown_for_unrecognized_title() {
        assert_eq!(
            infer_task_id_from_title("Fix some random bug"),
            UNKNOWN_TASK_ID
        );
    }

    #[test]
    fn infer_task_id_with_ralph_name() {
        assert_eq!(infer_task_id_from_title("[wreck-it] back-to-roots 1"), "1");
        assert_eq!(infer_task_id_from_title("[wreck-it] docs impl-3"), "impl-3");
        assert_eq!(
            infer_task_id_from_title("[wreck-it] feature-dev eval-2"),
            "eval-2"
        );
    }

    #[test]
    fn mark_task_complete_by_id_updates_status() {
        let dir = tempfile::tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![crate::types::Task {
            id: "t1".to_string(),
            description: "task one".to_string(),
            status: crate::types::TaskStatus::InProgress,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }];
        save_tasks(&task_file, &tasks).unwrap();

        mark_task_complete_by_id("t1", &task_file).unwrap();

        let reloaded = load_tasks(&task_file).unwrap();
        assert_eq!(reloaded[0].status, crate::types::TaskStatus::Completed);
    }

    #[test]
    fn mark_task_complete_by_id_ignores_unknown() {
        let dir = tempfile::tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![crate::types::Task {
            id: "t1".to_string(),
            description: "task one".to_string(),
            status: crate::types::TaskStatus::Pending,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }];
        save_tasks(&task_file, &tasks).unwrap();

        // UNKNOWN_TASK_ID task IDs are skipped.
        mark_task_complete_by_id(UNKNOWN_TASK_ID, &task_file).unwrap();

        let reloaded = load_tasks(&task_file).unwrap();
        assert_eq!(reloaded[0].status, crate::types::TaskStatus::Pending);
    }

    #[test]
    fn mark_task_complete_by_id_sets_last_attempt_at() {
        let dir = tempfile::tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![crate::types::Task {
            id: "rec-1".to_string(),
            description: "recurring task".to_string(),
            status: crate::types::TaskStatus::InProgress,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::Recurring,
            cooldown_seconds: Some(3600),
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }];
        save_tasks(&task_file, &tasks).unwrap();

        mark_task_complete_by_id("rec-1", &task_file).unwrap();

        let reloaded = load_tasks(&task_file).unwrap();
        assert_eq!(reloaded[0].status, crate::types::TaskStatus::Completed);
        assert!(
            reloaded[0].last_attempt_at.is_some(),
            "last_attempt_at should be set on completion so cooldown is respected"
        );
    }

    #[test]
    fn mark_task_complete_then_cooldown_prevents_immediate_reset() {
        let dir = tempfile::tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![crate::types::Task {
            id: "rec-1".to_string(),
            description: "recurring task".to_string(),
            status: crate::types::TaskStatus::InProgress,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::Recurring,
            cooldown_seconds: Some(3600),
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }];
        save_tasks(&task_file, &tasks).unwrap();

        // Complete the task (sets last_attempt_at to now).
        mark_task_complete_by_id("rec-1", &task_file).unwrap();

        // Reload and immediately try to reset recurring tasks.
        let mut reloaded = load_tasks(&task_file).unwrap();
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let reset_count = reset_recurring_tasks(&mut reloaded, now);

        // Cooldown of 3600s should prevent an immediate reset.
        assert_eq!(
            reset_count, 0,
            "recurring task should not reset before cooldown elapses"
        );
        assert_eq!(reloaded[0].status, crate::types::TaskStatus::Completed);
    }

    #[test]
    fn awaiting_review_phase_resets_on_completed() {
        // The Completed phase handler resets review_requested along with
        // other state fields.
        let mut state = HeadlessState {
            phase: AgentPhase::Completed,
            iteration: 5,
            current_task_id: Some("task-1".to_string()),
            issue_number: Some(42),
            pr_number: Some(10),
            pr_url: Some("https://github.com/o/r/pull/10".to_string()),
            last_prompt: Some("do something".to_string()),
            memory: vec![],
            tracked_prs: vec![],
            review_requested: Some(true),
        };

        // Simulate the Completed branch of the loop.
        state.phase = AgentPhase::NeedsTrigger;
        state.current_task_id = None;
        state.issue_number = None;
        state.pr_number = None;
        state.pr_url = None;
        state.last_prompt = None;
        state.review_requested = None;

        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert!(state.review_requested.is_none());
    }

    #[test]
    fn awaiting_review_phase_exists() {
        // Verify the AwaitingReview variant is properly defined and serializable.
        let phase = AgentPhase::AwaitingReview;
        assert_eq!(phase, AgentPhase::AwaitingReview);
        assert_ne!(phase, AgentPhase::NeedsVerification);
        assert_ne!(phase, AgentPhase::AgentWorking);

        // Verify serde roundtrip.
        let json = serde_json::to_string(&phase).unwrap();
        assert_eq!(json, "\"awaiting_review\"");
        let loaded: AgentPhase = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, AgentPhase::AwaitingReview);
    }

    #[test]
    fn tracked_pr_review_requested_field() {
        let pr = TrackedPr {
            pr_number: 42,
            task_id: "task-1".to_string(),
            issue_number: Some(10),
            review_requested: Some(true),
        };
        assert_eq!(pr.review_requested, Some(true));

        // Roundtrip.
        let json = serde_json::to_string(&pr).unwrap();
        assert!(json.contains("review_requested"));
        let loaded: TrackedPr = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.review_requested, Some(true));
    }

    #[test]
    fn tracked_pr_review_requested_omitted_when_none() {
        let pr = TrackedPr {
            pr_number: 42,
            task_id: "task-1".to_string(),
            issue_number: None,
            review_requested: None,
        };
        let json = serde_json::to_string(&pr).unwrap();
        assert!(!json.contains("review_requested"));
    }

    #[test]
    fn headless_state_review_requested_backward_compat() {
        // Existing state JSON without review_requested should load fine.
        let json = r#"{"phase":"needs_trigger","iteration":3}"#;
        let state: HeadlessState = serde_json::from_str(json).unwrap();
        assert!(state.review_requested.is_none());
    }
}
