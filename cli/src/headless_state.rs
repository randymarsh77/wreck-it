use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

// Re-export shared types from wreck-it-core so that the rest of the crate
// can continue to use `crate::headless_state::AgentPhase`, etc.
pub use wreck_it_core::state::{AgentPhase, HeadlessState, TrackedPr};

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
                issue_number: Some(99),
                review_requested: None,
            }],
            review_requested: None,
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
            issue_number: Some(42),
            review_requested: None,
        };
        let json = serde_json::to_string(&pr).unwrap();
        let loaded: TrackedPr = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, pr);
    }

    #[test]
    fn test_tracked_pr_issue_number_backward_compat() {
        // Existing state files without issue_number should load fine.
        let json = r#"{"pr_number":10,"task_id":"impl-1"}"#;
        let loaded: TrackedPr = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.pr_number, 10);
        assert_eq!(loaded.task_id, "impl-1");
        assert_eq!(loaded.issue_number, None);
    }

    #[test]
    fn test_tracked_pr_issue_number_none_omitted_from_json() {
        // When issue_number is None, it should not appear in serialized JSON.
        let pr = TrackedPr {
            pr_number: 5,
            task_id: "eval-1".to_string(),
            issue_number: None,
            review_requested: None,
        };
        let json = serde_json::to_string(&pr).unwrap();
        assert!(!json.contains("issue_number"));
    }

    #[test]
    fn test_tracked_pr_with_issue_number_roundtrip() {
        let pr = TrackedPr {
            pr_number: 50,
            task_id: "ideas-2".to_string(),
            issue_number: Some(100),
            review_requested: None,
        };
        let json = serde_json::to_string(&pr).unwrap();
        assert!(json.contains("\"issue_number\":100"));
        let loaded: TrackedPr = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.issue_number, Some(100));
    }

    #[test]
    fn test_tracked_pr_review_requested_backward_compat() {
        // Existing state files without review_requested should load fine.
        let json = r#"{"pr_number":10,"task_id":"impl-1"}"#;
        let loaded: TrackedPr = serde_json::from_str(json).unwrap();
        assert_eq!(loaded.review_requested, None);
    }

    #[test]
    fn test_tracked_pr_review_requested_roundtrip() {
        let pr = TrackedPr {
            pr_number: 50,
            task_id: "ideas-2".to_string(),
            issue_number: None,
            review_requested: Some(true),
        };
        let json = serde_json::to_string(&pr).unwrap();
        assert!(json.contains("review_requested"));
        let loaded: TrackedPr = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.review_requested, Some(true));
    }

    #[test]
    fn test_headless_state_review_requested_backward_compat() {
        // Existing state files without review_requested should load fine.
        let dir = tempdir().unwrap();
        let state_file = dir.path().join(".wreck-it-state.json");
        fs::write(
            &state_file,
            r#"{"phase":"agent_working","iteration":2,"pr_number":5}"#,
        )
        .unwrap();

        let loaded = load_headless_state(&state_file).unwrap();
        assert_eq!(loaded.phase, AgentPhase::AgentWorking);
        assert!(loaded.review_requested.is_none());
    }

    #[test]
    fn test_awaiting_review_phase_roundtrip() {
        let dir = tempdir().unwrap();
        let state_file = dir.path().join(".wreck-it-state.json");
        let state = HeadlessState {
            phase: AgentPhase::AwaitingReview,
            iteration: 4,
            current_task_id: Some("task-1".to_string()),
            issue_number: Some(10),
            pr_number: Some(20),
            pr_url: None,
            last_prompt: None,
            memory: vec![],
            tracked_prs: vec![],
            review_requested: Some(true),
        };

        save_headless_state(&state_file, &state).unwrap();
        let loaded = load_headless_state(&state_file).unwrap();

        assert_eq!(loaded.phase, AgentPhase::AwaitingReview);
        assert_eq!(loaded.review_requested, Some(true));
    }
}
