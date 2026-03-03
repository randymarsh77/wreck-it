//! Repository-level configuration types.
//!
//! The `RepoConfig` lives on the main branch at `.wreck-it/config.toml` and
//! tells wreck-it where to find its state data.  These types are shared
//! across the CLI and worker.

use serde::{Deserialize, Serialize};

/// Default orphan branch name for wreck-it state.
pub const DEFAULT_STATE_BRANCH: &str = "wreck-it-state";

/// Directory on the main branch that holds the wreck-it repo config.
pub const CONFIG_DIR: &str = ".wreck-it";

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
    #[serde(default = "default_task_file")]
    pub task_file: String,

    /// Path to the persistent state file, relative to the state root.
    #[serde(default = "default_state_file")]
    pub state_file: String,
}

fn default_state_branch() -> String {
    DEFAULT_STATE_BRANCH.to_string()
}

fn default_state_root() -> String {
    CONFIG_DIR.to_string()
}

fn default_task_file() -> String {
    "tasks.json".to_string()
}

fn default_state_file() -> String {
    ".wreck-it-state.json".to_string()
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

/// Look up a named ralph context from the repo config.
///
/// Returns `None` if the config has no `[[ralphs]]` entry with the given name.
pub fn find_ralph<'a>(config: &'a RepoConfig, name: &str) -> Option<&'a RalphConfig> {
    config.ralphs.iter().find(|r| r.name == name)
}
