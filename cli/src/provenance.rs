use anyhow::{Context, Result};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};
use wreck_it_core::store::{ProvenanceStore, StoreError};

// Re-export from wreck-it-core so callers of `crate::provenance::*`
// continue to work unchanged.
pub use wreck_it_core::provenance::hash_string;
pub use wreck_it_core::types::ProvenanceRecord;

/// Compute a hex hash of the current uncommitted git diff in `work_dir`.
/// Returns `"0000000000000000"` when the diff cannot be obtained.
pub fn git_diff_hash(work_dir: &Path) -> String {
    let output = Command::new("git")
        .args(["diff", "HEAD"])
        .current_dir(work_dir)
        .output();
    match output {
        Ok(out) => hash_string(&String::from_utf8_lossy(&out.stdout)),
        Err(_) => "0000000000000000".to_string(),
    }
}

/// Return the current Unix timestamp in seconds.
pub fn now_timestamp() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Load all provenance records for `task_id` from `<work_dir>/.wreck-it-provenance/`.
///
/// Files are matched by the prefix `<task_id>-`.  An empty `Vec` is returned
/// when no matching records exist (or when the directory does not exist yet).
pub fn load_provenance_records(task_id: &str, work_dir: &Path) -> Result<Vec<ProvenanceRecord>> {
    let dir = work_dir.join(".wreck-it-provenance");
    if !dir.exists() {
        return Ok(vec![]);
    }
    let prefix = format!("{}-", task_id);
    let mut records = Vec::new();
    for entry in std::fs::read_dir(&dir).context("Failed to read provenance directory")? {
        let entry = entry.context("Failed to read provenance directory entry")?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(&prefix) && name_str.ends_with(".json") {
            let content =
                std::fs::read_to_string(entry.path()).context("Failed to read provenance file")?;
            let record: ProvenanceRecord =
                serde_json::from_str(&content).context("Failed to parse provenance record")?;
            records.push(record);
        }
    }
    // Sort by timestamp ascending so the most recent record is last.
    records.sort_by_key(|r| r.timestamp);
    Ok(records)
}

/// Persist `record` as a JSON file inside `<work_dir>/.wreck-it-provenance/`.
///
/// Files are named `<task_id>-<timestamp>.json` so that multiple records for
/// the same task (across attempts) are all preserved.
pub fn persist_provenance_record(record: &ProvenanceRecord, work_dir: &Path) -> Result<()> {
    let dir = work_dir.join(".wreck-it-provenance");
    std::fs::create_dir_all(&dir).context("Failed to create provenance directory")?;
    let filename = format!("{}-{}.json", record.task_id, record.timestamp);
    let path = dir.join(&filename);
    let content =
        serde_json::to_string_pretty(record).context("Failed to serialise provenance record")?;
    std::fs::write(&path, content).context("Failed to write provenance record")?;
    Ok(())
}

// ---------------------------------------------------------------------------
// FileProvenanceStore — file-system-backed ProvenanceStore implementation
// ---------------------------------------------------------------------------

/// File-system-backed implementation of [`ProvenanceStore`].
///
/// Stores provenance records as individual JSON files under
/// `<work_dir>/.wreck-it-provenance/`.
#[allow(dead_code)]
pub struct FileProvenanceStore {
    work_dir: PathBuf,
}

#[allow(dead_code)]
impl FileProvenanceStore {
    pub fn new(work_dir: impl Into<PathBuf>) -> Self {
        Self {
            work_dir: work_dir.into(),
        }
    }
}

impl ProvenanceStore for FileProvenanceStore {
    fn load_provenance_records(&self, task_id: &str) -> Result<Vec<ProvenanceRecord>, StoreError> {
        load_provenance_records(task_id, &self.work_dir).map_err(|e| StoreError::new(e.to_string()))
    }

    fn persist_provenance_record(&self, record: &ProvenanceRecord) -> Result<(), StoreError> {
        persist_provenance_record(record, &self.work_dir)
            .map_err(|e| StoreError::new(e.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::AgentRole;
    use tempfile::tempdir;

    // ---- hash_string tests ----

    #[test]
    fn hash_string_is_16_hex_chars() {
        let h = hash_string("hello world");
        assert_eq!(h.len(), 16);
        assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
    }

    #[test]
    fn hash_string_same_input_same_output() {
        assert_eq!(hash_string("task-1"), hash_string("task-1"));
    }

    #[test]
    fn hash_string_different_inputs_different_outputs() {
        assert_ne!(hash_string("task-1"), hash_string("task-2"));
    }

    #[test]
    fn hash_empty_string() {
        let h = hash_string("");
        assert_eq!(h.len(), 16);
    }

    // ---- ProvenanceRecord serialisation tests ----

    fn make_record() -> ProvenanceRecord {
        ProvenanceRecord {
            task_id: "impl-10".to_string(),
            agent_role: AgentRole::Implementer,
            model: "copilot".to_string(),
            prompt_hash: hash_string("do the thing"),
            tool_calls: vec![],
            git_diff_hash: "abcd1234abcd1234".to_string(),
            timestamp: 1_700_000_000,
            outcome: "success".to_string(),
        }
    }

    #[test]
    fn provenance_record_roundtrip() {
        let record = make_record();
        let json = serde_json::to_string(&record).unwrap();
        let loaded: ProvenanceRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.task_id, record.task_id);
        assert_eq!(loaded.agent_role, record.agent_role);
        assert_eq!(loaded.model, record.model);
        assert_eq!(loaded.prompt_hash, record.prompt_hash);
        assert_eq!(loaded.tool_calls, record.tool_calls);
        assert_eq!(loaded.git_diff_hash, record.git_diff_hash);
        assert_eq!(loaded.timestamp, record.timestamp);
        assert_eq!(loaded.outcome, record.outcome);
    }

    #[test]
    fn provenance_record_tool_calls_roundtrip() {
        let mut record = make_record();
        record.tool_calls = vec![
            "execute_task".to_string(),
            "evaluate_completeness".to_string(),
        ];
        let json = serde_json::to_string(&record).unwrap();
        let loaded: ProvenanceRecord = serde_json::from_str(&json).unwrap();
        assert_eq!(
            loaded.tool_calls,
            vec!["execute_task", "evaluate_completeness"]
        );
    }

    // ---- persist_provenance_record tests ----

    #[test]
    fn persist_creates_directory_and_file() {
        let dir = tempdir().unwrap();
        let record = make_record();
        persist_provenance_record(&record, dir.path()).unwrap();
        let prov_dir = dir.path().join(".wreck-it-provenance");
        assert!(prov_dir.exists());
        let expected_file = prov_dir.join(format!("{}-{}.json", record.task_id, record.timestamp));
        assert!(expected_file.exists());
    }

    #[test]
    fn persist_writes_valid_json() {
        let dir = tempdir().unwrap();
        let record = make_record();
        persist_provenance_record(&record, dir.path()).unwrap();
        let prov_dir = dir.path().join(".wreck-it-provenance");
        let filename = format!("{}-{}.json", record.task_id, record.timestamp);
        let content = std::fs::read_to_string(prov_dir.join(filename)).unwrap();
        let loaded: ProvenanceRecord = serde_json::from_str(&content).unwrap();
        assert_eq!(loaded.task_id, "impl-10");
        assert_eq!(loaded.outcome, "success");
    }

    #[test]
    fn persist_multiple_records_for_same_task() {
        let dir = tempdir().unwrap();
        let mut record = make_record();
        persist_provenance_record(&record, dir.path()).unwrap();
        record.timestamp = 1_700_000_001;
        record.outcome = "failure".to_string();
        persist_provenance_record(&record, dir.path()).unwrap();

        let prov_dir = dir.path().join(".wreck-it-provenance");
        let entries: Vec<_> = std::fs::read_dir(&prov_dir)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(entries.len(), 2);
    }

    #[test]
    fn now_timestamp_is_positive() {
        assert!(now_timestamp() > 0);
    }

    // ---- FileProvenanceStore tests ----

    #[test]
    fn file_provenance_store_persist_and_load() {
        let dir = tempdir().unwrap();
        let store = FileProvenanceStore::new(dir.path());

        let record = make_record();
        store.persist_provenance_record(&record).unwrap();

        let records = store.load_provenance_records("impl-10").unwrap();
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id, "impl-10");
        assert_eq!(records[0].outcome, "success");
    }

    #[test]
    fn file_provenance_store_returns_empty_for_missing_task() {
        let dir = tempdir().unwrap();
        let store = FileProvenanceStore::new(dir.path());

        let records = store.load_provenance_records("nonexistent").unwrap();
        assert!(records.is_empty());
    }

    #[test]
    fn file_provenance_store_multiple_records() {
        let dir = tempdir().unwrap();
        let store = FileProvenanceStore::new(dir.path());

        let mut record = make_record();
        store.persist_provenance_record(&record).unwrap();
        record.timestamp = 1_700_000_001;
        record.outcome = "failure".to_string();
        store.persist_provenance_record(&record).unwrap();

        let records = store.load_provenance_records("impl-10").unwrap();
        assert_eq!(records.len(), 2);
        // Should be sorted by timestamp ascending.
        assert_eq!(records[0].timestamp, 1_700_000_000);
        assert_eq!(records[1].timestamp, 1_700_000_001);
    }

    // ---- git_diff_hash multi-repo tests ----

    /// Initialise a minimal git repository in `dir` with one committed file so
    /// that `git diff HEAD` is valid.
    fn init_git_repo(dir: &std::path::Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git init failed in {}: {}", dir.display(), e));
        Command::new("git")
            .args(["config", "user.email", "test@test.com"])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git config user.email failed in {}: {}", dir.display(), e));
        Command::new("git")
            .args(["config", "user.name", "Test"])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git config user.name failed in {}: {}", dir.display(), e));
        std::fs::write(dir.join("README.md"), "# test\n")
            .unwrap_or_else(|e| panic!("failed to write README.md in {}: {}", dir.display(), e));
        Command::new("git")
            .args(["add", "."])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git add failed in {}: {}", dir.display(), e));
        Command::new("git")
            .args(["commit", "-m", "initial"])
            .current_dir(dir)
            .output()
            .unwrap_or_else(|e| panic!("git commit failed in {}: {}", dir.display(), e));
    }

    /// `git_diff_hash` must reflect the state of the directory it is called on.
    /// When a task is mapped to a secondary repository, git operations (and
    /// therefore the diff captured by provenance) correspond to that secondary
    /// repo, not the default one.
    #[test]
    fn git_diff_hash_reads_from_specified_work_dir() {
        let default_dir = tempdir().unwrap();
        let secondary_dir = tempdir().unwrap();

        init_git_repo(default_dir.path());
        init_git_repo(secondary_dir.path());

        // Both repos start clean – their hashes should be equal (both hash
        // the empty string produced by `git diff HEAD` on a clean tree).
        let clean_default = git_diff_hash(default_dir.path());
        let clean_secondary = git_diff_hash(secondary_dir.path());
        assert_eq!(
            clean_default, clean_secondary,
            "clean repos should produce the same diff hash"
        );

        // Introduce an uncommitted change in the secondary repo only.
        std::fs::write(secondary_dir.path().join("feature.rs"), "fn foo() {}\n").unwrap();

        let default_hash = git_diff_hash(default_dir.path());
        let _secondary_hash = git_diff_hash(secondary_dir.path());

        // The secondary repo now has an untracked file; git diff HEAD does not
        // capture untracked files, so the diff is still empty.  Commit the file
        // instead to produce a tracked modification.
        Command::new("git")
            .args(["add", "feature.rs"])
            .current_dir(secondary_dir.path())
            .output()
            .unwrap();
        Command::new("git")
            .args(["commit", "-m", "add feature"])
            .current_dir(secondary_dir.path())
            .output()
            .unwrap();
        // Now modify the committed file so `git diff HEAD` is non-empty.
        std::fs::write(
            secondary_dir.path().join("feature.rs"),
            "fn foo() { todo!() }\n",
        )
        .unwrap();

        let default_hash_after = git_diff_hash(default_dir.path());
        let secondary_hash_after = git_diff_hash(secondary_dir.path());

        // The default repo is still clean – its hash is unchanged.
        assert_eq!(
            default_hash, default_hash_after,
            "default repo must not be affected by changes in the secondary repo"
        );

        // The secondary repo has an uncommitted modification, so its hash
        // must differ from a clean repo's hash.
        assert_ne!(
            secondary_hash_after, clean_secondary,
            "secondary repo hash must reflect its uncommitted changes"
        );

        // The two repos now produce different hashes, confirming that
        // git_diff_hash reads from whichever directory is passed to it – which
        // is the directory resolved by work_dirs for the current task.
        assert_ne!(
            default_hash_after, secondary_hash_after,
            "git_diff_hash must return different values for different repositories"
        );
    }
}
