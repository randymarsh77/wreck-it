//! wreck-it library — programmatic access to the wreck-it task management
//! engine.
//!
//! The primary entry points are:
//!
//! * [`project_api::ProjectManager`] — high-level CRUD API for epics and
//!   sub-tasks.
//! * [`ffi`] — C-compatible FFI functions for consumption from Swift (or
//!   any other C-ABI consumer).
//! * [`types`] — core domain types (`Task`, `TaskStatus`, etc.).
//! * [`task_manager`] — low-level task file I/O.

// Internal modules are shared with the binary; suppress unused warnings for
// items that are only called from `main.rs`.
#![allow(dead_code)]

mod agent;
mod agent_memory;
mod artefact_store;
mod cli;
mod cloud_agent;
mod config_manager;
mod cost_tracker;
mod error_classifier;
mod gastown_client;
mod github_auth;
mod github_client;
mod graph;
mod headless;
mod headless_config;
mod headless_state;
mod install;
#[cfg(test)]
mod integration_eval;
mod notifier;
mod openclaw;
mod plan_migration;
mod plan_wizard;
mod planner;
mod prompt_loader;
mod provenance;
mod ralph_loop;
mod replanner;
mod repo_config;
mod report;
mod semantic_eval;
mod state_worktree;
mod task_cli;
mod templates;
mod tui;
mod unstuck;

pub mod ffi;
pub mod project_api;
pub mod task_manager;
pub mod types;

/// Shared helpers for unit tests.
#[cfg(test)]
pub(crate) mod test_helpers {
    use std::sync::Mutex;

    /// Serialize any test that reads or writes environment variables so that
    /// concurrent test threads cannot interfere with each other.
    pub static ENV_LOCK: Mutex<()> = Mutex::new(());
}
