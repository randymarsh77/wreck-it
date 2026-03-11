use crate::types::{
    AgentRole, ModelProvider, Task, TaskKind, TaskStatus, DEFAULT_GITHUB_MODELS_MODEL,
    DEFAULT_GITHUB_MODELS_NAMING_MODEL, DEFAULT_LLAMA_MODEL,
};
use anyhow::{bail, Context, Result};
use serde::Deserialize;

/// A minimal task plan entry as returned by the LLM planner.
#[derive(Debug, Deserialize)]
struct PlanEntry {
    id: String,
    description: String,
    #[serde(default = "default_phase")]
    phase: u32,
    #[serde(default)]
    depends_on: Vec<String>,
}

fn default_phase() -> u32 {
    1
}

/// LLM-powered task planner that converts a natural-language goal into a
/// structured list of [`Task`] objects.
pub struct TaskPlanner {
    model_provider: ModelProvider,
    api_endpoint: String,
    api_token: Option<String>,
}

impl TaskPlanner {
    /// Create a new planner from explicit configuration values.
    pub fn new(
        model_provider: ModelProvider,
        api_endpoint: String,
        api_token: Option<String>,
    ) -> Self {
        Self {
            model_provider,
            api_endpoint,
            api_token,
        }
    }

    /// Send a natural-language `goal` to the configured LLM and return a
    /// validated list of [`Task`] objects.
    ///
    /// The planner instructs the model to emit tasks with `id`, `description`,
    /// `phase`, and optional `depends_on` fields.  The raw output is validated
    /// against the [`Task`] schema before being returned.
    pub async fn generate_task_plan(&self, goal: &str) -> Result<Vec<Task>> {
        let prompt = build_planner_prompt(goal);
        let raw = self.call_llm(&prompt, None).await?;
        parse_and_validate_plan(&raw)
    }

    /// Ask the LLM to produce a short, descriptive plan name from a goal.
    ///
    /// Uses a low-cost model when available. The returned name is slugified so
    /// it is safe for use as a ralph name and in filenames.
    pub async fn generate_plan_name(&self, goal: &str) -> Result<String> {
        let prompt = build_naming_prompt(goal);

        let model_override = match self.model_provider {
            ModelProvider::GithubModels => Some(DEFAULT_GITHUB_MODELS_NAMING_MODEL),
            _ => None, // Llama uses same local model; Copilot SDK doesn't accept model choice
        };

        let raw = self.call_llm(&prompt, model_override).await?;
        Ok(slugify_plan_name(&raw))
    }

    async fn call_llm(&self, prompt: &str, model_override: Option<&str>) -> Result<String> {
        match self.model_provider {
            ModelProvider::GithubModels
            | ModelProvider::Llama
            | ModelProvider::CopilotAutopilot => self.call_via_http(prompt, model_override).await,
            ModelProvider::Copilot => self.call_via_copilot_sdk(prompt).await,
        }
    }

    async fn call_via_http(&self, prompt: &str, model_override: Option<&str>) -> Result<String> {
        let token = self
            .api_token
            .as_deref()
            .context("API token is required for this model provider")?;

        let model = match model_override {
            Some(m) => m,
            None => match self.model_provider {
                ModelProvider::Llama => DEFAULT_LLAMA_MODEL,
                _ => DEFAULT_GITHUB_MODELS_MODEL,
            },
        };

        let body = serde_json::json!({
            "model": model,
            "messages": [
                { "role": "user", "content": prompt }
            ]
        });

        tracing::info!(
            "Sending planner HTTP request to {} (model: {})",
            self.api_endpoint,
            model
        );

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

/// Build the planner prompt that instructs the LLM to emit a structured task plan.
fn build_planner_prompt(goal: &str) -> String {
    format!(
        "You are a task planning assistant. Your job is to break down a high-level goal \
         into a structured list of concrete development tasks.\n\n\
         Goal: {goal}\n\n\
         Return ONLY a JSON array of task objects with NO additional text, markdown, or explanation.\n\
         Each task object must have exactly these fields:\n\
         - \"id\": a unique string identifier (e.g. \"1\", \"2\", or \"task-1\")\n\
         - \"description\": a clear, actionable description of the task\n\
         - \"phase\": an integer (>= 1) indicating the execution phase; tasks in the same phase \
           can run in parallel, lower phases run first\n\
         - \"depends_on\": (optional) an array of task ID strings that must complete before this \
           task starts\n\n\
         Example output:\n\
         [\n\
           {{\"id\": \"1\", \"description\": \"Set up project structure\", \"phase\": 1}},\n\
           {{\"id\": \"2\", \"description\": \"Implement core logic\", \"phase\": 2, \"depends_on\": [\"1\"]}},\n\
           {{\"id\": \"3\", \"description\": \"Add tests\", \"phase\": 2, \"depends_on\": [\"1\"]}}\n\
         ]\n\n\
         Output the JSON array now:",
        goal = goal,
    )
}

/// Build a prompt that asks the LLM for a short, descriptive plan name.
fn build_naming_prompt(goal: &str) -> String {
    format!(
        "Given the following project goal, produce a short identifier name (2-4 words, \
         lowercase, separated by hyphens) that captures the essence of the goal. \
         The name will be used as a filename-safe label.\n\n\
         Goal: {goal}\n\n\
         Reply with ONLY the hyphenated name and nothing else. \
         Do not include quotes, punctuation, or explanation.\n\n\
         Examples:\n\
         - Goal: \"Build a REST API for user management\" → rest-api-users\n\
         - Goal: \"Add CI/CD pipeline with GitHub Actions\" → ci-cd-pipeline\n\
         - Goal: \"Migrate database from MySQL to PostgreSQL\" → mysql-to-postgres\n\n\
         Name:",
        goal = goal,
    )
}

/// Sanitise raw LLM output into a filesystem-safe slug suitable as a ralph name.
///
/// Takes only the first line (to discard any extra commentary the LLM may
/// append), lowercases, replaces non-alphanumeric characters with hyphens,
/// collapses runs of hyphens, and caps length.
pub fn slugify_plan_name(raw: &str) -> String {
    // Take only the first non-empty line to ignore any trailing explanation.
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");

    let slug: String = first_line
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    let collapsed: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    let max_len = 40;
    if collapsed.len() > max_len {
        collapsed[..max_len].trim_end_matches('-').to_string()
    } else if collapsed.is_empty() {
        "plan".to_string()
    } else {
        collapsed
    }
}

/// Parse the raw LLM output and validate it against the [`Task`] schema.
///
/// Returns an error if:
/// - The output cannot be parsed as a JSON array.
/// - The array is empty.
/// - Any task has an empty `id` or `description`.
/// - Any task has a `phase` of 0.
/// - There are duplicate task IDs.
pub fn parse_and_validate_plan(raw: &str) -> Result<Vec<Task>> {
    let json_str = extract_json_array(raw)?;

    let entries: Vec<PlanEntry> = serde_json::from_str(&json_str)
        .context("LLM output is not a valid JSON array of task objects")?;

    if entries.is_empty() {
        bail!("LLM returned an empty task plan");
    }

    let tasks: Vec<Task> = entries
        .into_iter()
        .map(|e| {
            if e.id.is_empty() {
                bail!("Task has an empty id");
            }
            if e.description.is_empty() {
                bail!("Task '{}' has an empty description", e.id);
            }
            if e.phase == 0 {
                bail!("Task '{}' has an invalid phase 0 (must be >= 1)", e.id);
            }
            Ok(Task {
                id: e.id,
                description: e.description,
                status: TaskStatus::Pending,
                role: AgentRole::default(),
                kind: TaskKind::default(),
                cooldown_seconds: None,
                phase: e.phase,
                depends_on: e.depends_on,
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
            })
        })
        .collect::<Result<Vec<Task>>>()?;

    // Validate for duplicate IDs.
    let mut seen_ids = std::collections::HashSet::new();
    for task in &tasks {
        if !seen_ids.insert(task.id.as_str()) {
            bail!("Duplicate task ID '{}' in plan", task.id);
        }
    }

    Ok(tasks)
}

/// Extract the first JSON array from a string, stripping markdown code fences
/// if present.
fn extract_json_array(raw: &str) -> Result<String> {
    // Handle markdown code blocks (```json ... ``` or ``` ... ```).
    if let Some(fence_start) = raw.find("```") {
        let after_fence = &raw[fence_start + 3..];
        // Skip optional language tag on the opening fence line.
        let body = if let Some(nl) = after_fence.find('\n') {
            &after_fence[nl + 1..]
        } else {
            after_fence
        };
        if let Some(fence_end) = body.find("```") {
            return Ok(body[..fence_end].trim().to_string());
        }
    }

    // Fall back: find the first '[' and the matching last ']'.
    let start = raw
        .find('[')
        .context("LLM output does not contain a JSON array")?;
    let end = raw
        .rfind(']')
        .context("LLM output does not contain a valid JSON array (missing ']')")?;
    if end < start {
        bail!("LLM output JSON array delimiters are malformed");
    }
    Ok(raw[start..=end].to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- parse_and_validate_plan tests ----

    #[test]
    fn parse_valid_plan() {
        let raw = r#"[
            {"id": "1", "description": "Set up project", "phase": 1, "status": "pending"},
            {"id": "2", "description": "Add tests", "phase": 2, "depends_on": ["1"], "status": "pending"}
        ]"#;
        let tasks = parse_and_validate_plan(raw).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].id, "1");
        assert_eq!(tasks[0].phase, 1);
        assert!(tasks[0].depends_on.is_empty());
        assert_eq!(tasks[1].id, "2");
        assert_eq!(tasks[1].phase, 2);
        assert_eq!(tasks[1].depends_on, vec!["1"]);
        assert_eq!(tasks[1].status, TaskStatus::Pending);
    }

    #[test]
    fn parse_plan_wrapped_in_markdown_code_block() {
        let raw = "Here is your plan:\n```json\n[\
            {\"id\":\"1\",\"description\":\"Do thing\",\"phase\":1,\"status\":\"pending\"}\
        ]\n```\nDone.";
        let tasks = parse_and_validate_plan(raw).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "1");
    }

    #[test]
    fn parse_plan_with_plain_code_block() {
        let raw = "```\n[\
            {\"id\":\"1\",\"description\":\"Do thing\",\"phase\":1,\"status\":\"pending\"}\
        ]\n```";
        let tasks = parse_and_validate_plan(raw).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn parse_plan_defaults_phase_to_one_when_omitted() {
        // phase field omitted – should default to 1.
        let raw = r#"[{"id":"1","description":"task","status":"pending"}]"#;
        let tasks = parse_and_validate_plan(raw).unwrap();
        assert_eq!(tasks[0].phase, 1);
    }

    #[test]
    fn parse_plan_rejects_empty_array() {
        let raw = "[]";
        let err = parse_and_validate_plan(raw).unwrap_err();
        assert!(err.to_string().contains("empty task plan"));
    }

    #[test]
    fn parse_plan_rejects_invalid_json() {
        let raw = "not json at all";
        let err = parse_and_validate_plan(raw).unwrap_err();
        // Should fail because there's no JSON array.
        assert!(err.to_string().contains("JSON array"));
    }

    #[test]
    fn parse_plan_rejects_empty_id() {
        let raw = r#"[{"id":"","description":"task","phase":1,"status":"pending"}]"#;
        let err = parse_and_validate_plan(raw).unwrap_err();
        assert!(err.to_string().contains("empty id"));
    }

    #[test]
    fn parse_plan_rejects_empty_description() {
        let raw = r#"[{"id":"1","description":"","phase":1,"status":"pending"}]"#;
        let err = parse_and_validate_plan(raw).unwrap_err();
        assert!(err.to_string().contains("empty description"));
    }

    #[test]
    fn parse_plan_rejects_phase_zero() {
        let raw = r#"[{"id":"1","description":"task","phase":0,"status":"pending"}]"#;
        let err = parse_and_validate_plan(raw).unwrap_err();
        assert!(err.to_string().contains("invalid phase 0"));
    }

    #[test]
    fn parse_plan_rejects_duplicate_ids() {
        let raw = r#"[
            {"id":"1","description":"a","phase":1,"status":"pending"},
            {"id":"1","description":"b","phase":2,"status":"pending"}
        ]"#;
        let err = parse_and_validate_plan(raw).unwrap_err();
        assert!(err.to_string().contains("Duplicate task ID"));
    }

    #[test]
    fn parse_plan_all_tasks_have_pending_status() {
        let raw = r#"[
            {"id":"1","description":"task one","phase":1,"status":"pending"},
            {"id":"2","description":"task two","phase":1,"status":"pending"}
        ]"#;
        let tasks = parse_and_validate_plan(raw).unwrap();
        for task in &tasks {
            assert_eq!(task.status, TaskStatus::Pending);
        }
    }

    // ---- extract_json_array tests ----

    #[test]
    fn extract_bare_array() {
        let s = r#"[{"id":"1"}]"#;
        assert_eq!(extract_json_array(s).unwrap(), r#"[{"id":"1"}]"#);
    }

    #[test]
    fn extract_array_with_surrounding_text() {
        let s = r#"Here you go: [{"id":"1"}] That's all."#;
        assert_eq!(extract_json_array(s).unwrap(), r#"[{"id":"1"}]"#);
    }

    #[test]
    fn extract_returns_error_when_no_array() {
        let err = extract_json_array("no array here").unwrap_err();
        assert!(err.to_string().contains("JSON array"));
    }

    // ---- build_planner_prompt tests ----

    #[test]
    fn prompt_contains_goal() {
        let prompt = build_planner_prompt("Build a REST API");
        assert!(prompt.contains("Build a REST API"));
    }

    #[test]
    fn prompt_contains_required_fields() {
        let prompt = build_planner_prompt("anything");
        assert!(prompt.contains("\"id\""));
        assert!(prompt.contains("\"description\""));
        assert!(prompt.contains("\"phase\""));
        assert!(prompt.contains("\"depends_on\""));
    }

    // ---- build_naming_prompt tests ----

    #[test]
    fn naming_prompt_contains_goal() {
        let prompt = build_naming_prompt("Build a REST API");
        assert!(prompt.contains("Build a REST API"));
    }

    #[test]
    fn naming_prompt_asks_for_hyphenated_name() {
        let prompt = build_naming_prompt("anything");
        assert!(prompt.contains("hyphen"));
    }

    // ---- slugify_plan_name tests ----

    #[test]
    fn slugify_clean_llm_response() {
        assert_eq!(slugify_plan_name("rest-api-users"), "rest-api-users");
    }

    #[test]
    fn slugify_strips_whitespace_and_newlines() {
        assert_eq!(slugify_plan_name("  rest-api-users\n"), "rest-api-users");
    }

    #[test]
    fn slugify_lowercases_response() {
        assert_eq!(slugify_plan_name("REST-API-Users"), "rest-api-users");
    }

    #[test]
    fn slugify_handles_quotes_and_punctuation() {
        assert_eq!(slugify_plan_name("\"rest-api-users\""), "rest-api-users");
    }

    #[test]
    fn slugify_collapses_special_chars() {
        assert_eq!(slugify_plan_name("rest--api...users"), "rest-api-users");
    }

    #[test]
    fn slugify_truncates_long_name() {
        let long = "a-very-long-plan-name-that-exceeds-the-maximum-allowed-length-limit";
        let result = slugify_plan_name(long);
        assert!(result.len() <= 40);
        assert!(!result.ends_with('-'));
    }

    #[test]
    fn slugify_empty_response_returns_plan() {
        assert_eq!(slugify_plan_name(""), "plan");
        assert_eq!(slugify_plan_name("   "), "plan");
    }

    #[test]
    fn slugify_handles_verbose_llm_response() {
        // LLM might include extra text despite instructions; only first line is used.
        assert_eq!(
            slugify_plan_name("ci-cd-pipeline\n\nThis name captures..."),
            "ci-cd-pipeline"
        );
    }
}
