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

/// A pull request being actively tracked by the headless runner.
///
/// Multiple PRs may be in flight at once (e.g. from different agent sessions).
/// The runner persists this list so it can manage all of them across cron
/// invocations — converting drafts, approving workflow runs, and merging.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TrackedPr {
    /// Pull request number on GitHub.
    pub pr_number: u64,
    /// The wreck-it task ID associated with this PR.
    pub task_id: String,
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

    /// GitHub issue number created to trigger the cloud agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,

    /// PR number created by the cloud agent (if any).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_number: Option<u64>,

    /// URL of the PR created by the cloud agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr_url: Option<String>,

    /// The last prompt sent to the agent.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_prompt: Option<String>,

    /// Freeform memory that persists across invocations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub memory: Vec<String>,

    /// All pull requests being actively managed by the headless runner.
    /// Populated during the sweep phase and persisted between invocations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_prs: Vec<TrackedPr>,
}

impl Default for HeadlessState {
    fn default() -> Self {
        Self {
            phase: AgentPhase::NeedsTrigger,
            iteration: 0,
            current_task_id: None,
            issue_number: None,
            pr_number: None,
            pr_url: None,
            last_prompt: None,
            memory: Vec::new(),
            tracked_prs: Vec::new(),
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
            issue_number: Some(99),
            pr_number: Some(42),
            pr_url: Some("https://github.com/o/r/pull/42".to_string()),
            last_prompt: Some("implement feature X".to_string()),
            memory: vec!["context note".to_string()],
            tracked_prs: vec![TrackedPr {
                pr_number: 42,
                task_id: "task-1".to_string(),
            }],
        };

        save_headless_state(&state_file, &state).unwrap();
        let loaded = load_headless_state(&state_file).unwrap();

        assert_eq!(loaded.phase, AgentPhase::AgentWorking);
        assert_eq!(loaded.iteration, 3);
        assert_eq!(loaded.current_task_id.as_deref(), Some("task-1"));
        assert_eq!(loaded.issue_number, Some(99));
        assert_eq!(loaded.pr_number, Some(42));
        assert_eq!(
            loaded.pr_url.as_deref(),
            Some("https://github.com/o/r/pull/42")
        );
        assert_eq!(loaded.last_prompt.as_deref(), Some("implement feature X"));
        assert_eq!(loaded.memory, vec!["context note".to_string()]);
        assert_eq!(loaded.tracked_prs.len(), 1);
        assert_eq!(loaded.tracked_prs[0].pr_number, 42);
        assert_eq!(loaded.tracked_prs[0].task_id, "task-1");
    }

    #[test]
    fn test_default_headless_state() {
        let state = HeadlessState::default();
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert_eq!(state.iteration, 0);
        assert!(state.current_task_id.is_none());
        assert!(state.issue_number.is_none());
        assert!(state.pr_number.is_none());
        assert!(state.pr_url.is_none());
        assert!(state.last_prompt.is_none());
        assert!(state.memory.is_empty());
        assert!(state.tracked_prs.is_empty());
    }

    #[test]
    fn test_tracked_prs_backward_compat() {
        // Existing state files without tracked_prs should load fine.
        let dir = tempdir().unwrap();
        let state_file = dir.path().join(".wreck-it-state.json");
        fs::write(&state_file, r#"{"phase":"needs_trigger","iteration":5}"#).unwrap();

        let loaded = load_headless_state(&state_file).unwrap();
        assert_eq!(loaded.phase, AgentPhase::NeedsTrigger);
        assert_eq!(loaded.iteration, 5);
        assert!(loaded.tracked_prs.is_empty());
    }

    #[test]
    fn test_tracked_pr_roundtrip() {
        let pr = TrackedPr {
            pr_number: 99,
            task_id: "impl-3".to_string(),
        };
        let json = serde_json::to_string(&pr).unwrap();
        let loaded: TrackedPr = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, pr);
    }
}
