/// End-to-end integration tests for eval-3.
///
/// Exercises all agent swarm features working together in a single scenario:
/// role-based routing, dynamic task generation, agent memory persistence, and
/// intelligent scheduling.
#[cfg(test)]
mod tests {
    use crate::headless_state::{
        load_headless_state, save_headless_state, AgentPhase, HeadlessState,
    };
    use crate::ralph_loop::TaskScheduler;
    use crate::task_manager::{
        append_task, filter_tasks_by_role, generate_task_id, load_tasks, save_tasks,
    };
    use crate::types::{AgentRole, Task, TaskStatus};
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
}
