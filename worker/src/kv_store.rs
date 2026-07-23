//! Cloudflare KV-backed storage for tasks and headless state.
//!
//! Keys follow the pattern `{owner}/{repo}/tasks` for the full task list
//! and `{owner}/{repo}/state/{context}` for per-context headless state.
//! Values are stored as JSON strings.

use crate::types::{HeadlessState, InstallationSettings, PulseRegistration, Task};
use wreck_it_core::triage::TriageItem;

/// KV binding name expected in `wrangler.toml`.
pub const KV_BINDING: &str = "WRECK_IT_STORE";

/// KV key for the pulse registry (list of repos to iterate on cron).
const PULSE_REGISTRY_KEY: &str = "_pulse/repos";

/// Build the KV key for a repository's task list.
pub fn tasks_key(owner: &str, repo: &str) -> String {
    format!("{}/{}/tasks", owner, repo)
}

/// Build the KV key for a repository's headless state within a context.
pub fn state_key(owner: &str, repo: &str, context: &str) -> String {
    format!("{}/{}/state/{}", owner, repo, context)
}

/// Load all tasks from KV for the given repository.
///
/// Returns an empty `Vec` when the key does not exist.
pub async fn load_tasks(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
) -> Result<Vec<Task>, String> {
    let key = tasks_key(owner, repo);
    match kv.get(&key).text().await {
        Ok(Some(json)) => {
            serde_json::from_str(&json).map_err(|e| format!("failed to parse tasks JSON: {e}"))
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(format!("KV get failed for {key}: {e}")),
    }
}

/// Persist the full task list to KV, replacing any previous value.
pub async fn save_tasks(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    tasks: &[Task],
) -> Result<(), String> {
    let key = tasks_key(owner, repo);
    let json =
        serde_json::to_string(tasks).map_err(|e| format!("failed to serialize tasks: {e}"))?;
    kv.put(&key, json)
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))
}

/// Load headless state from KV for the given repository and context.
///
/// Returns `HeadlessState::default()` when the key does not exist.
pub async fn load_state(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    context: &str,
) -> Result<HeadlessState, String> {
    let key = state_key(owner, repo, context);
    match kv.get(&key).text().await {
        Ok(Some(json)) => {
            serde_json::from_str(&json).map_err(|e| format!("failed to parse state JSON: {e}"))
        }
        Ok(None) => Ok(HeadlessState::default()),
        Err(e) => Err(format!("KV get failed for {key}: {e}")),
    }
}

/// Persist headless state to KV.
pub async fn save_state(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    context: &str,
    state: &HeadlessState,
) -> Result<(), String> {
    let key = state_key(owner, repo, context);
    let json =
        serde_json::to_string(state).map_err(|e| format!("failed to serialize state: {e}"))?;
    kv.put(&key, json)
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))
}

/// Delete the task list key from KV.
#[allow(dead_code)]
pub async fn delete_tasks(kv: &worker::kv::KvStore, owner: &str, repo: &str) -> Result<(), String> {
    let key = tasks_key(owner, repo);
    kv.delete(&key)
        .await
        .map_err(|e| format!("KV delete failed for {key}: {e}"))
}

/// Delete a specific state key from KV.
pub async fn delete_state(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    context: &str,
) -> Result<(), String> {
    let key = state_key(owner, repo, context);
    kv.delete(&key)
        .await
        .map_err(|e| format!("KV delete failed for {key}: {e}"))
}

// ---------------------------------------------------------------------------
// Installation settings
// ---------------------------------------------------------------------------

/// Build the KV key for an installation's settings.
fn installation_settings_key(installation_id: u64) -> String {
    format!("_installation/{installation_id}/settings")
}

/// Load per-installation settings from KV.
///
/// Returns `InstallationSettings::default()` when no settings have been
/// saved yet.
pub async fn load_installation_settings(
    kv: &worker::kv::KvStore,
    installation_id: u64,
) -> Result<InstallationSettings, String> {
    let key = installation_settings_key(installation_id);
    match kv.get(&key).text().await {
        Ok(Some(json)) => serde_json::from_str(&json)
            .map_err(|e| format!("failed to parse installation settings JSON: {e}")),
        Ok(None) => Ok(InstallationSettings::default()),
        Err(e) => Err(format!("KV get failed for {key}: {e}")),
    }
}

/// Persist per-installation settings to KV.
pub async fn save_installation_settings(
    kv: &worker::kv::KvStore,
    installation_id: u64,
    settings: &InstallationSettings,
) -> Result<(), String> {
    let key = installation_settings_key(installation_id);
    let json = serde_json::to_string(settings)
        .map_err(|e| format!("failed to serialize installation settings: {e}"))?;
    kv.put(&key, json)
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))
}

// ---------------------------------------------------------------------------
// Triage items
// ---------------------------------------------------------------------------

/// Build the KV key for a repository's triage item list.
pub fn triage_key(owner: &str, repo: &str) -> String {
    format!("{}/{}/triage", owner, repo)
}

/// Load all triage items from KV for the given repository.
///
/// Returns an empty `Vec` when the key does not exist.
pub async fn load_triage(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
) -> Result<Vec<TriageItem>, String> {
    let key = triage_key(owner, repo);
    match kv.get(&key).text().await {
        Ok(Some(json)) => {
            serde_json::from_str(&json).map_err(|e| format!("failed to parse triage JSON: {e}"))
        }
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(format!("KV get failed for {key}: {e}")),
    }
}

/// Persist the full triage item list to KV, replacing any previous value.
///
/// Like the tasks document, this is a read-modify-write over a single JSON
/// document: concurrent webhook deliveries for the same repository can lose
/// an update.  That is accepted for v1 — a lost CI-failure upsert is
/// recreated by the next failing run — and the Durable Object backend
/// (spec 001) is the long-term fix.
pub async fn save_triage(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    items: &[TriageItem],
) -> Result<(), String> {
    let key = triage_key(owner, repo);
    let json =
        serde_json::to_string(items).map_err(|e| format!("failed to serialize triage: {e}"))?;
    kv.put(&key, json)
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))
}

// ---------------------------------------------------------------------------
// Slack integration
// ---------------------------------------------------------------------------

use crate::slack::{SlackChannelLink, SlackLinkRef, SlackWorkspace};

/// KV key for an installed Slack workspace.
pub fn slack_team_key(team_id: &str) -> String {
    format!("_slack/team/{team_id}")
}

/// KV key for a channel→repo link.
pub fn slack_link_key(team_id: &str, channel_id: &str) -> String {
    format!("_slack/link/{team_id}/{channel_id}")
}

/// KV key for a repository's reverse index of linked channels.
pub fn slack_links_index_key(owner: &str, repo: &str) -> String {
    format!("{owner}/{repo}/slack_links")
}

/// KV key marking a processed Slack event id (retry dedup, 1h TTL).
pub fn slack_event_key(event_id: &str) -> String {
    format!("_slack/event/{event_id}")
}

async fn load_json<T: serde::de::DeserializeOwned>(
    kv: &worker::kv::KvStore,
    key: &str,
) -> Result<Option<T>, String> {
    match kv.get(key).text().await {
        Ok(Some(json)) => serde_json::from_str(&json)
            .map(Some)
            .map_err(|e| format!("failed to parse {key}: {e}")),
        Ok(None) => Ok(None),
        Err(e) => Err(format!("KV get failed for {key}: {e}")),
    }
}

async fn save_json<T: serde::Serialize>(
    kv: &worker::kv::KvStore,
    key: &str,
    value: &T,
) -> Result<(), String> {
    let json = serde_json::to_string(value).map_err(|e| format!("failed to serialize: {e}"))?;
    kv.put(key, json)
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))
}

/// Load an installed Slack workspace by team id.
pub async fn load_slack_workspace(
    kv: &worker::kv::KvStore,
    team_id: &str,
) -> Result<Option<SlackWorkspace>, String> {
    load_json(kv, &slack_team_key(team_id)).await
}

/// Persist an installed Slack workspace.
pub async fn save_slack_workspace(
    kv: &worker::kv::KvStore,
    workspace: &SlackWorkspace,
) -> Result<(), String> {
    save_json(kv, &slack_team_key(&workspace.team_id), workspace).await
}

/// Load a channel→repo link.
pub async fn load_slack_link(
    kv: &worker::kv::KvStore,
    team_id: &str,
    channel_id: &str,
) -> Result<Option<SlackChannelLink>, String> {
    load_json(kv, &slack_link_key(team_id, channel_id)).await
}

/// Persist a channel→repo link and maintain the repo's reverse index.
///
/// The link is written first, the reverse index second; a crash in between
/// leaves an orphaned link that outbound notification simply never finds —
/// tolerated rather than transactional (KV has no transactions).
pub async fn save_slack_link(
    kv: &worker::kv::KvStore,
    team_id: &str,
    channel_id: &str,
    link: &SlackChannelLink,
) -> Result<(), String> {
    save_json(kv, &slack_link_key(team_id, channel_id), link).await?;

    let index_key = slack_links_index_key(&link.owner, &link.repo);
    let mut index: Vec<SlackLinkRef> = load_json(kv, &index_key).await?.unwrap_or_default();
    if !index
        .iter()
        .any(|r| r.team_id == team_id && r.channel_id == channel_id)
    {
        index.push(SlackLinkRef {
            team_id: team_id.to_string(),
            channel_id: channel_id.to_string(),
        });
        save_json(kv, &index_key, &index).await?;
    }
    Ok(())
}

/// Remove a channel→repo link (and its reverse-index entry).
pub async fn delete_slack_link(
    kv: &worker::kv::KvStore,
    team_id: &str,
    channel_id: &str,
) -> Result<bool, String> {
    let link: Option<SlackChannelLink> =
        load_json(kv, &slack_link_key(team_id, channel_id)).await?;
    let link = match link {
        Some(l) => l,
        None => return Ok(false),
    };
    kv.delete(&slack_link_key(team_id, channel_id))
        .await
        .map_err(|e| format!("KV delete failed: {e}"))?;

    let index_key = slack_links_index_key(&link.owner, &link.repo);
    let mut index: Vec<SlackLinkRef> = load_json(kv, &index_key).await?.unwrap_or_default();
    let before = index.len();
    index.retain(|r| !(r.team_id == team_id && r.channel_id == channel_id));
    if index.len() != before {
        save_json(kv, &index_key, &index).await?;
    }
    Ok(true)
}

/// Load a repository's linked channels (reverse index).
pub async fn load_slack_links_for_repo(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
) -> Result<Vec<SlackLinkRef>, String> {
    Ok(load_json(kv, &slack_links_index_key(owner, repo))
        .await?
        .unwrap_or_default())
}

/// KV key for the index of installed Slack teams (KV cannot enumerate keys
/// without `list()`, which this codebase avoids).
const SLACK_TEAMS_INDEX_KEY: &str = "_slack/teams";

/// Summary entry in the installed-teams index.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct SlackTeamRef {
    pub team_id: String,
    pub team_name: String,
}

/// Load the installed-teams index.
pub async fn load_slack_teams(kv: &worker::kv::KvStore) -> Result<Vec<SlackTeamRef>, String> {
    Ok(load_json(kv, SLACK_TEAMS_INDEX_KEY)
        .await?
        .unwrap_or_default())
}

/// Upsert a team into the installed-teams index.
pub async fn upsert_slack_team(
    kv: &worker::kv::KvStore,
    team_id: &str,
    team_name: &str,
) -> Result<(), String> {
    let mut teams = load_slack_teams(kv).await?;
    if let Some(existing) = teams.iter_mut().find(|t| t.team_id == team_id) {
        existing.team_name = team_name.to_string();
    } else {
        teams.push(SlackTeamRef {
            team_id: team_id.to_string(),
            team_name: team_name.to_string(),
        });
    }
    save_json(kv, SLACK_TEAMS_INDEX_KEY, &teams).await
}

/// Record a Slack event id as processed (1h TTL).  Returns `false` when the
/// id was already recorded — the caller should skip the duplicate delivery.
pub async fn mark_slack_event_processed(
    kv: &worker::kv::KvStore,
    event_id: &str,
) -> Result<bool, String> {
    let key = slack_event_key(event_id);
    match kv.get(&key).text().await {
        Ok(Some(_)) => return Ok(false),
        Ok(None) => {}
        Err(e) => return Err(format!("KV get failed for {key}: {e}")),
    }
    kv.put(&key, "1")
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .expiration_ttl(3600)
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))?;
    Ok(true)
}

// ---------------------------------------------------------------------------
// Pulse registry
// ---------------------------------------------------------------------------

/// Load the pulse registry from KV.
///
/// Returns an empty `Vec` when the key does not exist.
pub async fn load_pulse_registry(
    kv: &worker::kv::KvStore,
) -> Result<Vec<PulseRegistration>, String> {
    match kv.get(PULSE_REGISTRY_KEY).text().await {
        Ok(Some(json)) => serde_json::from_str(&json)
            .map_err(|e| format!("failed to parse pulse registry JSON: {e}")),
        Ok(None) => Ok(Vec::new()),
        Err(e) => Err(format!("KV get failed for {PULSE_REGISTRY_KEY}: {e}")),
    }
}

/// Persist the pulse registry to KV, replacing any previous value.
pub async fn save_pulse_registry(
    kv: &worker::kv::KvStore,
    registrations: &[PulseRegistration],
) -> Result<(), String> {
    let json = serde_json::to_string(registrations)
        .map_err(|e| format!("failed to serialize pulse registry: {e}"))?;
    kv.put(PULSE_REGISTRY_KEY, json)
        .map_err(|e| format!("KV put build failed for {PULSE_REGISTRY_KEY}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {PULSE_REGISTRY_KEY}: {e}"))
}

/// Register (upsert) a repository in the pulse registry.
///
/// If a registration for the same `owner/repo` already exists, its
/// `installation_id` and `default_branch` are updated.  Otherwise a new
/// entry is appended.
pub async fn upsert_pulse_registration(
    kv: &worker::kv::KvStore,
    reg: &PulseRegistration,
) -> Result<(), String> {
    let mut regs = load_pulse_registry(kv).await?;
    if let Some(existing) = regs
        .iter_mut()
        .find(|r| r.owner == reg.owner && r.repo == reg.repo)
    {
        existing.installation_id = reg.installation_id;
        existing.default_branch = reg.default_branch.clone();
    } else {
        regs.push(reg.clone());
    }
    save_pulse_registry(kv, &regs).await
}

/// Remove a repository from the pulse registry.
///
/// Returns `true` if the entry was found and removed.
pub async fn remove_pulse_registration(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
) -> Result<bool, String> {
    let mut regs = load_pulse_registry(kv).await?;
    let len_before = regs.len();
    regs.retain(|r| !(r.owner == owner && r.repo == repo));
    if regs.len() == len_before {
        return Ok(false);
    }
    save_pulse_registry(kv, &regs).await?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tasks_key_format() {
        assert_eq!(tasks_key("octo", "repo"), "octo/repo/tasks");
    }

    #[test]
    fn state_key_format() {
        assert_eq!(
            state_key("octo", "repo", "default"),
            "octo/repo/state/default"
        );
    }

    #[test]
    fn state_key_named_context() {
        assert_eq!(state_key("octo", "repo", "docs"), "octo/repo/state/docs");
    }

    #[test]
    fn triage_key_format() {
        assert_eq!(triage_key("octo", "repo"), "octo/repo/triage");
    }

    #[test]
    fn slack_key_formats() {
        assert_eq!(slack_team_key("T123"), "_slack/team/T123");
        assert_eq!(slack_link_key("T123", "C9"), "_slack/link/T123/C9");
        assert_eq!(
            slack_links_index_key("octo", "repo"),
            "octo/repo/slack_links"
        );
        assert_eq!(slack_event_key("Ev1"), "_slack/event/Ev1");
    }

    #[test]
    fn pulse_registry_key_is_underscore_prefixed() {
        // The pulse registry key should not collide with repo keys.
        assert!(PULSE_REGISTRY_KEY.starts_with('_'));
    }

    #[test]
    fn installation_settings_key_format() {
        let key = installation_settings_key(42);
        assert_eq!(key, "_installation/42/settings");
    }

    #[test]
    fn installation_settings_key_large_id() {
        let key = installation_settings_key(123456789);
        assert_eq!(key, "_installation/123456789/settings");
    }

    #[test]
    fn installation_settings_key_is_underscore_prefixed() {
        let key = installation_settings_key(1);
        assert!(key.starts_with('_'));
    }
}
