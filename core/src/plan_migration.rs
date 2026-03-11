//! Plan migration logic — merging pending plans from the main branch into
//! the state branch task list.
//!
//! These functions contain no I/O and are shared by the CLI and worker.

use crate::types::{Task, TaskStatus};
use std::collections::HashSet;

/// Merge pending tasks from a plan file into an existing task list.
///
/// Rules:
/// - Tasks whose ID already exists in `existing` are **replaced** if they are
///   not yet `Completed` (allows replanning to update descriptions, deps, etc.).
/// - Tasks whose ID already exists and are `Completed` are left untouched.
/// - Truly new tasks (ID not in `existing`) are appended.
///
/// Returns the number of tasks that were added or updated.
pub fn merge_pending_tasks(existing: &mut Vec<Task>, pending: &[Task]) -> usize {
    let existing_ids: HashSet<String> = existing.iter().map(|t| t.id.clone()).collect();
    let mut changed = 0;

    for task in pending {
        if existing_ids.contains(&task.id) {
            // Update in-place if the existing task is not completed.
            if let Some(slot) = existing.iter_mut().find(|t| t.id == task.id) {
                if slot.status != TaskStatus::Completed {
                    *slot = task.clone();
                    changed += 1;
                }
            }
        } else {
            existing.push(task.clone());
            changed += 1;
        }
    }

    changed
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskRuntime};

    fn make_task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
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
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
        }
    }

    #[test]
    fn merge_appends_new_tasks() {
        let mut existing = vec![make_task("a", TaskStatus::Completed)];
        let pending = vec![make_task("b", TaskStatus::Pending)];
        let count = merge_pending_tasks(&mut existing, &pending);
        assert_eq!(count, 1);
        assert_eq!(existing.len(), 2);
        assert_eq!(existing[1].id, "b");
    }

    #[test]
    fn merge_skips_duplicate_completed() {
        let mut existing = vec![make_task("a", TaskStatus::Completed)];
        let mut replacement = make_task("a", TaskStatus::Pending);
        replacement.description = "revised a".to_string();
        let count = merge_pending_tasks(&mut existing, &[replacement]);
        assert_eq!(count, 0);
        // Original description preserved.
        assert_eq!(existing[0].description, "task a");
        assert_eq!(existing[0].status, TaskStatus::Completed);
    }

    #[test]
    fn merge_replaces_non_completed() {
        let mut existing = vec![make_task("a", TaskStatus::Pending)];
        let mut replacement = make_task("a", TaskStatus::Pending);
        replacement.description = "revised a".to_string();
        let count = merge_pending_tasks(&mut existing, &[replacement]);
        assert_eq!(count, 1);
        assert_eq!(existing[0].description, "revised a");
    }

    #[test]
    fn merge_handles_empty_pending() {
        let mut existing = vec![make_task("a", TaskStatus::Pending)];
        let count = merge_pending_tasks(&mut existing, &[]);
        assert_eq!(count, 0);
        assert_eq!(existing.len(), 1);
    }

    #[test]
    fn merge_handles_empty_existing() {
        let mut existing: Vec<Task> = vec![];
        let pending = vec![make_task("a", TaskStatus::Pending)];
        let count = merge_pending_tasks(&mut existing, &pending);
        assert_eq!(count, 1);
        assert_eq!(existing.len(), 1);
    }

    #[test]
    fn merge_mixed_new_and_existing() {
        let mut existing = vec![
            make_task("a", TaskStatus::Completed),
            make_task("b", TaskStatus::Failed),
        ];
        let mut updated_b = make_task("b", TaskStatus::Pending);
        updated_b.description = "revised b".to_string();
        let new_c = make_task("c", TaskStatus::Pending);
        let count = merge_pending_tasks(&mut existing, &[updated_b, new_c]);
        assert_eq!(count, 2);
        assert_eq!(existing.len(), 3);
        // a is untouched (completed).
        assert_eq!(existing[0].description, "task a");
        // b is replaced (was failed, not completed).
        assert_eq!(existing[1].description, "revised b");
        // c is new.
        assert_eq!(existing[2].id, "c");
    }
}
