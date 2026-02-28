mod agent;
mod cli;
mod cloud_agent;
mod config_manager;
mod headless;
mod headless_config;
mod headless_state;
mod ralph_loop;
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
    AgentRole, ModelProvider, Task, TaskStatus, DEFAULT_COPILOT_ENDPOINT,
    DEFAULT_GITHUB_MODELS_ENDPOINT, DEFAULT_LLAMA_ENDPOINT,
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
            // Create a sample task file
            let sample_tasks = vec![
                Task {
                    id: "1".to_string(),
                    description: "First task - implement feature X".to_string(),
                    status: TaskStatus::Pending,
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    role: AgentRole::default(),
                },
                Task {
                    id: "2".to_string(),
                    description: "Second task - add tests for feature X".to_string(),
                    status: TaskStatus::Pending,
                    phase: 1,
                    depends_on: vec![],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    role: AgentRole::default(),
                },
                Task {
                    id: "3".to_string(),
                    description: "Third task - update documentation".to_string(),
                    status: TaskStatus::Pending,
                    phase: 2,
                    depends_on: vec!["1".to_string(), "2".to_string()],
                    priority: 0,
                    complexity: 1,
                    failed_attempts: 0,
                    last_attempt_at: None,
                    role: AgentRole::default(),
                },
            ];

            task_manager::save_tasks(&output, &sample_tasks)?;
            println!("Created sample task file at: {}", output.display());
        }
    }

    Ok(())
}
