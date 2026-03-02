/// End-to-end integration tests for eval-3, eval-5, eval-6, and eval-7.
///
/// Exercises all agent swarm features working together in a single scenario:
/// role-based routing, dynamic task generation, agent memory persistence, and
/// intelligent scheduling (eval-3); and adaptive re-planning on failure and
/// typed artefact store (eval-5); Gastown cloud runtime integration and
/// Openclaw provenance tracking (eval-6); and the full Horizon 2–3 acceptance
/// gate that combines every feature end-to-end (eval-7).
#[cfg(test)]
mod tests {
    use crate::artefact_store::{load_manifest, persist_output_artefacts, resolve_input_artefacts};
    use crate::headless_state::{load_headless_state, save_headless_state, AgentPhase};
    use crate::openclaw::{build_document, serialise_document};
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
            precondition_prompt: None,
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
            precondition_prompt: None,
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
            precondition_prompt: None,
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

    /// Full Horizon 2–3 acceptance gate (eval-7).
    ///
    /// Exercises every feature from impl-5 through impl-10 in a single
    /// realistic scenario:
    ///
    /// Scenario: "Build a REST API" project with four phases:
    ///   1. `plan-1`   (ideas)       – LLM-generated planning task.
    ///   2. `design-1` (ideas)       – Outputs a "spec" artefact consumed by impl.
    ///   3. `impl-1`   (implementer) – Consumes spec; outputs a "code" artefact.
    ///   4. `review-1` (evaluator)   – Reviews impl-1; fails once (tests adaptive
    ///                                  re-planning), then succeeds on re-plan.
    ///
    /// Checks:
    /// R1 – Role-based routing assigns tasks to the correct role pools.
    /// R2 – Intelligent scheduling respects dependency order.
    /// R3 – Artefact chaining: spec flows from design-1 into impl-1's prompt.
    /// R4 – Provenance records are written for every completed step.
    /// R5 – Adaptive re-planning fires when review-1 fails threshold times.
    /// R6 – Openclaw export contains all nodes, provenance, and artefact links.
    /// R7 – Agent memory persists across "cron invocations".
    #[test]
    fn eval7_full_horizon2_horizon3_acceptance_gate() {
        use crate::gastown_client::{GastownClient, GastownStatusEvent, GastownTaskStatus};
        use crate::planner::parse_and_validate_plan;
        use crate::provenance::{hash_string, persist_provenance_record, ProvenanceRecord};
        use crate::types::TaskRuntime;

        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let state_file = dir.path().join(".wreck-it-state.json");

        // ── R1: Role-based routing ───────────────────────────────────────────
        // Simulate the output from 'wreck-it plan' (impl-5): a JSON array
        // that the planner module parses and validates.
        let plan_output = r#"[
            {"id":"plan-1",   "description":"Research requirements and plan the REST API",   "phase":1},
            {"id":"design-1", "description":"Write the API design specification",             "phase":2, "depends_on":["plan-1"]},
            {"id":"impl-1",   "description":"Implement the REST API",                        "phase":3, "depends_on":["design-1"]},
            {"id":"review-1", "description":"Review the REST API implementation for quality","phase":4, "depends_on":["impl-1"]}
        ]"#;

        let mut tasks = parse_and_validate_plan(plan_output).unwrap();
        assert_eq!(tasks.len(), 4, "planner produced 4 tasks");

        // Assign roles to each task (as a specialist-routing step would do).
        tasks[0].role = AgentRole::Ideas;
        tasks[1].role = AgentRole::Ideas;
        tasks[2].role = AgentRole::Implementer;
        tasks[3].role = AgentRole::Evaluator;

        // Add artefact declarations: design-1 outputs a spec; impl-1 consumes it.
        tasks[1].outputs = vec![TaskArtefact {
            kind: ArtefactKind::Summary,
            name: "spec".to_string(),
            path: "spec.md".to_string(),
        }];
        tasks[2].inputs = vec!["design-1/spec".to_string()];
        tasks[2].outputs = vec![TaskArtefact {
            kind: ArtefactKind::Json,
            name: "code".to_string(),
            path: "api.rs".to_string(),
        }];

        save_tasks(&task_file, &tasks).unwrap();

        // Verify role-based routing (R1).
        let loaded = load_tasks(&task_file).unwrap();
        assert_eq!(
            filter_tasks_by_role(&loaded, AgentRole::Ideas).len(),
            2,
            "R1: two ideas tasks"
        );
        assert_eq!(
            filter_tasks_by_role(&loaded, AgentRole::Implementer).len(),
            1,
            "R1: one implementer task"
        );
        assert_eq!(
            filter_tasks_by_role(&loaded, AgentRole::Evaluator).len(),
            1,
            "R1: one evaluator task"
        );

        // ── R2: Intelligent scheduling respects dependency order ─────────────
        let ready = TaskScheduler::schedule(&loaded);
        assert_eq!(ready.len(), 1, "R2: only plan-1 is initially unblocked");
        assert_eq!(loaded[ready[0]].id, "plan-1", "R2: plan-1 is first");

        // ── R3: Artefact chaining ────────────────────────────────────────────
        // Simulate plan-1 and design-1 completing; persist design-1's spec artefact.
        let mut tasks = load_tasks(&task_file).unwrap();
        tasks[0].status = TaskStatus::Completed; // plan-1
        tasks[1].status = TaskStatus::Completed; // design-1
        save_tasks(&task_file, &tasks).unwrap();

        fs::write(
            dir.path().join("spec.md"),
            "# REST API Spec\n\nEndpoints: /users, /items",
        )
        .unwrap();
        let design_outputs = vec![TaskArtefact {
            kind: ArtefactKind::Summary,
            name: "spec".to_string(),
            path: "spec.md".to_string(),
        }];
        persist_output_artefacts(&manifest_path, "design-1", &design_outputs, dir.path()).unwrap();

        // Verify the artefact is accessible for impl-1's input.
        let resolved =
            resolve_input_artefacts(&manifest_path, &["design-1/spec".to_string()]).unwrap();
        assert_eq!(resolved.len(), 1, "R3: artefact resolved");
        assert!(
            resolved[0].1.contains("REST API Spec"),
            "R3: spec content injected"
        );

        // Simulate impl-1 completing; persist its code artefact.
        let mut tasks = load_tasks(&task_file).unwrap();
        tasks[2].status = TaskStatus::Completed; // impl-1
        save_tasks(&task_file, &tasks).unwrap();

        fs::write(dir.path().join("api.rs"), "pub fn router() {}").unwrap();
        let impl_outputs = vec![TaskArtefact {
            kind: ArtefactKind::Json,
            name: "code".to_string(),
            path: "api.rs".to_string(),
        }];
        persist_output_artefacts(&manifest_path, "impl-1", &impl_outputs, dir.path()).unwrap();

        let manifest = load_manifest(&manifest_path).unwrap();
        assert!(
            manifest.artefacts.contains_key("design-1/spec"),
            "R3: design-1/spec in manifest"
        );
        assert!(
            manifest.artefacts.contains_key("impl-1/code"),
            "R3: impl-1/code in manifest"
        );

        // ── R4: Provenance records written for every completed step ──────────
        let ts_base: u64 = 1_710_000_000;
        for (idx, (task_id, role)) in [
            ("plan-1", AgentRole::Ideas),
            ("design-1", AgentRole::Ideas),
            ("impl-1", AgentRole::Implementer),
        ]
        .iter()
        .enumerate()
        {
            let record = ProvenanceRecord {
                task_id: task_id.to_string(),
                agent_role: *role,
                model: "copilot".to_string(),
                prompt_hash: hash_string(task_id),
                tool_calls: vec![],
                git_diff_hash: "0000000000000000".to_string(),
                timestamp: ts_base + idx as u64,
                outcome: "success".to_string(),
            };
            persist_provenance_record(&record, dir.path()).unwrap();
        }

        let prov_dir = dir.path().join(".wreck-it-provenance");
        assert!(prov_dir.exists(), "R4: provenance dir created");
        let entries: Vec<_> = fs::read_dir(&prov_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 3, "R4: three provenance files written");

        // ── R5: Adaptive re-planning fires on repeated review-1 failure ──────
        // review-1 fails twice (threshold=2).  The re-planner splits it into
        // review-1a (quick sanity check) + review-1b (full quality review).
        let mut tasks = load_tasks(&task_file).unwrap();
        let rev_idx = tasks.iter().position(|t| t.id == "review-1").unwrap();
        tasks[rev_idx].status = TaskStatus::Failed;
        tasks[rev_idx].failed_attempts = 2;
        save_tasks(&task_file, &tasks).unwrap();

        let replan_output = r#"[
            {"id":"plan-1",    "description":"Research requirements and plan the REST API",    "status":"completed","phase":1,"depends_on":[]},
            {"id":"design-1",  "description":"Write the API design specification",              "status":"completed","phase":2,"depends_on":["plan-1"]},
            {"id":"impl-1",    "description":"Implement the REST API",                         "status":"completed","phase":3,"depends_on":["design-1"],"inputs":["design-1/spec"],"outputs":[{"kind":"json","name":"code","path":"api.rs"}]},
            {"id":"review-1a", "description":"Sanity-check the REST API implementation",       "status":"pending",  "phase":4,"depends_on":["impl-1"]},
            {"id":"review-1b", "description":"Full quality review of the REST API implementation","status":"pending","phase":5,"depends_on":["review-1a"]}
        ]"#;

        let current_tasks = load_tasks(&task_file).unwrap();
        let replanned = parse_and_validate_replan(&current_tasks, replan_output).unwrap();
        assert_eq!(
            replanned.len(),
            5,
            "R5: re-planner split review-1 into two tasks"
        );
        assert!(
            replanned.iter().any(|t| t.id == "review-1a"),
            "R5: review-1a present"
        );
        assert!(
            replanned.iter().any(|t| t.id == "review-1b"),
            "R5: review-1b present"
        );

        // Completed tasks must remain completed regardless of LLM output.
        let plan1_status = replanned.iter().find(|t| t.id == "plan-1").unwrap().status;
        assert_eq!(
            plan1_status,
            TaskStatus::Completed,
            "R5: plan-1 stays completed"
        );

        save_tasks(&task_file, &replanned).unwrap();

        // Simulate review-1a and review-1b completing successfully.
        let mut tasks = load_tasks(&task_file).unwrap();
        for task in tasks.iter_mut() {
            if task.id == "review-1a" || task.id == "review-1b" {
                task.status = TaskStatus::Completed;
            }
        }
        save_tasks(&task_file, &tasks).unwrap();

        // Write provenance for the two review tasks.
        for (offset, task_id) in [(10u64, "review-1a"), (11u64, "review-1b")] {
            let record = ProvenanceRecord {
                task_id: task_id.to_string(),
                agent_role: AgentRole::Evaluator,
                model: "copilot".to_string(),
                prompt_hash: hash_string(task_id),
                tool_calls: vec!["evaluate_completeness".to_string()],
                git_diff_hash: "0000000000000000".to_string(),
                timestamp: ts_base + offset,
                outcome: "success".to_string(),
            };
            persist_provenance_record(&record, dir.path()).unwrap();
        }

        // ── R6: Openclaw export covers all nodes, provenance, artefacts ──────
        let final_tasks = load_tasks(&task_file).unwrap();
        assert!(
            final_tasks
                .iter()
                .all(|t| t.status == TaskStatus::Completed),
            "R6: all tasks completed before export"
        );

        let doc = build_document(&task_file, dir.path(), "Build REST API").unwrap();
        assert_eq!(doc.schema_version, "1.0");
        assert_eq!(doc.workflow.name, "Build REST API");
        // 5 tasks in the final (re-planned) task list.
        assert_eq!(
            doc.workflow.nodes.len(),
            5,
            "R6: 5 nodes in openclaw export"
        );

        // Every node that had provenance written should appear in the export.
        let plan1_node = doc
            .workflow
            .nodes
            .iter()
            .find(|n| n.id == "plan-1")
            .unwrap();
        assert_eq!(
            plan1_node.provenance.len(),
            1,
            "R6: plan-1 has one provenance record"
        );
        assert_eq!(plan1_node.provenance[0].outcome, "success");

        let impl1_node = doc
            .workflow
            .nodes
            .iter()
            .find(|n| n.id == "impl-1")
            .unwrap();
        assert_eq!(
            impl1_node.provenance.len(),
            1,
            "R6: impl-1 has one provenance record"
        );
        // impl-1 should list its consumed input artefact.
        assert_eq!(
            impl1_node.artefacts.inputs,
            vec!["design-1/spec"],
            "R6: impl-1 input artefact in export"
        );
        // impl-1 should list its produced output artefact.
        assert!(
            impl1_node
                .artefacts
                .outputs
                .contains(&"impl-1/code".to_string()),
            "R6: impl-1/code output artefact in export"
        );

        let design1_node = doc
            .workflow
            .nodes
            .iter()
            .find(|n| n.id == "design-1")
            .unwrap();
        assert!(
            design1_node
                .artefacts
                .outputs
                .contains(&"design-1/spec".to_string()),
            "R6: design-1/spec output artefact in export"
        );

        // The document must serialise to valid JSON.
        let json = serialise_document(&doc).unwrap();
        let reparsed: crate::openclaw::OpenclawDocument = serde_json::from_str(&json).unwrap();
        assert_eq!(
            reparsed.workflow.nodes.len(),
            5,
            "R6: export round-trips correctly"
        );

        // ── R6b: Gastown DAG serialisation is consistent with openclaw export ─
        // Mark review-1a/b as gastown tasks and verify the DAG includes them.
        let mut tasks = load_tasks(&task_file).unwrap();
        for task in tasks.iter_mut() {
            if task.id == "review-1a" || task.id == "review-1b" {
                task.runtime = TaskRuntime::Gastown;
            }
        }
        save_tasks(&task_file, &tasks).unwrap();

        let tasks_for_dag = load_tasks(&task_file).unwrap();
        let dag = GastownClient::build_dag(&tasks_for_dag, "REST-API-review");
        assert_eq!(
            dag.nodes.len(),
            2,
            "R6b: gastown DAG contains two review nodes"
        );
        assert_eq!(dag.nodes[0].id, "review-1a");
        assert_eq!(dag.nodes[1].id, "review-1b");

        // Simulate gastown reporting both review tasks as completed.
        let events = vec![
            GastownStatusEvent {
                task_id: "review-1a".to_string(),
                status: GastownTaskStatus::Completed,
            },
            GastownStatusEvent {
                task_id: "review-1b".to_string(),
                status: GastownTaskStatus::Completed,
            },
        ];
        GastownClient::apply_status_events(&events, &task_file).unwrap();

        let after_events = load_tasks(&task_file).unwrap();
        for task in after_events
            .iter()
            .filter(|t| t.runtime == TaskRuntime::Gastown)
        {
            assert_eq!(
                task.status,
                TaskStatus::Completed,
                "R6b: gastown event applied to {}",
                task.id
            );
        }

        // ── R7: Agent memory persists across invocations ─────────────────────
        let mut state = load_headless_state(&state_file).unwrap();
        state.iteration = 1;
        state
            .memory
            .push("iteration 1: plan-1 completed".to_string());
        save_headless_state(&state_file, &state).unwrap();

        let mut state = load_headless_state(&state_file).unwrap();
        state.iteration = 2;
        state.phase = AgentPhase::NeedsVerification;
        state
            .memory
            .push("iteration 2: review re-plan triggered".to_string());
        save_headless_state(&state_file, &state).unwrap();

        let state = load_headless_state(&state_file).unwrap();
        assert_eq!(state.iteration, 2, "R7: iteration persisted");
        assert_eq!(state.memory.len(), 2, "R7: both memory entries survive");
        assert!(
            state.memory[0].contains("plan-1"),
            "R7: first memory entry intact"
        );
        assert!(
            state.memory[1].contains("re-plan"),
            "R7: second memory entry intact"
        );

        // All tasks completed – scheduler returns nothing.
        let final_tasks = load_tasks(&task_file).unwrap();
        assert!(
            TaskScheduler::schedule(&final_tasks).is_empty(),
            "scheduler empty when all tasks complete"
        );
    }
}
