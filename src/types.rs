use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_COPILOT_ENDPOINT: &str = "https://api.githubcopilot.com";
pub const DEFAULT_LLAMA_ENDPOINT: &str = "http://localhost:11434/v1";
pub const DEFAULT_GITHUB_MODELS_ENDPOINT: &str =
    "https://models.github.ai/inference/chat/completions";
pub const DEFAULT_LLAMA_MODEL: &str = "llama3.2";
pub const DEFAULT_GITHUB_MODELS_MODEL: &str = "anthropic/claude-opus-4.6";
pub const LLAMA_PROVIDER_TYPE: &str = "openai";
pub const DEFAULT_COMPLETION_MARKER: &str = ".task-complete";
pub const DEFAULT_REFLECTION_ROUNDS: u8 = 2;
pub const DEFAULT_REPLAN_THRESHOLD: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, ValueEnum)]
#[serde(rename_all = "lowercase")]
pub enum ModelProvider {
    Copilot,
    Llama,
    GithubModels,
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
        }
    }
}

/// The lifecycle kind of a task.
///
/// When absent in a JSON task file, the field defaults to [`TaskKind::Milestone`]
/// so that pre-existing task files continue to work without modification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// A one-shot goal that completes permanently.
    #[default]
    Milestone,
    /// A long-running goal that resets to pending after completion, subject to
    /// an optional cooldown period.
    Recurring,
}

/// The role of the agent assigned to execute this task.
///
/// When absent in a JSON task file the field defaults to [`AgentRole::Implementer`]
/// so that pre-existing task files continue to work without modification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, ValueEnum, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    /// Research and generate new tasks.
    Ideas,
    /// Write code / implement the work (default).
    #[default]
    Implementer,
    /// Review and validate completed work.
    Evaluator,
}

/// A task to be completed by the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,

    /// Agent role responsible for this task.  Defaults to `implementer` for
    /// backward compatibility with task files that pre-date this field.
    #[serde(default, skip_serializing_if = "is_default_role")]
    pub role: AgentRole,

    /// Lifecycle kind of the task.  Defaults to `milestone` (one-shot).
    /// Set to `recurring` for long-running goals that reset to pending after
    /// completion, subject to an optional cooldown.
    #[serde(default, skip_serializing_if = "is_default_kind")]
    pub kind: TaskKind,

    /// Minimum number of seconds that must elapse after a recurring task
    /// completes before it is eligible to run again.  Only meaningful when
    /// `kind` is `recurring`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_seconds: Option<u64>,

    /// Execution phase (tasks in the same phase may run in parallel).
    /// Tasks in a lower phase run before tasks in a higher phase.
    /// When omitted, defaults to `1` (all tasks share one sequential phase).
    #[serde(default = "default_phase", skip_serializing_if = "is_default_phase")]
    pub phase: u32,

    /// IDs of tasks that must complete before this task can start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Scheduling priority (higher = run sooner).  Defaults to 0.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub priority: u32,

    /// Estimated complexity on a 1–10 scale (lower = quicker win).  Defaults to 1.
    #[serde(
        default = "default_complexity",
        skip_serializing_if = "is_default_complexity"
    )]
    pub complexity: u32,

    /// Number of previous failed execution attempts.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub failed_attempts: u32,

    /// Unix timestamp (seconds) of the most recent execution attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<u64>,
}

fn is_default_role(r: &AgentRole) -> bool {
    *r == AgentRole::Implementer
}

fn is_default_kind(k: &TaskKind) -> bool {
    *k == TaskKind::Milestone
}

fn default_phase() -> u32 {
    1
}

fn is_default_phase(v: &u32) -> bool {
    *v == 1
}

fn default_complexity() -> u32 {
    1
}

fn is_default_complexity(v: &u32) -> bool {
    *v == 1
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
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
            failed_attempts: 0,
            last_attempt_at: None,
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
}
