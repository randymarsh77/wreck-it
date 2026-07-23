//! Sentry backend for the log source integration.
//!
//! Thin reqwest transport around the shared pure request builders in
//! [`wreck_it_core::log_source`] (the worker drives the same builders with
//! the Cloudflare Fetch API).  Entries are Sentry **issues** (grouped
//! events), which is exactly the dedup granularity wanted for triage: a
//! recurring error updates one task instead of creating one per event.

use super::{LogEntry, LogSourceProvider};
use anyhow::{bail, Context, Result};
use wreck_it_core::log_source::{
    build_sentry_issues_request, parse_sentry_issues_response, SentryQuery,
};

/// Default Sentry API base URL.
pub const DEFAULT_SENTRY_API: &str = wreck_it_core::log_source::DEFAULT_SENTRY_BASE_URL;

/// Sentry log source provider.
pub struct SentryProvider {
    query: SentryQuery,
}

impl SentryProvider {
    pub fn new(
        auth_token: String,
        base_url: String,
        organization: String,
        project: String,
        query: String,
    ) -> Self {
        Self {
            query: SentryQuery {
                base_url,
                organization,
                project,
                auth_token,
                query,
                stats_period: wreck_it_core::log_source::DEFAULT_STATS_PERIOD.to_string(),
            },
        }
    }
}

impl LogSourceProvider for SentryProvider {
    fn provider_name(&self) -> &str {
        "Sentry"
    }

    /// Query unresolved issues, newest activity first.
    ///
    /// `since` is interpreted as a Sentry pagination cursor and passed
    /// through verbatim.  The ralph loop passes `None` and relies on
    /// label-based dedup, so a single page (`count` ≤ 100) is sufficient.
    async fn query_entries(&self, since: Option<&str>, count: usize) -> Result<Vec<LogEntry>> {
        let spec = build_sentry_issues_request(&self.query, count, since);

        let client = reqwest::Client::new();
        let mut request = client.get(&spec.url);
        for (name, value) in &spec.headers {
            request = request.header(name, value);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("Sentry request to {} failed", spec.url))?;

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            bail!("Sentry API returned {status}: {body}");
        }

        let body = response
            .text()
            .await
            .context("failed to read Sentry response body")?;
        let entries = parse_sentry_issues_response(&body).map_err(anyhow::Error::msg)?;

        Ok(entries
            .into_iter()
            .map(|e| LogEntry {
                id: e.id,
                timestamp: e.timestamp,
                level: e.level,
                message: e.message,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_name() {
        let provider = SentryProvider::new(
            "tok".into(),
            DEFAULT_SENTRY_API.into(),
            "acme".into(),
            "web".into(),
            "is:unresolved".into(),
        );
        assert_eq!(provider.provider_name(), "Sentry");
    }
}
