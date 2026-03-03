//! Pure replanner logic — prompt building and response validation.
//!
//! These functions contain no I/O and no LLM-client dependencies, so they
//! are shared by both the CLI and the worker.

use crate::task_manager::has_circular_dependency;
use crate::types::{Task, TaskStatus};
use std::collections::HashSet;

/// Maximum length (in bytes) to which the error string is truncated before
/// being embedded in the re-planner prompt.  This limits excessively long
/// error messages and reduces the prompt-injection attack surface.
pub const MAX_ERROR_LEN: usize = 4096;

/// Build the re-planner prompt containing the full task list, failed task
/// info, error output, and current git status.
///
/// **Note**: task descriptions, error output, and git status are user/agent
/// controlled data embedded in the prompt.  All three are included as
/// informational context for the LLM; the response is validated structurally
/// before being used.
pub fn build_replan_prompt(tasks: &[Task], failed: &Task, error: &str, git_status: &str) -> String {
    let tasks_json = serde_json::to_string_pretty(tasks).unwrap_or_else(|_| "[]".to_string());
    let git_section = if git_status.trim().is_empty() {
        "(no changes)".to_string()
    } else {
        git_status.to_string()
    };
    // Truncate error to keep prompt size reasonable and limit injection surface.
    let truncated_error = if error.len() > MAX_ERROR_LEN {
        format!("{}... (truncated)", &error[..MAX_ERROR_LEN])
    } else {
        error.to_string()
    };
    format!(
        "You are an adaptive task re-planner for a software development agent loop.\n\
         A task has failed {failed_attempts} consecutive time(s) and needs to be revised.\n\n\
         Current task list (JSON):\n{tasks}\n\n\
         Failed task ID: {id}\n\
         Failed task description: {desc}\n\n\
         Error output:\n{error}\n\n\
         Current git status:\n{git_status}\n\n\
         Your job is to fix the failed task. You may:\n\
         (a) Rewrite the failed task's description to be clearer or more actionable.\n\
         (b) Split it into smaller sub-tasks with new unique IDs.\n\
         (c) Inject a new prerequisite task before the failed task.\n\n\
         Rules:\n\
         - Return the COMPLETE updated task list as a JSON array (all tasks, not just changed ones).\n\
         - Do NOT introduce duplicate task IDs.\n\
         - Do NOT introduce circular dependencies.\n\
         - Do NOT change the status of any task that is already completed.\n\
         - New or rewritten tasks should have status \"pending\".\n\
         - Preserve all task fields (id, description, status, phase, depends_on, etc.).\n\
         - Return ONLY the JSON array with NO additional text, markdown, or explanation.\n\n\
         Output the updated JSON array now:",
        failed_attempts = failed.failed_attempts,
        tasks = tasks_json,
        id = failed.id,
        desc = failed.description,
        error = truncated_error,
        git_status = git_section,
    )
}

/// Parse the raw LLM output and validate the updated task list.
///
/// Validation rules:
/// - Must parse as a JSON array of tasks.
/// - No empty task IDs.
/// - No duplicate task IDs.
/// - No circular dependencies in the new task graph.
/// - Tasks that were `Completed` in the original list remain `Completed`.
pub fn parse_and_validate_replan(
    original_tasks: &[Task],
    raw: &str,
) -> Result<Vec<Task>, String> {
    let json_str = extract_json_array(raw)?;

    let mut tasks: Vec<Task> = serde_json::from_str(&json_str)
        .map_err(|e| format!("Re-planner output is not a valid JSON array of task objects: {e}"))?;

    if tasks.is_empty() {
        return Err("Re-planner returned an empty task list".to_string());
    }

    // Validate no empty or duplicate IDs.
    let mut seen_ids: HashSet<&str> = HashSet::new();
    for task in &tasks {
        if task.id.is_empty() {
            return Err("Re-planned task has an empty id".to_string());
        }
        if !seen_ids.insert(task.id.as_str()) {
            return Err(format!("Duplicate task ID '{}' in re-plan", task.id));
        }
    }

    // Validate no circular dependencies in the new task graph.
    for i in 0..tasks.len() {
        let rest: Vec<Task> = tasks
            .iter()
            .enumerate()
            .filter(|(j, _)| *j != i)
            .map(|(_, t)| t.clone())
            .collect();
        if has_circular_dependency(&rest, &tasks[i]) {
            return Err(format!(
                "Re-planned task list contains a circular dependency involving task '{}'",
                tasks[i].id
            ));
        }
    }

    // Preserve `Completed` status for tasks that were already done.
    let original_completed: HashSet<&str> = original_tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .map(|t| t.id.as_str())
        .collect();

    for task in tasks.iter_mut() {
        if original_completed.contains(task.id.as_str()) {
            task.status = TaskStatus::Completed;
        }
    }

    Ok(tasks)
}

/// Extract the first JSON array from a string, stripping markdown code fences
/// if present.
fn extract_json_array(raw: &str) -> Result<String, String> {
    if let Some(fence_start) = raw.find("```") {
        let after_fence = &raw[fence_start + 3..];
        let body = if let Some(nl) = after_fence.find('\n') {
            &after_fence[nl + 1..]
        } else {
            after_fence
        };
        if let Some(fence_end) = body.find("```") {
            return Ok(body[..fence_end].trim().to_string());
        }
    }

    let start = raw
        .find('[')
        .ok_or("Re-planner output does not contain a JSON array")?;
    let end = raw
        .rfind(']')
        .ok_or("Re-planner output does not contain a valid JSON array (missing ']')")?;
    if end < start {
        return Err("Re-planner output JSON array delimiters are malformed".to_string());
    }
    Ok(raw[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskRuntime};

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

    // ---- parse_and_validate_replan tests ----

    #[test]
    fn replan_parses_valid_task_list() {
        let original = vec![
            make_task("1", TaskStatus::Completed, vec![]),
            make_task("2", TaskStatus::Failed, vec!["1"]),
        ];
        let raw = r#"[
            {"id":"1","description":"task 1","status":"completed"},
            {"id":"2","description":"revised task 2","status":"pending","depends_on":["1"]}
        ]"#;
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].description, "revised task 2");
    }

    #[test]
    fn replan_preserves_completed_status() {
        let original = vec![
            make_task("1", TaskStatus::Completed, vec![]),
            make_task("2", TaskStatus::Failed, vec![]),
        ];
        let raw = r#"[
            {"id":"1","description":"task 1","status":"pending"},
            {"id":"2","description":"rewritten task 2","status":"pending"}
        ]"#;
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[1].status, TaskStatus::Pending);
    }

    #[test]
    fn replan_rejects_empty_task_list() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let err = parse_and_validate_replan(&original, "[]").unwrap_err();
        assert!(err.contains("empty task list"));
    }

    #[test]
    fn replan_rejects_duplicate_ids() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = r#"[
            {"id":"1","description":"a","status":"pending"},
            {"id":"1","description":"b","status":"pending"}
        ]"#;
        let err = parse_and_validate_replan(&original, raw).unwrap_err();
        assert!(err.contains("Duplicate task ID"));
    }

    #[test]
    fn replan_rejects_circular_dependency() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = r#"[
            {"id":"a","description":"a","status":"pending","depends_on":["b"]},
            {"id":"b","description":"b","status":"pending","depends_on":["a"]}
        ]"#;
        let err = parse_and_validate_replan(&original, raw).unwrap_err();
        assert!(err.contains("circular dependency"));
    }

    #[test]
    fn replan_rejects_empty_id() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = r#"[{"id":"","description":"task","status":"pending"}]"#;
        let err = parse_and_validate_replan(&original, raw).unwrap_err();
        assert!(err.contains("empty id"));
    }

    #[test]
    fn replan_accepts_split_subtasks() {
        let original = vec![make_task("big", TaskStatus::Failed, vec![])];
        let raw = r#"[
            {"id":"big-a","description":"part 1","status":"pending"},
            {"id":"big-b","description":"part 2","status":"pending","depends_on":["big-a"]}
        ]"#;
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "big-a");
    }

    #[test]
    fn replan_strips_markdown_code_fence() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = "```json\n[\
            {\"id\":\"1\",\"description\":\"revised\",\"status\":\"pending\"}\
        ]\n```";
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        assert_eq!(tasks[0].description, "revised");
    }

    // ---- build_replan_prompt tests ----

    #[test]
    fn prompt_contains_failed_task_info() {
        let mut failed = make_task("my-task", TaskStatus::Failed, vec![]);
        failed.failed_attempts = 3;
        let prompt = build_replan_prompt(&[], &failed, "timeout error", "M src/main.rs");
        assert!(prompt.contains("my-task"));
        assert!(prompt.contains("timeout error"));
        assert!(prompt.contains("src/main.rs"));
    }

    #[test]
    fn prompt_shows_no_changes_when_git_status_empty() {
        let failed = make_task("t", TaskStatus::Failed, vec![]);
        let prompt = build_replan_prompt(&[], &failed, "", "");
        assert!(prompt.contains("(no changes)"));
    }

    #[test]
    fn prompt_includes_full_task_list() {
        let tasks = vec![
            make_task("a", TaskStatus::Completed, vec![]),
            make_task("b", TaskStatus::Failed, vec!["a"]),
        ];
        let failed = tasks[1].clone();
        let prompt = build_replan_prompt(&tasks, &failed, "error", "");
        assert!(prompt.contains("\"id\""));
        assert!(prompt.contains("\"a\""));
        assert!(prompt.contains("\"b\""));
    }
}
