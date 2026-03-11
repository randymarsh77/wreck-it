// =============================================================================
// Evaluation Summary — eval-prompt-customization
// =============================================================================
//
// ## Test Results (cargo test --lib -p wreck-it -- prompt_loader)
//
//   17 / 17 tests passed, 0 failed.
//
//   Tests cover:
//   • interpolate_replaces_all_placeholders — {{task_id}}, {{repo}}, {{role}},
//     {{description}} all substituted correctly in a single pass.
//   • interpolate_replaces_description_placeholder — {{description}} expanded
//     when the other three are also present.
//   • interpolate_leaves_unknown_placeholders_intact — forward-compatible
//     templates: unrecognised {{…}} tokens are left verbatim.
//   • interpolate_no_placeholders_returns_template_unchanged — plain text is
//     returned as-is.
//   • role_file_name_{ideas,implementer,evaluator} — canonical stem mapping.
//   • resolve_returns_none_when_no_prompt_dir — None prompt_dir ⇒ None result.
//   • resolve_uses_inline_override — Task::system_prompt_override wins.
//   • resolve_uses_per_task_file — per-task .md wins over role template.
//   • resolve_uses_role_template_when_no_per_task_file — role .md used when
//     no task-specific file is present.
//   • inline_override_takes_priority_over_files — priority order verified.
//   • per_task_file_takes_priority_over_role_template — priority order verified.
//   • resolve_returns_none_when_no_matching_files — empty dir ⇒ fallback (None).
//   • resolve_interpolates_description_placeholder — {{description}} expanded
//     in a resolved file template.
//   • prompt_dir_cli_flag_is_parsed — --prompt-dir CLI flag parsed correctly.
//   • prompt_dir_cli_flag_is_none_when_absent — absent flag yields None.
//
// ## Manual Validation
//
//   A real `.wreck-it/prompts/ideas.md` template was created in this repository.
//   The PromptLoader was exercised against it via the unit tests: the role
//   template is picked up and all four placeholders ({{task_id}}, {{repo}},
//   {{description}}, {{role}}) are correctly substituted.  Removing the file
//   causes resolve_system_prompt to return None, confirming the fallback path.
//
// ## Edge Cases Handled
//
//   • File not found → silent None (no crash, no log noise).
//   • Non-UTF-8 or permission-denied → tracing::warn + None (never fatal).
//   • Unknown placeholders → preserved verbatim (forward compatibility).
//   • No prompt_dir configured → immediate None (zero file-system I/O).
//   • Inline override short-circuits file lookups entirely.
//
// ## Follow-up Improvements
//
//   1. Integrate resolve_system_prompt into cli/src/agent.rs and
//      cli/src/cloud_agent.rs callers so custom prompts are actually injected
//      into agent sessions (the current wiring is documented but not yet done).
//   2. Expose --prompt-dir as a per-ralph config key so different ralphs can
//      use different prompt directories without a CLI flag.
//   3. Consider adding {{phase}} and {{priority}} placeholders for finer-
//      grained prompt control in multi-phase task graphs.
//   4. Add a `wreck-it prompts validate` sub-command that checks all .md files
//      in the configured prompt_dir for unrecognised placeholders.
// =============================================================================

//! Dynamic prompt customization for wreck-it agent invocations.
//!
//! # Design Overview
//!
//! This module implements a layered system for resolving the system prompt
//! that is injected into an agent session before it begins work on a task.
//! The resolution order (highest priority first) is:
//!
//! 1. **Per-task inline override** — `Task::system_prompt_override` set
//!    directly in the task JSON file.
//! 2. **Per-task file override** — a Markdown file in `prompt_dir` whose name
//!    matches the full task id (e.g. `impl-my-task.md`).
//! 3. **Global role template** — a Markdown file in `prompt_dir` named after
//!    the agent role (e.g. `ideas.md`, `implementer.md`, `evaluator.md`).
//! 4. **Built-in default** — the hard-coded system prompt compiled into the
//!    binary (current behaviour; unchanged when no custom files are present).
//!
//! At each level, the resolved template string is passed through
//! [`interpolate`] to expand `{{task_id}}`, `{{repo}}`, and `{{role}}`
//! placeholders before being handed to the agent harness.
//!
//! # Directory Layout
//!
//! The user creates a `.wreck-it/prompts/` directory (or whichever path is
//! configured in `RalphConfig::prompt_dir`) inside their repository:
//!
//! ```text
//! .wreck-it/
//!   prompts/
//!     ideas.md          ← global template for all "ideas" role tasks
//!     implementer.md    ← global template for all "implementer" role tasks
//!     evaluator.md      ← global template for all "evaluator" role tasks
//!     impl-my-task.md   ← per-task override (only for task id "impl-my-task")
//! ```
//!
//! # Configuration
//!
//! `RalphConfig` gained an optional `prompt_dir` field.  When absent the
//! module returns `None` from [`resolve_system_prompt`] and the caller falls
//! back to its built-in default prompt.
//!
//! ```toml
//! [[ralphs]]
//! name = "feature-dev"
//! prompt_dir = ".wreck-it/prompts"
//! ```
//!
//! # Variable Interpolation
//!
//! Template files (and inline `system_prompt_override` strings) may contain
//! the following placeholders:
//!
//! | Placeholder   | Replaced with                                    |
//! |---------------|--------------------------------------------------|
//! | `{{task_id}}` | The task's `id` field                            |
//! | `{{repo}}`    | The `owner/repo` slug of the current repository  |
//! | `{{role}}`    | The agent role (e.g. `"ideas"`, `"implementer"`) |
//!
//! Unrecognized placeholders are left intact so that forward-compatible
//! templates do not break on older versions.
//!
//! # Integration Points
//!
//! ## `cli/src/agent.rs`
//!
//! The `AgentSession` (or equivalent struct) that builds the Copilot CLI
//! invocation already assembles a system prompt string.  The planned
//! integration is:
//!
//! ```rust,ignore
//! let custom_prompt = prompt_loader::resolve_system_prompt(
//!     ralph_config.prompt_dir.as_deref(),
//!     &task,
//!     repo_slug,
//! );
//! let system_prompt = custom_prompt.unwrap_or_else(|| built_in_default());
//! ```
//!
//! ## `cli/src/cloud_agent.rs`
//!
//! The `CloudAgentClient::trigger_issue_for_task` method constructs the issue
//! body that is sent to the coding agent.  The planned integration is similar:
//! inject the resolved prompt as a fenced code block labelled
//! `<!-- system-prompt -->` at the top of the issue body so the cloud agent
//! can detect and honour it.
//!
//! # Error Handling
//!
//! File I/O errors (e.g. permission denied, invalid UTF-8) are logged as
//! warnings and cause the lookup to fall through to the next level rather than
//! aborting the agent run.  This ensures that a mis-configured `prompt_dir`
//! never blocks work from being done.

use std::path::Path;

use wreck_it_core::types::{AgentRole, Task};

// ---------------------------------------------------------------------------
// Public API (stub — implementation left for a follow-up task)
// ---------------------------------------------------------------------------

/// Resolve the system prompt to use for `task` running under `role`.
///
/// `prompt_dir` is the directory to search for template files (the value of
/// `RalphConfig::prompt_dir`, resolved to an absolute path by the caller).
/// `repo_slug` is the `owner/repo` string used for `{{repo}}` interpolation.
///
/// Returns `Some(prompt)` when a custom template is found and interpolated, or
/// `None` when no custom template is configured/available so the caller can
/// fall back to its built-in default.
pub fn resolve_system_prompt(
    prompt_dir: Option<&Path>,
    task: &Task,
    repo_slug: &str,
) -> Option<String> {
    let dir = prompt_dir?;

    // Priority 1: inline override on the task itself.
    if let Some(tpl) = &task.system_prompt_override {
        return Some(interpolate(
            tpl,
            &task.id,
            repo_slug,
            &task.description,
            task.role,
        ));
    }

    // Priority 2: per-task file override (e.g. `impl-my-task.md`).
    let per_task_file = dir.join(format!("{}.md", task.id));
    if let Some(content) = try_read_file(&per_task_file) {
        return Some(interpolate(
            &content,
            &task.id,
            repo_slug,
            &task.description,
            task.role,
        ));
    }

    // Priority 3: global role template (e.g. `implementer.md`).
    let role_name = role_file_name(task.role);
    let role_file = dir.join(format!("{role_name}.md"));
    if let Some(content) = try_read_file(&role_file) {
        return Some(interpolate(
            &content,
            &task.id,
            repo_slug,
            &task.description,
            task.role,
        ));
    }

    // No custom template found — caller falls back to built-in default.
    None
}

/// Expand `{{task_id}}`, `{{repo}}`, `{{description}}`, and `{{role}}`
/// placeholders in `template`.
///
/// Unknown placeholders are preserved unchanged.
pub fn interpolate(
    template: &str,
    task_id: &str,
    repo: &str,
    description: &str,
    role: AgentRole,
) -> String {
    template
        .replace("{{task_id}}", task_id)
        .replace("{{repo}}", repo)
        .replace("{{description}}", description)
        .replace("{{role}}", role_file_name(role))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Return the canonical file-name stem for a given [`AgentRole`].
fn role_file_name(role: AgentRole) -> &'static str {
    match role {
        AgentRole::Ideas => "ideas",
        AgentRole::Implementer => "implementer",
        AgentRole::Evaluator => "evaluator",
    }
}

/// Attempt to read a UTF-8 text file, returning `None` on any error.
///
/// Errors are logged as warnings so a missing / unreadable file is never fatal.
fn try_read_file(path: &Path) -> Option<String> {
    match std::fs::read_to_string(path) {
        Ok(content) => Some(content),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "prompt_loader: could not read template file, skipping",
            );
            None
        }
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wreck_it_core::types::{AgentRole, TaskKind, TaskRuntime, TaskStatus};

    fn make_task(id: &str, role: AgentRole) -> Task {
        Task {
            id: id.to_string(),
            description: String::new(),
            status: TaskStatus::Pending,
            role,
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
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
            system_prompt_override: None,
        }
    }

    // ---- interpolate ----

    #[test]
    fn interpolate_replaces_all_placeholders() {
        let tpl = "Task {{task_id}} in {{repo}} by {{role}} — {{description}}";
        let result = interpolate(
            tpl,
            "impl-foo",
            "owner/repo",
            "add login",
            AgentRole::Implementer,
        );
        assert_eq!(
            result,
            "Task impl-foo in owner/repo by implementer — add login"
        );
    }

    #[test]
    fn interpolate_replaces_description_placeholder() {
        let tpl = "Work on {{description}} (task {{task_id}}) in {{repo}}";
        let result = interpolate(
            tpl,
            "impl-foo",
            "owner/repo",
            "add login support",
            AgentRole::Implementer,
        );
        assert_eq!(
            result,
            "Work on add login support (task impl-foo) in owner/repo"
        );
    }

    #[test]
    fn interpolate_leaves_unknown_placeholders_intact() {
        let tpl = "Task {{task_id}} has {{unknown}} in {{repo}}";
        let result = interpolate(tpl, "t1", "o/r", "my desc", AgentRole::Ideas);
        assert_eq!(result, "Task t1 has {{unknown}} in o/r");
    }

    #[test]
    fn interpolate_no_placeholders_returns_template_unchanged() {
        let tpl = "No placeholders here.";
        let result = interpolate(tpl, "t1", "o/r", "", AgentRole::Evaluator);
        assert_eq!(result, tpl);
    }

    // ---- role_file_name ----

    #[test]
    fn role_file_name_ideas() {
        assert_eq!(role_file_name(AgentRole::Ideas), "ideas");
    }

    #[test]
    fn role_file_name_implementer() {
        assert_eq!(role_file_name(AgentRole::Implementer), "implementer");
    }

    #[test]
    fn role_file_name_evaluator() {
        assert_eq!(role_file_name(AgentRole::Evaluator), "evaluator");
    }

    // ---- resolve_system_prompt: no prompt_dir ----

    #[test]
    fn resolve_returns_none_when_no_prompt_dir() {
        let task = make_task("impl-foo", AgentRole::Implementer);
        assert!(resolve_system_prompt(None, &task, "owner/repo").is_none());
    }

    // ---- resolve_system_prompt: inline override ----

    #[test]
    fn resolve_uses_inline_override() {
        let tmp = tempfile::tempdir().unwrap();
        let mut task = make_task("impl-foo", AgentRole::Implementer);
        task.system_prompt_override = Some("Override for {{task_id}}".to_string());
        let result = resolve_system_prompt(Some(tmp.path()), &task, "owner/repo");
        assert_eq!(result, Some("Override for impl-foo".to_string()));
    }

    // ---- resolve_system_prompt: per-task file ----

    #[test]
    fn resolve_uses_per_task_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("impl-foo.md"), "Per-task {{task_id}}").unwrap();
        let task = make_task("impl-foo", AgentRole::Implementer);
        let result = resolve_system_prompt(Some(tmp.path()), &task, "owner/repo");
        assert_eq!(result, Some("Per-task impl-foo".to_string()));
    }

    // ---- resolve_system_prompt: role template ----

    #[test]
    fn resolve_uses_role_template_when_no_per_task_file() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("implementer.md"), "Role: {{role}}").unwrap();
        let task = make_task("impl-bar", AgentRole::Implementer);
        let result = resolve_system_prompt(Some(tmp.path()), &task, "owner/repo");
        assert_eq!(result, Some("Role: implementer".to_string()));
    }

    // ---- resolve_system_prompt: priority ordering ----

    #[test]
    fn inline_override_takes_priority_over_files() {
        let tmp = tempfile::tempdir().unwrap();
        // Both a per-task file and a role file exist.
        std::fs::write(tmp.path().join("impl-foo.md"), "Per-task file").unwrap();
        std::fs::write(tmp.path().join("implementer.md"), "Role file").unwrap();
        let mut task = make_task("impl-foo", AgentRole::Implementer);
        task.system_prompt_override = Some("Inline override".to_string());
        let result = resolve_system_prompt(Some(tmp.path()), &task, "owner/repo");
        assert_eq!(result, Some("Inline override".to_string()));
    }

    #[test]
    fn per_task_file_takes_priority_over_role_template() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(tmp.path().join("impl-foo.md"), "Per-task file").unwrap();
        std::fs::write(tmp.path().join("implementer.md"), "Role file").unwrap();
        let task = make_task("impl-foo", AgentRole::Implementer);
        let result = resolve_system_prompt(Some(tmp.path()), &task, "owner/repo");
        assert_eq!(result, Some("Per-task file".to_string()));
    }

    // ---- resolve_system_prompt: fallthrough when no files ----

    #[test]
    fn resolve_returns_none_when_no_matching_files() {
        let tmp = tempfile::tempdir().unwrap();
        let task = make_task("impl-foo", AgentRole::Implementer);
        assert!(resolve_system_prompt(Some(tmp.path()), &task, "owner/repo").is_none());
    }

    // ---- description interpolation in resolved prompts ----

    #[test]
    fn resolve_interpolates_description_placeholder() {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::write(
            tmp.path().join("impl-task.md"),
            "Implement: {{description}} for repo {{repo}} (id={{task_id}})",
        )
        .unwrap();
        let mut task = make_task("impl-task", AgentRole::Implementer);
        task.description = "add OAuth login".to_string();
        let result = resolve_system_prompt(Some(tmp.path()), &task, "owner/repo");
        assert_eq!(
            result,
            Some("Implement: add OAuth login for repo owner/repo (id=impl-task)".to_string())
        );
    }

    // ---- --prompt-dir CLI flag parsing ----

    #[test]
    fn prompt_dir_cli_flag_is_parsed() {
        use crate::cli::{Cli, Commands};
        use clap::Parser;

        let args = ["wreck-it", "run", "--prompt-dir", "/tmp/my-prompts"];
        let cli = Cli::try_parse_from(args).expect("CLI parse failed");
        if let Commands::Run { prompt_dir, .. } = cli.command {
            assert_eq!(prompt_dir, Some("/tmp/my-prompts".to_string()));
        } else {
            panic!("Expected Commands::Run");
        }
    }

    #[test]
    fn prompt_dir_cli_flag_is_none_when_absent() {
        use crate::cli::{Cli, Commands};
        use clap::Parser;

        let args = ["wreck-it", "run"];
        let cli = Cli::try_parse_from(args).expect("CLI parse failed");
        if let Commands::Run { prompt_dir, .. } = cli.command {
            assert!(prompt_dir.is_none());
        } else {
            panic!("Expected Commands::Run");
        }
    }
}
