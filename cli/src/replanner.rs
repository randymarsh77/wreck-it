use crate::task_manager::save_tasks;
use crate::types::{ModelProvider, Task, DEFAULT_GITHUB_MODELS_MODEL, DEFAULT_LLAMA_MODEL};
use anyhow::{bail, Context, Result};
use std::path::Path;
use std::process::Command;

// Re-export from wreck-it-core so callers of `crate::replanner::*`
// continue to work unchanged.
pub use wreck_it_core::replanner::{build_replan_prompt, parse_and_validate_replan};

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
    pub async fn replan(&self, tasks: &[Task], failed: &Task, error: &str) -> Result<Vec<Task>> {
        let git_status = self.get_git_status();
        let prompt = build_replan_prompt(tasks, failed, error, &git_status);
        let raw = self.call_llm(&prompt).await?;
        parse_and_validate_replan(tasks, &raw).map_err(|e| anyhow::anyhow!(e))
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
            ModelProvider::GithubModels
            | ModelProvider::Llama
            | ModelProvider::CopilotAutopilot => self.call_via_http(prompt).await,
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

        let cli_path = crate::agent::resolve_copilot_cli_path().context(
            "Could not find the 'copilot' binary on PATH. \
                      Install GitHub Copilot CLI (https://gh.io/copilot-install) \
                      or ensure it is available in your shell environment.",
        )?;

        let config = SessionConfig {
            request_permission: Some(false),
            request_user_input: Some(false),
            ..Default::default()
        };

        crate::agent::copilot_oneshot(cli_path, config, prompt.to_string(), 120_000, "[]").await
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskStatus};

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
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
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
