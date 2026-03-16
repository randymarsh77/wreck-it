//! Seq backend for the log source integration.
//!
//! [Seq](https://datalust.co/seq) is a structured log server with a rich
//! query API.  This backend queries events matching a configurable filter
//! and severity level, converting them into [`LogEntry`] items that the
//! ralph loop can triage and convert into tasks.
//!
//! ## Authentication
//!
//! Seq supports API keys passed via the `X-Seq-ApiKey` header.  Set the
//! `api_token` field in [`LogSourceConfig`] to the key value.

use super::{LogEntry, LogSourceProvider};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::sync::OnceLock;

/// Default Seq API base URL (local development instance).
pub const DEFAULT_SEQ_API: &str = "http://localhost:5341";

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
fn http() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

// ---------------------------------------------------------------------------
// Seq API response types
// ---------------------------------------------------------------------------

/// A single event returned by the Seq `/api/events` endpoint.
#[derive(Debug, Deserialize)]
#[serde(rename_all = "PascalCase")]
struct SeqEvent {
    /// Unique event id assigned by Seq.
    id: String,
    /// ISO-8601 timestamp of the event.
    timestamp: String,
    /// Severity level name (e.g. `"Error"`, `"Warning"`, `"Fatal"`).
    #[serde(default)]
    level: String,
    /// Rendered message template.
    #[serde(default)]
    rendered_message: String,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct SeqProvider {
    /// API key for `X-Seq-ApiKey` header.  May be empty for open instances.
    api_key: String,
    /// Base URL of the Seq instance (e.g. `http://localhost:5341`).
    api_url: String,
    /// Seq filter expression (e.g. `@Level = 'Error'`).
    filter: String,
}

impl SeqProvider {
    pub fn new(
        api_key: impl Into<String>,
        api_url: impl Into<String>,
        filter: impl Into<String>,
    ) -> Self {
        Self {
            api_key: api_key.into(),
            api_url: api_url.into(),
            filter: filter.into(),
        }
    }
}

impl LogSourceProvider for SeqProvider {
    fn provider_name(&self) -> &str {
        "Seq"
    }

    async fn query_entries(&self, since: Option<&str>, count: usize) -> Result<Vec<LogEntry>> {
        // Build the Seq REST API URL for querying events.
        // Seq ≥ 2020.1 supports `/api/events?filter=<expr>&count=<n>&afterId=<id>`.
        let mut url = format!(
            "{}/api/events?count={}&filter={}",
            self.api_url.trim_end_matches('/'),
            count,
            urlencoding::encode(&self.filter),
        );

        if let Some(after_id) = since {
            url.push_str(&format!("&afterId={}", urlencoding::encode(after_id)));
        }

        let mut req = http().get(&url);
        if !self.api_key.is_empty() {
            req = req.header("X-Seq-ApiKey", &self.api_key);
        }

        let resp = req.send().await.context("Seq API request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Seq API error ({status}): {text}");
        }

        let events: Vec<SeqEvent> = resp
            .json()
            .await
            .context("Failed to parse Seq events response")?;

        let entries = events
            .into_iter()
            .map(|e| LogEntry {
                id: e.id,
                timestamp: e.timestamp,
                level: e.level,
                message: e.rendered_message,
            })
            .collect();

        Ok(entries)
    }
}

// ---------------------------------------------------------------------------
// URL-encoding helper — tiny inline implementation to avoid a new dependency.
// ---------------------------------------------------------------------------

mod urlencoding {
    use std::fmt::Write;

    /// Percent-encode a string for use in a URL query parameter.
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
    fn provider_name_is_seq() {
        let p = SeqProvider::new("key", DEFAULT_SEQ_API, "@Level = 'Error'");
        assert_eq!(p.provider_name(), "Seq");
    }

    #[test]
    fn default_api_url_constant() {
        assert_eq!(DEFAULT_SEQ_API, "http://localhost:5341");
    }

    #[test]
    fn url_encode_special_chars() {
        assert_eq!(
            urlencoding::encode("@Level = 'Error'"),
            "%40Level%20%3D%20%27Error%27"
        );
    }

    #[test]
    fn url_encode_passthrough() {
        assert_eq!(urlencoding::encode("hello-world_1.0"), "hello-world_1.0");
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
        // Return the first line (e.g. "GET /api/events?... HTTP/1.1")
        text.lines().next().unwrap_or_default().to_string()
    }

    #[tokio::test]
    async fn query_entries_parses_seq_events() {
        let body = r#"[{"Id":"evt-1","Timestamp":"2025-01-01T00:00:00Z","Level":"Error","RenderedMessage":"Disk full"},{"Id":"evt-2","Timestamp":"2025-01-01T00:01:00Z","Level":"Warning","RenderedMessage":"High memory"}]"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = SeqProvider::new("", &url, "@Level = 'Error'");
        let (result, _raw) = tokio::join!(provider.query_entries(None, 10), req_fut);

        let entries = result.unwrap();
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].id, "evt-1");
        assert_eq!(entries[0].level, "Error");
        assert_eq!(entries[0].message, "Disk full");
        assert_eq!(entries[1].id, "evt-2");
    }

    #[tokio::test]
    async fn query_entries_sends_after_id() {
        let body = r#"[]"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = SeqProvider::new("my-key", &url, "has(@Exception)");
        let (result, raw) = tokio::join!(provider.query_entries(Some("evt-99"), 5), req_fut);

        result.unwrap();
        let req_line = extract_request(&raw);
        assert!(req_line.contains("afterId=evt-99"), "req: {req_line}");
        assert!(req_line.contains("count=5"), "req: {req_line}");
    }

    #[tokio::test]
    async fn query_entries_sends_api_key_header() {
        let body = r#"[]"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = SeqProvider::new("my-api-key", &url, "true");
        let (result, raw) = tokio::join!(provider.query_entries(None, 1), req_fut);

        result.unwrap();
        let text = std::str::from_utf8(&raw).unwrap().to_lowercase();
        assert!(
            text.contains("x-seq-apikey: my-api-key"),
            "header missing: {text}"
        );
    }

    #[tokio::test]
    async fn query_entries_api_error_returns_err() {
        let body = r#"{"Error":"Unauthorized"}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 401 Unauthorized", body).await;

        let provider = SeqProvider::new("bad", &url, "true");
        let (result, _) = tokio::join!(provider.query_entries(None, 10), req_fut);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("401"));
    }

    #[tokio::test]
    async fn query_entries_empty_returns_empty_vec() {
        let body = r#"[]"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", body).await;

        let provider = SeqProvider::new("", &url, "true");
        let (result, _) = tokio::join!(provider.query_entries(None, 10), req_fut);

        assert_eq!(result.unwrap().len(), 0);
    }
}
