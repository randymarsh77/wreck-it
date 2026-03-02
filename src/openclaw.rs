//! Openclaw-compatible provenance export.
//!
//! This module serialises a complete wreck-it run into a single JSON document
//! that is compatible with the openclaw plan-graph visualiser.  The export
//! captures:
//!
//! - The full task graph (nodes, dependencies, roles, and statuses).
//! - Per-task provenance records (model, prompt hash, diff hash, tool calls,
//!   outcome, timestamp).
//! - Artefact links (which outputs each task produced and which inputs it
//!   consumed).
//!
//! ## Usage
//!
//! ```bash
//! wreck-it export-openclaw --output run.openclaw.json
//! ```
//!
//! The resulting JSON can be loaded into the openclaw UI for interactive
//! inspection of the audit trail.

use crate::artefact_store::load_manifest;
use crate::provenance::load_provenance_records;
use crate::task_manager::load_tasks;
use crate::types::{AgentRole, Task, TaskStatus};
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

// ---------------------------------------------------------------------------
// Openclaw document schema
// ---------------------------------------------------------------------------

/// Top-level openclaw export document.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenclawDocument {
    /// Schema version for forward-compatibility.
    pub schema_version: String,
    /// Unix timestamp (seconds) when the document was exported.
    pub exported_at: u64,
    /// The workflow captured in this document.
    pub workflow: OpenclawWorkflow,
}

/// A workflow (wreck-it run) represented as a set of annotated nodes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenclawWorkflow {
    /// Human-readable workflow name.
    pub name: String,
    /// All task nodes in the workflow.
    pub nodes: Vec<OpenclawNode>,
}

/// A single task node with its provenance and artefact metadata.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenclawNode {
    /// Task identifier.
    pub id: String,
    /// Human-readable description.
    pub description: String,
    /// Final execution status of the task.
    pub status: TaskStatus,
    /// Agent role responsible for this task.
    pub role: AgentRole,
    /// IDs of upstream tasks that this task depended on.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,
    /// All provenance records written for this task (one per execution attempt),
    /// sorted by timestamp ascending.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub provenance: Vec<OpenclawProvenance>,
    /// Artefact links declared for this task.
    #[serde(default, skip_serializing_if = "OpenclawArtefacts::is_empty")]
    pub artefacts: OpenclawArtefacts,
}

/// Condensed provenance entry (derived from [`crate::provenance::ProvenanceRecord`]).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct OpenclawProvenance {
    /// Unix timestamp (seconds) of the execution attempt.
    pub timestamp: u64,
    /// Model / provider used for this attempt.
    pub model: String,
    /// Hex hash of the prompt sent to the agent.
    pub prompt_hash: String,
    /// Hex hash of the git diff produced by this attempt.
    pub git_diff_hash: String,
    /// Tool calls made during this attempt.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<String>,
    /// Outcome of the attempt: `"success"` or `"failure"`.
    pub outcome: String,
}

/// Artefact declarations for a task node.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OpenclawArtefacts {
    /// Input artefact references consumed by this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,
    /// Output artefact references produced by this task.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<String>,
}

impl OpenclawArtefacts {
    /// Returns `true` when both `inputs` and `outputs` are empty, used by
    /// serde's `skip_serializing_if`.
    pub fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.outputs.is_empty()
    }
}

// ---------------------------------------------------------------------------
// Export logic
// ---------------------------------------------------------------------------

/// Build an [`OpenclawDocument`] from the on-disk state in `work_dir`.
///
/// Reads:
/// - `task_file` – the wreck-it task list.
/// - `.wreck-it-provenance/` – one JSON file per task execution attempt.
/// - `.wreck-it-artefacts.json` – the artefact manifest (if present).
///
/// Returns a fully-populated [`OpenclawDocument`] ready for serialisation.
pub fn build_document(
    task_file: &Path,
    work_dir: &Path,
    workflow_name: &str,
) -> Result<OpenclawDocument> {
    let tasks = load_tasks(task_file).context("Failed to load task list for openclaw export")?;

    let manifest_path = work_dir.join(".wreck-it-artefacts.json");
    let manifest = load_manifest(&manifest_path)
        .context("Failed to load artefact manifest for openclaw export")?;

    let exported_at = crate::provenance::now_timestamp();

    let nodes = tasks
        .iter()
        .map(|task| build_node(task, work_dir, &manifest))
        .collect::<Result<Vec<_>>>()?;

    Ok(OpenclawDocument {
        schema_version: "1.0".to_string(),
        exported_at,
        workflow: OpenclawWorkflow {
            name: workflow_name.to_string(),
            nodes,
        },
    })
}

fn build_node(
    task: &Task,
    work_dir: &Path,
    manifest: &crate::artefact_store::ArtefactManifest,
) -> Result<OpenclawNode> {
    // Collect all provenance records for this task.
    let prov_records =
        load_provenance_records(&task.id, work_dir).context("Failed to load provenance records")?;

    let provenance = prov_records
        .into_iter()
        .map(|r| OpenclawProvenance {
            timestamp: r.timestamp,
            model: r.model,
            prompt_hash: r.prompt_hash,
            git_diff_hash: r.git_diff_hash,
            tool_calls: r.tool_calls,
            outcome: r.outcome,
        })
        .collect();

    // Resolve artefact output keys for this task from the manifest.
    let prefix = format!("{}/", task.id);
    let output_keys: Vec<String> = manifest
        .artefacts
        .keys()
        .filter(|k| k.starts_with(&prefix))
        .cloned()
        .collect();

    let artefacts = OpenclawArtefacts {
        inputs: task.inputs.clone(),
        outputs: output_keys,
    };

    Ok(OpenclawNode {
        id: task.id.clone(),
        description: task.description.clone(),
        status: task.status,
        role: task.role,
        depends_on: task.depends_on.clone(),
        provenance,
        artefacts,
    })
}

/// Serialise `doc` to a pretty-printed JSON string.
pub fn serialise_document(doc: &OpenclawDocument) -> Result<String> {
    serde_json::to_string_pretty(doc).context("Failed to serialise openclaw document")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artefact_store::persist_output_artefacts;
    use crate::provenance::{hash_string, persist_provenance_record, ProvenanceRecord};
    use crate::task_manager::save_tasks;
    use crate::types::{
        AgentRole, ArtefactKind, Task, TaskArtefact, TaskKind, TaskRuntime, TaskStatus,
    };
    use std::fs;
    use tempfile::tempdir;

    fn make_task(id: &str, status: TaskStatus, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role: AgentRole::Implementer,
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
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
        }
    }

    fn make_provenance(task_id: &str, timestamp: u64, outcome: &str) -> ProvenanceRecord {
        ProvenanceRecord {
            task_id: task_id.to_string(),
            agent_role: AgentRole::Implementer,
            model: "copilot".to_string(),
            prompt_hash: hash_string(task_id),
            tool_calls: vec![],
            git_diff_hash: "0000000000000000".to_string(),
            timestamp,
            outcome: outcome.to_string(),
        }
    }

    // ---- OpenclawArtefacts::is_empty ----

    #[test]
    fn artefacts_is_empty_when_both_empty() {
        let a = OpenclawArtefacts::default();
        assert!(a.is_empty());
    }

    #[test]
    fn artefacts_not_empty_when_inputs_present() {
        let a = OpenclawArtefacts {
            inputs: vec!["task-1/spec".to_string()],
            outputs: vec![],
        };
        assert!(!a.is_empty());
    }

    #[test]
    fn artefacts_not_empty_when_outputs_present() {
        let a = OpenclawArtefacts {
            inputs: vec![],
            outputs: vec!["task-2/result".to_string()],
        };
        assert!(!a.is_empty());
    }

    // ---- Document round-trip ----

    #[test]
    fn document_roundtrip_through_json() {
        let doc = OpenclawDocument {
            schema_version: "1.0".to_string(),
            exported_at: 1_700_000_000,
            workflow: OpenclawWorkflow {
                name: "test".to_string(),
                nodes: vec![OpenclawNode {
                    id: "t1".to_string(),
                    description: "task 1".to_string(),
                    status: TaskStatus::Completed,
                    role: AgentRole::Implementer,
                    depends_on: vec![],
                    provenance: vec![],
                    artefacts: OpenclawArtefacts::default(),
                }],
            },
        };
        let json = serialise_document(&doc).unwrap();
        let loaded: OpenclawDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, doc);
    }

    // ---- build_document ----

    #[test]
    fn build_document_produces_one_node_per_task() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![
            make_task("a", TaskStatus::Completed, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        save_tasks(&task_file, &tasks).unwrap();

        let doc = build_document(&task_file, dir.path(), "my-run").unwrap();
        assert_eq!(doc.schema_version, "1.0");
        assert_eq!(doc.workflow.name, "my-run");
        assert_eq!(doc.workflow.nodes.len(), 2);
        assert_eq!(doc.workflow.nodes[0].id, "a");
        assert_eq!(doc.workflow.nodes[1].id, "b");
        assert_eq!(doc.workflow.nodes[1].depends_on, vec!["a"]);
    }

    #[test]
    fn build_document_includes_provenance_records() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![make_task("impl-1", TaskStatus::Completed, vec![])];
        save_tasks(&task_file, &tasks).unwrap();

        // Write two provenance records for impl-1.
        persist_provenance_record(&make_provenance("impl-1", 1_000, "success"), dir.path())
            .unwrap();
        persist_provenance_record(&make_provenance("impl-1", 2_000, "failure"), dir.path())
            .unwrap();

        let doc = build_document(&task_file, dir.path(), "run").unwrap();
        let node = &doc.workflow.nodes[0];
        assert_eq!(node.provenance.len(), 2);
        // Sorted by timestamp ascending.
        assert_eq!(node.provenance[0].timestamp, 1_000);
        assert_eq!(node.provenance[0].outcome, "success");
        assert_eq!(node.provenance[1].timestamp, 2_000);
        assert_eq!(node.provenance[1].outcome, "failure");
    }

    #[test]
    fn build_document_includes_artefact_outputs() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");

        let tasks = vec![make_task("design-1", TaskStatus::Completed, vec![])];
        save_tasks(&task_file, &tasks).unwrap();

        // Write an artefact for design-1.
        fs::write(dir.path().join("spec.md"), "# Spec").unwrap();
        persist_output_artefacts(
            &manifest_path,
            "design-1",
            &[TaskArtefact {
                kind: ArtefactKind::Summary,
                name: "spec".to_string(),
                path: "spec.md".to_string(),
            }],
            dir.path(),
        )
        .unwrap();

        let doc = build_document(&task_file, dir.path(), "run").unwrap();
        let node = &doc.workflow.nodes[0];
        assert_eq!(node.artefacts.outputs, vec!["design-1/spec"]);
    }

    #[test]
    fn build_document_includes_artefact_inputs() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let mut task = make_task("impl-1", TaskStatus::Pending, vec!["design-1"]);
        task.inputs = vec!["design-1/spec".to_string()];
        save_tasks(&task_file, &[task]).unwrap();

        let doc = build_document(&task_file, dir.path(), "run").unwrap();
        let node = &doc.workflow.nodes[0];
        assert_eq!(node.artefacts.inputs, vec!["design-1/spec"]);
    }

    #[test]
    fn build_document_empty_provenance_and_artefacts_when_none() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![make_task("t1", TaskStatus::Pending, vec![])];
        save_tasks(&task_file, &tasks).unwrap();

        let doc = build_document(&task_file, dir.path(), "run").unwrap();
        let node = &doc.workflow.nodes[0];
        assert!(node.provenance.is_empty());
        assert!(node.artefacts.is_empty());
    }

    #[test]
    fn serialise_document_produces_valid_json() {
        let doc = OpenclawDocument {
            schema_version: "1.0".to_string(),
            exported_at: 1_700_000_000,
            workflow: OpenclawWorkflow {
                name: "w".to_string(),
                nodes: vec![],
            },
        };
        let json = serialise_document(&doc).unwrap();
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["schema_version"], "1.0");
        assert_eq!(v["workflow"]["name"], "w");
    }

    #[test]
    fn serialise_document_omits_empty_artefacts_field() {
        let doc = OpenclawDocument {
            schema_version: "1.0".to_string(),
            exported_at: 0,
            workflow: OpenclawWorkflow {
                name: "w".to_string(),
                nodes: vec![OpenclawNode {
                    id: "t".to_string(),
                    description: "d".to_string(),
                    status: TaskStatus::Pending,
                    role: AgentRole::Implementer,
                    depends_on: vec![],
                    provenance: vec![],
                    artefacts: OpenclawArtefacts::default(),
                }],
            },
        };
        let json = serialise_document(&doc).unwrap();
        assert!(
            !json.contains("\"artefacts\""),
            "empty artefacts block should be omitted"
        );
    }
}
