use crate::types::{Task, TaskStatus};
use anyhow::{Context, Result};
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

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_save_tasks() {
        let dir = tempdir().unwrap();
        let task_file = dir.path().join("tasks.json");

        let tasks = vec![Task {
            id: "1".to_string(),
            description: "Test task".to_string(),
            status: TaskStatus::Pending,
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
            },
            Task {
                id: "2".to_string(),
                description: "Pending".to_string(),
                status: TaskStatus::Pending,
            },
        ];

        assert_eq!(get_next_task(&tasks), Some(1));
    }
}
