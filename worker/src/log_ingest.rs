//! Server-side log-source ingestion: poll an external error tracker
//! (Sentry in v1) from the pulse loop and sync its issues into the triage
//! queue as `log_event` items.
//!
//! Non-secret settings come from `[log_source]` in `.wreck-it/config.toml`;
//! the auth token lives **only** in worker KV (written through the portal),
//! never in the repository.  Ingestion is gated on `[triage].enabled` and
//! is always best-effort from the pulse's perspective.
//!
//! No pagination cursor in v1: the query window (`statsPeriod`, capped
//! limit) is bounded and correlation-key dedup (`log:sentry:{issue_id}`)
//! makes repeats harmless, so recurring issues update their open item
//! rather than duplicating.

use crate::kv_store;
use worker::Fetch;
use wreck_it_core::config::{LogSourceSettings, TriageConfig};
use wreck_it_core::log_source::{
    build_sentry_issues_request, parse_sentry_issues_response, CoreLogEntry, HttpRequestSpec,
    SentryQuery, DEFAULT_SENTRY_BASE_URL, DEFAULT_SENTRY_QUERY, DEFAULT_STATS_PERIOD,
};
use wreck_it_core::triage::{upsert_item, TriageItem, TriageSeverity, TriageSource, TriageUpsert};

/// Default entries ingested per pulse.
const DEFAULT_MAX_ENTRIES: usize = 20;

/// Execute a transport-agnostic [`HttpRequestSpec`] via the Workers Fetch
/// API and return the response body.
async fn fetch_spec(spec: &HttpRequestSpec) -> Result<String, String> {
    let headers = worker::Headers::new();
    for (name, value) in &spec.headers {
        headers.set(name, value).ok();
    }
    headers.set("User-Agent", "wreck-it-worker").ok();

    let method = match spec.method {
        "GET" => worker::Method::Get,
        "POST" => worker::Method::Post,
        other => return Err(format!("unsupported method {other}")),
    };

    let request = worker::Request::new_with_init(
        &spec.url,
        worker::RequestInit::new()
            .with_method(method)
            .with_headers(headers),
    )
    .map_err(|e| format!("Failed to create request: {e}"))?;

    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| format!("log-source request failed: {e}"))?;

    let status = response.status_code();
    let body = response.text().await.unwrap_or_default();
    if !(200..300).contains(&status) {
        return Err(format!("log-source API returned {status}: {body}"));
    }
    Ok(body)
}

/// Map a log level to a triage severity.
pub fn severity_from_level(level: &str) -> TriageSeverity {
    match level.to_ascii_lowercase().as_str() {
        "fatal" => TriageSeverity::High,
        "error" => TriageSeverity::Medium,
        _ => TriageSeverity::Low,
    }
}

/// Build a candidate triage item for an ingested log entry.
pub fn entry_to_item(provider: &str, entry: &CoreLogEntry, now: u64) -> TriageItem {
    let title: String = {
        let first_line = entry.message.lines().next().unwrap_or_default();
        let mut title: String = first_line.chars().take(100).collect();
        if title.chars().count() < first_line.chars().count() {
            title.push('…');
        }
        title
    };
    let detail = format!(
        "Level: {}\nLast seen: {}\n\n{}",
        entry.level, entry.timestamp, entry.message
    );
    let mut item = TriageItem::new(
        TriageSource::LogEvent {
            provider: provider.to_string(),
            event_id: entry.id.clone(),
        },
        format!("[{}] {title}", entry.level),
        Some(detail),
        now,
    );
    item.severity = severity_from_level(&entry.level);
    item
}

/// Resolve the effective Sentry query from repo settings, when the section
/// is present, complete, and names a supported provider.
pub fn sentry_query_from_settings(
    settings: &LogSourceSettings,
    token: &str,
) -> Option<SentryQuery> {
    if settings.provider.as_deref() != Some("sentry") {
        return None;
    }
    Some(SentryQuery {
        base_url: settings
            .api_base_url
            .clone()
            .unwrap_or_else(|| DEFAULT_SENTRY_BASE_URL.to_string()),
        organization: settings.organization.clone()?,
        project: settings.project.clone()?,
        auth_token: token.to_string(),
        query: settings
            .query
            .clone()
            .unwrap_or_else(|| DEFAULT_SENTRY_QUERY.to_string()),
        stats_period: DEFAULT_STATS_PERIOD.to_string(),
    })
}

/// Poll the configured log source and sync entries into the triage queue.
///
/// Returns a short summary.  Skips quietly (with a reason) when the repo
/// has no usable configuration or token.
pub async fn run_log_ingest(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    settings: &LogSourceSettings,
    triage_config: &TriageConfig,
    now: u64,
) -> Result<String, String> {
    if !triage_config.enabled {
        return Ok("log ingest skipped (triage disabled)".to_string());
    }
    let token = match kv_store::load_log_source_token(kv, owner, repo).await? {
        Some(t) => t,
        None => return Ok("log ingest skipped (no token configured)".to_string()),
    };
    let query = match sentry_query_from_settings(settings, &token) {
        Some(q) => q,
        None => return Ok("log ingest skipped (incomplete [log_source] settings)".to_string()),
    };

    let limit = settings.max_entries.unwrap_or(DEFAULT_MAX_ENTRIES);
    let spec = build_sentry_issues_request(&query, limit, None);
    let body = fetch_spec(&spec).await?;
    let entries = parse_sentry_issues_response(&body)?;

    if entries.is_empty() {
        return Ok("log ingest: no matching entries".to_string());
    }

    let mut items = kv_store::load_triage(kv, owner, repo).await?;
    let cap = triage_config.effective_max_items();
    let mut created = 0;
    let mut updated = 0;
    for entry in &entries {
        match upsert_item(&mut items, entry_to_item("sentry", entry, now), cap) {
            TriageUpsert::Created => created += 1,
            TriageUpsert::UpdatedExisting { .. } => updated += 1,
        }
    }
    if created + updated > 0 {
        crate::slack_notify::sync_and_save(kv, owner, repo, &mut items).await?;
    }
    Ok(format!(
        "log ingest: {created} new, {updated} updated item(s) from sentry"
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(id: &str, level: &str) -> CoreLogEntry {
        CoreLogEntry {
            id: id.to_string(),
            timestamp: "2026-07-23T10:00:00Z".to_string(),
            level: level.to_string(),
            message: "TypeError: boom [WEB-1A] app/checkout https://s.io/1/".to_string(),
        }
    }

    fn settings() -> LogSourceSettings {
        LogSourceSettings {
            provider: Some("sentry".to_string()),
            organization: Some("acme".to_string()),
            project: Some("web".to_string()),
            api_base_url: None,
            query: None,
            max_entries: None,
        }
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(severity_from_level("fatal"), TriageSeverity::High);
        assert_eq!(severity_from_level("Error"), TriageSeverity::Medium);
        assert_eq!(severity_from_level("warning"), TriageSeverity::Low);
        assert_eq!(severity_from_level("info"), TriageSeverity::Low);
    }

    #[test]
    fn entry_item_mapping() {
        let item = entry_to_item("sentry", &entry("42", "Error"), 1000);
        assert_eq!(item.correlation_key, "log:sentry:42");
        assert_eq!(item.severity, TriageSeverity::Medium);
        assert!(item.title.starts_with("[Error] TypeError: boom"));
        let detail = item.detail.unwrap();
        assert!(detail.contains("Last seen: 2026-07-23T10:00:00Z"));
        assert!(detail.contains("https://s.io/1/"));
    }

    #[test]
    fn entry_item_title_truncates() {
        let mut long = entry("1", "Error");
        long.message = "x".repeat(300);
        let item = entry_to_item("sentry", &long, 1000);
        // "[Error] " + 100 chars + ellipsis.
        assert!(item.title.chars().count() <= 8 + 101);
        assert!(item.title.ends_with('…'));
    }

    #[test]
    fn query_from_complete_settings() {
        let q = sentry_query_from_settings(&settings(), "tok").unwrap();
        assert_eq!(q.base_url, DEFAULT_SENTRY_BASE_URL);
        assert_eq!(q.organization, "acme");
        assert_eq!(q.query, DEFAULT_SENTRY_QUERY);
        assert_eq!(q.auth_token, "tok");
    }

    #[test]
    fn query_requires_supported_provider_and_slugs() {
        let mut s = settings();
        s.provider = Some("seq".to_string());
        assert!(sentry_query_from_settings(&s, "tok").is_none());

        let mut s = settings();
        s.provider = None;
        assert!(sentry_query_from_settings(&s, "tok").is_none());

        let mut s = settings();
        s.organization = None;
        assert!(sentry_query_from_settings(&s, "tok").is_none());

        let mut s = settings();
        s.project = None;
        assert!(sentry_query_from_settings(&s, "tok").is_none());
    }

    #[test]
    fn query_honors_overrides() {
        let mut s = settings();
        s.api_base_url = Some("https://sentry.example.com".to_string());
        s.query = Some("is:unresolved level:fatal".to_string());
        let q = sentry_query_from_settings(&s, "tok").unwrap();
        assert_eq!(q.base_url, "https://sentry.example.com");
        assert_eq!(q.query, "is:unresolved level:fatal");
    }
}
