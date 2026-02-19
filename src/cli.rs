use crate::types::ModelProvider;
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "wreck-it")]
#[command(about = "A TUI agent harness for Ralph Wiggum loops", long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand)]
pub enum Commands {
    /// Run the Ralph Wiggum loop with TUI
    Run {
        /// Path to the task file
        #[arg(short, long)]
        task_file: Option<PathBuf>,

        /// Maximum number of iterations
        #[arg(short, long)]
        max_iterations: Option<usize>,

        /// Working directory
        #[arg(short, long)]
        work_dir: Option<PathBuf>,

        /// GitHub Copilot API endpoint
        #[arg(long)]
        api_endpoint: Option<String>,

        /// GitHub Copilot API token (can also be set via COPILOT_API_TOKEN env var)
        #[arg(long)]
        api_token: Option<String>,

        /// Model provider
        #[arg(long, value_enum)]
        model_provider: Option<ModelProvider>,

        /// Shell command/script used to verify completion after each task
        #[arg(long)]
        verify_command: Option<String>,
    },

    /// Initialize a new task file
    Init {
        /// Path to create the task file
        #[arg(short, long, default_value = "tasks.json")]
        output: PathBuf,
    },
}
