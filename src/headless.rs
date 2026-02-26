use crate::cloud_agent::{resolve_repo_info, CloudAgentClient, CloudAgentStatus};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::headless_state::{load_headless_state, save_headless_state, AgentPhase, HeadlessState};
use crate::task_manager::{load_tasks, save_tasks};
use crate::types::Config;
use anyhow::{Context, Result};
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Run wreck-it in headless mode.
///
/// This is designed for CI environments (e.g. a cron-triggered GitHub Actions
/// workflow).  Instead of running a local AI chat loop, each invocation drives
/// one step of a cloud-agent state machine:
///
///   NeedsTrigger → create a GitHub issue and assign Copilot (triggers the
///                  cloud coding agent)
///   AgentWorking → poll the issue for a linked PR created by the agent
///   NeedsVerification → check PR mergeability and merge it
///   Completed → mark the task done, advance to the next one
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

    println!(
        "[wreck-it] headless iteration {} | phase: {:?}",
        state.iteration, state.phase
    );

    match state.phase {
        AgentPhase::NeedsTrigger => {
            run_needs_trigger(&config, &headless_cfg, &mut state, &work_dir).await?;
        }
        AgentPhase::AgentWorking => {
            run_agent_working(&config, &headless_cfg, &mut state, &work_dir).await?;
        }
        AgentPhase::NeedsVerification => {
            run_needs_verification(&config, &headless_cfg, &mut state, &work_dir).await?;
        }
        AgentPhase::Completed => {
            println!("[wreck-it] previous task completed, advancing to next trigger");
            state.phase = AgentPhase::NeedsTrigger;
            state.current_task_id = None;
            state.issue_number = None;
            state.pr_number = None;
            state.pr_url = None;
            state.last_prompt = None;
        }
    }

    save_headless_state(&state_path, &state).context("Failed to save headless state")?;

    println!(
        "[wreck-it] saved state: phase={:?} iteration={}",
        state.phase, state.iteration
    );

    Ok(())
}

/// Phase: NeedsTrigger – pick the next task and trigger a cloud coding agent.
///
/// Instead of running a local AI chat loop, this creates a GitHub issue with
/// the task description and assigns Copilot to it, which triggers the cloud
/// coding agent to autonomously make changes and create a pull request.
async fn run_needs_trigger(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<()> {
    let task_file = work_dir.join(&headless_cfg.task_file);
    let tasks = load_tasks(&task_file)?;

    // Check if there are pending tasks
    let pending = tasks
        .iter()
        .find(|t| t.status == crate::types::TaskStatus::Pending);
    let pending_task = match pending {
        Some(t) => t.clone(),
        None => {
            println!("[wreck-it] no pending tasks, nothing to do");
            return Ok(());
        }
    };

    if state.iteration >= headless_cfg.max_iterations {
        println!(
            "[wreck-it] max iterations ({}) reached",
            headless_cfg.max_iterations
        );
        return Ok(());
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
        .trigger_agent(&pending_task.id, &pending_task.description)
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

    Ok(())
}

/// Phase: AgentWorking – the cloud agent is still processing; check whether it
/// has created a PR yet.
async fn run_agent_working(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<()> {
    let issue_number = match state.issue_number {
        Some(n) => n,
        None => {
            println!("[wreck-it] no issue number in state, going back to trigger");
            state.phase = AgentPhase::NeedsTrigger;
            return Ok(());
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
        }
        CloudAgentStatus::CompletedNoPr => {
            println!("[wreck-it] agent completed without creating a PR");
            state.phase = AgentPhase::NeedsTrigger;
            state.memory.push(format!(
                "iteration {}: agent completed without PR for task {:?}",
                state.iteration, state.current_task_id,
            ));
        }
    }

    Ok(())
}

/// Phase: NeedsVerification – the agent created a PR; try to merge it.
///
/// Checks whether the PR is mergeable and merges it.  On success the task is
/// marked complete; on failure we stay in this phase so the next cron
/// invocation will retry.
async fn run_needs_verification(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<()> {
    let pr_number = match state.pr_number {
        Some(n) => n,
        None => {
            println!("[wreck-it] no PR to verify, going back to trigger");
            state.phase = AgentPhase::NeedsTrigger;
            return Ok(());
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

    // Check if the PR is mergeable before attempting the merge.
    match client.is_pr_mergeable(pr_number).await {
        Ok(true) => { /* proceed to merge */ }
        Ok(false) => {
            println!(
                "[wreck-it] PR #{} is not yet mergeable, will retry next run",
                pr_number
            );
            state.memory.push(format!(
                "iteration {}: PR #{} not yet mergeable",
                state.iteration, pr_number,
            ));
            return Ok(());
        }
        Err(e) => {
            println!(
                "[wreck-it] error checking PR #{} mergeability: {}",
                pr_number, e
            );
            return Ok(());
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
            let mut tasks = load_tasks(&task_file)?;
            if let Some(task_id) = &state.current_task_id {
                if let Some(task) = tasks.iter_mut().find(|t| &t.id == task_id) {
                    task.status = crate::types::TaskStatus::Completed;
                }
            }
            save_tasks(&task_file, &tasks)?;
        }
        Err(e) => {
            println!("[wreck-it] failed to merge PR #{}: {}", pr_number, e);
            state.memory.push(format!(
                "iteration {}: merge failed for PR #{}: {}",
                state.iteration, pr_number, e,
            ));
            // Stay in NeedsVerification to retry on the next invocation.
        }
    }

    Ok(())
}
