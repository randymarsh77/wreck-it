//! Cloudflare Workers backend for the log source integration.
//!
//! [Cloudflare Workers](https://developers.cloudflare.com/workers/) expose
//! invocation and error telemetry via the
//! [Workers Telemetry API](https://developers.cloudflare.com/api/resources/workers/subresources/telemetry/methods/query/).
//! This backend queries recent error events from a configured Cloudflare
//! account and script, converting them into [`LogEntry`] items that the ralph
//! loop can triage and convert into tasks.
//!
//! ## Authentication
//!
//! Cloudflare uses a **Bearer** token in the `Authorization` header.
//! Set the `api_token` field in [`LogSourceConfig`] to a Cloudflare API
//! token that has the `Workers Tail:Read` permission.
//!
//! ## Filter
//!
//! The `filter` field in [`LogSourceConfig`] is interpreted as a SQL-like
//! filter clause for the telemetry query (e.g. `"outcome = 'exception'"` or
//! `"status >= 500"`).  When omitted a default filter of
//! `"outcome = 'exception'"` is used, which captures unhandled exceptions
//! thrown by the worker.

use super::{LogEntry, LogSourceProvider};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::sync::OnceLock;

/// Default Cloudflare API base URL.
pub const DEFAULT_CF_API: &str = "https://api.cloudflare.com/client/v4";

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
fn http() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

// ---------------------------------------------------------------------------
// Cloudflare Workers Telemetry API response types
// ---------------------------------------------------------------------------

/// Envelope for the Cloudflare API v4 JSON response.
#[derive(Debug, Deserialize)]
struct CfResponse {
    success: bool,
    #[serde(default)]
    result: Vec<CfTelemetryEvent>,
    #[serde(default)]
    errors: Vec<CfApiError>,
}

/// A single error in the Cloudflare API envelope.
#[derive(Debug, Deserialize)]
struct CfApiError {
    #[serde(default)]
    message: String,
}

/// A single telemetry event from the Workers Telemetry API.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct CfTelemetryEvent {
    /// Unique event identifier.
    #[serde(default)]
    event_id: String,
    /// ISO-8601 timestamp.
    #[serde(default)]
    event_timestamp: String,
    /// Outcome of the invocation (e.g. `"ok"`, `"exception"`, `"exceededCpu"`).
    #[serde(default)]
    outcome: String,
    /// Log messages emitted during the invocation (console.log, etc.).
    #[serde(default)]
    logs: Vec<CfLogLine>,
    /// Exception messages, if any.
    #[serde(default)]
    exceptions: Vec<CfException>,
}

/// A single `console.log` / `console.error` line inside a telemetry event.
#[derive(Debug, Deserialize)]
struct CfLogLine {
    #[serde(default)]
    message: Vec<String>,
    #[serde(default)]
    level: String,
}

/// An exception captured from a worker invocation.
#[derive(Debug, Deserialize)]
struct CfException {
    #[serde(default)]
    name: String,
    #[serde(default)]
    message: String,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct CloudflareProvider {
    /// Cloudflare API token (Bearer auth).
    api_token: String,
    /// Cloudflare Account ID.
    account_id: String,
    /// Worker script name.
    script_name: String,
    /// Base URL of the Cloudflare API (overridable for testing).
    api_url: String,
    /// SQL-like filter clause for the telemetry query.
    filter: String,
}

impl CloudflareProvider {
    pub fn new(
        api_token: impl Into<String>,
        account_id: impl Into<String>,
        script_name: impl Into<String>,
        api_url: impl Into<String>,
        filter: impl Into<String>,
    ) -> Self {
        Self {
            api_token: api_token.into(),
            account_id: account_id.into(),
            script_name: script_name.into(),
            api_url: api_url.into(),
            filter: filter.into(),
        }
    }

    /// Build the rendered log message from a telemetry event.
    fn render_message(event: &CfTelemetryEvent) -> String {
        // Prefer exception messages; fall back to error-level log lines; then outcome.
        if let Some(exc) = event.exceptions.first() {
            let name = if exc.name.is_empty() {
                "Error"
            } else {
                &exc.name
            };
            return format!("{name}: {}", exc.message);
        }

        // Look for error-level log lines.
        for line in &event.logs {
            if line.level.eq_ignore_ascii_case("error") {
                return line.message.join(" ");
            }
        }

        // Fall back to a summary line.
        format!("Worker invocation outcome: {}", event.outcome)
    }

    /// Map the outcome string to a log level.
    fn level_from_outcome(outcome: &str) -> &'static str {
        match outcome {
            "exception" => "Error",
            "exceededCpu" | "exceededMemory" | "killSwitch" => "Fatal",
            "canceled" => "Warning",
            _ => "Error",
        }
    }
}

impl LogSourceProvider for CloudflareProvider {
    fn provider_name(&self) -> &str {
        "Cloudflare"
    }

    async fn query_entries(&self, since: Option<&str>, count: usize) -> Result<Vec<LogEntry>> {
        // Build URL for the Workers Telemetry API.
        // POST /accounts/{account_id}/workers/scripts/{script_name}/telemetry/events
        let url = format!(
            "{}/accounts/{}/workers/scripts/{}/telemetry/events",
            self.api_url.trim_end_matches('/'),
            urlencoding::encode(&self.account_id),
            urlencoding::encode(&self.script_name),
        );

        // Build the query body.
        let mut filters = vec![self.filter.clone()];
        if let Some(after_ts) = since {
            // Sanitise the cursor value – it is expected to be an ISO-8601
            // timestamp or event ID.  Reject anything that could alter the
            // filter clause (single-quotes, semicolons, etc.).
            let safe = after_ts
                .chars()
                .all(|c| c.is_alphanumeric() || "-:+.TZz ".contains(c));
            if safe {
                filters.push(format!("eventTimestamp > '{after_ts}'"));
            }
        }
        let filter_clause = filters.join(" AND ");

        let body = serde_json::json!({
            "limit": count,
            "filters": filter_clause,
            "orderBy": "eventTimestamp DESC",
        });

        let resp = http()
            .post(&url)
            .bearer_auth(&self.api_token)
            .json(&body)
            .send()
            .await
            .context("Cloudflare telemetry API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Cloudflare API error ({status}): {text}");
        }

        let envelope: CfResponse = resp
            .json()
            .await
            .context("Failed to parse Cloudflare telemetry response")?;

        if !envelope.success {
            let msgs: Vec<_> = envelope.errors.iter().map(|e| e.message.as_str()).collect();
            bail!("Cloudflare API returned errors: {}", msgs.join("; "));
        }

        let entries = envelope
            .result
            .iter()
            .map(|e| LogEntry {
                id: e.event_id.clone(),
                timestamp: e.event_timestamp.clone(),
                level: Self::level_from_outcome(&e.outcome).to_string(),
                message: Self::render_message(e),
            })
            .collect();

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// URL-encoding helper – tiny inline copy (same as the Seq backend).
// ---------------------------------------------------------------------------

mod urlencoding {
    use std::fmt::Write;

    pub fn encode(input: &str) -> String {
        let mut out = String::with_capacity(input.len() * 2);
        for b in input.bytes() {
            match b {
                b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                    out.push(b as char)
                }
                _ => write!(out, "%{b:02X}").unwrap(),
            }
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name_is_cloudflare() {
        let p = CloudflareProvider::new("tok", "acct", "my-worker", DEFAULT_CF_API, "true");
        assert_eq!(p.provider_name(), "Cloudflare");
    }

    #[test]
    fn default_api_url_constant() {
        assert_eq!(DEFAULT_CF_API, "https://api.cloudflare.com/client/v4");
    }

    #[test]
    fn render_message_exception() {
        let event = CfTelemetryEvent {
            event_id: "e1".into(),
            event_timestamp: "2025-01-01T00:00:00Z".into(),
            outcome: "exception".into(),
            logs: vec![],
            exceptions: vec![CfException {
                name: "TypeError".into(),
                message: "Cannot read property 'x' of undefined".into(),
            }],
        };
        assert_eq!(
            CloudflareProvider::render_message(&event),
            "TypeError: Cannot read property 'x' of undefined"
        );
    }

    #[test]
    fn render_message_exception_no_name() {
        let event = CfTelemetryEvent {
            event_id: "e2".into(),
            event_timestamp: "2025-01-01T00:00:00Z".into(),
            outcome: "exception".into(),
            logs: vec![],
            exceptions: vec![CfException {
                name: String::new(),
                message: "boom".into(),
            }],
        };
        assert_eq!(CloudflareProvider::render_message(&event), "Error: boom");
    }

    #[test]
    fn render_message_error_log_line() {
        let event = CfTelemetryEvent {
            event_id: "e3".into(),
            event_timestamp: "2025-01-01T00:00:00Z".into(),
            outcome: "ok".into(),
            logs: vec![CfLogLine {
                message: vec!["disk".into(), "full".into()],
                level: "error".into(),
            }],
            exceptions: vec![],
        };
        assert_eq!(CloudflareProvider::render_message(&event), "disk full");
    }

    #[test]
    fn render_message_fallback_outcome() {
        let event = CfTelemetryEvent {
            event_id: "e4".into(),
            event_timestamp: "2025-01-01T00:00:00Z".into(),
            outcome: "exceededCpu".into(),
            logs: vec![],
            exceptions: vec![],
        };
        assert_eq!(
            CloudflareProvider::render_message(&event),
            "Worker invocation outcome: exceededCpu"
        );
    }

    #[test]
    fn level_from_outcome_mappings() {
        assert_eq!(CloudflareProvider::level_from_outcome("exception"), "Error");
        assert_eq!(
            CloudflareProvider::level_from_outcome("exceededCpu"),
            "Fatal"
        );
        assert_eq!(
            CloudflareProvider::level_from_outcome("exceededMemory"),
            "Fatal"
        );
        assert_eq!(
            CloudflareProvider::level_from_outcome("killSwitch"),
            "Fatal"
        );
        assert_eq!(
            CloudflareProvider::level_from_outcome("canceled"),
            "Warning"
        );
        assert_eq!(CloudflareProvider::level_from_outcome("unknown"), "Error");
    }

    #[test]
    fn url_encode_special_chars() {
        assert_eq!(urlencoding::encode("abc 123"), "abc%20123");
        assert_eq!(urlencoding::encode("a/b"), "a%2Fb");
    }

    // ── HTTP integration tests (mock TCP server) ─────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spin up a minimal HTTP/1.1 server that returns the given response.
    async fn mock_server(
        status: &'static str,
        body: &'static str,
    ) -> (String, impl std::future::Future<Output = Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");
        let fut = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 32768];
            let n = stream.read(&mut buf).await.unwrap();
            buf.truncate(n);
            let resp = format!(
                "{status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            buf
        };
        (url, fut)
    }

    fn extract_request(raw: &[u8]) -> String {
        let text = std::str::from_utf8(raw).unwrap();
        text.lines().next().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn query_entries_parses_telemetry_events() {
        let body = r#"{"success":true,"result":[{"eventId":"evt-1","eventTimestamp":"2025-01-01T00:00:00Z","outcome":"exception","logs":[],"exceptions":[{"name":"RangeError","message":"out of bounds"}]},{"eventId":"evt-2","eventTimestamp":"2025-01-01T00:01:00Z","outcome":"exceededCpu","logs":[],"exceptions":[]}],"errors":[]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider =
            CloudflareProvider::new("tok", "acct-1", "my-worker", &url, "outcome = 'exception'");
        let (result, _raw) = tokio::join!(provider.query_entries(None, 10), req_fut);

        let entries = result.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "evt-1");
        assert_eq!(entries[0].level, "Error");
        assert_eq!(entries[0].message, "RangeError: out of bounds");
        assert_eq!(entries[1].id, "evt-2");
        assert_eq!(entries[1].level, "Fatal");
    }

    #[tokio::test]
    async fn query_entries_sends_bearer_auth() {
        let body = r#"{"success":true,"result":[],"errors":[]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = CloudflareProvider::new("my-secret-token", "acct", "w", &url, "true");
        let (result, raw) = tokio::join!(provider.query_entries(None, 1), req_fut);

        result.unwrap();
        let text = std::str::from_utf8(&raw).unwrap().to_lowercase();
        assert!(
            text.contains("authorization: bearer my-secret-token"),
            "bearer auth missing: {text}"
        );
    }

    #[tokio::test]
    async fn query_entries_posts_to_correct_path() {
        let body = r#"{"success":true,"result":[],"errors":[]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = CloudflareProvider::new("tok", "my-acct", "my-script", &url, "true");
        let (result, raw) = tokio::join!(provider.query_entries(None, 5), req_fut);

        result.unwrap();
        let req_line = extract_request(&raw);
        assert!(
            req_line.contains("/accounts/my-acct/workers/scripts/my-script/telemetry/events"),
            "path wrong: {req_line}"
        );
        assert!(req_line.starts_with("POST"), "should be POST: {req_line}");
    }

    #[tokio::test]
    async fn query_entries_api_error_returns_err() {
        let body = r#"{"success":false,"result":[],"errors":[{"message":"Unauthorized"}]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 401 Unauthorized", body).await;

        let provider = CloudflareProvider::new("bad", "a", "w", &url, "true");
        let (result, _) = tokio::join!(provider.query_entries(None, 10), req_fut);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("401"));
    }

    #[tokio::test]
    async fn query_entries_envelope_error_returns_err() {
        let body = r#"{"success":false,"result":[],"errors":[{"message":"Bad filter"}]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = CloudflareProvider::new("tok", "a", "w", &url, "true");
        let (result, _) = tokio::join!(provider.query_entries(None, 10), req_fut);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Bad filter"));
    }

    #[tokio::test]
    async fn query_entries_empty_returns_empty_vec() {
        let body = r#"{"success":true,"result":[],"errors":[]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = CloudflareProvider::new("tok", "a", "w", &url, "true");
        let (result, _) = tokio::join!(provider.query_entries(None, 10), req_fut);

        assert_eq!(result.unwrap().len(), 0);
    }
}
