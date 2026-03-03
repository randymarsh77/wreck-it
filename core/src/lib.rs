//! `wreck-it-core` — shared domain types and iteration logic for the
//! wreck-it orchestration framework.
//!
//! This crate is the canonical source for types and business logic that
//! are consumed by both the native CLI (`wreck-it`) and the Cloudflare
//! Worker (`wreck-it-worker`).  It has no native-only dependencies and
//! compiles cleanly to `wasm32-unknown-unknown`.
//!
//! # Feature flags
//!
//! - **`clap`** — enables `clap::ValueEnum` derives on selected enums
//!   (`AgentRole`, etc.) so the CLI can use them directly.

pub mod config;
pub mod iteration;
pub mod state;
pub mod types;
