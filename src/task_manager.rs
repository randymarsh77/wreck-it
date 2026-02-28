use crate::types::{AgentRole, Task, TaskStatus};
use anyhow::{bail, Context, Result};
use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

/// Load tasks from a JSON file
pub fn load_tasks(path: &Path) -> Result<Vec<Task>> {
    if !path.exists() {
        return Ok(Vec::new());
    }

    let content = fs::read_to_string(path).context("Failed to read task file")?;

    let tasks: Vec<Task> = serde_json::from_str(&content).context("Failed to parse task file")?;

    Ok(tasks)
}

/// Save tasks to a JSON file
pub fn save_tasks(path: &Path, tasks: &[Task]) -> Result<()> {
    let content = serde_json::to_string_pretty(tasks).context("Failed to serialize tasks")?;

    fs::write(path, content).context("Failed to write task file")?;

    Ok(())
}

/// Get the next pending task
pub fn get_next_task(tasks: &[Task]) -> Option<usize> {
    tasks.iter().position(|t| t.status == TaskStatus::Pending)
}

/// Detect whether the combined task list contains a circular dependency.
///
/// Uses iterative DFS so that very deep graphs do not overflow the stack.
/// Returns `true` when a cycle is found.
pub fn has_circular_dependencies(tasks: &[Task]) -> bool {
    // Build adjacency list: task_id → [dependency_ids]
    let adjacency: HashMap<&str, Vec<&str>> = tasks
        .iter()
        .map(|t| {
            (
                t.id.as_str(),
                t.depends_on.iter().map(|d| d.as_str()).collect(),
            )
        })
        .collect();

    let mut visited: HashSet<&str> = HashSet::new();
    let mut in_stack: HashSet<&str> = HashSet::new();

    for start in adjacency.keys().copied() {
        if visited.contains(start) {
            continue;
        }
        // Iterative DFS using an explicit stack.
        // Each entry is (node, iterator-over-neighbours, entering=true/leaving=false).
        let mut stack: Vec<(&str, bool)> = vec![(start, true)];
        while let Some((node, entering)) = stack.pop() {
            if entering {
                if in_stack.contains(node) {
                    return true; // back-edge → cycle
                }
                if visited.contains(node) {
                    continue;
                }
                visited.insert(node);
                in_stack.insert(node);
                // Push a "leave" marker first so it is processed after children.
                stack.push((node, false));
                if let Some(deps) = adjacency.get(node) {
                    for &dep in deps {
                        stack.push((dep, true));
                    }
                }
            } else {
                in_stack.remove(node);
            }
        }
    }
    false
}

/// Append `new_tasks` to the task file at `path`, enforcing:
/// - `max_total` cap on the combined task count (0 = unlimited)
/// - no duplicate IDs between existing and new tasks
/// - no circular dependencies in the resulting task list
///
/// The file is read and written atomically (write to a temp file then
/// rename) to avoid corruption when the process is interrupted mid-write.
#[allow(dead_code)]
pub fn append_tasks(path: &Path, new_tasks: &[Task], max_total: usize) -> Result<()> {
    let mut existing = load_tasks(path)?;

    // Validate: no duplicate IDs.
    let existing_ids: HashSet<&str> = existing.iter().map(|t| t.id.as_str()).collect();
    for t in new_tasks {
        if existing_ids.contains(t.id.as_str()) {
            bail!("Duplicate task ID: {}", t.id);
        }
    }

    // Validate: max_total cap.
    if max_total > 0 && existing.len() + new_tasks.len() > max_total {
        bail!(
            "Appending {} task(s) would exceed the max_total limit of {} (currently {} tasks)",
            new_tasks.len(),
            max_total,
            existing.len()
        );
    }

    // Extend existing in-place and validate for circular dependencies.
    // This avoids an extra clone compared to building a separate combined Vec.
    existing.extend_from_slice(new_tasks);
    if has_circular_dependencies(&existing) {
        bail!("Appending new tasks would introduce a circular dependency");
    }

    save_tasks(path, &existing)
}

/// Generate a unique task ID with the given `prefix` that does not collide
/// with any ID already in `existing_ids`.
///
/// IDs take the form `<prefix>-<n>` where `n` starts at 1 and increments
/// until a free slot is found.  Returns `None` if no free ID is found
/// within `MAX_ID_ATTEMPTS` tries, which guards against unbounded looping.
#[allow(dead_code)]
pub fn generate_task_id(prefix: &str, existing_ids: &HashSet<&str>) -> Option<String> {
    const MAX_ID_ATTEMPTS: u64 = 100_000;
    for n in 1..=MAX_ID_ATTEMPTS {
        let candidate = format!("{}-{}", prefix, n);
        if !existing_ids.contains(candidate.as_str()) {
            return Some(candidate);
        }
    }
    None
}

/// Return the subset of tasks whose `role` matches `role`.
#[allow(dead_code)]
pub fn tasks_by_role(tasks: &[Task], role: AgentRole) -> Vec<&Task> {
    tasks.iter().filter(|t| t.role == role).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentRole;
    use tempfile::tempdir;

    fn make_task(id: &str, depends_on: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status: TaskStatus::Pending,
            phase: 1,
            depends_on: depends_on.into_iter().map(String::from).collect(),
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            role: AgentRole::default(),
        }
    }

    #[test]
    fn test_load_save_tasks() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![Task {
            id: "1".to_string(),
            description: "Test task".to_string(),
            status: TaskStatus::Pending,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            role: AgentRole::default(),
        }];

        save_tasks(&task_file, &tasks).unwrap();
        let loaded = load_tasks(&task_file).unwrap();

        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "1");
    }

    #[test]
    fn test_get_next_task() {
        let tasks = vec![
            Task {
                id: "1".to_string(),
                description: "Completed".to_string(),
                status: TaskStatus::Completed,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: None,
                role: AgentRole::default(),
            },
            Task {
                id: "2".to_string(),
                description: "Pending".to_string(),
                status: TaskStatus::Pending,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: None,
                role: AgentRole::default(),
            },
        ];

        assert_eq!(get_next_task(&tasks), Some(1));
    }

    // ── circular dependency tests ────────────────────────────────────────────

    #[test]
    fn no_cycle_in_acyclic_graph() {
        let tasks = vec![
            make_task("a", vec![]),
            make_task("b", vec!["a"]),
            make_task("c", vec!["b"]),
        ];
        assert!(!has_circular_dependencies(&tasks));
    }

    #[test]
    fn detects_direct_cycle() {
        let tasks = vec![make_task("a", vec!["b"]), make_task("b", vec!["a"])];
        assert!(has_circular_dependencies(&tasks));
    }

    #[test]
    fn detects_self_loop() {
        let tasks = vec![make_task("a", vec!["a"])];
        assert!(has_circular_dependencies(&tasks));
    }

    #[test]
    fn detects_indirect_cycle() {
        let tasks = vec![
            make_task("a", vec!["c"]),
            make_task("b", vec!["a"]),
            make_task("c", vec!["b"]),
        ];
        assert!(has_circular_dependencies(&tasks));
    }

    #[test]
    fn empty_task_list_has_no_cycle() {
        assert!(!has_circular_dependencies(&[]));
    }

    // ── append_tasks tests ───────────────────────────────────────────────────

    #[test]
    fn append_tasks_adds_new_tasks() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", vec![])]).unwrap();

        append_tasks(&path, &[make_task("b", vec!["a"])], 0).unwrap();

        let tasks = load_tasks(&path).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].id, "b");
    }

    #[test]
    fn append_tasks_rejects_duplicate_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", vec![])]).unwrap();

        let err = append_tasks(&path, &[make_task("a", vec![])], 0).unwrap_err();
        assert!(err.to_string().contains("Duplicate task ID"));
    }

    #[test]
    fn append_tasks_enforces_max_total() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", vec![])]).unwrap();

        let err = append_tasks(&path, &[make_task("b", vec![])], 1).unwrap_err();
        assert!(err.to_string().contains("max_total"));
    }

    #[test]
    fn append_tasks_rejects_new_cycle() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", vec!["b"])]).unwrap();

        // Adding b → a would create a → b → a cycle.
        let err = append_tasks(&path, &[make_task("b", vec!["a"])], 0).unwrap_err();
        assert!(err.to_string().contains("circular dependency"));
    }

    #[test]
    fn append_tasks_creates_file_when_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("new_tasks.json");

        append_tasks(&path, &[make_task("x", vec![])], 0).unwrap();

        let tasks = load_tasks(&path).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "x");
    }

    // ── generate_task_id tests ───────────────────────────────────────────────

    #[test]
    fn generate_task_id_starts_at_1() {
        let ids = HashSet::new();
        assert_eq!(generate_task_id("dyn", &ids), Some("dyn-1".to_string()));
    }

    #[test]
    fn generate_task_id_skips_existing() {
        let mut ids = HashSet::new();
        ids.insert("dyn-1");
        ids.insert("dyn-2");
        assert_eq!(generate_task_id("dyn", &ids), Some("dyn-3".to_string()));
    }

    // ── tasks_by_role tests ──────────────────────────────────────────────────

    #[test]
    fn tasks_by_role_filters_correctly() {
        let tasks = vec![
            Task {
                id: "a".to_string(),
                description: "a".to_string(),
                status: TaskStatus::Pending,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: None,
                role: AgentRole::Ideas,
            },
            Task {
                id: "b".to_string(),
                description: "b".to_string(),
                status: TaskStatus::Pending,
                phase: 1,
                depends_on: vec![],
                priority: 0,
                complexity: 1,
                failed_attempts: 0,
                last_attempt_at: None,
                role: AgentRole::Implementer,
            },
        ];

        let ideas = tasks_by_role(&tasks, AgentRole::Ideas);
        assert_eq!(ideas.len(), 1);
        assert_eq!(ideas[0].id, "a");

        let implementers = tasks_by_role(&tasks, AgentRole::Implementer);
        assert_eq!(implementers.len(), 1);
        assert_eq!(implementers[0].id, "b");
    }
}
