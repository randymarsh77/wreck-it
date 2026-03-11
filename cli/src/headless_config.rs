use crate::types::EvaluationMode;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

/// Configuration for headless/CI mode, intended to be committed to the repo
/// (e.g. `.wreck-it.toml`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeadlessConfig {
    /// Path to the task file relative to the repo root.
    #[serde(default = "default_task_file")]
    pub task_file: PathBuf,

    /// Maximum number of loop iterations.
    #[serde(default = "default_max_iterations")]
    pub max_iterations: usize,

    /// Shell command used to verify task completion (trusted input only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verify_command: Option<String>,

    /// Path where headless state is persisted between invocations.
    #[serde(default = "default_state_file")]
    pub state_file: PathBuf,

    /// How completeness is evaluated.
    #[serde(default)]
    pub evaluation_mode: EvaluationMode,

    /// Prompt describing completeness for the evaluation agent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completeness_prompt: Option<String>,

    /// Marker file the evaluation agent writes when the task is complete.
    #[serde(default = "default_completion_marker")]
    pub completion_marker_file: PathBuf,

    /// GitHub repository owner (org or user).  When not set, derived from
    /// the `origin` git remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_owner: Option<String>,

    /// GitHub repository name.  When not set, derived from the `origin` git
    /// remote.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,

    /// Git branch used to persist wreck-it state (config, tasks, state file).
    /// Defaults to `wreck-it-state`.  Locally, a git worktree is checked out
    /// to this branch so state I/O never touches the main working tree.
    #[serde(default = "default_state_branch")]
    pub state_branch: String,
}

fn default_task_file() -> PathBuf {
    PathBuf::from("tasks.json")
}

fn default_max_iterations() -> usize {
    100
}

fn default_state_file() -> PathBuf {
    PathBuf::from(".wreck-it-state.json")
}

fn default_completion_marker() -> PathBuf {
    PathBuf::from(crate::types::DEFAULT_COMPLETION_MARKER)
}

fn default_state_branch() -> String {
    crate::state_worktree::DEFAULT_STATE_BRANCH.to_string()
}

impl Default for HeadlessConfig {
    fn default() -> Self {
        Self {
            task_file: default_task_file(),
            max_iterations: default_max_iterations(),
            verify_command: None,
            state_file: default_state_file(),
            evaluation_mode: EvaluationMode::default(),
            completeness_prompt: None,
            completion_marker_file: default_completion_marker(),
            repo_owner: None,
            repo_name: None,
            state_branch: default_state_branch(),
        }
    }
}

/// Load a headless config from a TOML file.
pub fn load_headless_config(path: &Path) -> Result<HeadlessConfig> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("Failed to read headless config file: {}", path.display()))?;
    let config: HeadlessConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse headless config file: {}", path.display()))?;
    Ok(config)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_load_headless_config_full() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join(".wreck-it.toml");
        fs::write(
            &config_file,
            r#"
task_file = "my-tasks.json"
max_iterations = 50
verify_command = "cargo test"
state_file = ".my-state.json"
"#,
        )
        .unwrap();

        let config = load_headless_config(&config_file).unwrap();
        assert_eq!(config.task_file, PathBuf::from("my-tasks.json"));
        assert_eq!(config.max_iterations, 50);
        assert_eq!(config.verify_command.as_deref(), Some("cargo test"));
        assert_eq!(config.state_file, PathBuf::from(".my-state.json"));
    }

    #[test]
    fn test_load_headless_config_defaults() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join(".wreck-it.toml");
        fs::write(&config_file, "").unwrap();

        let config = load_headless_config(&config_file).unwrap();
        assert_eq!(config.task_file, PathBuf::from("tasks.json"));
        assert_eq!(config.max_iterations, 100);
        assert!(config.verify_command.is_none());
        assert_eq!(config.state_file, PathBuf::from(".wreck-it-state.json"));
    }

    #[test]
    fn test_default_headless_config() {
        let config = HeadlessConfig::default();
        assert_eq!(config.task_file, PathBuf::from("tasks.json"));
        assert_eq!(config.max_iterations, 100);
        assert!(config.verify_command.is_none());
        assert_eq!(config.state_file, PathBuf::from(".wreck-it-state.json"));
        assert_eq!(config.evaluation_mode, EvaluationMode::Command);
        assert!(config.completeness_prompt.is_none());
        assert_eq!(
            config.completion_marker_file,
            PathBuf::from(crate::types::DEFAULT_COMPLETION_MARKER)
        );
        assert!(config.repo_owner.is_none());
        assert!(config.repo_name.is_none());
        assert_eq!(config.state_branch, "wreck-it-state");
    }

    #[test]
    fn test_load_headless_config_with_agent_file_evaluation() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join(".wreck-it.toml");
        fs::write(
            &config_file,
            r#"
evaluation_mode = "agent_file"
completeness_prompt = "Verify all tests pass and code compiles"
completion_marker_file = ".done"
"#,
        )
        .unwrap();

        let config = load_headless_config(&config_file).unwrap();
        assert_eq!(config.evaluation_mode, EvaluationMode::AgentFile);
        assert_eq!(
            config.completeness_prompt.as_deref(),
            Some("Verify all tests pass and code compiles")
        );
        assert_eq!(config.completion_marker_file, PathBuf::from(".done"));
    }

    #[test]
    fn test_load_headless_config_defaults_evaluation_mode() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join(".wreck-it.toml");
        fs::write(&config_file, "").unwrap();

        let config = load_headless_config(&config_file).unwrap();
        assert_eq!(config.evaluation_mode, EvaluationMode::Command);
        assert!(config.completeness_prompt.is_none());
    }

    #[test]
    fn test_load_headless_config_with_semantic_evaluation() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join(".wreck-it.toml");
        fs::write(
            &config_file,
            r#"
evaluation_mode = "semantic"
completeness_prompt = "All acceptance criteria must be addressed in the diff"
"#,
        )
        .unwrap();

        let config = load_headless_config(&config_file).unwrap();
        assert_eq!(config.evaluation_mode, EvaluationMode::Semantic);
        assert_eq!(
            config.completeness_prompt.as_deref(),
            Some("All acceptance criteria must be addressed in the diff")
        );
    }

    #[test]
    fn test_load_headless_config_with_repo_info() {
        let dir = tempdir().unwrap();
        let config_file = dir.path().join(".wreck-it.toml");
        fs::write(
            &config_file,
            r#"
repo_owner = "octocat"
repo_name = "hello-world"
"#,
        )
        .unwrap();

        let config = load_headless_config(&config_file).unwrap();
        assert_eq!(config.repo_owner.as_deref(), Some("octocat"));
        assert_eq!(config.repo_name.as_deref(), Some("hello-world"));
    }
}
