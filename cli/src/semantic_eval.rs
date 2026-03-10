//! Semantic evaluation mode for wreck-it tasks.
//!
//! # Overview
//!
//! `semantic` is a third [`EvaluationMode`][crate::types::EvaluationMode] that
//! complements the existing `command` and `agent_file` modes.  Instead of
//! relying on a shell command exit code or on the agent creating a marker file,
//! the evaluator is prompted to read:
//!
//! * The original task description (what the task was supposed to accomplish).
//! * The acceptance criteria attached to the task (if any).
//! * A summary of the git diff produced by the implementation (what actually
//!   changed).
//!
//! The evaluator returns a **structured JSON verdict**:
//!
//! ```json
//! {
//!   "passed": true,
//!   "score":  85,
//!   "rationale": "All required files were created and the tests pass."
//! }
//! ```
//!
//! * `passed`    — Boolean.  `true` when the implementation is considered
//!   complete and correct; `false` otherwise.
//! * `score`     — Unsigned 8-bit integer (0–100).  A quantitative quality
//!   signal: 0 means the task was not attempted at all, 100 means
//!   a perfect implementation.  The score is surfaced in the TUI
//!   and persisted to logs so trends can be tracked over time.
//! * `rationale` — Free-text explanation of the verdict.  Surfaced in the TUI
//!   task detail panel and written to the structured log so
//!   operators can understand *why* a task passed or failed
//!   without having to read the raw diff themselves.
//!
//! # Prompt design
//!
//! The evaluation prompt is constructed in [`build_semantic_eval_prompt`].  It
//! intentionally provides the evaluator with three sections, each clearly
//! delimited so the model can reliably locate them:
//!
//! 1. **Task description** — the `description` field of the [`Task`].
//! 2. **Acceptance criteria** — the `completeness_prompt` from the agent
//!    configuration, if present (e.g. "all unit tests must pass", "the README
//!    must be updated", …).  When absent, the evaluator is told to use its
//!    general judgement.
//! 3. **Git diff summary** — the output of `git diff HEAD` in the work
//!    directory, trimmed to a configurable maximum length to avoid exceeding
//!    the model's context window.  When the diff is empty (nothing changed) the
//!    evaluator is told explicitly so it can penalise no-op implementations.
//!
//! The prompt closes with an explicit JSON schema instruction so the model
//! knows the *exact* shape it must return.
//!
//! # Parsing
//!
//! [`parse_semantic_verdict`] attempts to extract the JSON object from the raw
//! LLM response.  Parsing is deliberately lenient:
//!
//! * Markdown code fences (` ``` `) are stripped before extraction.
//! * The first `{` … last `}` substring is treated as the JSON payload,
//!   allowing the model to emit surrounding prose without breaking parsing.
//!
//! # Fallback behaviour
//!
//! When the evaluator returns malformed output (no JSON, missing fields, wrong
//! types) the verdict defaults to **failed** with score 0 and a rationale
//! that explains the parse failure.  This conservative fallback ensures that a
//! model-side error does not silently mark a task as complete.
//!
//! Specifically:
//!
//! * If the response is empty or contains no `{…}` block → `passed: false`,
//!   `score: 0`, `rationale: "Evaluator returned no structured verdict."`.
//! * If the JSON parses but `passed` is missing → `passed: false`.
//! * If `score` is missing or out of range → clamped to 0.
//! * If `rationale` is missing → a placeholder string is used.
//!
//! # TUI / log surfacing
//!
//! The caller ([`crate::ralph_loop::TaskScheduler::evaluate_task`]) receives
//! the full [`SemanticVerdict`] and is responsible for:
//!
//! * Logging the verdict via `tracing::info!` / `tracing::warn!`.
//! * Appending the rationale to `LoopState::logs` so it appears in the TUI
//!   event stream (the TUI reads logs from `LoopState`).
//! * Returning `verdict.passed` as the boolean evaluation result that drives
//!   the task lifecycle state machine.

use crate::types::Task;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

/// Maximum number of UTF-8 characters included from the git diff in the
/// evaluation prompt.  Diffs beyond this limit are truncated with a notice so
/// the model is not surprised by the cut-off.
pub const MAX_DIFF_CHARS: usize = 8_000;

/// The structured verdict returned by the semantic evaluator.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SemanticVerdict {
    /// `true` when the implementation is considered complete and correct.
    pub passed: bool,
    /// Quality score from 0 (not attempted) to 100 (perfect).
    pub score: u8,
    /// Human-readable explanation of the verdict.
    pub rationale: String,
}

impl SemanticVerdict {
    /// Conservative fallback verdict used when parsing fails.
    pub fn parse_failure(reason: impl Into<String>) -> Self {
        SemanticVerdict {
            passed: false,
            score: 0,
            rationale: format!(
                "Semantic evaluator returned malformed output: {}",
                reason.into()
            ),
        }
    }
}

/// Build the evaluation prompt sent to the LLM.
///
/// The prompt is structured into clearly delimited sections so the model can
/// locate each piece of context reliably.
///
/// # Parameters
/// * `task`                — The task being evaluated.
/// * `completeness_prompt` — Optional acceptance-criteria string from the
///   agent configuration.
/// * `diff`                — Output of `git diff HEAD` in the work directory.
pub fn build_semantic_eval_prompt(
    task: &Task,
    completeness_prompt: Option<&str>,
    diff: &str,
) -> String {
    // Trim the diff to avoid blowing the context window.
    let diff_section = if diff.is_empty() {
        "No changes detected (empty diff).".to_string()
    } else if diff.len() > MAX_DIFF_CHARS {
        format!(
            "{}\n\n[diff truncated — {} chars omitted]",
            &diff[..MAX_DIFF_CHARS],
            diff.len() - MAX_DIFF_CHARS
        )
    } else {
        diff.to_string()
    };

    let criteria_section = completeness_prompt
        .map(|p| format!("Acceptance criteria:\n{p}"))
        .unwrap_or_else(|| {
            "Acceptance criteria: (none provided — use your general judgement)".to_string()
        });

    format!(
        "You are a task-completion evaluator for a software engineering agent.\n\
         Your job is to decide whether the implementation described by the git diff \
         below correctly and fully addresses the task.\n\n\
         ## Task description\n{task_desc}\n\n\
         ## {criteria}\n\n\
         ## Git diff\n```\n{diff}\n```\n\n\
         Respond ONLY with a single JSON object matching this schema (no other text):\n\
         {{\"passed\": <true|false>, \"score\": <integer 0-100>, \"rationale\": \"<string>\"}}\n\
         - passed:   true if the implementation adequately addresses the task\n\
         - score:    0 (nothing done) to 100 (perfect implementation)\n\
         - rationale: one or two sentences explaining your verdict",
        task_desc = task.description,
        criteria = criteria_section,
        diff = diff_section,
    )
}

/// Parse a [`SemanticVerdict`] from a raw LLM response string.
///
/// Handles raw JSON as well as JSON embedded in markdown code fences.
/// Returns a conservative failure verdict instead of an error when the
/// response is malformed — see the module-level docs for the fallback policy.
pub fn parse_semantic_verdict(response: &str) -> SemanticVerdict {
    // Strip markdown code fences.
    let stripped: String = response
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("```")
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Find the first `{` … last `}` block.
    let Some(start) = stripped.find('{') else {
        return SemanticVerdict::parse_failure("no JSON object found in response");
    };
    let Some(end_offset) = stripped.rfind('}') else {
        return SemanticVerdict::parse_failure("no closing brace found in response");
    };
    let json_part = &stripped[start..=end_offset];

    match serde_json::from_str::<SemanticVerdict>(json_part) {
        Ok(verdict) => verdict,
        Err(e) => SemanticVerdict::parse_failure(e.to_string()),
    }
}

/// Run a semantic evaluation against a git diff.
///
/// This is a testable wrapper that:
/// 1. Builds the evaluation prompt via [`build_semantic_eval_prompt`].
/// 2. Calls the provided `chat_fn` to get the LLM response.
/// 3. Parses the response into a [`SemanticVerdict`] via
///    [`parse_semantic_verdict`].
///
/// The `chat_fn` parameter accepts a prompt string and returns the raw model
/// response.  In production this is wired to
/// `AgentClient::chat_via_http` / `AgentClient::critique_via_copilot`.
/// In tests it can be replaced with a stub that returns canned JSON.
///
/// The caller is responsible for collecting the git diff before calling this
/// function (see `AgentClient::get_git_diff` in production).
pub async fn evaluate_semantically<F, Fut>(
    task: &Task,
    diff: &str,
    completeness_prompt: Option<&str>,
    chat_fn: F,
) -> Result<SemanticVerdict>
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<String>>,
{
    // Build prompt.
    let prompt = build_semantic_eval_prompt(task, completeness_prompt, diff);

    // Call the LLM.
    let response = chat_fn(prompt)
        .await
        .context("Semantic evaluator: chat request failed")?;

    tracing::info!("Semantic evaluation raw response: {}", response);

    // Parse the verdict — never fails; uses conservative fallback on error.
    let verdict = parse_semantic_verdict(&response);

    tracing::info!(
        "Semantic verdict: passed={}, score={}, rationale={}",
        verdict.passed,
        verdict.score,
        verdict.rationale
    );

    Ok(verdict)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskRuntime, TaskStatus};

    fn make_task(description: &str) -> Task {
        Task {
            id: "test-task".to_string(),
            description: description.to_string(),
            status: TaskStatus::Pending,
            role: AgentRole::Evaluator,
            kind: TaskKind::Milestone,
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
            runtime: TaskRuntime::Local,
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
            system_prompt_override: None,
        }
    }

    // ── parse_semantic_verdict ────────────────────────────────────────────

    #[test]
    fn parse_valid_passed_verdict() {
        let json = r#"{"passed": true, "score": 90, "rationale": "All tests pass."}"#;
        let verdict = parse_semantic_verdict(json);
        assert!(verdict.passed);
        assert_eq!(verdict.score, 90);
        assert_eq!(verdict.rationale, "All tests pass.");
    }

    #[test]
    fn parse_valid_failed_verdict() {
        let json = r#"{"passed": false, "score": 30, "rationale": "Missing implementation."}"#;
        let verdict = parse_semantic_verdict(json);
        assert!(!verdict.passed);
        assert_eq!(verdict.score, 30);
        assert_eq!(verdict.rationale, "Missing implementation.");
    }

    #[test]
    fn parse_verdict_strips_markdown_fences() {
        let response = "```json\n{\"passed\": true, \"score\": 75, \"rationale\": \"Good.\"}\n```";
        let verdict = parse_semantic_verdict(response);
        assert!(verdict.passed);
        assert_eq!(verdict.score, 75);
    }

    #[test]
    fn parse_verdict_with_surrounding_prose() {
        let response =
            "Here is my evaluation:\n{\"passed\": false, \"score\": 10, \"rationale\": \"Bad.\"}";
        let verdict = parse_semantic_verdict(response);
        assert!(!verdict.passed);
        assert_eq!(verdict.score, 10);
    }

    #[test]
    fn parse_verdict_fallback_on_empty_response() {
        let verdict = parse_semantic_verdict("");
        assert!(!verdict.passed);
        assert_eq!(verdict.score, 0);
        assert!(verdict.rationale.contains("malformed"));
    }

    #[test]
    fn parse_verdict_fallback_on_missing_braces() {
        let verdict = parse_semantic_verdict("passed true score 90");
        assert!(!verdict.passed);
        assert_eq!(verdict.score, 0);
    }

    #[test]
    fn parse_verdict_fallback_on_invalid_json() {
        let verdict = parse_semantic_verdict("{not valid json}");
        assert!(!verdict.passed);
        assert_eq!(verdict.score, 0);
        assert!(verdict.rationale.contains("malformed"));
    }

    // ── build_semantic_eval_prompt ────────────────────────────────────────

    #[test]
    fn prompt_includes_task_description() {
        let task = make_task("Implement the frobnicate feature");
        let prompt = build_semantic_eval_prompt(&task, None, "diff content");
        assert!(prompt.contains("Implement the frobnicate feature"));
    }

    #[test]
    fn prompt_includes_acceptance_criteria_when_provided() {
        let task = make_task("Do something");
        let prompt =
            build_semantic_eval_prompt(&task, Some("All unit tests must pass"), "diff content");
        assert!(prompt.contains("All unit tests must pass"));
    }

    #[test]
    fn prompt_uses_general_judgement_when_no_criteria() {
        let task = make_task("Do something");
        let prompt = build_semantic_eval_prompt(&task, None, "diff content");
        assert!(prompt.contains("general judgement"));
    }

    #[test]
    fn prompt_notes_empty_diff() {
        let task = make_task("Do something");
        let prompt = build_semantic_eval_prompt(&task, None, "");
        assert!(prompt.contains("empty diff"));
    }

    #[test]
    fn prompt_truncates_large_diff() {
        let task = make_task("Do something");
        let big_diff = "x".repeat(MAX_DIFF_CHARS + 100);
        let prompt = build_semantic_eval_prompt(&task, None, &big_diff);
        assert!(prompt.contains("truncated"));
    }

    // ── evaluate_semantically (async, stub chat_fn) ───────────────────────

    #[tokio::test]
    async fn evaluate_semantically_returns_verdict_from_chat_fn() {
        let task = make_task("Implement feature X");

        // Stub chat function always returns a valid verdict.
        let canned_response =
            r#"{"passed": true, "score": 88, "rationale": "Looks great."}"#.to_string();
        let chat_fn = move |_prompt: String| {
            let resp = canned_response.clone();
            async move { Ok(resp) }
        };

        let verdict = evaluate_semantically(&task, "some diff content", None, chat_fn)
            .await
            .unwrap();

        assert!(verdict.passed);
        assert_eq!(verdict.score, 88);
        assert_eq!(verdict.rationale, "Looks great.");
    }
}
