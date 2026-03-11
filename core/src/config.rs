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

/// Subdirectory under [`CONFIG_DIR`] on the main branch where agents can drop
/// new or revised task plans.  The headless runner migrates these into the
/// state branch at the start of each iteration.
pub const PLANS_DIR: &str = "plans";

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

    /// Git branch used to read task definition files.
    ///
    /// Task definitions are treated as stateless documents: they describe
    /// *what* needs to be done but do not carry runtime status.  Runtime
    /// status for each task is tracked by ID inside the state files on the
    /// [`state_branch`].
    ///
    /// When omitted, task files are read from the state branch for backward
    /// compatibility.  Set this to the repository's default branch (e.g.
    /// `"master"` or `"main"`) to keep task definitions alongside the code
    /// where agents can work with them directly.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_branch: Option<String>,

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

    /// Optional feature branch for the target repository.
    ///
    /// When set, the headless runner ensures this branch exists before
    /// triggering a cloud agent and instructs the agent to base its work
    /// on this branch.  PRs created by the agent will target this branch
    /// instead of the repository default.  If the branch does not exist it
    /// is created from the repository's default branch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,

    /// Optional agent login to assign to issues.
    ///
    /// When set, this specific agent login is preferred when assigning a
    /// coding agent to a triggered issue.  The agent must still appear in the
    /// repository's `suggestedActors` list.  If omitted, the first known
    /// agent from the default list is used.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent: Option<String>,

    /// Optional list of reviewer logins (agents or GitHub users) to request
    /// reviews from when a PR is created by the coding agent.
    ///
    /// When set, the headless runner requests reviews from these users after
    /// the PR is marked ready for review, waits for all reviews to complete,
    /// and at-mentions the PR author to address any requested changes.  If
    /// omitted, the PR proceeds directly to merge without review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reviewers: Option<Vec<String>>,

    /// Optional command override for this ralph context.
    ///
    /// When set to `"unstuck"`, the headless runner skips the normal task
    /// state machine and instead scans all open PRs for failing CI checks,
    /// commenting `@copilot` to request fixes.  If omitted, the default
    /// headless loop runs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub command: Option<String>,

    /// When `true`, enables "brute mode" for this ralph context.
    ///
    /// In brute mode the headless runner disables auto-merge (if it was
    /// previously enabled) and merges pull requests directly instead of
    /// waiting for GitHub's auto-merge to kick in once checks pass.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brute_mode: Option<bool>,

    /// Backend to use for this ralph context.
    ///
    /// Supported values:
    /// - `"cloud_agent"` – creates a GitHub issue describing the work and
    ///   assigns a coding agent to resolve it remotely.
    /// - `"cli"` – performs the work locally (e.g. git merge) and pushes the
    ///   result directly.
    ///
    /// When omitted, the backend defaults to `"cloud_agent"` for commands
    /// that support it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub backend: Option<String>,

    /// Optional path to a directory containing per-role system prompt
    /// template files (e.g. `ideas.md`, `implementer.md`, `evaluator.md`) and
    /// per-task overrides (e.g. `impl-my-task.md`).
    ///
    /// When set, the `prompt_loader` module reads templates from this
    /// directory and injects them as the system prompt for the matching agent
    /// invocation, falling back to the built-in defaults when no matching file
    /// is found.  Relative paths are resolved from the repository root.
    ///
    /// Example: `.wreck-it/prompts`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt_dir: Option<String>,

    /// Optional shell command to validate PR changes before merging.
    ///
    /// When set, the headless runner executes this command in the repository
    /// work directory during the `NeedsVerification` phase, before attempting
    /// to merge a pull request.  The command is run via the system shell
    /// (`sh -c` on Unix, `cmd /C` on Windows) so pipes, redirects, and other
    /// shell features are available.
    ///
    /// If the command exits with a non-zero status, the PR is **not** merged.
    /// Instead, a comment is posted on the PR at-mentioning `@copilot` with
    /// the command, its exit code, and the captured stdout / stderr output so
    /// the coding agent can address the failure.
    ///
    /// Example: `"cargo test --lib"`
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub validation_command: Option<String>,
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
            task_branch: None,
            state_root: default_state_root(),
            ralphs: Vec::new(),
        }
    }
}

impl RepoConfig {
    /// Return the effective branch from which task files are read.
    ///
    /// When `task_branch` is set, returns that value.  Otherwise falls back to
    /// the `state_branch` for backward compatibility.
    pub fn effective_task_branch(&self) -> &str {
        self.task_branch.as_deref().unwrap_or(&self.state_branch)
    }
}

/// Look up a named ralph context from the repo config.
///
/// Returns `None` if the config has no `[[ralphs]]` entry with the given name.
pub fn find_ralph<'a>(config: &'a RepoConfig, name: &str) -> Option<&'a RalphConfig> {
    config.ralphs.iter().find(|r| r.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_no_task_branch() {
        let cfg = RepoConfig::default();
        assert!(cfg.task_branch.is_none());
    }

    #[test]
    fn effective_task_branch_falls_back_to_state_branch() {
        let cfg = RepoConfig::default();
        assert_eq!(cfg.effective_task_branch(), DEFAULT_STATE_BRANCH);
    }

    #[test]
    fn effective_task_branch_uses_explicit_value() {
        let cfg = RepoConfig {
            task_branch: Some("main".to_string()),
            ..RepoConfig::default()
        };
        assert_eq!(cfg.effective_task_branch(), "main");
    }

    #[test]
    fn task_branch_roundtrips_via_toml() {
        let toml_str = r#"
task_branch = "master"
"#;
        let cfg: RepoConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.task_branch.as_deref(), Some("master"));
        assert_eq!(cfg.effective_task_branch(), "master");
    }

    #[test]
    fn task_branch_omitted_from_toml_when_none() {
        let cfg = RepoConfig::default();
        let toml_str = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !toml_str.contains("task_branch"),
            "task_branch should be absent: {toml_str}"
        );
    }

    #[test]
    fn task_branch_absent_in_toml_defaults_to_none() {
        let toml_str = r#"
state_branch = "my-state"
"#;
        let cfg: RepoConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.task_branch.is_none());
        assert_eq!(cfg.effective_task_branch(), "my-state");
    }

    #[test]
    fn validation_command_roundtrips_via_toml() {
        let toml_str = r#"
name = "ci-check"
validation_command = "cargo test --lib"
"#;
        let cfg: RalphConfig = toml::from_str(toml_str).unwrap();
        assert_eq!(cfg.validation_command.as_deref(), Some("cargo test --lib"),);
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            serialized.contains("validation_command"),
            "validation_command should be present: {serialized}"
        );
    }

    #[test]
    fn validation_command_omitted_from_toml_when_none() {
        let toml_str = r#"
name = "docs"
"#;
        let cfg: RalphConfig = toml::from_str(toml_str).unwrap();
        assert!(cfg.validation_command.is_none());
        let serialized = toml::to_string_pretty(&cfg).unwrap();
        assert!(
            !serialized.contains("validation_command"),
            "validation_command should be absent: {serialized}"
        );
    }
}
