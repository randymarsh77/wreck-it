use crate::types::Task;
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};
use wreck_it_core::store::{StoreError, TaskStore};
use wreck_it_core::types::TaskStatus;

// Re-export from wreck-it-core so callers of `crate::task_manager::*`
// continue to work unchanged.
pub use wreck_it_core::iteration::reset_recurring_tasks;
#[allow(unused_imports)]
pub use wreck_it_core::task_manager::{
    filter_tasks_by_role, generate_task_id, get_next_task, has_circular_dependency, MAX_TASKS,
};

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

/// Append a new task to the task file, validating structure before writing.
///
/// Safeguards:
/// - The total task count (existing + new) must not exceed [`MAX_TASKS`].
/// - The new task's `id` must not already exist in the file.
/// - Adding the new task must not introduce a circular dependency.
#[cfg_attr(not(test), allow(dead_code))]
pub fn append_task(path: &Path, new_task: Task) -> Result<()> {
    let mut tasks = load_tasks(path)?;

    if tasks.len() >= MAX_TASKS {
        bail!(
            "Cannot add task '{}': task limit of {} reached",
            new_task.id,
            MAX_TASKS
        );
    }

    if tasks.iter().any(|t| t.id == new_task.id) {
        bail!("Task with id '{}' already exists", new_task.id);
    }

    if has_circular_dependency(&tasks, &new_task) {
        bail!(
            "Adding task '{}' would introduce a circular dependency",
            new_task.id
        );
    }

    tasks.push(new_task);
    save_tasks(path, &tasks)
}

/// Update the status of a single task identified by `id`.
///
/// Returns an error when no task with the given `id` exists in the file.
pub fn set_task_status(path: &Path, id: &str, status: TaskStatus) -> Result<()> {
    let mut tasks = load_tasks(path)?;
    let task = tasks
        .iter_mut()
        .find(|t| t.id == id)
        .ok_or_else(|| anyhow::anyhow!("Task '{}' not found in '{}'", id, path.display()))?;
    task.status = status;
    save_tasks(path, &tasks)
}

/// Validate a task list and return a list of human-readable issue descriptions.
///
/// Checks performed:
/// 1. Duplicate task IDs.
/// 2. `depends_on` references that point to non-existent task IDs.
/// 3. Circular dependencies (delegated to `graph::detect_cycles`).
///
/// Returns an empty `Vec` when the task list is valid.
pub fn validate_tasks(tasks: &[Task]) -> Vec<String> {
    use crate::graph::detect_cycles;
    use std::collections::HashMap;

    let mut issues = Vec::new();

    // 1. Duplicate IDs.
    let mut seen: HashMap<&str, usize> = HashMap::new();
    for t in tasks {
        let count = seen.entry(t.id.as_str()).or_insert(0);
        *count += 1;
        if *count == 2 {
            issues.push(format!("Duplicate task ID: '{}'", t.id));
        }
    }

    // 2. Unresolved depends_on references.
    let all_ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    for t in tasks {
        for dep in &t.depends_on {
            if !all_ids.contains(dep.as_str()) {
                issues.push(format!("Task '{}' depends_on unknown task '{}'", t.id, dep));
            }
        }
    }

    // 3. Circular dependencies.
    let cycles = detect_cycles(tasks);
    for cycle in &cycles {
        issues.push(format!("Circular dependency: {}", cycle.join(" -> ")));
    }

    issues
}

// ---------------------------------------------------------------------------
// FileTaskStore — file-system-backed TaskStore implementation
// ---------------------------------------------------------------------------

/// File-system-backed implementation of [`TaskStore`].
///
/// Reads and writes tasks as a JSON array in a single file.
#[allow(dead_code)]
pub struct FileTaskStore {
    path: PathBuf,
}

#[allow(dead_code)]
impl FileTaskStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }
}

impl TaskStore for FileTaskStore {
    fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
        load_tasks(&self.path).map_err(|e| StoreError::new(e.to_string()))
    }

    fn save_tasks(&self, tasks: &[Task]) -> Result<(), StoreError> {
        save_tasks(&self.path, tasks).map_err(|e| StoreError::new(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskStatus};
    use tempfile::tempdir;

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
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
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
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
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
                runtime: crate::types::TaskRuntime::default(),
                precondition_prompt: None,
                parent_id: None,
                labels: vec![],
            },
            Task {
                id: "2".to_string(),
                description: "Pending".to_string(),
                status: TaskStatus::Pending,
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
                runtime: crate::types::TaskRuntime::default(),
                precondition_prompt: None,
                parent_id: None,
                labels: vec![],
            },
        ];

        assert_eq!(get_next_task(&tasks), Some(1));
    }

    // ---- filter_tasks_by_role tests ----

    #[test]
    fn filter_by_role_returns_matching_tasks() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            {
                let mut t = make_task("b", TaskStatus::Pending, vec![]);
                t.role = AgentRole::Ideas;
                t
            },
            {
                let mut t = make_task("c", TaskStatus::Pending, vec![]);
                t.role = AgentRole::Evaluator;
                t
            },
        ];
        let implementers = filter_tasks_by_role(&tasks, AgentRole::Implementer);
        assert_eq!(implementers.len(), 1);
        assert_eq!(implementers[0].id, "a");

        let ideas = filter_tasks_by_role(&tasks, AgentRole::Ideas);
        assert_eq!(ideas.len(), 1);
        assert_eq!(ideas[0].id, "b");
    }

    #[test]
    fn filter_by_role_returns_empty_when_none_match() {
        let tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        assert!(filter_tasks_by_role(&tasks, AgentRole::Evaluator).is_empty());
    }

    // ---- generate_task_id tests ----

    #[test]
    fn generate_task_id_starts_at_one_when_no_existing() {
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
    fn cycle_detected_when_new_task_depends_on_itself() {
        let tasks = vec![];
        let mut new = make_task("a", TaskStatus::Pending, vec![]);
        new.depends_on = vec!["a".to_string()];
        assert!(has_circular_dependency(&tasks, &new));
    }

    #[test]
    fn cycle_detected_when_new_task_closes_a_cycle() {
        // a → b → c, and new task makes c → a (closes cycle a→b→c→a)
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
            make_task("c", TaskStatus::Pending, vec!["b"]),
        ];
        // Adding a new edge: a now depends on c → cycle
        let mut new_a = make_task("a", TaskStatus::Pending, vec!["c"]);
        new_a.depends_on = vec!["c".to_string()];
        // has_circular_dependency inserts new_a replacing any existing "a" edge in adj.
        assert!(has_circular_dependency(&tasks, &new_a));
    }

    #[test]
    fn no_cycle_for_dag_with_shared_dep() {
        // Diamond: a ← b, a ← c, b ← d, c ← d  (d depends on b and c)
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
            make_task("c", TaskStatus::Pending, vec!["a"]),
        ];
        let new = make_task("d", TaskStatus::Pending, vec!["b", "c"]);
        assert!(!has_circular_dependency(&tasks, &new));
    }

    // ---- append_task tests ----

    #[test]
    fn append_task_adds_task_to_file() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", TaskStatus::Pending, vec![])]).unwrap();

        let new = make_task("b", TaskStatus::Pending, vec!["a"]);
        append_task(&path, new).unwrap();

        let loaded = load_tasks(&path).unwrap();
        assert_eq!(loaded.len(), 2);
        assert_eq!(loaded[1].id, "b");
    }

    #[test]
    fn append_task_rejects_duplicate_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", TaskStatus::Pending, vec![])]).unwrap();

        let dup = make_task("a", TaskStatus::Pending, vec![]);
        let result = append_task(&path, dup);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("already exists"));
    }

    #[test]
    fn append_task_rejects_circular_dependency() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(
            &path,
            &[
                make_task("a", TaskStatus::Pending, vec![]),
                make_task("b", TaskStatus::Pending, vec!["a"]),
            ],
        )
        .unwrap();

        // New task a' depends on b, but a (same id) already depends on nothing.
        // We inject a cycle: c depends on b, and we'll try to make b depend on c.
        let circular = make_task("c", TaskStatus::Pending, vec!["b"]);
        append_task(&path, circular).unwrap(); // no cycle yet

        // Now try to add a task that closes the cycle: make a depend on c
        // (but a already exists → duplicate-id error fires first)
        // Instead rewrite as: add "d" that depends on itself.
        let mut self_dep = make_task("d", TaskStatus::Pending, vec![]);
        self_dep.depends_on = vec!["d".to_string()];
        let result = append_task(&path, self_dep);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("circular dependency"));
    }

    #[test]
    fn append_task_enforces_max_tasks_limit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");

        // Fill up to the limit.
        let tasks: Vec<Task> = (0..MAX_TASKS)
            .map(|i| make_task(&format!("t{}", i), TaskStatus::Pending, vec![]))
            .collect();
        save_tasks(&path, &tasks).unwrap();

        let extra = make_task("overflow", TaskStatus::Pending, vec![]);
        let result = append_task(&path, extra);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("task limit"));
    }

    // ---- reset_recurring_tasks tests ----

    #[test]
    fn reset_recurring_resets_completed_recurring_task() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, vec![]);
            t.kind = TaskKind::Recurring;
            t.last_attempt_at = Some(100);
            t
        }];
        let count = reset_recurring_tasks(&mut tasks, 200);
        assert_eq!(count, 1);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn reset_recurring_skips_milestone_tasks() {
        let mut tasks = vec![make_task("a", TaskStatus::Completed, vec![])];
        let count = reset_recurring_tasks(&mut tasks, 200);
        assert_eq!(count, 0);
        assert_eq!(tasks[0].status, TaskStatus::Completed);
    }

    #[test]
    fn reset_recurring_respects_cooldown() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, vec![]);
            t.kind = TaskKind::Recurring;
            t.cooldown_seconds = Some(3600);
            t.last_attempt_at = Some(100);
            t
        }];
        // Only 100 seconds have passed, cooldown is 3600.
        let count = reset_recurring_tasks(&mut tasks, 200);
        assert_eq!(count, 0);
        assert_eq!(tasks[0].status, TaskStatus::Completed);

        // Now enough time has passed.
        let count = reset_recurring_tasks(&mut tasks, 3800);
        assert_eq!(count, 1);
        assert_eq!(tasks[0].status, TaskStatus::Pending);
    }

    #[test]
    fn reset_recurring_without_cooldown_always_resets() {
        let mut tasks = vec![{
            let mut t = make_task("a", TaskStatus::Completed, vec![]);
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
                let mut t = make_task("a", TaskStatus::Pending, vec![]);
                t.kind = TaskKind::Recurring;
                t
            },
            {
                let mut t = make_task("b", TaskStatus::InProgress, vec![]);
                t.kind = TaskKind::Recurring;
                t
            },
        ];
        let count = reset_recurring_tasks(&mut tasks, 9999);
        assert_eq!(count, 0);
    }

    // ---- FileTaskStore tests ----

    #[test]
    fn file_task_store_load_save_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let store = FileTaskStore::new(&path);

        // Initially returns empty when file doesn't exist.
        let tasks = store.load_tasks().unwrap();
        assert!(tasks.is_empty());

        // Save and reload.
        let tasks = vec![make_task("a", TaskStatus::Pending, vec![])];
        store.save_tasks(&tasks).unwrap();
        let loaded = store.load_tasks().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "a");
    }

    #[test]
    fn file_task_store_overwrites_on_save() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        let store = FileTaskStore::new(&path);

        store
            .save_tasks(&[make_task("a", TaskStatus::Pending, vec![])])
            .unwrap();
        store
            .save_tasks(&[make_task("b", TaskStatus::Completed, vec![])])
            .unwrap();

        let loaded = store.load_tasks().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "b");
    }

    // ---- set_task_status tests ----

    #[test]
    fn set_task_status_updates_existing_task() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", TaskStatus::Pending, vec![])]).unwrap();

        set_task_status(&path, "a", TaskStatus::Completed).unwrap();

        let tasks = load_tasks(&path).unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Completed);
    }

    #[test]
    fn set_task_status_errors_for_unknown_id() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        save_tasks(&path, &[make_task("a", TaskStatus::Pending, vec![])]).unwrap();

        let result = set_task_status(&path, "nonexistent", TaskStatus::Completed);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("nonexistent"));
    }

    // ---- validate_tasks tests ----

    #[test]
    fn validate_tasks_valid_list_returns_no_issues() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        assert!(validate_tasks(&tasks).is_empty());
    }

    #[test]
    fn validate_tasks_detects_duplicate_ids() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec![]),
            make_task("a", TaskStatus::Completed, vec![]),
        ];
        let issues = validate_tasks(&tasks);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("Duplicate") && issues[0].contains("'a'"));
    }

    #[test]
    fn validate_tasks_detects_unresolved_depends_on() {
        let tasks = vec![make_task("a", TaskStatus::Pending, vec!["missing"])];
        let issues = validate_tasks(&tasks);
        assert_eq!(issues.len(), 1);
        assert!(issues[0].contains("missing"));
    }

    #[test]
    fn validate_tasks_detects_circular_dependency() {
        let tasks = vec![
            make_task("a", TaskStatus::Pending, vec!["b"]),
            make_task("b", TaskStatus::Pending, vec!["a"]),
        ];
        let issues = validate_tasks(&tasks);
        assert!(!issues.is_empty());
        let combined = issues.join(" ");
        assert!(combined.to_lowercase().contains("circular"));
    }

    #[test]
    fn validate_tasks_empty_list_is_valid() {
        assert!(validate_tasks(&[]).is_empty());
    }
}
