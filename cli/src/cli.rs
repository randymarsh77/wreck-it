use crate::types::{EvaluationMode, ModelProvider};
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

        /// Shell command/script used to verify completion after each task (trusted input only)
        #[arg(long)]
        verify_command: Option<String>,

        /// Evaluation mode: "command" (default) runs a shell command, "agent-file"
        /// asks an agent to evaluate completeness and write a marker file.
        #[arg(long, value_enum)]
        evaluation_mode: Option<EvaluationMode>,

        /// Prompt describing task completeness for the evaluation agent
        /// (used when --evaluation-mode=agent-file)
        #[arg(long)]
        completeness_prompt: Option<String>,

        /// Path to the marker file the evaluation agent writes when the task
        /// is complete (used when --evaluation-mode=agent-file)
        #[arg(long)]
        completion_marker_file: Option<PathBuf>,

        /// Run in headless mode (no TUI, for CI environments)
        #[arg(long)]
        headless: bool,

        /// Maximum number of critic-actor reflection rounds (0 = disabled, default 2)
        #[arg(long)]
        reflection_rounds: Option<u8>,

        /// Number of consecutive failures before triggering adaptive re-planning
        /// (0 = disabled, default 2)
        #[arg(long)]
        replan_threshold: Option<u32>,

        /// Named ralph context to use (from repo config `[[ralphs]]`).
        /// When set, task file and state file paths are taken from the
        /// matching ralph entry in `.wreck-it/config.toml`.
        /// Use `--ralph all` to run every ralph in the config sequentially
        /// (headless mode only).
        #[arg(long)]
        ralph: Option<String>,

        /// Natural-language goal: when provided, a task plan is generated via
        /// the configured LLM before the loop starts and written to the task
        /// file (overwriting any existing tasks).
        #[arg(long)]
        goal: Option<String>,
    },

    /// Generate a structured task plan from a natural-language goal using the
    /// configured LLM and write it to the state worktree as a new ralph context.
    Plan {
        /// Natural-language goal to plan tasks for (required)
        #[arg(short, long)]
        goal: String,

        /// Name for the ralph context.  Defaults to a slug derived from the goal.
        /// If a ralph with this name already exists the task file is overwritten.
        #[arg(short, long)]
        ralph: Option<String>,

        /// Path to write the generated task file (relative to state root,
        /// default derived from ralph name)
        #[arg(short, long)]
        output: Option<PathBuf>,

        /// GitHub Copilot API endpoint
        #[arg(long)]
        api_endpoint: Option<String>,

        /// API token (can also be set via COPILOT_API_TOKEN or GITHUB_TOKEN env vars)
        #[arg(long)]
        api_token: Option<String>,

        /// Model provider
        #[arg(long, value_enum)]
        model_provider: Option<ModelProvider>,
    },

    /// Initialize a new task file
    Init {
        /// Path to create the task file
        #[arg(short, long, default_value = "tasks.json")]
        output: PathBuf,
    },

    /// Display provenance records for a specific task
    Provenance {
        /// Task ID whose provenance records should be displayed
        #[arg(short, long)]
        task: String,

        /// Working directory to look for .wreck-it-provenance/ records
        #[arg(short, long)]
        work_dir: Option<PathBuf>,
    },

    /// Manage built-in task templates
    Template {
        #[command(subcommand)]
        action: TemplateAction,
    },

    /// Export the full run provenance as an openclaw-compatible JSON document
    ExportOpenclaw {
        /// Path to the task file to read (default: tasks.json)
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,

        /// Working directory containing .wreck-it-provenance/ and
        /// .wreck-it-artefacts.json
        #[arg(short, long)]
        work_dir: Option<PathBuf>,

        /// Human-readable name for the workflow in the export (default: "wreck-it run")
        #[arg(long, default_value = "wreck-it run")]
        workflow_name: String,

        /// Path to write the openclaw JSON document (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },
}

#[derive(Subcommand)]
pub enum TemplateAction {
    /// List available built-in templates
    List,

    /// Apply a built-in template to the current project
    Apply {
        /// Name of the template to apply (e.g. "engineering-team")
        name: String,
    },
}
