//! Outbound Slack notifications for triage-item lifecycle changes.
//!
//! Rather than plumbing notification calls through every transition site,
//! callers replace `kv_store::save_triage` with [`sync_and_save`]: right
//! before persisting, the sweep announces every item whose current status
//! differs from the last status posted to Slack (recorded on the item's
//! [`SlackThreadRef`]).  First announcements create a channel message;
//! later ones thread onto it.  The design makes double-posting structurally
//! impossible — the dedup state saves atomically with the items it guards.
//!
//! Notification is always best-effort: Slack failures are logged and never
//! block persistence.

use crate::kv_store;
use crate::slack::{triage_status_message, SlackChannelLink, SlackClient, SlackWorkspace};
use std::collections::HashMap;
use worker::console_warn;
use wreck_it_core::triage::{SlackThreadRef, TriageItem, TriageSeverity, TriageSource};

/// Announce pending lifecycle changes, then persist the triage document.
pub async fn sync_and_save(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    items: &mut [TriageItem],
) -> Result<(), String> {
    if let Err(e) = sync_notifications(kv, owner, repo, items).await {
        console_warn!("[wreck-it][slack] notification sweep failed: {e}");
    }
    kv_store::save_triage(kv, owner, repo, items).await
}

/// Whether an item's current status still needs announcing.
///
/// Items with a thread announce every status change.  Items without a
/// thread only get a first announcement while non-terminal — a dead item
/// that was never announced stays silent.
pub fn needs_announcement(item: &TriageItem) -> bool {
    match &item.slack_thread {
        Some(thread) => thread.last_notified_status != Some(item.status),
        None => !item.status.is_terminal(),
    }
}

/// Whether a channel link wants to hear about this item.
///
/// CI failures, log events, and Slack callouts ride the `notify_triage`
/// flag.  Security findings ride `notify_security` and only at high or
/// critical severity — routine dependency chatter stays out of channels.
pub fn link_allows(link: &SlackChannelLink, item: &TriageItem) -> bool {
    match &item.source {
        TriageSource::SecurityFinding { .. } => {
            link.notify_security && item.severity >= TriageSeverity::High
        }
        _ => link.notify_triage,
    }
}

async fn sync_notifications(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    items: &mut [TriageItem],
) -> Result<(), String> {
    if !items.iter().any(needs_announcement) {
        return Ok(());
    }

    // Load this repo's channel links once; bail early when Slack was never
    // connected (the common case — one KV read).
    let link_refs = kv_store::load_slack_links_for_repo(kv, owner, repo).await?;
    let has_threads = items.iter().any(|i| i.slack_thread.is_some());
    if link_refs.is_empty() && !has_threads {
        return Ok(());
    }

    let mut links: Vec<(String, String, SlackChannelLink)> = Vec::new();
    for r in &link_refs {
        if let Some(link) = kv_store::load_slack_link(kv, &r.team_id, &r.channel_id).await? {
            links.push((r.team_id.clone(), r.channel_id.clone(), link));
        }
    }

    // Workspace (bot token) cache — one KV read per team per sweep.
    let mut workspaces: HashMap<String, Option<SlackWorkspace>> = HashMap::new();

    for item in items.iter_mut().filter(|i| needs_announcement(i)) {
        match &item.slack_thread {
            Some(thread) => {
                let thread = thread.clone();
                let workspace =
                    match load_workspace_cached(kv, &mut workspaces, &thread.team_id).await {
                        Some(w) => w,
                        None => continue,
                    };
                let (blocks, fallback) = triage_status_message(owner, repo, item);
                match SlackClient::new(&workspace.bot_token)
                    .post_message(&thread.channel, &blocks, &fallback, Some(&thread.thread_ts))
                    .await
                {
                    Ok(_) => {
                        item.slack_thread = Some(SlackThreadRef {
                            last_notified_status: Some(item.status),
                            ..thread
                        });
                    }
                    Err(e) => console_warn!("[wreck-it][slack] threaded update failed: {e}"),
                }
            }
            None => {
                // First announcement: the first eligible linked channel
                // becomes the item's thread home.
                let target = links.iter().find(|(_, _, link)| link_allows(link, item));
                let (team_id, channel_id) = match target {
                    Some((t, c, _)) => (t.clone(), c.clone()),
                    None => continue,
                };
                let workspace = match load_workspace_cached(kv, &mut workspaces, &team_id).await {
                    Some(w) => w,
                    None => continue,
                };
                let (blocks, fallback) = triage_status_message(owner, repo, item);
                match SlackClient::new(&workspace.bot_token)
                    .post_message(&channel_id, &blocks, &fallback, None)
                    .await
                {
                    Ok(ts) => {
                        item.slack_thread = Some(SlackThreadRef {
                            team_id,
                            channel: channel_id,
                            thread_ts: ts,
                            last_notified_status: Some(item.status),
                        });
                    }
                    Err(e) => console_warn!("[wreck-it][slack] announcement failed: {e}"),
                }
            }
        }
    }
    Ok(())
}

async fn load_workspace_cached(
    kv: &worker::kv::KvStore,
    cache: &mut HashMap<String, Option<SlackWorkspace>>,
    team_id: &str,
) -> Option<SlackWorkspace> {
    if let Some(cached) = cache.get(team_id) {
        return cached.clone();
    }
    let loaded = kv_store::load_slack_workspace(kv, team_id)
        .await
        .unwrap_or_default();
    cache.insert(team_id.to_string(), loaded.clone());
    loaded
}

#[cfg(test)]
mod tests {
    use super::*;
    use wreck_it_core::triage::{TriageStatus, TriageUpsert};

    fn ci_item(status: TriageStatus) -> TriageItem {
        let mut item = TriageItem::new(
            TriageSource::CiFailure {
                run_id: 1,
                workflow_name: "CI".into(),
                branch: "main".into(),
                head_sha: "abc".into(),
                conclusion: "failure".into(),
                run_url: None,
                run_attempt: 1,
            },
            "CI failure".into(),
            None,
            1000,
        );
        item.status = status;
        item
    }

    fn sec_item(severity: TriageSeverity) -> TriageItem {
        let mut item = TriageItem::new(
            TriageSource::SecurityFinding {
                tool: "dependabot".into(),
                finding_id: "1".into(),
            },
            "vuln".into(),
            None,
            1000,
        );
        item.severity = severity;
        item
    }

    fn link(triage: bool, security: bool) -> SlackChannelLink {
        SlackChannelLink {
            owner: "o".into(),
            repo: "r".into(),
            installation_id: 1,
            notify_triage: triage,
            notify_pr: true,
            notify_security: security,
        }
    }

    #[test]
    fn unannounced_open_item_needs_announcement() {
        assert!(needs_announcement(&ci_item(TriageStatus::New)));
        assert!(needs_announcement(&ci_item(TriageStatus::Investigating)));
    }

    #[test]
    fn unannounced_terminal_item_stays_silent() {
        assert!(!needs_announcement(&ci_item(TriageStatus::Resolved)));
        assert!(!needs_announcement(&ci_item(TriageStatus::Dismissed)));
    }

    #[test]
    fn threaded_item_announces_only_status_changes() {
        let mut item = ci_item(TriageStatus::Investigating);
        item.slack_thread = Some(SlackThreadRef {
            team_id: "T1".into(),
            channel: "C1".into(),
            thread_ts: "1700.1".into(),
            last_notified_status: Some(TriageStatus::Investigating),
        });
        assert!(!needs_announcement(&item));

        item.status = TriageStatus::PrOpen;
        assert!(needs_announcement(&item));

        // Terminal changes DO announce when a thread exists.
        item.status = TriageStatus::Resolved;
        assert!(needs_announcement(&item));
    }

    #[test]
    fn upsert_absorption_preserves_thread_dedup() {
        // A repeat failure absorbed into an announced item must not
        // re-announce (status is preserved by upsert).
        let mut items = vec![ci_item(TriageStatus::Investigating)];
        items[0].slack_thread = Some(SlackThreadRef {
            team_id: "T1".into(),
            channel: "C1".into(),
            thread_ts: "1700.1".into(),
            last_notified_status: Some(TriageStatus::Investigating),
        });
        let result =
            wreck_it_core::triage::upsert_item(&mut items, ci_item(TriageStatus::New), 200);
        assert!(matches!(result, TriageUpsert::UpdatedExisting { .. }));
        assert!(!needs_announcement(&items[0]));
    }

    #[test]
    fn link_gates_by_source_and_severity() {
        let both = link(true, true);
        assert!(link_allows(&both, &ci_item(TriageStatus::New)));
        assert!(link_allows(&both, &sec_item(TriageSeverity::Critical)));
        assert!(link_allows(&both, &sec_item(TriageSeverity::High)));
        // Routine severity security items stay quiet.
        assert!(!link_allows(&both, &sec_item(TriageSeverity::Medium)));
        assert!(!link_allows(&both, &sec_item(TriageSeverity::Low)));

        let triage_only = link(true, false);
        assert!(!link_allows(
            &triage_only,
            &sec_item(TriageSeverity::Critical)
        ));

        let security_only = link(false, true);
        assert!(!link_allows(&security_only, &ci_item(TriageStatus::New)));
        assert!(link_allows(
            &security_only,
            &sec_item(TriageSeverity::Critical)
        ));
    }
}
