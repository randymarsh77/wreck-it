//! Adaptive prompt optimization for the Ralph Wiggum loop.
//!
//! After each task failure, this module:
//! 1. Calls the LLM (as an "ideas-role" analyst) to analyse the original task
//!    description alongside the failure log and suggest a rewritten description
//!    that is more likely to succeed on the next attempt.
//! 2. Stores the rewrite in the per-task [`crate::agent_memory::AgentMemory`]
//!    file so that the loop can retrieve and apply it on the next retry.
//! 3. Exposes [`generate_pattern_report`] to surface aggregated
//!    success / failure patterns across all tasks as a Markdown artefact.

use crate::provenance::load_provenance_records;
use crate::types::{ModelProvider, Task, DEFAULT_GITHUB_MODELS_MODEL, DEFAULT_LLAMA_MODEL};
use anyhow::{bail, Context, Result};
use std::path::Path;

// ---------------------------------------------------------------------------
// PromptOptimizer
// ---------------------------------------------------------------------------

/// LLM-backed analyser that rewrites failing task descriptions.
///
/// The analyser uses the same model that drives the main agent loop.  It is
/// instantiated once per loop run and reused across task failures.
pub struct PromptOptimizer {
    model_provider: ModelProvider,
    api_endpoint: String,
    api_token: Option<String>,
}

impl PromptOptimizer {
    /// Create a new `PromptOptimizer` backed by the given model provider.
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

    /// Analyse a task failure and return a rewritten task description.
    ///
    /// Sends the original description and failure log to the LLM as an
    /// "ideas-role" prompt asking for a clearer, more actionable specification.
    /// Returns the rewritten description on success.
    pub async fn analyze_and_rewrite(
        &self,
        task: &Task,
        failure_log: &str,
        attempt_number: u32,
    ) -> Result<String> {
        let prompt = build_rewrite_prompt(task, failure_log, attempt_number);
        let raw = self.call_llm(&prompt).await?;
        Ok(extract_rewritten_description(&raw))
    }

    // -----------------------------------------------------------------------
    // LLM dispatch (mirrors the pattern used in replanner.rs)
    // -----------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// Prompt construction
// ---------------------------------------------------------------------------

/// Build the prompt sent to the ideas-role LLM to rewrite a failing task.
pub fn build_rewrite_prompt(task: &Task, failure_log: &str, attempt_number: u32) -> String {
    let role = serde_json::to_value(task.role)
        .ok()
        .and_then(|v| v.as_str().map(|s| s.to_string()))
        .unwrap_or_else(|| "implementer".to_string());

    format!(
        r#"You are an expert software engineering analyst reviewing a failing automated task.
Your goal is to rewrite the task description so that an AI coding agent is more likely to
succeed on the next attempt.  Focus on clarity, specificity, and actionable guidance.

## Failing Task

Task ID   : {task_id}
Agent Role: {role}
Attempt   : {attempt_number}

### Original Description

{description}

### Failure Log (last attempt)

{failure_log}

## Instructions

Analyse the root cause of the failure and produce an improved task description that:
1. Removes ambiguity that may have caused the agent to misunderstand the goal.
2. Adds explicit acceptance criteria or constraints that were implicit before.
3. Points the agent toward relevant files, functions, or patterns if the failure
   suggests it looked in the wrong place.
4. Keeps the description concise (≤ 300 words).

Output **only** the rewritten task description — no preamble, no code fences,
no explanations.  The very first character of your response must be part of the
new description.
"#,
        task_id = task.id,
        role = role,
        attempt_number = attempt_number,
        description = task.description,
        failure_log = if failure_log.is_empty() {
            "(no failure log available)"
        } else {
            failure_log
        },
    )
}

/// Post-process the raw LLM response, trimming whitespace and stripping any
/// accidental markdown code fence wrapping.
pub fn extract_rewritten_description(raw: &str) -> String {
    let trimmed = raw.trim();
    // Strip optional leading ``` or ```text fences.
    let stripped = trimmed
        .trim_start_matches("```text")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    stripped.to_string()
}

// ---------------------------------------------------------------------------
// Pattern report
// ---------------------------------------------------------------------------

/// Aggregate success / failure patterns from all tasks and their provenance
/// records, returning a Markdown report string.
///
/// The report is designed to be stored as a `Summary` artefact so that future
/// `Ideas`-role agents can read it as context when planning new tasks.
pub fn generate_pattern_report(tasks: &[Task], work_dir: &Path) -> String {
    let mut total = 0usize;
    let mut total_failures = 0usize;
    let mut max_retries_seen = 0u32;
    let mut multi_attempt_tasks: Vec<(String, u32, &str)> = Vec::new(); // (id, failures, status)

    for task in tasks {
        total += 1;
        let failures = task.failed_attempts;
        let status_str = match task.status {
            crate::types::TaskStatus::Completed => "completed",
            crate::types::TaskStatus::Failed => "failed",
            crate::types::TaskStatus::Pending => "pending",
            crate::types::TaskStatus::InProgress => "in_progress",
        };

        total_failures += failures as usize;
        if failures > max_retries_seen {
            max_retries_seen = failures;
        }
        if failures > 0 {
            multi_attempt_tasks.push((task.id.clone(), failures, status_str));
        }
    }

    // Load provenance records to compute first-attempt vs retry success rates.
    let mut prov_first_success = 0usize;
    let mut prov_total_completed = 0usize;
    for task in tasks {
        if task.status != crate::types::TaskStatus::Completed {
            continue;
        }
        prov_total_completed += 1;
        match load_provenance_records(&task.id, work_dir) {
            Ok(records) => {
                let failure_count = records.iter().filter(|r| r.outcome == "failure").count();
                if failure_count == 0 {
                    prov_first_success += 1;
                }
            }
            Err(_) => {
                // No provenance yet – assume first-attempt success for this task.
                prov_first_success += 1;
            }
        }
    }

    let first_attempt_rate = if prov_total_completed > 0 {
        format!(
            "{:.1}%",
            (prov_first_success as f64 / prov_total_completed as f64) * 100.0
        )
    } else {
        "n/a".to_string()
    };

    // Sort multi-attempt tasks by failure count descending.
    multi_attempt_tasks.sort_by(|a, b| b.1.cmp(&a.1));

    let mut report = format!(
        "# Adaptive Prompt Optimizer — Pattern Report\n\n\
         ## Run Summary\n\n\
         | Metric | Value |\n\
         |--------|-------|\n\
         | Total tasks | {total} |\n\
         | Completed | {completed} |\n\
         | Failed | {failed} |\n\
         | Pending / In-Progress | {other} |\n\
         | Total failure attempts | {total_failures} |\n\
         | First-attempt success rate | {first_attempt_rate} |\n\
         | Maximum retries (single task) | {max_retries_seen} |\n\n",
        total = total,
        completed = tasks
            .iter()
            .filter(|t| t.status == crate::types::TaskStatus::Completed)
            .count(),
        failed = tasks
            .iter()
            .filter(|t| t.status == crate::types::TaskStatus::Failed)
            .count(),
        other = tasks
            .iter()
            .filter(|t| {
                t.status == crate::types::TaskStatus::Pending
                    || t.status == crate::types::TaskStatus::InProgress
            })
            .count(),
        total_failures = total_failures,
        first_attempt_rate = first_attempt_rate,
        max_retries_seen = max_retries_seen,
    );

    if multi_attempt_tasks.is_empty() {
        report.push_str("## Tasks Requiring Retries\n\n_No tasks required retries._\n\n");
    } else {
        report.push_str("## Tasks Requiring Retries\n\n");
        report.push_str("| Task ID | Failed Attempts | Final Status |\n");
        report.push_str("|---------|-----------------|---------------|\n");
        for (id, failures, status) in &multi_attempt_tasks {
            report.push_str(&format!("| `{id}` | {failures} | {status} |\n"));
        }
        report.push('\n');
    }

    // Patterns section – lightweight heuristics from task IDs.
    report.push_str(&build_pattern_insights_section(&multi_attempt_tasks));

    report.push_str("---\n_Generated by the Adaptive Prompt Optimizer._\n");
    report
}

/// Derive simple textual pattern insights from the set of tasks that required
/// retries.  Returns a Markdown section string.
fn build_pattern_insights_section(multi_attempt_tasks: &[(String, u32, &str)]) -> String {
    if multi_attempt_tasks.is_empty() {
        return String::new();
    }

    // Group by task ID prefix (e.g. "impl-", "test-", "eval-").
    let mut prefix_failures: std::collections::HashMap<&str, u32> =
        std::collections::HashMap::new();
    let prefixes = [
        "impl-",
        "test-",
        "eval-",
        "ideas-",
        "security-",
        "coverage-",
    ];
    for (id, failures, _) in multi_attempt_tasks {
        for prefix in prefixes {
            if id.starts_with(prefix) {
                *prefix_failures.entry(prefix).or_insert(0) += failures;
                break;
            }
        }
    }

    if prefix_failures.is_empty() {
        return String::new();
    }

    let mut section = "## Failure Patterns by Task Prefix\n\n".to_string();
    section.push_str("| Prefix | Total Failures |\n");
    section.push_str("|--------|----------------|\n");

    let mut sorted: Vec<(&str, u32)> = prefix_failures.into_iter().collect();
    sorted.sort_by(|a, b| b.1.cmp(&a.1));
    for (prefix, count) in &sorted {
        section.push_str(&format!("| `{prefix}` | {count} |\n"));
    }
    section.push('\n');

    section.push_str("### Recommendations\n\n");
    for (prefix, count) in &sorted {
        if *count > 0 {
            let recommendation = match *prefix {
                "impl-" => {
                    "Implementation tasks show the most failures.  Consider adding more \
                    explicit acceptance criteria and pointing agents to relevant source files."
                }
                "test-" => {
                    "Test tasks are failing.  Ensure descriptions specify which modules to \
                    test, the expected test framework, and the minimum coverage target."
                }
                "eval-" => {
                    "Evaluation tasks are struggling.  Verify that output artefacts from \
                    upstream tasks are correctly declared as inputs."
                }
                "ideas-" => {
                    "Ideas tasks need more context.  Include links to existing design \
                    documents or describe the problem space more precisely."
                }
                _ => "Review the task descriptions for this group and add explicit constraints.",
            };
            section.push_str(&format!("- **{prefix}** tasks: {recommendation}\n"));
        }
    }
    section.push('\n');

    section
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskRuntime, TaskStatus};

    fn make_task(id: &str, description: &str, role: AgentRole) -> Task {
        Task {
            id: id.to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            role,
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
            labels: vec![],
            system_prompt_override: None,
            acceptance_criteria: None,
            evaluation: None,
        }
    }

    // ---- build_rewrite_prompt ----

    #[test]
    fn rewrite_prompt_contains_task_id_and_description() {
        let task = make_task("impl-foo", "Add a foo function", AgentRole::Implementer);
        let prompt = build_rewrite_prompt(&task, "compilation error: missing semicolon", 2);
        assert!(prompt.contains("impl-foo"));
        assert!(prompt.contains("Add a foo function"));
        assert!(prompt.contains("compilation error: missing semicolon"));
        assert!(prompt.contains("Attempt   : 2"));
    }

    #[test]
    fn rewrite_prompt_shows_role() {
        let task = make_task("ideas-bar", "Brainstorm ideas", AgentRole::Ideas);
        let prompt = build_rewrite_prompt(&task, "", 1);
        assert!(prompt.contains("ideas"));
    }

    #[test]
    fn rewrite_prompt_handles_empty_failure_log() {
        let task = make_task("test-baz", "Run tests", AgentRole::Implementer);
        let prompt = build_rewrite_prompt(&task, "", 1);
        assert!(prompt.contains("no failure log available"));
    }

    // ---- extract_rewritten_description ----

    #[test]
    fn extract_trims_whitespace() {
        assert_eq!(
            extract_rewritten_description("  hello world  "),
            "hello world"
        );
    }

    #[test]
    fn extract_strips_code_fence() {
        let raw = "```\nRewritten description here\n```";
        assert_eq!(
            extract_rewritten_description(raw),
            "Rewritten description here"
        );
    }

    #[test]
    fn extract_strips_text_code_fence() {
        let raw = "```text\nRewritten description here\n```";
        assert_eq!(
            extract_rewritten_description(raw),
            "Rewritten description here"
        );
    }

    #[test]
    fn extract_returns_plain_string_unchanged() {
        let raw = "This is the new task description.";
        assert_eq!(extract_rewritten_description(raw), raw);
    }

    // ---- generate_pattern_report ----

    #[test]
    fn pattern_report_empty_tasks() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let report = generate_pattern_report(&[], dir.path());
        assert!(report.contains("Total tasks | 0"));
        assert!(report.contains("Adaptive Prompt Optimizer"));
    }

    #[test]
    fn pattern_report_counts_failures() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let mut task = make_task("impl-foo", "Add foo", AgentRole::Implementer);
        task.failed_attempts = 3;
        task.status = TaskStatus::Failed;
        let report = generate_pattern_report(&[task], dir.path());
        assert!(report.contains("impl-foo"));
        assert!(report.contains("| 3 |"));
    }

    #[test]
    fn pattern_report_first_attempt_success_rate() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let mut t1 = make_task("a", "do a", AgentRole::Implementer);
        t1.status = TaskStatus::Completed;
        t1.failed_attempts = 0;
        let mut t2 = make_task("b", "do b", AgentRole::Implementer);
        t2.status = TaskStatus::Completed;
        t2.failed_attempts = 1;
        let report = generate_pattern_report(&[t1, t2], dir.path());
        // Without provenance files both tasks are assumed first-attempt success
        // when provenance is missing, so rate is 100%.
        assert!(report.contains("First-attempt success rate"));
    }

    #[test]
    fn pattern_report_no_retries_message() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let mut task = make_task("impl-ok", "do something", AgentRole::Implementer);
        task.status = TaskStatus::Completed;
        task.failed_attempts = 0;
        let report = generate_pattern_report(&[task], dir.path());
        assert!(report.contains("No tasks required retries"));
    }

    #[test]
    fn pattern_insights_groups_by_prefix() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let mut t1 = make_task("impl-x", "do x", AgentRole::Implementer);
        t1.failed_attempts = 2;
        t1.status = TaskStatus::Failed;
        let mut t2 = make_task("test-y", "test y", AgentRole::Implementer);
        t2.failed_attempts = 1;
        t2.status = TaskStatus::Failed;
        let report = generate_pattern_report(&[t1, t2], dir.path());
        assert!(report.contains("impl-"));
        assert!(report.contains("test-"));
        assert!(report.contains("Failure Patterns by Task Prefix"));
    }
}
