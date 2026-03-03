//! Iteration processor — reads state from the state branch, advances the
//! task state machine, and commits updated state back via the GitHub API.

use crate::github::GitHubClient;
use crate::types::{HeadlessState, RepoConfig, Task};
use wreck_it_core::iteration::{advance_iteration, IterationOutcome};

/// Result of processing a single iteration.
#[derive(Debug)]
pub struct IterationResult {
    /// Human-readable summary of what happened.
    pub summary: String,
    /// Whether the iteration made any state changes.
    pub changed: bool,
}

/// Process one iteration for a repository.
///
/// 1. Read `.wreck-it/config.toml` from the default branch to discover the
///    state branch name and ralph contexts.
/// 2. For each ralph context (or the default single-ralph), read the task file
///    and state file from the state branch.
/// 3. Advance the state machine: select the next pending task, mark it
///    in-progress, bump the iteration counter.
/// 4. Write updated task and state files back to the state branch.
pub async fn process_iteration(
    client: &GitHubClient,
    default_branch: &str,
) -> Result<IterationResult, String> {
    // Step 1: Read repo config from the default branch.
    let config = read_repo_config(client, default_branch).await?;

    // Step 2: Determine ralph contexts.
    let contexts: Vec<RalphContext> = if config.ralphs.is_empty() {
        vec![RalphContext {
            name: "default".into(),
            task_file: "tasks.json".into(),
            state_file: ".wreck-it-state.json".into(),
        }]
    } else {
        config
            .ralphs
            .iter()
            .map(|r| RalphContext {
                name: r.name.clone(),
                task_file: r.task_file.clone(),
                state_file: r.state_file.clone(),
            })
            .collect()
    };

    let mut summaries = Vec::new();
    let mut any_changed = false;

    for ctx in &contexts {
        let result = process_ralph(client, &config.state_branch, ctx).await?;
        if result.changed {
            any_changed = true;
        }
        summaries.push(format!("[{}] {}", ctx.name, result.summary));
    }

    Ok(IterationResult {
        summary: summaries.join("; "),
        changed: any_changed,
    })
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

struct RalphContext {
    name: String,
    task_file: String,
    state_file: String,
}

/// Read and parse `.wreck-it/config.toml` from the default branch.
async fn read_repo_config(
    client: &GitHubClient,
    default_branch: &str,
) -> Result<RepoConfig, String> {
    let file = client
        .get_file(".wreck-it/config.toml", default_branch)
        .await?;
    match file {
        Some(f) => {
            let content = GitHubClient::decode_content(&f)?;
            toml::from_str(&content).map_err(|e| format!("Failed to parse repo config: {e}"))
        }
        None => Ok(RepoConfig::default()),
    }
}

/// Read a JSON file from the state branch, returning `None` if the file does
/// not exist.
async fn read_json_file<T: serde::de::DeserializeOwned>(
    client: &GitHubClient,
    branch: &str,
    path: &str,
) -> Result<Option<(T, String)>, String> {
    let file = client.get_file(path, branch).await?;
    match file {
        Some(f) => {
            let sha = f.sha.clone();
            let content = GitHubClient::decode_content(&f)?;
            let parsed: T = serde_json::from_str(&content)
                .map_err(|e| format!("Failed to parse {path}: {e}"))?;
            Ok(Some((parsed, sha)))
        }
        None => Ok(None),
    }
}

/// Process a single ralph context: read tasks + state, advance, write back.
async fn process_ralph(
    client: &GitHubClient,
    state_branch: &str,
    ctx: &RalphContext,
) -> Result<IterationResult, String> {
    // Read task file.
    let (mut tasks, tasks_sha) =
        match read_json_file::<Vec<Task>>(client, state_branch, &ctx.task_file).await? {
            Some(pair) => pair,
            None => {
                return Ok(IterationResult {
                    summary: format!("task file '{}' not found", ctx.task_file),
                    changed: false,
                });
            }
        };

    // Read state file (defaults if missing).
    let (mut state, state_sha) =
        match read_json_file::<HeadlessState>(client, state_branch, &ctx.state_file).await? {
            Some(pair) => pair,
            None => (HeadlessState::default(), String::new()),
        };

    // Use the shared iteration logic from wreck-it-core.
    let now_secs = js_sys_now_secs();
    let outcome = advance_iteration(&mut tasks, &mut state, now_secs);

    match outcome {
        IterationOutcome::AllComplete => Ok(IterationResult {
            summary: "all tasks complete".into(),
            changed: false,
        }),
        IterationOutcome::NoPendingTasks => Ok(IterationResult {
            summary: "no eligible pending tasks".into(),
            changed: false,
        }),
        IterationOutcome::TaskStarted {
            task_id,
            task_description,
        } => {
            // Write updated task file.
            let tasks_json = serde_json::to_string_pretty(&tasks)
                .map_err(|e| format!("Failed to serialize tasks: {e}"))?;
            client
                .put_file(
                    &ctx.task_file,
                    state_branch,
                    &tasks_json,
                    &format!(
                        "wreck-it: start task '{}' (iteration {})",
                        task_id, state.iteration
                    ),
                    Some(&tasks_sha),
                )
                .await?;

            // Write updated state file.
            let state_json = serde_json::to_string_pretty(&state)
                .map_err(|e| format!("Failed to serialize state: {e}"))?;
            let state_sha_opt = if state_sha.is_empty() {
                None
            } else {
                Some(state_sha.as_str())
            };
            client
                .put_file(
                    &ctx.state_file,
                    state_branch,
                    &state_json,
                    &format!("wreck-it: update state (iteration {})", state.iteration),
                    state_sha_opt,
                )
                .await?;

            Ok(IterationResult {
                summary: format!("started task '{}': {}", task_id, task_description),
                changed: true,
            })
        }
    }
}

/// Current Unix timestamp in seconds (via the JS `Date.now()` binding).
fn js_sys_now_secs() -> u64 {
    // In the Cloudflare Worker environment, `Date.now()` is available.
    // We use a simple fallback for non-WASM environments (tests).
    #[cfg(target_arch = "wasm32")]
    {
        (js_sys::Date::now() / 1000.0) as u64
    }
    #[cfg(not(target_arch = "wasm32"))]
    {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentPhase, AgentRole, TaskKind, TaskRuntime, TaskStatus};
    use wreck_it_core::iteration::{reset_recurring_tasks, select_next_task};

    fn make_task(id: &str, status: TaskStatus, phase: u32, deps: Vec<&str>) -> Task {
        Task {
            id: id.into(),
            description: format!("task {id}"),
            status,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase,
            depends_on: deps.into_iter().map(String::from).collect(),
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }
    }

    #[test]
    fn select_next_picks_lowest_phase_pending() {
        let tasks = vec![
            make_task("a", TaskStatus::Completed, 1, vec![]),
            make_task("b", TaskStatus::Pending, 2, vec!["a"]),
            make_task("c", TaskStatus::Pending, 3, vec![]),
        ];
        let state = HeadlessState::default();
        assert_eq!(select_next_task(&tasks, &state), Some(1));
    }

    #[test]
    fn select_next_respects_dependencies() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, 1, vec![]),
            make_task("b", TaskStatus::Pending, 1, vec!["a"]),
        ];
        let state = HeadlessState::default();
        // 'b' depends on 'a' which is not complete → only 'a' is eligible.
        assert_eq!(select_next_task(&tasks, &state), Some(0));
    }

    #[test]
    fn select_next_prefers_higher_priority() {
        let mut tasks = vec![
            make_task("a", TaskStatus::Pending, 1, vec![]),
            make_task("b", TaskStatus::Pending, 1, vec![]),
        ];
        tasks[1].priority = 10;
        let state = HeadlessState::default();
        assert_eq!(select_next_task(&tasks, &state), Some(1));
    }

    #[test]
    fn select_next_prefers_lower_complexity_at_same_priority() {
        let mut tasks = vec![
            make_task("a", TaskStatus::Pending, 1, vec![]),
            make_task("b", TaskStatus::Pending, 1, vec![]),
        ];
        tasks[0].complexity = 5;
        tasks[1].complexity = 2;
        let state = HeadlessState::default();
        assert_eq!(select_next_task(&tasks, &state), Some(1));
    }

    #[test]
    fn select_next_returns_none_when_all_complete() {
        let tasks = vec![
            make_task("a", TaskStatus::Completed, 1, vec![]),
            make_task("b", TaskStatus::Completed, 1, vec![]),
        ];
        let state = HeadlessState::default();
        assert_eq!(select_next_task(&tasks, &state), None);
    }

    #[test]
    fn reset_recurring_resets_eligible() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.last_attempt_at = Some(100);
            t
        }];
        let count = reset_recurring_tasks(&mut tasks, 200);
        assert_eq!(count, 1);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn reset_recurring_respects_cooldown() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.cooldown_seconds = Some(3600);
            t.last_attempt_at = Some(100);
            t
        }];
        let count = reset_recurring_tasks(&mut tasks, 200);
        assert_eq!(count, 0);
        assert_eq!(tasks[0].status, TaskStatus::Completed);

        let count = reset_recurring_tasks(&mut tasks, 3800);
        assert_eq!(count, 1);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn reset_recurring_skips_milestone() {
        let mut tasks = vec![make_task("a", TaskStatus::Completed, 1, vec![])];
        let count = reset_recurring_tasks(&mut tasks, 9999);
        assert_eq!(count, 0);
        assert_eq!(tasks[0].status, TaskStatus::Completed);
    }

    // ---- advance_iteration tests (shared core logic) ----

    #[test]
    fn advance_iteration_selects_task_and_updates_state() {
        let mut tasks = vec![
            make_task("a", TaskStatus::Pending, 1, vec![]),
            make_task("b", TaskStatus::Pending, 2, vec!["a"]),
        ];
        let mut state = HeadlessState::default();

        let outcome = advance_iteration(&mut tasks, &mut state, 0);
        assert_eq!(
            outcome,
            IterationOutcome::TaskStarted {
                task_id: "a".into(),
                task_description: "task a".into(),
            }
        );
        assert_eq!(tasks[0].status, TaskStatus::InProgress);
        assert_eq!(state.current_task_id, Some("a".into()));
        assert_eq!(state.iteration, 1);
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
    }

    #[test]
    fn advance_iteration_returns_all_complete() {
        let mut tasks = vec![
            make_task("a", TaskStatus::Completed, 1, vec![]),
            make_task("b", TaskStatus::Completed, 1, vec![]),
        ];
        let mut state = HeadlessState::default();
        assert_eq!(
            advance_iteration(&mut tasks, &mut state, 0),
            IterationOutcome::AllComplete
        );
    }

    #[test]
    fn advance_iteration_returns_no_pending() {
        let mut tasks = vec![
            make_task("a", TaskStatus::InProgress, 1, vec![]),
            make_task("b", TaskStatus::Pending, 1, vec!["a"]),
        ];
        let mut state = HeadlessState::default();
        assert_eq!(
            advance_iteration(&mut tasks, &mut state, 0),
            IterationOutcome::NoPendingTasks
        );
    }
}
