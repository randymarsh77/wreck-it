use serde::{Deserialize, Serialize};
use std::path::PathBuf;

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
    "https://api.githubcopilot.com".to_string()
}

impl Default for Config {
    fn default() -> Self {
        Self {
            max_iterations: default_max_iterations(),
            task_file: default_task_file(),
            work_dir: default_work_dir(),
            api_endpoint: default_api_endpoint(),
            api_token: None,
        }
    }
}

/// A task to be completed by the agent
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
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
}
