//! Modular log source integration for ingest-based task triage.
//!
//! This module provides a [`LogSourceProvider`] trait that abstracts queries
//! against structured log platforms (Seq, and potentially others in the
//! future).  Each backend implements the same trait so the rest of the
//! codebase is provider-agnostic.
//!
//! ## Capabilities
//!
//! * **Ingest** – query log entries (errors, warnings, exceptions) from a
//!   remote log platform and convert them into wreck-it tasks for an agent to
//!   triage and fix.
//!
//! ## Configuration
//!
//! The active provider is selected via the `provider` field in
//! [`LogSourceConfig`], with provider-specific settings under `log_source`
//! keys in the wreck-it config.  See [`LogSourceConfig`] and
//! [`provider_from_config`] for details.

pub mod cloudflare;
pub mod seq;

use anyhow::Result;
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// A single log entry fetched from the external log platform.
///
/// This is the provider-agnostic representation.  Each backend maps its
/// native event format into this struct.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct LogEntry {
    /// Provider-specific unique event identifier.
    pub id: String,
    /// ISO-8601 timestamp of the log event.
    pub timestamp: String,
    /// Severity level (e.g. `"Error"`, `"Warning"`, `"Fatal"`).
    pub level: String,
    /// Rendered / human-readable log message.
    pub message: String,
}

/// Which log source backend to use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum LogSourceBackend {
    Seq,
    Cloudflare,
}

impl std::fmt::Display for LogSourceBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Seq => write!(f, "Seq"),
            Self::Cloudflare => write!(f, "Cloudflare"),
        }
    }
}

/// Label prefix stored in a task's `labels` array to record the external
/// log event that originated the task.
///
/// The full label value is `"log-source:{event_id}"`.  The ralph loop checks
/// for this prefix when polling for inbound log entries to avoid re-importing
/// events that have already been converted to tasks.
pub const LOG_SOURCE_LABEL_PREFIX: &str = "log-source:";

/// Provider-agnostic configuration for the log source integration.
///
/// These fields live in the wreck-it `Config` and are used by
/// [`provider_from_config`] to construct the appropriate backend.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct LogSourceConfig {
    /// Which provider to use.  When `None` the integration is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<LogSourceBackend>,

    /// API token / key for authenticating with the log platform.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,

    /// Base URL of the log platform instance.
    ///
    /// * **Seq** – e.g. `http://localhost:5341` or `https://seq.example.com`
    /// * **Cloudflare** – e.g. `https://api.cloudflare.com/client/v4`
    ///   (defaults to the public API)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,

    /// Provider-specific filter / query expression.
    ///
    /// * **Seq** – a Seq filter expression, e.g. `"@Level = 'Error'"` or
    ///   `"has(@Exception)"`.
    /// * **Cloudflare** – a SQL-like filter clause for the Workers Telemetry
    ///   API, e.g. `"outcome = 'exception'"` or `"status >= 500"`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,

    /// Cloudflare Account ID (required when `provider` is `Cloudflare`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,

    /// Cloudflare Worker script name (required when `provider` is
    /// `Cloudflare`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub script_name: Option<String>,

    /// Maximum number of log entries to fetch per sync cycle.
    /// Defaults to `20` when not set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_entries: Option<usize>,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over an external structured log platform.
///
/// Each backend (Seq, …) implements this trait.  The ralph loop interacts
/// only with [`LogSourceClient`] (an enum wrapper), keeping provider-specific
/// details isolated in the backend modules.
pub trait LogSourceProvider: Send + Sync {
    /// Human-readable name of the provider (e.g. `"Seq"`).
    fn provider_name(&self) -> &str;

    /// Query log entries from the platform.
    ///
    /// * `since` – when provided, only entries **after** this cursor/id are
    ///   returned (provider-specific semantics).
    /// * `count` – maximum number of entries to return.
    fn query_entries(
        &self,
        since: Option<&str>,
        count: usize,
    ) -> impl std::future::Future<Output = Result<Vec<LogEntry>>> + Send;
}

// ---------------------------------------------------------------------------
// Enum dispatch wrapper
// ---------------------------------------------------------------------------

/// Concrete wrapper that dispatches [`LogSourceProvider`] calls to the
/// configured backend.  Using an enum instead of `dyn Trait` avoids
/// object-safety issues with async methods.
pub enum LogSourceClient {
    Seq(seq::SeqProvider),
    Cloudflare(cloudflare::CloudflareProvider),
}

impl LogSourceClient {
    #[allow(dead_code)]
    pub fn provider_name(&self) -> &str {
        match self {
            Self::Seq(p) => p.provider_name(),
            Self::Cloudflare(p) => p.provider_name(),
        }
    }

    pub async fn query_entries(&self, since: Option<&str>, count: usize) -> Result<Vec<LogEntry>> {
        match self {
            Self::Seq(p) => p.query_entries(since, count).await,
            Self::Cloudflare(p) => p.query_entries(since, count).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Default maximum number of log entries to fetch per sync cycle.
const DEFAULT_MAX_ENTRIES: usize = 20;

/// Construct a [`LogSourceClient`] from the wreck-it config.
///
/// Returns `None` when log source integration is disabled (no provider
/// configured) or when required settings are missing.  Warnings are emitted
/// for misconfiguration so that operators know why the feature is inactive.
pub fn provider_from_config(cfg: &LogSourceConfig) -> Option<LogSourceClient> {
    let backend = match &cfg.provider {
        Some(b) => b,
        None => return None,
    };

    match backend {
        LogSourceBackend::Seq => {
            let base_url = cfg
                .api_base_url
                .clone()
                .unwrap_or_else(|| seq::DEFAULT_SEQ_API.to_string());
            let api_key = cfg.api_token.clone().unwrap_or_default();
            let filter = cfg
                .filter
                .clone()
                .unwrap_or_else(|| "@Level = 'Error'".to_string());
            Some(LogSourceClient::Seq(seq::SeqProvider::new(
                api_key, base_url, filter,
            )))
        }
        LogSourceBackend::Cloudflare => {
            let account_id = match &cfg.account_id {
                Some(id) => id.clone(),
                None => {
                    eprintln!("Warning: Cloudflare log source requires `account_id`");
                    return None;
                }
            };
            let script_name = match &cfg.script_name {
                Some(n) => n.clone(),
                None => {
                    eprintln!("Warning: Cloudflare log source requires `script_name`");
                    return None;
                }
            };
            let api_token = cfg.api_token.clone().unwrap_or_default();
            let api_url = cfg
                .api_base_url
                .clone()
                .unwrap_or_else(|| cloudflare::DEFAULT_CF_API.to_string());
            let filter = cfg
                .filter
                .clone()
                .unwrap_or_else(|| "outcome = 'exception'".to_string());
            Some(LogSourceClient::Cloudflare(
                cloudflare::CloudflareProvider::new(
                    api_token,
                    account_id,
                    script_name,
                    api_url,
                    filter,
                ),
            ))
        }
    }
}

/// Return the effective `max_entries` value from the config, falling back to
/// [`DEFAULT_MAX_ENTRIES`] when not set.
pub fn effective_max_entries(cfg: &LogSourceConfig) -> usize {
    cfg.max_entries.unwrap_or(DEFAULT_MAX_ENTRIES)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_from_config_returns_none_when_no_provider() {
        let cfg = LogSourceConfig::default();
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn provider_from_config_creates_seq_with_defaults() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Seq),
            ..Default::default()
        };
        let p = provider_from_config(&cfg);
        assert!(p.is_some());
        assert_eq!(p.unwrap().provider_name(), "Seq");
    }

    #[test]
    fn provider_from_config_creates_seq_with_custom_url() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Seq),
            api_base_url: Some("https://seq.example.com".into()),
            api_token: Some("my-key".into()),
            filter: Some("has(@Exception)".into()),
            ..Default::default()
        };
        let p = provider_from_config(&cfg);
        assert!(p.is_some());
        assert_eq!(p.unwrap().provider_name(), "Seq");
    }

    #[test]
    fn log_source_backend_serde_roundtrip() {
        let json = serde_json::to_string(&LogSourceBackend::Seq).unwrap();
        assert_eq!(json, r#""seq""#);
        let back: LogSourceBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(back, LogSourceBackend::Seq);
    }

    #[test]
    fn log_entry_serde_roundtrip() {
        let entry = LogEntry {
            id: "evt-1".into(),
            timestamp: "2025-01-01T00:00:00Z".into(),
            level: "Error".into(),
            message: "Disk full".into(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let back: LogEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(back, entry);
    }

    #[test]
    fn log_source_config_roundtrip() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Seq),
            api_token: Some("tok".into()),
            api_base_url: Some("http://localhost:5341".into()),
            filter: Some("@Level = 'Error'".into()),
            max_entries: Some(50),
            account_id: None,
            script_name: None,
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: LogSourceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn log_source_config_defaults() {
        let cfg = LogSourceConfig::default();
        assert!(cfg.provider.is_none());
        assert!(cfg.api_token.is_none());
        assert!(cfg.api_base_url.is_none());
        assert!(cfg.filter.is_none());
        assert!(cfg.max_entries.is_none());
        assert!(cfg.account_id.is_none());
        assert!(cfg.script_name.is_none());
    }

    #[test]
    fn log_source_config_serialise_omits_defaults() {
        let cfg = LogSourceConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("provider"));
        assert!(!json.contains("api_token"));
    }

    #[test]
    fn effective_max_entries_returns_configured_value() {
        let cfg = LogSourceConfig {
            max_entries: Some(42),
            ..Default::default()
        };
        assert_eq!(effective_max_entries(&cfg), 42);
    }

    #[test]
    fn effective_max_entries_returns_default() {
        let cfg = LogSourceConfig::default();
        assert_eq!(effective_max_entries(&cfg), DEFAULT_MAX_ENTRIES);
    }

    #[test]
    fn log_source_label_prefix_constant() {
        assert_eq!(LOG_SOURCE_LABEL_PREFIX, "log-source:");
    }

    #[test]
    fn log_source_backend_display() {
        assert_eq!(format!("{}", LogSourceBackend::Seq), "Seq");
        assert_eq!(format!("{}", LogSourceBackend::Cloudflare), "Cloudflare");
    }

    #[test]
    fn log_source_backend_cloudflare_serde_roundtrip() {
        let json = serde_json::to_string(&LogSourceBackend::Cloudflare).unwrap();
        assert_eq!(json, r#""cloudflare""#);
        let back: LogSourceBackend = serde_json::from_str(&json).unwrap();
        assert_eq!(back, LogSourceBackend::Cloudflare);
    }

    #[test]
    fn provider_from_config_creates_cloudflare() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Cloudflare),
            api_token: Some("cf-tok".into()),
            account_id: Some("acct-123".into()),
            script_name: Some("my-worker".into()),
            ..Default::default()
        };
        let p = provider_from_config(&cfg);
        assert!(p.is_some());
        assert_eq!(p.unwrap().provider_name(), "Cloudflare");
    }

    #[test]
    fn provider_from_config_cloudflare_requires_account_id() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Cloudflare),
            api_token: Some("cf-tok".into()),
            script_name: Some("my-worker".into()),
            ..Default::default()
        };
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn provider_from_config_cloudflare_requires_script_name() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Cloudflare),
            api_token: Some("cf-tok".into()),
            account_id: Some("acct-123".into()),
            ..Default::default()
        };
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn log_source_config_cloudflare_roundtrip() {
        let cfg = LogSourceConfig {
            provider: Some(LogSourceBackend::Cloudflare),
            api_token: Some("cf-tok".into()),
            api_base_url: Some("https://api.cloudflare.com/client/v4".into()),
            filter: Some("outcome = 'exception'".into()),
            max_entries: Some(10),
            account_id: Some("acct-123".into()),
            script_name: Some("my-worker".into()),
        };
        let json = serde_json::to_string(&cfg).unwrap();
        let back: LogSourceConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back, cfg);
    }

    #[test]
    fn log_source_config_serialise_omits_cloudflare_defaults() {
        let cfg = LogSourceConfig::default();
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("account_id"));
        assert!(!json.contains("script_name"));
    }
}
