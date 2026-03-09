//! Core domain types shared across wreck-it crates (CLI, worker, etc.).
//!
//! These types are the canonical definitions for tasks, agent state, and
//! repository configuration.  They are designed to be WASM-compatible
//! (no `std::fs`, no native-only dependencies).
//!
//! When the `clap` feature is enabled, selected enums derive
//! [`clap::ValueEnum`] so the CLI can use them directly as CLI arguments.

use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Artefact types
// ---------------------------------------------------------------------------

/// The kind of an artefact produced or consumed by a task.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ArtefactKind {
    /// A raw file on disk.
    File,
    /// A structured JSON document.
    Json,
    /// A human-readable summary or notes.
    Summary,
}

/// An artefact declared as an input or output of a task.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TaskArtefact {
    pub kind: ArtefactKind,
    /// Logical name used as the manifest key (combined with the task id).
    pub name: String,
    /// Path relative to the work directory where the artefact file resides.
    pub path: String,
}

// ---------------------------------------------------------------------------
// Task enums
// ---------------------------------------------------------------------------

/// Execution runtime for a task.
///
/// When absent the task is executed locally by the wreck-it agent harness
/// (the default).  Set to `gastown` to offload execution to the gastown cloud
/// agent service.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskRuntime {
    /// Execute locally (default behaviour).
    #[default]
    Local,
    /// Offload execution to the gastown cloud agent service.
    Gastown,
}

/// The lifecycle kind of a task.
///
/// When absent in a JSON task file, the field defaults to [`TaskKind::Milestone`]
/// so that pre-existing task files continue to work without modification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(rename_all = "lowercase")]
pub enum TaskKind {
    /// A one-shot goal that completes permanently.
    #[default]
    Milestone,
    /// A long-running goal that resets to pending after completion, subject to
    /// an optional cooldown period.
    Recurring,
}

/// The role of the agent assigned to execute this task.
///
/// When absent in a JSON task file the field defaults to [`AgentRole::Implementer`]
/// so that pre-existing task files continue to work without modification.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default)]
#[cfg_attr(feature = "clap", derive(clap::ValueEnum))]
#[serde(rename_all = "lowercase")]
pub enum AgentRole {
    /// Research and generate new tasks.
    Ideas,
    /// Write code / implement the work (default).
    #[default]
    Implementer,
    /// Review and validate completed work.
    Evaluator,
}

/// Status of an individual task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
}

// ---------------------------------------------------------------------------
// Task struct
// ---------------------------------------------------------------------------

/// A task to be completed by the agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,

    /// Agent role responsible for this task.  Defaults to `implementer` for
    /// backward compatibility with task files that pre-date this field.
    #[serde(default, skip_serializing_if = "is_default_role")]
    pub role: AgentRole,

    /// Lifecycle kind of the task.  Defaults to `milestone` (one-shot).
    /// Set to `recurring` for long-running goals that reset to pending after
    /// completion, subject to an optional cooldown.
    #[serde(default, skip_serializing_if = "is_default_kind")]
    pub kind: TaskKind,

    /// Minimum number of seconds that must elapse after a recurring task
    /// completes before it is eligible to run again.  Only meaningful when
    /// `kind` is `recurring`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_seconds: Option<u64>,

    /// Execution phase (tasks in the same phase may run in parallel).
    /// Tasks in a lower phase run before tasks in a higher phase.
    /// When omitted, defaults to `1` (all tasks share one sequential phase).
    #[serde(default = "default_phase", skip_serializing_if = "is_default_phase")]
    pub phase: u32,

    /// IDs of tasks that must complete before this task can start.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on: Vec<String>,

    /// Scheduling priority (higher = run sooner).  Defaults to 0.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub priority: u32,

    /// Estimated complexity on a 1–10 scale (lower = quicker win).  Defaults to 1.
    #[serde(
        default = "default_complexity",
        skip_serializing_if = "is_default_complexity"
    )]
    pub complexity: u32,

    /// Maximum time in seconds the agent may spend on a single execution
    /// attempt.  When the timeout elapses the attempt is treated as a failure
    /// and the task may be retried subject to `max_retries`.  When absent,
    /// execution is unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub timeout_seconds: Option<u64>,

    /// Maximum number of automatic retry attempts after a failure.  The
    /// existing `failed_attempts` counter tracks how many attempts have been
    /// made, so no additional state is required.  A value of `0` or absence
    /// means the task is not retried; the first failure is permanent until
    /// the operator resets the status manually.
    ///
    /// With `max_retries = N` the task may run at most `N + 1` times in
    /// total (one initial attempt plus up to N retries).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_retries: Option<u32>,

    /// Number of previous failed execution attempts.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub failed_attempts: u32,

    /// Unix timestamp (seconds) of the most recent execution attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_at: Option<u64>,

    /// Input artefact references in the form `"task-id/artefact-name"`.
    /// The referenced artefacts are resolved from the manifest and injected
    /// into the agent's prompt context before execution.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<String>,

    /// Output artefacts that should be persisted to the manifest when this
    /// task completes successfully.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub outputs: Vec<TaskArtefact>,

    /// Execution runtime for this task.  Defaults to `local`.  Set to
    /// `gastown` to offload execution to the gastown cloud agent service.
    #[serde(default, skip_serializing_if = "is_default_runtime")]
    pub runtime: TaskRuntime,

    /// Optional agent-evaluated precondition prompt.  When present, an
    /// evaluation agent checks this condition before the task is eligible
    /// to execute.  If the agent determines the precondition is not met the
    /// task is skipped for that iteration.  This is especially useful for
    /// recurring tasks that need nuanced re-run criteria beyond a simple
    /// cooldown timer.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub precondition_prompt: Option<String>,

    /// ID of the parent task (epic).  When set, this task is a sub-task of
    /// the referenced parent.  A task with no `parent_id` that has children
    /// pointing to it is considered an **epic**.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_id: Option<String>,

    /// Free-form labels for categorization (e.g. board columns, tags).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub labels: Vec<String>,
}

fn is_default_role(r: &AgentRole) -> bool {
    *r == AgentRole::Implementer
}

fn is_default_kind(k: &TaskKind) -> bool {
    *k == TaskKind::Milestone
}

pub(crate) fn default_phase() -> u32 {
    1
}

fn is_default_phase(v: &u32) -> bool {
    *v == 1
}

pub(crate) fn default_complexity() -> u32 {
    1
}

fn is_default_complexity(v: &u32) -> bool {
    *v == 1
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_default_runtime(r: &TaskRuntime) -> bool {
    *r == TaskRuntime::Local
}

// ---------------------------------------------------------------------------
// Provenance
// ---------------------------------------------------------------------------

/// A single provenance record capturing the context of one agent invocation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvenanceRecord {
    /// Identifier of the task that was executed.
    pub task_id: String,
    /// Role of the agent that executed the task.
    pub agent_role: AgentRole,
    /// Human-readable name of the model/provider used.
    pub model: String,
    /// Hex hash of the prompt (task description) sent to the agent.
    pub prompt_hash: String,
    /// Tool calls made during the invocation (e.g. reflection, evaluation).
    pub tool_calls: Vec<String>,
    /// Hex hash of the git diff produced after the invocation.
    pub git_diff_hash: String,
    /// Unix timestamp (seconds) when the invocation started.
    pub timestamp: u64,
    /// Outcome of the invocation: `"success"` or `"failure"`.
    pub outcome: String,
}

// ---------------------------------------------------------------------------
// Trusted author validation (supply-chain protection)
// ---------------------------------------------------------------------------

/// Known coding-agent logins that are authorized to open pull requests and
/// work on issues.
///
/// Both the CLI and worker use this list to validate that events originate
/// from a known agent rather than an unauthorized third party.  The bare
/// login (e.g. `"copilot"`) and the `[bot]` suffixed variant
/// (e.g. `"copilot[bot]"`) are both accepted by the helper functions.
pub const KNOWN_AGENT_LOGINS: &[&str] = &["copilot-swe-agent", "copilot", "claude", "codex"];

/// Check whether a login belongs to a known coding agent.
///
/// Accepts both bare logins (`"copilot"`) and the `[bot]` suffixed form
/// (`"copilot[bot]"`).
pub fn is_known_agent_login(login: &str) -> bool {
    let bare = login.strip_suffix("[bot]").unwrap_or(login);
    KNOWN_AGENT_LOGINS
        .iter()
        .any(|known| known.eq_ignore_ascii_case(bare))
}

/// Check whether an issue was opened by a trusted author.
///
/// Trusted if any of the following hold:
///
/// - **App flow**: the author's account type is `"Bot"` (GitHub App created
///   the issue on behalf of itself).
/// - **PAT flow**: `user_login` matches `authenticated_login` (the issue
///   was created by the same user whose token we are using).
///
/// `user_type` is the GitHub account `type` field (e.g. `"User"`, `"Bot"`,
/// `"Organization"`).  `user_login` is the author's login.
/// `authenticated_login` is the login of the user whose token is being used
/// (may be `None` when unavailable, e.g. before token resolution).
pub fn is_trusted_issue_author(
    user_type: Option<&str>,
    user_login: Option<&str>,
    authenticated_login: Option<&str>,
) -> bool {
    // App flow: Bot type is always trusted.
    if user_type.is_some_and(|t| t == "Bot") {
        return true;
    }
    // PAT flow: trust if the author matches our authenticated user.
    matches!((user_login, authenticated_login), (Some(l), Some(al)) if l == al)
}

/// Check whether a pull request was opened by a known coding agent or the
/// authenticated user.
///
/// Trusted if any of the following hold:
///
/// - `login` belongs to a known coding agent (bare or `[bot]`-suffixed).
/// - `login` matches `authenticated_login` (the PR was created by the same
///   user whose token we are using).
///
/// `authenticated_login` may be `None` when unavailable; in that case only
/// the known-agent check applies.
pub fn is_trusted_pr_author(login: Option<&str>, authenticated_login: Option<&str>) -> bool {
    if login.is_some_and(is_known_agent_login) {
        return true;
    }
    matches!((login, authenticated_login), (Some(l), Some(al)) if l == al)
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- KNOWN_AGENT_LOGINS ----

    #[test]
    fn known_agent_logins_contains_expected_entries() {
        assert!(KNOWN_AGENT_LOGINS.contains(&"copilot-swe-agent"));
        assert!(KNOWN_AGENT_LOGINS.contains(&"copilot"));
        assert!(KNOWN_AGENT_LOGINS.contains(&"claude"));
        assert!(KNOWN_AGENT_LOGINS.contains(&"codex"));
    }

    // ---- is_known_agent_login ----

    #[test]
    fn known_agent_bare_login() {
        assert!(is_known_agent_login("copilot"));
        assert!(is_known_agent_login("copilot-swe-agent"));
        assert!(is_known_agent_login("claude"));
        assert!(is_known_agent_login("codex"));
    }

    #[test]
    fn known_agent_bot_suffix() {
        assert!(is_known_agent_login("copilot[bot]"));
        assert!(is_known_agent_login("copilot-swe-agent[bot]"));
        assert!(is_known_agent_login("claude[bot]"));
        assert!(is_known_agent_login("codex[bot]"));
    }

    #[test]
    fn unknown_agent_login() {
        assert!(!is_known_agent_login("attacker"));
        assert!(!is_known_agent_login("evil-bot[bot]"));
        assert!(!is_known_agent_login(""));
    }

    #[test]
    fn known_agent_case_insensitive() {
        // GitHub may return display-name casing (e.g. "Copilot") instead of
        // the lowercase login.
        assert!(is_known_agent_login("Copilot"));
        assert!(is_known_agent_login("COPILOT"));
        assert!(is_known_agent_login("Copilot-Swe-Agent"));
        assert!(is_known_agent_login("Claude"));
        assert!(is_known_agent_login("Codex"));
        assert!(is_known_agent_login("Copilot[bot]"));
    }

    // ---- is_trusted_issue_author ----

    #[test]
    fn trusted_issue_author_bot() {
        assert!(is_trusted_issue_author(Some("Bot"), None, None));
    }

    #[test]
    fn trusted_issue_author_bot_ignores_login() {
        // Bot type is trusted regardless of login/authenticated_login.
        assert!(is_trusted_issue_author(
            Some("Bot"),
            Some("any"),
            Some("other")
        ));
    }

    #[test]
    fn trusted_issue_author_matching_login() {
        // PAT flow: author login matches authenticated_login.
        assert!(is_trusted_issue_author(
            Some("User"),
            Some("my-user"),
            Some("my-user")
        ));
    }

    #[test]
    fn untrusted_issue_author_user() {
        assert!(!is_trusted_issue_author(Some("User"), None, None));
    }

    #[test]
    fn untrusted_issue_author_user_no_authenticated_login() {
        assert!(!is_trusted_issue_author(
            Some("User"),
            Some("attacker"),
            None
        ));
    }

    #[test]
    fn untrusted_issue_author_login_mismatch() {
        assert!(!is_trusted_issue_author(
            Some("User"),
            Some("attacker"),
            Some("my-user")
        ));
    }

    #[test]
    fn untrusted_issue_author_none() {
        assert!(!is_trusted_issue_author(None, None, None));
    }

    #[test]
    fn untrusted_issue_author_organization() {
        assert!(!is_trusted_issue_author(Some("Organization"), None, None));
    }

    // ---- is_trusted_pr_author ----

    #[test]
    fn trusted_pr_author_copilot() {
        assert!(is_trusted_pr_author(Some("copilot"), None));
    }

    #[test]
    fn trusted_pr_author_copilot_swe_agent_bot() {
        assert!(is_trusted_pr_author(Some("copilot-swe-agent[bot]"), None));
    }

    #[test]
    fn trusted_pr_author_claude() {
        assert!(is_trusted_pr_author(Some("claude"), None));
    }

    #[test]
    fn trusted_pr_author_codex_bot() {
        assert!(is_trusted_pr_author(Some("codex[bot]"), None));
    }

    #[test]
    fn trusted_pr_author_matching_login() {
        // PAT flow: author login matches authenticated_login.
        assert!(is_trusted_pr_author(Some("my-user"), Some("my-user")));
    }

    #[test]
    fn untrusted_pr_author_random_user() {
        assert!(!is_trusted_pr_author(Some("attacker"), None));
    }

    #[test]
    fn untrusted_pr_author_unknown_bot() {
        assert!(!is_trusted_pr_author(Some("evil-bot[bot]"), None));
    }

    #[test]
    fn untrusted_pr_author_none() {
        assert!(!is_trusted_pr_author(None, None));
    }

    #[test]
    fn untrusted_pr_author_login_mismatch() {
        assert!(!is_trusted_pr_author(Some("attacker"), Some("my-user")));
    }

    #[test]
    fn trusted_pr_author_case_insensitive() {
        // GitHub may return "Copilot" (display-name casing) as the login.
        assert!(is_trusted_pr_author(Some("Copilot"), None));
        assert!(is_trusted_pr_author(Some("COPILOT-SWE-AGENT"), None));
        assert!(is_trusted_pr_author(Some("Claude[bot]"), None));
    }

    // ---- Task: timeout_seconds / max_retries ----

    fn make_minimal_task(id: &str) -> Task {
        Task {
            id: id.to_string(),
            description: "test task".to_string(),
            status: TaskStatus::Pending,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: default_phase(),
            depends_on: vec![],
            priority: 0,
            complexity: default_complexity(),
            timeout_seconds: None,
            max_retries: None,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }
    }

    #[test]
    fn task_new_fields_default_to_none() {
        let t = make_minimal_task("t1");
        assert!(t.timeout_seconds.is_none());
        assert!(t.max_retries.is_none());
    }

    #[test]
    fn task_new_fields_serialise_when_set() {
        let mut t = make_minimal_task("t1");
        t.timeout_seconds = Some(60);
        t.max_retries = Some(2);
        let json = serde_json::to_string(&t).unwrap();
        assert!(json.contains("\"timeout_seconds\":60"));
        assert!(json.contains("\"max_retries\":2"));
    }

    #[test]
    fn task_new_fields_omitted_from_json_when_none() {
        let t = make_minimal_task("t1");
        let json = serde_json::to_string(&t).unwrap();
        assert!(
            !json.contains("timeout_seconds"),
            "key should be absent: {json}"
        );
        assert!(
            !json.contains("max_retries"),
            "key should be absent: {json}"
        );
    }

    #[test]
    fn task_new_fields_roundtrip_via_serde() {
        let mut t = make_minimal_task("t1");
        t.timeout_seconds = Some(120);
        t.max_retries = Some(5);
        let json = serde_json::to_string(&t).unwrap();
        let back: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(back.timeout_seconds, Some(120));
        assert_eq!(back.max_retries, Some(5));
    }

    /// A JSON task file without the new fields should deserialise cleanly
    /// (backward compatibility).
    #[test]
    fn task_new_fields_backward_compatible() {
        let json = r#"{"id":"t1","description":"d","status":"pending"}"#;
        let t: Task = serde_json::from_str(json).unwrap();
        assert!(t.timeout_seconds.is_none());
        assert!(t.max_retries.is_none());
    }
}
