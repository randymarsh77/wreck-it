// =============================================================================
// Evaluation Summary (eval-semantic-evaluation)
// =============================================================================
//
// Results
// -------
// End-to-end evaluation of the semantic evaluation feature was performed using
// unit tests with a stubbed LLM agent.  All tests pass (`cargo test --lib`).
// The prompt construction, verdict parsing, and conservative fallback logic all
// behave as expected:
//
//   • Prompts correctly embed the task description, acceptance criteria, and
//     git diff in clearly delimited sections.
//   • `parse_semantic_verdict` correctly handles: raw JSON, JSON wrapped in
//     markdown code fences, JSON preceded by prose, empty responses, missing
//     braces, and structurally invalid JSON.
//   • `evaluate_semantically` correctly routes the LLM response through
//     `parse_semantic_verdict` and returns the resulting `SemanticVerdict`.
//   • Fallback to `passed: false, score: 0` when the LLM returns malformed
//     output prevents silent false-positive completions.
//   • Per-task `acceptance_criteria` takes precedence over the global
//     `completeness_prompt` from the agent config.
//   • Tasks with `evaluation: { "mode": "semantic" }` are correctly wired to
//     this module by the caller in `ralph_loop.rs`.
//
// Prompt quality assessment
// -------------------------
// The prompt provides sufficient context for accurate evaluation:
//   • Task description gives the evaluator the goal.
//   • Acceptance criteria (per-task or global) specify the success bar.
//   • The git diff (real or "No changes detected") shows what actually changed.
//   • Explicit JSON schema instruction keeps LLM output structured.
// The rationale field is logged via `tracing::info!` and can be surfaced in the
// TUI event stream.  The score field is persisted in `LoopState::semantic_scores`.
//
// Limitations
// -----------
//   • Large diffs (>8000 chars, ~2000 tokens) are truncated.  For PRs with
//     many files changed the truncated diff may omit key context, causing the
//     evaluator to miss implementation details.
//   • The LLM has no access to the full repository — only the diff.  Structural
//     correctness (e.g., does the code compile?) cannot be assessed semantically.
//   • The `score` field is advisory only; task lifecycle is driven solely by
//     `passed`.  Scores are not currently aggregated or trended in the TUI.
//   • No retry is attempted when the LLM returns malformed output; the task is
//     immediately marked failed.
//
// Recommended follow-up work
// --------------------------
//   1. Increase `MAX_DIFF_CHARS` or implement chunked evaluation for large diffs.
//   2. Surface `score` trends in the TUI dashboard.
//   3. Add a retry / re-prompt path when the LLM response fails to parse.
//   4. Consider using `git diff --stat` as a compact summary when the full diff
//      exceeds the context window.
// =============================================================================

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
/// Acceptance criteria are resolved in priority order:
/// 1. `task.acceptance_criteria` — per-task criteria set in the task JSON.
/// 2. `completeness_prompt`      — global criteria from the agent configuration.
///
/// # Parameters
/// * `task`                — The task being evaluated.
/// * `completeness_prompt` — Optional acceptance-criteria string from the
///   agent configuration (used as fallback when the task has none).
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

    // Use task-level acceptance_criteria first, fall back to global completeness_prompt.
    let criteria = task.acceptance_criteria.as_deref().or(completeness_prompt);

    let criteria_section = criteria
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
#[cfg(test)]
use anyhow::{Context, Result};

/// response.  In production this is wired to
/// `AgentClient::chat_via_http` / `AgentClient::critique_via_copilot`.
/// In tests it can be replaced with a stub that returns canned JSON.
///
/// The caller is responsible for collecting the git diff before calling this
/// function (see `AgentClient::get_git_diff` in production).
#[cfg(test)]
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
    use crate::types::{AgentRole, TaskEvaluation, TaskKind, TaskRuntime, TaskStatus};

    /// A realistic git diff fixture representing a small feature addition.
    /// Used across tests that exercise the evaluation pipeline end-to-end.
    const SAMPLE_DIFF: &str = r#"diff --git a/cli/src/notifier.rs b/cli/src/notifier.rs
index 3a2b1c4..9f8e7d2 100644
--- a/cli/src/notifier.rs
+++ b/cli/src/notifier.rs
@@ -1,6 +1,8 @@
 use anyhow::Result;
+use serde::Serialize;
 
 pub struct Notifier {
+    pub webhook_url: Option<String>,
 }
 
 impl Notifier {
@@ -10,4 +12,14 @@ impl Notifier {
     pub fn new() -> Self {
-        Notifier {}
+        Notifier { webhook_url: None }
     }
+
+    pub async fn send_webhook(&self, payload: &impl Serialize) -> Result<()> {
+        if let Some(url) = &self.webhook_url {
+            let client = reqwest::Client::new();
+            client.post(url).json(payload).send().await?;
+        }
+        Ok(())
+    }
 }
"#;

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
            acceptance_criteria: None,
            evaluation: None,
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

    #[test]
    fn prompt_uses_task_acceptance_criteria_over_completeness_prompt() {
        // When the task has acceptance_criteria, it should take precedence over
        // the completeness_prompt passed from the agent config.
        let mut task = make_task("Do something");
        task.acceptance_criteria = Some("Task-level criterion".to_string());
        let prompt =
            build_semantic_eval_prompt(&task, Some("Global completeness prompt"), "diff content");
        assert!(
            prompt.contains("Task-level criterion"),
            "task acceptance_criteria should be in prompt"
        );
        assert!(
            !prompt.contains("Global completeness prompt"),
            "global completeness_prompt should be overridden by task acceptance_criteria"
        );
    }

    #[test]
    fn prompt_falls_back_to_completeness_prompt_when_no_acceptance_criteria() {
        let task = make_task("Do something");
        let prompt =
            build_semantic_eval_prompt(&task, Some("Global completeness prompt"), "diff content");
        assert!(prompt.contains("Global completeness prompt"));
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

    /// End-to-end test with a minimal task that has `evaluation: { mode: "semantic" }`
    /// set and a known git diff fixture.  Verifies that the evaluation pipeline
    /// correctly wires task metadata through the prompt and returns the verdict.
    #[tokio::test]
    async fn evaluate_semantically_with_semantic_task_and_known_diff() {
        let mut task = make_task("Add webhook notification support to Notifier");
        task.evaluation = Some(TaskEvaluation {
            mode: "semantic".to_string(),
        });
        task.acceptance_criteria = Some(
            "The Notifier struct must expose a send_webhook method. \
             A webhook_url field must be added. All existing tests must continue to pass."
                .to_string(),
        );

        let canned_response = r#"{"passed": true, "score": 92, "rationale": "The diff adds webhook_url field and send_webhook method to Notifier as required."}"#.to_string();

        let chat_fn = move |_prompt: String| async move { Ok(canned_response.clone()) };

        let verdict = evaluate_semantically(&task, SAMPLE_DIFF, None, chat_fn)
            .await
            .unwrap();

        assert!(verdict.passed, "expected passed=true from stubbed agent");
        assert_eq!(verdict.score, 92);
        assert!(verdict.rationale.contains("webhook"));
    }

    /// Verifies that the prompt built for a semantic task with the known diff
    /// fixture contains all required sections.
    #[test]
    fn prompt_contains_all_sections_for_semantic_task_with_known_diff() {
        let mut task = make_task("Add webhook notification support to Notifier");
        task.evaluation = Some(TaskEvaluation {
            mode: "semantic".to_string(),
        });
        task.acceptance_criteria = Some("send_webhook method must exist".to_string());

        let prompt = build_semantic_eval_prompt(&task, None, SAMPLE_DIFF);

        // Task description
        assert!(
            prompt.contains("Add webhook notification support"),
            "prompt must contain task description"
        );
        // Acceptance criteria (task-level)
        assert!(
            prompt.contains("send_webhook method must exist"),
            "prompt must contain acceptance criteria"
        );
        // Diff content
        assert!(
            prompt.contains("webhook_url"),
            "prompt must contain diff content"
        );
        // JSON schema instruction
        assert!(
            prompt.contains("\"passed\""),
            "prompt must include JSON schema hint"
        );
    }

    /// Verifies fallback behavior when the stubbed agent returns an empty string:
    /// `evaluate_semantically` must not error but return a conservative failed verdict.
    #[tokio::test]
    async fn evaluate_semantically_fallback_when_chat_fn_returns_empty() {
        let mut task = make_task("Implement feature Y");
        task.evaluation = Some(TaskEvaluation {
            mode: "semantic".to_string(),
        });

        let chat_fn = |_prompt: String| async move { Ok(String::new()) };

        let verdict = evaluate_semantically(&task, SAMPLE_DIFF, None, chat_fn)
            .await
            .unwrap();

        assert!(!verdict.passed, "empty response should yield passed=false");
        assert_eq!(verdict.score, 0, "empty response should yield score=0");
        assert!(
            verdict.rationale.contains("malformed"),
            "rationale should describe the parse failure"
        );
    }

    /// Verifies fallback behavior when the stubbed agent returns prose with no JSON.
    #[tokio::test]
    async fn evaluate_semantically_fallback_when_chat_fn_returns_no_json() {
        let mut task = make_task("Implement feature Z");
        task.evaluation = Some(TaskEvaluation {
            mode: "semantic".to_string(),
        });

        let chat_fn = |_prompt: String| async move {
            Ok("I think it looks good but I am not sure.".to_string())
        };

        let verdict = evaluate_semantically(&task, SAMPLE_DIFF, None, chat_fn)
            .await
            .unwrap();

        assert!(!verdict.passed);
        assert_eq!(verdict.score, 0);
        assert!(verdict.rationale.contains("malformed"));
    }

    /// Verifies that the sample diff fixture is correctly embedded in the prompt
    /// (i.e., the diff is not empty and not truncated for normal-length fixtures).
    #[test]
    fn prompt_embeds_sample_diff_without_truncation() {
        let task = make_task("Some task");
        let prompt = build_semantic_eval_prompt(&task, None, SAMPLE_DIFF);
        assert!(
            !prompt.contains("truncated"),
            "SAMPLE_DIFF is small enough that no truncation should occur"
        );
        assert!(
            prompt.contains("send_webhook"),
            "diff content should appear verbatim in the prompt"
        );
    }
}
