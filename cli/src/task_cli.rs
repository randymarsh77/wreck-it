//! Helper functions for the `wreck-it tasks` sub-command family.
//!
//! Encapsulates the filtering and row-formatting logic so that it can be
//! exercised by unit tests independently of the `main` binary entry-point.

use crate::types::{AgentRole, Task, TaskStatus};

/// Return a filtered view of `tasks` matching the optional `status`.
///
/// When `status` is `None` every task is included.  When it is `Some(s)` only
/// tasks whose `status` field equals `s` are returned.
pub fn filter_tasks_by_status(tasks: &[Task], status: Option<TaskStatus>) -> Vec<&Task> {
    tasks
        .iter()
        .filter(|t| status.is_none_or(|s| t.status == s))
        .collect()
}

/// Format a single task as a human-readable table row.
///
/// `id_w`, `status_w`, and `role_w` control the minimum column widths for the
/// ID, STATUS, and ROLE columns respectively so that all rows align with the
/// header line produced by the `tasks list` sub-command.
pub fn format_task_row(task: &Task, id_w: usize, status_w: usize, role_w: usize) -> String {
    let status_str = match task.status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in-progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
    };
    let role_str = match task.role {
        AgentRole::Ideas => "ideas",
        AgentRole::Implementer => "implementer",
        AgentRole::Evaluator => "evaluator",
        AgentRole::SecurityGate => "security_gate",
        AgentRole::CoverageEnforcer => "coverage_enforcer",
    };
    let deps = if task.depends_on.is_empty() {
        "-".to_string()
    } else {
        task.depends_on.join(",")
    };
    format!(
        "{:<id_w$}  {:<status_w$}  {:<role_w$}  {:>5}  {:>8}  {}",
        task.id,
        status_str,
        role_str,
        task.phase,
        task.priority,
        deps,
        id_w = id_w,
        status_w = status_w,
        role_w = role_w,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::task_manager;
    use crate::types::{AgentRole, TaskKind, TaskRuntime};
    use tempfile::tempdir;

    fn make_task(id: &str, status: TaskStatus, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {id}"),
            status,
            role: AgentRole::Implementer,
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: depends_on.into_iter().map(String::from).collect(),
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

    // ── (1) list: filter by status ────────────────────────────────────────────

    #[test]
    fn list_no_filter_returns_all_tasks() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Completed, vec![]),
            make_task("c", TaskStatus::InProgress, vec![]),
        ];
        let result = filter_tasks_by_status(&tasks, None);
        assert_eq!(result.len(), 3);
    }

    #[test]
    fn list_filter_by_pending_returns_only_pending() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Completed, vec![]),
            make_task("c", TaskStatus::InProgress, vec![]),
        ];
        let result = filter_tasks_by_status(&tasks, Some(TaskStatus::Pending));
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, "a");
    }

    #[test]
    fn list_filter_by_completed_excludes_other_statuses() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Completed, vec![]),
            make_task("c", TaskStatus::Completed, vec![]),
            make_task("d", TaskStatus::Failed, vec![]),
        ];
        let result = filter_tasks_by_status(&tasks, Some(TaskStatus::Completed));
        assert_eq!(result.len(), 2);
        assert!(result.iter().all(|t| t.status == TaskStatus::Completed));
    }

    #[test]
    fn list_filter_returns_empty_when_no_match() {
        let tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        let result = filter_tasks_by_status(&tasks, Some(TaskStatus::Failed));
        assert!(result.is_empty());
    }

    // ── (1) list: row formatting ──────────────────────────────────────────────

    #[test]
    fn format_row_contains_task_id_and_status() {
        let task = make_task("my-task", TaskStatus::InProgress, vec![]);
        let row = format_task_row(&task, 7, 11, 11);
        assert!(row.contains("my-task"), "row missing task id: {row}");
        assert!(row.contains("in-progress"), "row missing status: {row}");
    }

    #[test]
    fn format_row_shows_dash_when_no_deps() {
        let task = make_task("t", TaskStatus::Pending, vec![]);
        let row = format_task_row(&task, 2, 11, 11);
        // The last column should be a single dash when there are no dependencies.
        assert!(row.ends_with('-'), "expected row to end with '-': {row}");
    }

    #[test]
    fn format_row_lists_depends_on_comma_separated() {
        let task = make_task("c", TaskStatus::Pending, vec!["a", "b"]);
        let row = format_task_row(&task, 2, 11, 11);
        assert!(row.contains("a,b"), "expected 'a,b' in row: {row}");
    }

    // ── (2) add: append to JSON and file parses correctly ────────────────────

    #[test]
    fn add_appends_new_task_and_file_parses_correctly() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");

        // Start with a single existing task.
        task_manager::save_tasks(&path, &[make_task("existing", TaskStatus::Pending, vec![])])
            .unwrap();

        let new_task = Task {
            id: "new-task".to_string(),
            description: "A freshly added task".to_string(),
            status: TaskStatus::Pending,
            role: AgentRole::Implementer,
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 2,
            depends_on: vec!["existing".to_string()],
            priority: 5,
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
        };
        task_manager::append_task(&path, new_task).unwrap();

        // The resulting file must parse back to exactly two tasks.
        let loaded = task_manager::load_tasks(&path).unwrap();
        assert_eq!(loaded.len(), 2, "expected 2 tasks after append");
        let added = loaded
            .iter()
            .find(|t| t.id == "new-task")
            .expect("new task not found");
        assert_eq!(added.description, "A freshly added task");
        assert_eq!(added.phase, 2);
        assert_eq!(added.priority, 5);
        assert_eq!(added.depends_on, vec!["existing".to_string()]);
    }

    // ── (3) set-status: updates correct task, leaves others unchanged ─────────

    #[test]
    fn set_status_updates_target_and_leaves_others_unchanged() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");

        task_manager::save_tasks(
            &path,
            &[
                make_task("alpha", TaskStatus::Pending, vec![]),
                make_task("beta", TaskStatus::Pending, vec![]),
                make_task("gamma", TaskStatus::InProgress, vec![]),
            ],
        )
        .unwrap();

        task_manager::set_task_status(&path, "beta", TaskStatus::Completed).unwrap();

        let tasks = task_manager::load_tasks(&path).unwrap();
        assert_eq!(tasks.len(), 3);
        let alpha = tasks.iter().find(|t| t.id == "alpha").unwrap();
        let beta = tasks.iter().find(|t| t.id == "beta").unwrap();
        let gamma = tasks.iter().find(|t| t.id == "gamma").unwrap();
        assert_eq!(
            alpha.status,
            TaskStatus::Pending,
            "alpha should be unchanged"
        );
        assert_eq!(beta.status, TaskStatus::Completed, "beta should be updated");
        assert_eq!(
            gamma.status,
            TaskStatus::InProgress,
            "gamma should be unchanged"
        );
    }

    // ── (4) validate: duplicate IDs and nonexistent depends_on ───────────────

    #[test]
    fn validate_reports_duplicate_task_ids() {
        let tasks = vec![
            make_task("dup", TaskStatus::Pending, vec![]),
            make_task("dup", TaskStatus::Completed, vec![]),
        ];
        let issues = task_manager::validate_tasks(&tasks);
        assert!(!issues.is_empty(), "expected at least one issue");
        assert!(
            issues
                .iter()
                .any(|i| i.contains("Duplicate") && i.contains("dup")),
            "expected a duplicate-ID issue: {issues:?}"
        );
    }

    #[test]
    fn validate_reports_nonexistent_depends_on() {
        let tasks = vec![make_task("a", TaskStatus::Pending, vec!["ghost"])];
        let issues = task_manager::validate_tasks(&tasks);
        assert!(!issues.is_empty(), "expected at least one issue");
        assert!(
            issues.iter().any(|i| i.contains("ghost")),
            "expected issue mentioning missing dependency: {issues:?}"
        );
    }

    // ── (5) validate: clean result for well-formed task file ─────────────────

    #[test]
    fn validate_returns_no_issues_for_well_formed_tasks() {
        let tasks = vec![
            make_task("first", TaskStatus::Pending, vec![]),
            make_task("second", TaskStatus::Pending, vec!["first"]),
            make_task("third", TaskStatus::Completed, vec!["first", "second"]),
        ];
        let issues = task_manager::validate_tasks(&tasks);
        assert!(
            issues.is_empty(),
            "expected no issues for valid task file but got: {issues:?}"
        );
    }
}
