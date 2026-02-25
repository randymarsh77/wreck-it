use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::headless_state::{load_headless_state, save_headless_state, AgentPhase, HeadlessState};
use crate::ralph_loop::RalphLoop;
use crate::task_manager::load_tasks;
use crate::types::Config;
use anyhow::{Context, Result};
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Run wreck-it in headless mode.
///
/// This is designed for CI environments (e.g. a cron-triggered GitHub Actions
/// workflow).  Each invocation performs a single iteration of the ralph loop
/// and persists state so subsequent cron runs can pick up where we left off.
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
            run_agent_working(&mut state)?;
        }
        AgentPhase::NeedsVerification => {
            run_needs_verification(&config, &headless_cfg, &mut state, &work_dir).await?;
        }
        AgentPhase::Completed => {
            println!("[wreck-it] previous task completed, advancing to next trigger");
            state.phase = AgentPhase::NeedsTrigger;
            state.current_task_id = None;
            state.pr_number = None;
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

/// Phase: NeedsTrigger – pick the next task and run one ralph loop iteration.
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

    println!(
        "[wreck-it] triggering agent for task {}: {}",
        pending_task.id, pending_task.description
    );

    // Run one iteration of the ralph loop
    let mut ralph = RalphLoop::new(config.clone());
    ralph.initialize()?;
    let _should_continue = ralph.run_iteration().await?;

    // After the iteration, check the task status
    state.memory.push(format!(
        "iteration {}: triggered task {}",
        state.iteration, pending_task.id
    ));
    state.phase = AgentPhase::NeedsVerification;

    Ok(())
}

/// Phase: AgentWorking – the cloud agent is still processing; nothing to do
/// this invocation.
fn run_agent_working(state: &mut HeadlessState) -> Result<()> {
    println!(
        "[wreck-it] agent is still working on task {:?}, will check again next run",
        state.current_task_id
    );
    // Transition to verification so the next invocation checks the result.
    state.phase = AgentPhase::NeedsVerification;
    Ok(())
}

/// Phase: NeedsVerification – run the verification command to check the agent's
/// work.
async fn run_needs_verification(
    config: &Config,
    headless_cfg: &HeadlessConfig,
    state: &mut HeadlessState,
    work_dir: &Path,
) -> Result<()> {
    let verify_cmd = headless_cfg
        .verify_command
        .as_deref()
        .or(config.verification_command.as_deref());

    if let Some(cmd) = verify_cmd {
        println!("[wreck-it] running verification: {}", cmd);
        let output = if cfg!(target_os = "windows") {
            std::process::Command::new("cmd")
                .args(["/C", cmd])
                .current_dir(work_dir)
                .output()
        } else {
            std::process::Command::new("sh")
                .args(["-c", cmd])
                .current_dir(work_dir)
                .output()
        };

        match output {
            Ok(out) if out.status.success() => {
                println!("[wreck-it] verification passed");
                state.phase = AgentPhase::Completed;
                state.memory.push(format!(
                    "iteration {}: verification passed for task {:?}",
                    state.iteration, state.current_task_id
                ));
            }
            Ok(out) => {
                let stderr = String::from_utf8_lossy(&out.stderr);
                println!("[wreck-it] verification failed: {}", stderr);
                state.memory.push(format!(
                    "iteration {}: verification failed for task {:?}",
                    state.iteration, state.current_task_id
                ));
                // Go back to trigger to retry
                state.phase = AgentPhase::NeedsTrigger;
            }
            Err(e) => {
                println!("[wreck-it] verification command error: {}", e);
                state.phase = AgentPhase::NeedsTrigger;
            }
        }
    } else {
        println!("[wreck-it] no verification command configured, marking complete");
        state.phase = AgentPhase::Completed;
    }

    Ok(())
}
