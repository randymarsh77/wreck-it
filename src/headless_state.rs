use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};

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
        }
    }
}

/// Load headless state from a JSON file. Returns default state if the file
/// does not exist.
#[cfg(test)]
pub fn load_headless_state(path: &Path) -> Result<HeadlessState> {
    use std::fs;
    if !path.exists() {
        return Ok(HeadlessState::default());
    }
    let content = fs::read_to_string(path).context("Failed to read headless state file")?;
    let state = serde_json::from_str(&content).context("Failed to parse headless state file")?;
    Ok(state)
}

/// Save headless state to a JSON file.
#[cfg(test)]
pub fn save_headless_state(path: &Path, state: &HeadlessState) -> Result<()> {
    use std::fs;
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create state directory")?;
    }
    let content =
        serde_json::to_string_pretty(state).context("Failed to serialize headless state")?;
    fs::write(path, content).context("Failed to write headless state file")?;
    Ok(())
}

/// Run a git command in `work_dir` and return trimmed stdout on success.
fn git_cmd(work_dir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to run git command")?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr);
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Run a git command with data piped to stdin and return trimmed stdout.
fn git_cmd_stdin(work_dir: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    let mut child = Command::new("git")
        .args(args)
        .current_dir(work_dir)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("Failed to spawn git")?;

    if let Some(mut stdin) = child.stdin.take() {
        stdin
            .write_all(input)
            .context("Failed to write to git stdin")?;
    }

    let output = child.wait_with_output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr);
    }

    Ok(String::from_utf8(output.stdout)?.trim().to_string())
}

/// Load headless state from an orphan branch using git plumbing.
///
/// The state is read via `git show <branch>:<state_file>` so the branch
/// never needs to be checked out.  Returns default state when the branch
/// or file does not exist.
pub fn load_headless_state_from_branch(
    work_dir: &Path,
    branch: &str,
    state_file: &Path,
) -> Result<HeadlessState> {
    let ref_spec = format!("{}:{}", branch, state_file.display());
    let output = Command::new("git")
        .args(["show", &ref_spec])
        .current_dir(work_dir)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .context("Failed to run git show for state branch")?;

    if !output.status.success() {
        // Branch or file doesn't exist yet – return default state.
        return Ok(HeadlessState::default());
    }

    let content = String::from_utf8(output.stdout).context("git show output is not valid UTF-8")?;
    let state =
        serde_json::from_str(&content).context("Failed to parse headless state from branch")?;
    Ok(state)
}

/// Save headless state to an orphan branch using git plumbing commands.
///
/// Uses `hash-object`, `mktree`, `commit-tree` and `update-ref` so that
/// the working tree and index are never touched – only the branch ref is
/// updated.  Creates the branch as an orphan if it doesn't already exist.
pub fn save_headless_state_to_branch(
    work_dir: &Path,
    branch: &str,
    state_file: &Path,
    state: &HeadlessState,
) -> Result<()> {
    let content =
        serde_json::to_string_pretty(state).context("Failed to serialize headless state")?;
    let content = format!("{}\n", content);

    let file_path = state_file.display().to_string();

    // 1. Write content into a blob.
    let blob_sha = git_cmd_stdin(
        work_dir,
        &["hash-object", "-w", "--stdin"],
        content.as_bytes(),
    )
    .context("Failed to create state blob")?;

    // 2. Build a tree containing the state file.
    let tree_entry = format!("100644 blob {}\t{}\n", blob_sha, file_path);
    let tree_sha = git_cmd_stdin(work_dir, &["mktree"], tree_entry.as_bytes())
        .context("Failed to create state tree")?;

    // 3. Determine parent commit (if the branch already exists).
    let branch_ref = format!("refs/heads/{}", branch);
    let parent = git_cmd(work_dir, &["rev-parse", "--verify", &branch_ref]).ok();

    // 4. Create a commit.
    let commit_sha = if let Some(parent_sha) = &parent {
        git_cmd(
            work_dir,
            &[
                "commit-tree",
                &tree_sha,
                "-p",
                parent_sha,
                "-m",
                "wreck-it: update headless state",
            ],
        )
        .context("Failed to create state commit")?
    } else {
        git_cmd(
            work_dir,
            &[
                "commit-tree",
                &tree_sha,
                "-m",
                "wreck-it: initialize headless state",
            ],
        )
        .context("Failed to create initial state commit")?
    };

    // 5. Point the branch ref at the new commit.
    git_cmd(work_dir, &["update-ref", &branch_ref, &commit_sha])
        .context("Failed to update state branch ref")?;

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
    }

    /// Helper: create a bare-minimum git repo in a temp directory so that
    /// the plumbing-based branch functions work.
    fn init_test_repo() -> tempfile::TempDir {
        let dir = tempdir().unwrap();
        Command::new("git")
            .args(["init"])
            .current_dir(dir.path())
            .output()
            .expect("git init failed");
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn test_load_from_branch_returns_default_when_branch_missing() {
        let dir = init_test_repo();
        let state_file = Path::new(".wreck-it-state.json");
        let state =
            load_headless_state_from_branch(dir.path(), "wreck-it-state", state_file).unwrap();
        assert_eq!(state.phase, AgentPhase::NeedsTrigger);
        assert_eq!(state.iteration, 0);
    }

    #[test]
    fn test_save_and_load_from_branch_roundtrip() {
        let dir = init_test_repo();
        let state_file = Path::new(".wreck-it-state.json");
        let branch = "wreck-it-state";

        let state = HeadlessState {
            phase: AgentPhase::AgentWorking,
            iteration: 5,
            current_task_id: Some("task-2".to_string()),
            issue_number: Some(100),
            pr_number: Some(50),
            pr_url: Some("https://github.com/o/r/pull/50".to_string()),
            last_prompt: Some("add feature Y".to_string()),
            memory: vec!["note".to_string()],
        };

        save_headless_state_to_branch(dir.path(), branch, state_file, &state).unwrap();
        let loaded = load_headless_state_from_branch(dir.path(), branch, state_file).unwrap();

        assert_eq!(loaded.phase, AgentPhase::AgentWorking);
        assert_eq!(loaded.iteration, 5);
        assert_eq!(loaded.current_task_id.as_deref(), Some("task-2"));
        assert_eq!(loaded.issue_number, Some(100));
        assert_eq!(loaded.pr_number, Some(50));
        assert_eq!(
            loaded.pr_url.as_deref(),
            Some("https://github.com/o/r/pull/50")
        );
        assert_eq!(loaded.last_prompt.as_deref(), Some("add feature Y"));
        assert_eq!(loaded.memory, vec!["note".to_string()]);
    }

    #[test]
    fn test_save_to_branch_creates_orphan() {
        let dir = init_test_repo();
        let state_file = Path::new("state.json");
        let branch = "my-state";

        let state = HeadlessState::default();
        save_headless_state_to_branch(dir.path(), branch, state_file, &state).unwrap();

        // Verify the branch exists.
        let output = Command::new("git")
            .args(["rev-parse", "--verify", &format!("refs/heads/{}", branch)])
            .current_dir(dir.path())
            .output()
            .unwrap();
        assert!(output.status.success(), "state branch should exist");
    }

    #[test]
    fn test_save_to_branch_updates_existing() {
        let dir = init_test_repo();
        let state_file = Path::new(".wreck-it-state.json");
        let branch = "wreck-it-state";

        // First save.
        let mut state = HeadlessState::default();
        save_headless_state_to_branch(dir.path(), branch, state_file, &state).unwrap();

        // Second save with updated iteration.
        state.iteration = 10;
        state.phase = AgentPhase::NeedsVerification;
        save_headless_state_to_branch(dir.path(), branch, state_file, &state).unwrap();

        let loaded = load_headless_state_from_branch(dir.path(), branch, state_file).unwrap();
        assert_eq!(loaded.iteration, 10);
        assert_eq!(loaded.phase, AgentPhase::NeedsVerification);

        // Verify the branch has exactly 2 commits (init + update).
        let log = Command::new("git")
            .args(["log", "--oneline", branch])
            .current_dir(dir.path())
            .output()
            .unwrap();
        let log_output = String::from_utf8(log.stdout).unwrap();
        let lines: Vec<&str> = log_output.trim().lines().collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
    }
}
