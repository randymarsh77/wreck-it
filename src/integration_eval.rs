/// End-to-end integration tests for eval-3 and eval-5.
///
/// Exercises all agent swarm features working together in a single scenario:
/// role-based routing, dynamic task generation, agent memory persistence, and
/// intelligent scheduling (eval-3); and adaptive re-planning on failure and
/// typed artefact store (eval-5).
#[cfg(test)]
mod tests {
    use crate::artefact_store::{load_manifest, persist_output_artefacts, resolve_input_artefacts};
    use crate::headless_state::{load_headless_state, save_headless_state, AgentPhase};
    use crate::ralph_loop::TaskScheduler;
    use crate::replanner::parse_and_validate_replan;
    use crate::task_manager::{
        append_task, filter_tasks_by_role, generate_task_id, load_tasks, save_tasks,
    };
    use crate::types::{AgentRole, ArtefactKind, Task, TaskArtefact, TaskStatus};
    use std::fs;
    use tempfile::tempdir;

    fn make_task(
        id: &str,
        role: AgentRole,
        status: TaskStatus,
        phase: u32,
        priority: u32,
        complexity: u32,
        depends_on: Vec<&str>,
    ) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role,
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            priority,
            complexity,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
        }
    }

    /// End-to-end integration scenario that exercises all new agent swarm
    /// features together: role-based routing, dynamic task generation, agent
    /// memory persistence, and intelligent scheduling.
    ///
    /// Scenario: a three-phase software project.
    ///   Phase 1 – an `ideas` task researches requirements.
    ///   Phase 2 – an `implementer` task writes the code (blocked on ideas).
    ///   Phase 3 – an `evaluator` task validates the work (blocked on impl).
    ///
    /// During execution the ideas agent dynamically generates an extra
    /// implementer sub-task, and each "cron invocation" persists memory to the
    /// headless state file so subsequent invocations can recall context.
    #[test]
    fn eval3_end_to_end_agent_swarm_workflow() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");
        let state_file = dir.path().join(".wreck-it-state.json");

        // ── Step 1: Build a mixed-role task list ─────────────────────────────
        let initial_tasks = vec![
            make_task(
                "ideas-1",
                AgentRole::Ideas,
                TaskStatus::Pending,
                1,
                5,
                3,
                vec![],
            ),
            make_task(
                "impl-1",
                AgentRole::Implementer,
                TaskStatus::Pending,
                2,
                0,
                5,
                vec!["ideas-1"],
            ),
            make_task(
                "eval-1",
                AgentRole::Evaluator,
                TaskStatus::Pending,
                3,
                0,
                2,
                vec!["impl-1"],
            ),
        ];
        save_tasks(&task_file, &initial_tasks).unwrap();

        // ── Step 2: Role-based routing ────────────────────────────────────────
        let tasks = load_tasks(&task_file).unwrap();
        assert_eq!(filter_tasks_by_role(&tasks, AgentRole::Ideas).len(), 1);
        assert_eq!(
            filter_tasks_by_role(&tasks, AgentRole::Implementer).len(),
            1
        );
        assert_eq!(filter_tasks_by_role(&tasks, AgentRole::Evaluator).len(), 1);
        assert_eq!(
            filter_tasks_by_role(&tasks, AgentRole::Ideas)[0].id,
            "ideas-1"
        );

        // ── Step 3: Intelligent scheduling ───────────────────────────────────
        // Only ideas-1 is ready; impl-1 and eval-1 are blocked by dependencies.
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready.len(), 1, "only ideas-1 is unblocked");
        assert_eq!(tasks[ready[0]].id, "ideas-1");

        // Simulate ideas-1 completing → impl-1 becomes unblocked.
        let mut tasks = load_tasks(&task_file).unwrap();
        tasks[0].status = TaskStatus::Completed;
        save_tasks(&task_file, &tasks).unwrap();

        let tasks = load_tasks(&task_file).unwrap();
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready.len(), 1, "impl-1 is now unblocked");
        assert_eq!(tasks[ready[0]].id, "impl-1");

        // ── Step 4: Dynamic task generation ──────────────────────────────────
        // The ideas agent dynamically spawns an extra implementer sub-task.
        let tasks_snapshot = load_tasks(&task_file).unwrap();
        let new_id = generate_task_id(&tasks_snapshot, "dyn-impl-");
        assert_eq!(new_id, "dyn-impl-1");

        let dynamic_task = make_task(
            &new_id,
            AgentRole::Implementer,
            TaskStatus::Pending,
            2,
            2,
            4,
            vec!["ideas-1"],
        );
        append_task(&task_file, dynamic_task).unwrap();

        let tasks = load_tasks(&task_file).unwrap();
        assert_eq!(tasks.len(), 4, "dynamic task was appended");
        assert!(
            tasks.iter().any(|t| t.id == "dyn-impl-1"),
            "dynamic task present in file"
        );

        // ── Step 5: Agent memory persistence ─────────────────────────────────
        // First "cron invocation": default state, write first memory entry.
        let mut state = load_headless_state(&state_file).unwrap();
        assert_eq!(state.iteration, 0);
        assert!(state.memory.is_empty());

        state.iteration = 1;
        state.current_task_id = Some("ideas-1".to_string());
        state
            .memory
            .push("iteration 1: triggered ideas-1 (issue #10)".to_string());
        save_headless_state(&state_file, &state).unwrap();

        // Second invocation: memory from iteration 1 is still present.
        let mut state = load_headless_state(&state_file).unwrap();
        assert_eq!(state.iteration, 1);
        assert_eq!(state.memory.len(), 1);

        state.iteration = 2;
        state.phase = AgentPhase::NeedsVerification;
        state
            .memory
            .push("iteration 2: impl-1 PR #20 created".to_string());
        save_headless_state(&state_file, &state).unwrap();

        // Third invocation: both memory entries survive across restarts.
        let state = load_headless_state(&state_file).unwrap();
        assert_eq!(state.iteration, 2);
        assert_eq!(state.memory.len(), 2, "memory persists across invocations");
        assert!(state.memory[0].contains("ideas-1"));
        assert!(state.memory[1].contains("impl-1"));

        // ── Step 6: All tasks complete; scheduler is empty ────────────────────
        let mut tasks = load_tasks(&task_file).unwrap();
        for task in &mut tasks {
            task.status = TaskStatus::Completed;
        }
        save_tasks(&task_file, &tasks).unwrap();

        let tasks = load_tasks(&task_file).unwrap();
        assert!(
            tasks.iter().all(|t| t.status == TaskStatus::Completed),
            "all tasks are completed"
        );
        assert!(
            TaskScheduler::schedule(&tasks).is_empty(),
            "scheduler returns nothing when all tasks complete"
        );
    }

    /// End-to-end integration scenario for eval-5: adaptive re-planning on
    /// failure (impl-7) and typed artefact store (impl-8).
    ///
    /// Verifies:
    /// 1. After exceeding the replan threshold the re-planner produces a valid
    ///    modified task list that is persisted correctly.
    /// 2. Circular dependency and duplicate ID guards reject invalid re-planner
    ///    outputs.
    /// 3. Artefact outputs are persisted after task completion and correctly
    ///    resolved for downstream task prompts.
    /// 4. Tasks without inputs/outputs behave identically to pre-impl-8 behaviour.
    #[test]
    fn eval5_replanning_and_artefact_store_integration() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");

        // ── Step 1: Build a three-task pipeline with artefacts ───────────────
        //   design  →  impl  →  review
        //   design outputs a "spec" artefact consumed by impl.
        //   impl has no artefacts (backward-compat check).
        //   review has no inputs/outputs (backward-compat check).
        let design = Task {
            id: "design-1".to_string(),
            description: "Write design spec".to_string(),
            status: TaskStatus::Pending,
            role: AgentRole::Ideas,
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 5,
            complexity: 2,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![TaskArtefact {
                kind: ArtefactKind::Summary,
                name: "spec".to_string(),
                path: "spec.md".to_string(),
            }],
            runtime: crate::types::TaskRuntime::default(),
        };

        let impl_task = Task {
            id: "impl-1".to_string(),
            description: "Implement feature".to_string(),
            status: TaskStatus::Pending,
            role: AgentRole::Implementer,
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 2,
            depends_on: vec!["design-1".to_string()],
            priority: 3,
            complexity: 5,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec!["design-1/spec".to_string()],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
        };

        let review = make_task(
            "review-1",
            AgentRole::Evaluator,
            TaskStatus::Pending,
            3,
            0,
            1,
            vec!["impl-1"],
        );

        save_tasks(&task_file, &[design, impl_task, review]).unwrap();

        // ── Step 2: Backward compatibility – tasks without inputs/outputs ─────
        // review-1 has no inputs/outputs; round-tripping must preserve that.
        let loaded = load_tasks(&task_file).unwrap();
        let review_loaded = loaded.iter().find(|t| t.id == "review-1").unwrap();
        assert!(
            review_loaded.inputs.is_empty(),
            "review-1 inputs should be empty (backward compat)"
        );
        assert!(
            review_loaded.outputs.is_empty(),
            "review-1 outputs should be empty (backward compat)"
        );

        // The manifest must not exist yet (no artefacts persisted).
        assert!(
            !manifest_path.exists(),
            "manifest must not exist before any artefact is persisted"
        );

        // ── Step 3: Persist an output artefact for design-1 ──────────────────
        let spec_file = dir.path().join("spec.md");
        fs::write(&spec_file, "# Feature Specification\n\nBuild X with Y.").unwrap();

        let outputs = vec![TaskArtefact {
            kind: ArtefactKind::Summary,
            name: "spec".to_string(),
            path: "spec.md".to_string(),
        }];
        persist_output_artefacts(&manifest_path, "design-1", &outputs, dir.path()).unwrap();

        let manifest = load_manifest(&manifest_path).unwrap();
        let entry = manifest.artefacts.get("design-1/spec").unwrap();
        assert_eq!(entry.kind, ArtefactKind::Summary);
        assert!(entry.content.contains("Feature Specification"));

        // ── Step 4: Resolve artefact into downstream task prompt ──────────────
        let inputs = vec!["design-1/spec".to_string()];
        let resolved = resolve_input_artefacts(&manifest_path, &inputs).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "design-1/spec");
        assert!(resolved[0].1.contains("Feature Specification"));

        // ── Step 5: Simulate repeated task failure → threshold reached ────────
        // We simulate the re-planner being invoked (without a real LLM call)
        // by exercising parse_and_validate_replan directly with LLM-like output.
        let mut tasks = load_tasks(&task_file).unwrap();
        // Mark impl-1 as having failed 3 times (as if threshold=3 was reached).
        let impl_idx = tasks.iter().position(|t| t.id == "impl-1").unwrap();
        tasks[impl_idx].status = TaskStatus::Failed;
        tasks[impl_idx].failed_attempts = 3;
        save_tasks(&task_file, &tasks).unwrap();

        // Re-planner LLM response: split impl-1 into impl-1a and impl-1b.
        let replan_output = r#"[
            {"id":"design-1","description":"Write design spec","status":"completed","phase":1,"depends_on":[],"priority":5,"complexity":2,"failed_attempts":0},
            {"id":"impl-1a","description":"Implement feature – part A","status":"pending","phase":2,"depends_on":["design-1"],"priority":3,"complexity":3,"failed_attempts":0},
            {"id":"impl-1b","description":"Implement feature – part B","status":"pending","phase":2,"depends_on":["impl-1a"],"priority":3,"complexity":3,"failed_attempts":0},
            {"id":"review-1","description":"task review-1","status":"pending","phase":3,"depends_on":["impl-1b"],"priority":0,"complexity":1,"failed_attempts":0}
        ]"#;

        let current_tasks = load_tasks(&task_file).unwrap();
        let updated = parse_and_validate_replan(&current_tasks, replan_output).unwrap();
        assert_eq!(updated.len(), 4, "re-planner split impl-1 into two tasks");
        assert!(
            updated.iter().any(|t| t.id == "impl-1a"),
            "impl-1a present after re-plan"
        );
        assert!(
            updated.iter().any(|t| t.id == "impl-1b"),
            "impl-1b present after re-plan"
        );
        // The completed design-1 must stay completed regardless of LLM output.
        let design_updated = updated.iter().find(|t| t.id == "design-1").unwrap();
        assert_eq!(
            design_updated.status,
            TaskStatus::Completed,
            "completed task status must be preserved by re-planner"
        );

        // Persist the re-planned task list as the loop would.
        save_tasks(&task_file, &updated).unwrap();
        let persisted = load_tasks(&task_file).unwrap();
        assert_eq!(persisted.len(), 4, "re-planned list persisted correctly");

        // ── Step 6: Re-planner rejects circular dependency ───────────────────
        let circular = r#"[
            {"id":"x","description":"x","status":"pending","depends_on":["y"]},
            {"id":"y","description":"y","status":"pending","depends_on":["x"]}
        ]"#;
        let err = parse_and_validate_replan(&persisted, circular).unwrap_err();
        assert!(
            err.to_string().contains("circular dependency"),
            "circular dependency guard fired: {}",
            err
        );

        // ── Step 7: Re-planner rejects duplicate IDs ─────────────────────────
        let duplicate = r#"[
            {"id":"impl-1a","description":"a","status":"pending"},
            {"id":"impl-1a","description":"b","status":"pending"}
        ]"#;
        let err = parse_and_validate_replan(&persisted, duplicate).unwrap_err();
        assert!(
            err.to_string().contains("Duplicate task ID"),
            "duplicate ID guard fired: {}",
            err
        );

        // ── Step 8: Manifest accumulates artefacts across tasks ───────────────
        let output_file = dir.path().join("result.json");
        fs::write(&output_file, r#"{"status":"ok"}"#).unwrap();

        let more_outputs = vec![TaskArtefact {
            kind: ArtefactKind::Json,
            name: "result".to_string(),
            path: "result.json".to_string(),
        }];
        persist_output_artefacts(&manifest_path, "impl-1a", &more_outputs, dir.path()).unwrap();

        let manifest = load_manifest(&manifest_path).unwrap();
        assert!(
            manifest.artefacts.contains_key("design-1/spec"),
            "design-1/spec still present"
        );
        assert!(
            manifest.artefacts.contains_key("impl-1a/result"),
            "impl-1a/result was added"
        );

        // ── Step 9: Missing artefact reference is rejected ───────────────────
        let bad_inputs = vec!["does-not-exist/artefact".to_string()];
        let err = resolve_input_artefacts(&manifest_path, &bad_inputs).unwrap_err();
        assert!(
            err.to_string().contains("does-not-exist/artefact"),
            "missing artefact error contains reference: {}",
            err
        );

        // ── Step 10: Empty inputs/outputs are a no-op (backward compat) ──────
        // Calling persist with empty outputs must not create a new manifest.
        let dir2 = tempdir().unwrap();
        let manifest_path2 = dir2.path().join("artefacts.json");
        persist_output_artefacts(&manifest_path2, "review-1", &[], dir2.path()).unwrap();
        assert!(
            !manifest_path2.exists(),
            "manifest must not be created for tasks without outputs"
        );
        // Calling resolve with empty inputs must return empty vec.
        let resolved_empty = resolve_input_artefacts(&manifest_path2, &[]).unwrap();
        assert!(
            resolved_empty.is_empty(),
            "empty inputs resolve to empty vec"
        );
    }

    /// End-to-end integration scenario for eval-6: Gastown cloud runtime
    /// integration (impl-9) and Openclaw provenance tracking (impl-10).
    ///
    /// Verifies:
    /// 1. The task graph is correctly serialised as a gastown-compatible
    ///    workflow DAG (only `runtime: gastown` tasks are included, edges are
    ///    preserved).
    /// 2. When `gastown_endpoint` is absent, `GastownClient::new` returns
    ///    `None` (silent disable / local fallback).
    /// 3. Provenance records are written to `.wreck-it-provenance/` for each
    ///    task execution.
    /// 4. `load_provenance_records` returns all records for a task ID, sorted
    ///    by timestamp (the `wreck-it provenance --task <id>` data source).
    #[test]
    fn eval6_gastown_and_provenance_integration() {
        use crate::gastown_client::{GastownClient, GastownStatusEvent, GastownTaskStatus};
        use crate::provenance::{
            hash_string, load_provenance_records, now_timestamp, persist_provenance_record,
            ProvenanceRecord,
        };
        use crate::types::{AgentRole, TaskRuntime};

        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        // ── Step 1: Build a mixed-runtime task list ──────────────────────────
        //   local-a  (local)
        //   gastown-b (gastown)
        //   gastown-c (gastown, depends on gastown-b)
        let tasks = vec![
            {
                let mut t = make_task(
                    "local-a",
                    AgentRole::Implementer,
                    TaskStatus::Pending,
                    1,
                    0,
                    1,
                    vec![],
                );
                t.runtime = TaskRuntime::Local;
                t
            },
            {
                let mut t = make_task(
                    "gastown-b",
                    AgentRole::Implementer,
                    TaskStatus::Pending,
                    1,
                    0,
                    1,
                    vec![],
                );
                t.runtime = TaskRuntime::Gastown;
                t
            },
            {
                let mut t = make_task(
                    "gastown-c",
                    AgentRole::Implementer,
                    TaskStatus::Pending,
                    2,
                    0,
                    1,
                    vec!["gastown-b"],
                );
                t.runtime = TaskRuntime::Gastown;
                t
            },
        ];
        save_tasks(&task_file, &tasks).unwrap();

        // ── Step 2: Requirement 1 – DAG serialisation ────────────────────────
        let loaded = load_tasks(&task_file).unwrap();
        let dag = GastownClient::build_dag(&loaded, "eval-6-workflow");

        assert_eq!(dag.name, "eval-6-workflow");
        // Only the two gastown-runtime tasks should appear in the DAG.
        assert_eq!(dag.nodes.len(), 2, "only gastown tasks in DAG");
        assert_eq!(dag.nodes[0].id, "gastown-b");
        assert_eq!(dag.nodes[1].id, "gastown-c");
        // Dependency edge from gastown-c → gastown-b must be preserved.
        assert_eq!(dag.nodes[1].depends_on, vec!["gastown-b"]);

        // DAG must round-trip through JSON without loss.
        let json = GastownClient::serialise_dag(&dag).unwrap();
        let parsed: crate::gastown_client::WorkflowDag = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed, dag, "DAG round-trips through JSON");

        // ── Step 3: Requirement 2 – absent endpoint → None (local fallback) ──
        let client_no_endpoint = GastownClient::new(None, Some("token"));
        assert!(
            client_no_endpoint.is_none(),
            "no endpoint → client is None (gastown disabled)"
        );
        let client_no_token = GastownClient::new(Some("https://gastown.example.com"), None);
        assert!(
            client_no_token.is_none(),
            "no token → client is None (gastown disabled)"
        );
        // Both present → integration is enabled.
        let client_enabled = GastownClient::new(Some("https://gastown.example.com"), Some("tok"));
        assert!(
            client_enabled.is_some(),
            "both present → gastown integration enabled"
        );

        // ── Step 4: Requirement 3 – provenance records written to disk ───────
        let ts1: u64 = 1_700_000_000;
        let ts2: u64 = 1_700_000_001;

        let record1 = ProvenanceRecord {
            task_id: "gastown-b".to_string(),
            agent_role: AgentRole::Implementer,
            model: "copilot".to_string(),
            prompt_hash: hash_string("implement gastown-b"),
            tool_calls: vec![],
            git_diff_hash: "abcd1234abcd1234".to_string(),
            timestamp: ts1,
            outcome: "success".to_string(),
        };
        let record2 = ProvenanceRecord {
            task_id: "gastown-b".to_string(),
            agent_role: AgentRole::Implementer,
            model: "copilot".to_string(),
            prompt_hash: hash_string("implement gastown-b retry"),
            tool_calls: vec!["evaluate_completeness".to_string()],
            git_diff_hash: "0000000000000000".to_string(),
            timestamp: ts2,
            outcome: "failure".to_string(),
        };

        persist_provenance_record(&record1, dir.path()).unwrap();
        persist_provenance_record(&record2, dir.path()).unwrap();

        let prov_dir = dir.path().join(".wreck-it-provenance");
        assert!(prov_dir.exists(), ".wreck-it-provenance directory created");
        let file1 = prov_dir.join(format!("gastown-b-{}.json", ts1));
        let file2 = prov_dir.join(format!("gastown-b-{}.json", ts2));
        assert!(file1.exists(), "first provenance file written");
        assert!(file2.exists(), "second provenance file written");

        // ── Step 5: Requirement 4 – load_provenance_records retrieves records ─
        // (This is the data source for `wreck-it provenance --task <id>`.)
        let records = load_provenance_records("gastown-b", dir.path()).unwrap();
        assert_eq!(records.len(), 2, "two records found for gastown-b");
        // Records sorted by timestamp ascending.
        assert_eq!(records[0].timestamp, ts1);
        assert_eq!(records[1].timestamp, ts2);
        assert_eq!(records[0].outcome, "success");
        assert_eq!(records[1].outcome, "failure");

        // A task with no records returns an empty Vec (no panic or error).
        let none = load_provenance_records("local-a", dir.path()).unwrap();
        assert!(none.is_empty(), "no records for local-a");

        // A non-existent provenance directory returns an empty Vec.
        let fresh_dir = tempdir().unwrap();
        let none2 = load_provenance_records("any-task", fresh_dir.path()).unwrap();
        assert!(none2.is_empty(), "empty vec when provenance dir absent");

        // ── Step 6: Gastown apply_status_events – local task list updated ─────
        // Even when tasks were dispatched to gastown, the local state file must
        // reflect completion events returned by the polling endpoint.
        let events = vec![
            GastownStatusEvent {
                task_id: "gastown-b".to_string(),
                status: GastownTaskStatus::Completed,
            },
            GastownStatusEvent {
                task_id: "gastown-c".to_string(),
                status: GastownTaskStatus::Failed,
            },
        ];
        GastownClient::apply_status_events(&events, &task_file).unwrap();

        let updated = load_tasks(&task_file).unwrap();
        let b = updated.iter().find(|t| t.id == "gastown-b").unwrap();
        let c = updated.iter().find(|t| t.id == "gastown-c").unwrap();
        let a = updated.iter().find(|t| t.id == "local-a").unwrap();
        assert_eq!(b.status, TaskStatus::Completed, "gastown-b → Completed");
        assert_eq!(c.status, TaskStatus::Failed, "gastown-c → Failed");
        assert_eq!(a.status, TaskStatus::Pending, "local-a untouched");

        // ── Step 7: Provenance records for now_timestamp() are positive ───────
        assert!(now_timestamp() > 0);
    }
}
