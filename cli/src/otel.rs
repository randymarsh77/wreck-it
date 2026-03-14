//! OpenTelemetry (OTEL) trace and metrics export for wreck-it's execution engine.
//!
//! # Overview
//!
//! This module provides OTLP-based observability for task lifecycle events in the
//! Ralph Wiggum loop.  When an OTLP endpoint is configured every task execution is
//! wrapped in an OTEL span with rich attributes sourced from wreck-it's existing
//! cost tracking and provenance audit trail:
//!
//! * Task `id`, `description`, `role`, `phase`, `complexity`, and `priority`
//! * Model name used for the execution
//! * `prompt_tokens` and `completion_tokens` consumed by the task
//! * Estimated cost in USD
//! * Task outcome (`success` / `failure`)
//! * Retry attempt number and whether the task was retried
//!
//! Spans are exported to any OTLP-compatible collector — Jaeger, Honeycomb, Grafana
//! Cloud, or a bare OpenTelemetry Collector — using the HTTP/protobuf transport so
//! no additional gRPC dependency (tonic) is required.
//!
//! # Configuration
//!
//! Add an `[otel]` section to your `wreck-it.toml`:
//!
//! ```toml
//! [otel]
//! endpoint = "http://localhost:4318"   # OTLP HTTP base URL
//! service_name = "my-project"          # optional, defaults to "wreck-it"
//!
//! # Optional per-header overrides, e.g. Honeycomb API key:
//! # [otel.headers]
//! # "x-honeycomb-team" = "YOUR_API_KEY"
//! ```
//!
//! When `otel` is absent or when `endpoint` is empty no spans are created.
//!
//! # Lifecycle
//!
//! 1. Call [`init_otel`] once at startup with the [`OtlpConfig`] from `Config`.
//! 2. Wrap each task execution with [`TaskSpan::start`] / [`TaskSpan::finish`].
//! 3. Call [`shutdown_otel`] when the loop exits so buffered spans are flushed.

use anyhow::{Context, Result};
use opentelemetry::global::{self, BoxedSpan};
use opentelemetry::trace::{Span, Status, Tracer};
use opentelemetry::KeyValue;
use opentelemetry_otlp::{SpanExporter, WithExportConfig, WithHttpConfig};
use opentelemetry_sdk::{runtime, trace::TracerProvider, Resource};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ── Configuration ─────────────────────────────────────────────────────────────

/// OTLP export configuration.
///
/// Placed under the `[otel]` key in `wreck-it.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq)]
pub struct OtlpConfig {
    /// Base URL of the OTLP HTTP endpoint.
    ///
    /// The traces path `/v1/traces` is appended automatically, so supply only
    /// the root URL:
    /// * Jaeger all-in-one: `http://localhost:4318`
    /// * Grafana Agent / Alloy: `http://localhost:4318`
    /// * Honeycomb: `https://api.honeycomb.io`
    pub endpoint: String,

    /// Service name recorded in every span's resource attributes.
    /// Defaults to `"wreck-it"` when absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub service_name: Option<String>,

    /// Optional HTTP headers forwarded with every export request.
    ///
    /// Common use-cases:
    /// * Honeycomb: `{"x-honeycomb-team": "YOUR_API_KEY"}`
    /// * Grafana Cloud: `{"Authorization": "Basic <base64>"}`.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub headers: HashMap<String, String>,
}

// ── Provider initialisation ───────────────────────────────────────────────────

/// Initialise the global OpenTelemetry tracer provider using the supplied
/// [`OtlpConfig`].
///
/// Returns `Ok(true)` when the provider was successfully registered, or
/// `Ok(false)` when `config.endpoint` is empty (OTEL disabled – no-op).
///
/// # Errors
///
/// Returns an error if the OTLP exporter or the tracer provider cannot be
/// constructed (e.g. the endpoint URL is malformed).
pub fn init_otel(config: &OtlpConfig) -> Result<bool> {
    if config.endpoint.is_empty() {
        return Ok(false);
    }

    let service_name = config
        .service_name
        .clone()
        .unwrap_or_else(|| "wreck-it".to_string());

    // Build the OTLP HTTP span exporter.
    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(format!(
            "{}/v1/traces",
            config.endpoint.trim_end_matches('/')
        ))
        .with_headers(config.headers.clone())
        .build()
        .context("Failed to build OTLP span exporter")?;

    // Build the SDK tracer provider with a batch exporter (non-blocking).
    let resource = Resource::new(vec![KeyValue::new(
        opentelemetry_semantic_conventions::resource::SERVICE_NAME,
        service_name,
    )]);
    let provider = TracerProvider::builder()
        .with_resource(resource)
        .with_batch_exporter(exporter, runtime::Tokio)
        .build();

    global::set_tracer_provider(provider);
    Ok(true)
}

/// Flush remaining spans and shut down the global tracer provider.
///
/// Call this once when the main loop finishes so that buffered spans that
/// have not yet been exported are flushed before the process exits.
pub fn shutdown_otel() {
    global::shutdown_tracer_provider();
}

// ── Per-task span ─────────────────────────────────────────────────────────────

/// Information about a task invocation used to populate span attributes.
#[derive(Debug, Clone, Default)]
pub struct TaskSpanAttributes {
    /// Task identifier string.
    pub task_id: String,
    /// Human-readable task description.
    pub task_description: String,
    /// Serialised agent role (e.g. `"implementer"`, `"evaluator"`).
    pub role: String,
    /// Task phase number.
    pub phase: u32,
    /// Task complexity (1–10 scale).
    pub complexity: u32,
    /// Task priority.
    pub priority: u32,
    /// Model name used for this invocation.
    pub model: String,
    /// Prompt tokens consumed (filled in on finish).
    pub prompt_tokens: u64,
    /// Completion tokens consumed (filled in on finish).
    pub completion_tokens: u64,
    /// Estimated cost in USD (filled in on finish).
    pub estimated_cost_usd: f64,
    /// Number of prior failed attempts (0 = first try).
    pub failed_attempts: u32,
}

/// An active OTEL span wrapping a single task execution.
///
/// Call [`TaskSpan::start`] when a task begins and [`TaskSpan::finish`] when
/// it ends.  If OTEL is not configured (global provider is a no-op) the
/// overhead is negligible.
pub struct TaskSpan {
    span: BoxedSpan,
}

impl TaskSpan {
    /// Start a new span for the given task attributes.
    ///
    /// The span is named `"task.execute"` and carries the task-level attributes
    /// immediately; token-count and outcome attributes are added later via
    /// [`TaskSpan::finish`].
    pub fn start(attrs: &TaskSpanAttributes) -> Self {
        let tracer = global::tracer("wreck-it");
        let mut span = tracer.start("task.execute");

        span.set_attributes([
            KeyValue::new("task.id", attrs.task_id.clone()),
            KeyValue::new("task.description", attrs.task_description.clone()),
            KeyValue::new("task.role", attrs.role.clone()),
            KeyValue::new("task.phase", attrs.phase as i64),
            KeyValue::new("task.complexity", attrs.complexity as i64),
            KeyValue::new("task.priority", attrs.priority as i64),
            KeyValue::new("task.model", attrs.model.clone()),
            KeyValue::new("task.failed_attempts", attrs.failed_attempts as i64),
        ]);

        Self { span }
    }

    /// Record the task start lifecycle event on the span.
    pub fn record_start(&mut self) {
        self.span
            .add_event("task.start", vec![KeyValue::new("event", "start")]);
    }

    /// Finish the span with outcome and cost/token attributes.
    ///
    /// `success` controls both the OTEL span status and the `task.outcome`
    /// attribute.  Token counts and estimated cost are applied as attributes
    /// so they appear in both trace UIs and OTLP consumers that support
    /// attribute-based alerting.
    ///
    /// The `attrs` parameter carries the updated token/cost counters read from
    /// `CostTracker` after the task finishes.  These values are not available
    /// at span-start time, which is why they are supplied here rather than
    /// being stored up-front.
    pub fn finish(mut self, success: bool, attrs: &TaskSpanAttributes) {
        // Token and cost attributes (known only after the task completes).
        self.span.set_attributes([
            KeyValue::new("task.prompt_tokens", attrs.prompt_tokens as i64),
            KeyValue::new("task.completion_tokens", attrs.completion_tokens as i64),
            KeyValue::new(
                "task.estimated_cost_usd",
                // Store as a string because the OTLP spec and many backend
                // UIs do not natively support floating-point attribute values.
                // Using a fixed-precision string avoids display rounding issues.
                format!("{:.6}", attrs.estimated_cost_usd),
            ),
            KeyValue::new("task.outcome", if success { "success" } else { "failure" }),
        ]);

        // Emit a completion/failure event so timeline views show clear markers.
        if success {
            self.span
                .add_event("task.complete", vec![KeyValue::new("event", "complete")]);
            self.span.set_status(Status::Ok);
        } else {
            self.span
                .add_event("task.fail", vec![KeyValue::new("event", "fail")]);
            self.span.set_status(Status::error("task failed"));
        }

        self.span.end();
    }

    /// Record a retry event on the span (called when a failed task is reset to
    /// Pending and will be attempted again).
    pub fn record_retry(&mut self, attempt: u32, max_retries: u32) {
        self.span.add_event(
            "task.retry",
            vec![
                KeyValue::new("event", "retry"),
                KeyValue::new("task.attempt", attempt as i64),
                KeyValue::new("task.max_retries", max_retries as i64),
            ],
        );
    }
}

// ── Semantic conventions helper ───────────────────────────────────────────────

// Inline the SERVICE_NAME constant so we avoid an extra crate dependency at
// compile time.  The string literal is stable across OTel specification
// versions.
mod opentelemetry_semantic_conventions {
    pub mod resource {
        pub const SERVICE_NAME: &str = "service.name";
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── OtlpConfig tests ──────────────────────────────────────────────────────

    #[test]
    fn otlp_config_default_is_disabled() {
        let cfg = OtlpConfig::default();
        assert!(cfg.endpoint.is_empty());
        assert!(cfg.service_name.is_none());
        assert!(cfg.headers.is_empty());
    }

    #[test]
    fn otlp_config_roundtrip_toml() {
        let cfg = OtlpConfig {
            endpoint: "http://localhost:4318".to_string(),
            service_name: Some("my-project".to_string()),
            headers: {
                let mut m = HashMap::new();
                m.insert("x-honeycomb-team".to_string(), "key123".to_string());
                m
            },
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        let loaded: OtlpConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded.endpoint, cfg.endpoint);
        assert_eq!(loaded.service_name, cfg.service_name);
        assert_eq!(loaded.headers, cfg.headers);
    }

    #[test]
    fn init_otel_returns_false_for_empty_endpoint() {
        let cfg = OtlpConfig::default();
        let result = init_otel(&cfg).unwrap();
        assert!(!result, "OTEL should be disabled when endpoint is empty");
    }

    // ── TaskSpan / TaskSpanAttributes tests ───────────────────────────────────

    /// Verify that constructing a TaskSpan with a no-op tracer provider (the
    /// default when OTEL is not initialised) does not panic.
    #[test]
    fn task_span_start_and_finish_no_op_tracer() {
        let attrs = TaskSpanAttributes {
            task_id: "impl-1".to_string(),
            task_description: "Implement feature X".to_string(),
            role: "implementer".to_string(),
            phase: 1,
            complexity: 5,
            priority: 2,
            model: "copilot".to_string(),
            prompt_tokens: 1000,
            completion_tokens: 200,
            estimated_cost_usd: 0.003,
            failed_attempts: 0,
        };
        // When OTEL is not initialised the global tracer is a no-op; this must
        // not panic.
        let mut span = TaskSpan::start(&attrs);
        span.record_start();
        span.finish(true, &attrs);
    }

    #[test]
    fn task_span_failure_finish_no_op_tracer() {
        let attrs = TaskSpanAttributes {
            task_id: "eval-2".to_string(),
            task_description: "Evaluate task".to_string(),
            role: "evaluator".to_string(),
            phase: 2,
            complexity: 3,
            priority: 1,
            model: "gpt-4o-mini".to_string(),
            prompt_tokens: 500,
            completion_tokens: 50,
            estimated_cost_usd: 0.0001,
            failed_attempts: 1,
        };

        let mut span = TaskSpan::start(&attrs);
        span.record_start();
        span.record_retry(1, 3);
        span.finish(false, &attrs);
    }

    #[test]
    fn otlp_config_serialises_without_optional_fields() {
        let cfg = OtlpConfig {
            endpoint: "http://localhost:4318".to_string(),
            service_name: None,
            headers: HashMap::new(),
        };
        let toml_str = toml::to_string(&cfg).unwrap();
        // Optional fields must be absent (skip_serializing_if).
        assert!(!toml_str.contains("service_name"));
        assert!(!toml_str.contains("headers"));
        assert!(toml_str.contains("localhost:4318"));
    }

    #[test]
    fn task_span_attributes_default() {
        let attrs = TaskSpanAttributes::default();
        assert_eq!(attrs.task_id, "");
        assert_eq!(attrs.phase, 0);
        assert_eq!(attrs.complexity, 0);
        assert_eq!(attrs.prompt_tokens, 0);
        assert!((attrs.estimated_cost_usd).abs() < f64::EPSILON);
    }
}
