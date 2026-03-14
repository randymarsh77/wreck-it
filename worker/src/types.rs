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
    pub state: String,
    pub merged: Option<bool>,
    pub user: Option<User>,
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
            "state": "closed",
            "merged": true,
            "user": {"login": "copilot-swe-agent[bot]", "type": "Bot"}
        }"#;
        let pr: PullRequest = serde_json::from_str(json).unwrap();
        let user = pr.user.unwrap();
        assert_eq!(user.login, "copilot-swe-agent[bot]");
        assert_eq!(user.user_type.as_deref(), Some("Bot"));
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
}
