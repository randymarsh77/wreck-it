//! Cloudflare KV-backed storage for tasks and headless state.
//!
//! Keys follow the pattern `{owner}/{repo}/tasks` for the full task list
//! and `{owner}/{repo}/state/{context}` for per-context headless state.
//! Values are stored as JSON strings.

use crate::types::{HeadlessState, PulseRegistration, Task};

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
    fn pulse_registry_key_is_underscore_prefixed() {
        // The pulse registry key should not collide with repo keys.
        assert!(PULSE_REGISTRY_KEY.starts_with('_'));
    }
}
