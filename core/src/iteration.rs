//! Shared iteration logic — task selection and recurring-task resets.
//!
//! This module contains the pure business logic that both the CLI headless
//! runner and the Cloudflare Worker use when processing an iteration.

use crate::state::HeadlessState;
use crate::types::{Task, TaskKind, TaskStatus};
use std::collections::HashSet;

/// Select the next task to execute.
///
/// Uses the standard wreck-it scheduling heuristics:
/// 1. Find the lowest phase that has pending tasks.
/// 2. Within that phase, filter to tasks whose dependencies are all completed.
/// 3. Sort candidates: higher priority first, then lower complexity first.
/// 4. Return the index of the best candidate.
pub fn select_next_task(tasks: &[Task], _state: &HeadlessState) -> Option<usize> {
    let completed_ids: HashSet<&str> = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .map(|t| t.id.as_str())
        .collect();

    // Find the lowest phase that has pending tasks.
    let min_phase = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .map(|t| t.phase)
        .min()?;

    // Collect candidates in that phase whose dependencies are satisfied.
    let mut candidates: Vec<(usize, &Task)> = tasks
        .iter()
        .enumerate()
        .filter(|(_, t)| {
            t.status == TaskStatus::Pending
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
/// - Tasks with `kind == Recurring` and `status == Completed` are reset
///   to `Pending` when either no `cooldown_seconds` is set, or enough
///   time has passed since `last_attempt_at`.
///
/// Returns the number of tasks that were reset.
pub fn reset_recurring_tasks(tasks: &mut [Task], now_secs: u64) -> usize {
    let mut count = 0;
    for task in tasks.iter_mut() {
        if task.kind != TaskKind::Recurring || task.status != TaskStatus::Completed {
            continue;
        }
        let ready = match (task.cooldown_seconds, task.last_attempt_at) {
            (Some(cd), Some(last)) => now_secs.saturating_sub(last) >= cd,
            // No cooldown → always eligible; no last_attempt → first run.
            _ => true,
        };
        if ready {
            task.status = TaskStatus::Pending;
            count += 1;
        }
    }
    count
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

    #[test]
    fn reset_recurring_without_cooldown_always_resets() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, 1, vec![]);
            t.kind = TaskKind::Recurring;
            t.last_attempt_at = Some(100);
            t
        }];
        let count = reset_recurring_tasks(&mut tasks, 101);
        assert_eq!(count, 1);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
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
        let count = reset_recurring_tasks(&mut tasks, 9999);
        assert_eq!(count, 0);
    }
}
