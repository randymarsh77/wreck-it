use crate::task_manager;
use crate::types::{AgentRole, Task, TaskKind, TaskRuntime, TaskStatus};
use anyhow::{bail, Result};
use std::path::{Path, PathBuf};

/// High-level project management API for epics and sub-tasks.
///
/// All operations read from and write to a JSON task file on disk.
/// When a `work_dir` is configured the operations can be paired with the
/// git-backed state worktree so that every mutation is committed.
pub struct ProjectManager {
    task_file: PathBuf,
}

/// Subset of task fields that can be updated.
#[derive(Debug, Default)]
pub struct TaskUpdate {
    pub description: Option<String>,
    pub status: Option<TaskStatus>,
    pub parent_id: Option<Option<String>>,
    pub labels: Option<Vec<String>>,
    pub priority: Option<u32>,
    pub complexity: Option<u32>,
    pub phase: Option<u32>,
    pub depends_on: Option<Vec<String>>,
}

impl ProjectManager {
    /// Create a new `ProjectManager` that reads/writes the given task file.
    pub fn new(task_file: impl Into<PathBuf>) -> Self {
        Self {
            task_file: task_file.into(),
        }
    }

    /// Return the path to the underlying task file.
    pub fn task_file(&self) -> &Path {
        &self.task_file
    }

    // ── Read operations ─────────────────────────────────────────────

    /// Load all tasks from the backing file.
    pub fn list_tasks(&self) -> Result<Vec<Task>> {
        task_manager::load_tasks(&self.task_file)
    }

    /// Return a single task by id.
    pub fn get_task(&self, id: &str) -> Result<Option<Task>> {
        let tasks = self.list_tasks()?;
        Ok(tasks.into_iter().find(|t| t.id == id))
    }

    /// Return all top-level tasks that are treated as epics.
    ///
    /// An epic is any task that has no `parent_id` **and** at least one other
    /// task references it via `parent_id`.
    pub fn list_epics(&self) -> Result<Vec<Task>> {
        let tasks = self.list_tasks()?;
        let parent_ids: std::collections::HashSet<String> =
            tasks.iter().filter_map(|t| t.parent_id.clone()).collect();
        Ok(tasks
            .into_iter()
            .filter(|t| t.parent_id.is_none() && parent_ids.contains(&t.id))
            .collect())
    }

    /// Return all sub-tasks of the given parent (epic) id.
    pub fn list_sub_tasks(&self, parent_id: &str) -> Result<Vec<Task>> {
        let tasks = self.list_tasks()?;
        Ok(tasks
            .into_iter()
            .filter(|t| t.parent_id.as_deref() == Some(parent_id))
            .collect())
    }

    /// Compute the aggregate progress of an epic (fraction of completed
    /// sub-tasks).  Returns `None` if the epic has no sub-tasks.
    pub fn epic_progress(&self, epic_id: &str) -> Result<Option<f64>> {
        let subs = self.list_sub_tasks(epic_id)?;
        if subs.is_empty() {
            return Ok(None);
        }
        let done = subs
            .iter()
            .filter(|t| t.status == TaskStatus::Completed)
            .count();
        Ok(Some(done as f64 / subs.len() as f64))
    }

    // ── Write operations ────────────────────────────────────────────

    /// Create a new top-level task (potential epic).
    pub fn create_task(&self, id: &str, description: &str, labels: Vec<String>) -> Result<Task> {
        let task = Task {
            id: id.to_string(),
            description: description.to_string(),
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
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels,
        };
        task_manager::append_task(&self.task_file, task.clone())?;
        Ok(task)
    }

    /// Create a sub-task under the given parent (epic) id.
    pub fn create_sub_task(
        &self,
        id: &str,
        parent_id: &str,
        description: &str,
        labels: Vec<String>,
    ) -> Result<Task> {
        // Verify parent exists.
        let tasks = self.list_tasks()?;
        if !tasks.iter().any(|t| t.id == parent_id) {
            bail!("Parent task '{}' does not exist", parent_id);
        }

        let task = Task {
            id: id.to_string(),
            description: description.to_string(),
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
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: Some(parent_id.to_string()),
            labels,
        };
        task_manager::append_task(&self.task_file, task.clone())?;
        Ok(task)
    }

    /// Update fields on an existing task.
    pub fn update_task(&self, id: &str, update: TaskUpdate) -> Result<Task> {
        let mut tasks = self.list_tasks()?;
        let task = tasks
            .iter_mut()
            .find(|t| t.id == id)
            .ok_or_else(|| anyhow::anyhow!("Task '{}' not found", id))?;

        if let Some(desc) = update.description {
            task.description = desc;
        }
        if let Some(status) = update.status {
            task.status = status;
        }
        if let Some(parent_id) = update.parent_id {
            task.parent_id = parent_id;
        }
        if let Some(labels) = update.labels {
            task.labels = labels;
        }
        if let Some(priority) = update.priority {
            task.priority = priority;
        }
        if let Some(complexity) = update.complexity {
            task.complexity = complexity;
        }
        if let Some(phase) = update.phase {
            task.phase = phase;
        }
        if let Some(depends_on) = update.depends_on {
            task.depends_on = depends_on;
        }

        let updated = task.clone();
        task_manager::save_tasks(&self.task_file, &tasks)?;
        Ok(updated)
    }

    /// Delete a task by id.  If the task is an epic with sub-tasks, the
    /// sub-tasks are also removed (cascade delete).
    pub fn delete_task(&self, id: &str) -> Result<()> {
        let mut tasks = self.list_tasks()?;
        let before = tasks.len();
        tasks.retain(|t| t.id != id && t.parent_id.as_deref() != Some(id));
        if tasks.len() == before {
            bail!("Task '{}' not found", id);
        }
        task_manager::save_tasks(&self.task_file, &tasks)
    }

    /// Move a task to a new status (board-style transition).
    pub fn move_task(&self, id: &str, new_status: TaskStatus) -> Result<Task> {
        self.update_task(
            id,
            TaskUpdate {
                status: Some(new_status),
                ..Default::default()
            },
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn setup() -> (tempfile::TempDir, ProjectManager) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        task_manager::save_tasks(&path, &[]).unwrap();
        let pm = ProjectManager::new(&path);
        (dir, pm)
    }

    #[test]
    fn create_and_list_task() {
        let (_dir, pm) = setup();
        pm.create_task("t1", "Do something", vec![]).unwrap();
        let tasks = pm.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t1");
    }

    #[test]
    fn create_sub_task_under_epic() {
        let (_dir, pm) = setup();
        pm.create_task("epic-1", "Big feature", vec![]).unwrap();
        pm.create_sub_task("sub-1", "epic-1", "Part A", vec![])
            .unwrap();
        pm.create_sub_task("sub-2", "epic-1", "Part B", vec!["backend".into()])
            .unwrap();

        let subs = pm.list_sub_tasks("epic-1").unwrap();
        assert_eq!(subs.len(), 2);
        assert_eq!(subs[0].parent_id.as_deref(), Some("epic-1"));
        assert_eq!(subs[1].labels, vec!["backend"]);
    }

    #[test]
    fn create_sub_task_rejects_missing_parent() {
        let (_dir, pm) = setup();
        let res = pm.create_sub_task("sub-1", "no-such-parent", "Oops", vec![]);
        assert!(res.is_err());
        assert!(res.unwrap_err().to_string().contains("does not exist"));
    }

    #[test]
    fn list_epics_only_returns_parents_with_children() {
        let (_dir, pm) = setup();
        pm.create_task("epic-1", "Epic", vec![]).unwrap();
        pm.create_task("standalone", "Solo task", vec![]).unwrap();
        pm.create_sub_task("sub-1", "epic-1", "Part A", vec![])
            .unwrap();

        let epics = pm.list_epics().unwrap();
        assert_eq!(epics.len(), 1);
        assert_eq!(epics[0].id, "epic-1");
    }

    #[test]
    fn epic_progress_calculation() {
        let (_dir, pm) = setup();
        pm.create_task("epic-1", "Epic", vec![]).unwrap();
        pm.create_sub_task("sub-1", "epic-1", "Part A", vec![])
            .unwrap();
        pm.create_sub_task("sub-2", "epic-1", "Part B", vec![])
            .unwrap();

        let progress = pm.epic_progress("epic-1").unwrap().unwrap();
        assert!((progress - 0.0).abs() < f64::EPSILON);

        pm.move_task("sub-1", TaskStatus::Completed).unwrap();
        let progress = pm.epic_progress("epic-1").unwrap().unwrap();
        assert!((progress - 0.5).abs() < f64::EPSILON);

        pm.move_task("sub-2", TaskStatus::Completed).unwrap();
        let progress = pm.epic_progress("epic-1").unwrap().unwrap();
        assert!((progress - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn epic_progress_none_for_no_sub_tasks() {
        let (_dir, pm) = setup();
        pm.create_task("t1", "Solo", vec![]).unwrap();
        assert!(pm.epic_progress("t1").unwrap().is_none());
    }

    #[test]
    fn update_task_fields() {
        let (_dir, pm) = setup();
        pm.create_task("t1", "Original", vec![]).unwrap();

        let updated = pm
            .update_task(
                "t1",
                TaskUpdate {
                    description: Some("Updated".into()),
                    labels: Some(vec!["urgent".into()]),
                    priority: Some(5),
                    ..Default::default()
                },
            )
            .unwrap();

        assert_eq!(updated.description, "Updated");
        assert_eq!(updated.labels, vec!["urgent"]);
        assert_eq!(updated.priority, 5);

        // Verify persisted
        let loaded = pm.get_task("t1").unwrap().unwrap();
        assert_eq!(loaded.description, "Updated");
    }

    #[test]
    fn update_task_not_found() {
        let (_dir, pm) = setup();
        let res = pm.update_task("ghost", TaskUpdate::default());
        assert!(res.is_err());
    }

    #[test]
    fn delete_task_removes_from_file() {
        let (_dir, pm) = setup();
        pm.create_task("t1", "Task 1", vec![]).unwrap();
        pm.create_task("t2", "Task 2", vec![]).unwrap();
        pm.delete_task("t1").unwrap();

        let tasks = pm.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "t2");
    }

    #[test]
    fn delete_epic_cascades_to_sub_tasks() {
        let (_dir, pm) = setup();
        pm.create_task("epic-1", "Epic", vec![]).unwrap();
        pm.create_sub_task("sub-1", "epic-1", "Part A", vec![])
            .unwrap();
        pm.create_sub_task("sub-2", "epic-1", "Part B", vec![])
            .unwrap();
        pm.create_task("standalone", "Solo", vec![]).unwrap();

        pm.delete_task("epic-1").unwrap();
        let tasks = pm.list_tasks().unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "standalone");
    }

    #[test]
    fn delete_task_not_found() {
        let (_dir, pm) = setup();
        let res = pm.delete_task("ghost");
        assert!(res.is_err());
    }

    #[test]
    fn move_task_changes_status() {
        let (_dir, pm) = setup();
        pm.create_task("t1", "Task", vec![]).unwrap();
        let moved = pm.move_task("t1", TaskStatus::InProgress).unwrap();
        assert_eq!(moved.status, TaskStatus::InProgress);

        let loaded = pm.get_task("t1").unwrap().unwrap();
        assert_eq!(loaded.status, TaskStatus::InProgress);
    }

    #[test]
    fn get_task_returns_none_for_missing() {
        let (_dir, pm) = setup();
        assert!(pm.get_task("ghost").unwrap().is_none());
    }

    #[test]
    fn update_task_parent_id() {
        let (_dir, pm) = setup();
        pm.create_task("epic-1", "Epic", vec![]).unwrap();
        pm.create_task("t1", "Orphan", vec![]).unwrap();

        let updated = pm
            .update_task(
                "t1",
                TaskUpdate {
                    parent_id: Some(Some("epic-1".into())),
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(updated.parent_id.as_deref(), Some("epic-1"));

        let subs = pm.list_sub_tasks("epic-1").unwrap();
        assert_eq!(subs.len(), 1);
    }
}
