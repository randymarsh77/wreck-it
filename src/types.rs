use clap::ValueEnum;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

pub const DEFAULT_COPILOT_ENDPOINT: &str = "https://api.githubcopilot.com";
pub const DEFAULT_LLAMA_ENDPOINT: &str = "http://localhost:11434/v1";
pub const DEFAULT_GITHUB_MODELS_ENDPOINT: &str =
    "https://models.github.ai/inference/chat/completions";
pub const DEFAULT_LLAMA_MODEL: &str = "llama3.2";
pub const DEFAULT_GITHUB_MODELS_MODEL: &str = "openai/gpt-4o";
pub const LLAMA_PROVIDER_TYPE: &str = "openai";
pub const DEFAULT_COMPLETION_MARKER: &str = ".task-complete";

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
        }
    }
}

/// A task to be completed by the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,

    /// Execution phase (tasks in the same phase may run in parallel).
    /// Tasks in a lower phase run before tasks in a higher phase.
    /// When omitted, defaults to `1` (all tasks share one sequential phase).
    #[serde(default = "default_phase", skip_serializing_if = "is_default_phase")]
    pub phase: u32,

    /// IDs of tasks that must complete before this task can start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
}

fn default_phase() -> u32 {
    1
}

fn is_default_phase(v: &u32) -> bool {
    *v == 1
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
            phase,
            depends_on: depends_on.into_iter().map(String::from).collect(),
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
}
