//! Persistent headless state types.
//!
//! These types are committed to the state branch between invocations and
//! are shared by the CLI headless runner and the Cloudflare Worker.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

use crate::types::TaskStatus;

/// Serde helper: returns `true` when the boolean value is `false`.
fn is_false(v: &bool) -> bool {
    !*v
}

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
    /// Reviews have been requested; waiting for all reviewers to complete.
    AwaitingReview,
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
    /// GitHub issue number that triggered the agent for this PR.
    ///
    /// When present, the runner can check whether the coding agent is still
    /// assigned to the issue (i.e. still actively working) before attempting
    /// to merge or mark the PR as ready for review.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub issue_number: Option<u64>,

    /// Whether reviews have been requested for this PR.
    ///
    /// Set to `true` after the runner calls `request_reviewers` for this PR
    /// so that it does not re-request on subsequent invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_requested: Option<bool>,
}

/// An entry created by the merge ralph to resolve merge conflicts.
///
/// When `comment_only` is `false` (the default / legacy behaviour), the
/// merge ralph created a GitHub issue and assigned a coding agent to it.
/// Subsequent invocations poll for the resulting PR and promote it to a
/// [`TrackedPr`].
///
/// When `comment_only` is `true`, the merge ralph posted a `@copilot`
/// comment directly on the conflicting PR instead of creating a separate
/// issue.  These entries only serve as deduplication guards and are
/// removed once the PR is no longer in a conflicting state (or is closed).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PendingMergeIssue {
    /// GitHub issue number that was created for conflict resolution, **or**
    /// the PR number when `comment_only` is `true`.
    pub issue_number: u64,
    /// The wreck-it task ID (e.g. `"merge-pr-42"`).
    pub task_id: String,
    /// When `true`, the entry represents a `@copilot` comment posted on
    /// the PR (stored in `issue_number`) rather than a coding-agent issue.
    #[serde(default, skip_serializing_if = "is_false")]
    pub comment_only: bool,
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
    /// Populated when a cloud agent creates a PR and persisted between
    /// invocations.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tracked_prs: Vec<TrackedPr>,

    /// Whether reviews have been requested for the current task's PR.
    ///
    /// Set to `true` after the runner calls `request_reviewers` for the
    /// current PR so that it does not re-request on subsequent invocations.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_requested: Option<bool>,

    /// Issues created by the merge ralph that are waiting for the coding
    /// agent to produce a PR.  Once a linked PR is detected, the entry is
    /// promoted to [`tracked_prs`] and removed from this list.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending_merge_issues: Vec<PendingMergeIssue>,

    /// Per-task runtime status, keyed by task ID.
    ///
    /// This map is the authoritative source for task status when present.
    /// Task definition files remain stateless (they carry no `status` field
    /// that mutates at runtime).  If a task ID is absent from this map, it
    /// is treated as [`TaskStatus::Pending`].
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub task_statuses: HashMap<String, TaskStatus>,
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
            review_requested: None,
            pending_merge_issues: Vec::new(),
            task_statuses: HashMap::new(),
        }
    }
}
