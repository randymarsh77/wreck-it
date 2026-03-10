use crate::graph::GraphFormat;
use crate::types::{AgentRole, EvaluationMode, ModelProvider, TaskStatus};
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
#[allow(clippy::large_enum_variant)]
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

        /// Maximum number of autonomous continuation steps when using
        /// `--model-provider copilot-autopilot`.  Maps to Copilot CLI's
        /// `--max-autopilot-continues` flag.  Default is unlimited.
        #[arg(long)]
        max_autopilot_continues: Option<u32>,

        /// Webhook URLs to notify on task status transitions (can be specified
        /// multiple times).
        #[arg(long = "notify-webhook", value_name = "URL")]
        notify_webhooks: Vec<String>,

        /// Enable GitHub Issues integration: open an issue when a task moves to
        /// InProgress and close it when it reaches Completed or Failed.
        #[arg(long = "github-issues")]
        github_issues: bool,

        /// GitHub repository for Issues integration in `owner/repo` format
        /// (e.g. `acme/my-project`).  Required when --github-issues is set.
        #[arg(long = "github-repo", value_name = "OWNER/REPO")]
        github_repo: Option<String>,

        /// GitHub personal-access token or fine-grained token with `issues: write`
        /// permission.  Falls back to the GITHUB_TOKEN environment variable when
        /// not provided.
        #[arg(long = "github-token", value_name = "TOKEN")]
        github_token: Option<String>,

        /// Maximum cumulative estimated API cost (USD) for the entire run.
        /// The loop aborts after any iteration where this limit is exceeded.
        /// Leave unset to impose no budget limit.
        #[arg(long = "max-cost", value_name = "USD")]
        max_cost_usd: Option<f64>,

        /// Per-task or per-role working directory overrides for multi-repository
        /// orchestration.  Specify as `ROLE_OR_ID=PATH` pairs (may be repeated).
        /// When a task's id or role matches a key, the agent uses that path
        /// instead of the top-level --work-dir.  Example:
        ///   --work-dir-map frontend=/home/user/my-frontend
        ///   --work-dir-map backend=/home/user/my-backend
        #[arg(long = "work-dir-map", value_name = "ROLE_OR_ID=PATH", number_of_values = 1)]
        work_dir_map: Vec<String>,
    },

    /// Generate a structured task plan from a natural-language goal using the
    /// configured LLM and write it to the state worktree as a new ralph context.
    Plan {
        /// Natural-language goal to plan tasks for.
        /// Mutually exclusive with --goal-file; exactly one must be provided.
        #[arg(short, long, conflicts_with = "goal_file")]
        goal: Option<String>,

        /// Path to a file containing the natural-language goal.
        /// Mutually exclusive with --goal; exactly one must be provided.
        #[arg(long, conflicts_with = "goal")]
        goal_file: Option<PathBuf>,

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

        /// Use a cloud agent to build the plan instead of the local LLM.
        /// Creates a GitHub issue with the goal and assigns Copilot to
        /// generate the plan as a file in `.wreck-it/plans/`.
        #[arg(long)]
        cloud: bool,
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

    /// Install wreck-it into a project: creates .wreck-it config directory
    /// with the engineering-team template and adds ralph.yml and plan.yml
    /// GitHub Actions workflows.
    Install {
        /// Target directory to install into (default: current directory)
        #[arg(short, long)]
        work_dir: Option<PathBuf>,
    },

    /// Scan open PRs (and optionally the default branch) for failing CI
    /// checks and comment `@copilot` to request fixes.
    Unstuck {
        /// Working directory (defaults to current directory)
        #[arg(short, long)]
        work_dir: Option<PathBuf>,
    },

    /// Scan open PRs for merge conflicts with the base branch and resolve
    /// them.  Uses a cloud coding agent by default; pass `--backend cli` to
    /// merge locally and push.
    Merge {
        /// Working directory (defaults to current directory)
        #[arg(short, long)]
        work_dir: Option<PathBuf>,

        /// Backend to use for conflict resolution: "cloud_agent" (default)
        /// or "cli".
        #[arg(long, default_value = "cloud_agent")]
        backend: String,
    },

    /// Export the task dependency graph in Mermaid or GraphViz DOT format.
    Graph {
        /// Path to the task file to read (default: tasks.json)
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,

        /// Output format: mermaid (default) or dot
        #[arg(short, long, value_enum, default_value = "mermaid")]
        format: GraphFormat,

        /// Path to write the graph output (default: stdout)
        #[arg(short, long)]
        output: Option<PathBuf>,
    },

    // ── `wreck-it tasks` sub-command family ────────────────────────────────
    //
    // Design overview
    // ───────────────
    // The `tasks` sub-command family lets users manage task JSON files from
    // the CLI without editing raw JSON by hand.  All four sub-commands operate
    // on a single task file (defaulting to `tasks.json` in the current
    // directory; overridable with `--task-file`).
    //
    // ## Sub-commands
    //
    // ### `tasks list [--task-file] [--status <status>]`
    // Reads the task file and prints a plain-text table with columns:
    //   ID | STATUS | ROLE | PHASE | PRIORITY | DEPENDS_ON
    // The optional `--status` filter narrows output to tasks whose `status`
    // matches the given value (pending, in-progress, completed, failed).
    //
    // ### `tasks add --id <id> --description <desc> [options]`
    // Constructs a new `Task` from the supplied flags and appends it to the
    // file via `task_manager::append_task`, which validates uniqueness and
    // circular-dependency constraints before writing.
    //
    // ### `tasks set-status --id <id> --status <status>`
    // Finds the task with the matching ID, updates its `status` field, and
    // rewrites the file.  Exits with an error when the ID is not found.
    //
    // ### `tasks validate [--task-file]`
    // Performs a structural audit of the task file:
    //   1. Duplicate task IDs.
    //   2. `depends_on` references that point to non-existent task IDs.
    //   3. Circular dependencies (using `graph::detect_cycles`).
    // Prints a human-readable report and exits with a non-zero code when any
    // issue is found.
    /// Manage task files interactively from the CLI.
    Tasks {
        #[command(subcommand)]
        action: TasksAction,
    },
}

/// Sub-commands for `wreck-it tasks`.
#[derive(Subcommand)]
pub enum TasksAction {
    /// List all tasks with their status and role.
    ///
    /// Prints a table of every task in the file.  Use `--status` to filter by
    /// lifecycle status (pending, in-progress, completed, failed).
    List {
        /// Path to the task file (default: tasks.json)
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,

        /// Show only tasks with the given status
        #[arg(long, value_enum)]
        status: Option<TaskStatus>,
    },

    /// Append a new task to the task file.
    ///
    /// The task ID must be unique.  If `--depends-on` IDs are supplied they
    /// must not form a cycle with the existing tasks.
    Add {
        /// Path to the task file (default: tasks.json)
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,

        /// Unique task identifier
        #[arg(long)]
        id: String,

        /// Human-readable task description
        #[arg(short, long)]
        description: String,

        /// Agent role (ideas | implementer | evaluator; default: implementer)
        #[arg(long, value_enum, default_value = "implementer")]
        role: AgentRole,

        /// Execution phase – tasks in lower phases run first (default: 1)
        #[arg(long, default_value_t = 1)]
        phase: u32,

        /// Scheduling priority – higher values run sooner (default: 0)
        #[arg(long, default_value_t = 0)]
        priority: u32,

        /// Comma-separated list of task IDs this task depends on
        #[arg(long, value_delimiter = ',')]
        depends_on: Vec<String>,
    },

    /// Update the status of an existing task in place.
    SetStatus {
        /// Path to the task file (default: tasks.json)
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,

        /// ID of the task to update
        #[arg(long)]
        id: String,

        /// New status value
        #[arg(long, value_enum)]
        status: TaskStatus,
    },

    /// Validate a task file for structural correctness.
    ///
    /// Checks for duplicate IDs, unresolved `depends_on` references, and
    /// circular dependencies.  Exits with a non-zero code when any issue is
    /// found.
    Validate {
        /// Path to the task file (default: tasks.json)
        #[arg(short, long, default_value = "tasks.json")]
        task_file: PathBuf,
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
