use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::Path;

/// Phases of a headless cloud agent iteration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentPhase {
    /// No agent work in progress; a new task should be dispatched.
    NeedsTrigger,
    /// The cloud agent has been triggered and is still working.
    AgentWorking,
    /// The agent finished; its output (e.g. a PR) should be verified.
    NeedsVerification,
    /// Verification passed; ready for the next task or done.
    Completed,
}

/// Persistent state that is committed to the repo between cron invocations.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessState {
    /// Current phase of the cloud agent cycle.
    pub phase: AgentPhase,

    /// The iteration counter across cron invocations.
    pub iteration: usize,

    /// ID of the task currently being worked on.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_task_id: Option<String>,

    /// PR number created by the cloud agent (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,

    /// The last prompt sent to the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,

    /// Freeform memory that persists across invocations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<String>,
}

impl Default for HeadlessState {
    fn default() -> Self {
        Self {
            phase: AgentPhase::NeedsTrigger,
            iteration: 0,
            current_task_id: None,
            pr_number: None,
            last_prompt: None,
            memory: Vec::new(),
        }
    }
}

/// Load headless state from a JSON file. Returns default state if the file
/// does not exist.
pub fn load_headless_state(path: &Path) -> Result<HeadlessState> {
    if !path.exists() {
        return Ok(HeadlessState::default());
    }
    let content = fs::read_to_string(path).context("Failed to read headless state file")?;
    let state = serde_json::from_str(&content).context("Failed to parse headless state file")?;
    Ok(state)
}

/// Save headless state to a JSON file.
pub fn save_headless_state(path: &Path, state: &HeadlessState) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create state directory")?;
    }
    let content =
        serde_json::to_string_pretty(state).context("Failed to serialize headless state")?;
    fs::write(path, content).context("Failed to write headless state file")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_headless_state_defaults_when_missing() {
        let dir = tempdir().unwrap();
        let state_file = dir.path().join(".wreck-it-state.json");
        let state = load_headless_state(&state_file).unwrap();
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert_eq!(state.iteration, 0);
    }

    #[test]
    fn test_save_and_load_headless_state() {
        let dir = tempdir().unwrap();
        let state_file = dir.path().join(".wreck-it-state.json");
        let state = HeadlessState {
            phase: AgentPhase::AgentWorking,
            iteration: 3,
            current_task_id: Some("task-1".to_string()),
            pr_number: Some(42),
            last_prompt: Some("implement feature X".to_string()),
            memory: vec!["context note".to_string()],
        };

        save_headless_state(&state_file, &state).unwrap();
        let loaded = load_headless_state(&state_file).unwrap();

        assert_eq!(loaded.phase, AgentPhase::AgentWorking);
        assert_eq!(loaded.iteration, 3);
        assert_eq!(loaded.current_task_id.as_deref(), Some("task-1"));
        assert_eq!(loaded.pr_number, Some(42));
        assert_eq!(loaded.last_prompt.as_deref(), Some("implement feature X"));
        assert_eq!(loaded.memory, vec!["context note".to_string()]);
    }

    #[test]
    fn test_default_headless_state() {
        let state = HeadlessState::default();
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert_eq!(state.iteration, 0);
        assert!(state.current_task_id.is_none());
        assert!(state.pr_number.is_none());
        assert!(state.last_prompt.is_none());
        assert!(state.memory.is_empty());
    }
}
