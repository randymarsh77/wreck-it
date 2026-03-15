//! Modular Kanban/Agile project management integration.
//!
//! This module provides a [`KanbanProvider`] trait that abstracts operations on
//! external project-management boards (Linear, JIRA, Trello, …).  Each backend
//! implements the same trait so the rest of the codebase is provider-agnostic.
//!
//! ## Capabilities
//!
//! * **Sync** – push task status changes (in-progress, completed, …) from the
//!   wreck-it loop into the destination project board.
//! * **Pull updates** – fetch description edits and new comments from the board
//!   back into the local task definition.
//! * **Links** – attach GitHub Issue / PR URLs to the board item so that
//!   cross-references are always visible.
//!
//! ## Configuration
//!
//! The active provider is selected via the `kanban_provider` field in the
//! wreck-it config, with provider-specific settings under `kanban_*` keys.
//! See [`KanbanConfig`] and [`provider_from_config`] for details.

pub mod jira;
pub mod linear;
pub mod trello;

use anyhow::Result;
use serde::{Deserialize, Serialize};
use wreck_it_core::types::TaskStatus;

// ---------------------------------------------------------------------------
// Shared types
// ---------------------------------------------------------------------------

/// Identifier of an issue/card on the external board.
///
/// Each provider uses a different identifier scheme (Linear uses UUIDs,
/// JIRA uses project-prefixed keys like `PROJ-42`, Trello uses card IDs).
/// We store the external id as an opaque string so the core loop does not
/// need to know the format.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct KanbanIssue {
    /// Provider-specific identifier (e.g. Linear UUID, JIRA key, Trello card id).
    pub external_id: String,
    /// Human-readable URL to the issue on the board.
    pub url: String,
    /// Title/summary of the issue as it appears on the board.
    pub title: String,
    /// Full description / body text.
    pub description: String,
}

/// Updates fetched from the external board for a single issue.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[allow(dead_code)]
pub struct KanbanUpdates {
    /// New description text, if the description changed since the last sync.
    pub description: Option<String>,
    /// New comments added since the last sync, oldest first.
    pub comments: Vec<String>,
}

/// Which Kanban backend to use.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum KanbanBackend {
    Linear,
    Jira,
    Trello,
}

impl std::fmt::Display for KanbanBackend {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Linear => write!(f, "Linear"),
            Self::Jira => write!(f, "Jira"),
            Self::Trello => write!(f, "Trello"),
        }
    }
}

/// Label prefix stored in a task's `labels` array to record the external
/// Kanban issue that originated the task.
///
/// The full label value is `"kanban-source:{external_id}"`.  The ralph loop
/// checks for this prefix when polling for inbound tickets to avoid
/// re-importing issues that have already been converted to tasks.
pub const KANBAN_SOURCE_LABEL_PREFIX: &str = "kanban-source:";

/// Provider-agnostic configuration for the Kanban integration.
///
/// These fields live in the wreck-it `Config` and are used by
/// [`provider_from_config`] to construct the appropriate backend.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct KanbanConfig {
    /// Which provider to use.  When `None` the integration is disabled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider: Option<KanbanBackend>,

    /// API token / key for authenticating with the provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_token: Option<String>,

    /// Provider-specific project or board identifier.
    ///
    /// * **Linear** – team key or project ID.
    /// * **JIRA** – project key (e.g. `"PROJ"`).
    /// * **Trello** – board ID.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Base URL for self-hosted or on-premise instances.
    ///
    /// * **JIRA** – e.g. `https://mycompany.atlassian.net`
    /// * **Linear** / **Trello** – usually left empty (uses the default SaaS endpoint).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub api_base_url: Option<String>,

    /// Optional email address used for JIRA basic-auth (along with `api_token`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub user_email: Option<String>,

    /// Label / tag to watch for **inbound** ticket sync.
    ///
    /// When set, the ralph loop polls the external board each iteration and
    /// automatically creates wreck-it task objects for any issues/cards that
    /// carry this label and have not yet been imported.  The label value is
    /// provider-specific:
    ///
    /// * **Linear** – label name (e.g. `"wreck-it"`).
    /// * **JIRA**   – label name as used in JQL (e.g. `"wreck-it"`).
    /// * **Trello** – label name on the board (e.g. `"wreck-it"`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sync_label: Option<String>,
}

// ---------------------------------------------------------------------------
// Trait
// ---------------------------------------------------------------------------

/// Abstraction over an external Kanban / project-management board.
///
/// Each backend (Linear, JIRA, Trello) implements this trait.  The ralph loop
/// interacts only with [`KanbanClient`] (an enum wrapper), keeping
/// provider-specific details isolated in the backend modules.
pub trait KanbanProvider: Send + Sync {
    /// Human-readable name of the provider (e.g. `"Linear"`, `"Jira"`, `"Trello"`).
    fn provider_name(&self) -> &str;

    /// Create a new issue/card on the board for the given task.
    ///
    /// Returns a [`KanbanIssue`] with the external id and URL of the newly
    /// created item.
    fn create_issue(
        &self,
        task_id: &str,
        description: &str,
    ) -> impl std::future::Future<Output = Result<KanbanIssue>> + Send;

    /// Transition an existing issue to the status that corresponds to the
    /// given [`TaskStatus`].
    fn transition_issue(
        &self,
        external_id: &str,
        status: TaskStatus,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Add a comment to an existing issue (e.g. a GitHub PR link or status
    /// update).
    fn add_comment(
        &self,
        external_id: &str,
        comment: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Attach a URL link to an existing issue (e.g. GitHub Issue or PR URL).
    fn add_link(
        &self,
        external_id: &str,
        url: &str,
        title: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// Fetch the current state of an issue from the board.
    fn get_issue(
        &self,
        external_id: &str,
    ) -> impl std::future::Future<Output = Result<KanbanIssue>> + Send;

    /// Fetch description edits and new comments since `since` (unix timestamp).
    ///
    /// When `since` is `None`, returns all comments.
    fn get_updates(
        &self,
        external_id: &str,
        since: Option<u64>,
    ) -> impl std::future::Future<Output = Result<KanbanUpdates>> + Send;

    /// Close / archive the issue on the board.
    fn close_issue(
        &self,
        external_id: &str,
    ) -> impl std::future::Future<Output = Result<()>> + Send;

    /// List issues/cards on the board that carry `label`.
    ///
    /// Used for **inbound** sync: the ralph loop calls this each iteration to
    /// discover tickets that a human created on the external board (e.g. to
    /// request AI work) and converts them into wreck-it tasks.  Only issues
    /// that do **not** yet have a corresponding wreck-it task (identified by
    /// the [`KANBAN_SOURCE_LABEL_PREFIX`] label) are acted upon.
    fn list_inbound_issues(
        &self,
        label: &str,
    ) -> impl std::future::Future<Output = Result<Vec<KanbanIssue>>> + Send;
}

// ---------------------------------------------------------------------------
// Enum dispatch wrapper
// ---------------------------------------------------------------------------

/// Concrete wrapper that dispatches [`KanbanProvider`] calls to the configured
/// backend.  Using an enum instead of `dyn Trait` avoids object-safety issues
/// with async methods.
pub enum KanbanClient {
    Linear(linear::LinearProvider),
    Jira(jira::JiraProvider),
    Trello(trello::TrelloProvider),
}

impl KanbanClient {
    pub fn provider_name(&self) -> &str {
        match self {
            Self::Linear(p) => p.provider_name(),
            Self::Jira(p) => p.provider_name(),
            Self::Trello(p) => p.provider_name(),
        }
    }

    pub async fn create_issue(&self, task_id: &str, description: &str) -> Result<KanbanIssue> {
        match self {
            Self::Linear(p) => p.create_issue(task_id, description).await,
            Self::Jira(p) => p.create_issue(task_id, description).await,
            Self::Trello(p) => p.create_issue(task_id, description).await,
        }
    }

    pub async fn transition_issue(&self, external_id: &str, status: TaskStatus) -> Result<()> {
        match self {
            Self::Linear(p) => p.transition_issue(external_id, status).await,
            Self::Jira(p) => p.transition_issue(external_id, status).await,
            Self::Trello(p) => p.transition_issue(external_id, status).await,
        }
    }

    pub async fn add_comment(&self, external_id: &str, comment: &str) -> Result<()> {
        match self {
            Self::Linear(p) => p.add_comment(external_id, comment).await,
            Self::Jira(p) => p.add_comment(external_id, comment).await,
            Self::Trello(p) => p.add_comment(external_id, comment).await,
        }
    }

    #[allow(dead_code)]
    pub async fn add_link(&self, external_id: &str, url: &str, title: &str) -> Result<()> {
        match self {
            Self::Linear(p) => p.add_link(external_id, url, title).await,
            Self::Jira(p) => p.add_link(external_id, url, title).await,
            Self::Trello(p) => p.add_link(external_id, url, title).await,
        }
    }

    #[allow(dead_code)]
    pub async fn get_issue(&self, external_id: &str) -> Result<KanbanIssue> {
        match self {
            Self::Linear(p) => p.get_issue(external_id).await,
            Self::Jira(p) => p.get_issue(external_id).await,
            Self::Trello(p) => p.get_issue(external_id).await,
        }
    }

    #[allow(dead_code)]
    pub async fn get_updates(
        &self,
        external_id: &str,
        since: Option<u64>,
    ) -> Result<KanbanUpdates> {
        match self {
            Self::Linear(p) => p.get_updates(external_id, since).await,
            Self::Jira(p) => p.get_updates(external_id, since).await,
            Self::Trello(p) => p.get_updates(external_id, since).await,
        }
    }

    #[allow(dead_code)]
    pub async fn close_issue(&self, external_id: &str) -> Result<()> {
        match self {
            Self::Linear(p) => p.close_issue(external_id).await,
            Self::Jira(p) => p.close_issue(external_id).await,
            Self::Trello(p) => p.close_issue(external_id).await,
        }
    }

    pub async fn list_inbound_issues(&self, label: &str) -> Result<Vec<KanbanIssue>> {
        match self {
            Self::Linear(p) => p.list_inbound_issues(label).await,
            Self::Jira(p) => p.list_inbound_issues(label).await,
            Self::Trello(p) => p.list_inbound_issues(label).await,
        }
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Construct a [`KanbanClient`] from the wreck-it config.
///
/// Returns `None` when Kanban integration is disabled (no provider configured)
/// or when required settings are missing.  Warnings are emitted for
/// misconfiguration so that operators know why the feature is inactive.
pub fn provider_from_config(cfg: &KanbanConfig) -> Option<KanbanClient> {
    let backend = match &cfg.provider {
        Some(b) => b,
        None => return None,
    };

    let api_token = match &cfg.api_token {
        Some(t) if !t.is_empty() => t.clone(),
        _ => {
            tracing::warn!(
                "kanban provider {} selected but kanban.api_token is not set; \
                 Kanban integration disabled",
                backend
            );
            return None;
        }
    };

    let project_id = match &cfg.project_id {
        Some(p) if !p.is_empty() => p.clone(),
        _ => {
            tracing::warn!(
                "kanban provider {} selected but kanban.project_id is not set; \
                 Kanban integration disabled",
                backend
            );
            return None;
        }
    };

    match backend {
        KanbanBackend::Linear => {
            let base_url = cfg
                .api_base_url
                .clone()
                .unwrap_or_else(|| linear::DEFAULT_LINEAR_API.to_string());
            Some(KanbanClient::Linear(linear::LinearProvider::new(
                api_token, project_id, base_url,
            )))
        }
        KanbanBackend::Jira => {
            let base_url = match &cfg.api_base_url {
                Some(u) if !u.is_empty() => u.clone(),
                _ => {
                    tracing::warn!(
                        "JIRA provider selected but kanban.api_base_url is not set; \
                         Kanban integration disabled"
                    );
                    return None;
                }
            };
            let user_email = cfg.user_email.clone().unwrap_or_default();
            Some(KanbanClient::Jira(jira::JiraProvider::new(
                api_token, project_id, base_url, user_email,
            )))
        }
        KanbanBackend::Trello => {
            // Trello uses api_token as key and expects a separate token.
            // For simplicity we encode both in `api_token` as `key:token`.
            let base_url = cfg
                .api_base_url
                .clone()
                .unwrap_or_else(|| trello::DEFAULT_TRELLO_API.to_string());
            Some(KanbanClient::Trello(trello::TrelloProvider::new(
                api_token, project_id, base_url,
            )))
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_from_config_returns_none_when_no_provider() {
        let cfg = KanbanConfig::default();
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn provider_from_config_returns_none_when_no_token() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Linear),
            api_token: None,
            project_id: Some("team-1".into()),
            ..Default::default()
        };
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn provider_from_config_returns_none_when_no_project() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Linear),
            api_token: Some("tok".into()),
            project_id: None,
            ..Default::default()
        };
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn provider_from_config_jira_requires_base_url() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Jira),
            api_token: Some("tok".into()),
            project_id: Some("PROJ".into()),
            api_base_url: None,
            ..Default::default()
        };
        assert!(provider_from_config(&cfg).is_none());
    }

    #[test]
    fn provider_from_config_creates_linear() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Linear),
            api_token: Some("lin_api_xxx".into()),
            project_id: Some("team-1".into()),
            ..Default::default()
        };
        let p = provider_from_config(&cfg);
        assert!(p.is_some());
        assert_eq!(p.unwrap().provider_name(), "Linear");
    }

    #[test]
    fn provider_from_config_creates_jira() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Jira),
            api_token: Some("jira_tok".into()),
            project_id: Some("PROJ".into()),
            api_base_url: Some("https://acme.atlassian.net".into()),
            user_email: Some("user@example.com".into()),
            ..Default::default()
        };
        let p = provider_from_config(&cfg);
        assert!(p.is_some());
        assert_eq!(p.unwrap().provider_name(), "Jira");
    }

    #[test]
    fn provider_from_config_creates_trello() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Trello),
            api_token: Some("key:token".into()),
            project_id: Some("board123".into()),
            ..Default::default()
        };
        let p = provider_from_config(&cfg);
        assert!(p.is_some());
        assert_eq!(p.unwrap().provider_name(), "Trello");
    }

    #[test]
    fn kanban_backend_serde_roundtrip() {
        for (variant, expected) in [
            (KanbanBackend::Linear, r#""linear""#),
            (KanbanBackend::Jira, r#""jira""#),
            (KanbanBackend::Trello, r#""trello""#),
        ] {
            let json = serde_json::to_string(&variant).unwrap();
            assert_eq!(json, expected);
            let back: KanbanBackend = serde_json::from_str(&json).unwrap();
            assert_eq!(back, variant);
        }
    }

    #[test]
    fn kanban_issue_serde_roundtrip() {
        let issue = KanbanIssue {
            external_id: "LIN-123".into(),
            url: "https://linear.app/team/LIN-123".into(),
            title: "[task-1] Do the thing".into(),
            description: "Implement feature X".into(),
        };
        let json = serde_json::to_string(&issue).unwrap();
        let back: KanbanIssue = serde_json::from_str(&json).unwrap();
        assert_eq!(back, issue);
    }

    #[test]
    fn kanban_updates_default_is_empty() {
        let u = KanbanUpdates::default();
        assert!(u.description.is_none());
        assert!(u.comments.is_empty());
    }

    #[test]
    fn kanban_config_sync_label_roundtrip() {
        let cfg = KanbanConfig {
            provider: Some(KanbanBackend::Linear),
            api_token: Some("tok".into()),
            project_id: Some("team-1".into()),
            sync_label: Some("wreck-it".into()),
            ..Default::default()
        };
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains("wreck-it"));
        let back: KanbanConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(back.sync_label, Some("wreck-it".into()));
    }

    #[test]
    fn kanban_config_sync_label_defaults_to_none() {
        let cfg = KanbanConfig::default();
        assert!(cfg.sync_label.is_none());
        // When sync_label is None it should be skipped in serialisation.
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(!json.contains("sync_label"));
    }

    #[test]
    fn kanban_source_label_prefix_constant() {
        assert_eq!(KANBAN_SOURCE_LABEL_PREFIX, "kanban-source:");
    }
}
