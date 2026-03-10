//! Shared iteration logic — task selection and recurring-task resets.
//!
//! This module contains the pure business logic that both the CLI headless
//! runner and the Cloudflare Worker use when processing an iteration.
//!
//! ## Status resolution
//!
//! Task status can come from two sources:
//!
//! 1. **`HeadlessState::task_statuses`** — the authoritative runtime map,
//!    keyed by task ID.  This is the preferred source when the state file is
//!    available.
//! 2. **`Task::status`** — the value embedded in the task definition file.
//!    Used as a fallback when the task ID is absent from the state map (e.g.
//!    for newly added tasks).
//!
//! All mutation functions in this module write to the state map so that task
//! definition files remain stateless.

use crate::state::{AgentPhase, HeadlessState};
use crate::types::{Task, TaskKind, TaskStatus};
use std::collections::HashSet;

/// Resolve the effective status for a task.
///
/// Returns the status from `HeadlessState::task_statuses` if present,
/// otherwise falls back to the `status` field on the task itself.
pub fn effective_status(task: &Task, state: &HeadlessState) -> TaskStatus {
    state
        .task_statuses
        .get(&task.id)
        .copied()
        .unwrap_or(task.status)
}

/// Select the next task to execute.
///
/// Uses the standard wreck-it scheduling heuristics:
/// 1. Find the lowest phase that has pending tasks.
/// 2. Within that phase, filter to tasks whose dependencies are all completed.
/// 3. Sort candidates: higher priority first, then lower complexity first.
/// 4. Return the index of the best candidate.
pub fn select_next_task(tasks: &[Task], state: &HeadlessState) -> Option<usize> {
    let completed_ids: HashSet<&str> = tasks
        .iter()
        .filter(|t| effective_status(t, state) == TaskStatus::Completed)
        .map(|t| t.id.as_str())
        .collect();

    // Find the lowest phase that has pending tasks.
    let min_phase = tasks
        .iter()
        .filter(|t| effective_status(t, state) == TaskStatus::Pending)
        .map(|t| t.phase)
        .min()?;

    // Collect candidates in that phase whose dependencies are satisfied.
    let mut candidates: Vec<(usize, &Task)> = tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            effective_status(t, state) == TaskStatus::Pending
                && t.phase == min_phase
                && t.depends_on
                    .iter()
                    .all(|dep| completed_ids.contains(dep.as_str()))
        })
        .collect();

    // Sort: higher priority first, then lower complexity first.
    candidates.sort_by(|a, b| {
        b.1.priority
            .cmp(&a.1.priority)
            .then(a.1.complexity.cmp(&b.1.complexity))
    });

    candidates.first().map(|(idx, _)| *idx)
}

/// Reset completed recurring tasks whose cooldown has elapsed back to
/// `Pending` so that the scheduler picks them up again.
///
/// - Tasks with `kind == Milestone` (the default) are never touched.
/// - Tasks with `kind == Recurring` and effective status `Completed` are reset
///   to `Pending` when either no `cooldown_seconds` is set, or enough
///   time has passed since `last_attempt_at`.
///
/// Status changes are written to `state.task_statuses` so the task definition
/// files remain stateless.
///
/// Returns the number of tasks that were reset.
pub fn reset_recurring_tasks(tasks: &mut [Task], state: &mut HeadlessState, now_secs: u64) -> usize {
    let mut count = 0;
    for task in tasks.iter_mut() {
        if task.kind != TaskKind::Recurring
            || effective_status(task, state) != TaskStatus::Completed
        {
            continue;
        }
        let ready = match (task.cooldown_seconds, task.last_attempt_at) {
            (Some(cd), Some(last_at)) => now_secs.saturating_sub(last_at) >= cd,
            // Cooldown set but no timestamp → treat as not ready yet.
            (Some(_), None) => false,
            // No cooldown → always eligible.
            _ => true,
        };
        if ready {
            state
                .task_statuses
                .insert(task.id.clone(), TaskStatus::Pending);
            count += 1;
        }
    }
    count
}

// ---------------------------------------------------------------------------
// Shared iteration step
// ---------------------------------------------------------------------------

/// Outcome of a single iteration advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IterationOutcome {
    /// All tasks are already complete — nothing to do.
    AllComplete,
    /// No eligible pending tasks (dependencies not met, etc.).
    NoPendingTasks,
    /// A task was selected and marked in-progress.
    TaskStarted {
        task_id: String,
        task_description: String,
    },
}

/// Advance one iteration of the task loop.
///
/// This is the canonical shared logic used by both the CLI headless runner and
/// the Cloudflare Worker.  It performs the following steps:
///
/// 1. Reset completed recurring tasks whose cooldown has elapsed.
/// 2. Check whether all tasks are complete.
/// 3. Select the next eligible pending task.
/// 4. Mark the selected task as `InProgress` and update `state`.
///
/// Status changes are recorded in `state.task_statuses` so that task
/// definition files remain stateless.  The caller is responsible for loading
/// tasks and state beforehand and persisting them afterward (via file I/O,
/// API calls, etc.).
pub fn advance_iteration(
    tasks: &mut [Task],
    state: &mut HeadlessState,
    now_secs: u64,
) -> IterationOutcome {
    // Step 1: Reset completed recurring tasks.
    reset_recurring_tasks(tasks, state, now_secs);

    // Step 2: Check if all tasks are complete.
    let all_done = !tasks.is_empty()
        && tasks
            .iter()
            .all(|t| effective_status(t, state) == TaskStatus::Completed);
    if all_done {
        return IterationOutcome::AllComplete;
    }

    // Step 3: Select the next pending task.
    let next_idx = match select_next_task(tasks, state) {
        Some(idx) => idx,
        None => return IterationOutcome::NoPendingTasks,
    };

    // Step 4: Advance state.
    let task_id = tasks[next_idx].id.clone();
    let task_desc = tasks[next_idx].description.clone();
    state
        .task_statuses
        .insert(task_id.clone(), TaskStatus::InProgress);
    state.phase = AgentPhase::NeedsTrigger;
    state.current_task_id = Some(task_id.clone());
    state.iteration += 1;

    IterationOutcome::TaskStarted {
        task_id,
        task_description: task_desc,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskRuntime};

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
            timeout_seconds: None,
            max_retries: None,
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

    // ---- select_next_task tests ----

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

    // ---- reset_recurring_tasks tests ----

    #[test]
    fn reset_recurring_resets_eligible() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.last_attempt_at = Some(100);
            t
        }];
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("a".into(), TaskStatus::Completed);
        let count = reset_recurring_tasks(&mut tasks, &mut state, 200);
        assert_eq!(count, 1);
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::Pending);
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
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("a".into(), TaskStatus::Completed);
        let count = reset_recurring_tasks(&mut tasks, &mut state, 200);
        assert_eq!(count, 0);
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::Completed);

        let count = reset_recurring_tasks(&mut tasks, &mut state, 3800);
        assert_eq!(count, 1);
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::Pending);
    }

    #[test]
    fn reset_recurring_skips_milestone() {
        let mut tasks = vec![make_task("a", TaskStatus::Completed, 1, vec![])];
        let mut state = HeadlessState::default();
        let count = reset_recurring_tasks(&mut tasks, &mut state, 9999);
        assert_eq!(count, 0);
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::Completed);
    }

    #[test]
    fn reset_recurring_without_cooldown_always_resets() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.last_attempt_at = Some(100);
            t
        }];
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("a".into(), TaskStatus::Completed);
        let count = reset_recurring_tasks(&mut tasks, &mut state, 101);
        assert_eq!(count, 1);
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::Pending);
    }

    #[test]
    fn reset_recurring_skips_pending_and_in_progress() {
        let mut tasks = vec![
            {
                let mut t = make_task("a", TaskStatus::Pending, 1, vec![]);
                t.kind = TaskKind::Recurring;
                t
            },
            {
                let mut t = make_task("b", TaskStatus::InProgress, 1, vec![]);
                t.kind = TaskKind::Recurring;
                t
            },
        ];
        let mut state = HeadlessState::default();
        let count = reset_recurring_tasks(&mut tasks, &mut state, 9999);
        assert_eq!(count, 0);
    }

    #[test]
    fn reset_recurring_with_cooldown_but_no_last_attempt_does_not_reset() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.cooldown_seconds = Some(3600);
            // last_attempt_at is None — cooldown should still block reset.
            t
        }];
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("a".into(), TaskStatus::Completed);
        let count = reset_recurring_tasks(&mut tasks, &mut state, 9999);
        assert_eq!(count, 0);
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::Completed);
    }

    // ---- advance_iteration tests ----

    #[test]
    fn advance_selects_and_marks_in_progress() {
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
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::InProgress);
        assert_eq!(state.current_task_id, Some("a".into()));
        assert_eq!(state.iteration, 1);
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
    }

    #[test]
    fn advance_returns_all_complete() {
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
    fn advance_returns_no_pending_when_deps_unmet() {
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

    #[test]
    fn advance_resets_recurring_before_selecting() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.last_attempt_at = Some(100);
            t
        }];
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("a".into(), TaskStatus::Completed);
        let outcome = advance_iteration(&mut tasks, &mut state, 200);
        // The recurring task should have been reset to Pending, then selected.
        assert_eq!(
            outcome,
            IterationOutcome::TaskStarted {
                task_id: "a".into(),
                task_description: "task a".into(),
            }
        );
        assert_eq!(effective_status(&tasks[0], &state), TaskStatus::InProgress);
    }

    // ---- effective_status tests ----

    #[test]
    fn effective_status_uses_state_map_when_present() {
        let task = make_task("a", TaskStatus::Pending, 1, vec![]);
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("a".into(), TaskStatus::Completed);
        assert_eq!(effective_status(&task, &state), TaskStatus::Completed);
    }

    #[test]
    fn effective_status_falls_back_to_task_field() {
        let task = make_task("a", TaskStatus::Pending, 1, vec![]);
        let state = HeadlessState::default();
        assert_eq!(effective_status(&task, &state), TaskStatus::Pending);
    }

    #[test]
    fn state_map_overrides_task_status() {
        let task = make_task("x", TaskStatus::Completed, 1, vec![]);
        let mut state = HeadlessState::default();
        state
            .task_statuses
            .insert("x".into(), TaskStatus::InProgress);
        assert_eq!(effective_status(&task, &state), TaskStatus::InProgress);
    }
}
