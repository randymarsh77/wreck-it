//! Domain types used by the worker.
//!
//! Shared types (Task, HeadlessState, RepoConfig, etc.) are re-exported from
//! `wreck-it-core`.  Webhook-specific payload types remain local.

use serde::Deserialize;

// Re-export shared types from wreck-it-core.
#[allow(unused_imports)]
pub use wreck_it_core::config::{RalphConfig, RepoConfig};
#[allow(unused_imports)]
pub use wreck_it_core::state::{AgentPhase, HeadlessState, TrackedPr};
#[allow(unused_imports)]
pub use wreck_it_core::types::{
    AgentRole, ArtefactKind, Task, TaskArtefact, TaskKind, TaskRuntime, TaskStatus,
};

// ---------------------------------------------------------------------------
// Pulse registry types
// ---------------------------------------------------------------------------

/// A registered repository for the pulse (scheduled) trigger system.
///
/// When the worker processes a webhook, it records the repository's
/// coordinates and installation ID so that subsequent cron-triggered
/// "pulse" invocations can iterate over all known repositories without
/// relying on an incoming webhook payload.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct PulseRegistration {
    pub owner: String,
    pub repo: String,
    pub installation_id: u64,
    pub default_branch: String,
}

// ---------------------------------------------------------------------------
// Installation settings
// ---------------------------------------------------------------------------

/// Per-installation settings stored in KV.
///
/// These settings control installation-wide behaviour such as scheduled
/// pulse execution and event processing.  They are managed via the portal
/// UI and persisted in KV under `_installation/{id}/settings`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct InstallationSettings {
    /// Cron expression for the recurring pulse schedule (e.g. `"*/30 * * * *"`).
    ///
    /// When set, a `SchedulerAgent` Durable Object schedules alarms at
    /// this cadence to pulse Ralph iterations across all repos in the
    /// installation.
    #[serde(default = "default_pulse_cron")]
    pub pulse_cron: String,

    /// Whether the scheduled pulse is enabled.  When `false`, the scheduler
    /// alarm still exists but skips processing.
    #[serde(default = "default_true")]
    pub pulse_enabled: bool,

    /// Whether webhook-driven events and triggers are enabled for this
    /// installation.  When `false`, all incoming webhook events are
    /// short-circuited with a `200 OK` response without any processing.
    #[serde(default = "default_true")]
    pub events_enabled: bool,
}

fn default_pulse_cron() -> String {
    "*/30 * * * *".to_string()
}

fn default_true() -> bool {
    true
}

impl Default for InstallationSettings {
    fn default() -> Self {
        Self {
            pulse_cron: default_pulse_cron(),
            pulse_enabled: true,
            events_enabled: true,
        }
    }
}

// ---------------------------------------------------------------------------
// Durable Object agent state types
// ---------------------------------------------------------------------------

/// Execution status of a ralph agent running as a Durable Object.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionStatus {
    #[default]
    #[serde(alias = "Idle")]
    Idle,
    #[serde(alias = "Running")]
    Running,
    #[serde(alias = "Paused")]
    Paused,
}

/// Execution metadata tracked by the ralph agent Durable Object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ExecutionState {
    pub status: ExecutionStatus,
    pub current_task_id: Option<String>,
    pub iteration_count: usize,
    pub last_run_at: Option<u64>,
}

impl Default for ExecutionState {
    fn default() -> Self {
        Self {
            status: ExecutionStatus::Idle,
            current_task_id: None,
            iteration_count: 0,
            last_run_at: None,
        }
    }
}

/// Config stashed inside the ralph agent Durable Object.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct AgentConfig {
    pub task_file: String,
    pub state_file: String,
    pub owner: String,
    pub repo: String,
}

/// Full persistent state for a `RalphAgent` Durable Object.
///
/// Stored in the DO's transactional storage under the key `"ralph_state"`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct RalphState {
    pub tasks: Vec<Task>,
    pub execution: ExecutionState,
    pub config: AgentConfig,
}

impl RalphState {
    /// Build an initial state from configuration values.
    #[allow(dead_code)]
    pub fn new(owner: &str, repo: &str, task_file: &str, state_file: &str) -> Self {
        Self {
            tasks: Vec::new(),
            execution: ExecutionState::default(),
            config: AgentConfig {
                task_file: task_file.to_string(),
                state_file: state_file.to_string(),
                owner: owner.to_string(),
                repo: repo.to_string(),
            },
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
    pub sender: Option<User>,
    pub workflow_run: Option<WorkflowRun>,
    /// Repositories included in `installation` webhook events.
    #[serde(default)]
    pub repositories: Vec<InstallationRepository>,
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
    pub account: Option<InstallationAccount>,
}

/// The account (user or org) that owns an installation.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct InstallationAccount {
    pub login: String,
}

/// A repository reference inside an `installation` webhook payload.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct InstallationRepository {
    pub id: u64,
    pub name: String,
    pub full_name: String,
    #[serde(default = "default_branch_main")]
    pub default_branch: String,
}

fn default_branch_main() -> String {
    "main".to_string()
}

/// A GitHub user (or bot) as represented in webhook payloads.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct User {
    pub login: String,
    /// Account type — typically `"User"`, `"Bot"`, or `"Organization"`.
    #[serde(rename = "type")]
    pub user_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct Issue {
    pub number: u64,
    pub title: String,
    pub body: Option<String>,
    pub labels: Vec<Label>,
    pub user: Option<User>,
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
    pub title: Option<String>,
    pub body: Option<String>,
    pub draft: Option<bool>,
    pub state: String,
    pub merged: Option<bool>,
    pub user: Option<User>,
}

/// A workflow run as represented in `workflow_run` webhook payloads.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WorkflowRun {
    pub id: u64,
    /// The outcome of the workflow run: `"success"`, `"failure"`,
    /// `"cancelled"`, `"timed_out"`, etc.
    pub conclusion: Option<String>,
    /// Pull requests associated with the head branch of this run.
    #[serde(default)]
    pub pull_requests: Vec<WorkflowRunPr>,
}

/// Minimal pull request reference inside a `workflow_run` payload.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
pub struct WorkflowRunPr {
    pub number: u64,
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
            acceptance_criteria: None,
            evaluation: None,
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
                issue_number: Some(10),
                review_requested: None,
            }],
            review_requested: None,
            pending_merge_issues: vec![],
            task_statuses: std::collections::HashMap::new(),
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

    #[test]
    fn issue_user_parsed() {
        let json = r#"{
            "number": 42,
            "title": "[wreck-it] t1",
            "body": null,
            "labels": [{"name": "wreck-it"}],
            "user": {"login": "my-app[bot]", "type": "Bot"}
        }"#;
        let issue: Issue = serde_json::from_str(json).unwrap();
        let user = issue.user.unwrap();
        assert_eq!(user.login, "my-app[bot]");
        assert_eq!(user.user_type.as_deref(), Some("Bot"));
    }

    #[test]
    fn pr_user_parsed() {
        let json = r#"{
            "number": 10,
            "title": "Fix something",
            "body": "Description here",
            "draft": false,
            "state": "closed",
            "merged": true,
            "user": {"login": "copilot-swe-agent[bot]", "type": "Bot"}
        }"#;
        let pr: PullRequest = serde_json::from_str(json).unwrap();
        let user = pr.user.unwrap();
        assert_eq!(user.login, "copilot-swe-agent[bot]");
        assert_eq!(user.user_type.as_deref(), Some("Bot"));
        assert_eq!(pr.title.as_deref(), Some("Fix something"));
        assert_eq!(pr.body.as_deref(), Some("Description here"));
        assert_eq!(pr.draft, Some(false));
    }

    #[test]
    fn pulse_registration_roundtrip() {
        let reg = PulseRegistration {
            owner: "octo".into(),
            repo: "repo".into(),
            installation_id: 42,
            default_branch: "main".into(),
        };
        let json = serde_json::to_string(&reg).unwrap();
        let loaded: PulseRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, reg);
    }

    #[test]
    fn pulse_registration_vec_roundtrip() {
        let regs = vec![
            PulseRegistration {
                owner: "a".into(),
                repo: "b".into(),
                installation_id: 1,
                default_branch: "main".into(),
            },
            PulseRegistration {
                owner: "c".into(),
                repo: "d".into(),
                installation_id: 2,
                default_branch: "develop".into(),
            },
        ];
        let json = serde_json::to_string(&regs).unwrap();
        let loaded: Vec<PulseRegistration> = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[0].owner, "a");
        assert_eq!(loaded[1].default_branch, "develop");
    }

    #[test]
    fn webhook_payload_with_sender() {
        let json = r#"{
            "action": "opened",
            "sender": {"login": "my-app[bot]", "type": "Bot"},
            "repository": {"full_name": "o/r", "name": "r", "owner": {"login": "o"}}
        }"#;
        let payload: WebhookPayload = serde_json::from_str(json).unwrap();
        let sender = payload.sender.unwrap();
        assert_eq!(sender.login, "my-app[bot]");
        assert_eq!(sender.user_type.as_deref(), Some("Bot"));
    }

    #[test]
    fn webhook_payload_with_workflow_run() {
        let json = r#"{
            "action": "completed",
            "workflow_run": {
                "id": 999,
                "conclusion": "failure",
                "pull_requests": [{"number": 42}, {"number": 7}]
            },
            "repository": {"full_name": "o/r", "name": "r", "owner": {"login": "o"}}
        }"#;
        let payload: WebhookPayload = serde_json::from_str(json).unwrap();
        let wr = payload.workflow_run.unwrap();
        assert_eq!(wr.id, 999);
        assert_eq!(wr.conclusion.as_deref(), Some("failure"));
        assert_eq!(wr.pull_requests.len(), 2);
        assert_eq!(wr.pull_requests[0].number, 42);
        assert_eq!(wr.pull_requests[1].number, 7);
    }

    #[test]
    fn workflow_run_no_prs() {
        let json = r#"{
            "id": 100,
            "conclusion": "success",
            "pull_requests": []
        }"#;
        let wr: WorkflowRun = serde_json::from_str(json).unwrap();
        assert_eq!(wr.id, 100);
        assert_eq!(wr.conclusion.as_deref(), Some("success"));
        assert!(wr.pull_requests.is_empty());
    }

    #[test]
    fn workflow_run_missing_prs_defaults_to_empty() {
        let json = r#"{"id": 100, "conclusion": null}"#;
        let wr: WorkflowRun = serde_json::from_str(json).unwrap();
        assert!(wr.pull_requests.is_empty());
        assert!(wr.conclusion.is_none());
    }

    #[test]
    fn execution_status_default_is_idle() {
        let status = ExecutionStatus::default();
        assert_eq!(status, ExecutionStatus::Idle);
    }

    #[test]
    fn execution_state_default() {
        let state = ExecutionState::default();
        assert_eq!(state.status, ExecutionStatus::Idle);
        assert!(state.current_task_id.is_none());
        assert_eq!(state.iteration_count, 0);
        assert!(state.last_run_at.is_none());
    }

    #[test]
    fn ralph_state_new() {
        let state = RalphState::new("octo", "repo", "tasks.json", ".state.json");
        assert!(state.tasks.is_empty());
        assert_eq!(state.execution.status, ExecutionStatus::Idle);
        assert_eq!(state.config.owner, "octo");
        assert_eq!(state.config.repo, "repo");
        assert_eq!(state.config.task_file, "tasks.json");
        assert_eq!(state.config.state_file, ".state.json");
    }

    #[test]
    fn ralph_state_roundtrip() {
        let state = RalphState {
            tasks: vec![],
            execution: ExecutionState {
                status: ExecutionStatus::Running,
                current_task_id: Some("t1".into()),
                iteration_count: 3,
                last_run_at: Some(1700000000),
            },
            config: AgentConfig {
                task_file: "tasks.json".into(),
                state_file: ".state.json".into(),
                owner: "octo".into(),
                repo: "repo".into(),
            },
        };
        let json = serde_json::to_string(&state).unwrap();
        let loaded: RalphState = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.execution.status, ExecutionStatus::Running);
        assert_eq!(loaded.execution.current_task_id.as_deref(), Some("t1"));
        assert_eq!(loaded.execution.iteration_count, 3);
        assert_eq!(loaded.config.owner, "octo");
    }

    #[test]
    fn execution_status_serde() {
        let json = serde_json::to_string(&ExecutionStatus::Running).unwrap();
        assert_eq!(json, r#""running""#);
        let loaded: ExecutionStatus = serde_json::from_str(r#""paused""#).unwrap();
        assert_eq!(loaded, ExecutionStatus::Paused);
    }

    #[test]
    fn execution_status_serde_capitalized_aliases() {
        let idle: ExecutionStatus = serde_json::from_str(r#""Idle""#).unwrap();
        assert_eq!(idle, ExecutionStatus::Idle);
        let running: ExecutionStatus = serde_json::from_str(r#""Running""#).unwrap();
        assert_eq!(running, ExecutionStatus::Running);
        let paused: ExecutionStatus = serde_json::from_str(r#""Paused""#).unwrap();
        assert_eq!(paused, ExecutionStatus::Paused);
    }

    #[test]
    fn installation_settings_default() {
        let settings = InstallationSettings::default();
        assert_eq!(settings.pulse_cron, "*/30 * * * *");
        assert!(settings.pulse_enabled);
        assert!(settings.events_enabled);
    }

    #[test]
    fn installation_settings_roundtrip() {
        let settings = InstallationSettings {
            pulse_cron: "*/15 * * * *".into(),
            pulse_enabled: false,
            events_enabled: true,
        };
        let json = serde_json::to_string(&settings).unwrap();
        let loaded: InstallationSettings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.pulse_cron, "*/15 * * * *");
        assert!(!loaded.pulse_enabled);
        assert!(loaded.events_enabled);
    }

    #[test]
    fn installation_settings_deserialize_defaults() {
        let json = r#"{}"#;
        let settings: InstallationSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.pulse_cron, "*/30 * * * *");
        assert!(settings.pulse_enabled);
        assert!(settings.events_enabled);
    }

    #[test]
    fn installation_settings_partial_deserialize() {
        let json = r#"{"events_enabled": false}"#;
        let settings: InstallationSettings = serde_json::from_str(json).unwrap();
        assert_eq!(settings.pulse_cron, "*/30 * * * *");
        assert!(settings.pulse_enabled);
        assert!(!settings.events_enabled);
    }

    #[test]
    fn installation_account_parsed() {
        let json = r#"{"id": 42, "account": {"login": "my-org"}}"#;
        let inst: Installation = serde_json::from_str(json).unwrap();
        assert_eq!(inst.id, 42);
        assert_eq!(inst.account.unwrap().login, "my-org");
    }

    #[test]
    fn installation_without_account() {
        let json = r#"{"id": 42}"#;
        let inst: Installation = serde_json::from_str(json).unwrap();
        assert_eq!(inst.id, 42);
        assert!(inst.account.is_none());
    }

    #[test]
    fn installation_repository_parsed() {
        let json = r#"{"id": 1, "name": "repo", "full_name": "org/repo", "default_branch": "develop"}"#;
        let repo: InstallationRepository = serde_json::from_str(json).unwrap();
        assert_eq!(repo.id, 1);
        assert_eq!(repo.name, "repo");
        assert_eq!(repo.full_name, "org/repo");
        assert_eq!(repo.default_branch, "develop");
    }

    #[test]
    fn installation_repository_default_branch() {
        let json = r#"{"id": 1, "name": "repo", "full_name": "org/repo"}"#;
        let repo: InstallationRepository = serde_json::from_str(json).unwrap();
        assert_eq!(repo.default_branch, "main");
    }

    #[test]
    fn webhook_payload_installation_event() {
        let json = r#"{
            "action": "created",
            "installation": {"id": 100, "account": {"login": "my-org"}},
            "repositories": [
                {"id": 1, "name": "repo-a", "full_name": "my-org/repo-a", "default_branch": "main"},
                {"id": 2, "name": "repo-b", "full_name": "my-org/repo-b", "default_branch": "develop"}
            ]
        }"#;
        let payload: WebhookPayload = serde_json::from_str(json).unwrap();
        assert_eq!(payload.action.as_deref(), Some("created"));
        assert_eq!(payload.installation.as_ref().unwrap().id, 100);
        assert_eq!(
            payload.installation.as_ref().unwrap().account.as_ref().unwrap().login,
            "my-org"
        );
        assert_eq!(payload.repositories.len(), 2);
        assert_eq!(payload.repositories[0].name, "repo-a");
        assert_eq!(payload.repositories[1].default_branch, "develop");
    }

    #[test]
    fn webhook_payload_no_repositories_defaults_empty() {
        let json = r#"{"action":"opened","repository":{"full_name":"o/r","name":"r","owner":{"login":"o"}}}"#;
        let payload: WebhookPayload = serde_json::from_str(json).unwrap();
        assert!(payload.repositories.is_empty());
    }
}
