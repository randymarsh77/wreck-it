use crate::task_manager::{has_circular_dependency, save_tasks};
use crate::types::{
    ModelProvider, Task, TaskStatus, DEFAULT_GITHUB_MODELS_MODEL, DEFAULT_LLAMA_MODEL,
};
use anyhow::{bail, Context, Result};
use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

/// LLM-powered adaptive re-planner that modifies a task list after repeated
/// consecutive failures, inspired by LangGraph's re-planner node and
/// MetaGPT's iterative planning.
pub struct TaskReplanner {
    model_provider: ModelProvider,
    api_endpoint: String,
    api_token: Option<String>,
    work_dir: String,
}

impl TaskReplanner {
    pub fn new(
        model_provider: ModelProvider,
        api_endpoint: String,
        api_token: Option<String>,
        work_dir: String,
    ) -> Self {
        Self {
            model_provider,
            api_endpoint,
            api_token,
            work_dir,
        }
    }

    /// Invoke the re-planner agent with the current task list, the failed
    /// task, and the error output.  Returns the updated (validated) task list.
    pub async fn replan(
        &self,
        tasks: &[Task],
        failed: &Task,
        error: &str,
    ) -> Result<Vec<Task>> {
        let git_status = self.get_git_status();
        let prompt = build_replan_prompt(tasks, failed, error, &git_status);
        let raw = self.call_llm(&prompt).await?;
        parse_and_validate_replan(tasks, &raw)
    }

    fn get_git_status(&self) -> String {
        Command::new("git")
            .args(["status", "--short"])
            .current_dir(&self.work_dir)
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
            .unwrap_or_default()
    }

    async fn call_llm(&self, prompt: &str) -> Result<String> {
        match self.model_provider {
            ModelProvider::GithubModels | ModelProvider::Llama => {
                self.call_via_http(prompt).await
            }
            ModelProvider::Copilot => self.call_via_copilot_sdk(prompt).await,
        }
    }

    async fn call_via_http(&self, prompt: &str) -> Result<String> {
        let token = self
            .api_token
            .as_deref()
            .context("API token is required for this model provider")?;

        let model = match self.model_provider {
            ModelProvider::Llama => DEFAULT_LLAMA_MODEL,
            _ => DEFAULT_GITHUB_MODELS_MODEL,
        };

        let body = serde_json::json!({
            "model": model,
            "messages": [{ "role": "user", "content": prompt }]
        });

        let client = reqwest::Client::new();
        let response = client
            .post(&self.api_endpoint)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send HTTP request to models API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            bail!("Models API returned error ({}): {}", status, body);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse models API response")?;

        let content = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("Models API response missing expected choices[0].message.content field")?
            .to_string();

        Ok(content)
    }

    async fn call_via_copilot_sdk(&self, prompt: &str) -> Result<String> {
        use copilot_sdk_supercharged::*;

        let options = CopilotClientOptions {
            log_level: "info".to_string(),
            ..Default::default()
        };

        let client = CopilotClient::new(options);
        client
            .start()
            .await
            .context("Failed to start Copilot client")?;

        let config = SessionConfig {
            request_permission: Some(false),
            request_user_input: Some(false),
            ..Default::default()
        };

        let session = client
            .create_session(config)
            .await
            .context("Failed to create Copilot session")?;

        let response = session
            .send_and_wait(
                MessageOptions {
                    prompt: prompt.to_string(),
                    attachments: None,
                    mode: None,
                },
                Some(120_000),
            )
            .await;

        session.destroy().await.ok();
        client.stop().await.ok();

        let result = response
            .context("Failed to get response from Copilot")?
            .map(|event| {
                event
                    .assistant_message_content()
                    .unwrap_or("[]")
                    .to_string()
            })
            .unwrap_or_else(|| "[]".to_string());

        Ok(result)
    }
}

/// Invoke the re-planner and persist the updated task list to `task_file`.
///
/// This is the top-level entry point called by the loop when the consecutive
/// failure threshold is reached.
pub async fn replan_and_save(
    replanner: &TaskReplanner,
    tasks: &[Task],
    failed: &Task,
    error: &str,
    task_file: &Path,
) -> Result<Vec<Task>> {
    let updated = replanner.replan(tasks, failed, error).await?;
    save_tasks(task_file, &updated).context("Failed to save re-planned tasks")?;
    Ok(updated)
}

/// Maximum length (in bytes) to which the error string is truncated before
/// being embedded in the re-planner prompt.  This limits excessively long
/// error messages and reduces the prompt-injection attack surface.
const MAX_ERROR_LEN: usize = 4096;

/// Build the re-planner prompt containing the full task list, failed task
/// info, error output, and current git status.
///
/// **Note**: task descriptions, error output, and git status are user/agent
/// controlled data embedded in the prompt.  All three are included as
/// informational context for the LLM; the response is validated structurally
/// before being used.
pub fn build_replan_prompt(
    tasks: &[Task],
    failed: &Task,
    error: &str,
    git_status: &str,
) -> String {
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
pub fn parse_and_validate_replan(original_tasks: &[Task], raw: &str) -> Result<Vec<Task>> {
    let json_str = extract_json_array(raw)?;

    let mut tasks: Vec<Task> = serde_json::from_str(&json_str)
        .context("Re-planner output is not a valid JSON array of task objects")?;

    if tasks.is_empty() {
        bail!("Re-planner returned an empty task list");
    }

    // Validate no empty or duplicate IDs.
    let mut seen_ids: HashSet<&str> = HashSet::new();
    for task in &tasks {
        if task.id.is_empty() {
            bail!("Re-planned task has an empty id");
        }
        if !seen_ids.insert(task.id.as_str()) {
            bail!("Duplicate task ID '{}' in re-plan", task.id);
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
            bail!(
                "Re-planned task list contains a circular dependency involving task '{}'",
                tasks[i].id
            );
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
fn extract_json_array(raw: &str) -> Result<String> {
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
        .context("Re-planner output does not contain a JSON array")?;
    let end = raw
        .rfind(']')
        .context("Re-planner output does not contain a valid JSON array (missing ']')")?;
    if end < start {
        bail!("Re-planner output JSON array delimiters are malformed");
    }
    Ok(raw[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind};

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
        // LLM tries to change status of completed task
        let raw = r#"[
            {"id":"1","description":"task 1","status":"pending"},
            {"id":"2","description":"rewritten task 2","status":"pending"}
        ]"#;
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        // Completed task must remain completed regardless of LLM output
        assert_eq!(tasks[0].status, TaskStatus::Completed);
        assert_eq!(tasks[1].status, TaskStatus::Pending);
    }

    #[test]
    fn replan_rejects_empty_task_list() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let err = parse_and_validate_replan(&original, "[]").unwrap_err();
        assert!(err.to_string().contains("empty task list"));
    }

    #[test]
    fn replan_rejects_duplicate_ids() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = r#"[
            {"id":"1","description":"a","status":"pending"},
            {"id":"1","description":"b","status":"pending"}
        ]"#;
        let err = parse_and_validate_replan(&original, raw).unwrap_err();
        assert!(err.to_string().contains("Duplicate task ID"));
    }

    #[test]
    fn replan_rejects_circular_dependency() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = r#"[
            {"id":"a","description":"a","status":"pending","depends_on":["b"]},
            {"id":"b","description":"b","status":"pending","depends_on":["a"]}
        ]"#;
        let err = parse_and_validate_replan(&original, raw).unwrap_err();
        assert!(err.to_string().contains("circular dependency"));
    }

    #[test]
    fn replan_rejects_empty_id() {
        let original = vec![make_task("1", TaskStatus::Failed, vec![])];
        let raw = r#"[{"id":"","description":"task","status":"pending"}]"#;
        let err = parse_and_validate_replan(&original, raw).unwrap_err();
        assert!(err.to_string().contains("empty id"));
    }

    #[test]
    fn replan_accepts_split_subtasks() {
        let original = vec![make_task("big", TaskStatus::Failed, vec![])];
        // LLM splits "big" into "big-a" and "big-b" (both new IDs)
        let raw = r#"[
            {"id":"big-a","description":"part 1","status":"pending"},
            {"id":"big-b","description":"part 2","status":"pending","depends_on":["big-a"]}
        ]"#;
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "big-a");
        assert_eq!(tasks[1].id, "big-b");
    }

    #[test]
    fn replan_accepts_injected_prerequisite() {
        let original = vec![
            make_task("1", TaskStatus::Completed, vec![]),
            make_task("2", TaskStatus::Failed, vec!["1"]),
        ];
        // LLM injects task "1.5" as a prerequisite for "2"
        let raw = r#"[
            {"id":"1","description":"task 1","status":"completed"},
            {"id":"1.5","description":"prereq","status":"pending","depends_on":["1"]},
            {"id":"2","description":"task 2","status":"pending","depends_on":["1","1.5"]}
        ]"#;
        let tasks = parse_and_validate_replan(&original, raw).unwrap();
        assert_eq!(tasks.len(), 3);
        assert_eq!(tasks[1].id, "1.5");
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
        // The JSON-serialised task list should appear in the prompt.
        assert!(prompt.contains("\"id\""));
        assert!(prompt.contains("\"a\""));
        assert!(prompt.contains("\"b\""));
    }
}
