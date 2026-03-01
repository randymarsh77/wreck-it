mod agent;
mod agent_memory;
mod artefact_store;
mod cli;
mod cloud_agent;
mod config_manager;
mod gastown_client;
mod headless;
mod headless_config;
mod headless_state;
#[cfg(test)]
mod integration_eval;
mod planner;
mod provenance;
mod ralph_loop;
mod replanner;
mod repo_config;
mod state_worktree;
mod task_manager;
mod tui;
mod types;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use config_manager::{load_user_config, save_user_config};
use ralph_loop::RalphLoop;
use repo_config::{
    is_interactive, is_state_uninitialized, load_repo_config, prompt_with_default,
    repo_config_path, save_repo_config, RepoConfig,
};
use std::env;
use tui::TuiApp;
use types::{
    ModelProvider, Task, TaskStatus, DEFAULT_COPILOT_ENDPOINT, DEFAULT_GITHUB_MODELS_ENDPOINT,
    DEFAULT_LLAMA_ENDPOINT,
};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive(tracing::Level::INFO.into()),
        )
        .init();

    let cli = Cli::parse();

    match cli.command {
        Commands::Run {
            task_file,
            max_iterations,
            work_dir,
            api_endpoint,
            api_token,
            model_provider,
            verify_command,
            evaluation_mode,
            completeness_prompt,
            completion_marker_file,
            headless,
            ralph,
            goal,
            reflection_rounds,
            replan_threshold,
        } => {
            // Determine work directory early so we can look for the repo config.
            let resolved_work_dir = work_dir
                .clone()
                .unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

            // Check for repo-level config.
            let repo_cfg = match load_repo_config(&resolved_work_dir)? {
                Some(cfg) => cfg,
                None => {
                    println!("wreck-it config not found; run wreck-it init");
                    return Ok(());
                }
            };

            // If a named ralph was requested, resolve its task and state paths.
            let ralph_overrides = match &ralph {
                Some(name) => match repo_config::find_ralph(&repo_cfg, name) {
                    Some(rc) => Some(rc.clone()),
                    None => {
                        let available: Vec<&str> =
                            repo_cfg.ralphs.iter().map(|r| r.name.as_str()).collect();
                        println!(
                            "ralph '{}' not found in repo config. available: {:?}",
                            name, available,
                        );
                        return Ok(());
                    }
                },
                None => None,
            };

            // Set up the state worktree.
            let state_dir =
                state_worktree::ensure_state_worktree(&resolved_work_dir, &repo_cfg.state_branch)?;

            // If state is empty, nothing to do.
            if is_state_uninitialized(&state_dir) {
                println!("wreck-it state is empty; nothing to do");
                return Ok(());
            }

            let mut config = load_user_config().unwrap_or_default();

            // Apply ralph overrides before explicit CLI flags so that
            // `--task-file` can still override the ralph default.
            if let Some(ref rc) = ralph_overrides {
                config.task_file = rc.task_file.clone().into();
            }

            if let Some(task_file) = task_file {
                config.task_file = task_file;
            }
            if let Some(max_iterations) = max_iterations {
                config.max_iterations = max_iterations;
            }
            if let Some(work_dir) = work_dir {
                config.work_dir = work_dir;
            }
            if let Some(api_endpoint) = api_endpoint {
                config.api_endpoint = api_endpoint;
            }
            if let Some(model_provider) = model_provider {
                config.model_provider = model_provider;
            }
            if let Some(verify_command) = verify_command {
                config.verification_command = Some(verify_command);
            }
            if let Some(evaluation_mode) = evaluation_mode {
                config.evaluation_mode = evaluation_mode;
            }
            if let Some(completeness_prompt) = completeness_prompt {
                config.completeness_prompt = Some(completeness_prompt);
            }
            if let Some(completion_marker_file) = completion_marker_file {
                config.completion_marker_file = completion_marker_file;
            }
            if let Some(reflection_rounds) = reflection_rounds {
                config.reflection_rounds = reflection_rounds;
            }
            if let Some(replan_threshold) = replan_threshold {
                config.replan_threshold = replan_threshold;
            }
            if config.model_provider == ModelProvider::Llama
                && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
            {
                config.api_endpoint = DEFAULT_LLAMA_ENDPOINT.to_string();
            }
            if config.model_provider == ModelProvider::GithubModels
                && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
            {
                config.api_endpoint = DEFAULT_GITHUB_MODELS_ENDPOINT.to_string();
            }
            config.api_token = api_token
                .or(config.api_token)
                .or_else(|| env::var("COPILOT_API_TOKEN").ok())
                .or_else(|| env::var("GITHUB_TOKEN").ok());

            save_user_config(&config)?;

            // Optional pre-loop planning phase: generate tasks from a goal.
            if let Some(ref goal_str) = goal {
                println!("Generating task plan for goal: {}", goal_str);
                let task_path = state_dir.join(&config.task_file);
                if task_path.exists() {
                    println!(
                        "Warning: existing task file '{}' will be overwritten with the generated plan",
                        task_path.display()
                    );
                }
                let task_planner = planner::TaskPlanner::new(
                    config.model_provider.clone(),
                    config.api_endpoint.clone(),
                    config.api_token.clone(),
                );
                let planned_tasks = task_planner.generate_task_plan(goal_str).await?;
                println!("Generated {} task(s)", planned_tasks.len());
                task_manager::save_tasks(&task_path, &planned_tasks)?;
                println!("Task plan written to {}", task_path.display());
            }

            if headless {
                headless::run_headless(config, ralph_overrides.as_ref()).await?;
            } else {
                let ralph_loop = RalphLoop::new(config);
                let mut app = TuiApp::new(ralph_loop);
                app.run().await?;
            }

            // Commit any pending state changes and push.
            let _ = state_worktree::commit_and_push_state(
                &resolved_work_dir,
                &repo_cfg.state_branch,
                "wreck-it: update state",
            );
        }

        Commands::Plan {
            goal,
            output,
            api_endpoint,
            api_token,
            model_provider,
        } => {
            let mut config = load_user_config().unwrap_or_default();

            if let Some(api_endpoint) = api_endpoint {
                config.api_endpoint = api_endpoint;
            }
            if let Some(model_provider) = model_provider {
                config.model_provider = model_provider;
            }
            if config.model_provider == ModelProvider::Llama
                && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
            {
                config.api_endpoint = DEFAULT_LLAMA_ENDPOINT.to_string();
            }
            if config.model_provider == ModelProvider::GithubModels
                && config.api_endpoint == DEFAULT_COPILOT_ENDPOINT
            {
                config.api_endpoint = DEFAULT_GITHUB_MODELS_ENDPOINT.to_string();
            }
            config.api_token = api_token
                .or(config.api_token)
                .or_else(|| env::var("COPILOT_API_TOKEN").ok())
                .or_else(|| env::var("GITHUB_TOKEN").ok());

            println!("Generating task plan for goal: {}", goal);
            let task_planner = planner::TaskPlanner::new(
                config.model_provider.clone(),
                config.api_endpoint.clone(),
                config.api_token.clone(),
            );
            let tasks = task_planner.generate_task_plan(&goal).await?;
            println!("Generated {} task(s)", tasks.len());

            task_manager::save_tasks(&output, &tasks)?;
            println!("Task plan written to {}", output.display());
        }

        Commands::Init { output } => {
            let work_dir = std::env::current_dir()?;
            let interactive = is_interactive();

            // ── Phase 1: Repo-level config ──────────────────────────────
            let repo_cfg = match load_repo_config(&work_dir)? {
                Some(cfg) => {
                    println!("Found existing wreck-it configuration");
                    cfg
                }
                None => {
                    let cfg = if interactive {
                        let branch = prompt_with_default(
                            "State branch",
                            state_worktree::DEFAULT_STATE_BRANCH,
                        );
                        let root =
                            prompt_with_default("State root directory", repo_config::CONFIG_DIR);
                        RepoConfig {
                            state_branch: branch,
                            state_root: root,
                            ralphs: vec![],
                        }
                    } else {
                        RepoConfig::default()
                    };

                    save_repo_config(&work_dir, &cfg)?;

                    // Commit the config to the current branch.
                    let cfg_path = repo_config_path(&work_dir);
                    let cfg_path_str = cfg_path
                        .to_str()
                        .ok_or_else(|| anyhow::anyhow!("config path contains invalid UTF-8"))?;
                    state_worktree::git_cmd(&work_dir, &["add", cfg_path_str])?;
                    state_worktree::git_cmd(
                        &work_dir,
                        &["commit", "-m", "Initialize wreck-it configuration"],
                    )?;

                    println!(
                        "Initialized wreck-it configuration (branch='{}', root='{}')",
                        cfg.state_branch, cfg.state_root,
                    );
                    cfg
                }
            };

            // ── Phase 2: State worktree ─────────────────────────────────
            let state_dir =
                state_worktree::ensure_state_worktree(&work_dir, &repo_cfg.state_branch)?;

            println!(
                "State worktree at {} (branch '{}')",
                state_dir.display(),
                repo_cfg.state_branch,
            );

            // ── Phase 3: Task creation ──────────────────────────────────
            let task_path = state_dir.join(&output);

            // If tasks already exist, we are already initialized.
            if task_path.exists() {
                let tasks = task_manager::load_tasks(&task_path)?;
                println!("wreck-it is initialized");
                println!(
                    "  config: state_branch={}, state_root={}",
                    repo_cfg.state_branch, repo_cfg.state_root,
                );
                println!("  tasks: {} task(s) in {}", tasks.len(), output.display());
                return Ok(());
            }

            // Non-interactive: don't create tasks (empty state is fine).
            if !interactive {
                return Ok(());
            }

            // Interactive: create sample tasks.
            let sample_tasks = vec![
                Task {
                    id: "1".to_string(),
                    description: "First task - implement feature X".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                },
                Task {
                    id: "2".to_string(),
                    description: "Second task - add tests for feature X".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                },
                Task {
                    id: "3".to_string(),
                    description: "Third task - update documentation".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    kind: types::TaskKind::default(),
                    cooldown_seconds: None,
                    phase: 2,
                    depends_on: vec!["1".to_string(), "2".to_string()],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    inputs: vec![],
                    outputs: vec![],
                    runtime: types::TaskRuntime::default(),
                },
            ];

            task_manager::save_tasks(&task_path, &sample_tasks)?;
            println!("Created sample task file at: {}", task_path.display());

            // Write a default .wreck-it.toml config into the state worktree.
            let config_path = state_dir.join(".wreck-it.toml");
            if !config_path.exists() {
                let default_toml = format!(
                    "# wreck-it headless configuration\n\
                     # This file lives on the state branch ({}).\n\
                     \n\
                     task_file = \"{}\"\n\
                     state_file = \".wreck-it-state.json\"\n\
                     max_iterations = 100\n",
                    repo_cfg.state_branch,
                    output.display(),
                );
                std::fs::write(&config_path, default_toml)?;
                println!("Created default config at: {}", config_path.display());
            }

            // Commit everything into the state branch.
            if let Ok(true) =
                state_worktree::commit_state_worktree(&work_dir, "wreck-it: init state")
            {
                println!(
                    "Committed initial state to branch '{}'",
                    repo_cfg.state_branch,
                );
            }
        }

        Commands::Provenance { task, work_dir } => {
            let resolved_work_dir =
                work_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());
            let records = provenance::load_provenance_records(&task, &resolved_work_dir)?;
            if records.is_empty() {
                println!("No provenance records found for task '{}'.", task);
            } else {
                println!("Provenance records for task '{}':", task);
                for record in &records {
                    println!("  timestamp : {}", record.timestamp);
                    println!("  outcome   : {}", record.outcome);
                    println!("  model     : {}", record.model);
                    println!("  agent_role: {:?}", record.agent_role);
                    println!("  prompt_hash: {}", record.prompt_hash);
                    println!("  diff_hash : {}", record.git_diff_hash);
                    if !record.tool_calls.is_empty() {
                        println!("  tool_calls: {}", record.tool_calls.join(", "));
                    }
                    println!();
                }
            }
        }
    }

    Ok(())
}
