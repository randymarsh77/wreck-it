use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::PathBuf;

// Re-export shared types from wreck-it-core so that the rest of the crate
// can continue to use `crate::types::Task`, etc. without changes.
pub use wreck_it_core::types::{
    AgentRole, ArtefactKind, Task, TaskArtefact, TaskKind, TaskRuntime, TaskStatus,
};

pub const DEFAULT_COPILOT_ENDPOINT: &str = "https://api.githubcopilot.com";
pub const DEFAULT_LLAMA_ENDPOINT: &str = "http://localhost:11434/v1";
pub const DEFAULT_GITHUB_MODELS_ENDPOINT: &str =
    "https://models.github.ai/inference/chat/completions";
pub const DEFAULT_LLAMA_MODEL: &str = "llama3.2";
pub const DEFAULT_GITHUB_MODELS_MODEL: &str = "anthropic/claude-opus-4.6";
pub const DEFAULT_GITHUB_MODELS_NAMING_MODEL: &str = "openai/gpt-4o-mini";
pub const LLAMA_PROVIDER_TYPE: &str = "openai";
pub const DEFAULT_COMPLETION_MARKER: &str = ".task-complete";
pub const DEFAULT_PRECONDITION_MARKER: &str = ".task-precondition-met";
pub const DEFAULT_REFLECTION_ROUNDS: u8 = 2;
pub const DEFAULT_REPLAN_THRESHOLD: u32 = 2;
pub const DEFAULT_AUTOPILOT_MODEL: &str = "copilot-autopilot";

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ModelProvider {
    Copilot,
    Llama,
    GithubModels,
    /// Use the Copilot CLI in autopilot mode (`copilot --autopilot --yolo -p`).
    ///
    /// This invokes the `copilot` binary as a subprocess with full autonomous
    /// permissions, letting it execute multi-step tasks (file edits, shell
    /// commands, git operations) without per-tool approval prompts.
    #[serde(alias = "copilot-autopilot")]
    #[value(alias = "copilot-autopilot")]
    CopilotAutopilot,
}

/// How task completeness is evaluated after the agent finishes work.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum, Default)]
#[serde(rename_all = "snake_case")]
pub enum EvaluationMode {
    /// Run a shell command and check its exit code (existing behaviour).
    #[default]
    Command,
    /// Ask an agent to evaluate completeness; it writes a marker file if done.
    AgentFile,
    /// Ask an agent to read the task description alongside the git diff and
    /// return a structured JSON verdict `{ passed: bool, score: u8,
    /// rationale: String }`.  The verdict is surfaced in TUI and logs.
    Semantic,
}

/// Result of a critic evaluation of a git diff against a task description.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CriticResult {
    /// Quality score from 0.0 (completely wrong) to 1.0 (perfect).
    pub score: f32,
    /// List of specific issues found in the implementation.
    pub issues: Vec<String>,
    /// Whether the critic approves the implementation as-is.
    pub approved: bool,
}

/// Configuration for the Ralph Wiggum loop
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    /// Maximum number of iterations before stopping
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,

    /// Path to the task file
    #[serde(default = "default_task_file")]
    pub task_file: PathBuf,

    /// Working directory for the agent
    #[serde(default = "default_work_dir")]
    pub work_dir: PathBuf,

    /// GitHub Copilot API endpoint
    #[serde(default = "default_api_endpoint")]
    pub api_endpoint: String,

    /// GitHub Copilot API token (optional, can be set via environment)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,

    /// Model provider
    #[serde(default = "default_model_provider")]
    pub model_provider: ModelProvider,

    /// Optional shell command to verify completion after each loop iteration (trusted input only)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verification_command: Option<String>,

    /// How completeness is evaluated after the agent finishes work.
    #[serde(default)]
    pub evaluation_mode: EvaluationMode,

    /// Prompt describing what "complete" means, fed to the evaluation agent
    /// when `evaluation_mode` is `AgentFile`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness_prompt: Option<String>,

    /// Path to the marker file that the evaluation agent writes when the task
    /// is deemed complete.  Checked (and removed) after the evaluation agent
    /// runs.
    #[serde(default = "default_completion_marker")]
    pub completion_marker_file: PathBuf,

    /// Maximum number of critic-actor reflection rounds after the actor
    /// completes a task.  A value of 0 disables reflection entirely.
    #[serde(default = "default_reflection_rounds")]
    pub reflection_rounds: u8,

    /// Number of consecutive task failures before the adaptive re-planner is
    /// invoked.  A value of 0 disables re-planning.
    #[serde(default = "default_replan_threshold")]
    pub replan_threshold: u32,

    /// Base URL of the gastown cloud agent service.  When absent, gastown
    /// integration is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gastown_endpoint: Option<String>,

    /// Authentication token for the gastown cloud agent service.  When absent,
    /// gastown integration is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub gastown_token: Option<String>,

    /// GitHub personal access token or OAuth token used for cloud plan
    /// generation (creating issues, assigning agents).  Can also be set via
    /// the `GITHUB_TOKEN` environment variable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_token: Option<String>,

    /// Maximum number of autonomous continuation steps the Copilot CLI agent
    /// may take per task when using the `CopilotAutopilot` model provider.
    /// Maps to `--max-autopilot-continues`.  `None` means unlimited.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_autopilot_continues: Option<u32>,

    /// List of URLs to notify via HTTP POST when a task changes status.
    /// Failures are logged as warnings and do not abort the loop.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notify_webhooks: Vec<String>,

    /// When `true`, a GitHub Issue is opened when a task moves to `InProgress`
    /// and closed when the task reaches `Completed` or `Failed`.
    /// Requires `github_repo` to be set and either `github_token` or the
    /// `GITHUB_TOKEN` environment variable to be available.
    #[serde(default)]
    pub github_issues_enabled: bool,

    /// GitHub repository in `owner/repo` format.  Used by the GitHub Issues
    /// integration to determine where issues are created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub github_repo: Option<String>,

    /// Maximum cumulative estimated cost in USD for a single run.
    ///
    /// When the [`crate::cost_tracker::CostTracker`] reports that the
    /// accumulated estimated spend has reached this threshold, the main loop
    /// aborts rather than starting the next task.  This prevents unintended
    /// runaway spending in long autonomous sessions.
    ///
    /// Only token usage reported by the GitHub Models HTTP path is tracked;
    /// Copilot SDK and Llama calls are not metered (they contribute $0.00
    /// to the estimate).  Leave `None` to impose no budget limit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost_usd: Option<f64>,

    /// Optional per-task or per-role working directory overrides.
    ///
    /// Maps a task id or role name (e.g. `"frontend"`, `"backend"`) to an
    /// absolute or relative path of a local git repository.  When a task's
    /// `id` (or `role`) matches a key in this map, the agent uses that path
    /// as its working directory instead of the top-level [`Config::work_dir`].
    ///
    /// This is the entry point for **multi-repository orchestration**: a
    /// single `wreck-it run` invocation can coordinate tasks that span
    /// multiple local git repositories by routing each task to the
    /// appropriate repo.  See `docs/multi-repo.md` for the full design.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub work_dirs: HashMap<String, String>,

    /// Optional path to a directory containing per-role system prompt template
    /// files and per-task overrides.  When set, the `prompt_loader` module
    /// resolves and injects custom prompts before each agent invocation,
    /// falling back to built-in defaults when no matching file is found.
    ///
    /// When `None`, downstream code uses `.wreck-it/prompts` as the conventional
    /// default directory (i.e. no automatic directory creation occurs; it only
    /// takes effect if the directory is present and the value is explicitly set).
    /// The value may be overridden at runtime via the `--prompt-dir` CLI flag.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_dir: Option<String>,
}

fn default_max_iterations() -> usize {
    100
}

fn default_task_file() -> PathBuf {
    PathBuf::from("tasks.json")
}

fn default_work_dir() -> PathBuf {
    PathBuf::from(".")
}

fn default_api_endpoint() -> String {
    DEFAULT_COPILOT_ENDPOINT.to_string()
}

fn default_model_provider() -> ModelProvider {
    ModelProvider::Copilot
}

fn default_completion_marker() -> PathBuf {
    PathBuf::from(DEFAULT_COMPLETION_MARKER)
}

fn default_reflection_rounds() -> u8 {
    DEFAULT_REFLECTION_ROUNDS
}

fn default_replan_threshold() -> u32 {
    DEFAULT_REPLAN_THRESHOLD
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            task_file: default_task_file(),
            work_dir: default_work_dir(),
            api_endpoint: default_api_endpoint(),
            api_token: None,
            model_provider: default_model_provider(),
            verification_command: None,
            evaluation_mode: EvaluationMode::default(),
            completeness_prompt: None,
            completion_marker_file: default_completion_marker(),
            reflection_rounds: default_reflection_rounds(),
            replan_threshold: default_replan_threshold(),
            gastown_endpoint: None,
            gastown_token: None,
            github_token: None,
            max_autopilot_continues: None,
            notify_webhooks: Vec::new(),
            github_issues_enabled: false,
            github_repo: None,
            max_cost_usd: None,
            work_dirs: HashMap::new(),
            prompt_dir: None,
        }
    }
}

/// State of the Ralph Wiggum loop
#[derive(Debug, Clone)]
pub struct LoopState {
    pub iteration: usize,
    pub max_iterations: usize,
    pub tasks: Vec<Task>,
    pub current_task: Option<usize>,
    pub running: bool,
    pub logs: Vec<String>,
    /// Number of consecutive task failures since the last success or re-plan.
    pub consecutive_failures: u32,
}

impl LoopState {
    pub fn new(max_iterations: usize) -> Self {
        Self {
            iteration: 0,
            max_iterations,
            tasks: Vec::new(),
            current_task: None,
            running: false,
            logs: Vec::new(),
            consecutive_failures: 0,
        }
    }

    pub fn add_log(&mut self, message: String) {
        self.logs
            .push(format!("[Iter {}] {}", self.iteration, message));
    }

    pub fn all_tasks_complete(&self) -> bool {
        !self.tasks.is_empty() && self.tasks.iter().all(|t| t.status == TaskStatus::Completed)
    }

    pub fn has_pending_tasks(&self) -> bool {
        self.tasks
            .iter()
            .any(|t| t.status == TaskStatus::Pending || t.status == TaskStatus::InProgress)
    }

    /// Return indices of all tasks in the lowest pending phase whose
    /// dependencies have been satisfied.  These tasks can be executed in
    /// parallel.
    ///
    /// This method is preserved for backward compatibility and use in tests.
    /// The main loop uses [`TaskScheduler::schedule`] for smarter ordering.
    #[cfg_attr(not(test), allow(dead_code))]
    pub fn ready_task_indices(&self) -> Vec<usize> {
        // Build a set of completed task IDs.
        let completed_ids: std::collections::HashSet<&str> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.id.as_str())
            .collect();

        // Find the minimum phase that still has pending tasks.
        let min_phase = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .map(|t| t.phase)
            .min();

        let min_phase = match min_phase {
            Some(p) => p,
            None => return Vec::new(),
        };

        self.tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.status == TaskStatus::Pending
                    && t.phase == min_phase
                    && t.depends_on
                        .iter()
                        .all(|dep| completed_ids.contains(dep.as_str()))
            })
            .map(|(i, _)| i)
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(id: &str, status: TaskStatus, phase: u32, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            priority: 0,
            complexity: 1,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
            system_prompt_override: None,
        }
    }

    #[test]
    fn ready_task_indices_returns_all_in_lowest_phase() {
        let mut state = LoopState::new(10);
        state.tasks = vec![
            make_task("a", TaskStatus::Pending, 1, vec![]),
            make_task("b", TaskStatus::Pending, 1, vec![]),
            make_task("c", TaskStatus::Pending, 2, vec!["a", "b"]),
        ];
        assert_eq!(state.ready_task_indices(), vec![0, 1]);
    }

    #[test]
    fn ready_task_indices_respects_dependencies() {
        let mut state = LoopState::new(10);
        state.tasks = vec![
            make_task("a", TaskStatus::Completed, 1, vec![]),
            make_task("b", TaskStatus::Pending, 1, vec![]),
            // Same phase as b, but depends on b which isn't done yet.
            make_task("c", TaskStatus::Pending, 1, vec!["b"]),
        ];
        // Only b is ready (c depends on b which is still pending).
        assert_eq!(state.ready_task_indices(), vec![1]);
    }

    #[test]
    fn ready_task_indices_advances_phase_when_earlier_done() {
        let mut state = LoopState::new(10);
        state.tasks = vec![
            make_task("a", TaskStatus::Completed, 1, vec![]),
            make_task("b", TaskStatus::Completed, 1, vec![]),
            make_task("c", TaskStatus::Pending, 2, vec!["a", "b"]),
        ];
        assert_eq!(state.ready_task_indices(), vec![2]);
    }

    #[test]
    fn ready_task_indices_empty_when_no_pending() {
        let mut state = LoopState::new(10);
        state.tasks = vec![make_task("a", TaskStatus::Completed, 1, vec![])];
        assert!(state.ready_task_indices().is_empty());
    }

    #[test]
    fn ready_task_indices_blocks_on_unmet_deps() {
        let mut state = LoopState::new(10);
        state.tasks = vec![
            make_task("a", TaskStatus::Failed, 1, vec![]),
            make_task("b", TaskStatus::Pending, 2, vec!["a"]),
        ];
        // b depends on a, but a is Failed (not Completed), so b is not ready.
        assert!(state.ready_task_indices().is_empty());
    }

    #[test]
    fn task_default_phase_is_one() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.phase, 1);
        assert!(task.depends_on.is_empty());
    }

    #[test]
    fn task_roundtrip_with_phase_and_deps() {
        let task = make_task("1", TaskStatus::Pending, 3, vec!["a", "b"]);
        let json = serde_json::to_string(&task).unwrap();
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.phase, 3);
        assert_eq!(loaded.depends_on, vec!["a", "b"]);
    }

    #[test]
    fn evaluation_mode_default_is_command() {
        assert_eq!(EvaluationMode::default(), EvaluationMode::Command);
    }

    #[test]
    fn config_default_has_evaluation_fields() {
        let config = Config::default();
        assert_eq!(config.evaluation_mode, EvaluationMode::Command);
        assert!(config.completeness_prompt.is_none());
        assert_eq!(
            config.completion_marker_file,
            PathBuf::from(DEFAULT_COMPLETION_MARKER)
        );
        assert_eq!(config.reflection_rounds, DEFAULT_REFLECTION_ROUNDS);
        assert_eq!(config.replan_threshold, DEFAULT_REPLAN_THRESHOLD);
    }

    #[test]
    fn config_reflection_rounds_roundtrip() {
        let mut config = Config::default();
        config.reflection_rounds = 5;
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.reflection_rounds, 5);
    }

    #[test]
    fn config_reflection_rounds_defaults_when_absent() {
        let json = r#"{"max_iterations":10}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert_eq!(config.reflection_rounds, DEFAULT_REFLECTION_ROUNDS);
        assert_eq!(config.replan_threshold, DEFAULT_REPLAN_THRESHOLD);
    }

    #[test]
    fn model_provider_github_models_roundtrip() {
        let config = Config {
            model_provider: ModelProvider::GithubModels,
            ..Config::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.model_provider, ModelProvider::GithubModels);
    }

    #[test]
    fn model_provider_copilot_autopilot_roundtrip() {
        let config = Config {
            model_provider: ModelProvider::CopilotAutopilot,
            max_autopilot_continues: Some(10),
            ..Config::default()
        };
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("copilotautopilot"));
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.model_provider, ModelProvider::CopilotAutopilot);
        assert_eq!(loaded.max_autopilot_continues, Some(10));
    }

    #[test]
    fn max_autopilot_continues_defaults_to_none() {
        let json = r#"{"max_iterations":10}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.max_autopilot_continues.is_none());
    }

    #[test]
    fn max_autopilot_continues_omitted_when_none() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("max_autopilot_continues"));
    }

    // ---- AgentRole tests ----

    #[test]
    fn task_role_defaults_to_implementer_when_absent() {
        // Backward compat: old task files have no "role" field.
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.role, AgentRole::Implementer);
    }

    #[test]
    fn task_role_implementer_is_omitted_in_serialization() {
        // The default role must NOT be written to the JSON (backward compat).
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("\"role\""), "default role should be omitted");
    }

    #[test]
    fn task_role_non_default_is_serialized() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.role = AgentRole::Ideas;
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"role\":\"ideas\""));
    }

    #[test]
    fn task_role_roundtrip_all_variants() {
        for role in [
            AgentRole::Ideas,
            AgentRole::Implementer,
            AgentRole::Evaluator,
        ] {
            let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
            task.role = role;
            let json = serde_json::to_string(&task).unwrap();
            let loaded: Task = serde_json::from_str(&json).unwrap();
            assert_eq!(loaded.role, role);
        }
    }

    #[test]
    fn agent_role_default_is_implementer() {
        assert_eq!(AgentRole::default(), AgentRole::Implementer);
    }

    // ---- TaskKind tests ----

    #[test]
    fn task_kind_default_is_milestone() {
        assert_eq!(TaskKind::default(), TaskKind::Milestone);
    }

    #[test]
    fn task_kind_defaults_to_milestone_when_absent() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.kind, TaskKind::Milestone);
        assert!(task.cooldown_seconds.is_none());
    }

    #[test]
    fn task_kind_milestone_is_omitted_in_serialization() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("\"kind\""), "default kind should be omitted");
    }

    #[test]
    fn task_kind_recurring_is_serialized() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.kind = TaskKind::Recurring;
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"kind\":\"recurring\""));
    }

    #[test]
    fn task_kind_recurring_roundtrip() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.kind = TaskKind::Recurring;
        task.cooldown_seconds = Some(3600);
        let json = serde_json::to_string(&task).unwrap();
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.kind, TaskKind::Recurring);
        assert_eq!(loaded.cooldown_seconds, Some(3600));
    }

    #[test]
    fn task_cooldown_seconds_omitted_when_none() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(!json.contains("cooldown_seconds"));
    }

    // ---- ArtefactKind / TaskArtefact tests ----

    #[test]
    fn artefact_kind_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&ArtefactKind::File).unwrap(),
            "\"file\""
        );
        assert_eq!(
            serde_json::to_string(&ArtefactKind::Json).unwrap(),
            "\"json\""
        );
        assert_eq!(
            serde_json::to_string(&ArtefactKind::Summary).unwrap(),
            "\"summary\""
        );
    }

    #[test]
    fn artefact_kind_roundtrip() {
        for kind in [
            ArtefactKind::File,
            ArtefactKind::Json,
            ArtefactKind::Summary,
        ] {
            let json = serde_json::to_string(&kind).unwrap();
            let loaded: ArtefactKind = serde_json::from_str(&json).unwrap();
            assert_eq!(loaded, kind);
        }
    }

    #[test]
    fn task_artefact_roundtrip() {
        let artefact = TaskArtefact {
            kind: ArtefactKind::Json,
            name: "result".to_string(),
            path: "output/result.json".to_string(),
        };
        let json = serde_json::to_string(&artefact).unwrap();
        let loaded: TaskArtefact = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, artefact);
    }

    #[test]
    fn task_inputs_outputs_default_to_empty() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.inputs.is_empty());
        assert!(task.outputs.is_empty());
    }

    #[test]
    fn task_inputs_outputs_omitted_when_empty() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("\"inputs\""),
            "empty inputs should be omitted"
        );
        assert!(
            !json.contains("\"outputs\""),
            "empty outputs should be omitted"
        );
    }

    #[test]
    fn task_with_inputs_outputs_roundtrip() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.inputs = vec!["prev-task/report".to_string()];
        task.outputs = vec![TaskArtefact {
            kind: ArtefactKind::File,
            name: "report".to_string(),
            path: "reports/out.txt".to_string(),
        }];
        let json = serde_json::to_string(&task).unwrap();
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.inputs, vec!["prev-task/report"]);
        assert_eq!(loaded.outputs.len(), 1);
        assert_eq!(loaded.outputs[0].kind, ArtefactKind::File);
        assert_eq!(loaded.outputs[0].name, "report");
        assert_eq!(loaded.outputs[0].path, "reports/out.txt");
    }

    // ---- TaskRuntime tests ----

    #[test]
    fn task_runtime_default_is_local() {
        assert_eq!(TaskRuntime::default(), TaskRuntime::Local);
    }

    #[test]
    fn task_runtime_defaults_to_local_when_absent() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert_eq!(task.runtime, TaskRuntime::Local);
    }

    #[test]
    fn task_runtime_local_is_omitted_in_serialization() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("\"runtime\""),
            "default runtime should be omitted"
        );
    }

    #[test]
    fn task_runtime_gastown_is_serialized() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.runtime = TaskRuntime::Gastown;
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"runtime\":\"gastown\""));
    }

    #[test]
    fn task_runtime_roundtrip() {
        for runtime in [TaskRuntime::Local, TaskRuntime::Gastown] {
            let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
            task.runtime = runtime;
            let json = serde_json::to_string(&task).unwrap();
            let loaded: Task = serde_json::from_str(&json).unwrap();
            assert_eq!(loaded.runtime, runtime);
        }
    }

    // ---- Config gastown fields tests ----

    #[test]
    fn config_gastown_fields_default_to_none() {
        let config = Config::default();
        assert!(config.gastown_endpoint.is_none());
        assert!(config.gastown_token.is_none());
    }

    #[test]
    fn config_gastown_fields_roundtrip() {
        let mut config = Config::default();
        config.gastown_endpoint = Some("https://gastown.example.com".to_string());
        config.gastown_token = Some("tok_secret".to_string());
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.gastown_endpoint.as_deref(),
            Some("https://gastown.example.com")
        );
        assert_eq!(loaded.gastown_token.as_deref(), Some("tok_secret"));
    }

    #[test]
    fn config_gastown_fields_omitted_when_none() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            !json.contains("gastown_endpoint"),
            "absent endpoint should be omitted"
        );
        assert!(
            !json.contains("gastown_token"),
            "absent token should be omitted"
        );
    }

    // ---- precondition_prompt tests ----

    #[test]
    fn task_precondition_prompt_defaults_to_none_when_absent() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.precondition_prompt.is_none());
    }

    #[test]
    fn task_precondition_prompt_omitted_when_none() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("precondition_prompt"),
            "absent precondition_prompt should be omitted"
        );
    }

    #[test]
    fn task_precondition_prompt_roundtrip() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.precondition_prompt = Some("Check if docs are stale".to_string());
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("precondition_prompt"));
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.precondition_prompt.as_deref(),
            Some("Check if docs are stale")
        );
    }

    #[test]
    fn task_precondition_prompt_with_recurring_kind_roundtrip() {
        let mut task = make_task("docs", TaskStatus::Pending, 1, vec![]);
        task.kind = TaskKind::Recurring;
        task.cooldown_seconds = Some(3600);
        task.precondition_prompt =
            Some("Only run if README.md has changed since last update".to_string());
        let json = serde_json::to_string(&task).unwrap();
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.kind, TaskKind::Recurring);
        assert_eq!(loaded.cooldown_seconds, Some(3600));
        assert_eq!(
            loaded.precondition_prompt.as_deref(),
            Some("Only run if README.md has changed since last update")
        );
    }

    // ---- parent_id tests ----

    #[test]
    fn task_parent_id_defaults_to_none_when_absent() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.parent_id.is_none());
    }

    #[test]
    fn task_parent_id_omitted_when_none() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("parent_id"),
            "absent parent_id should be omitted"
        );
    }

    #[test]
    fn task_parent_id_roundtrip() {
        let mut task = make_task("sub-1", TaskStatus::Pending, 1, vec![]);
        task.parent_id = Some("epic-1".to_string());
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"parent_id\":\"epic-1\""));
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.parent_id.as_deref(), Some("epic-1"));
    }

    // ---- labels tests ----

    #[test]
    fn task_labels_default_to_empty_when_absent() {
        let json = r#"{"id":"x","description":"d","status":"pending"}"#;
        let task: Task = serde_json::from_str(json).unwrap();
        assert!(task.labels.is_empty());
    }

    #[test]
    fn task_labels_omitted_when_empty() {
        let task = make_task("x", TaskStatus::Pending, 1, vec![]);
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            !json.contains("\"labels\""),
            "empty labels should be omitted"
        );
    }

    #[test]
    fn task_labels_roundtrip() {
        let mut task = make_task("x", TaskStatus::Pending, 1, vec![]);
        task.labels = vec!["frontend".to_string(), "urgent".to_string()];
        let json = serde_json::to_string(&task).unwrap();
        assert!(json.contains("\"labels\""));
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.labels, vec!["frontend", "urgent"]);
    }

    #[test]
    fn task_epic_with_subtask_roundtrip() {
        let epic = make_task("epic-1", TaskStatus::Pending, 1, vec![]);
        let mut sub = make_task("sub-1", TaskStatus::Pending, 1, vec!["epic-1"]);
        sub.parent_id = Some("epic-1".to_string());
        sub.labels = vec!["backend".to_string()];

        let tasks = vec![epic, sub];
        let json = serde_json::to_string(&tasks).unwrap();
        let loaded: Vec<Task> = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.len(), 2);
        assert!(loaded[0].parent_id.is_none());
        assert_eq!(loaded[1].parent_id.as_deref(), Some("epic-1"));
        assert_eq!(loaded[1].labels, vec!["backend"]);
    }

    // ---- work_dirs tests ----

    #[test]
    fn config_work_dirs_defaults_to_empty() {
        let config = Config::default();
        assert!(config.work_dirs.is_empty());
    }

    #[test]
    fn config_work_dirs_omitted_when_empty() {
        let config = Config::default();
        let json = serde_json::to_string(&config).unwrap();
        assert!(
            !json.contains("work_dirs"),
            "empty work_dirs should be omitted from serialization"
        );
    }

    #[test]
    fn config_work_dirs_roundtrip() {
        let mut config = Config::default();
        config
            .work_dirs
            .insert("frontend".to_string(), "/repos/frontend".to_string());
        config
            .work_dirs
            .insert("backend".to_string(), "/repos/backend".to_string());
        let json = serde_json::to_string(&config).unwrap();
        let loaded: Config = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.work_dirs.get("frontend").map(String::as_str),
            Some("/repos/frontend")
        );
        assert_eq!(
            loaded.work_dirs.get("backend").map(String::as_str),
            Some("/repos/backend")
        );
    }

    #[test]
    fn config_work_dirs_defaults_when_absent_from_json() {
        let json = r#"{"max_iterations":5}"#;
        let config: Config = serde_json::from_str(json).unwrap();
        assert!(config.work_dirs.is_empty());
    }
}
