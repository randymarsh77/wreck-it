mod agent;
mod cli;
mod ralph_loop;
mod task_manager;
mod tui;
mod types;

use anyhow::Result;
use clap::Parser;
use cli::{Cli, Commands};
use ralph_loop::RalphLoop;
use std::env;
use tui::TuiApp;
use types::{Config, Task, TaskStatus};

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
        } => {
            // Get API token from argument or environment variable
            let token = api_token.or_else(|| env::var("COPILOT_API_TOKEN").ok());

            let config = Config {
                max_iterations,
                task_file,
                work_dir,
                api_endpoint,
                api_token: token,
            };

            let ralph_loop = RalphLoop::new(config);
            let mut app = TuiApp::new(ralph_loop);
            app.run().await?;
        }

        Commands::Init { output } => {
            // Create a sample task file
            let sample_tasks = vec![
                Task {
                    id: "1".to_string(),
                    description: "First task - implement feature X".to_string(),
                    status: TaskStatus::Pending,
                },
                Task {
                    id: "2".to_string(),
                    description: "Second task - add tests for feature X".to_string(),
                    status: TaskStatus::Pending,
                },
                Task {
                    id: "3".to_string(),
                    description: "Third task - update documentation".to_string(),
                    status: TaskStatus::Pending,
                },
            ];

            task_manager::save_tasks(&output, &sample_tasks)?;
            println!("Created sample task file at: {}", output.display());
        }
    }

    Ok(())
}
