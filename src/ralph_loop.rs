use crate::agent::AgentClient;
use crate::task_manager::{load_tasks, save_tasks};
use crate::types::{Config, EvaluationMode, LoopState, Task, TaskStatus};
use anyhow::{Context, Result};
use std::collections::HashSet;

// ---------------------------------------------------------------------------
// Scoring weights
// ---------------------------------------------------------------------------

/// Weights used by [`TaskScheduler`] when computing per-task scores.
pub struct ScoringWeights {
    /// Multiplier applied to `task.priority`.  Higher → more urgent tasks
    /// rise to the top.
    pub priority_weight: f64,
    /// Penalty applied to `task.complexity`.  Simpler tasks are preferred
    /// when scores are otherwise equal.
    pub complexity_weight: f64,
    /// Penalty per failed attempt.  Tasks that keep failing are
    /// deprioritised.
    pub failed_attempts_penalty: f64,
    /// Maximum bonus awarded for not having been attempted recently.
    pub recency_bonus_max: f64,
    /// Half-life (seconds) for the recency bonus.  After this many seconds
    /// the bonus reaches half of `recency_bonus_max`.
    pub recency_half_life_secs: f64,
}

impl Default for ScoringWeights {
    fn default() -> Self {
        Self {
            priority_weight: 10.0,
            complexity_weight: 0.5,
            failed_attempts_penalty: 5.0,
            recency_bonus_max: 20.0,
            recency_half_life_secs: 300.0,
        }
    }
}

// ---------------------------------------------------------------------------
// TaskScheduler
// ---------------------------------------------------------------------------

/// Selects and orders ready tasks using a multi-factor scoring algorithm.
///
/// Factors considered (in order of typical influence):
/// - **Priority** (`task.priority`): higher-priority tasks score higher.
/// - **Complexity** (`task.complexity`): more complex tasks score slightly
///   lower, so simpler work is preferred when priorities are equal.
/// - **Failed attempts** (`task.failed_attempts`): tasks that have failed
///   many times are deprioritised to avoid spinning on a broken task.
/// - **Time since last attempt** (`task.last_attempted_at`): tasks that
///   have not been tried recently receive a growing bonus, giving previously
///   failing tasks another chance after a cool-down period.
/// - **Dependency satisfaction**: only tasks whose `depends_on` ids are all
///   `Completed` are included in the result at all.
pub struct TaskScheduler {
    weights: ScoringWeights,
}

impl TaskScheduler {
    /// Create a scheduler with default scoring weights.
    pub fn new() -> Self {
        Self {
            weights: ScoringWeights::default(),
        }
    }

    /// Create a scheduler with custom scoring weights.
    pub fn with_weights(weights: ScoringWeights) -> Self {
        Self { weights }
    }

    /// Compute a scalar score for a single task (higher = run sooner).
    fn score(&self, task: &Task, now_secs: u64) -> f64 {
        let priority_score = task.priority as f64 * self.weights.priority_weight;
        let complexity_penalty = task.complexity as f64 * self.weights.complexity_weight;
        let failure_penalty = task.failed_attempts as f64 * self.weights.failed_attempts_penalty;
        let recency_bonus = match task.last_attempted_at {
            // Never attempted → full recency bonus.
            None => self.weights.recency_bonus_max,
            Some(last) => {
                let elapsed = now_secs.saturating_sub(last) as f64;
                // Exponential approach: reaches ~50 % of the maximum after
                // `recency_half_life_secs` and approaches the maximum
                // asymptotically.  Formula: max * (1 - e^(-t/half_life)).
                self.weights.recency_bonus_max
                    * (1.0 - (-elapsed / self.weights.recency_half_life_secs).exp())
            }
        };
        priority_score - complexity_penalty - failure_penalty + recency_bonus
    }

    /// Return the indices of all ready tasks ordered by score (highest first).
    ///
    /// A task is *ready* when its `status` is `Pending` and every id listed
    /// in `depends_on` belongs to a `Completed` task.
    pub fn scheduled_task_indices(&self, tasks: &[Task]) -> Vec<usize> {
        self.scheduled_task_indices_at(tasks, current_unix_secs())
    }

    /// Like [`scheduled_task_indices`] but accepts the current time (seconds
    /// since the Unix epoch) explicitly, making the result deterministic in
    /// unit tests.
    pub fn scheduled_task_indices_at(&self, tasks: &[Task], now_secs: u64) -> Vec<usize> {
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
            .map(|(i, t)| (i, self.score(t, now_secs)))
            .collect();

        // Stable sort descending by score.
        ready.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        ready.into_iter().map(|(i, _)| i).collect()
    }
}

impl Default for TaskScheduler {
    fn default() -> Self {
        Self::new()
    }
}

/// Return the current time as seconds since the Unix epoch.
fn current_unix_secs() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

// ---------------------------------------------------------------------------
// RalphLoop
// ---------------------------------------------------------------------------

/// The Ralph Wiggum Loop - a bash-style loop that continuously executes tasks
pub struct RalphLoop {
    config: Config,
    state: LoopState,
    agent: AgentClient,
    scheduler: TaskScheduler,
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
            scheduler: TaskScheduler::new(),
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

        // Use the scheduler to obtain an ordered list of ready tasks.
        let ready = self.scheduler.scheduled_task_indices(&self.state.tasks);

        if ready.len() > 1 {
            return self.run_parallel_tasks(ready).await;
        }

        // Single (or zero) ready task – run it sequentially.
        let task_idx = match ready.into_iter().next() {
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
        self.state.tasks[task_idx].last_attempted_at = Some(current_unix_secs());

        let task_desc = self.state.tasks[task_idx].description.clone();
        self.state.add_log(format!("Starting task: {}", task_desc));

        // Execute the task
        let task = &self.state.tasks[task_idx];
        match self.agent.execute_task(task).await {
            Ok(result) => {
                self.state.add_log(format!("Task completed: {}", result));
                self.state.tasks[task_idx].status = TaskStatus::Completed;
            }
            Err(e) => {
                self.state.add_log(format!("Task failed: {}", e));
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
                if self.state.tasks[task_idx].status != TaskStatus::Failed {
                    self.state.tasks[task_idx].failed_attempts += 1;
                }
                self.state.tasks[task_idx].status = TaskStatus::Failed;
            }
            Err(e) => {
                self.state.add_log(format!("Error running tests: {}", e));
            }
        }

        // Commit changes (if task succeeded)
        if self.state.tasks[task_idx].status == TaskStatus::Completed {
            let commit_msg = format!("Complete task: {}", self.state.tasks[task_idx].description);
            if let Err(e) = self.agent.commit_changes(&commit_msg) {
                self.state.add_log(format!("Failed to commit: {}", e));
            } else {
                self.state.add_log("Changes committed".to_string());
            }
        }

        // Save task state to filesystem
        save_tasks(&self.config.task_file, &self.state.tasks).context("Failed to save tasks")?;

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
            "Running {} tasks in parallel",
            indices.len(),
        ));

        // Mark all as in-progress and record attempt time.
        let now = current_unix_secs();
        let mut task_data: Vec<(usize, crate::types::Task)> = Vec::new();
        for &idx in &indices {
            self.state.tasks[idx].status = TaskStatus::InProgress;
            self.state.tasks[idx].last_attempted_at = Some(now);
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
                        self.state.tasks[idx].failed_attempts += 1;
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

// ---------------------------------------------------------------------------
// Unit tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Task, TaskStatus};

    fn make_task(id: &str, status: TaskStatus, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            ..Task::default()
        }
    }

    fn make_task_with(
        id: &str,
        status: TaskStatus,
        priority: u32,
        complexity: u32,
        failed_attempts: u32,
        last_attempted_at: Option<u64>,
    ) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            priority,
            complexity,
            failed_attempts,
            last_attempted_at,
            ..Task::default()
        }
    }

    // -----------------------------------------------------------------------
    // Dependency filtering
    // -----------------------------------------------------------------------

    #[test]
    fn scheduler_excludes_tasks_with_unmet_deps() {
        let scheduler = TaskScheduler::new();
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        // b depends on a which is still Pending, so only a is ready.
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        assert_eq!(ready, vec![0]);
    }

    #[test]
    fn scheduler_includes_task_when_dep_completed() {
        let scheduler = TaskScheduler::new();
        let tasks = vec![
            make_task("a", TaskStatus::Completed, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        assert_eq!(ready, vec![1]);
    }

    #[test]
    fn scheduler_excludes_failed_dep() {
        let scheduler = TaskScheduler::new();
        let tasks = vec![
            make_task("a", TaskStatus::Failed, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        // Failed ≠ Completed, so b is blocked.
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        assert!(ready.is_empty());
    }

    #[test]
    fn scheduler_empty_when_all_complete() {
        let scheduler = TaskScheduler::new();
        let tasks = vec![make_task("a", TaskStatus::Completed, vec![])];
        assert!(scheduler.scheduled_task_indices_at(&tasks, 0).is_empty());
    }

    // -----------------------------------------------------------------------
    // Priority ordering
    // -----------------------------------------------------------------------

    #[test]
    fn higher_priority_task_ranked_first() {
        let scheduler = TaskScheduler::new();
        // Task b has higher priority than a (same everything else).
        let tasks = vec![
            make_task_with("a", TaskStatus::Pending, 3, 5, 0, None),
            make_task_with("b", TaskStatus::Pending, 8, 5, 0, None),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        // b (index 1) should come before a (index 0).
        assert_eq!(ready[0], 1);
        assert_eq!(ready[1], 0);
    }

    // -----------------------------------------------------------------------
    // Complexity penalty
    // -----------------------------------------------------------------------

    #[test]
    fn lower_complexity_task_ranked_first_when_priority_equal() {
        let scheduler = TaskScheduler::new();
        // Task b has lower complexity than a (same priority).
        let tasks = vec![
            make_task_with("a", TaskStatus::Pending, 5, 8, 0, None),
            make_task_with("b", TaskStatus::Pending, 5, 2, 0, None),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        assert_eq!(ready[0], 1); // b (lower complexity) first
    }

    // -----------------------------------------------------------------------
    // Failed-attempts penalty
    // -----------------------------------------------------------------------

    #[test]
    fn more_failed_attempts_lowers_score() {
        let scheduler = TaskScheduler::new();
        // Task a has many failed attempts; b is fresh.
        let tasks = vec![
            make_task_with("a", TaskStatus::Pending, 5, 5, 10, None),
            make_task_with("b", TaskStatus::Pending, 5, 5, 0, None),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        assert_eq!(ready[0], 1); // b (no failures) first
    }

    // -----------------------------------------------------------------------
    // Recency bonus
    // -----------------------------------------------------------------------

    #[test]
    fn never_attempted_task_gets_full_recency_bonus() {
        let weights = ScoringWeights {
            priority_weight: 0.0,
            complexity_weight: 0.0,
            failed_attempts_penalty: 0.0,
            recency_bonus_max: 20.0,
            recency_half_life_secs: 300.0,
        };
        let scheduler = TaskScheduler::with_weights(weights);
        // Task a was attempted just now; b has never been attempted.
        let now = 1_000_u64;
        let tasks = vec![
            make_task_with("a", TaskStatus::Pending, 5, 5, 0, Some(now)),
            make_task_with("b", TaskStatus::Pending, 5, 5, 0, None),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, now);
        assert_eq!(ready[0], 1); // b (never tried) gets full bonus
    }

    #[test]
    fn older_attempt_scores_higher_than_recent() {
        let weights = ScoringWeights {
            priority_weight: 0.0,
            complexity_weight: 0.0,
            failed_attempts_penalty: 0.0,
            recency_bonus_max: 20.0,
            recency_half_life_secs: 300.0,
        };
        let scheduler = TaskScheduler::with_weights(weights);
        let now = 10_000_u64;
        // a was attempted 1 second ago; b was attempted 600 seconds ago.
        let tasks = vec![
            make_task_with("a", TaskStatus::Pending, 5, 5, 0, Some(now - 1)),
            make_task_with("b", TaskStatus::Pending, 5, 5, 0, Some(now - 600)),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, now);
        assert_eq!(ready[0], 1); // b (longer ago) scores higher
    }

    // -----------------------------------------------------------------------
    // Combined scoring
    // -----------------------------------------------------------------------

    #[test]
    fn combined_score_orders_tasks_correctly() {
        let scheduler = TaskScheduler::new();
        let now = 1_000_u64;
        // c: high priority, low complexity, no failures → should be first.
        // a: low priority, high complexity, failures → should be last.
        // b: medium → should be in the middle.
        let tasks = vec![
            make_task_with("a", TaskStatus::Pending, 2, 8, 3, Some(now - 1)),
            make_task_with("b", TaskStatus::Pending, 5, 5, 1, Some(now - 60)),
            make_task_with("c", TaskStatus::Pending, 9, 1, 0, None),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, now);
        assert_eq!(ready[0], 2); // c first
        assert_eq!(ready[2], 0); // a last
    }

    // -----------------------------------------------------------------------
    // InProgress tasks are not re-scheduled
    // -----------------------------------------------------------------------

    #[test]
    fn in_progress_tasks_are_not_returned() {
        let scheduler = TaskScheduler::new();
        let tasks = vec![
            make_task("a", TaskStatus::InProgress, vec![]),
            make_task("b", TaskStatus::Pending, vec![]),
        ];
        let ready = scheduler.scheduled_task_indices_at(&tasks, 1_000);
        assert_eq!(ready, vec![1]);
    }
}
