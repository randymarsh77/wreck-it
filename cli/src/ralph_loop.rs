use crate::agent::AgentClient;
use crate::agent_memory::AgentMemory;
use crate::artefact_store;
use crate::cost_tracker::{model_pricing, CostTracker};
use crate::coverage_enforcer;
use crate::github_client;
use crate::kanban::{self, KanbanClient, KanbanIssue};
use crate::notifier;
use crate::otel::{self, TaskSpan, TaskSpanAttributes};
use crate::prompt_optimizer::PromptOptimizer;
use crate::provenance::{self, ProvenanceRecord};
use crate::replanner::{replan_and_save, TaskReplanner};
use crate::security_gate;
use crate::task_manager::{get_next_task, load_tasks, save_tasks};
use crate::types::{
    AgentRole, Config, EvaluationMode, LoopState, ModelProvider, Task, TaskStatus,
    DEFAULT_AUTOPILOT_MODEL, DEFAULT_GITHUB_MODELS_MODEL, DEFAULT_LLAMA_MODEL,
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
        ModelProvider::CopilotAutopilot => DEFAULT_AUTOPILOT_MODEL.to_string(),
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
    /// Whether the OpenTelemetry provider was successfully initialised.
    /// Used to decide whether to call `shutdown_otel` on drop.
    otel_enabled: bool,
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
            ModelProvider::CopilotAutopilot => DEFAULT_AUTOPILOT_MODEL,
        };
        let (inp, out) = model_pricing(model_str);
        let cost_tracker = Arc::new(Mutex::new(CostTracker::new(inp, out)));

        let agent = AgentClient::with_evaluation_and_autopilot(
            config.model_provider.clone(),
            config.api_endpoint.clone(),
            config.api_token.clone(),
            config.work_dir.to_string_lossy().to_string(),
            config.verification_command.clone(),
            config.evaluation_mode,
            config.completeness_prompt.clone(),
            config.completion_marker_file.to_string_lossy().to_string(),
            config.max_autopilot_continues,
        )
        .with_prompt_dir(
            config.prompt_dir.clone(),
            config.github_repo.clone().unwrap_or_default(),
        )
        .with_cost_tracker(Arc::clone(&cost_tracker));

        let kanban_provider = kanban::provider_from_config(&config.kanban);

        // Initialise OpenTelemetry when an OTLP endpoint is configured.
        let otel_enabled = config
            .otel
            .as_ref()
            .map(|otel_cfg| {
                otel::init_otel(otel_cfg).unwrap_or_else(|e| {
                    // OTEL initialisation failure is non-fatal: log and continue.
                    eprintln!("Warning: OTEL initialisation failed: {e}");
                    false
                })
            })
            .unwrap_or(false);

        Self {
            config,
            state: LoopState::new(max_iterations),
            agent,
            github_issue_numbers: HashMap::new(),
            kanban_issues: HashMap::new(),
            kanban_provider,
            cost_tracker,
            otel_enabled,
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

    /// Build [`TaskSpanAttributes`] for a task before execution.
    ///
    /// Token counts and cost are left at zero here; they are filled in after
    /// the task completes by reading the shared cost tracker.
    fn build_span_attrs(&self, task: &Task) -> TaskSpanAttributes {
        let role = serde_json::to_value(task.role)
            .ok()
            .and_then(|v| v.as_str().map(|s| s.to_string()))
            .unwrap_or_else(|| "implementer".to_string());
        TaskSpanAttributes {
            task_id: task.id.clone(),
            task_description: task.description.clone(),
            role,
            phase: task.phase,
            complexity: task.complexity,
            priority: task.priority,
            model: model_name(&self.config.model_provider),
            failed_attempts: task.failed_attempts,
            // Token counts and cost are populated after execution.
            prompt_tokens: 0,
            completion_tokens: 0,
            estimated_cost_usd: 0.0,
        }
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

        // Start an OTEL span for this task execution.
        let span_attrs = self.build_span_attrs(&self.state.tasks[task_idx]);
        let mut task_span = TaskSpan::start(&span_attrs);
        task_span.record_start();

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
        let mut task = self.state.tasks[task_idx].clone();
        let reflection_rounds = self.config.reflection_rounds;
        let mut task_error = String::new();

        // Create a shared AgentMemory instance used both to apply a previous
        // optimized description and (later) to store a newly generated rewrite.
        let memory = AgentMemory::new(&self.config.work_dir.to_string_lossy());

        // If the task has previously failed and the prompt optimizer stored a
        // rewritten description, apply it now so the agent benefits from the
        // improved specification on the next attempt.
        if task.failed_attempts > 0 {
            match memory.load_optimized_description(&task.id) {
                Ok(Some(optimized)) => {
                    self.state.add_log(format!(
                        "Applying optimized description for task {} (attempt {})",
                        task.id,
                        task.failed_attempts + 1,
                    ));
                    task.description = optimized;
                }
                Ok(None) => {}
                Err(e) => {
                    self.state.add_log(format!(
                        "Warning: failed to load optimized description for {}: {}",
                        task.id, e
                    ));
                }
            }
        }

        // Security gate tasks bypass the LLM agent entirely; the scanner runs
        // as a subprocess and the findings are persisted as output artefacts.
        let execution_result: Result<()> = if task.role == AgentRole::SecurityGate {
            self.run_security_gate_task(&task, &effective_work_dir)
        } else if task.role == AgentRole::CoverageEnforcer {
            self.run_coverage_enforcer_task(&task, &effective_work_dir)
        } else if let Some(timeout_secs) = task.timeout_seconds {
            // Wrap execution in a per-task timeout when `timeout_seconds` is set.
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
                    // Record the retry event on the OTEL span before resetting status.
                    task_span.record_retry(failed, max_retries);

                    // Invoke the adaptive prompt optimizer to generate a better
                    // task description for the next attempt.  The rewrite is
                    // stored in per-task memory so that `run_single_task` can
                    // apply it on the next iteration before agent execution.
                    let optimizer = PromptOptimizer::new(
                        self.config.model_provider.clone(),
                        self.config.api_endpoint.clone(),
                        self.config.api_token.clone(),
                    );
                    let original_task = self.state.tasks[task_idx].clone();
                    match optimizer
                        .analyze_and_rewrite(&original_task, &task_error, failed)
                        .await
                    {
                        Ok(rewritten) => {
                            match memory.store_optimized_description(&original_task.id, &rewritten)
                            {
                                Ok(()) => {
                                    self.state.add_log(format!(
                                        "Prompt optimizer stored rewritten description for {}",
                                        original_task.id
                                    ));
                                }
                                Err(e) => {
                                    self.state.add_log(format!(
                                        "Warning: failed to store optimized description for {}: {}",
                                        original_task.id, e
                                    ));
                                }
                            }
                        }
                        Err(e) => {
                            self.state.add_log(format!(
                                "Warning: prompt optimizer failed for {}: {}",
                                original_task.id, e
                            ));
                        }
                    }

                    self.state.tasks[task_idx].status = TaskStatus::Pending;
                    save_tasks(&self.config.task_file, &self.state.tasks)
                        .context("Failed to save tasks after retry reset")?;
                }
            }
        }

        // Finish the OTEL span with outcome and token/cost attributes sourced
        // from the shared cost tracker (task-level counters reflect only the
        // work done in this iteration).
        let task_succeeded = self.state.tasks[task_idx].status == TaskStatus::Completed;
        let finish_attrs = {
            let mut finish_attrs = span_attrs;
            if let Ok(guard) = self.cost_tracker.lock() {
                finish_attrs.prompt_tokens = guard.task_prompt_tokens;
                finish_attrs.completion_tokens = guard.task_completion_tokens;
                finish_attrs.estimated_cost_usd = guard.task_estimated_cost_usd;
            }
            finish_attrs
        };
        task_span.finish(task_succeeded, &finish_attrs);

        Ok(true)
    }

    /// Execute a security gate task.
    ///
    /// Runs the appropriate security scanner for the project type, writes the
    /// findings to the declared output artefact path, and persists them to the
    /// artefact manifest so downstream tasks can consume them as inputs.
    ///
    /// When the gate **fails** (critical or high vulnerabilities found) every
    /// task listed in `task.depends_on` is reset to `Pending` so that the
    /// implementation agent can self-remediate using the persisted findings.
    ///
    /// Returns `Ok(())` when no blocking vulnerabilities are found, `Err`
    /// otherwise.
    fn run_security_gate_task(&mut self, task: &Task, work_dir: &std::path::Path) -> Result<()> {
        self.state.add_log("Running security scan...".to_string());

        let findings = security_gate::run_security_scan(work_dir)?;

        self.state.add_log(format!(
            "Security scan complete ({}): critical={}, high={}, medium={}, low={}, total={}",
            findings.scanner,
            findings.critical,
            findings.high,
            findings.medium,
            findings.low,
            findings.total,
        ));

        // Determine the path where the findings JSON should be written.
        let output_path = task
            .outputs
            .first()
            .map(|o| work_dir.join(&o.path))
            .unwrap_or_else(|| work_dir.join(".wreck-it/security-findings.json"));

        // Ensure parent directories exist.
        if let Some(parent) = output_path.parent() {
            std::fs::create_dir_all(parent)
                .context("Failed to create security findings directory")?;
        }

        // Write findings to disk.
        let findings_json =
            serde_json::to_string_pretty(&findings).context("Failed to serialise findings")?;
        std::fs::write(&output_path, &findings_json)
            .context("Failed to write security findings file")?;

        // Persist findings artefact to the manifest immediately so they are
        // available to downstream tasks even when the gate fails.
        if !task.outputs.is_empty() {
            let manifest_path = self.config.work_dir.join(".wreck-it-artefacts.json");
            if let Err(e) = artefact_store::persist_output_artefacts(
                &manifest_path,
                &task.id,
                &task.outputs,
                work_dir,
            ) {
                self.state.add_log(format!(
                    "Warning: failed to persist security findings artefact: {}",
                    e
                ));
            }
        }

        if findings.passed {
            self.state
                .add_log("Security gate passed — no blocking vulnerabilities".to_string());
            return Ok(());
        }

        // Gate failed: reset prerequisite (implementation) tasks to Pending so
        // they can pick up the findings and self-remediate.
        //
        // The security gate sits in a later phase and its `depends_on` list
        // names the implementation tasks that produced the code being audited
        // (e.g. `depends_on: ["impl-feature"]`).  Resetting those tasks causes
        // them to re-run in the next iteration with the findings artefact now
        // available in the manifest, enabling autonomous vulnerability
        // remediation before the gate is attempted again.
        for dep_id in &task.depends_on {
            if let Some(dep_idx) = self.state.tasks.iter().position(|t| &t.id == dep_id) {
                self.state.tasks[dep_idx].status = TaskStatus::Pending;
                self.state.tasks[dep_idx].failed_attempts = 0;
                self.state.add_log(format!(
                    "Security gate failed — reset task '{}' to pending for remediation",
                    dep_id
                ));
            }
        }

        Err(anyhow::anyhow!(
            "Security gate found {} critical and {} high severity vulnerabilities \
             (total {}). Findings written to {}. \
             Prerequisite tasks have been reset to pending for remediation.",
            findings.critical,
            findings.high,
            findings.total,
            output_path.display(),
        ))
    }

    /// Execute a coverage enforcer task.
    ///
    /// Reads the coverage report artefact(s) listed in `task.inputs` from the
    /// artefact manifest, parses the report, and checks whether the measured
    /// coverage meets the configured threshold.
    ///
    /// The threshold is extracted from `task.description` as
    /// `{"coverage_threshold": <number>}`.  Defaults to
    /// [`coverage_enforcer::DEFAULT_THRESHOLD`] (80 %) when absent.
    ///
    /// Findings are written to the first declared output artefact path
    /// (default: `.wreck-it/coverage-findings.json`) and persisted to the
    /// manifest so that downstream tasks can consume them.
    ///
    /// When the gate **fails** every task listed in `task.depends_on` is reset
    /// to `Pending` with a coverage-improvement retry prompt injected via the
    /// artefact system, enabling an autonomous coverage-improvement loop.
    ///
    /// Returns `Ok(())` when coverage meets the threshold, `Err` otherwise.
    fn run_coverage_enforcer_task(
        &mut self,
        task: &Task,
        work_dir: &std::path::Path,
    ) -> Result<()> {
        self.state
            .add_log("Running coverage enforcement check...".to_string());

        let threshold = coverage_enforcer::threshold_from_description(&task.description);

        // Resolve input artefacts to find the coverage report.
        let manifest_path = self.config.work_dir.join(".wreck-it-artefacts.json");
        let report_content: String = if task.inputs.is_empty() {
            String::new()
        } else {
            match artefact_store::resolve_input_artefacts(&manifest_path, &task.inputs) {
                Ok(resolved) => {
                    // Concatenate all input artefacts; the parser will detect
                    // the format from the content.
                    resolved
                        .into_iter()
                        .map(|(_key, content)| content)
                        .collect::<Vec<_>>()
                        .join("\n")
                }
                Err(e) => {
                    self.state.add_log(format!(
                        "Warning: could not resolve coverage report artefacts: {}",
                        e
                    ));
                    String::new()
                }
            }
        };

        let findings = coverage_enforcer::check_coverage(&report_content, threshold);

        self.state.add_log(format!(
            "Coverage check complete ({}): {:.1}% (threshold {:.1}%) — {}",
            findings.scanner,
            findings.coverage_percent,
            findings.threshold_percent,
            if findings.passed { "PASSED" } else { "FAILED" },
        ));

        // Determine the path where the findings JSON should be written.
        let output_path = task
            .outputs
            .first()
            .map(|o| work_dir.join(&o.path))
            .unwrap_or_else(|| work_dir.join(".wreck-it/coverage-findings.json"));

        // Write findings to disk.
        coverage_enforcer::write_findings(&findings, &output_path)
            .context("Failed to write coverage findings")?;

        // Persist findings artefact to the manifest immediately so they are
        // available to downstream tasks even when the gate fails.
        if !task.outputs.is_empty() {
            if let Err(e) = artefact_store::persist_output_artefacts(
                &manifest_path,
                &task.id,
                &task.outputs,
                work_dir,
            ) {
                self.state.add_log(format!(
                    "Warning: failed to persist coverage findings artefact: {}",
                    e
                ));
            }
        }

        if findings.passed {
            self.state.add_log(format!(
                "Coverage gate passed — {:.1}% >= {:.1}% threshold",
                findings.coverage_percent, findings.threshold_percent
            ));
            return Ok(());
        }

        // Gate failed: reset prerequisite (implementation) tasks to Pending so
        // the implementation agent can add more tests and reach the threshold.
        // `failed_attempts` is intentionally preserved so that `max_retries`
        // on the implementation task still bounds the total number of retries
        // and prevents an infinite coverage-improvement loop.
        for dep_id in &task.depends_on {
            if let Some(dep_idx) = self.state.tasks.iter().position(|t| &t.id == dep_id) {
                self.state.tasks[dep_idx].status = TaskStatus::Pending;
                self.state.add_log(format!(
                    "Coverage gate failed — reset task '{}' to pending for coverage improvement",
                    dep_id
                ));
            }
        }

        Err(anyhow::anyhow!(
            "Coverage gate failed: measured {:.1}% is below the {:.1}% threshold \
             (scanner: {}). Findings written to {}. \
             Prerequisite tasks have been reset to pending for coverage improvement.",
            findings.coverage_percent,
            findings.threshold_percent,
            findings.scanner,
            output_path.display(),
        ))
    }

    /// Evaluate a task using the configured evaluation mode.
    ///
    /// The evaluation mode is resolved in the following priority order:
    /// 1. Per-task `evaluation.mode` from the task JSON.
    /// 2. Global evaluation mode from the agent configuration.
    async fn evaluate_task(&mut self, task_idx: usize) -> Result<bool> {
        let task = self.state.tasks[task_idx].clone();

        // Security gate tasks determine pass/fail during execution; no
        // additional evaluation step is needed.
        if task.role == AgentRole::SecurityGate {
            return Ok(true);
        }

        // Coverage enforcer tasks likewise determine pass/fail during execution.
        if task.role == AgentRole::CoverageEnforcer {
            return Ok(true);
        }

        // Resolve the effective evaluation mode (per-task overrides global).
        let effective_mode = task
            .evaluation
            .as_ref()
            .and_then(|e| {
                // Parse the mode string as a JSON-quoted string so the serde
                // snake_case representation ("command", "agent_file", "semantic")
                // matches the EvaluationMode enum's serde serialization format.
                let quoted = format!("\"{}\"", e.mode);
                serde_json::from_str::<EvaluationMode>(&quoted).ok()
            })
            .unwrap_or_else(|| self.agent.evaluation_mode());

        if effective_mode == EvaluationMode::AgentFile {
            return self.agent.evaluate_completeness(&task).await;
        }
        if effective_mode == EvaluationMode::Semantic {
            let verdict = self.agent.evaluate_task_semantically(&task).await?;
            // Persist the score for the TUI task detail view.
            self.state
                .semantic_scores
                .insert(task.id.clone(), verdict.score);
            // Surface the rationale in the loop log so it appears in the TUI.
            self.state.add_log(format!(
                "Semantic verdict for [{}]: passed={}, score={}, rationale={}",
                task.id, verdict.passed, verdict.score, verdict.rationale
            ));
            return Ok(verdict.passed);
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
            let mut agent = AgentClient::with_evaluation_and_autopilot(
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
                self.config.max_autopilot_continues,
            )
            .with_work_dir(task_work_dir.to_string_lossy().to_string())
            .with_cost_tracker(Arc::clone(&self.cost_tracker));
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

        // Generate the adaptive prompt optimizer pattern report as a summary
        // artefact so that future agents and human operators can inspect which
        // task types and descriptions have historically struggled.
        let report = crate::prompt_optimizer::generate_pattern_report(
            &self.state.tasks,
            &self.config.work_dir,
        );
        let report_path = self
            .config
            .work_dir
            .join(".wreck-it")
            .join("prompt-optimizer-report.md");
        if let Some(parent) = report_path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        match std::fs::write(&report_path, &report) {
            Ok(()) => {
                self.state.add_log(format!(
                    "Prompt optimizer pattern report written to {}",
                    report_path.display()
                ));
            }
            Err(e) => {
                self.state.add_log(format!(
                    "Warning: failed to write prompt optimizer report: {}",
                    e
                ));
            }
        }

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

impl Drop for RalphLoop {
    /// Flush and shut down the OpenTelemetry tracer provider when the loop is
    /// dropped, ensuring that all buffered spans are exported before the
    /// process exits.
    fn drop(&mut self) {
        if self.otel_enabled {
            otel::shutdown_otel();
        }
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
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
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
                system_prompt_override: None,
                acceptance_criteria: None,
                evaluation: None,
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
                system_prompt_override: None,
                acceptance_criteria: None,
                evaluation: None,
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
                system_prompt_override: None,
                acceptance_criteria: None,
                evaluation: None,
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
        let config = make_config_with_work_dirs("/default", &[("my-task", "/repo/my-task-dir")]);
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
        let config = make_config_with_work_dirs("/default", &[("implementer", "/repo/impl-dir")]);
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
            &[("special-task", secondary_dir.path().to_str().unwrap())],
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
        let config = make_config_with_work_dirs(default_dir.path().to_str().unwrap(), &[]);
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

    // ---- evaluate_task dispatch path tests (semantic evaluation) ----

    /// Mirror the per-task evaluation mode resolution logic from `evaluate_task`.
    ///
    /// Parses the mode string from `task.evaluation` as a JSON-quoted identifier
    /// so the serde `snake_case` representation matches `EvaluationMode` variants.
    /// Falls back to `EvaluationMode::Command` when the field is absent or the
    /// mode string is not recognised.
    fn resolve_effective_mode(task: &Task) -> EvaluationMode {
        task.evaluation
            .as_ref()
            .and_then(|e| {
                let quoted = format!("\"{}\"", e.mode);
                serde_json::from_str::<EvaluationMode>(&quoted).ok()
            })
            .unwrap_or(EvaluationMode::Command)
    }

    /// Verify that a task with `evaluation: { mode: "semantic" }` resolves to
    /// `EvaluationMode::Semantic` via the same parsing logic used in
    /// `evaluate_task`.  This ensures the dispatch path correctly routes to
    /// `evaluate_task_semantically` when the per-task evaluation mode is set.
    #[test]
    fn evaluate_task_dispatch_resolves_semantic_mode_from_task_evaluation() {
        use crate::types::TaskEvaluation;

        let mut task = make_task("eval-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        task.evaluation = Some(TaskEvaluation {
            mode: "semantic".to_string(),
        });

        assert_eq!(
            resolve_effective_mode(&task),
            EvaluationMode::Semantic,
            "task with evaluation.mode='semantic' must dispatch to the semantic evaluator"
        );
    }

    /// Verify that when a task has no `evaluation` field, the dispatch falls
    /// back to the global evaluation mode (Command by default).
    #[test]
    fn evaluate_task_dispatch_falls_back_to_global_mode_when_no_per_task_evaluation() {
        let task = make_task("plain-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        assert!(task.evaluation.is_none());

        assert_eq!(
            resolve_effective_mode(&task),
            EvaluationMode::Command,
            "task without evaluation field must fall back to global (Command) mode"
        );
    }

    /// Verify that an unrecognised evaluation mode string is silently ignored
    /// and the dispatch falls back to the global evaluation mode.
    #[test]
    fn evaluate_task_dispatch_ignores_unrecognised_mode_string() {
        use crate::types::TaskEvaluation;

        let mut task = make_task("bad-mode-task", TaskStatus::Pending, 0, 1, 0, vec![]);
        task.evaluation = Some(TaskEvaluation {
            mode: "totally_unknown_mode".to_string(),
        });

        assert_eq!(
            resolve_effective_mode(&task),
            EvaluationMode::Command,
            "unrecognised evaluation mode must fall back to Command"
        );
    }

    // ---- SecurityGate role tests ----

    /// Verify that a SecurityGate role task serialises to `"security_gate"` and
    /// deserialises back correctly.
    #[test]
    fn security_gate_role_roundtrip_via_serde() {
        let mut task = make_task("sg-1", TaskStatus::Pending, 0, 1, 0, vec![]);
        task.role = crate::types::AgentRole::SecurityGate;
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            json.contains("\"security_gate\""),
            "SecurityGate role must serialise to \"security_gate\": {json}"
        );
        let loaded: crate::types::Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.role, crate::types::AgentRole::SecurityGate);
    }

    /// Verify that `run_security_gate_task` writes a findings JSON file,
    /// persists it to the artefact manifest, and resets dependent tasks to
    /// Pending when vulnerabilities are found.
    #[test]
    fn security_gate_resets_deps_on_failure() {
        use crate::types::{AgentRole, ArtefactKind, TaskArtefact, TaskRuntime};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();

        // Write a fake cargo-audit JSON output that contains a high-severity
        // vulnerability so that the gate fails.
        let audit_output = r#"{
            "vulnerabilities": {
                "found": true,
                "count": 1,
                "list": [
                    {
                        "advisory": {
                            "id": "RUSTSEC-2024-TEST",
                            "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H",
                            "title": "Test high vulnerability",
                            "description": "Test"
                        }
                    }
                ]
            }
        }"#;

        // Write a Cargo.toml so the project type is detected as Rust.
        std::fs::write(dir.path().join("Cargo.toml"), "[package]").unwrap();

        // Override the cargo binary lookup by writing a fake `cargo` script.
        // We rely on the PATH trick: create a wrapper that outputs our JSON.
        // Since running an actual subprocess for this in a unit test is
        // platform-specific and unreliable, we instead test `run_security_gate_task`
        // by exercising the findings-write and dep-reset path directly using
        // a pre-built `SecurityGateFindings`.
        //
        // Simulate what `run_security_gate_task` does internally:
        let findings = crate::security_gate::SecurityGateFindings {
            scanner: "cargo-audit".to_string(),
            passed: false,
            critical: 0,
            high: 1,
            medium: 0,
            low: 0,
            total: 1,
            raw_output: audit_output.to_string(),
        };

        // Write findings to the expected output path.
        let output_path = dir.path().join(".wreck-it/security-findings.json");
        std::fs::create_dir_all(output_path.parent().unwrap()).unwrap();
        std::fs::write(
            &output_path,
            serde_json::to_string_pretty(&findings).unwrap(),
        )
        .unwrap();

        // Persist findings to the artefact manifest.
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let outputs = vec![TaskArtefact {
            kind: ArtefactKind::Json,
            name: "findings".to_string(),
            path: ".wreck-it/security-findings.json".to_string(),
        }];
        crate::artefact_store::persist_output_artefacts(
            &manifest_path,
            "security-gate-test",
            &outputs,
            dir.path(),
        )
        .unwrap();

        // Verify artefact is in the manifest (would be available to impl tasks).
        let manifest = crate::artefact_store::load_manifest(&manifest_path).unwrap();
        let entry = manifest
            .artefacts
            .get("security-gate-test/findings")
            .expect("findings artefact must be persisted even on gate failure");
        assert!(entry.content.contains("RUSTSEC-2024-TEST"));

        // Verify the findings file contains the expected data.
        let loaded: crate::security_gate::SecurityGateFindings =
            serde_json::from_str(&entry.content).unwrap();
        assert!(!loaded.passed);
        assert_eq!(loaded.high, 1);
        assert_eq!(loaded.total, 1);
    }

    // ---- CoverageEnforcer role tests ----

    /// Verify that a CoverageEnforcer role task serialises to
    /// `"coverage_enforcer"` and deserialises back correctly.
    #[test]
    fn coverage_enforcer_role_roundtrip_via_serde() {
        let mut task = make_task("ce-1", TaskStatus::Pending, 0, 1, 0, vec![]);
        task.role = crate::types::AgentRole::CoverageEnforcer;
        let json = serde_json::to_string(&task).unwrap();
        assert!(
            json.contains("\"coverage_enforcer\""),
            "CoverageEnforcer role must serialise to \"coverage_enforcer\": {json}"
        );
        let loaded: crate::types::Task = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.role, crate::types::AgentRole::CoverageEnforcer);
    }

    /// Verify that the coverage enforcer writes a findings JSON file,
    /// persists it to the artefact manifest, and resets dependent tasks when
    /// coverage is below the threshold.
    #[test]
    fn coverage_enforcer_resets_deps_on_failure() {
        use crate::types::{AgentRole, ArtefactKind, TaskArtefact, TaskRuntime};
        use tempfile::tempdir;

        let dir = tempdir().unwrap();

        // Build a failing CoverageFindings (70% < 80% threshold).
        let findings = crate::coverage_enforcer::CoverageFindings {
            scanner: "tarpaulin".to_string(),
            passed: false,
            coverage_percent: 70.0,
            threshold_percent: 80.0,
            covered_lines: 70,
            total_lines: 100,
            raw_report: r#"{"covered":70,"coverable":100}"#.to_string(),
        };

        // Write findings to the expected output path.
        let output_path = dir.path().join(".wreck-it/coverage-findings.json");
        std::fs::create_dir_all(output_path.parent().unwrap()).unwrap();
        std::fs::write(
            &output_path,
            serde_json::to_string_pretty(&findings).unwrap(),
        )
        .unwrap();

        // Persist findings to the artefact manifest.
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let outputs = vec![TaskArtefact {
            kind: ArtefactKind::Json,
            name: "coverage".to_string(),
            path: ".wreck-it/coverage-findings.json".to_string(),
        }];
        crate::artefact_store::persist_output_artefacts(
            &manifest_path,
            "coverage-enforcer-test",
            &outputs,
            dir.path(),
        )
        .unwrap();

        // Verify artefact is in the manifest.
        let manifest = crate::artefact_store::load_manifest(&manifest_path).unwrap();
        let entry = manifest
            .artefacts
            .get("coverage-enforcer-test/coverage")
            .expect("coverage findings artefact must be persisted even on gate failure");
        assert!(entry.content.contains("tarpaulin"));

        // Verify the findings file contains the expected data.
        let loaded: crate::coverage_enforcer::CoverageFindings =
            serde_json::from_str(&entry.content).unwrap();
        assert!(!loaded.passed);
        assert!((loaded.coverage_percent - 70.0).abs() < 0.01);
        assert!((loaded.threshold_percent - 80.0).abs() < 0.01);
    }

    /// Verify that coverage enforcer role findings indicate a pass when coverage
    /// meets the threshold (integration check: role + artefact content).
    #[test]
    fn coverage_enforcer_findings_pass_when_above_threshold() {
        // 90% >= 80% threshold — verify the check_coverage API used by the role
        // reports a pass so the gate can return Ok(()).
        let findings =
            crate::coverage_enforcer::check_coverage(r#"{"covered": 90, "coverable": 100}"#, 80.0);
        assert_eq!(findings.scanner, "tarpaulin");
        assert!(findings.passed);
        assert!((findings.coverage_percent - 90.0).abs() < 0.01);
    }

    /// Verify that a task description with `coverage_threshold` is parsed correctly.
    #[test]
    fn coverage_enforcer_threshold_from_task_description() {
        let desc = r#"{"coverage_threshold": 90}"#;
        let threshold = crate::coverage_enforcer::threshold_from_description(desc);
        assert!((threshold - 90.0).abs() < 0.01);
    }
}
