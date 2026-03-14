//! # Error Classification and Smart Recovery
//!
//! This module defines the design for classifying task-execution errors into
//! discrete categories and selecting an appropriate recovery strategy for each
//! one.  The current wreck-it loop (`ralph_loop.rs`) treats every failure
//! identically: increment `failed_attempts`, retry up to `max_retries` times,
//! then trigger a full re-plan after `replan_threshold` consecutive failures.
//!
//! A smarter system can short-circuit that one-size-fits-all path by
//! recognising *why* a task failed and reacting accordingly.
//!
//! ## Error Categories
//!
//! | Category        | Root cause examples                                          |
//! |-----------------|--------------------------------------------------------------|
//! | `Transient`     | HTTP 429 rate-limit, network timeout, transient 503/504      |
//! | `Permanent`     | compile error, test failure, tool crash (non-zero exit)      |
//! | `NeedsReplan`   | ambiguous task description, wrong file targeted, missing dep |
//! | `ContextOverflow` | agent returned truncated output, context window exceeded   |
//!
//! ## Recovery Strategies
//!
//! * **Transient** → Exponential-backoff retry.  Do *not* count the attempt
//!   against `max_retries`; the failure is environmental, not algorithmic.
//!   Back off using `2^attempt * base_delay_ms` with an optional jitter.
//!
//! * **Permanent** → Invoke the critic with the full error output injected
//!   into the prompt.  The critic can suggest a concrete code-level fix that
//!   the next agent invocation can apply.  Only after the critic pass fails
//!   `max_retries` times should the task be marked permanently Failed.
//!
//! * **NeedsReplan** → Bypass the retry counter and trigger an immediate
//!   re-plan by setting `consecutive_failures = replan_threshold`.  This
//!   avoids wasting retries on a task that is fundamentally mis-specified.
//!
//! * **ContextOverflow** → Attempt task splitting: divide the current task
//!   into smaller sub-tasks that individually fit within the context window,
//!   then insert them into the task queue in place of the original.
//!
//! ## Integration Points
//!
//! The classifier is intended to be called from `ralph_loop.rs` inside the
//! `Err(e)` arm of the task execution match (around line 436) **before** the
//! existing retry / re-plan logic runs:
//!
//! ```text
//! Err(e) => {
//!     task_error = e.to_string();
//!     let category = ErrorClassifier::classify(&task_error);
//!     // dispatch to recovery strategy based on category …
//! }
//! ```
//!
//! It can also be called from `agent.rs` when an HTTP error is returned by
//! the Models API (around line 752) to surface a `Transient` classification
//! before the error propagates back to the loop.
//!
//! ## Heuristic Signal Sources
//!
//! The classifier works on plain-text error messages because that is what the
//! current codebase exposes (all errors are converted to `anyhow::Error` and
//! then to `String` via `.to_string()`).  A richer typed-error scheme would
//! allow more precise dispatch, but the heuristic approach is backward
//! compatible with the existing infrastructure.
//!
//! Signals used for classification:
//!
//! * HTTP status codes embedded in error strings (e.g. `"(429)"`, `"(503)"`)
//! * Keyword patterns in error text (`"timed out"`, `"rate limit"`,
//!   `"compile error"`, `"context length exceeded"`, `"truncated"`, …)
//! * Exit-code patterns surfaced by tool invocations
//!
//! ## Future Work
//!
//! * Integrate a structured `WreckItError` enum throughout `agent.rs`,
//!   `ralph_loop.rs`, and `headless.rs` so that classification is based on
//!   typed variants rather than string heuristics.
//! * Persist the classification alongside provenance data so that dashboards
//!   can surface per-category failure rates.
//! * Allow per-ralph configuration of backoff parameters and max retry counts
//!   per error category inside `RalphConfig`.

use std::time::Duration;

// ---------------------------------------------------------------------------
// Standalone classify_error function
// ---------------------------------------------------------------------------

/// Classify a task-execution failure using all available signals.
///
/// This is the primary entry point intended for use in `headless.rs` when a
/// validation command or agent interaction fails.  It combines output text,
/// process exit code, and HTTP status into a single [`ErrorCategory`].
///
/// # Classification rules (in priority order)
///
/// 1. **HTTP status** – 429 → `Transient`.
/// 2. **ContextOverflow keywords** – "context length", "max_tokens", etc.
/// 3. **Transient keywords** – "rate limit", "429", "timeout", etc.
/// 4. **Permanent keywords** – "error[E" (Rust compile errors), "FAILED".
/// 5. **Default** → `NeedsReplan` (the task description is likely the problem).
///
/// The default of `NeedsReplan` (rather than `Permanent`) is intentional:
/// when none of the above patterns match, the most likely explanation in the
/// headless flow is that the task specification needs revision.
///
/// # Arguments
///
/// * `output` – combined stdout/stderr from the failed command.
/// * `exit_code` – process exit code if available.
/// * `http_status` – HTTP response status if available (e.g., from an API call).
///
/// # Examples
///
/// ```ignore
/// # use wreck_it::error_classifier::{classify_error, ErrorCategory};
/// assert_eq!(
///     classify_error("rate limit exceeded", Some(1), None),
///     ErrorCategory::Transient,
/// );
/// assert_eq!(
///     classify_error("error[E0308]: type mismatch", Some(1), None),
///     ErrorCategory::Permanent,
/// );
/// assert_eq!(
///     classify_error("context length exceeded", Some(1), None),
///     ErrorCategory::ContextOverflow,
/// );
/// assert_eq!(
///     classify_error("unexpected situation", Some(1), None),
///     ErrorCategory::NeedsReplan,
/// );
/// ```
pub(crate) fn classify_error(
    output: &str,
    exit_code: Option<i32>,
    http_status: Option<u16>,
) -> ErrorCategory {
    let lower = output.to_lowercase();

    // HTTP status takes priority as it is the most unambiguous signal.
    if let Some(status) = http_status {
        if status == 429 {
            return ErrorCategory::Transient;
        }
    }

    // Suppress unused-variable warning: exit_code is a documented parameter
    // for future use (e.g. exit code 137 → OOM → Transient), but current
    // classification is keyword-driven.
    let _ = exit_code;

    // --- ContextOverflow signals ---
    //
    // Checked before Transient/Permanent so that a context overflow that
    // surfaces as a generic "session error" is not misclassified.
    if lower.contains("context length")
        || lower.contains("max_tokens")
        || lower.contains("context window")
        || lower.contains("maximum context")
        || lower.contains("token limit")
        || lower.contains("truncated output")
        || lower.contains("output was truncated")
    {
        return ErrorCategory::ContextOverflow;
    }

    // --- Transient signals ---
    //
    // Rate limits, network blips, and timeout keywords from the issue spec:
    // "rate limit", "429", "timeout" → Transient.
    if lower.contains("rate limit")
        || lower.contains("429")
        || lower.contains("timeout")
        || lower.contains("timed out")
        || lower.contains("connection refused")
        || lower.contains("network error")
        || lower.contains("temporarily unavailable")
        || lower.contains("(503)")
        || lower.contains("(504)")
        || lower.contains("(408)")
        || lower.contains("rate-limit")
        || lower.contains("too many requests")
    {
        return ErrorCategory::Transient;
    }

    // --- Permanent signals ---
    //
    // Deterministic failures per issue spec:
    // - "error[E" matches Rust compiler errors such as `error[E0308]: mismatched types`.
    // - "FAILED" (uppercase) matches cargo test runner output (e.g. `test foo ... FAILED`).
    //   We check the original-case output here so that lower-case "failed" (e.g.
    //   "connection failed") is not incorrectly classified as Permanent.
    if lower.contains("error[e") || output.contains("FAILED") {
        return ErrorCategory::Permanent;
    }

    // --- Default: NeedsReplan ---
    //
    // When no specific pattern matches, assume the task description is the
    // root cause and trigger an immediate re-plan.
    ErrorCategory::NeedsReplan
}

// ---------------------------------------------------------------------------
// Error category
// ---------------------------------------------------------------------------

/// Broad category of a task-execution failure used to select a recovery
/// strategy without requiring a fully-typed error hierarchy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ErrorCategory {
    /// The failure is transient and environmental (rate-limit, network blip,
    /// temporary upstream unavailability).  Retrying after a delay is likely
    /// to succeed without any change to the task or plan.
    Transient,

    /// The failure is deterministic given the current code / configuration.
    /// Retrying without a corrective prompt will produce the same outcome.
    /// The recommended recovery is to invoke the critic with the error output
    /// and let it suggest a targeted fix.
    Permanent,

    /// The task description itself is the problem: it is ambiguous, targets
    /// the wrong file, or depends on an artefact that does not exist yet.
    /// The recommended recovery is to skip straight to re-planning rather than
    /// wasting retries on an unfixable task.
    NeedsReplan,

    /// The agent's context window was exhausted, causing truncated or missing
    /// output.  The recommended recovery is to split the task into smaller
    /// chunks that individually fit within the window.
    ContextOverflow,
}

// ---------------------------------------------------------------------------
// Classifier
// ---------------------------------------------------------------------------

/// Classifies a plain-text error message into an [`ErrorCategory`].
///
/// The implementation uses heuristic keyword and pattern matching because the
/// current codebase surfaces all errors as `anyhow::Error` strings.  A future
/// refactor to a structured `WreckItError` enum would allow this to be a
/// simple match statement instead.
///
/// # Examples
///
/// ```ignore
/// # use wreck_it::error_classifier::{ErrorClassifier, ErrorCategory};
/// assert_eq!(
///     ErrorClassifier::classify("Models API returned error (429): rate limit exceeded"),
///     ErrorCategory::Transient,
/// );
/// assert_eq!(
///     ErrorClassifier::classify("Copilot session timed out after 30000ms"),
///     ErrorCategory::Transient,
/// );
/// assert_eq!(
///     ErrorClassifier::classify("compile error: expected `;`"),
///     ErrorCategory::Permanent,
/// );
/// assert_eq!(
///     ErrorClassifier::classify("context length exceeded"),
///     ErrorCategory::ContextOverflow,
/// );
/// ```
#[allow(dead_code)]
pub(crate) struct ErrorClassifier;

impl ErrorClassifier {
    /// Classify `error_text` into an [`ErrorCategory`].
    ///
    /// The matching order matters: more-specific patterns are checked before
    /// more-general ones.
    #[allow(dead_code)]
    pub(crate) fn classify(error_text: &str) -> ErrorCategory {
        let lower = error_text.to_lowercase();

        // --- ContextOverflow signals ---
        //
        // These must be checked before Transient / Permanent because a context
        // overflow often manifests as a generic "session error" which would
        // otherwise be misclassified.
        if lower.contains("context length exceeded")
            || lower.contains("context window")
            || lower.contains("maximum context")
            || lower.contains("token limit")
            || lower.contains("truncated output")
            || lower.contains("output was truncated")
        {
            return ErrorCategory::ContextOverflow;
        }

        // --- NeedsReplan signals ---
        //
        // Explicit markers that indicate the task specification is the problem,
        // not the implementation.  The re-planner is better equipped to handle
        // these than the retry / critic loop.
        if lower.contains("ambiguous task")
            || lower.contains("wrong file")
            || lower.contains("file not found")
            || lower.contains("no such file")
            || lower.contains("missing dependency")
            || lower.contains("task description")
            || lower.contains("unclear objective")
        {
            return ErrorCategory::NeedsReplan;
        }

        // --- Transient signals ---
        //
        // HTTP status codes embedded in error strings by the Models API handler
        // in `agent.rs` (format: "Models API returned error (NNN): …"):
        //   429 – rate limit
        //   408 – request timeout
        //   503 – service unavailable
        //   504 – gateway timeout
        //
        // Also covers timeout messages emitted by `ralph_loop.rs` and
        // `agent.rs` (`"timed out after NNN seconds/ms"`).
        if lower.contains("(429)")
            || lower.contains("rate limit")
            || lower.contains("rate-limit")
            || lower.contains("too many requests")
            || lower.contains("(408)")
            || lower.contains("(503)")
            || lower.contains("(504)")
            || lower.contains("timed out")
            || lower.contains("timeout")
            || lower.contains("connection refused")
            || lower.contains("network error")
            || lower.contains("temporarily unavailable")
        {
            return ErrorCategory::Transient;
        }

        // --- Permanent (default) ---
        //
        // Compile errors, test failures, tool crashes, and any other
        // deterministic failures fall here.  The critic should be invoked
        // with the error text so it can suggest a targeted fix.
        ErrorCategory::Permanent
    }
}

// ---------------------------------------------------------------------------
// Recovery strategies (stubs)
// ---------------------------------------------------------------------------

/// Parameters for exponential-backoff retry used by [`RecoveryAction::RetryAfterDelay`].
///
/// The delay before attempt *n* (0-indexed) is:
///
/// ```text
/// delay = min(base_delay * 2^n, max_delay)
/// ```
///
/// An optional `jitter_fraction` in `[0.0, 1.0]` adds up to that fraction of
/// the calculated delay as uniform random jitter to prevent thundering-herd
/// behaviour when many tasks fail simultaneously.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) struct BackoffParams {
    /// Delay before the first retry.
    pub(crate) base_delay: Duration,
    /// Upper bound on delay regardless of attempt count.
    pub(crate) max_delay: Duration,
    /// Fraction of the computed delay to add as random jitter (0.0 = no jitter).
    pub(crate) jitter_fraction: f64,
}

impl Default for BackoffParams {
    fn default() -> Self {
        Self {
            base_delay: Duration::from_secs(5),
            max_delay: Duration::from_secs(300),
            jitter_fraction: 0.1,
        }
    }
}

/// The concrete action that the loop should take for a given error category.
///
/// This is returned by [`recover`] and consumed by the caller in
/// `ralph_loop.rs` to avoid scattering recovery logic across the loop body.
#[allow(dead_code)]
#[derive(Debug, Clone)]
pub(crate) enum RecoveryAction {
    /// Wait `delay` then retry the task without incrementing `failed_attempts`.
    ///
    /// Integration point: `ralph_loop.rs` around line 604 – skip the
    /// `failed_attempts` increment and insert a `tokio::time::sleep(delay)`
    /// before resetting the task to `Pending`.
    RetryAfterDelay { delay: Duration },

    /// Invoke the critic with the provided error context injected into the
    /// prompt, then retry.
    ///
    /// Integration point: a new `invoke_critic_with_error(task, error_text)`
    /// helper should be added to `agent.rs` / `ralph_loop.rs` and called here
    /// before the task is reset to `Pending`.
    InvokeCriticThenRetry { error_context: String },

    /// Trigger an immediate re-plan bypassing the `replan_threshold` counter.
    ///
    /// Integration point: `ralph_loop.rs` around line 559 – set
    /// `consecutive_failures = replan_threshold` to trigger the existing
    /// replan branch immediately, or call `replan_and_save` directly.
    ImmediateReplan,

    /// Split the failing task into smaller sub-tasks and insert them into the
    /// queue in place of the original.
    ///
    /// Integration point: a new `split_task(task, error_text, tasks)` helper
    /// should be added (likely in `replanner.rs`) and called before the task
    /// is marked `Failed`.
    SplitTask,
}

/// Select the [`RecoveryAction`] for a given [`ErrorCategory`] and attempt
/// number.
///
/// `attempt` is the 0-indexed retry count *after* classification; callers
/// should pass `failed_attempts` from the `Task` struct.
///
/// # Design note
/// This function is intentionally pure (no I/O, no `async`) so that it can
/// be unit-tested without mocking the runtime.  Actual side-effects (sleeping,
/// HTTP calls, file writes) are the caller's responsibility.
#[allow(dead_code)]
pub(crate) fn recover(category: &ErrorCategory, attempt: u32) -> RecoveryAction {
    match category {
        // Transient: exponential backoff, do not penalise the attempt counter.
        //
        // `attempt` is used purely to compute the backoff interval; the caller
        // should *not* increment `failed_attempts` before calling this function
        // for a transient error so that the cap remains meaningful.
        ErrorCategory::Transient => {
            let params = BackoffParams::default();
            let multiplier = 1u32.checked_shl(attempt).unwrap_or(u32::MAX);
            let raw = params.base_delay.saturating_mul(multiplier);
            let capped = raw.min(params.max_delay);
            // Simple truncated-binary-exponential backoff without OS-level
            // random calls (avoids a dependency on `rand`).  Callers that want
            // jitter can add it themselves using the `jitter_fraction` field.
            RecoveryAction::RetryAfterDelay { delay: capped }
        }

        // Permanent: invoke the critic with the error output so that the next
        // agent invocation has concrete guidance on what to fix.
        ErrorCategory::Permanent => RecoveryAction::InvokeCriticThenRetry {
            error_context: String::new(), // filled in by the caller
        },

        // NeedsReplan: skip retries entirely and go straight to re-planning.
        ErrorCategory::NeedsReplan => RecoveryAction::ImmediateReplan,

        // ContextOverflow: split the task into smaller pieces.
        ErrorCategory::ContextOverflow => RecoveryAction::SplitTask,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // --- ErrorClassifier::classify ---

    #[test]
    fn classifies_429_as_transient() {
        assert_eq!(
            ErrorClassifier::classify("Models API returned error (429): rate limit exceeded"),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classifies_rate_limit_keyword_as_transient() {
        assert_eq!(
            ErrorClassifier::classify("rate limit hit, please slow down"),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classifies_504_as_transient() {
        assert_eq!(
            ErrorClassifier::classify("upstream returned (504) gateway timeout"),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classifies_session_timeout_as_transient() {
        // Matches the message emitted by agent.rs: "Copilot session timed out after Nms"
        assert_eq!(
            ErrorClassifier::classify("Copilot session timed out after 30000ms"),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classifies_task_timeout_as_transient() {
        // Matches the message emitted by ralph_loop.rs: "Task timed out after N seconds"
        assert_eq!(
            ErrorClassifier::classify("Task timed out after 120 seconds"),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classifies_context_overflow() {
        assert_eq!(
            ErrorClassifier::classify("context length exceeded, reduce input size"),
            ErrorCategory::ContextOverflow,
        );
    }

    #[test]
    fn classifies_truncated_output_as_context_overflow() {
        assert_eq!(
            ErrorClassifier::classify("output was truncated due to context window limit"),
            ErrorCategory::ContextOverflow,
        );
    }

    #[test]
    fn classifies_missing_file_as_needs_replan() {
        assert_eq!(
            ErrorClassifier::classify("no such file or directory: src/missing.rs"),
            ErrorCategory::NeedsReplan,
        );
    }

    #[test]
    fn classifies_ambiguous_task_as_needs_replan() {
        assert_eq!(
            ErrorClassifier::classify("ambiguous task: multiple matching targets found"),
            ErrorCategory::NeedsReplan,
        );
    }

    #[test]
    fn classifies_compile_error_as_permanent() {
        assert_eq!(
            ErrorClassifier::classify("compile error: expected `;` at line 42"),
            ErrorCategory::Permanent,
        );
    }

    #[test]
    fn classifies_test_failure_as_permanent() {
        assert_eq!(
            ErrorClassifier::classify("test failed: 3 assertions did not pass"),
            ErrorCategory::Permanent,
        );
    }

    #[test]
    fn classifies_unknown_error_as_permanent() {
        assert_eq!(
            ErrorClassifier::classify("something completely unexpected happened"),
            ErrorCategory::Permanent,
        );
    }

    // --- recover ---

    #[test]
    fn transient_first_attempt_uses_base_delay() {
        match recover(&ErrorCategory::Transient, 0) {
            RecoveryAction::RetryAfterDelay { delay } => {
                assert_eq!(delay, Duration::from_secs(5));
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn transient_second_attempt_doubles_delay() {
        match recover(&ErrorCategory::Transient, 1) {
            RecoveryAction::RetryAfterDelay { delay } => {
                assert_eq!(delay, Duration::from_secs(10));
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn transient_high_attempt_is_capped_at_max_delay() {
        match recover(&ErrorCategory::Transient, 20) {
            RecoveryAction::RetryAfterDelay { delay } => {
                assert_eq!(delay, Duration::from_secs(300));
            }
            other => panic!("unexpected action: {other:?}"),
        }
    }

    #[test]
    fn permanent_produces_invoke_critic() {
        assert!(matches!(
            recover(&ErrorCategory::Permanent, 0),
            RecoveryAction::InvokeCriticThenRetry { .. }
        ));
    }

    #[test]
    fn needs_replan_produces_immediate_replan() {
        assert!(matches!(
            recover(&ErrorCategory::NeedsReplan, 0),
            RecoveryAction::ImmediateReplan
        ));
    }

    #[test]
    fn context_overflow_produces_split_task() {
        assert!(matches!(
            recover(&ErrorCategory::ContextOverflow, 0),
            RecoveryAction::SplitTask
        ));
    }

    // --- classify_error ---

    #[test]
    fn classify_error_429_http_status_is_transient() {
        assert_eq!(
            classify_error("some output", Some(1), Some(429)),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classify_error_rate_limit_keyword_is_transient() {
        assert_eq!(
            classify_error("rate limit exceeded", Some(1), None),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classify_error_429_in_output_is_transient() {
        assert_eq!(
            classify_error("error (429): too many requests", Some(1), None),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classify_error_timeout_keyword_is_transient() {
        assert_eq!(
            classify_error("connection timeout after 30s", Some(1), None),
            ErrorCategory::Transient,
        );
    }

    #[test]
    fn classify_error_rust_compile_error_is_permanent() {
        assert_eq!(
            classify_error("error[E0308]: mismatched types", Some(1), None),
            ErrorCategory::Permanent,
        );
    }

    #[test]
    fn classify_error_failed_keyword_is_permanent() {
        assert_eq!(
            classify_error("FAILED: 3 tests did not pass", Some(1), None),
            ErrorCategory::Permanent,
        );
    }

    #[test]
    fn classify_error_context_length_is_context_overflow() {
        assert_eq!(
            classify_error("context length exceeded", Some(1), None),
            ErrorCategory::ContextOverflow,
        );
    }

    #[test]
    fn classify_error_max_tokens_is_context_overflow() {
        assert_eq!(
            classify_error("max_tokens reached, output truncated", Some(1), None),
            ErrorCategory::ContextOverflow,
        );
    }

    #[test]
    fn classify_error_unknown_defaults_to_needs_replan() {
        assert_eq!(
            classify_error("something completely unexpected", Some(1), None),
            ErrorCategory::NeedsReplan,
        );
    }

    #[test]
    fn classify_error_context_overflow_before_transient() {
        // A message containing both "timeout" and "context length" should be
        // ContextOverflow because that check runs first.
        assert_eq!(
            classify_error("context length exceeded after timeout", Some(1), None),
            ErrorCategory::ContextOverflow,
        );
    }

    #[test]
    fn classify_error_no_http_status_does_not_panic() {
        // Ensure None http_status is handled gracefully.
        let result = classify_error("", None, None);
        assert_eq!(result, ErrorCategory::NeedsReplan);
    }
}
