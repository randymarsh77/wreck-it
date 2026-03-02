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
mod gastown_client;
mod headless;
mod headless_config;
mod headless_state;
#[cfg(test)]
mod integration_eval;
mod openclaw;
mod planner;
mod provenance;
mod ralph_loop;
mod replanner;
mod repo_config;
mod state_worktree;
mod templates;
mod tui;

pub mod ffi;
pub mod project_api;
pub mod task_manager;
pub mod types;
