//! Fan-out / fan-in sub-task spawning.
//!
//! When an `ideas`-role task completes and one of its declared output artefacts
//! has kind [`ArtefactKind::SubTaskManifest`], the runner calls
//! [`detect_and_spawn_fan_out`] to:
//!
//! 1. Parse the JSON content of each sub-task manifest artefact as a
//!    [`SubTaskManifestSpec`].
//! 2. Create one [`Task`] per entry in `sub_tasks`, assigned to
//!    `parent.phase + 1`, with `parent_id` set to the parent task's ID and
//!    the parent's description injected into the sub-task description for
//!    context.
//! 3. Optionally create a fan-in aggregator [`Task`] at `parent.phase + 2`,
//!    with `depends_on` containing all sibling sub-task IDs and `inputs`
//!    pre-populated with every output artefact declared by those siblings.
//!
//! The caller is responsible for appending the returned tasks to the live
//! task list and persisting them to the task file.

use crate::artefact_store::ArtefactManifest;
use crate::types::{
    ArtefactKind, FanInSpec, SubTaskManifestSpec, SubTaskSpec, Task, TaskKind, TaskRuntime,
    TaskStatus,
};
use anyhow::Result;
use std::collections::HashSet;
use std::path::Path;

/// Detect [`ArtefactKind::SubTaskManifest`] outputs from `parent_task` and
/// materialise the described sub-tasks.
///
/// # Parameters
///
/// * `parent_task` – The completed task whose outputs are inspected.
/// * `manifest_path` – Path to the `.wreck-it-artefacts.json` manifest
///   file from which artefact content is loaded.
/// * `existing_task_ids` – IDs already present in the task list; any
///   sub-task or fan-in whose ID matches an existing entry is silently
///   skipped to prevent duplicates.
///
/// Returns the list of newly created tasks (zero or more sub-tasks plus an
/// optional fan-in aggregator).
pub fn detect_and_spawn_fan_out(
    parent_task: &Task,
    manifest_path: &Path,
    existing_task_ids: &HashSet<String>,
) -> Result<Vec<Task>> {
    let mut new_tasks = Vec::new();

    for output in &parent_task.outputs {
        if output.kind != ArtefactKind::SubTaskManifest {
            continue;
        }

        // Load the artefact manifest and retrieve the manifest content.
        let artefact_manifest = crate::artefact_store::load_manifest(manifest_path)?;
        let key = ArtefactManifest::key(&parent_task.id, &output.name);
        let content = match artefact_manifest.artefacts.get(&key) {
            Some(entry) => entry.content.clone(),
            None => {
                tracing::warn!(
                    "fan-out: sub-task manifest artefact '{}' not found in manifest",
                    key
                );
                continue;
            }
        };

        // Parse the manifest spec.
        let spec: SubTaskManifestSpec = match serde_json::from_str(&content) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    "fan-out: failed to parse sub-task manifest '{}': {}",
                    key,
                    e
                );
                continue;
            }
        };

        let (sub_task_ids, sub_tasks) =
            build_sub_tasks(parent_task, &spec.sub_tasks, existing_task_ids);

        if !sub_tasks.is_empty() {
            tracing::info!(
                "fan-out: spawning {} sub-task(s) from parent task '{}'",
                sub_tasks.len(),
                parent_task.id
            );
        }
        new_tasks.extend(sub_tasks.iter().cloned());

        if let Some(fan_in_spec) = &spec.fan_in {
            if !existing_task_ids.contains(&fan_in_spec.id) {
                let fan_in = build_fan_in_task(parent_task, fan_in_spec, &sub_task_ids, &sub_tasks);
                tracing::info!(
                    "fan-out: spawning fan-in task '{}' (phase {})",
                    fan_in.id,
                    fan_in.phase
                );
                new_tasks.push(fan_in);
            }
        }
    }

    Ok(new_tasks)
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build [`Task`] instances for each [`SubTaskSpec`], returning the list of
/// created IDs alongside the task list so the fan-in builder can reference them.
fn build_sub_tasks(
    parent: &Task,
    specs: &[SubTaskSpec],
    existing_ids: &HashSet<String>,
) -> (Vec<String>, Vec<Task>) {
    let sub_phase = parent.phase + 1;
    let mut ids = Vec::new();
    let mut tasks = Vec::new();

    for spec in specs {
        ids.push(spec.id.clone());

        if existing_ids.contains(&spec.id) {
            continue;
        }

        // Inject parent context into the sub-task description so the agent
        // knows the broader goal it is contributing to.
        let description = format!(
            "{}\n\n[Parent task `{}` context]: {}",
            spec.description, parent.id, parent.description,
        );

        // Automatically add any Summary-kind parent outputs as inputs so the
        // sub-task can read the parent's research/planning notes.
        let mut inputs = spec.inputs.clone();
        for parent_out in &parent.outputs {
            if parent_out.kind == ArtefactKind::Summary {
                let artefact_ref = format!("{}/{}", parent.id, parent_out.name);
                if !inputs.contains(&artefact_ref) {
                    inputs.push(artefact_ref);
                }
            }
        }

        tasks.push(Task {
            id: spec.id.clone(),
            description,
            status: TaskStatus::Pending,
            role: spec.role,
            kind: TaskKind::Milestone,
            cooldown_seconds: None,
            phase: sub_phase,
            depends_on: vec![],
            priority: spec.priority.max(parent.priority),
            complexity: spec.complexity,
            timeout_seconds: spec.timeout_seconds,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs,
            outputs: spec.outputs.clone(),
            runtime: TaskRuntime::Local,
            precondition_prompt: None,
            parent_id: Some(parent.id.clone()),
            labels: vec![],
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
        });
    }

    (ids, tasks)
}

/// Build the fan-in aggregator [`Task`] that waits for all sibling sub-tasks.
fn build_fan_in_task(
    parent: &Task,
    spec: &FanInSpec,
    sibling_ids: &[String],
    sibling_tasks: &[Task],
) -> Task {
    let fan_in_phase = parent.phase + 2;

    // Pre-populate inputs with every output artefact declared by the siblings
    // so the aggregator agent automatically has access to all their results.
    let sibling_inputs: Vec<String> = sibling_tasks
        .iter()
        .flat_map(|t| t.outputs.iter().map(|o| format!("{}/{}", t.id, o.name)))
        .collect();

    let description = format!(
        "{}\n\n[Fan-in: aggregate results from {} parallel sub-task(s) \
         spawned by parent task `{}`]",
        spec.description,
        sibling_ids.len(),
        parent.id,
    );

    Task {
        id: spec.id.clone(),
        description,
        status: TaskStatus::Pending,
        role: spec.role,
        kind: TaskKind::Milestone,
        cooldown_seconds: None,
        phase: fan_in_phase,
        depends_on: sibling_ids.to_vec(),
        priority: parent.priority,
        complexity: 1,
        timeout_seconds: None,
        max_retries: None,
        failed_attempts: 0,
        last_attempt_at: None,
        inputs: sibling_inputs,
        outputs: spec.outputs.clone(),
        runtime: TaskRuntime::Local,
        precondition_prompt: None,
        parent_id: Some(parent.id.clone()),
        labels: vec![],
        system_prompt_override: None,
        acceptance_criteria: None,
        evaluation: None,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artefact_store::{ArtefactEntry, ArtefactManifest};
    use crate::types::{AgentRole, ArtefactKind, TaskArtefact};
    use std::collections::HashSet;
    use tempfile::tempdir;

    fn make_parent_task(phase: u32) -> Task {
        Task {
            id: "ideas-parent".to_string(),
            description: "Parent ideas task".to_string(),
            status: TaskStatus::Completed,
            role: AgentRole::Ideas,
            kind: TaskKind::Milestone,
            cooldown_seconds: None,
            phase,
            depends_on: vec![],
            priority: 5,
            complexity: 2,
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![TaskArtefact {
                kind: ArtefactKind::SubTaskManifest,
                name: "subtasks".to_string(),
                path: "subtasks.json".to_string(),
            }],
            runtime: TaskRuntime::Local,
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
        }
    }

    fn write_manifest_artefact(dir: &std::path::Path, parent_id: &str, content: &str) {
        let key = format!("{}/subtasks", parent_id);
        let mut manifest = ArtefactManifest::default();
        manifest.artefacts.insert(
            key,
            ArtefactEntry {
                kind: ArtefactKind::SubTaskManifest,
                name: "subtasks".to_string(),
                path: "subtasks.json".to_string(),
                content: content.to_string(),
            },
        );
        crate::artefact_store::save_manifest(&dir.join(".wreck-it-artefacts.json"), &manifest)
            .unwrap();
    }

    #[test]
    fn no_sub_task_manifest_outputs_returns_empty() {
        let dir = tempdir().unwrap();
        let mut parent = make_parent_task(1);
        // Replace the SubTaskManifest output with a regular File output.
        parent.outputs = vec![TaskArtefact {
            kind: ArtefactKind::File,
            name: "result".to_string(),
            path: "result.txt".to_string(),
        }];

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let result = detect_and_spawn_fan_out(&parent, &manifest_path, &HashSet::new()).unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn spawns_sub_tasks_from_manifest() {
        let dir = tempdir().unwrap();
        let manifest_content = serde_json::json!({
            "sub_tasks": [
                {
                    "id": "impl-a",
                    "description": "Implement A",
                    "role": "implementer",
                    "outputs": [{ "kind": "summary", "name": "result", "path": "a.md" }]
                },
                {
                    "id": "impl-b",
                    "description": "Implement B",
                    "role": "implementer"
                }
            ]
        })
        .to_string();

        let parent = make_parent_task(1);
        write_manifest_artefact(dir.path(), &parent.id, &manifest_content);

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &HashSet::new()).unwrap();

        assert_eq!(tasks.len(), 2);

        // Sub-tasks should be in phase parent.phase + 1 = 2.
        assert_eq!(tasks[0].phase, 2);
        assert_eq!(tasks[1].phase, 2);

        // parent_id should be set.
        assert_eq!(tasks[0].parent_id, Some("ideas-parent".to_string()));
        assert_eq!(tasks[1].parent_id, Some("ideas-parent".to_string()));

        // Status should be Pending.
        assert_eq!(tasks[0].status, TaskStatus::Pending);

        // Description should include parent context.
        assert!(tasks[0].description.contains("Parent ideas task"));

        // Outputs declared in spec should be preserved.
        assert_eq!(tasks[0].outputs.len(), 1);
        assert_eq!(tasks[0].outputs[0].name, "result");
    }

    #[test]
    fn spawns_fan_in_task_with_sibling_inputs() {
        let dir = tempdir().unwrap();
        let manifest_content = serde_json::json!({
            "sub_tasks": [
                {
                    "id": "impl-a",
                    "description": "Implement A",
                    "outputs": [{ "kind": "summary", "name": "result-a", "path": "a.md" }]
                },
                {
                    "id": "impl-b",
                    "description": "Implement B",
                    "outputs": [{ "kind": "summary", "name": "result-b", "path": "b.md" }]
                }
            ],
            "fan_in": {
                "id": "aggregate-results",
                "description": "Aggregate results from A and B",
                "role": "ideas"
            }
        })
        .to_string();

        let parent = make_parent_task(2);
        write_manifest_artefact(dir.path(), &parent.id, &manifest_content);

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &HashSet::new()).unwrap();

        // 2 sub-tasks + 1 fan-in = 3 tasks total.
        assert_eq!(tasks.len(), 3);

        let fan_in = tasks.iter().find(|t| t.id == "aggregate-results").unwrap();

        // Fan-in should be in phase parent.phase + 2 = 4.
        assert_eq!(fan_in.phase, 4);

        // Fan-in depends on both siblings.
        assert!(fan_in.depends_on.contains(&"impl-a".to_string()));
        assert!(fan_in.depends_on.contains(&"impl-b".to_string()));

        // Fan-in inputs should be populated with sibling outputs.
        assert!(fan_in.inputs.contains(&"impl-a/result-a".to_string()));
        assert!(fan_in.inputs.contains(&"impl-b/result-b".to_string()));

        // Fan-in parent_id should be set.
        assert_eq!(fan_in.parent_id, Some("ideas-parent".to_string()));

        // Fan-in role should be Ideas as declared.
        assert_eq!(fan_in.role, AgentRole::Ideas);
    }

    #[test]
    fn skips_duplicate_sub_task_ids() {
        let dir = tempdir().unwrap();
        let manifest_content = serde_json::json!({
            "sub_tasks": [
                { "id": "existing-task", "description": "Already exists" },
                { "id": "new-task", "description": "Brand new" }
            ]
        })
        .to_string();

        let parent = make_parent_task(1);
        write_manifest_artefact(dir.path(), &parent.id, &manifest_content);

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let mut existing = HashSet::new();
        existing.insert("existing-task".to_string());

        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &existing).unwrap();

        // Only new-task should be spawned (existing-task skipped).
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "new-task");
    }

    #[test]
    fn skips_duplicate_fan_in_id() {
        let dir = tempdir().unwrap();
        let manifest_content = serde_json::json!({
            "sub_tasks": [{ "id": "impl-x", "description": "X" }],
            "fan_in": { "id": "already-exists-fan-in", "description": "Aggregate" }
        })
        .to_string();

        let parent = make_parent_task(1);
        write_manifest_artefact(dir.path(), &parent.id, &manifest_content);

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let mut existing = HashSet::new();
        existing.insert("already-exists-fan-in".to_string());

        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &existing).unwrap();

        // Only the sub-task should be spawned; fan-in is skipped.
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "impl-x");
    }

    #[test]
    fn parent_summary_outputs_injected_as_sub_task_inputs() {
        let dir = tempdir().unwrap();
        let manifest_content = serde_json::json!({
            "sub_tasks": [{ "id": "impl-x", "description": "X" }]
        })
        .to_string();

        // Parent has a Summary artefact in its outputs.
        let mut parent = make_parent_task(1);
        parent.outputs = vec![
            TaskArtefact {
                kind: ArtefactKind::SubTaskManifest,
                name: "subtasks".to_string(),
                path: "subtasks.json".to_string(),
            },
            TaskArtefact {
                kind: ArtefactKind::Summary,
                name: "research-notes".to_string(),
                path: "notes.md".to_string(),
            },
        ];

        write_manifest_artefact(dir.path(), &parent.id, &manifest_content);

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &HashSet::new()).unwrap();

        assert_eq!(tasks.len(), 1);
        // The parent summary artefact should be automatically included as input.
        assert!(tasks[0]
            .inputs
            .contains(&"ideas-parent/research-notes".to_string()));
    }

    #[test]
    fn gracefully_handles_malformed_manifest_json() {
        let dir = tempdir().unwrap();
        let parent = make_parent_task(1);
        write_manifest_artefact(dir.path(), &parent.id, "not valid json {{{");

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &HashSet::new()).unwrap();

        // Malformed manifest is silently skipped; no tasks spawned.
        assert!(tasks.is_empty());
    }

    #[test]
    fn sub_task_priority_inherits_parent_priority() {
        let dir = tempdir().unwrap();
        let manifest_content = serde_json::json!({
            "sub_tasks": [{ "id": "impl-x", "description": "X", "priority": 2 }]
        })
        .to_string();

        // Parent has higher priority (5).
        let parent = make_parent_task(1);
        write_manifest_artefact(dir.path(), &parent.id, &manifest_content);

        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let tasks = detect_and_spawn_fan_out(&parent, &manifest_path, &HashSet::new()).unwrap();

        // Priority = max(spec priority 2, parent priority 5) = 5.
        assert_eq!(tasks[0].priority, 5);
    }

    #[test]
    fn build_sub_tasks_returns_all_ids_including_existing() {
        let existing: HashSet<String> = vec!["already-there".to_string()].into_iter().collect();

        let parent = make_parent_task(1);
        let specs = vec![
            SubTaskSpec {
                id: "already-there".to_string(),
                description: "Existing".to_string(),
                role: AgentRole::default(),
                inputs: vec![],
                outputs: vec![],
                timeout_seconds: None,
                priority: 0,
                complexity: 1,
            },
            SubTaskSpec {
                id: "brand-new".to_string(),
                description: "New".to_string(),
                role: AgentRole::default(),
                inputs: vec![],
                outputs: vec![],
                timeout_seconds: None,
                priority: 0,
                complexity: 1,
            },
        ];

        let (ids, tasks) = build_sub_tasks(&parent, &specs, &existing);

        // Both IDs are returned (for depends_on tracking).
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&"already-there".to_string()));
        assert!(ids.contains(&"brand-new".to_string()));

        // Only the new task is created.
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "brand-new");
    }
}
