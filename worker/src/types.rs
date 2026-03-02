//! Minimal subset of wreck-it domain types used by the worker.
//!
//! These mirror the structures in the main `wreck-it` crate so that config,
//! task, and state files can be read and written from the Cloudflare Worker
//! without pulling in native-only dependencies.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Task types
// ---------------------------------------------------------------------------

/// Status of an individual task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

/// Lifecycle kind of a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    #[default]
    Milestone,
    Recurring,
}

/// The role of the agent assigned to a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    Ideas,
    #[default]
    Implementer,
    Evaluator,
}

/// Execution runtime for a task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskRuntime {
    #[default]
    Local,
    Gastown,
}

/// Kind of an artefact.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArtefactKind {
    File,
    Json,
    Summary,
}

/// An artefact declared as input/output of a task.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TaskArtefact {
    pub kind: ArtefactKind,
    pub name: String,
    pub path: String,
}

/// A task in the wreck-it task file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,

    #[serde(default)]
    pub role: AgentRole,

    #[serde(default)]
    pub kind: TaskKind,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_seconds: Option<u64>,

    #[serde(default = "default_phase")]
    pub phase: u32,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    #[serde(default)]
    pub priority: u32,

    #[serde(default = "default_complexity")]
    pub complexity: u32,

    #[serde(default)]
    pub failed_attempts: u32,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<u64>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<TaskArtefact>,

    #[serde(default)]
    pub runtime: TaskRuntime,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition_prompt: Option<String>,

    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

fn default_phase() -> u32 {
    1
}

fn default_complexity() -> u32 {
    1
}

// ---------------------------------------------------------------------------
// Repo config (`.wreck-it/config.toml` on the main branch)
// ---------------------------------------------------------------------------

/// Repository-level wreck-it configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RepoConfig {
    #[serde(default = "default_state_branch")]
    pub state_branch: String,

    #[serde(default = "default_state_root")]
    pub state_root: String,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ralphs: Vec<RalphConfig>,
}

/// Named ralph context.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RalphConfig {
    pub name: String,

    #[serde(default = "default_task_file")]
    pub task_file: String,

    #[serde(default = "default_state_file")]
    pub state_file: String,
}

fn default_state_branch() -> String {
    "wreck-it-state".to_string()
}

fn default_state_root() -> String {
    ".wreck-it".to_string()
}

fn default_task_file() -> String {
    "tasks.json".to_string()
}

fn default_state_file() -> String {
    ".wreck-it-state.json".to_string()
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            state_branch: default_state_branch(),
            state_root: default_state_root(),
            ralphs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Headless state (persisted on the state branch)
// ---------------------------------------------------------------------------

/// Phases of a headless cloud agent iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    NeedsTrigger,
    AgentWorking,
    NeedsVerification,
    Completed,
}

/// A pull request being tracked by the headless runner.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackedPr {
    pub pr_number: u64,
    pub task_id: String,
}

/// Persistent headless state committed to the state branch.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessState {
    pub phase: AgentPhase,
    pub iteration: usize,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,

    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<String>,

    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_prs: Vec<TrackedPr>,
}

impl Default for HeadlessState {
    fn default() -> Self {
        Self {
            phase: AgentPhase::NeedsTrigger,
            iteration: 0,
            current_task_id: None,
            issue_number: None,
            pr_number: None,
            pr_url: None,
            last_prompt: None,
            memory: Vec::new(),
            tracked_prs: Vec::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// Webhook payload types
//
// Fields are populated by deserialization and exposed for consumers; suppress
// dead-code warnings for the entire section.
// ---------------------------------------------------------------------------

/// Subset of a GitHub webhook payload we care about.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WebhookPayload {
    pub action: Option<String>,
    pub repository: Option<Repository>,
    pub installation: Option<Installation>,
    pub issue: Option<Issue>,
    pub pull_request: Option<PullRequest>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Repository {
    pub full_name: String,
    pub name: String,
    pub owner: RepositoryOwner,
    pub default_branch: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct RepositoryOwner {
    pub login: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Installation {
    pub id: u64,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<Label>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct PullRequest {
    pub number: u64,
    pub state: String,
    pub merged: Option<bool>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_roundtrip_json() {
        let task = Task {
            id: "1".into(),
            description: "Test task".into(),
            status: TaskStatus::Pending,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        };
        let json = serde_json::to_string(&task).unwrap();
        let loaded: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.id, "1");
        assert_eq!(loaded.status, TaskStatus::Pending);
    }

    #[test]
    fn headless_state_default() {
        let state = HeadlessState::default();
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert_eq!(state.iteration, 0);
        assert!(state.tracked_prs.is_empty());
    }

    #[test]
    fn headless_state_roundtrip() {
        let state = HeadlessState {
            phase: AgentPhase::AgentWorking,
            iteration: 5,
            current_task_id: Some("t1".into()),
            issue_number: Some(10),
            pr_number: Some(20),
            pr_url: Some("https://github.com/o/r/pull/20".into()),
            last_prompt: Some("do the thing".into()),
            memory: vec!["note".into()],
            tracked_prs: vec![TrackedPr {
                pr_number: 20,
                task_id: "t1".into(),
            }],
        };
        let json = serde_json::to_string_pretty(&state).unwrap();
        let loaded: HeadlessState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.phase, AgentPhase::AgentWorking);
        assert_eq!(loaded.iteration, 5);
        assert_eq!(loaded.tracked_prs.len(), 1);
    }

    #[test]
    fn repo_config_from_toml() {
        let toml_str = r#"
state_branch = "my-state"
state_root = ".my-root"

[[ralphs]]
name = "docs"
task_file = "docs-tasks.json"
state_file = ".docs-state.json"
"#;
        let cfg: RepoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.state_branch, "my-state");
        assert_eq!(cfg.ralphs.len(), 1);
        assert_eq!(cfg.ralphs[0].name, "docs");
    }

    #[test]
    fn repo_config_defaults() {
        let cfg: RepoConfig = toml::from_str("").unwrap();
        assert_eq!(cfg.state_branch, "wreck-it-state");
        assert_eq!(cfg.state_root, ".wreck-it");
        assert!(cfg.ralphs.is_empty());
    }

    #[test]
    fn webhook_payload_minimal_parse() {
        let json = r#"{"action":"opened","repository":{"full_name":"o/r","name":"r","owner":{"login":"o"}}}"#;
        let payload: WebhookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.action.as_deref(), Some("opened"));
        assert_eq!(payload.repository.unwrap().full_name, "o/r");
    }
}
