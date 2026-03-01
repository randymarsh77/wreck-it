use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

/// Directory on the main branch that holds the wreck-it repo config.
pub const CONFIG_DIR: &str = ".wreck-it";

/// Name of the repo-level config file inside [`CONFIG_DIR`].
pub const CONFIG_FILE: &str = "config.toml";

/// Repository-level wreck-it configuration.
///
/// This file lives on the main branch (the branch where `wreck-it init` was
/// run) at `.wreck-it/config.toml`.  It tells wreck-it where to find its
/// state data so that state changes are isolated from the code being worked on.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RepoConfig {
    /// Git branch used to track state files.
    #[serde(default = "default_state_branch")]
    pub state_branch: String,

    /// Root directory for state files (inside the state worktree).
    #[serde(default = "default_state_root")]
    pub state_root: String,

    /// Named ralph contexts.  Each entry defines an independent long-running
    /// loop with its own task file, state file, and optional scheduling config.
    /// When empty, wreck-it falls back to the default single-ralph behaviour
    /// (task file and state file come from the headless config or CLI flags).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ralphs: Vec<RalphConfig>,
}

/// Configuration for a named ralph context.
///
/// A repository can declare multiple ralphs in `.wreck-it/config.toml` to
/// manage parallel persistent loops — for example one that maintains
/// documentation and another that monitors test coverage.  Each ralph has its
/// own task file and state file so the loops are fully independent.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct RalphConfig {
    /// Unique name for this ralph context (e.g. `"docs"`, `"coverage"`).
    pub name: String,

    /// Path to the task file, relative to the state root.
    #[serde(default = "default_ralph_task_file")]
    pub task_file: String,

    /// Path to the persistent state file, relative to the state root.
    #[serde(default = "default_ralph_state_file")]
    pub state_file: String,
}

fn default_ralph_task_file() -> String {
    "tasks.json".to_string()
}

fn default_ralph_state_file() -> String {
    ".wreck-it-state.json".to_string()
}

fn default_state_branch() -> String {
    crate::state_worktree::DEFAULT_STATE_BRANCH.to_string()
}

fn default_state_root() -> String {
    CONFIG_DIR.to_string()
}

impl Default for RepoConfig {
    fn default() -> Self {
        Self {
            state_branch: default_state_branch(),
            state_root: default_state_root(),
            ralphs: Vec::new(),
        }
    }
}

/// Return the path to the `.wreck-it` directory in the repo root.
pub fn repo_config_dir(repo_root: &Path) -> PathBuf {
    repo_root.join(CONFIG_DIR)
}

/// Return the path to `.wreck-it/config.toml`.
pub fn repo_config_path(repo_root: &Path) -> PathBuf {
    repo_config_dir(repo_root).join(CONFIG_FILE)
}

/// Load the repo-level config.  Returns `None` if the file does not exist.
pub fn load_repo_config(repo_root: &Path) -> Result<Option<RepoConfig>> {
    let path = repo_config_path(repo_root);
    if !path.exists() {
        return Ok(None);
    }
    let content = std::fs::read_to_string(&path)
        .with_context(|| format!("Failed to read repo config: {}", path.display()))?;
    let config: RepoConfig = toml::from_str(&content)
        .with_context(|| format!("Failed to parse repo config: {}", path.display()))?;
    Ok(Some(config))
}

/// Write the repo-level config to `.wreck-it/config.toml`.
pub fn save_repo_config(repo_root: &Path, config: &RepoConfig) -> Result<()> {
    let dir = repo_config_dir(repo_root);
    std::fs::create_dir_all(&dir).context("Failed to create .wreck-it directory")?;
    let path = repo_config_path(repo_root);
    let content = toml::to_string_pretty(config).context("Failed to serialize repo config")?;
    std::fs::write(&path, content).context("Failed to write repo config")?;
    Ok(())
}

/// Return `true` when stdin and stdout are both connected to a terminal.
pub fn is_interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}

/// Look up a named ralph context from the repo config.
///
/// Returns `None` if the config has no `[[ralphs]]` entry with the given name.
pub fn find_ralph<'a>(config: &'a RepoConfig, name: &str) -> Option<&'a RalphConfig> {
    config.ralphs.iter().find(|r| r.name == name)
}

/// Prompt the user for a value, showing a default.  Returns the default when
/// the user presses Enter without typing anything.
pub fn prompt_with_default(prompt: &str, default: &str) -> String {
    print!("{} [{}]: ", prompt, default);
    let _ = io::stdout().flush();
    let mut input = String::new();
    if io::stdin().read_line(&mut input).is_err() {
        return default.to_string();
    }
    let trimmed = input.trim();
    if trimmed.is_empty() {
        default.to_string()
    } else {
        trimmed.to_string()
    }
}

/// Return `true` when the state worktree contains no files besides `.git`,
/// indicating that no state has been written yet.
pub fn is_state_uninitialized(state_dir: &Path) -> bool {
    let entries = match std::fs::read_dir(state_dir) {
        Ok(e) => e,
        Err(_) => return true,
    };
    for entry in entries.flatten() {
        if entry.file_name() != ".git" {
            return false;
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_default_repo_config() {
        let cfg = RepoConfig::default();
        assert_eq!(cfg.state_branch, "wreck-it-state");
        assert_eq!(cfg.state_root, ".wreck-it");
        assert!(cfg.ralphs.is_empty());
    }

    #[test]
    fn test_repo_config_roundtrip() {
        let cfg = RepoConfig {
            state_branch: "my-state".to_string(),
            state_root: ".my-state-dir".to_string(),
            ralphs: vec![],
        };
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let loaded: RepoConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded, cfg);
    }

    #[test]
    fn test_save_and_load_repo_config() {
        let dir = tempdir().unwrap();
        let cfg = RepoConfig {
            state_branch: "custom-branch".to_string(),
            state_root: ".custom-root".to_string(),
            ralphs: vec![],
        };
        save_repo_config(dir.path(), &cfg).unwrap();
        let loaded = load_repo_config(dir.path()).unwrap();
        assert_eq!(loaded, Some(cfg));
    }

    #[test]
    fn test_load_repo_config_returns_none_when_missing() {
        let dir = tempdir().unwrap();
        let loaded = load_repo_config(dir.path()).unwrap();
        assert_eq!(loaded, None);
    }

    #[test]
    fn test_repo_config_defaults_for_missing_fields() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join(CONFIG_FILE), "").unwrap();

        let loaded = load_repo_config(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.state_branch, "wreck-it-state");
        assert_eq!(loaded.state_root, ".wreck-it");
    }

    #[test]
    fn test_repo_config_path() {
        let root = Path::new("/some/repo");
        assert_eq!(
            repo_config_path(root),
            PathBuf::from("/some/repo/.wreck-it/config.toml")
        );
    }

    #[test]
    fn test_is_state_uninitialized_true_for_git_only() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        assert!(is_state_uninitialized(dir.path()));
    }

    #[test]
    fn test_is_state_uninitialized_false_with_files() {
        let dir = tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".git")).unwrap();
        std::fs::write(dir.path().join("tasks.json"), "[]").unwrap();
        assert!(!is_state_uninitialized(dir.path()));
    }

    #[test]
    fn test_is_state_uninitialized_true_for_nonexistent() {
        assert!(is_state_uninitialized(Path::new("/nonexistent/path")));
    }

    // ---- RalphConfig / multi-ralph tests ----

    #[test]
    fn test_repo_config_with_ralphs_roundtrip() {
        let cfg = RepoConfig {
            state_branch: "wreck-it-state".to_string(),
            state_root: ".wreck-it".to_string(),
            ralphs: vec![
                RalphConfig {
                    name: "docs".to_string(),
                    task_file: "docs-tasks.json".to_string(),
                    state_file: ".docs-state.json".to_string(),
                },
                RalphConfig {
                    name: "coverage".to_string(),
                    task_file: "coverage-tasks.json".to_string(),
                    state_file: ".coverage-state.json".to_string(),
                },
            ],
        };
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        let loaded: RepoConfig = toml::from_str(&toml_str).unwrap();
        assert_eq!(loaded, cfg);
        assert_eq!(loaded.ralphs.len(), 2);
        assert_eq!(loaded.ralphs[0].name, "docs");
        assert_eq!(loaded.ralphs[1].name, "coverage");
    }

    #[test]
    fn test_repo_config_ralphs_default_to_empty() {
        let dir = tempdir().unwrap();
        let config_dir = dir.path().join(CONFIG_DIR);
        std::fs::create_dir_all(&config_dir).unwrap();
        std::fs::write(config_dir.join(CONFIG_FILE), "").unwrap();

        let loaded = load_repo_config(dir.path()).unwrap().unwrap();
        assert!(loaded.ralphs.is_empty());
    }

    #[test]
    fn test_ralph_config_default_paths() {
        let toml_str = r#"
[[ralphs]]
name = "docs"
"#;
        let cfg: RepoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.ralphs.len(), 1);
        assert_eq!(cfg.ralphs[0].task_file, "tasks.json");
        assert_eq!(cfg.ralphs[0].state_file, ".wreck-it-state.json");
    }

    #[test]
    fn test_find_ralph_returns_match() {
        let cfg = RepoConfig {
            ralphs: vec![
                RalphConfig {
                    name: "docs".to_string(),
                    task_file: "docs-tasks.json".to_string(),
                    state_file: ".docs-state.json".to_string(),
                },
                RalphConfig {
                    name: "coverage".to_string(),
                    task_file: "coverage-tasks.json".to_string(),
                    state_file: ".coverage-state.json".to_string(),
                },
            ],
            ..RepoConfig::default()
        };
        let found = find_ralph(&cfg, "coverage").unwrap();
        assert_eq!(found.task_file, "coverage-tasks.json");
    }

    #[test]
    fn test_find_ralph_returns_none_for_unknown() {
        let cfg = RepoConfig::default();
        assert!(find_ralph(&cfg, "nonexistent").is_none());
    }

    #[test]
    fn test_save_and_load_repo_config_with_ralphs() {
        let dir = tempdir().unwrap();
        let cfg = RepoConfig {
            state_branch: "wreck-it-state".to_string(),
            state_root: ".wreck-it".to_string(),
            ralphs: vec![RalphConfig {
                name: "docs".to_string(),
                task_file: "docs-tasks.json".to_string(),
                state_file: ".docs-state.json".to_string(),
            }],
        };
        save_repo_config(dir.path(), &cfg).unwrap();
        let loaded = load_repo_config(dir.path()).unwrap().unwrap();
        assert_eq!(loaded.ralphs.len(), 1);
        assert_eq!(loaded.ralphs[0].name, "docs");
    }
}
