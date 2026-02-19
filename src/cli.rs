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
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,

        /// Maximum number of iterations
        #[arg(short, long, default_value = "100")]
        max_iterations: usize,

        /// Working directory
        #[arg(short, long, default_value = ".")]
        work_dir: PathBuf,

        /// GitHub Copilot API endpoint
        #[arg(long, default_value = "https://api.githubcopilot.com")]
        api_endpoint: String,

        /// GitHub Copilot API token (can also be set via COPILOT_API_TOKEN env var)
        #[arg(long)]
        api_token: Option<String>,
    },

    /// Initialize a new task file
    Init {
        /// Path to create the task file
        #[arg(short, long, default_value = "tasks.json")]
        output: PathBuf,
    },
}
