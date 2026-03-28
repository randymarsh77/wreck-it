//! Pure task-management logic — no I/O, no platform dependencies.
//!
//! These functions operate only on in-memory task data and are shared by both
//! the CLI (file-backed) and the worker (API-backed).

use crate::types::{AgentRole, Task, TaskStatus};
use std::collections::{HashMap, HashSet};

/// Maximum number of tasks allowed in a task list (safeguard for dynamic generation).
pub const MAX_TASKS: usize = 500;

/// Get the index of the first pending task (simple linear scan).
pub fn get_next_task(tasks: &[Task]) -> Option<usize> {
    tasks.iter().position(|t| t.status == TaskStatus::Pending)
}

/// Return references to all tasks that match the given `role`.
pub fn filter_tasks_by_role(tasks: &[Task], role: AgentRole) -> Vec<&Task> {
    tasks.iter().filter(|t| t.role == role).collect()
}

/// Generate a unique task ID by finding the largest numeric suffix among IDs
/// that share the given `prefix` and returning `<prefix><n+1>`.
///
/// Example: prefix `"dyn-"`, existing IDs `["dyn-1", "dyn-3"]` → `"dyn-4"`.
pub fn generate_task_id(existing_tasks: &[Task], prefix: &str) -> String {
    let max = existing_tasks
        .iter()
        .filter_map(|t| t.id.strip_prefix(prefix)?.parse::<u64>().ok())
        .max()
        .unwrap_or(0);
    format!("{}{}", prefix, max + 1)
}

/// Detect whether the proposed `depends_on` relationships introduce a cycle
/// when combined with the rest of the task graph.
///
/// Returns `true` if a cycle is detected.
pub fn has_circular_dependency(tasks: &[Task], new_task: &Task) -> bool {
    // Build adjacency list: task_id → list of dependency IDs.
    let mut adj: HashMap<&str, Vec<&str>> = HashMap::new();
    for t in tasks {
        adj.insert(
            t.id.as_str(),
            t.depends_on.iter().map(|s| s.as_str()).collect(),
        );
    }
    // Include the new task (it may not be in `tasks` yet).
    adj.insert(
        new_task.id.as_str(),
        new_task.depends_on.iter().map(|s| s.as_str()).collect(),
    );

    // DFS-based cycle detection.
    let mut visited: HashSet<&str> = HashSet::new();
    let mut in_stack: HashSet<&str> = HashSet::new();
    let all_ids: Vec<&str> = adj.keys().copied().collect();
    for id in all_ids {
        if !visited.contains(id) && dfs_has_cycle(id, &adj, &mut visited, &mut in_stack) {
            return true;
        }
    }
    false
}

fn dfs_has_cycle<'a>(
    node: &'a str,
    adj: &HashMap<&'a str, Vec<&'a str>>,
    visited: &mut HashSet<&'a str>,
    in_stack: &mut HashSet<&'a str>,
) -> bool {
    visited.insert(node);
    in_stack.insert(node);

    if let Some(deps) = adj.get(node) {
        for &dep in deps {
            if !visited.contains(dep) {
                if dfs_has_cycle(dep, adj, visited, in_stack) {
                    return true;
                }
            } else if in_stack.contains(dep) {
                return true;
            }
        }
    }

    in_stack.remove(node);
    false
}

/// Normalise a task description for redundancy comparison.
///
/// Trims leading/trailing whitespace, collapses internal whitespace runs to a
/// single space, and lowercases the result.
pub fn normalize_description(desc: &str) -> String {
    desc.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

/// Identify indices of redundant **pending** tasks.
///
/// Two pending tasks are considered redundant when they share the same
/// normalised `(description, role)` pair.  For each group of duplicates the
/// first occurrence (by index) is kept; the remaining indices are returned so
/// the caller can mark them as completed and close associated issues.
pub fn find_redundant_pending_tasks(tasks: &[Task]) -> Vec<usize> {
    let mut seen: HashMap<(String, AgentRole), usize> = HashMap::new();
    let mut duplicates: Vec<usize> = Vec::new();

    for (idx, task) in tasks.iter().enumerate() {
        if task.status != TaskStatus::Pending {
            continue;
        }

        let key = (normalize_description(&task.description), task.role);
        match seen.entry(key) {
            std::collections::hash_map::Entry::Occupied(_) => {
                duplicates.push(idx);
            }
            std::collections::hash_map::Entry::Vacant(e) => {
                e.insert(idx);
            }
        }
    }

    duplicates
}

/// Validate and append a new task to an in-memory task list.
///
/// Safeguards:
/// - The total task count (existing + new) must not exceed [`MAX_TASKS`].
/// - The new task's `id` must not already exist.
/// - Adding the new task must not introduce a circular dependency.
///
/// On success, pushes the task and returns `Ok(())`.
/// On failure, returns `Err` with a human-readable message.
pub fn validate_and_append_task(tasks: &mut Vec<Task>, new_task: Task) -> Result<(), String> {
    if tasks.len() >= MAX_TASKS {
        return Err(format!(
            "Cannot add task '{}': task limit of {} reached",
            new_task.id, MAX_TASKS
        ));
    }

    if tasks.iter().any(|t| t.id == new_task.id) {
        return Err(format!("Task with id '{}' already exists", new_task.id));
    }

    if has_circular_dependency(tasks, &new_task) {
        return Err(format!(
            "Adding task '{}' would introduce a circular dependency",
            new_task.id
        ));
    }

    tasks.push(new_task);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TaskKind, TaskRuntime};

    fn make_task(id: &str, status: TaskStatus, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role: AgentRole::default(),
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

    // ---- get_next_task tests ----

    #[test]
    fn get_next_task_finds_first_pending() {
        let tasks = vec![
            make_task("1", TaskStatus::Completed, vec![]),
            make_task("2", TaskStatus::Pending, vec![]),
        ];
        assert_eq!(get_next_task(&tasks), Some(1));
    }

    #[test]
    fn get_next_task_returns_none_when_all_complete() {
        let tasks = vec![make_task("1", TaskStatus::Completed, vec![])];
        assert_eq!(get_next_task(&tasks), None);
    }

    // ---- filter_tasks_by_role tests ----

    #[test]
    fn filter_by_role_returns_matching() {
        let tasks = vec![make_task("a", TaskStatus::Pending, vec![]), {
            let mut t = make_task("b", TaskStatus::Pending, vec![]);
            t.role = AgentRole::Ideas;
            t
        }];
        let implementers = filter_tasks_by_role(&tasks, AgentRole::Implementer);
        assert_eq!(implementers.len(), 1);
        assert_eq!(implementers[0].id, "a");
    }

    #[test]
    fn filter_by_role_returns_empty_when_none_match() {
        let tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        assert!(filter_tasks_by_role(&tasks, AgentRole::Evaluator).is_empty());
    }

    // ---- generate_task_id tests ----

    #[test]
    fn generate_task_id_starts_at_one() {
        assert_eq!(generate_task_id(&[], "dyn-"), "dyn-1");
    }

    #[test]
    fn generate_task_id_increments_past_max() {
        let tasks = vec![
            make_task("dyn-1", TaskStatus::Pending, vec![]),
            make_task("dyn-3", TaskStatus::Pending, vec![]),
        ];
        assert_eq!(generate_task_id(&tasks, "dyn-"), "dyn-4");
    }

    #[test]
    fn generate_task_id_ignores_other_prefixes() {
        let tasks = vec![make_task("other-9", TaskStatus::Pending, vec![])];
        assert_eq!(generate_task_id(&tasks, "dyn-"), "dyn-1");
    }

    // ---- has_circular_dependency tests ----

    #[test]
    fn no_cycle_when_linear_chain() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        let new = make_task("c", TaskStatus::Pending, vec!["b"]);
        assert!(!has_circular_dependency(&tasks, &new));
    }

    #[test]
    fn cycle_detected_self_dependency() {
        let tasks = vec![];
        let mut new = make_task("a", TaskStatus::Pending, vec![]);
        new.depends_on = vec!["a".to_string()];
        assert!(has_circular_dependency(&tasks, &new));
    }

    #[test]
    fn cycle_detected_closing_cycle() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
            make_task("c", TaskStatus::Pending, vec!["b"]),
        ];
        let mut new_a = make_task("a", TaskStatus::Pending, vec!["c"]);
        new_a.depends_on = vec!["c".to_string()];
        assert!(has_circular_dependency(&tasks, &new_a));
    }

    #[test]
    fn no_cycle_for_dag_with_shared_dep() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
            make_task("c", TaskStatus::Pending, vec!["a"]),
        ];
        let new = make_task("d", TaskStatus::Pending, vec!["b", "c"]);
        assert!(!has_circular_dependency(&tasks, &new));
    }

    // ---- validate_and_append_task tests ----

    #[test]
    fn validate_append_success() {
        let mut tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        let new = make_task("b", TaskStatus::Pending, vec!["a"]);
        assert!(validate_and_append_task(&mut tasks, new).is_ok());
        assert_eq!(tasks.len(), 2);
    }

    #[test]
    fn validate_append_rejects_duplicate() {
        let mut tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        let dup = make_task("a", TaskStatus::Pending, vec![]);
        let result = validate_and_append_task(&mut tasks, dup);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("already exists"));
    }

    #[test]
    fn validate_append_rejects_cycle() {
        let mut tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        let mut self_dep = make_task("b", TaskStatus::Pending, vec![]);
        self_dep.depends_on = vec!["b".to_string()];
        let result = validate_and_append_task(&mut tasks, self_dep);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("circular dependency"));
    }

    #[test]
    fn validate_append_enforces_max_tasks() {
        let mut tasks: Vec<Task> = (0..MAX_TASKS)
            .map(|i| make_task(&format!("t{}", i), TaskStatus::Pending, vec![]))
            .collect();
        let extra = make_task("overflow", TaskStatus::Pending, vec![]);
        let result = validate_and_append_task(&mut tasks, extra);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("task limit"));
    }

    // ---- normalize_description tests ----

    #[test]
    fn normalize_description_trims_and_lowercases() {
        assert_eq!(normalize_description("  Hello WORLD  "), "hello world");
    }

    #[test]
    fn normalize_description_collapses_whitespace() {
        assert_eq!(
            normalize_description("  multiple   spaces\ttab\nnewline  "),
            "multiple spaces tab newline"
        );
    }

    #[test]
    fn normalize_description_empty_string() {
        assert_eq!(normalize_description(""), "");
    }

    // ---- find_redundant_pending_tasks tests ----

    #[test]
    fn find_redundant_no_duplicates() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec![]),
        ];
        assert!(find_redundant_pending_tasks(&tasks).is_empty());
    }

    #[test]
    fn find_redundant_detects_exact_duplicate_descriptions() {
        let mut t1 = make_task("a", TaskStatus::Pending, vec![]);
        t1.description = "implement caching".to_string();
        let mut t2 = make_task("b", TaskStatus::Pending, vec![]);
        t2.description = "implement caching".to_string();
        let tasks = vec![t1, t2];

        let dupes = find_redundant_pending_tasks(&tasks);
        assert_eq!(dupes, vec![1]); // keeps index 0, marks index 1
    }

    #[test]
    fn find_redundant_normalises_before_comparing() {
        let mut t1 = make_task("a", TaskStatus::Pending, vec![]);
        t1.description = "  Implement  Caching ".to_string();
        let mut t2 = make_task("b", TaskStatus::Pending, vec![]);
        t2.description = "implement caching".to_string();
        let tasks = vec![t1, t2];

        let dupes = find_redundant_pending_tasks(&tasks);
        assert_eq!(dupes, vec![1]);
    }

    #[test]
    fn find_redundant_ignores_non_pending_tasks() {
        let mut t1 = make_task("a", TaskStatus::Completed, vec![]);
        t1.description = "same description".to_string();
        let mut t2 = make_task("b", TaskStatus::Pending, vec![]);
        t2.description = "same description".to_string();
        let mut t3 = make_task("c", TaskStatus::Pending, vec![]);
        t3.description = "same description".to_string();
        let tasks = vec![t1, t2, t3];

        // t1 is Completed so ignored; t2 is kept; t3 is the duplicate
        let dupes = find_redundant_pending_tasks(&tasks);
        assert_eq!(dupes, vec![2]);
    }

    #[test]
    fn find_redundant_different_roles_not_redundant() {
        let mut t1 = make_task("a", TaskStatus::Pending, vec![]);
        t1.description = "implement caching".to_string();
        t1.role = AgentRole::Implementer;
        let mut t2 = make_task("b", TaskStatus::Pending, vec![]);
        t2.description = "implement caching".to_string();
        t2.role = AgentRole::Evaluator;
        let tasks = vec![t1, t2];

        assert!(find_redundant_pending_tasks(&tasks).is_empty());
    }

    #[test]
    fn find_redundant_multiple_groups() {
        let mut tasks = Vec::new();
        for (id, desc) in [
            ("a", "foo"),
            ("b", "bar"),
            ("c", "foo"),
            ("d", "bar"),
            ("e", "baz"),
        ] {
            let mut t = make_task(id, TaskStatus::Pending, vec![]);
            t.description = desc.to_string();
            tasks.push(t);
        }

        let mut dupes = find_redundant_pending_tasks(&tasks);
        dupes.sort();
        // "foo" group: keeps 0, marks 2; "bar" group: keeps 1, marks 3
        assert_eq!(dupes, vec![2, 3]);
    }
}
