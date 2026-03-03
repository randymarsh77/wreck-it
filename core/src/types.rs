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
