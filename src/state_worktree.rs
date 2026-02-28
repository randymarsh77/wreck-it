use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Default orphan branch name for wreck-it state.
pub const DEFAULT_STATE_BRANCH: &str = "wreck-it-state";

/// Default subdirectory under the repo root where the worktree is placed.
const WORKTREE_SUBDIR: &str = ".wreck-it/state";

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

/// Return the worktree path for the state branch inside `repo_root`.
pub fn state_worktree_path(repo_root: &Path) -> PathBuf {
    repo_root.join(WORKTREE_SUBDIR)
}

/// Ensure the state branch exists as a local ref.
///
/// If neither a local branch nor a remote tracking branch exist, an empty
/// orphan commit is created and the local branch ref is pointed at it.
fn ensure_state_branch(repo_root: &Path, branch: &str) -> Result<()> {
    let local_ref = format!("refs/heads/{}", branch);

    // Check if the local branch already exists.
    if git_cmd(repo_root, &["rev-parse", "--verify", &local_ref]).is_ok() {
        return Ok(());
    }

    // Check if a remote tracking branch exists (e.g. origin/<branch>).
    let remote_ref = format!("origin/{}", branch);
    if let Ok(sha) = git_cmd(repo_root, &["rev-parse", "--verify", &remote_ref]) {
        git_cmd(repo_root, &["branch", branch, &sha])?;
        return Ok(());
    }

    // Neither exists – create an orphan branch with an empty commit.
    println!("[wreck-it] creating orphan branch '{}'", branch);

    // Use git plumbing to create the orphan without touching the working tree:
    // 1. Create an empty tree object.
    let empty_tree = git_cmd(repo_root, &["hash-object", "-t", "tree", "/dev/null"])
        .context("Failed to create empty tree")?;

    // 2. Create a root commit (no parent) from that tree.
    let commit = git_cmd(
        repo_root,
        &[
            "commit-tree",
            &empty_tree,
            "-m",
            "wreck-it: initialize state branch",
        ],
    )
    .context("Failed to create orphan commit")?;

    // 3. Point the branch ref at the commit.
    git_cmd(repo_root, &["update-ref", &local_ref, &commit])
        .context("Failed to create state branch ref")?;

    Ok(())
}

/// Ensure the git worktree for the state branch exists and is checked out.
///
/// This creates the orphan branch if necessary, then adds a worktree for it
/// at `<repo_root>/.wreck-it/state`.  If the worktree already exists and
/// points to the correct branch, this is a no-op.
pub fn ensure_state_worktree(repo_root: &Path, branch: &str) -> Result<PathBuf> {
    let wt_path = state_worktree_path(repo_root);

    // Ensure the branch exists (create orphan if needed).
    ensure_state_branch(repo_root, branch)?;

    // If the worktree directory already exists and is valid, we're done.
    if wt_path.join(".git").exists() {
        return Ok(wt_path);
    }

    // Create parent directory.
    if let Some(parent) = wt_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create worktree parent directory")?;
    }

    // Remove stale directory if it exists but isn't a valid worktree.
    if wt_path.exists() {
        std::fs::remove_dir_all(&wt_path).context("Failed to remove stale worktree directory")?;
    }

    // Add the worktree.
    println!(
        "[wreck-it] adding worktree at {} for branch '{}'",
        wt_path.display(),
        branch
    );
    git_cmd(
        repo_root,
        &["worktree", "add", &wt_path.to_string_lossy(), branch],
    )
    .context("Failed to add git worktree")?;

    Ok(wt_path)
}

/// Remove the state worktree (cleanup).
#[allow(dead_code)]
pub fn remove_state_worktree(repo_root: &Path) -> Result<()> {
    let wt_path = state_worktree_path(repo_root);
    if wt_path.exists() {
        // Remove from git's worktree list first.
        let _ = git_cmd(
            repo_root,
            &["worktree", "remove", &wt_path.to_string_lossy(), "--force"],
        );
        // Ensure the directory is gone even if git-worktree-remove failed.
        if wt_path.exists() {
            std::fs::remove_dir_all(&wt_path)?;
        }
    }
    // Prune stale worktree metadata.
    let _ = git_cmd(repo_root, &["worktree", "prune"]);
    Ok(())
}

/// Commit all changes inside the state worktree and optionally push.
///
/// This stages everything in the worktree, creates a commit with the given
/// message, and returns `true` if a commit was made.
pub fn commit_state_worktree(repo_root: &Path, message: &str) -> Result<bool> {
    let wt_path = state_worktree_path(repo_root);
    if !wt_path.join(".git").exists() {
        return Ok(false);
    }

    // Check if there are changes.
    let status = Command::new("git")
        .args(["diff", "--quiet"])
        .current_dir(&wt_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to check worktree status")?;

    let cached_status = Command::new("git")
        .args(["diff", "--cached", "--quiet"])
        .current_dir(&wt_path)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .context("Failed to check worktree cached status")?;

    if status.success() && cached_status.success() {
        // Also check for untracked files.
        let untracked = git_cmd(&wt_path, &["ls-files", "--others", "--exclude-standard"])?;
        if untracked.is_empty() {
            return Ok(false);
        }
    }

    git_cmd(&wt_path, &["add", "-A"])?;
    git_cmd(&wt_path, &["commit", "-m", message])?;

    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    /// Create a minimal git repo in a temp dir for testing.
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
        // Need at least one commit on the default branch for worktree to work.
        std::fs::write(dir.path().join("README.md"), "# test\n").unwrap();
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir.path())
            .output()
            .unwrap();
        dir
    }

    #[test]
    fn test_ensure_state_branch_creates_orphan() {
        let dir = init_test_repo();
        ensure_state_branch(dir.path(), "wreck-it-state").unwrap();

        // The branch should exist now.
        let result = git_cmd(
            dir.path(),
            &["rev-parse", "--verify", "refs/heads/wreck-it-state"],
        );
        assert!(result.is_ok());
    }

    #[test]
    fn test_ensure_state_branch_idempotent() {
        let dir = init_test_repo();
        ensure_state_branch(dir.path(), "wreck-it-state").unwrap();
        // Second call should succeed silently.
        ensure_state_branch(dir.path(), "wreck-it-state").unwrap();
    }

    #[test]
    fn test_ensure_state_worktree_creates_and_returns_path() {
        let dir = init_test_repo();
        let wt = ensure_state_worktree(dir.path(), "wreck-it-state").unwrap();

        assert_eq!(wt, state_worktree_path(dir.path()));
        assert!(wt.join(".git").exists());
    }

    #[test]
    fn test_ensure_state_worktree_idempotent() {
        let dir = init_test_repo();
        let wt1 = ensure_state_worktree(dir.path(), "wreck-it-state").unwrap();
        let wt2 = ensure_state_worktree(dir.path(), "wreck-it-state").unwrap();
        assert_eq!(wt1, wt2);
    }

    #[test]
    fn test_remove_state_worktree() {
        let dir = init_test_repo();
        ensure_state_worktree(dir.path(), "wreck-it-state").unwrap();
        let wt = state_worktree_path(dir.path());
        assert!(wt.exists());

        remove_state_worktree(dir.path()).unwrap();
        assert!(!wt.exists());
    }

    #[test]
    fn test_commit_state_worktree_no_changes() {
        let dir = init_test_repo();
        ensure_state_worktree(dir.path(), "wreck-it-state").unwrap();
        let committed = commit_state_worktree(dir.path(), "test commit").unwrap();
        assert!(!committed);
    }

    #[test]
    fn test_commit_state_worktree_with_new_file() {
        let dir = init_test_repo();
        let wt = ensure_state_worktree(dir.path(), "wreck-it-state").unwrap();

        // Create a file in the worktree.
        std::fs::write(wt.join("state.json"), "{}").unwrap();

        let committed = commit_state_worktree(dir.path(), "add state").unwrap();
        assert!(committed);

        // Verify the commit exists on the branch.
        let log = git_cmd(dir.path(), &["log", "--oneline", "wreck-it-state"]).unwrap();
        assert!(log.contains("add state"));
    }

    #[test]
    fn test_state_worktree_path_default() {
        let root = Path::new("/some/repo");
        assert_eq!(
            state_worktree_path(root),
            PathBuf::from("/some/repo/.wreck-it/state")
        );
    }
}
