//! Abstract storage traits for task and provenance persistence.
//!
//! These traits decouple the business logic (scheduling, replanning,
//! provenance tracking) from the underlying storage backend.  The CLI
//! provides a file-system implementation while the worker can supply an
//! API-backed implementation.

use crate::types::{ProvenanceRecord, Task};

// ---------------------------------------------------------------------------
// Error type
// ---------------------------------------------------------------------------

/// Error type returned by store operations.
#[derive(Debug)]
pub struct StoreError {
    pub message: String,
}

impl StoreError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

impl std::fmt::Display for StoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

impl std::error::Error for StoreError {}

// ---------------------------------------------------------------------------
// TaskStore trait
// ---------------------------------------------------------------------------

/// Trait for task persistence operations.
///
/// Implementations provide the actual storage backend — file I/O for the CLI,
/// API calls for the worker, etc.
pub trait TaskStore {
    /// Load all tasks from the store.
    fn load_tasks(&self) -> Result<Vec<Task>, StoreError>;

    /// Persist the full task list, replacing any previous content.
    fn save_tasks(&self, tasks: &[Task]) -> Result<(), StoreError>;
}

// ---------------------------------------------------------------------------
// ProvenanceStore trait
// ---------------------------------------------------------------------------

/// Trait for provenance record persistence.
///
/// Implementations provide the actual storage backend — file I/O for the CLI,
/// API calls for the worker, etc.
pub trait ProvenanceStore {
    /// Load all provenance records for the given task ID, sorted by timestamp
    /// ascending.
    fn load_provenance_records(
        &self,
        task_id: &str,
    ) -> Result<Vec<ProvenanceRecord>, StoreError>;

    /// Persist a single provenance record.
    fn persist_provenance_record(
        &self,
        record: &ProvenanceRecord,
    ) -> Result<(), StoreError>;
}
