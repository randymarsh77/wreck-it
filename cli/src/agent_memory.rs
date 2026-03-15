use anyhow::{Context, Result};
use std::fs;
use std::path::{Path, PathBuf};

/// Manages persistent per-task memory stored as Markdown files under
/// `<work_dir>/.wreck-it-memory/{task_id}.md`.
///
/// Each file records previous attempt outcomes so that the agent can learn
/// from prior iterations rather than starting completely fresh each time.
pub struct AgentMemory {
    memory_dir: PathBuf,
}

impl AgentMemory {
    /// Create an `AgentMemory` rooted at `<work_dir>/.wreck-it-memory/`.
    pub fn new(work_dir: &str) -> Self {
        Self {
            memory_dir: Path::new(work_dir).join(".wreck-it-memory"),
        }
    }

    /// Return the path of the memory file for the given task ID.
    fn memory_path(&self, task_id: &str) -> PathBuf {
        // Sanitize the task ID so it is safe to use as a filename.
        let safe_id: String = task_id
            .chars()
            .map(|c| {
                if c.is_alphanumeric() || c == '-' || c == '_' {
                    c
                } else {
                    '_'
                }
            })
            .collect();
        self.memory_dir.join(format!("{}.md", safe_id))
    }

    /// Load the memory context for a task.
    ///
    /// Returns the Markdown content of the file, or an empty string when no
    /// memory file exists yet.
    pub fn load_context(&self, task_id: &str) -> Result<String> {
        let path = self.memory_path(task_id);
        if !path.exists() {
            return Ok(String::new());
        }
        fs::read_to_string(&path)
            .with_context(|| format!("Failed to read memory file: {}", path.display()))
    }

    /// Count the number of attempts already recorded for the given task.
    ///
    /// This is used to derive a 1-based iteration number for the next entry.
    pub fn attempt_count(&self, task_id: &str) -> usize {
        let path = self.memory_path(task_id);
        if !path.exists() {
            return 0;
        }
        fs::read_to_string(&path)
            .map(|content| content.matches("### Attempt ").count())
            .unwrap_or(0)
    }

    /// Store an optimised task description produced by the prompt optimizer.
    ///
    /// The description is written to an `## Optimized Description` section at
    /// the end of the memory file.  Any previously stored optimized description
    /// is replaced.
    pub fn store_optimized_description(&self, task_id: &str, description: &str) -> Result<()> {
        fs::create_dir_all(&self.memory_dir).with_context(|| {
            format!(
                "Failed to create memory directory: {}",
                self.memory_dir.display()
            )
        })?;

        let path = self.memory_path(task_id);

        let existing = if path.exists() {
            fs::read_to_string(&path)
                .with_context(|| format!("Failed to read memory file: {}", path.display()))?
        } else {
            format!("# Task Memory: {}\n\n## Previous Attempts\n", task_id)
        };

        // Remove any existing optimized description section before appending the
        // new one so there is always at most one such section per file.
        let base = if let Some(idx) = existing.find("\n## Optimized Description\n") {
            existing[..idx].to_string()
        } else {
            existing
        };

        let updated = format!("{}\n## Optimized Description\n\n{}\n", base, description);
        fs::write(&path, updated)
            .with_context(|| format!("Failed to write memory file: {}", path.display()))?;

        Ok(())
    }

    /// Load a previously stored optimised task description from memory.
    ///
    /// Returns `None` when no `## Optimized Description` section exists in the
    /// memory file (i.e. the prompt optimizer has not yet run for this task).
    pub fn load_optimized_description(&self, task_id: &str) -> Result<Option<String>> {
        let path = self.memory_path(task_id);
        if !path.exists() {
            return Ok(None);
        }
        let content = fs::read_to_string(&path)
            .with_context(|| format!("Failed to read memory file: {}", path.display()))?;

        // Find the `## Optimized Description` header and extract the content
        // that follows it (stopping at the next `##`-level header or EOF).
        if let Some(start) = content.find("\n## Optimized Description\n") {
            let after_header = &content[start + "\n## Optimized Description\n".len()..];
            // Strip leading blank line(s) and stop at the next `##` header.
            let body = if let Some(next) = after_header.find("\n## ") {
                after_header[..next].trim().to_string()
            } else {
                after_header.trim().to_string()
            };
            if body.is_empty() {
                Ok(None)
            } else {
                Ok(Some(body))
            }
        } else {
            Ok(None)
        }
    }

    ///
    /// If the file does not yet exist it is created along with all necessary
    /// parent directories.
    pub fn record_attempt(
        &self,
        task_id: &str,
        iteration: usize,
        outcome: &str,
        summary: &str,
    ) -> Result<()> {
        fs::create_dir_all(&self.memory_dir).with_context(|| {
            format!(
                "Failed to create memory directory: {}",
                self.memory_dir.display()
            )
        })?;

        let path = self.memory_path(task_id);

        // Build or extend the file content.
        let existing = if path.exists() {
            fs::read_to_string(&path)
                .with_context(|| format!("Failed to read memory file: {}", path.display()))?
        } else {
            format!("# Task Memory: {}\n\n## Previous Attempts\n", task_id)
        };

        let entry = format!("\n### Attempt {} - {}\n{}\n", iteration, outcome, summary);

        let updated = format!("{}{}", existing, entry);
        fs::write(&path, updated)
            .with_context(|| format!("Failed to write memory file: {}", path.display()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn load_context_returns_empty_when_no_file() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());
        let ctx = memory.load_context("task-1").unwrap();
        assert!(ctx.is_empty());
    }

    #[test]
    fn record_attempt_creates_file_and_directory() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .record_attempt("task-1", 1, "Failure", "Compilation error: missing import")
            .unwrap();

        let path = dir.path().join(".wreck-it-memory").join("task-1.md");
        assert!(path.exists());
        let content = fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Task Memory: task-1"));
        assert!(content.contains("Attempt 1"));
        assert!(content.contains("Failure"));
        assert!(content.contains("Compilation error: missing import"));
    }

    #[test]
    fn record_attempt_appends_multiple_entries() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .record_attempt("task-2", 1, "Failure", "Build failed")
            .unwrap();
        memory
            .record_attempt("task-2", 2, "Success", "Added missing dependency")
            .unwrap();

        let ctx = memory.load_context("task-2").unwrap();
        assert!(ctx.contains("Attempt 1"));
        assert!(ctx.contains("Attempt 2"));
        assert!(ctx.contains("Failure"));
        assert!(ctx.contains("Success"));
    }

    #[test]
    fn load_context_returns_recorded_content() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .record_attempt(
                "task-3",
                1,
                "Failure",
                "Test failed due to off-by-one error",
            )
            .unwrap();

        let ctx = memory.load_context("task-3").unwrap();
        assert!(!ctx.is_empty());
        assert!(ctx.contains("off-by-one"));
    }

    #[test]
    fn memory_path_sanitizes_special_characters() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        // Record an attempt with a task ID that has special characters.
        memory
            .record_attempt("task/with/slashes", 1, "Success", "done")
            .unwrap();

        // The file should exist with slashes replaced.
        let path = dir
            .path()
            .join(".wreck-it-memory")
            .join("task_with_slashes.md");
        assert!(path.exists());
    }

    #[test]
    fn attempt_count_is_zero_before_any_records() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());
        assert_eq!(memory.attempt_count("task-x"), 0);
    }

    #[test]
    fn attempt_count_increments_with_each_record() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        assert_eq!(memory.attempt_count("task-y"), 0);
        memory
            .record_attempt("task-y", 1, "Failure", "first")
            .unwrap();
        assert_eq!(memory.attempt_count("task-y"), 1);
        memory
            .record_attempt("task-y", 2, "Success", "second")
            .unwrap();
        assert_eq!(memory.attempt_count("task-y"), 2);
    }

    #[test]
    fn separate_tasks_get_separate_files() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .record_attempt("alpha", 1, "Success", "task alpha done")
            .unwrap();
        memory
            .record_attempt("beta", 1, "Failure", "task beta failed")
            .unwrap();

        let ctx_alpha = memory.load_context("alpha").unwrap();
        let ctx_beta = memory.load_context("beta").unwrap();

        assert!(ctx_alpha.contains("alpha done"));
        assert!(!ctx_alpha.contains("beta failed"));
        assert!(ctx_beta.contains("beta failed"));
        assert!(!ctx_beta.contains("alpha done"));
    }

    // ---- optimized description tests ----

    #[test]
    fn load_optimized_description_returns_none_when_no_file() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());
        let result = memory.load_optimized_description("no-such-task").unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn store_and_load_optimized_description_roundtrip() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .store_optimized_description("task-opt", "Rewritten: do the thing properly")
            .unwrap();

        let loaded = memory
            .load_optimized_description("task-opt")
            .unwrap()
            .expect("expected a stored description");
        assert_eq!(loaded, "Rewritten: do the thing properly");
    }

    #[test]
    fn store_optimized_description_replaces_previous() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .store_optimized_description("task-repl", "First version")
            .unwrap();
        memory
            .store_optimized_description("task-repl", "Second version")
            .unwrap();

        let loaded = memory
            .load_optimized_description("task-repl")
            .unwrap()
            .expect("expected a stored description");
        assert_eq!(loaded, "Second version");
        // The file must not contain "First version" any more.
        let raw = memory.load_context("task-repl").unwrap();
        assert!(!raw.contains("First version"));
    }

    #[test]
    fn optimized_description_coexists_with_attempt_records() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        memory
            .record_attempt("task-combo", 1, "Failure", "build failed")
            .unwrap();
        memory
            .store_optimized_description("task-combo", "Clearer description")
            .unwrap();

        // Both the attempt record and the optimized description must be present.
        let ctx = memory.load_context("task-combo").unwrap();
        assert!(ctx.contains("build failed"));
        assert!(ctx.contains("Clearer description"));

        let opt = memory
            .load_optimized_description("task-combo")
            .unwrap()
            .expect("expected description");
        assert_eq!(opt, "Clearer description");
    }

    #[test]
    fn load_optimized_description_returns_none_when_section_missing() {
        let dir = tempdir().unwrap();
        let memory = AgentMemory::new(dir.path().to_str().unwrap());

        // Only record attempts, no optimized description stored.
        memory
            .record_attempt("task-noopt", 1, "Failure", "some error")
            .unwrap();

        let result = memory.load_optimized_description("task-noopt").unwrap();
        assert!(result.is_none());
    }
}
