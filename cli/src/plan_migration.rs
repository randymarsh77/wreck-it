//! Plan migration — moves pending task plans from the main branch config
//! directory into the state branch.
//!
//! Cloud agents write new or revised task plans as JSON files in
//! `.wreck-it/plans/` on the main branch.  At the start of each headless
//! iteration the runner calls [`migrate_pending_plans`] to merge those
//! plans into the active task list on the state branch and remove the
//! consumed files.
//!
//! ## Targeted plan routing
//!
//! Plan files can target a specific task file using a filename convention:
//!
//! - **`{target}--{label}.json`** — routes tasks into the task file named
//!   `{target}` on the state branch.  For example,
//!   `feature-dev-tasks.json--batch-01.json` merges into
//!   `feature-dev-tasks.json`.
//! - **`{name}.json`** (no `--` separator) — merges into the current
//!   ralph's default task file.
//!
//! This allows a single agent (e.g. a feature assessor running under the
//! "features" ralph) to generate tasks destined for a different ralph
//! (e.g. "feature-dev").

use crate::task_manager::{load_tasks, save_tasks};
use crate::types::Task;
use anyhow::{Context, Result};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use wreck_it_core::plan_migration::merge_pending_tasks;

// Re-export the constant so headless.rs can use it.
pub use wreck_it_core::config::PLANS_DIR;

/// Filename separator used to encode the target task file in a plan filename.
///
/// A plan file named `feature-dev-tasks.json--batch-01.json` targets
/// `feature-dev-tasks.json` on the state branch.
const TARGET_SEPARATOR: &str = "--";

/// Extract the target task filename from a plan filename.
///
/// Returns `Some("target.json")` for filenames matching the
/// `{target.json}--{label}.json` convention, or `None` for plain names.
fn parse_plan_target(plan_filename: &str) -> Option<&str> {
    let (target, _rest) = plan_filename.split_once(TARGET_SEPARATOR)?;
    if target.is_empty() {
        return None;
    }
    Some(target)
}

/// Scan the plans directory on the main branch for pending task files,
/// merge them into the task list(s) on the state branch, and remove the
/// consumed plan files.
///
/// `config_dir` is the `.wreck-it/` directory on the main branch checkout.
/// `state_dir` is the state worktree directory where task files live.
/// `default_task_file` is the full path to the current ralph's task file
///  (used when a plan filename has no `--` target prefix).
///
/// Returns the total number of tasks added or updated across all targets.
pub fn migrate_pending_plans(
    config_dir: &Path,
    state_dir: &Path,
    default_task_file: &Path,
) -> Result<usize> {
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

    // Group plan files by their target task file.
    let mut grouped: HashMap<PathBuf, Vec<(PathBuf, Vec<Task>)>> = HashMap::new();

    for entry in &entries {
        let path = entry.path();
        let content = std::fs::read_to_string(&path)
            .with_context(|| format!("Failed to read plan file: {}", path.display()))?;
        let pending: Vec<Task> = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse plan file: {}", path.display()))?;

        let filename = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();

        let target = match parse_plan_target(filename) {
            Some(target_name) => state_dir.join(target_name),
            None => default_task_file.to_path_buf(),
        };

        grouped.entry(target).or_default().push((path, pending));
    }

    let mut total_changed: usize = 0;
    let mut consumed: Vec<PathBuf> = Vec::new();

    for (target_file, plans) in &grouped {
        let mut existing = load_tasks(target_file)?;
        let mut file_changed = 0;

        for (path, pending) in plans {
            let changed = merge_pending_tasks(&mut existing, pending);
            file_changed += changed;
            consumed.push(path.clone());
        }

        if file_changed > 0 {
            save_tasks(target_file, &existing)
                .with_context(|| format!("Failed to save merged tasks to {}", target_file.display()))?;
        }

        total_changed += file_changed;
    }

    // Remove consumed plan files regardless of whether any tasks changed.
    // A plan that only contains already-completed tasks still needs to be
    // removed so it is not re-processed on every subsequent iteration.
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

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &task_file).unwrap();
        assert_eq!(count, 0);
    }

    #[test]
    fn migrate_empty_plans_dir() {
        let config_dir = tempdir().unwrap();
        std::fs::create_dir_all(config_dir.path().join(PLANS_DIR)).unwrap();
        let state_dir = tempdir().unwrap();
        let task_file = state_dir.path().join("tasks.json");
        save_tasks(&task_file, &[]).unwrap();

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &task_file).unwrap();
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

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &task_file).unwrap();
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

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &task_file).unwrap();
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

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &task_file).unwrap();
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

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &task_file).unwrap();
        // The task is completed, so it should not be replaced.
        assert_eq!(count, 0);
    }

    // ---- parse_plan_target tests ----

    #[test]
    fn parse_target_with_separator() {
        assert_eq!(
            parse_plan_target("feature-dev-tasks.json--batch-01.json"),
            Some("feature-dev-tasks.json"),
        );
    }

    #[test]
    fn parse_target_without_separator() {
        assert_eq!(parse_plan_target("replan-001.json"), None);
    }

    #[test]
    fn parse_target_empty_prefix() {
        // Leading `--` should not produce an empty target.
        assert_eq!(parse_plan_target("--something.json"), None);
    }

    // ---- targeted plan routing tests ----

    #[test]
    fn migrate_routes_targeted_plan_to_correct_file() {
        let config_dir = tempdir().unwrap();
        let plans_dir = config_dir.path().join(PLANS_DIR);
        std::fs::create_dir_all(&plans_dir).unwrap();

        // Write a targeted plan file: `other-tasks.json--batch-01.json`
        let plan_tasks = vec![make_task("targeted-1", TaskStatus::Pending)];
        std::fs::write(
            plans_dir.join("other-tasks.json--batch-01.json"),
            serde_json::to_string(&plan_tasks).unwrap(),
        )
        .unwrap();

        let state_dir = tempdir().unwrap();
        // Create the default task file (should NOT receive the targeted plan).
        let default_task_file = state_dir.path().join("tasks.json");
        save_tasks(&default_task_file, &[]).unwrap();
        // Create the target task file.
        let target_task_file = state_dir.path().join("other-tasks.json");
        save_tasks(&target_task_file, &[]).unwrap();

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &default_task_file).unwrap();
        assert_eq!(count, 1);

        // Default task file should remain empty.
        let default_tasks = load_tasks(&default_task_file).unwrap();
        assert!(default_tasks.is_empty(), "default task file should be empty");

        // Target task file should contain the new task.
        let target_tasks = load_tasks(&target_task_file).unwrap();
        assert_eq!(target_tasks.len(), 1);
        assert_eq!(target_tasks[0].id, "targeted-1");
    }

    #[test]
    fn migrate_mixes_targeted_and_default_plans() {
        let config_dir = tempdir().unwrap();
        let plans_dir = config_dir.path().join(PLANS_DIR);
        std::fs::create_dir_all(&plans_dir).unwrap();

        // A plain plan file (goes to default).
        let plain = vec![make_task("default-1", TaskStatus::Pending)];
        std::fs::write(
            plans_dir.join("plan.json"),
            serde_json::to_string(&plain).unwrap(),
        )
        .unwrap();

        // A targeted plan file (goes to feature-dev-tasks.json).
        let targeted = vec![make_task("dev-1", TaskStatus::Pending)];
        std::fs::write(
            plans_dir.join("feature-dev-tasks.json--assessor.json"),
            serde_json::to_string(&targeted).unwrap(),
        )
        .unwrap();

        let state_dir = tempdir().unwrap();
        let default_file = state_dir.path().join("tasks.json");
        let dev_file = state_dir.path().join("feature-dev-tasks.json");
        save_tasks(&default_file, &[]).unwrap();
        save_tasks(&dev_file, &[]).unwrap();

        let count =
            migrate_pending_plans(config_dir.path(), state_dir.path(), &default_file).unwrap();
        assert_eq!(count, 2);

        let default_tasks = load_tasks(&default_file).unwrap();
        assert_eq!(default_tasks.len(), 1);
        assert_eq!(default_tasks[0].id, "default-1");

        let dev_tasks = load_tasks(&dev_file).unwrap();
        assert_eq!(dev_tasks.len(), 1);
        assert_eq!(dev_tasks[0].id, "dev-1");
    }
}
