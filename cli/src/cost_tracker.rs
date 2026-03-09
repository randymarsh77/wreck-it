//! API token cost-tracking and budget-control for wreck-it.
//!
//! # Design overview
//!
//! wreck-it dispatches many LLM API calls during a run (actor, critic,
//! evaluation, precondition checks …).  Without visibility into cumulative
//! spend it is easy to exceed a budget unintentionally.  This module provides:
//!
//! 1. **[`TokenUsage`]** – a lightweight struct that carries the raw
//!    `prompt_tokens` and `completion_tokens` values returned by any
//!    OpenAI-compatible `/chat/completions` response.
//!
//! 2. **[`CostTracker`]** – a stateful accumulator that aggregates token
//!    counts and estimated cost in USD across both a single task *and* the
//!    full run.  It is shared (via `Arc<Mutex<CostTracker>>`) between the
//!    [`crate::ralph_loop::RalphLoop`] and each [`crate::agent::AgentClient`]
//!    so that parallel task execution is also captured.
//!
//! 3. **Per-model pricing constants** – approximate retail USD prices per
//!    1 million tokens for the GitHub Models and OpenAI models that wreck-it
//!    commonly uses.  Callers derive the right prices via [`model_pricing`].
//!
//! # Where token usage is captured
//!
//! Token usage is only available on the **direct HTTP** path
//! (`ModelProvider::GithubModels`).  The GitHub Copilot SDK
//! (`ModelProvider::Copilot`) and the local Llama server
//! (`ModelProvider::Llama`) do not currently expose per-call usage through
//! the SDK event stream, so those providers log a zero-usage record instead.
//!
//! The capture point is `AgentClient::chat_via_http` in `agent.rs`, which
//! parses `response.usage.prompt_tokens` and `response.usage.completion_tokens`
//! from the JSON payload and forwards them to the shared [`CostTracker`].
//!
//! # Budget control
//!
//! [`Config::max_cost_usd`](crate::types::Config::max_cost_usd) is an optional
//! upper bound on cumulative estimated spend for the entire run.  After each
//! iteration `RalphLoop::run_iteration` calls
//! [`CostTracker::budget_exceeded`]; if the limit is reached the loop returns
//! `Ok(false)` (stop) instead of continuing to the next task.
//!
//! # Cost summary output
//!
//! At the end of every iteration (both headless and TUI modes) the loop emits
//! a one-line cost summary via `state.add_log(tracker.iteration_summary())`.
//! The summary format is:
//!
//! ```text
//! Cost — task: 1234 in / 567 out / $0.0123  |  total: 12345 in / 5678 out / $0.1234
//! ```

// ── Per-model pricing constants ───────────────────────────────────────────────
//
// All values are expressed in **USD per 1 million tokens**.
// Sources: OpenAI pricing page and GitHub Models documentation (early 2025).
// Update these constants when providers change their pricing.

// OpenAI GPT-4o
/// Input price for `gpt-4o` (USD / 1M tokens).
pub const PRICE_GPT4O_INPUT: f64 = 2.50;
/// Output price for `gpt-4o` (USD / 1M tokens).
pub const PRICE_GPT4O_OUTPUT: f64 = 10.00;

// OpenAI GPT-4o-mini
/// Input price for `gpt-4o-mini` (USD / 1M tokens).
pub const PRICE_GPT4O_MINI_INPUT: f64 = 0.15;
/// Output price for `gpt-4o-mini` (USD / 1M tokens).
pub const PRICE_GPT4O_MINI_OUTPUT: f64 = 0.60;

// OpenAI o3-mini
/// Input price for `o3-mini` (USD / 1M tokens).
pub const PRICE_O3_MINI_INPUT: f64 = 1.10;
/// Output price for `o3-mini` (USD / 1M tokens).
pub const PRICE_O3_MINI_OUTPUT: f64 = 4.40;

// Anthropic Claude Opus 4 (via GitHub Models)
/// Input price for `claude-opus-4` (USD / 1M tokens).
pub const PRICE_CLAUDE_OPUS_4_INPUT: f64 = 15.00;
/// Output price for `claude-opus-4` (USD / 1M tokens).
pub const PRICE_CLAUDE_OPUS_4_OUTPUT: f64 = 75.00;

// Anthropic Claude Sonnet 4 (via GitHub Models)
/// Input price for `claude-sonnet-4` (USD / 1M tokens).
pub const PRICE_CLAUDE_SONNET_4_INPUT: f64 = 3.00;
/// Output price for `claude-sonnet-4` (USD / 1M tokens).
pub const PRICE_CLAUDE_SONNET_4_OUTPUT: f64 = 15.00;

// ── TokenUsage ────────────────────────────────────────────────────────────────

/// Raw token counts from a single LLM API response.
///
/// These map directly to the `usage` object in an OpenAI-compatible
/// `/chat/completions` response:
///
/// ```json
/// { "usage": { "prompt_tokens": 1234, "completion_tokens": 567 } }
/// ```
#[derive(Debug, Clone, Default)]
pub struct TokenUsage {
    /// Tokens in the prompt (input).
    pub prompt_tokens: u64,
    /// Tokens in the completion (output).
    pub completion_tokens: u64,
}

impl TokenUsage {
    /// Estimate the cost (in USD) of this call given per-million-token prices.
    pub fn estimate_cost(&self, input_price_per_m: f64, output_price_per_m: f64) -> f64 {
        (self.prompt_tokens as f64 / 1_000_000.0) * input_price_per_m
            + (self.completion_tokens as f64 / 1_000_000.0) * output_price_per_m
    }
}

// ── CostTracker ───────────────────────────────────────────────────────────────

/// Accumulates token usage and estimated USD cost across tasks in a run.
///
/// ## Lifecycle
///
/// 1. Create one `CostTracker` per run using [`CostTracker::new`], passing the
///    input/output prices derived from [`model_pricing`] for the active model.
/// 2. Wrap it in an `Arc<Mutex<CostTracker>>` and share it with every
///    `AgentClient` used by the run (including those spawned for parallel
///    tasks).
/// 3. Every HTTP chat completion call should invoke [`CostTracker::record`]
///    with the `TokenUsage` extracted from the API response.
/// 4. At each task boundary call [`CostTracker::reset_task`] and emit
///    [`CostTracker::iteration_summary`] to the log.
/// 5. After each iteration check [`CostTracker::budget_exceeded`] against
///    the configured `max_cost_usd` limit and abort if it returns `true`.
#[derive(Debug, Default)]
pub struct CostTracker {
    // ── per-task counters (reset at the start of each task) ─────────────────
    /// Prompt tokens consumed by the current task (all calls combined).
    pub task_prompt_tokens: u64,
    /// Completion tokens produced by the current task (all calls combined).
    pub task_completion_tokens: u64,
    /// Estimated USD cost for the current task.
    pub task_estimated_cost_usd: f64,

    // ── cumulative counters for the entire run ───────────────────────────────
    /// Total prompt tokens consumed during this run.
    pub total_prompt_tokens: u64,
    /// Total completion tokens produced during this run.
    pub total_completion_tokens: u64,
    /// Total estimated USD cost for this run.
    pub total_estimated_cost_usd: f64,

    // ── pricing parameters ───────────────────────────────────────────────────
    /// Cost per 1 million input tokens (USD).
    input_price_per_m: f64,
    /// Cost per 1 million output tokens (USD).
    output_price_per_m: f64,
}

impl CostTracker {
    /// Create a new tracker with the given per-million-token prices.
    ///
    /// Obtain the right prices for the active model via [`model_pricing`]:
    ///
    /// ```rust,ignore
    /// let (inp, out) = model_pricing("anthropic/claude-opus-4.6");
    /// let tracker = CostTracker::new(inp, out);
    /// ```
    pub fn new(input_price_per_m: f64, output_price_per_m: f64) -> Self {
        Self {
            input_price_per_m,
            output_price_per_m,
            ..Default::default()
        }
    }

    /// Record the token usage from one API call.
    ///
    /// Updates both the per-task and the cumulative run totals.
    pub fn record(&mut self, usage: &TokenUsage) {
        let call_cost = usage.estimate_cost(self.input_price_per_m, self.output_price_per_m);

        self.task_prompt_tokens += usage.prompt_tokens;
        self.task_completion_tokens += usage.completion_tokens;
        self.task_estimated_cost_usd += call_cost;

        self.total_prompt_tokens += usage.prompt_tokens;
        self.total_completion_tokens += usage.completion_tokens;
        self.total_estimated_cost_usd += call_cost;
    }

    /// Reset the per-task counters.
    ///
    /// Call this at the *start* of each new task so that the per-task summary
    /// printed at the end of the iteration reflects only that task's work.
    pub fn reset_task(&mut self) {
        self.task_prompt_tokens = 0;
        self.task_completion_tokens = 0;
        self.task_estimated_cost_usd = 0.0;
    }

    /// Return a one-line cost summary suitable for log output.
    ///
    /// Example output:
    /// ```text
    /// Cost — task: 1234 in / 567 out / $0.0123  |  total: 12345 in / 5678 out / $0.1234
    /// ```
    pub fn iteration_summary(&self) -> String {
        format!(
            "Cost — task: {} in / {} out / ${:.4}  |  total: {} in / {} out / ${:.4}",
            self.task_prompt_tokens,
            self.task_completion_tokens,
            self.task_estimated_cost_usd,
            self.total_prompt_tokens,
            self.total_completion_tokens,
            self.total_estimated_cost_usd,
        )
    }

    /// Return `true` when the cumulative estimated cost has reached or
    /// exceeded `max_cost_usd`.  Returns `false` when the limit is `None`.
    pub fn budget_exceeded(&self, max_cost_usd: Option<f64>) -> bool {
        max_cost_usd.is_some_and(|limit| self.total_estimated_cost_usd >= limit)
    }
}

// ── model_pricing ─────────────────────────────────────────────────────────────

/// Return `(input_price_per_m, output_price_per_m)` in USD for the given model
/// name string.
///
/// Matching is case-insensitive and based on substring checks against known
/// model name fragments.  Unknown models fall back to GPT-4o pricing, which
/// is a reasonable conservative estimate for typical frontier models.
///
/// # Examples
///
/// ```rust,ignore
/// let (inp, out) = model_pricing("anthropic/claude-opus-4.6");
/// // inp == PRICE_CLAUDE_OPUS_4_INPUT, out == PRICE_CLAUDE_OPUS_4_OUTPUT
/// ```
pub fn model_pricing(model: &str) -> (f64, f64) {
    let m = model.to_lowercase();
    if m.contains("claude-opus") {
        (PRICE_CLAUDE_OPUS_4_INPUT, PRICE_CLAUDE_OPUS_4_OUTPUT)
    } else if m.contains("claude-sonnet") {
        (PRICE_CLAUDE_SONNET_4_INPUT, PRICE_CLAUDE_SONNET_4_OUTPUT)
    } else if m.contains("gpt-4o-mini") || m.contains("gpt4o-mini") {
        (PRICE_GPT4O_MINI_INPUT, PRICE_GPT4O_MINI_OUTPUT)
    } else if m.contains("gpt-4o") || m.contains("gpt4o") {
        (PRICE_GPT4O_INPUT, PRICE_GPT4O_OUTPUT)
    } else if m.contains("o3-mini") {
        (PRICE_O3_MINI_INPUT, PRICE_O3_MINI_OUTPUT)
    } else {
        // Unknown model – use GPT-4o pricing as a conservative fallback.
        (PRICE_GPT4O_INPUT, PRICE_GPT4O_OUTPUT)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_usage_estimate_cost() {
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 1_000_000,
        };
        // GPT-4o: $2.50 in + $10.00 out = $12.50 per 1M each
        let cost = usage.estimate_cost(PRICE_GPT4O_INPUT, PRICE_GPT4O_OUTPUT);
        assert!((cost - 12.50).abs() < 1e-9);
    }

    #[test]
    fn cost_tracker_accumulates_across_calls() {
        let mut tracker = CostTracker::new(2.50, 10.00);
        let usage = TokenUsage {
            prompt_tokens: 500,
            completion_tokens: 100,
        };
        tracker.record(&usage);
        tracker.record(&usage);

        assert_eq!(tracker.total_prompt_tokens, 1000);
        assert_eq!(tracker.total_completion_tokens, 200);
        assert_eq!(tracker.task_prompt_tokens, 1000);
    }

    #[test]
    fn reset_task_clears_only_task_counters() {
        let mut tracker = CostTracker::new(2.50, 10.00);
        let usage = TokenUsage {
            prompt_tokens: 1000,
            completion_tokens: 200,
        };
        tracker.record(&usage);
        tracker.reset_task();

        assert_eq!(tracker.task_prompt_tokens, 0);
        assert_eq!(tracker.task_completion_tokens, 0);
        assert!((tracker.task_estimated_cost_usd).abs() < 1e-9);

        // Run totals must survive the reset.
        assert_eq!(tracker.total_prompt_tokens, 1000);
        assert_eq!(tracker.total_completion_tokens, 200);
    }

    #[test]
    fn budget_exceeded_none_never_triggers() {
        let tracker = CostTracker::default();
        assert!(!tracker.budget_exceeded(None));
    }

    #[test]
    fn budget_exceeded_triggers_at_threshold() {
        let mut tracker = CostTracker::new(2.50, 10.00);
        let usage = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 0,
        };
        tracker.record(&usage); // $2.50 total
        assert!(!tracker.budget_exceeded(Some(5.00)));
        tracker.record(&usage); // $5.00 total – exactly at limit
        assert!(tracker.budget_exceeded(Some(5.00)));
    }

    #[test]
    fn model_pricing_known_models() {
        let (inp, out) = model_pricing("anthropic/claude-opus-4.6");
        assert_eq!(inp, PRICE_CLAUDE_OPUS_4_INPUT);
        assert_eq!(out, PRICE_CLAUDE_OPUS_4_OUTPUT);

        let (inp, out) = model_pricing("openai/gpt-4o-mini");
        assert_eq!(inp, PRICE_GPT4O_MINI_INPUT);
        assert_eq!(out, PRICE_GPT4O_MINI_OUTPUT);
    }

    #[test]
    fn model_pricing_unknown_falls_back_to_gpt4o() {
        let (inp, out) = model_pricing("some-unknown-model-v99");
        assert_eq!(inp, PRICE_GPT4O_INPUT);
        assert_eq!(out, PRICE_GPT4O_OUTPUT);
    }

    /// Verify that total counters keep growing across two sequential tasks while
    /// the per-task counters are correctly reset between them.
    #[test]
    fn accumulates_across_multiple_tasks() {
        let mut tracker = CostTracker::new(PRICE_GPT4O_INPUT, PRICE_GPT4O_OUTPUT);

        // ── Task 1 ────────────────────────────────────────────────────────────
        let task1 = TokenUsage {
            prompt_tokens: 1_000,
            completion_tokens: 200,
        };
        tracker.record(&task1);
        tracker.record(&task1);

        assert_eq!(tracker.task_prompt_tokens, 2_000);
        assert_eq!(tracker.task_completion_tokens, 400);
        assert_eq!(tracker.total_prompt_tokens, 2_000);
        assert_eq!(tracker.total_completion_tokens, 400);

        tracker.reset_task();

        // ── Task 2 ────────────────────────────────────────────────────────────
        let task2 = TokenUsage {
            prompt_tokens: 500,
            completion_tokens: 100,
        };
        tracker.record(&task2);

        // Per-task counters reflect only task 2.
        assert_eq!(tracker.task_prompt_tokens, 500);
        assert_eq!(tracker.task_completion_tokens, 100);

        // Run totals must span both tasks.
        assert_eq!(tracker.total_prompt_tokens, 2_500);
        assert_eq!(tracker.total_completion_tokens, 500);

        // Cumulative cost must be positive and greater than any single task.
        assert!(tracker.total_estimated_cost_usd > tracker.task_estimated_cost_usd);
    }

    /// Verify that `estimated_cost_usd` is computed correctly for two distinct
    /// known models (GPT-4o-mini and Claude Sonnet 4) using their published
    /// per-million-token rates.
    #[test]
    fn estimated_cost_usd_two_known_models() {
        // ── GPT-4o-mini ───────────────────────────────────────────────────────
        // $0.15 / 1M input, $0.60 / 1M output
        // 2_000_000 prompt + 1_000_000 completion
        // expected = 2 * 0.15 + 1 * 0.60 = 0.30 + 0.60 = $0.90
        let usage_mini = TokenUsage {
            prompt_tokens: 2_000_000,
            completion_tokens: 1_000_000,
        };
        let cost_mini = usage_mini.estimate_cost(PRICE_GPT4O_MINI_INPUT, PRICE_GPT4O_MINI_OUTPUT);
        assert!(
            (cost_mini - 0.90).abs() < 1e-9,
            "gpt-4o-mini cost was {cost_mini}"
        );

        // ── Claude Sonnet 4 ───────────────────────────────────────────────────
        // $3.00 / 1M input, $15.00 / 1M output
        // 1_000_000 prompt + 500_000 completion
        // expected = 1 * 3.00 + 0.5 * 15.00 = 3.00 + 7.50 = $10.50
        let usage_sonnet = TokenUsage {
            prompt_tokens: 1_000_000,
            completion_tokens: 500_000,
        };
        let cost_sonnet =
            usage_sonnet.estimate_cost(PRICE_CLAUDE_SONNET_4_INPUT, PRICE_CLAUDE_SONNET_4_OUTPUT);
        assert!(
            (cost_sonnet - 10.50).abs() < 1e-9,
            "claude-sonnet-4 cost was {cost_sonnet}"
        );
    }

    #[test]
    fn iteration_summary_format() {
        let mut tracker = CostTracker::new(2.50, 10.00);
        let usage = TokenUsage {
            prompt_tokens: 1234,
            completion_tokens: 567,
        };
        tracker.record(&usage);
        let summary = tracker.iteration_summary();
        assert!(summary.contains("1234 in"));
        assert!(summary.contains("567 out"));
        assert!(summary.contains("total:"));
    }
}
