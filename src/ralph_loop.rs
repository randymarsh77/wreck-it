use crate::agent::AgentClient;
use crate::artefact_store;
use crate::replanner::{replan_and_save, TaskReplanner};
use crate::task_manager::{get_next_task, load_tasks, save_tasks};
use crate::types::{Config, EvaluationMode, LoopState, Task, TaskStatus};
use anyhow::{Context, Result};
use std::collections::HashSet;
use std::time::{SystemTime, UNIX_EPOCH};

/// Error message used when evaluation/tests fail without a prior agent error.
const TEST_FAILURE_ERROR: &str = "Tests failed";

/// Intelligent task scheduler that scores ready tasks across multiple factors
/// and returns them ordered from highest to lowest priority.
pub struct TaskScheduler;

impl TaskScheduler {
    /// Return an ordered list of task indices that are ready to execute
    /// (status `Pending` with all dependencies satisfied), sorted from
    /// highest scheduling score to lowest.
    pub fn schedule(tasks: &[Task]) -> Vec<usize> {
        let completed_ids: HashSet<&str> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .map(|t| t.id.as_str())
            .collect();

        let mut ready: Vec<(usize, f64)> = tasks
            .iter()
            .enumerate()
            .filter(|(_, t)| {
                t.status == TaskStatus::Pending
                    && t.depends_on
                        .iter()
                        .all(|dep| completed_ids.contains(dep.as_str()))
            })
            .map(|(i, t)| (i, Self::score(t, tasks)))
            .collect();

        // Descending score order; stable sort preserves original order on ties.
        ready.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap());
        ready.into_iter().map(|(i, _)| i).collect()
    }

    /// Compute a scheduling score for a single task.  Higher is better.
    ///
    /// Factors (all additive):
    /// - **Priority** (×10): higher `priority` field → run sooner.
    /// - **Complexity** (×2, inverted): lower complexity → quicker win.
    /// - **Dependency fan-out** (×5): tasks that unblock more downstream work
    ///   run sooner.
    /// - **Failed attempts** (×3, penalty): back off from repeatedly-failing
    ///   tasks.
    /// - **Time since last attempt** (up to +60): tasks idle longer get a
    ///   recency bonus to avoid starvation.
    fn score(task: &Task, all_tasks: &[Task]) -> f64 {
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Factor 1: priority
        let priority_score = task.priority as f64 * 10.0;

        // Factor 2: complexity (clamped 1–10; lower = better)
        let clamped_complexity = task.complexity.clamp(1, 10);
        let complexity_score = (11 - clamped_complexity) as f64 * 2.0;

        // Factor 3: number of tasks directly waiting on this one
        let unblocks = all_tasks
            .iter()
            .filter(|t| t.depends_on.contains(&task.id))
            .count();
        let dependency_score = unblocks as f64 * 5.0;

        // Factor 4: failure penalty
        let failure_penalty = task.failed_attempts as f64 * 3.0;

        // Factor 5: recency bonus – capped at 60 points (1 hour of idle time)
        let recency_score = match task.last_attempt_at {
            None => 0.0,
            Some(ts) => {
                let elapsed = now_secs.saturating_sub(ts);
                elapsed.min(3600) as f64 / 60.0
            }
        };

        priority_score + complexity_score + dependency_score - failure_penalty + recency_score
    }
}

/// The Ralph Wiggum Loop - a bash-style loop that continuously executes tasks
pub struct RalphLoop {
    config: Config,
    state: LoopState,
    agent: AgentClient,
}

impl RalphLoop {
    pub fn new(config: Config) -> Self {
        let max_iterations = config.max_iterations;
        let agent = AgentClient::with_evaluation(
            config.model_provider.clone(),
            config.api_endpoint.clone(),
            config.api_token.clone(),
            config.work_dir.to_string_lossy().to_string(),
            config.verification_command.clone(),
            config.evaluation_mode,
            config.completeness_prompt.clone(),
            config.completion_marker_file.to_string_lossy().to_string(),
        );

        Self {
            config,
            state: LoopState::new(max_iterations),
            agent,
        }
    }

    /// Initialize the loop by loading tasks
    pub fn initialize(&mut self) -> Result<()> {
        self.state
            .add_log("Initializing Ralph Wiggum Loop".to_string());

        // Load tasks from file
        let tasks = load_tasks(&self.config.task_file).context("Failed to load tasks")?;

        self.state.tasks = tasks;
        self.state
            .add_log(format!("Loaded {} tasks", self.state.tasks.len()));

        Ok(())
    }

    /// Run a single iteration of the loop (sequential – picks one task).
    pub async fn run_iteration(&mut self) -> Result<bool> {
        self.state.iteration += 1;

        // Check if we've exceeded max iterations
        if self.state.iteration > self.state.max_iterations {
            self.state.add_log("Max iterations reached".to_string());
            return Ok(false);
        }

        // Re-read the task file so that tasks dynamically appended by agents
        // during a previous iteration are incorporated into the queue.
        // We merge by updating in-memory entries with the on-disk version for
        // any ID that exists in both (preserving live InProgress status), and
        // appending any IDs that only exist on disk.
        if let Ok(disk_tasks) = load_tasks(&self.config.task_file) {
            for disk_task in disk_tasks {
                match self.state.tasks.iter().position(|t| t.id == disk_task.id) {
                    Some(idx) => {
                        // Keep the in-memory status (may be InProgress); refresh
                        // everything else (description, role, deps, etc.).
                        let current_status = self.state.tasks[idx].status;
                        self.state.tasks[idx] = disk_task;
                        self.state.tasks[idx].status = current_status;
                    }
                    None => {
                        self.state
                            .add_log(format!("Discovered new task: {}", disk_task.id));
                        self.state.tasks.push(disk_task);
                    }
                }
            }
        }

        // Check if all tasks are complete
        if self.state.all_tasks_complete() {
            self.state.add_log("All tasks completed!".to_string());
            return Ok(false);
        }

        // Check if there are no pending tasks
        if !self.state.has_pending_tasks() {
            self.state.add_log("No pending tasks".to_string());
            return Ok(false);
        }

        // Determine whether we can run tasks in parallel.
        let ready = TaskScheduler::schedule(&self.state.tasks);
        if ready.len() > 1 {
            return self.run_parallel_tasks(ready).await;
        }

        // Single-task execution: use scheduler result when available, fall back
        // to the simple first-pending scan.
        let task_idx = match ready
            .into_iter()
            .next()
            .or_else(|| get_next_task(&self.state.tasks))
        {
            Some(idx) => idx,
            None => {
                self.state.add_log("No more tasks to process".to_string());
                return Ok(false);
            }
        };

        self.run_single_task(task_idx).await
    }

    /// Execute a single task by index, running evaluation & commit logic.
    async fn run_single_task(&mut self, task_idx: usize) -> Result<bool> {
        self.state.current_task = Some(task_idx);
        self.state.tasks[task_idx].status = TaskStatus::InProgress;
        self.state.tasks[task_idx].last_attempt_at = Some(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );

        let task_desc = self.state.tasks[task_idx].description.clone();
        self.state.add_log(format!("Starting task: {}", task_desc));

        // Execute the task with reflection rounds; capture any error text for
        // potential use by the re-planner.
        let task = self.state.tasks[task_idx].clone();
        let reflection_rounds = self.config.reflection_rounds;
        let mut task_error = String::new();
        match self.agent.execute_task_with_reflection(&task, reflection_rounds).await {
            Ok(()) => {
                self.state.add_log("Task completed".to_string());
                self.state.tasks[task_idx].status = TaskStatus::Completed;
            }
            Err(e) => {
                task_error = e.to_string();
                self.state.add_log(format!("Task failed: {}", task_error));
                self.state.tasks[task_idx].status = TaskStatus::Failed;
                self.state.tasks[task_idx].failed_attempts += 1;
            }
        }

        // Run tests / evaluation
        self.state.add_log("Running tests...".to_string());
        let test_passed = self.evaluate_task(task_idx).await;

        match test_passed {
            Ok(true) => {
                self.state.add_log("Tests passed".to_string());
            }
            Ok(false) => {
                self.state.add_log("Tests failed".to_string());
                if task_error.is_empty() {
                    task_error = TEST_FAILURE_ERROR.to_string();
                }
                self.state.tasks[task_idx].status = TaskStatus::Failed;
            }
            Err(e) => {
                self.state.add_log(format!("Error running tests: {}", e));
            }
        }

        // Commit changes (if task succeeded)
        if self.state.tasks[task_idx].status == TaskStatus::Completed {
            // Persist declared output artefacts before committing.
            if !task.outputs.is_empty() {
                let manifest_path = self.config.work_dir.join(".wreck-it-artefacts.json");
                match artefact_store::persist_output_artefacts(
                    &manifest_path,
                    &task.id,
                    &task.outputs,
                    &self.config.work_dir,
                ) {
                    Ok(()) => {
                        self.state.add_log("Output artefacts persisted".to_string());
                    }
                    Err(e) => {
                        self.state
                            .add_log(format!("Warning: failed to persist output artefacts: {}", e));
                    }
                }
            }

            let commit_msg = format!("Complete task: {}", self.state.tasks[task_idx].description);
            if let Err(e) = self.agent.commit_changes(&commit_msg) {
                self.state.add_log(format!("Failed to commit: {}", e));
            } else {
                self.state.add_log("Changes committed".to_string());
            }
        }

        // Save task state to filesystem
        save_tasks(&self.config.task_file, &self.state.tasks).context("Failed to save tasks")?;

        // Update consecutive failure counter and optionally invoke re-planner.
        if self.state.tasks[task_idx].status == TaskStatus::Failed {
            self.state.consecutive_failures += 1;
            let threshold = self.config.replan_threshold;
            if threshold > 0 && self.state.consecutive_failures >= threshold {
                self.state.add_log(format!(
                    "Consecutive failure threshold ({}) reached – invoking re-planner",
                    threshold
                ));
                let replanner = TaskReplanner::new(
                    self.config.model_provider.clone(),
                    self.config.api_endpoint.clone(),
                    self.config.api_token.clone(),
                    self.config.work_dir.to_string_lossy().to_string(),
                );
                let failed_task = self.state.tasks[task_idx].clone();
                match replan_and_save(
                    &replanner,
                    &self.state.tasks,
                    &failed_task,
                    &task_error,
                    &self.config.task_file,
                )
                .await
                {
                    Ok(updated) => {
                        self.state.tasks = updated;
                        self.state.consecutive_failures = 0;
                        self.state.add_log("Re-planning succeeded".to_string());
                    }
                    Err(e) => {
                        self.state.add_log(format!("Re-planning failed: {}", e));
                    }
                }
            }
        } else {
            self.state.consecutive_failures = 0;
        }

        Ok(true)
    }

    /// Evaluate a task using the configured evaluation mode.
    async fn evaluate_task(&mut self, task_idx: usize) -> Result<bool> {
        if self.agent.evaluation_mode() == EvaluationMode::AgentFile {
            let task = self.state.tasks[task_idx].clone();
            return self.agent.evaluate_completeness(&task).await;
        }
        self.agent.run_tests()
    }

    /// Run multiple parallelizable tasks concurrently.
    ///
    /// Each task gets its own `AgentClient` and runs in a separate tokio task.
    /// Results are collected and applied back to the shared state.
    async fn run_parallel_tasks(&mut self, indices: Vec<usize>) -> Result<bool> {
        self.state.add_log(format!(
            "Running {} tasks in parallel (phase {})",
            indices.len(),
            self.state.tasks[indices[0]].phase,
        ));

        // Mark all as in-progress and collect task data.
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let mut task_data: Vec<(usize, crate::types::Task)> = Vec::new();
        for &idx in &indices {
            self.state.tasks[idx].status = TaskStatus::InProgress;
            self.state.tasks[idx].last_attempt_at = Some(now_secs);
            task_data.push((idx, self.state.tasks[idx].clone()));
        }

        // Spawn concurrent agent work.
        let mut handles = Vec::new();
        for (idx, task) in task_data {
            let mut agent = AgentClient::with_evaluation(
                self.config.model_provider.clone(),
                self.config.api_endpoint.clone(),
                self.config.api_token.clone(),
                self.config.work_dir.to_string_lossy().to_string(),
                self.config.verification_command.clone(),
                self.config.evaluation_mode,
                self.config.completeness_prompt.clone(),
                self.config
                    .completion_marker_file
                    .to_string_lossy()
                    .to_string(),
            );
            let handle = tokio::spawn(async move {
                let result = agent.execute_task(&task).await;
                (idx, result)
            });
            handles.push(handle);
        }

        // Collect results.
        for handle in handles {
            match handle.await {
                Ok((idx, Ok(result))) => {
                    self.state.add_log(format!(
                        "Task [{}] completed: {}",
                        self.state.tasks[idx].id, result
                    ));
                    self.state.tasks[idx].status = TaskStatus::Completed;
                }
                Ok((idx, Err(e))) => {
                    self.state
                        .add_log(format!("Task [{}] failed: {}", self.state.tasks[idx].id, e));
                    self.state.tasks[idx].status = TaskStatus::Failed;
                    self.state.tasks[idx].failed_attempts += 1;
                }
                Err(e) => {
                    self.state.add_log(format!("Parallel task panicked: {}", e));
                }
            }
        }

        // Run evaluation for each completed task.
        for &idx in &indices {
            if self.state.tasks[idx].status == TaskStatus::Completed {
                let eval = self.evaluate_task(idx).await;
                match eval {
                    Ok(true) => {
                        self.state.add_log(format!(
                            "Task [{}] evaluation passed",
                            self.state.tasks[idx].id
                        ));
                    }
                    Ok(false) => {
                        self.state.add_log(format!(
                            "Task [{}] evaluation failed",
                            self.state.tasks[idx].id
                        ));
                        self.state.tasks[idx].status = TaskStatus::Failed;
                    }
                    Err(e) => {
                        self.state.add_log(format!(
                            "Task [{}] evaluation error: {}",
                            self.state.tasks[idx].id, e
                        ));
                    }
                }

                if self.state.tasks[idx].status == TaskStatus::Completed {
                    let msg = format!("Complete task: {}", self.state.tasks[idx].description);
                    if let Err(e) = self.agent.commit_changes(&msg) {
                        self.state.add_log(format!("Failed to commit: {}", e));
                    }
                }
            }
        }

        save_tasks(&self.config.task_file, &self.state.tasks).context("Failed to save tasks")?;
        Ok(true)
    }

    /// Run the complete loop until all tasks are done or max iterations reached
    #[allow(dead_code)]
    pub async fn run(&mut self) -> Result<()> {
        self.state.running = true;
        self.initialize()?;

        self.state.add_log("Starting Ralph Wiggum Loop".to_string());

        while self.state.running {
            match self.run_iteration().await {
                Ok(should_continue) => {
                    if !should_continue {
                        self.state.running = false;
                    }
                }
                Err(e) => {
                    self.state.add_log(format!("Error in iteration: {}", e));
                    self.state.running = false;
                    return Err(e);
                }
            }
        }

        self.state.add_log("Ralph Wiggum Loop finished".to_string());
        Ok(())
    }

    /// Get the current state of the loop
    pub fn state(&self) -> &LoopState {
        &self.state
    }

    /// Get mutable state for updates
    pub fn state_mut(&mut self) -> &mut LoopState {
        &mut self.state
    }

    /// Stop the loop
    pub fn stop(&mut self) {
        self.state.running = false;
        self.state.add_log("Loop stopped by user".to_string());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Task;

    fn make_task(
        id: &str,
        status: TaskStatus,
        priority: u32,
        complexity: u32,
        failed_attempts: u32,
        depends_on: Vec<&str>,
    ) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            priority,
            complexity,
            failed_attempts,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
        }
    }

    #[test]
    fn scheduler_empty_when_no_pending() {
        let tasks = vec![make_task("a", TaskStatus::Completed, 0, 1, 0, vec![])];
        assert!(TaskScheduler::schedule(&tasks).is_empty());
    }

    #[test]
    fn scheduler_respects_dependencies() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, 0, 1, 0, vec![]),
            // b depends on a which is still Pending → not ready
            make_task("b", TaskStatus::Pending, 0, 1, 0, vec!["a"]),
        ];
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready, vec![0]);
    }

    #[test]
    fn scheduler_unblocks_after_dependency_completes() {
        let tasks = vec![
            make_task("a", TaskStatus::Completed, 0, 1, 0, vec![]),
            make_task("b", TaskStatus::Pending, 0, 1, 0, vec!["a"]),
        ];
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready, vec![1]);
    }

    #[test]
    fn scheduler_orders_by_priority() {
        let tasks = vec![
            make_task("low", TaskStatus::Pending, 1, 1, 0, vec![]),
            make_task("high", TaskStatus::Pending, 5, 1, 0, vec![]),
        ];
        let ready = TaskScheduler::schedule(&tasks);
        // high-priority task should be first
        assert_eq!(ready[0], 1);
        assert_eq!(ready[1], 0);
    }

    #[test]
    fn scheduler_penalizes_failed_attempts() {
        // "retried" has 10 failures → penalty of 30; "fresh" penalty 0
        let tasks = vec![
            make_task("fresh", TaskStatus::Pending, 0, 1, 0, vec![]),
            make_task("retried", TaskStatus::Pending, 0, 1, 10, vec![]),
        ];
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready[0], 0); // fresh scores higher
    }

    #[test]
    fn scheduler_prefers_lower_complexity() {
        let tasks = vec![
            make_task("complex", TaskStatus::Pending, 0, 10, 0, vec![]),
            make_task("simple", TaskStatus::Pending, 0, 1, 0, vec![]),
        ];
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready[0], 1); // simple first
    }

    #[test]
    fn scheduler_rewards_dependency_fanout() {
        // "a" is depended on by both "b" and "c" → higher fan-out score than "d"
        let tasks = vec![
            make_task("d", TaskStatus::Pending, 0, 1, 0, vec![]),
            make_task("a", TaskStatus::Pending, 0, 1, 0, vec![]),
            make_task("b", TaskStatus::Pending, 0, 1, 0, vec!["a"]),
            make_task("c", TaskStatus::Pending, 0, 1, 0, vec!["a"]),
        ];
        // only "d" (idx 0) and "a" (idx 1) are ready; "a" unblocks 2 tasks
        let ready = TaskScheduler::schedule(&tasks);
        assert_eq!(ready.len(), 2);
        assert_eq!(ready[0], 1); // "a" scores higher
    }

    #[test]
    fn scheduler_recency_bonus_for_older_attempt() {
        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(7200); // 2 hours ago → capped at 60 pts

        let recent_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(10); // 10 seconds ago → ~0.17 pts

        let tasks = vec![
            Task {
                id: "recent".to_string(),
                description: "recent".to_string(),
                status: TaskStatus::Pending,
                role: crate::types::AgentRole::default(),
                kind: crate::types::TaskKind::default(),
                cooldown_seconds: None,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: Some(recent_ts),
                inputs: vec![],
                outputs: vec![],
            },
            Task {
                id: "old".to_string(),
                description: "old".to_string(),
                status: TaskStatus::Pending,
                role: crate::types::AgentRole::default(),
                kind: crate::types::TaskKind::default(),
                cooldown_seconds: None,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: Some(old_ts),
                inputs: vec![],
                outputs: vec![],
            },
        ];
        let ready = TaskScheduler::schedule(&tasks);
        // "old" should win due to larger recency bonus
        assert_eq!(ready[0], 1);
    }

    #[test]
    fn scheduler_never_attempted_has_no_recency_bonus() {
        // A task never attempted (None) scores no recency bonus; a task
        // attempted 2 hours ago scores up to 60 pts.
        let old_ts = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_sub(7200);

        let tasks = vec![
            make_task("never", TaskStatus::Pending, 0, 1, 0, vec![]),
            Task {
                id: "old".to_string(),
                description: "old".to_string(),
                status: TaskStatus::Pending,
                role: crate::types::AgentRole::default(),
                kind: crate::types::TaskKind::default(),
                cooldown_seconds: None,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: Some(old_ts),
                inputs: vec![],
                outputs: vec![],
            },
        ];
        let ready = TaskScheduler::schedule(&tasks);
        // "old" has recency bonus, "never" does not
        assert_eq!(ready[0], 1);
    }

    #[test]
    fn scheduler_returns_empty_for_empty_task_list() {
        assert!(TaskScheduler::schedule(&[]).is_empty());
    }

    #[test]
    fn scheduler_returns_empty_when_all_tasks_failed() {
        let tasks = vec![
            make_task("a", TaskStatus::Failed, 0, 1, 1, vec![]),
            make_task("b", TaskStatus::Failed, 0, 1, 3, vec![]),
        ];
        assert!(TaskScheduler::schedule(&tasks).is_empty());
    }
}
