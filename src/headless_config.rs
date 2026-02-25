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

impl Default for HeadlessConfig {
    fn default() -> Self {
        Self {
            task_file: default_task_file(),
            max_iterations: default_max_iterations(),
            verify_command: None,
            state_file: default_state_file(),
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
    }
}
