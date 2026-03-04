//! Plan migration — moves pending task plans from the main branch config
//! directory into the state branch.
//!
//! Cloud agents write new or revised task plans as JSON files in
//! `.wreck-it/plans/` on the main branch.  At the start of each headless
//! iteration the runner calls [`migrate_pending_plans`] to merge those
//! plans into the active task list on the state branch and remove the
//! consumed files.

use crate::task_manager::{load_tasks, save_tasks};
use crate::types::Task;
use anyhow::{Context, Result};
use std::path::Path;
use wreck_it_core::plan_migration::merge_pending_tasks;

// Re-export the constant so headless.rs can use it.
pub use wreck_it_core::config::PLANS_DIR;

/// Scan the plans directory on the main branch for pending task files,
/// merge them into the task list on the state branch, and remove the
/// consumed plan files.
///
/// `config_dir` is the `.wreck-it/` directory on the main branch checkout.
/// `task_file` is the path to the task JSON file on the state branch.
///
/// Returns the total number of tasks added or updated.
pub fn migrate_pending_plans(config_dir: &Path, task_file: &Path) -> Result<usize> {
    let plans_dir = config_dir.join(PLANS_DIR);
    if !plans_dir.exists() {
        return Ok(0);
    }

    let entries: Vec<_> = std::fs::read_dir(&plans_dir)
        .context("Failed to read plans directory")?
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .is_some_and(|ext| ext == "json")
        })
        .collect();

    if entries.is_empty() {
        return Ok(0);
    }

    let mut existing = load_tasks(task_file)?;
    let mut total_changed = 0;
    let mut consumed: Vec<std::path::PathBuf> = Vec::new();

    for entry in &entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read plan file: {}", path.display()))?;
        let pending: Vec<Task> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse plan file: {}", path.display()))?;
        let changed = merge_pending_tasks(&mut existing, &pending);
        total_changed += changed;
        consumed.push(path);
    }

    if total_changed > 0 {
        save_tasks(task_file, &existing).context("Failed to save merged tasks")?;
    }

    // Remove consumed plan files so they are not re-processed.
    for path in consumed {
        if let Err(e) = std::fs::remove_file(&path) {
            println!(
                "[wreck-it] warning: failed to remove consumed plan file {}: {}",
                path.display(),
                e,
            );
        }
    }

    Ok(total_changed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskRuntime, TaskStatus};
    use tempfile::tempdir;

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
    fn migrate_no_plans_dir() {
        let config_dir = tempdir().unwrap();
        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        save_tasks(&task_file, &[]).unwrap();

        let count = migrate_pending_plans(config_dir.path(), &task_file).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migrate_empty_plans_dir() {
        let config_dir = tempdir().unwrap();
        std::fs::create_dir_all(config_dir.path().join(PLANS_DIR)).unwrap();
        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        save_tasks(&task_file, &[]).unwrap();

        let count = migrate_pending_plans(config_dir.path(), &task_file).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migrate_merges_and_removes_plan_file() {
        let config_dir = tempdir().unwrap();
        let plans_dir = config_dir.path().join(PLANS_DIR);
        std::fs::create_dir_all(&plans_dir).unwrap();

        // Write a plan file with one new task.
        let plan_tasks = vec![make_task("new-1", TaskStatus::Pending)];
        let plan_json = serde_json::to_string_pretty(&plan_tasks).unwrap();
        let plan_file = plans_dir.join("replan-001.json");
        std::fs::write(&plan_file, &plan_json).unwrap();

        // Write an existing task file on the "state branch".
        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        let existing = vec![make_task("old-1", TaskStatus::Completed)];
        save_tasks(&task_file, &existing).unwrap();

        let count = migrate_pending_plans(config_dir.path(), &task_file).unwrap();
        assert_eq!(count, 1);

        // Task file should now contain both tasks.
        let reloaded = load_tasks(&task_file).unwrap();
        assert_eq!(reloaded.len(), 2);
        assert_eq!(reloaded[0].id, "old-1");
        assert_eq!(reloaded[1].id, "new-1");

        // Plan file should have been removed.
        assert!(!plan_file.exists());
    }

    #[test]
    fn migrate_skips_non_json_files() {
        let config_dir = tempdir().unwrap();
        let plans_dir = config_dir.path().join(PLANS_DIR);
        std::fs::create_dir_all(&plans_dir).unwrap();

        // Write a non-JSON file (should be ignored).
        std::fs::write(plans_dir.join(".gitkeep"), "").unwrap();
        std::fs::write(plans_dir.join("notes.txt"), "some notes").unwrap();

        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        save_tasks(&task_file, &[]).unwrap();

        let count = migrate_pending_plans(config_dir.path(), &task_file).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migrate_multiple_plan_files() {
        let config_dir = tempdir().unwrap();
        let plans_dir = config_dir.path().join(PLANS_DIR);
        std::fs::create_dir_all(&plans_dir).unwrap();

        // Write two plan files.
        let plan1 = vec![make_task("a", TaskStatus::Pending)];
        std::fs::write(
            plans_dir.join("plan-1.json"),
            serde_json::to_string(&plan1).unwrap(),
        )
        .unwrap();

        let plan2 = vec![make_task("b", TaskStatus::Pending)];
        std::fs::write(
            plans_dir.join("plan-2.json"),
            serde_json::to_string(&plan2).unwrap(),
        )
        .unwrap();

        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        save_tasks(&task_file, &[]).unwrap();

        let count = migrate_pending_plans(config_dir.path(), &task_file).unwrap();
        assert_eq!(count, 2);

        let reloaded = load_tasks(&task_file).unwrap();
        assert_eq!(reloaded.len(), 2);

        // Both plan files should be consumed.
        assert!(!plans_dir.join("plan-1.json").exists());
        assert!(!plans_dir.join("plan-2.json").exists());
    }

    #[test]
    fn migrate_does_not_write_when_nothing_changed() {
        let config_dir = tempdir().unwrap();
        let plans_dir = config_dir.path().join(PLANS_DIR);
        std::fs::create_dir_all(&plans_dir).unwrap();

        // Plan file contains a task that already exists and is completed.
        let plan = vec![make_task("done", TaskStatus::Pending)];
        std::fs::write(
            plans_dir.join("noop.json"),
            serde_json::to_string(&plan).unwrap(),
        )
        .unwrap();

        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        let existing = vec![make_task("done", TaskStatus::Completed)];
        save_tasks(&task_file, &existing).unwrap();

        let count = migrate_pending_plans(config_dir.path(), &task_file).unwrap();
        // The task is completed, so it should not be replaced.
        assert_eq!(count, 0);
    }
}
