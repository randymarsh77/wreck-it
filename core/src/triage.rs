//! First-class triage items.
//!
//! A [`TriageItem`] represents a signal that something needs attention — a
//! failing CI workflow, an error surfaced by a log source, a Slack callout,
//! or a security finding.  Items are stored per repository (one JSON document
//! in the worker's KV store) and flow through a small lifecycle:
//!
//! ```text
//! New → Investigating → PrOpen → Resolved
//!   \→ Dismissed / Stale
//! ```
//!
//! Repeated occurrences of the same underlying signal are collapsed into a
//! single open item via [`TriageSource::correlation_key`] rather than
//! creating duplicates.  All logic here is pure: callers pass the current
//! unix time and persist the mutated list themselves, so the same code runs
//! natively in the CLI and in the WASM worker.

use serde::{Deserialize, Serialize};

/// Default maximum number of items retained per repository.
pub const DEFAULT_MAX_ITEMS: usize = 200;

/// Where a triage item originated.
///
/// Internally tagged so the JSON is self-describing:
/// `{"type":"ci_failure","run_id":123,...}`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TriageSource {
    /// A completed GitHub Actions workflow run with a failing conclusion.
    CiFailure {
        run_id: u64,
        workflow_name: String,
        branch: String,
        head_sha: String,
        /// Raw GitHub conclusion string (`"failure"`, `"timed_out"`, ...).
        conclusion: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        run_url: Option<String>,
        #[serde(default = "default_run_attempt")]
        run_attempt: u32,
    },
    /// An error entry ingested from a log source (Seq, Cloudflare, Sentry).
    LogEvent { provider: String, event_id: String },
    /// An @-mention callout from a linked Slack channel.
    SlackMention {
        channel: String,
        ts: String,
        user: String,
    },
    /// A security finding (e.g. a Dependabot alert or audit result).
    SecurityFinding { tool: String, finding_id: String },
}

fn default_run_attempt() -> u32 {
    1
}

impl TriageSource {
    /// Stable dedup key for this signal.
    ///
    /// The same workflow failing repeatedly on the same branch collapses
    /// into one open item; a Sentry issue firing again updates the existing
    /// item; and so on.
    pub fn correlation_key(&self) -> String {
        match self {
            Self::CiFailure {
                workflow_name,
                branch,
                ..
            } => format!("ci:{workflow_name}:{branch}"),
            Self::LogEvent { provider, event_id } => format!("log:{provider}:{event_id}"),
            Self::SlackMention { channel, ts, .. } => format!("slack:{channel}:{ts}"),
            Self::SecurityFinding { tool, finding_id } => format!("sec:{tool}:{finding_id}"),
        }
    }

    /// Short identifier used as the suffix of a generated item id.
    fn id_suffix(&self) -> String {
        match self {
            Self::CiFailure { run_id, .. } => run_id.to_string(),
            Self::LogEvent { event_id, .. } => event_id.clone(),
            Self::SlackMention { ts, .. } => ts.replace('.', "-"),
            Self::SecurityFinding { finding_id, .. } => finding_id.clone(),
        }
    }
}

/// Lifecycle state of a triage item.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TriageStatus {
    /// Newly created; no action taken yet.
    New,
    /// An issue has been created and a coding agent assigned.
    Investigating,
    /// A linked fix PR has been detected (`pr_number` is set).
    PrOpen,
    /// The underlying signal was fixed (PR merged or signal cleared).
    Resolved,
    /// Manually dismissed by a user.
    Dismissed,
    /// Aged out without resolution.
    Stale,
}

impl TriageStatus {
    /// Terminal items never absorb new occurrences and are prune candidates.
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Resolved | Self::Dismissed | Self::Stale)
    }
}

/// Severity of a triage item.
#[derive(
    Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, PartialOrd, Ord, Default, Hash,
)]
#[serde(rename_all = "snake_case")]
pub enum TriageSeverity {
    Low,
    #[default]
    Medium,
    High,
    Critical,
}

/// A single triage item.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TriageItem {
    /// Unique id within a repository (e.g. `tri-1712345678-8842213`).
    pub id: String,

    /// Where the signal came from.
    pub source: TriageSource,

    /// Current lifecycle state.
    pub status: TriageStatus,

    /// Severity, defaulting to medium.
    #[serde(default)]
    pub severity: TriageSeverity,

    /// Short human-readable summary.
    pub title: String,

    /// Evidence excerpt (truncated log tail, alert detail, ...).  Writers
    /// cap this — see [`MAX_DETAIL_BYTES`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,

    /// Dedup key; recomputable via [`TriageSource::correlation_key`] but
    /// stored so consumers don't need the logic.
    pub correlation_key: String,

    /// How many times this signal recurred while the item was open.
    #[serde(default = "default_occurrences")]
    pub occurrences: u32,

    /// Unix seconds when the item was created.
    pub created_at: u64,

    /// Unix seconds when the item was last updated.
    pub updated_at: u64,

    /// GitHub issue number created to dispatch a fix, if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,

    /// Linked fix PR number, if detected.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,

    /// Linked wreck-it task ids (unused in v1; reserved for the DO backend).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_ids: Vec<String>,
}

/// Maximum stored size of [`TriageItem::detail`] in bytes.
pub const MAX_DETAIL_BYTES: usize = 8 * 1024;

fn default_occurrences() -> u32 {
    1
}

impl TriageItem {
    /// Create a new item in the [`TriageStatus::New`] state.
    ///
    /// The id is derived from the creation time and a source-specific
    /// suffix (run id, event id, ...), which is unique within a repository.
    /// `detail` is truncated to [`MAX_DETAIL_BYTES`].
    pub fn new(source: TriageSource, title: String, detail: Option<String>, now: u64) -> Self {
        let correlation_key = source.correlation_key();
        let id = format!("tri-{now}-{}", source.id_suffix());
        Self {
            id,
            source,
            status: TriageStatus::New,
            severity: TriageSeverity::default(),
            title,
            detail: detail.map(truncate_detail),
            correlation_key,
            occurrences: 1,
            created_at: now,
            updated_at: now,
            issue_number: None,
            pr_number: None,
            task_ids: Vec::new(),
        }
    }
}

/// Truncate `detail` to at most [`MAX_DETAIL_BYTES`], keeping the tail
/// (errors live at the end of logs) and respecting UTF-8 boundaries.
pub fn truncate_detail(detail: String) -> String {
    if detail.len() <= MAX_DETAIL_BYTES {
        return detail;
    }
    let mut start = detail.len() - MAX_DETAIL_BYTES;
    while !detail.is_char_boundary(start) {
        start += 1;
    }
    format!("…{}", &detail[start..])
}

/// Outcome of [`upsert_item`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TriageUpsert {
    /// No open item shared the candidate's correlation key; it was appended.
    Created,
    /// An open item with the same correlation key absorbed the occurrence.
    UpdatedExisting { id: String },
}

/// Insert `candidate` into `items`, collapsing repeated signals.
///
/// If a **non-terminal** item shares the candidate's correlation key, that
/// item absorbs the occurrence: `occurrences` is bumped, the source is
/// replaced (fresh run id / head sha), `detail` and `updated_at` are
/// refreshed, and `id`/`status`/`issue_number`/`pr_number` are preserved.
/// Otherwise the candidate is appended and terminal items beyond `cap` are
/// pruned, oldest `updated_at` first (non-terminal items are never pruned).
pub fn upsert_item(items: &mut Vec<TriageItem>, candidate: TriageItem, cap: usize) -> TriageUpsert {
    if let Some(existing) = items
        .iter_mut()
        .find(|i| !i.status.is_terminal() && i.correlation_key == candidate.correlation_key)
    {
        existing.occurrences = existing.occurrences.saturating_add(1);
        existing.source = candidate.source;
        if candidate.detail.is_some() {
            existing.detail = candidate.detail;
        }
        existing.updated_at = candidate.updated_at;
        return TriageUpsert::UpdatedExisting {
            id: existing.id.clone(),
        };
    }

    items.push(candidate);
    prune_terminal(items, cap);
    TriageUpsert::Created
}

/// Drop terminal items until `items.len() <= cap`, oldest `updated_at`
/// first.  Non-terminal items are never dropped, so the list can exceed
/// `cap` when everything is still open.
fn prune_terminal(items: &mut Vec<TriageItem>, cap: usize) {
    while items.len() > cap {
        let oldest_terminal = items
            .iter()
            .enumerate()
            .filter(|(_, i)| i.status.is_terminal())
            .min_by_key(|(_, i)| i.updated_at)
            .map(|(idx, _)| idx);
        match oldest_terminal {
            Some(idx) => {
                items.remove(idx);
            }
            None => break,
        }
    }
}

/// Mark every non-terminal item linked to `pr_number` as resolved.
///
/// Returns the number of items transitioned.
pub fn resolve_items_for_pr(items: &mut [TriageItem], pr_number: u64, now: u64) -> usize {
    let mut count = 0;
    for item in items.iter_mut() {
        if !item.status.is_terminal() && item.pr_number == Some(pr_number) {
            item.status = TriageStatus::Resolved;
            item.updated_at = now;
            count += 1;
        }
    }
    count
}

/// Mark open CI-failure items for `workflow_name` on `branch` as resolved
/// (called when a later run of the same workflow succeeds).
///
/// Returns the number of items transitioned.
pub fn resolve_ci_items_for_success(
    items: &mut [TriageItem],
    workflow_name: &str,
    branch: &str,
    now: u64,
) -> usize {
    let key = format!("ci:{workflow_name}:{branch}");
    let mut count = 0;
    for item in items.iter_mut() {
        if !item.status.is_terminal() && item.correlation_key == key {
            item.status = TriageStatus::Resolved;
            item.updated_at = now;
            count += 1;
        }
    }
    count
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ci_source(run_id: u64) -> TriageSource {
        TriageSource::CiFailure {
            run_id,
            workflow_name: "CI".to_string(),
            branch: "master".to_string(),
            head_sha: "abc123".to_string(),
            conclusion: "failure".to_string(),
            run_url: Some(format!("https://github.com/o/r/actions/runs/{run_id}")),
            run_attempt: 1,
        }
    }

    fn ci_item(run_id: u64, now: u64) -> TriageItem {
        TriageItem::new(
            ci_source(run_id),
            "CI failure: CI on master".to_string(),
            Some("error: it broke".to_string()),
            now,
        )
    }

    #[test]
    fn correlation_key_shapes() {
        assert_eq!(ci_source(1).correlation_key(), "ci:CI:master");
        assert_eq!(
            TriageSource::LogEvent {
                provider: "sentry".into(),
                event_id: "42".into()
            }
            .correlation_key(),
            "log:sentry:42"
        );
        assert_eq!(
            TriageSource::SlackMention {
                channel: "C123".into(),
                ts: "1700.001".into(),
                user: "U1".into()
            }
            .correlation_key(),
            "slack:C123:1700.001"
        );
        assert_eq!(
            TriageSource::SecurityFinding {
                tool: "dependabot".into(),
                finding_id: "7".into()
            }
            .correlation_key(),
            "sec:dependabot:7"
        );
    }

    #[test]
    fn new_item_defaults() {
        let item = ci_item(99, 1000);
        assert_eq!(item.id, "tri-1000-99");
        assert_eq!(item.status, TriageStatus::New);
        assert_eq!(item.severity, TriageSeverity::Medium);
        assert_eq!(item.occurrences, 1);
        assert_eq!(item.correlation_key, "ci:CI:master");
        assert!(item.issue_number.is_none());
        assert!(item.pr_number.is_none());
    }

    #[test]
    fn upsert_creates_when_empty() {
        let mut items = Vec::new();
        let result = upsert_item(&mut items, ci_item(1, 1000), DEFAULT_MAX_ITEMS);
        assert_eq!(result, TriageUpsert::Created);
        assert_eq!(items.len(), 1);
    }

    #[test]
    fn upsert_collapses_repeat_failure_into_open_item() {
        let mut items = Vec::new();
        upsert_item(&mut items, ci_item(1, 1000), DEFAULT_MAX_ITEMS);
        items[0].status = TriageStatus::Investigating;
        items[0].issue_number = Some(55);

        let result = upsert_item(&mut items, ci_item(2, 2000), DEFAULT_MAX_ITEMS);
        assert_eq!(
            result,
            TriageUpsert::UpdatedExisting {
                id: "tri-1000-1".to_string()
            }
        );
        assert_eq!(items.len(), 1);
        let item = &items[0];
        assert_eq!(item.occurrences, 2);
        assert_eq!(item.updated_at, 2000);
        assert_eq!(item.created_at, 1000);
        // Status and issue linkage preserved; source refreshed.
        assert_eq!(item.status, TriageStatus::Investigating);
        assert_eq!(item.issue_number, Some(55));
        match &item.source {
            TriageSource::CiFailure { run_id, .. } => assert_eq!(*run_id, 2),
            other => panic!("unexpected source: {other:?}"),
        }
    }

    #[test]
    fn upsert_does_not_absorb_into_terminal_item() {
        let mut items = Vec::new();
        upsert_item(&mut items, ci_item(1, 1000), DEFAULT_MAX_ITEMS);
        items[0].status = TriageStatus::Resolved;

        let result = upsert_item(&mut items, ci_item(2, 2000), DEFAULT_MAX_ITEMS);
        assert_eq!(result, TriageUpsert::Created);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].status, TriageStatus::Resolved);
        assert_eq!(items[1].status, TriageStatus::New);
    }

    #[test]
    fn upsert_keeps_existing_detail_when_candidate_has_none() {
        let mut items = Vec::new();
        upsert_item(&mut items, ci_item(1, 1000), DEFAULT_MAX_ITEMS);
        let candidate = TriageItem::new(ci_source(2), "t".to_string(), None, 2000);
        upsert_item(&mut items, candidate, DEFAULT_MAX_ITEMS);
        assert_eq!(items[0].detail.as_deref(), Some("error: it broke"));
    }

    #[test]
    fn prune_drops_oldest_terminal_first_and_spares_open_items() {
        let mut items = Vec::new();
        for run in 0..4u64 {
            let mut item = TriageItem::new(
                TriageSource::LogEvent {
                    provider: "seq".into(),
                    event_id: run.to_string(),
                },
                format!("event {run}"),
                None,
                1000 + run,
            );
            if run < 2 {
                item.status = TriageStatus::Resolved;
            }
            items.push(item);
        }
        // Cap 3: both terminal items (oldest first) must go.
        let candidate = ci_item(9, 5000);
        upsert_item(&mut items, candidate, 3);
        assert_eq!(items.len(), 3);
        let kept: Vec<_> = items.iter().map(|i| i.title.clone()).collect();
        assert!(!kept.contains(&"event 0".to_string()));
        assert!(!kept.contains(&"event 1".to_string()));
        assert!(kept.contains(&"event 2".to_string()));
        assert!(kept.contains(&"event 3".to_string()));
    }

    #[test]
    fn prune_never_drops_open_items_even_over_cap() {
        let mut items = Vec::new();
        for run in 0..5u64 {
            items.push(TriageItem::new(
                TriageSource::LogEvent {
                    provider: "seq".into(),
                    event_id: run.to_string(),
                },
                format!("event {run}"),
                None,
                1000 + run,
            ));
        }
        upsert_item(&mut items, ci_item(9, 5000), 3);
        // All 6 are open; nothing can be pruned.
        assert_eq!(items.len(), 6);
    }

    #[test]
    fn resolve_by_pr() {
        let mut items = vec![ci_item(1, 1000), ci_item(2, 1000)];
        items[0].pr_number = Some(77);
        items[0].status = TriageStatus::PrOpen;
        let count = resolve_items_for_pr(&mut items, 77, 3000);
        assert_eq!(count, 1);
        assert_eq!(items[0].status, TriageStatus::Resolved);
        assert_eq!(items[0].updated_at, 3000);
        assert_eq!(items[1].status, TriageStatus::New);
    }

    #[test]
    fn resolve_by_pr_skips_terminal() {
        let mut items = vec![ci_item(1, 1000)];
        items[0].pr_number = Some(77);
        items[0].status = TriageStatus::Dismissed;
        assert_eq!(resolve_items_for_pr(&mut items, 77, 3000), 0);
        assert_eq!(items[0].status, TriageStatus::Dismissed);
    }

    #[test]
    fn resolve_ci_on_success() {
        let mut items = vec![ci_item(1, 1000)];
        items[0].status = TriageStatus::Investigating;
        let count = resolve_ci_items_for_success(&mut items, "CI", "master", 4000);
        assert_eq!(count, 1);
        assert_eq!(items[0].status, TriageStatus::Resolved);
        // Different workflow or branch: untouched.
        let mut items = vec![ci_item(1, 1000)];
        assert_eq!(
            resolve_ci_items_for_success(&mut items, "Deploy", "master", 4000),
            0
        );
        assert_eq!(
            resolve_ci_items_for_success(&mut items, "CI", "main", 4000),
            0
        );
    }

    #[test]
    fn truncate_detail_keeps_tail_and_utf8_boundary() {
        let detail = format!("{}é-tail", "x".repeat(MAX_DETAIL_BYTES));
        let truncated = truncate_detail(detail);
        assert!(truncated.len() <= MAX_DETAIL_BYTES + '…'.len_utf8());
        assert!(truncated.starts_with('…'));
        assert!(truncated.ends_with("é-tail"));
        // Short strings pass through untouched.
        assert_eq!(truncate_detail("short".to_string()), "short");
    }

    #[test]
    fn json_roundtrip_is_stable_and_tagged() {
        let item = ci_item(1, 1000);
        let json = serde_json::to_string(&item).unwrap();
        assert!(json.contains("\"type\":\"ci_failure\""));
        assert!(json.contains("\"status\":\"new\""));
        let back: TriageItem = serde_json::from_str(&json).unwrap();
        assert_eq!(back, item);
    }

    #[test]
    fn json_missing_optional_fields_defaults() {
        // Simulates an older document without severity/occurrences.
        let json = r#"{
            "id": "tri-1-1",
            "source": {"type": "log_event", "provider": "seq", "event_id": "9"},
            "status": "new",
            "title": "t",
            "correlation_key": "log:seq:9",
            "created_at": 1,
            "updated_at": 1
        }"#;
        let item: TriageItem = serde_json::from_str(json).unwrap();
        assert_eq!(item.severity, TriageSeverity::Medium);
        assert_eq!(item.occurrences, 1);
        assert!(item.task_ids.is_empty());
    }
}
