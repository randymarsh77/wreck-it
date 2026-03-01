mod agent;
mod agent_memory;
mod cli;
mod cloud_agent;
mod config_manager;
mod headless;
mod headless_config;
mod headless_state;
#[cfg(test)]
mod integration_eval;
mod ralph_loop;
mod state_worktree;
mod task_manager;
mod tui;
mod types;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use config_manager::{load_user_config, save_user_config};
use ralph_loop::RalphLoop;
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
        } => {
            let mut config = load_user_config().unwrap_or_default();
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

            if headless {
                headless::run_headless(config).await?;
            } else {
                let ralph_loop = RalphLoop::new(config);
                let mut app = TuiApp::new(ralph_loop);
                app.run().await?;
            }
        }

        Commands::Init { output } => {
            // Set up the state worktree: create the orphan branch (if needed)
            // and check out a worktree at .wreck-it/state.
            let work_dir = std::env::current_dir()?;
            let state_branch = state_worktree::DEFAULT_STATE_BRANCH;
            let state_dir = state_worktree::ensure_state_worktree(&work_dir, state_branch)?;

            println!(
                "Initialized state worktree at {} (branch '{}')",
                state_dir.display(),
                state_branch,
            );

            // Write sample task file into the state worktree.
            let task_path = state_dir.join(&output);
            let sample_tasks = vec![
                Task {
                    id: "1".to_string(),
                    description: "First task - implement feature X".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                },
                Task {
                    id: "2".to_string(),
                    description: "Second task - add tests for feature X".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                },
                Task {
                    id: "3".to_string(),
                    description: "Third task - update documentation".to_string(),
                    status: TaskStatus::Pending,
                    role: types::AgentRole::default(),
                    phase: 2,
                    depends_on: vec!["1".to_string(), "2".to_string()],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
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
                    state_branch,
                    output.display(),
                );
                std::fs::write(&config_path, default_toml)?;
                println!("Created default config at: {}", config_path.display());
            }

            // Commit everything into the state branch.
            if let Ok(true) =
                state_worktree::commit_state_worktree(&work_dir, "wreck-it: init state")
            {
                println!("Committed initial state to branch '{}'", state_branch);
            }
        }
    }

    Ok(())
}
