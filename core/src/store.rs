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
    fn load_provenance_records(&self, task_id: &str) -> Result<Vec<ProvenanceRecord>, StoreError>;

    /// Persist a single provenance record.
    fn persist_provenance_record(&self, record: &ProvenanceRecord) -> Result<(), StoreError>;
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, TaskKind, TaskStatus};
    use std::cell::RefCell;

    // ---- StoreError tests ----

    #[test]
    fn store_error_display() {
        let err = StoreError::new("something went wrong");
        assert_eq!(err.to_string(), "something went wrong");
    }

    #[test]
    fn store_error_is_std_error() {
        let err = StoreError::new("oops");
        let _: &dyn std::error::Error = &err;
    }

    // ---- In-memory TaskStore for trait validation ----

    struct InMemoryTaskStore {
        tasks: RefCell<Vec<Task>>,
    }

    impl InMemoryTaskStore {
        fn new() -> Self {
            Self {
                tasks: RefCell::new(Vec::new()),
            }
        }
    }

    impl TaskStore for InMemoryTaskStore {
        fn load_tasks(&self) -> Result<Vec<Task>, StoreError> {
            Ok(self.tasks.borrow().clone())
        }

        fn save_tasks(&self, tasks: &[Task]) -> Result<(), StoreError> {
            *self.tasks.borrow_mut() = tasks.to_vec();
            Ok(())
        }
    }

    fn make_task(id: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {}", id),
            status,
            role: AgentRole::default(),
            kind: TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        }
    }

    #[test]
    fn in_memory_task_store_roundtrip() {
        let store = InMemoryTaskStore::new();
        assert!(store.load_tasks().unwrap().is_empty());

        let tasks = vec![make_task("a", TaskStatus::Pending)];
        store.save_tasks(&tasks).unwrap();

        let loaded = store.load_tasks().unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].id, "a");
    }

    // ---- In-memory ProvenanceStore for trait validation ----

    struct InMemoryProvenanceStore {
        records: RefCell<Vec<ProvenanceRecord>>,
    }

    impl InMemoryProvenanceStore {
        fn new() -> Self {
            Self {
                records: RefCell::new(Vec::new()),
            }
        }
    }

    impl ProvenanceStore for InMemoryProvenanceStore {
        fn load_provenance_records(
            &self,
            task_id: &str,
        ) -> Result<Vec<ProvenanceRecord>, StoreError> {
            let mut filtered: Vec<ProvenanceRecord> = self
                .records
                .borrow()
                .iter()
                .filter(|r| r.task_id == task_id)
                .cloned()
                .collect();
            filtered.sort_by_key(|r| r.timestamp);
            Ok(filtered)
        }

        fn persist_provenance_record(&self, record: &ProvenanceRecord) -> Result<(), StoreError> {
            self.records.borrow_mut().push(record.clone());
            Ok(())
        }
    }

    #[test]
    fn in_memory_provenance_store_roundtrip() {
        let store = InMemoryProvenanceStore::new();
        assert!(store.load_provenance_records("t1").unwrap().is_empty());

        let record = ProvenanceRecord {
            task_id: "t1".to_string(),
            agent_role: AgentRole::Implementer,
            model: "test".to_string(),
            prompt_hash: "abc".to_string(),
            tool_calls: vec![],
            git_diff_hash: "def".to_string(),
            timestamp: 100,
            outcome: "success".to_string(),
        };
        store.persist_provenance_record(&record).unwrap();

        let loaded = store.load_provenance_records("t1").unwrap();
        assert_eq!(loaded.len(), 1);
        assert_eq!(loaded[0].task_id, "t1");

        // Different task_id returns empty.
        assert!(store.load_provenance_records("t2").unwrap().is_empty());
    }

    /// Verify that business logic can be written generically over the trait.
    fn count_pending_tasks(store: &dyn TaskStore) -> Result<usize, StoreError> {
        let tasks = store.load_tasks()?;
        Ok(tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Pending)
            .count())
    }

    #[test]
    fn generic_function_over_task_store_trait() {
        let store = InMemoryTaskStore::new();
        store
            .save_tasks(&[
                make_task("a", TaskStatus::Pending),
                make_task("b", TaskStatus::Completed),
            ])
            .unwrap();
        assert_eq!(count_pending_tasks(&store).unwrap(), 1);
    }

    #[test]
    fn generic_function_all_completed() {
        let store = InMemoryTaskStore::new();
        store
            .save_tasks(&[
                make_task("a", TaskStatus::Completed),
                make_task("b", TaskStatus::Completed),
            ])
            .unwrap();
        assert_eq!(count_pending_tasks(&store).unwrap(), 0);
    }

    #[test]
    fn generic_function_all_pending() {
        let store = InMemoryTaskStore::new();
        store
            .save_tasks(&[
                make_task("a", TaskStatus::Pending),
                make_task("b", TaskStatus::Pending),
            ])
            .unwrap();
        assert_eq!(count_pending_tasks(&store).unwrap(), 2);
    }

    #[test]
    fn generic_function_empty_store() {
        let store = InMemoryTaskStore::new();
        assert_eq!(count_pending_tasks(&store).unwrap(), 0);
    }
}
