use crate::cloud_agent::{resolve_repo_info, CloudAgentClient, CloudAgentStatus, PrMergeStatus};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::headless_state::{
    load_headless_state, save_headless_state, AgentPhase, HeadlessState, TrackedPr,
};
use crate::task_manager::{load_tasks, save_tasks};
use crate::types::Config;
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Maximum number of synchronous steps executed in a single invocation before
/// forcing a yield.  This prevents infinite loops when the state machine
/// repeatedly transitions through synchronous phases.
const MAX_SYNC_STEPS: usize = 20;

/// Sentinel value returned by [`infer_task_id_from_title`] when no task ID
/// can be determined from the PR title.
const UNKNOWN_TASK_ID: &str = "unknown";

/// Known task-ID prefixes used by the wreck-it agent swarm.
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
pub async fn run_headless(config: Config) -> Result<()> {
    // Try to load repo-committed headless config for state_file path and
    // other overrides.
    let work_dir = config.work_dir.clone();
    let headless_cfg_path = work_dir.join(DEFAULT_CONFIG_FILE);
    let headless_cfg = if headless_cfg_path.exists() {
        load_headless_config(&headless_cfg_path).context("Failed to load .wreck-it.toml")?
    } else {
        HeadlessConfig::default()
    };

    let state_path = work_dir.join(&headless_cfg.state_file);
    let mut state = load_headless_state(&state_path).context("Failed to load headless state")?;

    // Sweep all open PRs before entering the main state machine.  This
    // converts drafts to ready, approves pending workflow runs, and merges
    // PRs that are ready – even for tasks not currently tracked in state.
    if let Err(e) = sweep_open_prs(&config, &headless_cfg, &mut state, &work_dir).await {
        println!("[wreck-it] warning: sweep_open_prs failed: {}", e);
    }

    let mut sync_steps: usize = 0;

    loop {
        println!(
            "[wreck-it] headless iteration {} | phase: {:?}",
            state.iteration, state.phase
        );

        let outcome = match state.phase {
            AgentPhase::NeedsTrigger => {
                run_needs_trigger(&config, &headless_cfg, &mut state, &work_dir).await?
            }
            AgentPhase::AgentWorking => {
                run_agent_working(&config, &headless_cfg, &mut state, &work_dir).await?
            }
            AgentPhase::NeedsVerification => {
                run_needs_verification(&config, &headless_cfg, &mut state, &work_dir).await?
            }
            AgentPhase::Completed => {
                println!("[wreck-it] previous task completed, advancing to next trigger");
                state.phase = AgentPhase::NeedsTrigger;
                state.current_task_id = None;
                state.issue_number = None;
                state.pr_number = None;
                state.pr_url = None;
                state.last_prompt = None;
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

    save_headless_state(&state_path, &state).context("Failed to save headless state")?;

    println!(
        "[wreck-it] saved state: phase={:?} iteration={}",
        state.phase, state.iteration
    );

    Ok(())
}

/// Sweep all open PRs in the repository.
///
/// For each open PR (and each previously tracked PR):
/// - If it is a draft → mark it ready for review and approve pending workflows.
/// - If it is not yet mergeable → approve pending workflow runs so CI can start.
/// - If it is mergeable → merge it.
/// - If it was already merged → mark the associated task complete.
///
/// All open PRs are recorded in `state.tracked_prs`.  When a PR is merged or
/// closed, it is removed from the tracked list and the associated task is
/// marked complete in the task file.
///
/// When a merged PR corresponds to the currently tracked task in `state`, the
/// headless state is advanced to [`AgentPhase::Completed`] so the next
/// invocation can pick up a new task.
async fn sweep_open_prs(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<()> {
    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required to sweep open PRs")?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    let client = CloudAgentClient::new(github_token, repo_owner, repo_name);

    let open_prs = client.list_open_prs().await?;

    // Merge the open PR list with previously tracked PRs.  Any PR that is
    // still open stays tracked; new PRs are added.  We attempt to infer the
    // task ID from the PR title ("[wreck-it] <task_id>") for any PR not
    // already in the tracked list.
    let previously_tracked: HashSet<u64> =
        state.tracked_prs.iter().map(|tp| tp.pr_number).collect();

    for pr in &open_prs {
        if !previously_tracked.contains(&pr.number) {
            let task_id = infer_task_id_from_title(&pr.title);
            state.tracked_prs.push(TrackedPr {
                pr_number: pr.number,
                task_id,
            });
        }
    }

    // Also ensure the current task's PR is tracked.
    if let (Some(pr_num), Some(task_id)) = (state.pr_number, state.current_task_id.clone()) {
        if !state.tracked_prs.iter().any(|tp| tp.pr_number == pr_num) {
            state.tracked_prs.push(TrackedPr {
                pr_number: pr_num,
                task_id,
            });
        }
    }

    if state.tracked_prs.is_empty() {
        println!("[wreck-it] sweep: no open or tracked PRs");
        return Ok(());
    }

    println!(
        "[wreck-it] sweep: processing {} tracked PR(s) ({} currently open)",
        state.tracked_prs.len(),
        open_prs.len(),
    );

    let task_file = work_dir.join(&headless_cfg.task_file);

    // Process each tracked PR.  Collect indices of PRs that have been merged
    // (to remove from the tracked list after the loop).
    let open_pr_numbers: HashSet<u64> = open_prs.iter().map(|p| p.number).collect();
    let mut merged_pr_numbers: Vec<u64> = Vec::new();

    // Iterate over a snapshot of tracked_prs to avoid borrow issues.
    let tracked_snapshot: Vec<TrackedPr> = state.tracked_prs.clone();

    for tracked in &tracked_snapshot {
        let pr_number = tracked.pr_number;

        // If the PR is no longer in the open list, check if it was merged.
        if !open_pr_numbers.contains(&pr_number) {
            match client.check_pr_merge_status(pr_number).await {
                Ok(PrMergeStatus::AlreadyMerged) => {
                    println!(
                        "[wreck-it] sweep: tracked PR #{} (task {}) already merged",
                        pr_number, tracked.task_id
                    );
                    mark_task_complete_by_id(&tracked.task_id, &task_file)?;
                    if state.pr_number == Some(pr_number) {
                        state.phase = AgentPhase::Completed;
                    }
                    merged_pr_numbers.push(pr_number);
                }
                _ => {
                    // PR closed without merge or error — remove from tracking.
                    println!(
                        "[wreck-it] sweep: tracked PR #{} (task {}) is no longer open",
                        pr_number, tracked.task_id
                    );
                    merged_pr_numbers.push(pr_number);
                }
            }
            continue;
        }

        println!(
            "[wreck-it] sweep: PR #{} (task {})",
            pr_number, tracked.task_id
        );

        match client.check_pr_merge_status(pr_number).await {
            Ok(PrMergeStatus::Draft) => {
                println!(
                    "[wreck-it] sweep: PR #{} is a draft, marking ready for review",
                    pr_number
                );
                if let Err(e) = client.mark_pr_ready_for_review(pr_number).await {
                    println!(
                        "[wreck-it] sweep: failed to mark PR #{} ready: {}",
                        pr_number, e
                    );
                }
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    println!(
                        "[wreck-it] sweep: failed to approve workflows for PR #{}: {}",
                        pr_number, e
                    );
                }
                state.memory.push(format!(
                    "sweep: marked PR #{} (task {}) as ready for review",
                    pr_number, tracked.task_id,
                ));
            }
            Ok(PrMergeStatus::NotMergeable) => {
                println!(
                    "[wreck-it] sweep: PR #{} not yet mergeable, approving workflows",
                    pr_number
                );
                if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                    println!(
                        "[wreck-it] sweep: failed to approve workflows for PR #{}: {}",
                        pr_number, e
                    );
                }
            }
            Ok(PrMergeStatus::Mergeable) => {
                println!("[wreck-it] sweep: PR #{} is mergeable, merging", pr_number);
                match client.merge_pr(pr_number).await {
                    Ok(()) => {
                        println!("[wreck-it] sweep: merged PR #{}", pr_number);
                        state
                            .memory
                            .push(format!("sweep: merged PR #{}", pr_number));
                        mark_task_complete_by_id(&tracked.task_id, &task_file)?;
                        if state.pr_number == Some(pr_number) {
                            state.phase = AgentPhase::Completed;
                        }
                        merged_pr_numbers.push(pr_number);
                    }
                    Err(e) => {
                        println!("[wreck-it] sweep: failed to merge PR #{}: {}", pr_number, e);
                    }
                }
            }
            Ok(PrMergeStatus::AlreadyMerged) => {
                println!("[wreck-it] sweep: PR #{} already merged", pr_number);
                mark_task_complete_by_id(&tracked.task_id, &task_file)?;
                if state.pr_number == Some(pr_number) {
                    state.phase = AgentPhase::Completed;
                }
                merged_pr_numbers.push(pr_number);
            }
            Err(e) => {
                println!("[wreck-it] sweep: error checking PR #{}: {}", pr_number, e);
            }
        }
    }

    // Remove merged/closed PRs from the tracked list.
    let merged_set: HashSet<u64> = merged_pr_numbers.into_iter().collect();
    state
        .tracked_prs
        .retain(|tp| !merged_set.contains(&tp.pr_number));

    Ok(())
}

/// Try to infer a task ID from a PR title.
///
/// Agent-created PRs often reference the triggering issue whose title is
/// `[wreck-it] <task_id>`.  PR titles may contain the issue number (e.g.
/// "Fixes #42") or quote the task ID directly.  This is a best-effort
/// extraction; returns [`UNKNOWN_TASK_ID`] when no ID can be inferred.
fn infer_task_id_from_title(title: &str) -> String {
    // Look for "[wreck-it] <task_id>" pattern.
    if let Some(rest) = title.strip_prefix("[wreck-it] ") {
        return rest.trim().to_string();
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
async fn run_needs_trigger(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<StepOutcome> {
    let task_file = work_dir.join(&headless_cfg.task_file);
    let tasks = load_tasks(&task_file)?;

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

    let client = CloudAgentClient::new(github_token, repo_owner, repo_name);

    println!(
        "[wreck-it] triggering cloud agent for task {}: {}",
        pending_task.id, pending_task.description
    );

    let result = client
        .trigger_agent(&pending_task.id, &pending_task.description, &state.memory)
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

    let client = CloudAgentClient::new(github_token, repo_owner, repo_name);

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
                    state.tracked_prs.push(TrackedPr { pr_number, task_id });
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

/// Phase: NeedsVerification – the agent created a PR; try to merge it.
///
/// Checks whether the PR is mergeable and merges it.  On success the task is
/// marked complete and [`StepOutcome::Continue`] is returned so the loop can
/// advance to the next task immediately.  When waiting for the PR to become
/// mergeable, returns [`StepOutcome::Yield`].
async fn run_needs_verification(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
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

    let client = CloudAgentClient::new(github_token, repo_owner, repo_name);

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
            // Approve any workflow runs that require approval so checks can
            // start running (repos may require approval for Actions).
            if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                println!(
                    "[wreck-it] failed to approve workflow runs for PR #{}: {}",
                    pr_number, e
                );
            }
            state.memory.push(format!(
                "iteration {}: marked PR #{} as ready for review",
                state.iteration, pr_number,
            ));
            // Mergeability may not be immediate; retry on the next run.
            return Ok(StepOutcome::Yield);
        }
        Ok(PrMergeStatus::NotMergeable) => {
            println!(
                "[wreck-it] PR #{} is not yet mergeable, will retry next run",
                pr_number
            );
            // Approve any workflow runs that require approval – the PR may
            // be stuck because checks haven't started yet.
            if let Err(e) = client.approve_pending_workflow_runs(pr_number).await {
                println!(
                    "[wreck-it] failed to approve workflow runs for PR #{}: {}",
                    pr_number, e
                );
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
            let task_file = work_dir.join(&headless_cfg.task_file);
            if let Some(task_id) = &state.current_task_id {
                mark_task_complete_by_id(task_id, &task_file)?;
            }
            // Remove from tracked list.
            state.tracked_prs.retain(|tp| tp.pr_number != pr_number);

            return Ok(StepOutcome::Continue);
        }
        Ok(PrMergeStatus::Mergeable) => { /* proceed to merge */ }
        Err(e) => {
            println!(
                "[wreck-it] error checking PR #{} merge status: {}",
                pr_number, e
            );
            return Ok(StepOutcome::Yield);
        }
    }

    match client.merge_pr(pr_number).await {
        Ok(()) => {
            println!("[wreck-it] PR #{} merged successfully", pr_number);
            state.phase = AgentPhase::Completed;
            state.memory.push(format!(
                "iteration {}: merged PR #{} for task {:?}",
                state.iteration, pr_number, state.current_task_id,
            ));

            // Mark task as completed.
            let task_file = work_dir.join(&headless_cfg.task_file);
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
    fn mark_task_complete_by_id_updates_status() {
        let dir = tempfile::tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![crate::types::Task {
            id: "t1".to_string(),
            description: "task one".to_string(),
            status: crate::types::TaskStatus::InProgress,
            role: crate::types::AgentRole::default(),
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
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
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
        }];
        save_tasks(&task_file, &tasks).unwrap();

        // UNKNOWN_TASK_ID task IDs are skipped.
        mark_task_complete_by_id(UNKNOWN_TASK_ID, &task_file).unwrap();

        let reloaded = load_tasks(&task_file).unwrap();
        assert_eq!(reloaded[0].status, crate::types::TaskStatus::Pending);
    }
}
