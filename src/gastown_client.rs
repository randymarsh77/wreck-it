//! Gastown cloud runtime integration.
//!
//! This module provides a client for the gastown cloud agent service.  When
//! one or more tasks in the wreck-it task graph carry `runtime: gastown`, the
//! [`GastownClient`] can:
//!
//! 1. Serialise the task graph as a gastown-compatible workflow DAG (JSON).
//! 2. Submit the workflow to a configurable gastown endpoint.
//! 3. Poll for task-completion events and update the local task-state file
//!    (`.wreck-it-state.json`) accordingly.
//!
//! ## Configuration
//!
//! Integration is enabled by setting both `gastown_endpoint` and
//! `gastown_token` in the wreck-it [`Config`](crate::types::Config).  When
//! either field is absent the module is a no-op.

use crate::types::{Task, TaskRuntime, TaskStatus};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// DAG data structures
// ---------------------------------------------------------------------------

/// A single node in the gastown workflow DAG.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DagNode {
    /// Unique task identifier within the workflow.
    pub id: String,
    /// Human-readable description of the work.
    pub description: String,
    /// IDs of upstream nodes that must complete before this node runs.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// Scheduling phase (lower runs first; same phase may run in parallel).
    pub phase: u32,
}

/// A gastown-compatible workflow DAG ready for submission.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct WorkflowDag {
    /// Workflow name / identifier.
    pub name: String,
    /// All nodes that carry `runtime: gastown`.
    pub nodes: Vec<DagNode>,
}

// ---------------------------------------------------------------------------
// Status events
// ---------------------------------------------------------------------------

/// The completion status reported by the gastown service for a task.
#[allow(dead_code)]
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GastownTaskStatus {
    /// The task completed successfully.
    Completed,
    /// The task failed.
    Failed,
    /// The task is still in progress.
    Running,
}

/// A status-update event received from the gastown polling endpoint.
#[allow(dead_code)]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct GastownStatusEvent {
    /// Wreck-it task ID whose status changed.
    pub task_id: String,
    /// Current status as reported by gastown.
    pub status: GastownTaskStatus,
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

/// Client for the gastown cloud agent service.
#[allow(dead_code)]
pub struct GastownClient {
    endpoint: String,
    token: String,
    http: reqwest::Client,
}

#[allow(dead_code)]
impl GastownClient {
    /// Create a new client.
    ///
    /// Returns `None` when either `endpoint` or `token` is absent so callers
    /// can guard against running the client when gastown integration is
    /// disabled.
    pub fn new(endpoint: Option<&str>, token: Option<&str>) -> Option<Self> {
        let endpoint = endpoint?.trim_end_matches('/').to_string();
        let token = token?.to_string();
        let http = reqwest::Client::new();
        Some(Self {
            endpoint,
            token,
            http,
        })
    }

    // -----------------------------------------------------------------------
    // DAG serialisation
    // -----------------------------------------------------------------------

    /// Build a [`WorkflowDag`] from a slice of tasks, keeping only those that
    /// carry `runtime: gastown`.
    pub fn build_dag(tasks: &[Task], workflow_name: &str) -> WorkflowDag {
        let nodes = tasks
            .iter()
            .filter(|t| t.runtime == TaskRuntime::Gastown)
            .map(|t| DagNode {
                id: t.id.clone(),
                description: t.description.clone(),
                depends_on: t.depends_on.clone(),
                phase: t.phase,
            })
            .collect();

        WorkflowDag {
            name: workflow_name.to_string(),
            nodes,
        }
    }

    /// Serialise a [`WorkflowDag`] to a JSON string.
    pub fn serialise_dag(dag: &WorkflowDag) -> Result<String> {
        serde_json::to_string_pretty(dag).context("Failed to serialise workflow DAG to JSON")
    }

    // -----------------------------------------------------------------------
    // Workflow submission
    // -----------------------------------------------------------------------

    /// Submit a pre-built workflow DAG to the gastown endpoint.
    ///
    /// Returns the workflow run ID assigned by the service.
    pub async fn submit_workflow(&self, dag: &WorkflowDag) -> Result<String> {
        let url = format!("{}/workflows", self.endpoint);
        let response = self
            .http
            .post(&url)
            .bearer_auth(&self.token)
            .json(dag)
            .send()
            .await
            .context("Failed to submit workflow to gastown")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Gastown submission failed with HTTP {}: {}", status, body);
        }

        #[derive(Deserialize)]
        struct SubmitResponse {
            run_id: String,
        }

        let resp: SubmitResponse = response
            .json()
            .await
            .context("Failed to parse gastown submission response")?;

        Ok(resp.run_id)
    }

    // -----------------------------------------------------------------------
    // Polling and state updates
    // -----------------------------------------------------------------------

    /// Poll the gastown service for status events on a given workflow run.
    pub async fn poll_status(&self, run_id: &str) -> Result<Vec<GastownStatusEvent>> {
        let url = format!("{}/workflows/{}/status", self.endpoint, run_id);
        let response = self
            .http
            .get(&url)
            .bearer_auth(&self.token)
            .send()
            .await
            .context("Failed to poll gastown status")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            bail!("Gastown status poll failed with HTTP {}: {}", status, body);
        }

        #[derive(Deserialize)]
        struct StatusResponse {
            events: Vec<GastownStatusEvent>,
        }

        let resp: StatusResponse = response
            .json()
            .await
            .context("Failed to parse gastown status response")?;

        Ok(resp.events)
    }

    /// Apply a slice of [`GastownStatusEvent`]s to the task file at `task_path`.
    ///
    /// Tasks reported as `completed` are marked [`TaskStatus::Completed`];
    /// tasks reported as `failed` are marked [`TaskStatus::Failed`].
    /// Events for unknown task IDs are silently ignored.
    pub fn apply_status_events(events: &[GastownStatusEvent], task_path: &Path) -> Result<()> {
        use crate::task_manager::{load_tasks, save_tasks};

        let mut tasks = load_tasks(task_path)?;
        for event in events {
            if let Some(task) = tasks.iter_mut().find(|t| t.id == event.task_id) {
                task.status = match event.status {
                    GastownTaskStatus::Completed => TaskStatus::Completed,
                    GastownTaskStatus::Failed => TaskStatus::Failed,
                    GastownTaskStatus::Running => TaskStatus::InProgress,
                };
            }
        }
        save_tasks(task_path, &tasks)?;
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind};
    use tempfile::tempdir;

    fn make_task(id: &str, runtime: TaskRuntime, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status: TaskStatus::Pending,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime,
            precondition_prompt: None,
        }
    }

    // ---- DAG serialisation ----

    #[test]
    fn build_dag_includes_only_gastown_tasks() {
        let tasks = vec![
            make_task("local-a", TaskRuntime::Local, vec![]),
            make_task("gastown-b", TaskRuntime::Gastown, vec![]),
            make_task("local-c", TaskRuntime::Local, vec![]),
            make_task("gastown-d", TaskRuntime::Gastown, vec!["gastown-b"]),
        ];

        let dag = GastownClient::build_dag(&tasks, "my-workflow");
        assert_eq!(dag.name, "my-workflow");
        assert_eq!(dag.nodes.len(), 2);
        assert_eq!(dag.nodes[0].id, "gastown-b");
        assert_eq!(dag.nodes[1].id, "gastown-d");
        assert_eq!(dag.nodes[1].depends_on, vec!["gastown-b"]);
    }

    #[test]
    fn build_dag_empty_when_no_gastown_tasks() {
        let tasks = vec![
            make_task("local-a", TaskRuntime::Local, vec![]),
            make_task("local-b", TaskRuntime::Local, vec![]),
        ];
        let dag = GastownClient::build_dag(&tasks, "empty");
        assert!(dag.nodes.is_empty());
    }

    #[test]
    fn serialise_dag_produces_valid_json() {
        let dag = WorkflowDag {
            name: "test-workflow".to_string(),
            nodes: vec![
                DagNode {
                    id: "a".to_string(),
                    description: "task a".to_string(),
                    depends_on: vec![],
                    phase: 1,
                },
                DagNode {
                    id: "b".to_string(),
                    description: "task b".to_string(),
                    depends_on: vec!["a".to_string()],
                    phase: 2,
                },
            ],
        };

        let json = GastownClient::serialise_dag(&dag).unwrap();
        let parsed: WorkflowDag = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, dag);
    }

    #[test]
    fn serialise_dag_omits_empty_depends_on() {
        let dag = WorkflowDag {
            name: "w".to_string(),
            nodes: vec![DagNode {
                id: "a".to_string(),
                description: "task a".to_string(),
                depends_on: vec![],
                phase: 1,
            }],
        };
        let json = GastownClient::serialise_dag(&dag).unwrap();
        assert!(
            !json.contains("depends_on"),
            "empty depends_on should be omitted"
        );
    }

    #[test]
    fn dag_node_roundtrip() {
        let node = DagNode {
            id: "x".to_string(),
            description: "do the thing".to_string(),
            depends_on: vec!["y".to_string(), "z".to_string()],
            phase: 3,
        };
        let json = serde_json::to_string(&node).unwrap();
        let loaded: DagNode = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, node);
    }

    // ---- Status events ----

    #[test]
    fn gastown_status_event_roundtrip() {
        let event = GastownStatusEvent {
            task_id: "impl-1".to_string(),
            status: GastownTaskStatus::Completed,
        };
        let json = serde_json::to_string(&event).unwrap();
        let loaded: GastownStatusEvent = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, event);
    }

    #[test]
    fn gastown_task_status_serialises_lowercase() {
        assert_eq!(
            serde_json::to_string(&GastownTaskStatus::Completed).unwrap(),
            "\"completed\""
        );
        assert_eq!(
            serde_json::to_string(&GastownTaskStatus::Failed).unwrap(),
            "\"failed\""
        );
        assert_eq!(
            serde_json::to_string(&GastownTaskStatus::Running).unwrap(),
            "\"running\""
        );
    }

    // ---- apply_status_events ----

    #[test]
    fn apply_status_events_marks_completed() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![
            make_task("a", TaskRuntime::Gastown, vec![]),
            make_task("b", TaskRuntime::Gastown, vec![]),
        ];
        crate::task_manager::save_tasks(&task_file, &tasks).unwrap();

        let events = vec![GastownStatusEvent {
            task_id: "a".to_string(),
            status: GastownTaskStatus::Completed,
        }];
        GastownClient::apply_status_events(&events, &task_file).unwrap();

        let loaded = crate::task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(loaded[0].status, TaskStatus::Completed);
        assert_eq!(loaded[1].status, TaskStatus::Pending);
    }

    #[test]
    fn apply_status_events_marks_failed() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![make_task("a", TaskRuntime::Gastown, vec![])];
        crate::task_manager::save_tasks(&task_file, &tasks).unwrap();

        let events = vec![GastownStatusEvent {
            task_id: "a".to_string(),
            status: GastownTaskStatus::Failed,
        }];
        GastownClient::apply_status_events(&events, &task_file).unwrap();

        let loaded = crate::task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(loaded[0].status, TaskStatus::Failed);
    }

    #[test]
    fn apply_status_events_marks_in_progress() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![make_task("a", TaskRuntime::Gastown, vec![])];
        crate::task_manager::save_tasks(&task_file, &tasks).unwrap();

        let events = vec![GastownStatusEvent {
            task_id: "a".to_string(),
            status: GastownTaskStatus::Running,
        }];
        GastownClient::apply_status_events(&events, &task_file).unwrap();

        let loaded = crate::task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(loaded[0].status, TaskStatus::InProgress);
    }

    #[test]
    fn apply_status_events_ignores_unknown_task_ids() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![make_task("known", TaskRuntime::Gastown, vec![])];
        crate::task_manager::save_tasks(&task_file, &tasks).unwrap();

        let events = vec![GastownStatusEvent {
            task_id: "ghost".to_string(),
            status: GastownTaskStatus::Completed,
        }];
        GastownClient::apply_status_events(&events, &task_file).unwrap();

        let loaded = crate::task_manager::load_tasks(&task_file).unwrap();
        // "known" remains Pending; no crash for "ghost"
        assert_eq!(loaded[0].status, TaskStatus::Pending);
    }

    #[test]
    fn apply_status_events_handles_empty_event_list() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![make_task("a", TaskRuntime::Gastown, vec![])];
        crate::task_manager::save_tasks(&task_file, &tasks).unwrap();

        GastownClient::apply_status_events(&[], &task_file).unwrap();

        let loaded = crate::task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(loaded[0].status, TaskStatus::Pending);
    }

    // ---- GastownClient::new ----

    #[test]
    fn client_new_returns_none_when_endpoint_absent() {
        assert!(GastownClient::new(None, Some("tok")).is_none());
    }

    #[test]
    fn client_new_returns_none_when_token_absent() {
        assert!(GastownClient::new(Some("https://gastown.example.com"), None).is_none());
    }

    #[test]
    fn client_new_returns_some_when_both_present() {
        assert!(GastownClient::new(Some("https://gastown.example.com"), Some("tok")).is_some());
    }

    #[test]
    fn client_new_strips_trailing_slash_from_endpoint() {
        let client = GastownClient::new(Some("https://gastown.example.com/"), Some("tok")).unwrap();
        assert_eq!(client.endpoint, "https://gastown.example.com");
    }
}
