//! Provider-agnostic log-source primitives shared by the CLI and worker.
//!
//! HTTP transports differ between the two consumers (the CLI uses
//! `reqwest`, the worker uses the Cloudflare Fetch API), so this module
//! contains only **pure** request builders and response parsers — no I/O.
//! A transport executes the returned [`HttpRequestSpec`] and hands the body
//! back to the parser.
//!
//! v1 covers Sentry; the Seq and Cloudflare backends predate this module
//! and keep their CLI-local implementations.

use serde::{Deserialize, Serialize};

/// A transport-agnostic description of an HTTP request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HttpRequestSpec {
    pub method: &'static str,
    pub url: String,
    /// Header name/value pairs (auth included).
    pub headers: Vec<(String, String)>,
}

/// Provider-agnostic log entry (mirrors the CLI's `LogEntry` shape).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CoreLogEntry {
    /// Provider-specific unique identifier (Sentry **issue** id — grouping
    /// repeated events is exactly the dedup granularity triage wants).
    pub id: String,
    /// ISO-8601 timestamp (Sentry `lastSeen`).
    pub timestamp: String,
    /// Severity level, capitalized (`"Error"`, `"Warning"`, ...).
    pub level: String,
    /// Rendered message: `"{title} [{shortId}] {culprit} {permalink}"`.
    pub message: String,
}

/// Parameters of a Sentry issues query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentryQuery {
    /// Instance base URL, e.g. `https://sentry.io` (self-hosted supported).
    pub base_url: String,
    /// Organization slug.
    pub organization: String,
    /// Project slug.
    pub project: String,
    /// Auth token (`Bearer`).
    pub auth_token: String,
    /// Sentry search query (default [`DEFAULT_SENTRY_QUERY`]).
    pub query: String,
    /// Sentry `statsPeriod` (default [`DEFAULT_STATS_PERIOD`]).
    pub stats_period: String,
}

/// Default Sentry search query: unresolved errors.
pub const DEFAULT_SENTRY_QUERY: &str = "is:unresolved level:error";

/// Default look-back window.
pub const DEFAULT_STATS_PERIOD: &str = "24h";

/// Default Sentry base URL.
pub const DEFAULT_SENTRY_BASE_URL: &str = "https://sentry.io";

fn percent_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

/// Build the request listing a project's issues, newest activity first.
///
/// `GET {base}/api/0/projects/{org}/{project}/issues/?query=…&limit=…&sort=date`
/// with an optional pagination `cursor`.
pub fn build_sentry_issues_request(
    q: &SentryQuery,
    limit: usize,
    cursor: Option<&str>,
) -> HttpRequestSpec {
    let mut url = format!(
        "{}/api/0/projects/{}/{}/issues/?query={}&statsPeriod={}&limit={}&sort=date",
        q.base_url.trim_end_matches('/'),
        percent_encode(&q.organization),
        percent_encode(&q.project),
        percent_encode(&q.query),
        percent_encode(&q.stats_period),
        limit.min(100),
    );
    if let Some(cursor) = cursor {
        url.push_str(&format!("&cursor={}", percent_encode(cursor)));
    }
    HttpRequestSpec {
        method: "GET",
        url,
        headers: vec![
            (
                "Authorization".to_string(),
                format!("Bearer {}", q.auth_token),
            ),
            ("Accept".to_string(), "application/json".to_string()),
        ],
    }
}

/// Subset of a Sentry issue we consume.
#[derive(Debug, Deserialize)]
struct SentryIssue {
    id: String,
    title: String,
    #[serde(rename = "shortId")]
    short_id: Option<String>,
    culprit: Option<String>,
    permalink: Option<String>,
    level: Option<String>,
    #[serde(rename = "lastSeen")]
    last_seen: Option<String>,
}

/// Parse a Sentry issues-list response body into [`CoreLogEntry`]s.
pub fn parse_sentry_issues_response(body: &str) -> Result<Vec<CoreLogEntry>, String> {
    let issues: Vec<SentryIssue> =
        serde_json::from_str(body).map_err(|e| format!("failed to parse Sentry issues: {e}"))?;
    Ok(issues
        .into_iter()
        .map(|issue| {
            let mut message = issue.title;
            if let Some(short_id) = &issue.short_id {
                message.push_str(&format!(" [{short_id}]"));
            }
            if let Some(culprit) = issue.culprit.as_deref().filter(|c| !c.is_empty()) {
                message.push_str(&format!(" {culprit}"));
            }
            if let Some(permalink) = issue.permalink.as_deref().filter(|p| !p.is_empty()) {
                message.push_str(&format!(" {permalink}"));
            }
            CoreLogEntry {
                id: issue.id,
                timestamp: issue.last_seen.unwrap_or_default(),
                level: capitalize(issue.level.as_deref().unwrap_or("error")),
                message,
            }
        })
        .collect())
}

fn capitalize(s: &str) -> String {
    let mut chars = s.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
        None => String::new(),
    }
}

/// Extract the next-page cursor from a Sentry `Link` response header.
///
/// Sentry paginates with `Link: <url>; rel="next"; results="true";
/// cursor="0:100:0", …` — the next cursor is only meaningful when
/// `results="true"`.
pub fn parse_sentry_link_header(link: &str) -> Option<String> {
    for part in link.split(',') {
        if !part.contains("rel=\"next\"") {
            continue;
        }
        if !part.contains("results=\"true\"") {
            return None;
        }
        let marker = "cursor=\"";
        let start = part.find(marker)? + marker.len();
        let end = part[start..].find('"')? + start;
        return Some(part[start..end].to_string());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn query() -> SentryQuery {
        SentryQuery {
            base_url: "https://sentry.io/".to_string(),
            organization: "acme corp".to_string(),
            project: "web-app".to_string(),
            auth_token: "tok".to_string(),
            query: DEFAULT_SENTRY_QUERY.to_string(),
            stats_period: DEFAULT_STATS_PERIOD.to_string(),
        }
    }

    #[test]
    fn issues_request_shape() {
        let spec = build_sentry_issues_request(&query(), 20, None);
        assert_eq!(spec.method, "GET");
        assert_eq!(
            spec.url,
            "https://sentry.io/api/0/projects/acme%20corp/web-app/issues/\
             ?query=is%3Aunresolved%20level%3Aerror&statsPeriod=24h&limit=20&sort=date"
        );
        assert!(spec
            .headers
            .contains(&("Authorization".to_string(), "Bearer tok".to_string())));
    }

    #[test]
    fn issues_request_caps_limit_and_appends_cursor() {
        let spec = build_sentry_issues_request(&query(), 500, Some("0:100:0"));
        assert!(spec.url.contains("limit=100"));
        assert!(spec.url.contains("&cursor=0%3A100%3A0"));
    }

    #[test]
    fn parse_issues_response_maps_fields() {
        // Trimmed from a real Sentry issues-list response.
        let body = r#"[{
            "id": "1234567890",
            "title": "TypeError: cannot read properties of undefined",
            "shortId": "WEB-APP-1A",
            "culprit": "app/checkout in submitOrder",
            "permalink": "https://acme.sentry.io/issues/1234567890/",
            "level": "error",
            "lastSeen": "2026-07-23T10:00:00Z",
            "count": "41",
            "status": "unresolved"
        }]"#;
        let entries = parse_sentry_issues_response(body).unwrap();
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, "1234567890");
        assert_eq!(entry.level, "Error");
        assert_eq!(entry.timestamp, "2026-07-23T10:00:00Z");
        assert!(entry.message.contains("TypeError"));
        assert!(entry.message.contains("[WEB-APP-1A]"));
        assert!(entry.message.contains("app/checkout in submitOrder"));
        assert!(entry.message.contains("https://acme.sentry.io/issues/1234567890/"));
    }

    #[test]
    fn parse_issues_response_minimal_fields() {
        let body = r#"[{"id": "9", "title": "boom"}]"#;
        let entries = parse_sentry_issues_response(body).unwrap();
        assert_eq!(entries[0].message, "boom");
        assert_eq!(entries[0].level, "Error");
        assert_eq!(entries[0].timestamp, "");
    }

    #[test]
    fn parse_issues_response_rejects_garbage() {
        assert!(parse_sentry_issues_response("not json").is_err());
    }

    #[test]
    fn link_header_next_cursor() {
        let link = "<https://sentry.io/api/0/x/?cursor=0:0:1>; rel=\"previous\"; \
                    results=\"false\"; cursor=\"0:0:1\", \
                    <https://sentry.io/api/0/x/?cursor=0:100:0>; rel=\"next\"; \
                    results=\"true\"; cursor=\"0:100:0\"";
        assert_eq!(parse_sentry_link_header(link).as_deref(), Some("0:100:0"));
    }

    #[test]
    fn link_header_no_more_results() {
        let link = "<https://sentry.io/api/0/x/?cursor=0:100:0>; rel=\"next\"; \
                    results=\"false\"; cursor=\"0:100:0\"";
        assert!(parse_sentry_link_header(link).is_none());
        assert!(parse_sentry_link_header("").is_none());
    }
}
