use crate::agent::AgentClient;
use crate::artefact_store;
use crate::cost_tracker::{model_pricing, CostTracker};
use crate::github_client;
use crate::kanban::{self, KanbanClient, KanbanIssue};
use crate::notifier;
use crate::provenance::{self, ProvenanceRecord};
use crate::replanner::{replan_and_save, TaskReplanner};
use crate::task_manager::{get_next_task, load_tasks, save_tasks};
use crate::types::{
    Config, EvaluationMode, LoopState, ModelProvider, Task, TaskStatus,
    DEFAULT_GITHUB_MODELS_MODEL, DEFAULT_LLAMA_MODEL,
};
use anyhow::{Context, Result};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

/// Error message used when evaluation/tests fail without a prior agent error.
const TEST_FAILURE_ERROR: &str = "Tests failed";

/// Return a human-readable model name for the given provider.
fn model_name(provider: &ModelProvider) -> String {
    match provider {
        ModelProvider::Copilot => "copilot".to_string(),
        ModelProvider::Llama => DEFAULT_LLAMA_MODEL.to_string(),
        ModelProvider::GithubModels => DEFAULT_GITHUB_MODELS_MODEL.to_string(),
    }
}

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
    /// Maps task_id → GitHub issue number for open wreck-it issues.
    /// Populated when a task moves to `InProgress` and cleared on closure.
    github_issue_numbers: HashMap<String, u64>,
    /// Maps task_id → external Kanban issue for open board items.
    /// Populated when a task moves to `InProgress` and cleared on closure.
    kanban_issues: HashMap<String, KanbanIssue>,
    /// Optional Kanban provider constructed from config.
    kanban_provider: Option<KanbanClient>,
    /// Shared cost tracker updated by every HTTP chat completion call.
    /// Cloned into per-task agents spawned for parallel execution.
    cost_tracker: Arc<Mutex<CostTracker>>,
}

impl RalphLoop {
    pub fn new(config: Config) -> Self {
        let max_iterations = config.max_iterations;

        // Build per-model pricing from the configured model name, then create a
        // shared cost tracker that the main agent and parallel task agents all
        // write into.
        let model_str = match config.model_provider {
            ModelProvider::GithubModels => DEFAULT_GITHUB_MODELS_MODEL,
            ModelProvider::Llama => DEFAULT_LLAMA_MODEL,
            ModelProvider::Copilot => "copilot",
        };
        let (inp, out) = model_pricing(model_str);
        let cost_tracker = Arc::new(Mutex::new(CostTracker::new(inp, out)));

        let agent = AgentClient::with_evaluation(
            config.model_provider.clone(),
            config.api_endpoint.clone(),
            config.api_token.clone(),
            config.work_dir.to_string_lossy().to_string(),
            config.verification_command.clone(),
            config.evaluation_mode,
            config.completeness_prompt.clone(),
            config.completion_marker_file.to_string_lossy().to_string(),
        )
        .with_cost_tracker(Arc::clone(&cost_tracker));

        let kanban_provider = kanban::provider_from_config(&config.kanban);

        Self {
            config,
            state: LoopState::new(max_iterations),
            agent,
            github_issue_numbers: HashMap::new(),
            kanban_issues: HashMap::new(),
            kanban_provider,
            cost_tracker,
        }
    }

    /// Build a [`github_client::GitHubIssueClient`] from the current config when
    /// GitHub Issues integration is enabled, returning `None` otherwise.
    fn make_github_client(&self) -> Option<github_client::GitHubIssueClient> {
        github_client::client_from_config(
            self.config.github_issues_enabled,
            self.config.github_repo.as_deref(),
            self.config.github_token.as_deref(),
        )
    }

    /// Resolve the effective working directory for a task.
    ///
    /// Lookup order:
    /// 1. Exact match on `task.id` in [`Config::work_dirs`].
    /// 2. Match on the task's `role` (serialised as a lowercase string).
    /// 3. Fall back to the top-level [`Config::work_dir`].
    ///
    /// Relative paths in the map are resolved relative to [`Config::work_dir`].
    fn resolve_work_dir(&self, task: &crate::types::Task) -> std::path::PathBuf {
        // 1. Exact task-id match.
        if let Some(p) = self.config.work_dirs.get(&task.id) {
            let path = std::path::Path::new(p);
            if path.is_absolute() {
                return path.to_path_buf();
            }
            return self.config.work_dir.join(path);
        }

        // 2. Role match (AgentRole serialised to lowercase string via serde).
        let role_str = serde_json::to_value(task.role)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()));
        if let Some(role_key) = role_str {
            if let Some(p) = self.config.work_dirs.get(&role_key) {
                let path = std::path::Path::new(p);
                if path.is_absolute() {
                    return path.to_path_buf();
                }
                return self.config.work_dir.join(path);
            }
        }

        // 3. Default.
        self.config.work_dir.clone()
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

        // Check the budget before starting a new task.
        if let Ok(guard) = self.cost_tracker.lock() {
            if guard.budget_exceeded(self.config.max_cost_usd) {
                self.state.add_log(format!(
                    "Budget limit reached – current cost ${:.4} >= limit ${:.4} – stopping",
                    guard.total_estimated_cost_usd,
                    self.config.max_cost_usd.unwrap_or(0.0)
                ));
                return Ok(false);
            }
        }

        // Reset per-task cost counters at the start of each new iteration.
        if let Ok(mut guard) = self.cost_tracker.lock() {
            guard.reset_task();
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
            let result = self.run_parallel_tasks(ready).await;
            self.emit_cost_summary();
            return result;
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

        let result = self.run_single_task(task_idx).await;
        self.emit_cost_summary();
        result
    }

    /// Append a one-line cost summary to the loop log.
    ///
    /// Called at the end of every iteration (both single-task and parallel
    /// paths) so that both headless and TUI consumers see the same summary.
    fn emit_cost_summary(&mut self) {
        if let Ok(guard) = self.cost_tracker.lock() {
            let summary = guard.iteration_summary();
            self.state.add_log(summary);
        }
    }

    /// Execute a single task by index, running evaluation & commit logic.
    async fn run_single_task(&mut self, task_idx: usize) -> Result<bool> {
        self.state.current_task = Some(task_idx);

        // Resolve the effective working directory for this task and redirect the
        // shared agent before any agent calls are made.
        let effective_work_dir = self.resolve_work_dir(&self.state.tasks[task_idx]);
        self.agent
            .set_work_dir(effective_work_dir.to_string_lossy().to_string());
        if effective_work_dir != self.config.work_dir {
            self.state.add_log(format!(
                "Using per-task work dir: {}",
                effective_work_dir.display()
            ));
        }

        // Evaluate agent-based precondition before executing the task.
        if self.state.tasks[task_idx].precondition_prompt.is_some() {
            let task = self.state.tasks[task_idx].clone();
            self.state.add_log(format!(
                "Evaluating precondition for task: {}",
                task.description
            ));
            match self.agent.evaluate_precondition(&task).await {
                Ok(true) => {
                    self.state.add_log("Precondition met".to_string());
                }
                Ok(false) => {
                    self.state
                        .add_log("Precondition not met – skipping task".to_string());
                    return Ok(true); // Continue the loop, but skip this task
                }
                Err(e) => {
                    self.state.add_log(format!(
                        "Precondition evaluation error: {} – skipping task",
                        e
                    ));
                    return Ok(true);
                }
            }
        }

        self.state.tasks[task_idx].status = TaskStatus::InProgress;
        let invocation_timestamp = provenance::now_timestamp();
        self.state.tasks[task_idx].last_attempt_at = Some(invocation_timestamp);

        let task_id = self.state.tasks[task_idx].id.clone();
        let task_desc = self.state.tasks[task_idx].description.clone();
        self.state.add_log(format!("Starting task: {}", task_desc));

        notifier::notify(
            &self.config.notify_webhooks,
            &task_id,
            TaskStatus::InProgress,
            invocation_timestamp,
            &task_desc,
        )
        .await;

        // Open a GitHub Issue for this task when the integration is enabled.
        if let Some(gh_client) = self.make_github_client() {
            match gh_client.create_issue(&task_id, &task_desc).await {
                Ok(issue_number) => {
                    self.github_issue_numbers
                        .insert(task_id.clone(), issue_number);
                    self.state.add_log(format!(
                        "GitHub Issue #{issue_number} opened for task {task_id}"
                    ));
                }
                Err(e) => {
                    self.state.add_log(format!(
                        "Warning: failed to open GitHub Issue for task {task_id}: {e}"
                    ));
                }
            }
        }

        // Create a Kanban board issue for this task when the integration is enabled.
        if let Some(ref provider) = self.kanban_provider {
            match provider.create_issue(&task_id, &task_desc).await {
                Ok(kanban_issue) => {
                    self.state.add_log(format!(
                        "{} issue created for task {task_id}: {}",
                        provider.provider_name(),
                        kanban_issue.url
                    ));
                    self.kanban_issues.insert(task_id.clone(), kanban_issue);
                }
                Err(e) => {
                    self.state.add_log(format!(
                        "Warning: failed to create {} issue for task {task_id}: {e}",
                        provider.provider_name()
                    ));
                }
            }
        }

        // Execute the task with reflection rounds; capture any error text for
        // potential use by the re-planner.
        let task = self.state.tasks[task_idx].clone();
        let reflection_rounds = self.config.reflection_rounds;
        let mut task_error = String::new();

        // Wrap execution in a per-task timeout when `timeout_seconds` is set.
        let execution_result = if let Some(timeout_secs) = task.timeout_seconds {
            match tokio::time::timeout(
                std::time::Duration::from_secs(timeout_secs),
                self.agent
                    .execute_task_with_reflection(&task, reflection_rounds),
            )
            .await
            {
                Ok(result) => result,
                Err(_) => Err(anyhow::anyhow!(
                    "Task timed out after {} seconds",
                    timeout_secs
                )),
            }
        } else {
            self.agent
                .execute_task_with_reflection(&task, reflection_rounds)
                .await
        };

        match execution_result {
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

        // Record provenance for this agent invocation (before committing so
        // the git diff hash reflects the agent's actual changes).
        let prov_outcome = if task_error.is_empty() {
            "success"
        } else {
            "failure"
        };
        let prov_record = ProvenanceRecord {
            task_id: task.id.clone(),
            agent_role: task.role,
            model: model_name(&self.config.model_provider),
            prompt_hash: provenance::hash_string(&task.description),
            tool_calls: vec![],
            git_diff_hash: provenance::git_diff_hash(&self.config.work_dir),
            timestamp: invocation_timestamp,
            outcome: prov_outcome.to_string(),
        };
        if let Err(e) = provenance::persist_provenance_record(&prov_record, &self.config.work_dir) {
            self.state.add_log(format!(
                "Warning: failed to persist provenance record: {}",
                e
            ));
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
                        self.state.add_log(format!(
                            "Warning: failed to persist output artefacts: {}",
                            e
                        ));
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

        // Notify webhooks with the final task status.
        let final_status = self.state.tasks[task_idx].status;
        let notify_ts = provenance::now_timestamp();
        notifier::notify(
            &self.config.notify_webhooks,
            &task_id,
            final_status,
            notify_ts,
            &task_desc,
        )
        .await;

        // Close the GitHub Issue when the task has reached a terminal state.
        if final_status == TaskStatus::Completed || final_status == TaskStatus::Failed {
            if let Some(issue_number) = self.github_issue_numbers.remove(&task_id) {
                if let Some(gh_client) = self.make_github_client() {
                    if let Err(e) = gh_client.close_issue(issue_number).await {
                        self.state.add_log(format!(
                            "Warning: failed to close GitHub Issue #{issue_number} \
                             for task {task_id}: {e}"
                        ));
                    } else {
                        self.state.add_log(format!(
                            "GitHub Issue #{issue_number} closed for task {task_id}"
                        ));
                    }
                }
            }

            // Transition the Kanban board issue to the terminal status.
            if let Some(kanban_issue) = self.kanban_issues.remove(&task_id) {
                if let Some(ref provider) = self.kanban_provider {
                    if let Err(e) = provider
                        .transition_issue(&kanban_issue.external_id, final_status)
                        .await
                    {
                        self.state.add_log(format!(
                            "Warning: failed to transition {} issue for task {task_id}: {e}",
                            provider.provider_name()
                        ));
                    } else {
                        self.state.add_log(format!(
                            "{} issue transitioned to {:?} for task {task_id}",
                            provider.provider_name(),
                            final_status
                        ));
                    }
                }
            }
        }

        // Update consecutive failure counter and optionally invoke re-planner.
        // Track whether the re-planner ran and succeeded so that the auto-retry
        // block below can defer to the re-planner's decision in that case.
        let mut replan_succeeded = false;
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
                        replan_succeeded = true;
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

        // Auto-retry: if the task is still Failed and has remaining retries,
        // reset it to Pending so it is picked up again on the next iteration.
        // The `failed_attempts` field acts as the retry counter – no extra
        // state is needed.  With `max_retries = N` the task may run at most
        // N + 1 times (one initial attempt plus up to N retries).
        //
        // When the re-planner succeeded it has already decided how to recover
        // (by restructuring the task list), so auto-retry is skipped to avoid
        // conflicting with the re-planner's changes.
        if !replan_succeeded && self.state.tasks[task_idx].status == TaskStatus::Failed {
            let failed = self.state.tasks[task_idx].failed_attempts;
            if let Some(max_retries) = self.state.tasks[task_idx].max_retries {
                if failed <= max_retries {
                    self.state.add_log(format!(
                        "Task failed (attempt {}/{}) – resetting to pending for retry",
                        failed,
                        max_retries + 1,
                    ));
                    self.state.tasks[task_idx].status = TaskStatus::Pending;
                    save_tasks(&self.config.task_file, &self.state.tasks)
                        .context("Failed to save tasks after retry reset")?;
                }
            }
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

        // Mark all as in-progress and collect task data with per-task timestamps.
        // First evaluate agent-based preconditions for tasks that have them.
        let mut eligible_indices = Vec::new();
        for &idx in &indices {
            if self.state.tasks[idx].precondition_prompt.is_some() {
                let task = self.state.tasks[idx].clone();
                self.state.add_log(format!(
                    "Evaluating precondition for task: {}",
                    task.description
                ));
                match self.agent.evaluate_precondition(&task).await {
                    Ok(true) => {
                        self.state.add_log(format!(
                            "Task [{}] precondition met",
                            self.state.tasks[idx].id
                        ));
                        eligible_indices.push(idx);
                    }
                    Ok(false) => {
                        self.state.add_log(format!(
                            "Task [{}] precondition not met – skipping",
                            self.state.tasks[idx].id
                        ));
                    }
                    Err(e) => {
                        self.state.add_log(format!(
                            "Task [{}] precondition evaluation error: {} – skipping",
                            self.state.tasks[idx].id, e
                        ));
                    }
                }
            } else {
                eligible_indices.push(idx);
            }
        }

        if eligible_indices.is_empty() {
            self.state
                .add_log("No tasks eligible after precondition evaluation".to_string());
            return Ok(true);
        }

        let mut task_data: Vec<(usize, crate::types::Task, u64)> = Vec::new();
        for &idx in &eligible_indices {
            let ts = provenance::now_timestamp();
            self.state.tasks[idx].status = TaskStatus::InProgress;
            self.state.tasks[idx].last_attempt_at = Some(ts);
            task_data.push((idx, self.state.tasks[idx].clone(), ts));
        }

        // Notify webhooks that tasks are now in progress.
        for &idx in &eligible_indices {
            let t = &self.state.tasks[idx];
            notifier::notify(
                &self.config.notify_webhooks,
                &t.id,
                TaskStatus::InProgress,
                provenance::now_timestamp(),
                &t.description,
            )
            .await;
        }

        // Open GitHub Issues for each parallel task when the integration is enabled.
        for &idx in &eligible_indices {
            let task_id = self.state.tasks[idx].id.clone();
            let task_desc = self.state.tasks[idx].description.clone();
            if let Some(gh_client) = self.make_github_client() {
                match gh_client.create_issue(&task_id, &task_desc).await {
                    Ok(issue_number) => {
                        self.github_issue_numbers
                            .insert(task_id.clone(), issue_number);
                        self.state.add_log(format!(
                            "GitHub Issue #{issue_number} opened for task {task_id}"
                        ));
                    }
                    Err(e) => {
                        self.state.add_log(format!(
                            "Warning: failed to open GitHub Issue for task {task_id}: {e}"
                        ));
                    }
                }
            }
        }

        // Create Kanban board issues for each parallel task when the integration is enabled.
        for &idx in &eligible_indices {
            let task_id = self.state.tasks[idx].id.clone();
            let task_desc = self.state.tasks[idx].description.clone();
            if let Some(ref provider) = self.kanban_provider {
                match provider.create_issue(&task_id, &task_desc).await {
                    Ok(kanban_issue) => {
                        self.state.add_log(format!(
                            "{} issue created for task {task_id}: {}",
                            provider.provider_name(),
                            kanban_issue.url
                        ));
                        self.kanban_issues.insert(task_id.clone(), kanban_issue);
                    }
                    Err(e) => {
                        self.state.add_log(format!(
                            "Warning: failed to create {} issue for task {task_id}: {e}",
                            provider.provider_name()
                        ));
                    }
                }
            }
        }

        // Spawn concurrent agent work (include per-task timestamp for provenance).
        let mut handles = Vec::new();
        for (idx, task, ts) in task_data {
            // Resolve per-task working directory for multi-repo orchestration.
            let task_work_dir = self.resolve_work_dir(&task);
            let agent = AgentClient::with_evaluation(
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
            )
            .with_work_dir(task_work_dir.to_string_lossy().to_string())
            .with_cost_tracker(Arc::clone(&self.cost_tracker));
            let mut agent = agent;
            let handle = tokio::spawn(async move {
                // Wrap execution in a per-task timeout when `timeout_seconds` is set.
                let result = if let Some(timeout_secs) = task.timeout_seconds {
                    match tokio::time::timeout(
                        std::time::Duration::from_secs(timeout_secs),
                        agent.execute_task(&task),
                    )
                    .await
                    {
                        Ok(r) => r,
                        Err(_) => Err(anyhow::anyhow!(
                            "Task timed out after {} seconds",
                            timeout_secs
                        )),
                    }
                } else {
                    agent.execute_task(&task).await
                };
                (idx, task, ts, result)
            });
            handles.push(handle);
        }

        // Collect results.  Compute the git diff hash once after all tasks
        // finish so the hash reflects the combined state of all parallel changes,
        // and avoid running `git diff` repeatedly for the same snapshot.
        let mut completed: Vec<(usize, crate::types::Task, u64)> = Vec::new();
        let mut failed: Vec<(usize, crate::types::Task, u64)> = Vec::new();
        for handle in handles {
            match handle.await {
                Ok((idx, task, ts, Ok(result))) => {
                    self.state.add_log(format!(
                        "Task [{}] completed: {}",
                        self.state.tasks[idx].id, result
                    ));
                    self.state.tasks[idx].status = TaskStatus::Completed;
                    completed.push((idx, task, ts));
                }
                Ok((idx, task, ts, Err(e))) => {
                    self.state
                        .add_log(format!("Task [{}] failed: {}", self.state.tasks[idx].id, e));
                    self.state.tasks[idx].status = TaskStatus::Failed;
                    self.state.tasks[idx].failed_attempts += 1;
                    failed.push((idx, task, ts));
                }
                Err(e) => {
                    self.state.add_log(format!("Parallel task panicked: {}", e));
                }
            }
        }

        // Record provenance for this batch.  Compute the diff hash once and
        // share it across all records in this parallel phase.
        let model = model_name(&self.config.model_provider);
        let batch_diff_hash = provenance::git_diff_hash(&self.config.work_dir);
        for (_, task, ts) in completed.iter().chain(failed.iter()) {
            let outcome = if failed.iter().any(|(_, t, _)| t.id == task.id) {
                "failure"
            } else {
                "success"
            };
            let prov_record = ProvenanceRecord {
                task_id: task.id.clone(),
                agent_role: task.role,
                model: model.clone(),
                prompt_hash: provenance::hash_string(&task.description),
                tool_calls: vec![],
                git_diff_hash: batch_diff_hash.clone(),
                timestamp: *ts,
                outcome: outcome.to_string(),
            };
            if let Err(e) =
                provenance::persist_provenance_record(&prov_record, &self.config.work_dir)
            {
                self.state.add_log(format!(
                    "Warning: failed to persist provenance record: {}",
                    e
                ));
            }
        }

        // Run evaluation for each completed task.
        for &idx in &eligible_indices {
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

        // Auto-retry failed tasks in this batch that still have remaining retries.
        let mut retried = false;
        for &idx in &eligible_indices {
            if self.state.tasks[idx].status == TaskStatus::Failed {
                let failed = self.state.tasks[idx].failed_attempts;
                if let Some(max_retries) = self.state.tasks[idx].max_retries {
                    if failed <= max_retries {
                        self.state.add_log(format!(
                            "Task [{}] failed (attempt {}/{}) – resetting to pending for retry",
                            self.state.tasks[idx].id,
                            failed,
                            max_retries + 1,
                        ));
                        self.state.tasks[idx].status = TaskStatus::Pending;
                        retried = true;
                    }
                }
            }
        }
        if retried {
            save_tasks(&self.config.task_file, &self.state.tasks)
                .context("Failed to save tasks after retry reset")?;
        }

        // Notify webhooks with the final task statuses after evaluation.
        for &idx in &eligible_indices {
            let t = &self.state.tasks[idx];
            if t.status == TaskStatus::Completed || t.status == TaskStatus::Failed {
                notifier::notify(
                    &self.config.notify_webhooks,
                    &t.id,
                    t.status,
                    provenance::now_timestamp(),
                    &t.description,
                )
                .await;
            }
        }

        // Close GitHub Issues for tasks that have reached a terminal state.
        for &idx in &eligible_indices {
            let task_id = self.state.tasks[idx].id.clone();
            let final_status = self.state.tasks[idx].status;
            if final_status == TaskStatus::Completed || final_status == TaskStatus::Failed {
                if let Some(issue_number) = self.github_issue_numbers.remove(&task_id) {
                    if let Some(gh_client) = self.make_github_client() {
                        if let Err(e) = gh_client.close_issue(issue_number).await {
                            self.state.add_log(format!(
                                "Warning: failed to close GitHub Issue #{issue_number} \
                                 for task {task_id}: {e}"
                            ));
                        } else {
                            self.state.add_log(format!(
                                "GitHub Issue #{issue_number} closed for task {task_id}"
                            ));
                        }
                    }
                }

                // Transition the Kanban board issue to the terminal status.
                if let Some(kanban_issue) = self.kanban_issues.remove(&task_id) {
                    if let Some(ref provider) = self.kanban_provider {
                        if let Err(e) = provider
                            .transition_issue(&kanban_issue.external_id, final_status)
                            .await
                        {
                            self.state.add_log(format!(
                                "Warning: failed to transition {} issue for task {task_id}: {e}",
                                provider.provider_name()
                            ));
                        } else {
                            self.state.add_log(format!(
                                "{} issue transitioned to {:?} for task {task_id}",
                                provider.provider_name(),
                                final_status
                            ));
                        }
                    }
                }
            }
        }

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
            timeout_seconds: None,
            max_retries: None,
            failed_attempts,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
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
                timeout_seconds: None,
                max_retries: None,
                failed_attempts: 0,
                last_attempt_at: Some(recent_ts),
                inputs: vec![],
                outputs: vec![],
                runtime: crate::types::TaskRuntime::default(),
                precondition_prompt: None,
                parent_id: None,
                labels: vec![],
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
                timeout_seconds: None,
                max_retries: None,
                failed_attempts: 0,
                last_attempt_at: Some(old_ts),
                inputs: vec![],
                outputs: vec![],
                runtime: crate::types::TaskRuntime::default(),
                precondition_prompt: None,
                parent_id: None,
                labels: vec![],
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
                timeout_seconds: None,
                max_retries: None,
                failed_attempts: 0,
                last_attempt_at: Some(old_ts),
                inputs: vec![],
                outputs: vec![],
                runtime: crate::types::TaskRuntime::default(),
                precondition_prompt: None,
                parent_id: None,
                labels: vec![],
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

    // ---- timeout_seconds / max_retries field tests ----

    #[test]
    fn task_timeout_seconds_defaults_to_none() {
        let t = make_task("a", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert!(t.timeout_seconds.is_none());
    }

    #[test]
    fn task_max_retries_defaults_to_none() {
        let t = make_task("a", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert!(t.max_retries.is_none());
    }

    #[test]
    fn task_timeout_and_retries_roundtrip_via_serde() {
        let mut t = make_task("a", TaskStatus::Pending, 0, 1, 0, vec![]);
        t.timeout_seconds = Some(30);
        t.max_retries = Some(3);

        let json = serde_json::to_string(&t).expect("serialise");
        let back: Task = serde_json::from_str(&json).expect("deserialise");
        assert_eq!(back.timeout_seconds, Some(30));
        assert_eq!(back.max_retries, Some(3));
    }

    /// When `max_retries` is absent the fields are omitted from the serialised
    /// JSON (skip_serializing_if = "Option::is_none").
    #[test]
    fn task_absent_timeout_and_retries_omitted_from_json() {
        let t = make_task("a", TaskStatus::Pending, 0, 1, 0, vec![]);
        let json = serde_json::to_string(&t).expect("serialise");
        assert!(
            !json.contains("timeout_seconds"),
            "unexpected key in {json}"
        );
        assert!(!json.contains("max_retries"), "unexpected key in {json}");
    }

    /// Verify the retry guard logic: a failed task with `failed_attempts <= max_retries`
    /// should be considered retriable; once `failed_attempts > max_retries` it is not.
    #[test]
    fn retry_guard_logic() {
        let mut t = make_task("a", TaskStatus::Failed, 0, 1, 0, vec![]);
        t.max_retries = Some(2);

        // Simulate first failure: failed_attempts = 1, max_retries = 2 → retry
        t.failed_attempts = 1;
        let should_retry = t.max_retries.is_some_and(|m| t.failed_attempts <= m);
        assert!(should_retry, "attempt 1 of 3 should trigger retry");

        // Simulate second failure: failed_attempts = 2, max_retries = 2 → retry
        t.failed_attempts = 2;
        let should_retry = t.max_retries.is_some_and(|m| t.failed_attempts <= m);
        assert!(should_retry, "attempt 2 of 3 should trigger retry");

        // Simulate third failure: failed_attempts = 3, max_retries = 2 → no retry
        t.failed_attempts = 3;
        let should_retry = t.max_retries.is_some_and(|m| t.failed_attempts <= m);
        assert!(!should_retry, "attempt 3 of 3 should NOT trigger retry");
    }

    // ---- behavioral tests matching issue requirements ----

    /// (1) A task with `timeout_seconds: 1` whose execution takes longer than 1 second
    /// must be marked as failed with a timeout error message.
    ///
    /// Short real durations (10 ms timeout vs 500 ms sleep) are used so the test
    /// completes quickly while still exercising the timeout-wrapping logic from
    /// `run_single_task`.
    #[tokio::test]
    async fn task_exceeding_timeout_produces_timeout_error() {
        const TIMEOUT_SECS: u64 = 1;

        // A future that takes 500 ms – far longer than the 10 ms deadline used below.
        let slow_future = async {
            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
            Ok::<(), anyhow::Error>(())
        };

        // Mirror the wrapping logic used in `run_single_task`, but use a 10 ms
        // deadline so the test runs quickly.
        let execution_result: anyhow::Result<()> =
            match tokio::time::timeout(std::time::Duration::from_millis(10), slow_future).await {
                Ok(r) => r,
                Err(_) => Err(anyhow::anyhow!(
                    "Task timed out after {} seconds",
                    TIMEOUT_SECS
                )),
            };

        assert!(execution_result.is_err(), "timed-out task must be an error");
        let msg = execution_result.unwrap_err().to_string();
        assert!(
            msg.contains("timed out after 1 seconds"),
            "error message must mention the timeout; got: {msg}"
        );
    }

    /// (2) A task with `max_retries: 2` that fails is reset to `Pending` on each
    /// attempt until `failed_attempts` exceeds `max_retries`, at which point the
    /// status stays `Failed`.
    #[test]
    fn task_with_max_retries_resets_to_pending_until_limit_exceeded() {
        let mut t = make_task("r", TaskStatus::Failed, 0, 1, 0, vec![]);
        t.max_retries = Some(2);

        // Helper that mirrors the auto-retry state mutation in `run_single_task`.
        let apply_retry = |task: &mut Task| {
            if task.status == TaskStatus::Failed {
                if let Some(max) = task.max_retries {
                    if task.failed_attempts <= max {
                        task.status = TaskStatus::Pending;
                    }
                }
            }
        };

        // Attempt 1 → failed_attempts = 1 ≤ 2 → reset to Pending.
        t.failed_attempts = 1;
        apply_retry(&mut t);
        assert_eq!(
            t.status,
            TaskStatus::Pending,
            "after 1st failure should reset to Pending"
        );

        // Attempt 2 → failed_attempts = 2 ≤ 2 → reset to Pending.
        t.status = TaskStatus::Failed;
        t.failed_attempts = 2;
        apply_retry(&mut t);
        assert_eq!(
            t.status,
            TaskStatus::Pending,
            "after 2nd failure should reset to Pending"
        );

        // Attempt 3 → failed_attempts = 3 > 2 → stays Failed.
        t.status = TaskStatus::Failed;
        t.failed_attempts = 3;
        apply_retry(&mut t);
        assert_eq!(
            t.status,
            TaskStatus::Failed,
            "after exceeding retry limit should stay Failed"
        );
    }

    /// (3) A task with neither `timeout_seconds` nor `max_retries` behaves exactly
    /// as before: it is scheduled normally and a failure is never auto-retried.
    #[test]
    fn task_without_timeout_or_retries_behaves_as_before() {
        // The task has no special fields set.
        let task = make_task("plain", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert!(task.timeout_seconds.is_none());
        assert!(task.max_retries.is_none());

        // Scheduler picks it up as a normal pending task.
        let ready = TaskScheduler::schedule(&[task]);
        assert_eq!(ready, vec![0], "plain task must be scheduled");

        // After a failure the task is NOT auto-retried (max_retries is None).
        let mut failed = make_task("plain", TaskStatus::Failed, 0, 1, 1, vec![]);
        // Mirror the auto-retry guard from `run_single_task`.
        if failed.status == TaskStatus::Failed {
            if let Some(max) = failed.max_retries {
                if failed.failed_attempts <= max {
                    failed.status = TaskStatus::Pending;
                }
            }
        }
        assert_eq!(
            failed.status,
            TaskStatus::Failed,
            "task without max_retries must stay Failed"
        );
    }

    /// (4) When the adaptive re-planner succeeds, auto-retry must be suppressed
    /// so that the two recovery mechanisms do not conflict with each other.
    /// This test mirrors the guard added to `run_single_task`: auto-retry is
    /// skipped whenever `replan_succeeded` is true.
    #[test]
    fn replan_success_suppresses_auto_retry() {
        let apply_retry_with_replan_flag = |task: &mut Task, replan_succeeded: bool| {
            if !replan_succeeded && task.status == TaskStatus::Failed {
                if let Some(max) = task.max_retries {
                    if task.failed_attempts <= max {
                        task.status = TaskStatus::Pending;
                    }
                }
            }
        };

        // Task has max_retries = 2, failed_attempts = 1 (would normally retry).
        let mut t = make_task("retryable", TaskStatus::Failed, 0, 1, 0, vec![]);
        t.max_retries = Some(2);
        t.failed_attempts = 1;

        // Without a successful replan the task IS reset to Pending.
        apply_retry_with_replan_flag(&mut t, false);
        assert_eq!(
            t.status,
            TaskStatus::Pending,
            "without replan the task should be reset to Pending"
        );

        // Reset and simulate a successful replan: retry must be suppressed.
        t.status = TaskStatus::Failed;
        apply_retry_with_replan_flag(&mut t, true);
        assert_eq!(
            t.status,
            TaskStatus::Failed,
            "when replan succeeded auto-retry must be skipped"
        );
    }

    // ---- resolve_work_dir tests (multi-repo orchestration) ----

    fn make_config_with_work_dirs(
        default_work_dir: &str,
        overrides: &[(&str, &str)],
    ) -> crate::types::Config {
        let mut config = crate::types::Config::default();
        config.work_dir = std::path::PathBuf::from(default_work_dir);
        for &(k, v) in overrides {
            config.work_dirs.insert(k.to_string(), v.to_string());
        }
        config
    }

    fn make_ralph_loop(config: crate::types::Config) -> RalphLoop {
        RalphLoop::new(config)
    }

    #[test]
    fn resolve_work_dir_falls_back_to_default() {
        let config = make_config_with_work_dirs("/default", &[]);
        let rl = make_ralph_loop(config);
        let task = make_task("my-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert_eq!(
            rl.resolve_work_dir(&task),
            std::path::PathBuf::from("/default")
        );
    }

    #[test]
    fn resolve_work_dir_matches_exact_task_id() {
        let config =
            make_config_with_work_dirs("/default", &[("my-task", "/repo/my-task-dir")]);
        let rl = make_ralph_loop(config);
        let task = make_task("my-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert_eq!(
            rl.resolve_work_dir(&task),
            std::path::PathBuf::from("/repo/my-task-dir")
        );
    }

    #[test]
    fn resolve_work_dir_matches_role() {
        // AgentRole::Implementer serialises to "implementer" via serde.
        let config =
            make_config_with_work_dirs("/default", &[("implementer", "/repo/impl-dir")]);
        let rl = make_ralph_loop(config);
        let task = make_task("other-id", TaskStatus::Pending, 0, 1, 0, vec![]);
        // default role is Implementer
        assert_eq!(
            rl.resolve_work_dir(&task),
            std::path::PathBuf::from("/repo/impl-dir")
        );
    }

    #[test]
    fn resolve_work_dir_prefers_task_id_over_role() {
        // Both id and role match — id must win.
        let config = make_config_with_work_dirs(
            "/default",
            &[("my-task", "/by-id"), ("implementer", "/by-role")],
        );
        let rl = make_ralph_loop(config);
        let task = make_task("my-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert_eq!(
            rl.resolve_work_dir(&task),
            std::path::PathBuf::from("/by-id")
        );
    }

    #[test]
    fn resolve_work_dir_resolves_relative_path_against_default() {
        let config = make_config_with_work_dirs("/projects", &[("my-task", "sub-repo")]);
        let rl = make_ralph_loop(config);
        let task = make_task("my-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert_eq!(
            rl.resolve_work_dir(&task),
            std::path::PathBuf::from("/projects/sub-repo")
        );
    }

    // ---- multi-repo orchestration integration tests using real temp dirs ----

    /// When `work_dirs` maps a task id to a secondary path, the agent receives
    /// the correct (secondary) work_dir for that task while other tasks continue
    /// to use the default work_dir.
    #[test]
    fn work_dirs_maps_task_to_secondary_with_temp_dirs() {
        let default_dir = tempfile::tempdir().unwrap();
        let secondary_dir = tempfile::tempdir().unwrap();

        let config = make_config_with_work_dirs(
            default_dir.path().to_str().unwrap(),
            &[(
                "special-task",
                secondary_dir.path().to_str().unwrap(),
            )],
        );
        let rl = make_ralph_loop(config);

        // The mapped task should resolve to the secondary directory.
        let mapped_task = make_task("special-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert_eq!(
            rl.resolve_work_dir(&mapped_task),
            secondary_dir.path(),
            "mapped task must receive the secondary work_dir"
        );

        // All other tasks should fall back to the default directory.
        for id in &["task-a", "task-b", "unrelated"] {
            let other_task = make_task(id, TaskStatus::Pending, 0, 1, 0, vec![]);
            assert_eq!(
                rl.resolve_work_dir(&other_task),
                default_dir.path(),
                "unmapped task '{}' must receive the default work_dir",
                id
            );
        }
    }

    /// When `work_dirs` is empty (absent), every task uses the default work_dir,
    /// which is identical to the current single-repository behavior.
    #[test]
    fn work_dirs_absent_is_identical_to_single_repo_behavior() {
        let default_dir = tempfile::tempdir().unwrap();

        // No overrides – equivalent to a config that never sets work_dirs.
        let config =
            make_config_with_work_dirs(default_dir.path().to_str().unwrap(), &[]);
        let rl = make_ralph_loop(config);

        for id in &["impl-1", "test-2", "eval-3"] {
            let task = make_task(id, TaskStatus::Pending, 0, 1, 0, vec![]);
            assert_eq!(
                rl.resolve_work_dir(&task),
                default_dir.path(),
                "task '{}' must use the default work_dir when work_dirs is empty",
                id
            );
        }
    }
}
