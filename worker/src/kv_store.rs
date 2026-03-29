//! Cloudflare KV-backed storage for tasks and headless state.
//!
//! Keys follow the pattern `{owner}/{repo}/tasks` for the full task list
//! and `{owner}/{repo}/state/{context}` for per-context headless state.
//! Values are stored as JSON strings.

use crate::types::{HeadlessState, Task};

/// KV binding name expected in `wrangler.toml`.
pub const KV_BINDING: &str = "WRECK_IT_STORE";

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
        Ok(Some(json)) => serde_json::from_str(&json)
            .map_err(|e| format!("failed to parse tasks JSON: {e}")),
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
    let json = serde_json::to_string(tasks)
        .map_err(|e| format!("failed to serialize tasks: {e}"))?;
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
        Ok(Some(json)) => serde_json::from_str(&json)
            .map_err(|e| format!("failed to parse state JSON: {e}")),
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
    let json = serde_json::to_string(state)
        .map_err(|e| format!("failed to serialize state: {e}"))?;
    kv.put(&key, json)
        .map_err(|e| format!("KV put build failed for {key}: {e}"))?
        .execute()
        .await
        .map_err(|e| format!("KV put execute failed for {key}: {e}"))
}

/// Delete the task list key from KV.
#[allow(dead_code)]
pub async fn delete_tasks(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
) -> Result<(), String> {
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
        assert_eq!(
            state_key("octo", "repo", "docs"),
            "octo/repo/state/docs"
        );
    }
}
