//! Semantic changelog generator for the `changelog_generator` agent role.
//!
//! A changelog generator task replaces LLM-based execution with a deterministic
//! pass over the provenance audit trail and artefact manifest.  It:
//!
//! 1. Loads all provenance records from `.wreck-it-provenance/`.
//! 2. Loads the artefact manifest to inspect completed task outputs.
//! 3. Classifies each successful task by **conventional-commit category**:
//!    - `feat`     — task IDs that start with `impl-` or `ideas-`
//!    - `fix`      — task IDs that start with `fix-` or contain `bugfix`
//!    - `test`     — task IDs that start with `test-`
//!    - `security` — task IDs that start with `security-` or produced
//!      findings via the `security_gate` role
//!    - `refactor` — task IDs that start with `refactor-`
//!    - `chore`    — everything else (e.g. `eval-*`, kanban imports, …)
//! 4. Emits a Markdown-formatted CHANGELOG entry with an ISO-8601 date
//!    heading and one bullet per change group.

use crate::artefact_store;
use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use wreck_it_core::types::{AgentRole, ProvenanceRecord};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The conventional-commit category of a change.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ChangeCategory {
    Feature,
    Fix,
    Test,
    Security,
    Refactor,
    Chore,
}

impl ChangeCategory {
    /// Markdown section heading for this category.
    pub fn heading(&self) -> &'static str {
        match self {
            ChangeCategory::Feature => "### Features",
            ChangeCategory::Fix => "### Bug Fixes",
            ChangeCategory::Test => "### Tests",
            ChangeCategory::Security => "### Security",
            ChangeCategory::Refactor => "### Refactors",
            ChangeCategory::Chore => "### Chores",
        }
    }

    /// Conventional-commit prefix used when serialising a single entry.
    pub fn prefix(&self) -> &'static str {
        match self {
            ChangeCategory::Feature => "feat",
            ChangeCategory::Fix => "fix",
            ChangeCategory::Test => "test",
            ChangeCategory::Security => "security",
            ChangeCategory::Refactor => "refactor",
            ChangeCategory::Chore => "chore",
        }
    }
}

/// A single classified change derived from a provenance record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangeEntry {
    pub category: ChangeCategory,
    pub task_id: String,
    pub description: String,
    pub model: String,
    pub timestamp: u64,
}

/// The full output of one changelog generation run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChangelogOutput {
    /// ISO-8601 date string for the generated entry (UTC, date only).
    pub date: String,
    /// All entries grouped by category, ordered feature → fix → test →
    /// security → refactor → chore.
    pub entries: Vec<ChangeEntry>,
    /// The rendered Markdown text written to the output artefact.
    pub markdown: String,
}

// ---------------------------------------------------------------------------
// Classification patterns (conventional-commit prefixes used by wreck-it)
// ---------------------------------------------------------------------------

/// Task ID prefixes that map to the `feat` category.
const FEATURE_PREFIXES: &[&str] = &["impl-", "ideas-", "feat-"];

/// Task ID prefixes / substrings that map to the `fix` category.
const FIX_PREFIXES: &[&str] = &["fix-"];
const FIX_SUBSTRINGS: &[&str] = &["bugfix", "hotfix"];

/// Task ID prefixes that map to the `test` category.
const TEST_PREFIXES: &[&str] = &["test-"];

/// Task ID prefixes / substrings that map to the `security` category.
const SECURITY_PREFIXES: &[&str] = &["security-"];
const SECURITY_SUBSTRINGS: &[&str] = &["sec-"];

/// Task ID prefixes that map to the `refactor` category.
const REFACTOR_PREFIXES: &[&str] = &["refactor-", "refact-"];

/// Delimiter used to detect existing release sections in CHANGELOG.md.
const CHANGELOG_SECTION_DELIMITER: &str = "\n## ";

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Derive the [`ChangeCategory`] of a task from its ID and role.
///
/// The classification is intentionally simple and keyword-based so it works
/// without an LLM: it matches the conventional-commit prefixes that wreck-it's
/// own task-naming convention uses (`impl-*`, `fix-*`, `test-*`, etc.).
pub fn classify_task(task_id: &str, role: AgentRole) -> ChangeCategory {
    // Role-based overrides take priority over name patterns.
    if role == AgentRole::SecurityGate {
        return ChangeCategory::Security;
    }
    let id = task_id.to_lowercase();
    if FEATURE_PREFIXES.iter().any(|p| id.starts_with(p)) {
        ChangeCategory::Feature
    } else if FIX_PREFIXES.iter().any(|p| id.starts_with(p))
        || FIX_SUBSTRINGS.iter().any(|s| id.contains(s))
    {
        ChangeCategory::Fix
    } else if TEST_PREFIXES.iter().any(|p| id.starts_with(p)) {
        ChangeCategory::Test
    } else if SECURITY_PREFIXES.iter().any(|p| id.starts_with(p))
        || SECURITY_SUBSTRINGS.iter().any(|s| id.contains(s))
    {
        ChangeCategory::Security
    } else if REFACTOR_PREFIXES.iter().any(|p| id.starts_with(p)) {
        ChangeCategory::Refactor
    } else {
        ChangeCategory::Chore
    }
}

/// Render the given provenance records and manifest into a changelog entry.
///
/// Only records with `outcome == "success"` are included.  The result
/// contains the rendered Markdown text and the structured data.
///
/// # Parameters
///
/// * `records`         – All provenance records to consider.
/// * `manifest_path`   – Path to the artefact manifest (for optional
///   per-task artefact summaries — future use).
/// * `now_secs`        – Current Unix timestamp used for the date header.
pub fn generate_changelog(
    records: &[ProvenanceRecord],
    _manifest_path: &Path,
    now_secs: u64,
) -> Result<ChangelogOutput> {
    // Deduplicate: keep only the most-recent successful record per task_id.
    let mut by_task: HashMap<String, &ProvenanceRecord> = HashMap::new();
    for rec in records {
        if rec.outcome != "success" {
            continue;
        }
        let entry = by_task.entry(rec.task_id.clone()).or_insert(rec);
        if rec.timestamp > entry.timestamp {
            *entry = rec;
        }
    }

    // Classify and sort entries.
    let mut entries: Vec<ChangeEntry> = by_task
        .values()
        .map(|rec| {
            let category = classify_task(&rec.task_id, rec.agent_role);
            ChangeEntry {
                category,
                task_id: rec.task_id.clone(),
                description: humanize_task_id(&rec.task_id),
                model: rec.model.clone(),
                timestamp: rec.timestamp,
            }
        })
        .collect();

    // Canonical display order.
    let category_order = |c: &ChangeCategory| match c {
        ChangeCategory::Feature => 0,
        ChangeCategory::Fix => 1,
        ChangeCategory::Test => 2,
        ChangeCategory::Security => 3,
        ChangeCategory::Refactor => 4,
        ChangeCategory::Chore => 5,
    };
    entries.sort_by(|a, b| {
        category_order(&a.category)
            .cmp(&category_order(&b.category))
            .then(a.task_id.cmp(&b.task_id))
    });

    // Build the date string from now_secs (UTC, date only).
    let date = format_date_utc(now_secs);

    // Render Markdown.
    let markdown = render_markdown(&date, &entries);

    Ok(ChangelogOutput {
        date,
        entries,
        markdown,
    })
}

/// Write the changelog entry to `output_path`, prepending it to any existing
/// content so older entries are preserved below the new one.
pub fn write_changelog(output: &ChangelogOutput, output_path: &Path) -> Result<()> {
    if let Some(parent) = output_path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create changelog directory")?;
    }

    let existing = if output_path.exists() {
        std::fs::read_to_string(output_path).context("Failed to read existing CHANGELOG.md")?
    } else {
        String::new()
    };

    let new_content = if existing.is_empty() {
        format!("# Changelog\n\n{}\n", output.markdown)
    } else {
        // Insert the new entry after the first `# Changelog` heading (if
        // present) or at the very top of the file.
        // CHANGELOG_SECTION_DELIMITER ("\n## ") marks the start of a release
        // section; the new entry is inserted before the first such marker.
        if let Some(pos) = existing.find(CHANGELOG_SECTION_DELIMITER) {
            // There's already at least one release section; insert before it.
            let (header, rest) = existing.split_at(pos);
            format!("{}\n\n{}\n{}", header, output.markdown, rest)
        } else if existing.starts_with("# Changelog") {
            // Only the top-level heading exists; append below it.
            format!("{}\n\n{}\n", existing.trim_end(), output.markdown)
        } else {
            format!("{}\n\n{}\n", output.markdown, existing)
        }
    };

    std::fs::write(output_path, new_content).context("Failed to write CHANGELOG.md")?;
    Ok(())
}

/// Load all provenance records from `<work_dir>/.wreck-it-provenance/`.
///
/// Files named `<anything>.json` inside the directory are all loaded and
/// returned; any file that fails to parse is silently skipped.
pub fn load_all_provenance_records(work_dir: &Path) -> Vec<ProvenanceRecord> {
    let dir = work_dir.join(".wreck-it-provenance");
    if !dir.exists() {
        return vec![];
    }
    let Ok(read_dir) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut records = Vec::new();
    for entry in read_dir.flatten() {
        let path = entry.path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(rec) = serde_json::from_str::<ProvenanceRecord>(&content) {
            records.push(rec);
        }
    }
    records.sort_by_key(|r| r.timestamp);
    records
}

// ---------------------------------------------------------------------------
// Entry point called from ralph_loop
// ---------------------------------------------------------------------------

/// Execute the changelog generation pass for a task.
///
/// * Loads all provenance records from `work_dir`.
/// * Generates a changelog entry.
/// * Writes it to the output artefact path (or `CHANGELOG.md` by default).
/// * Persists the artefact to the manifest so downstream tasks can consume it.
///
/// Always returns `Ok(())` — the generator is non-blocking and does not
/// reset any predecessor tasks on failure (partial output is still useful).
pub fn run_changelog_generator(
    task: &wreck_it_core::types::Task,
    work_dir: &Path,
    manifest_path: &Path,
    now_secs: u64,
) -> Result<()> {
    let records = load_all_provenance_records(work_dir);

    let output = generate_changelog(&records, manifest_path, now_secs)?;

    let output_path = task
        .outputs
        .first()
        .map(|o| work_dir.join(&o.path))
        .unwrap_or_else(|| work_dir.join("CHANGELOG.md"));

    write_changelog(&output, &output_path)?;

    // Persist the output artefact to the manifest so it can be consumed by
    // release automation as an input.
    if !task.outputs.is_empty() {
        if let Err(e) = artefact_store::persist_output_artefacts(
            manifest_path,
            &task.id,
            &task.outputs,
            work_dir,
        ) {
            // Non-fatal: log but don't fail the task.
            eprintln!(
                "Warning: changelog generator failed to persist output artefact: {}",
                e
            );
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a task ID like `impl-webhook-notifications` into a readable
/// description like `Webhook Notifications` by:
/// 1. Stripping any leading category prefix (`impl-`, `fix-`, `test-`, etc.).
/// 2. Replacing remaining hyphens with spaces.
/// 3. Title-casing each word.
fn humanize_task_id(task_id: &str) -> String {
    // Strip the first word (category prefix) when it is a known wreck-it prefix.
    let known_prefixes = [
        "impl-",
        "ideas-",
        "feat-",
        "fix-",
        "test-",
        "eval-",
        "security-",
        "refactor-",
        "refact-",
    ];
    let stripped = known_prefixes
        .iter()
        .find_map(|p| task_id.strip_prefix(p))
        .unwrap_or(task_id);

    // Title-case: capitalise the first letter of each hyphen-separated word.
    stripped
        .split('-')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn render_markdown(date: &str, entries: &[ChangeEntry]) -> String {
    if entries.is_empty() {
        return format!("## [Unreleased] — {}\n\nNo changes recorded.\n", date);
    }

    let mut md = format!("## [Unreleased] — {}\n\n", date);

    // Group entries by category (maintaining sorted order).
    let mut current_category: Option<&ChangeCategory> = None;
    for entry in entries {
        if current_category != Some(&entry.category) {
            if current_category.is_some() {
                md.push('\n');
            }
            md.push_str(entry.category.heading());
            md.push('\n');
            md.push('\n');
            current_category = Some(&entry.category);
        }
        md.push_str(&format!(
            "- **{}**: {} ({})\n",
            entry.category.prefix(),
            entry.description,
            entry.task_id
        ));
    }

    md
}

/// Format a Unix timestamp as `YYYY-MM-DD` (UTC).
///
/// This uses a simple arithmetic approach that works without external crates.
fn format_date_utc(secs: u64) -> String {
    // Days since Unix epoch.
    let days = secs / 86_400;

    // Civil date from days since epoch.
    // Algorithm adapted from http://howardhinnant.github.io/date_algorithms.html
    let z = days as i64 + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{:04}-{:02}-{:02}", y, m, d)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use wreck_it_core::types::{AgentRole, ProvenanceRecord};

    fn make_record(task_id: &str, role: AgentRole, outcome: &str, ts: u64) -> ProvenanceRecord {
        ProvenanceRecord {
            task_id: task_id.to_string(),
            agent_role: role,
            model: "copilot".to_string(),
            prompt_hash: "0000000000000000".to_string(),
            tool_calls: vec![],
            git_diff_hash: "0000000000000000".to_string(),
            timestamp: ts,
            outcome: outcome.to_string(),
        }
    }

    // ---- classify_task ----

    #[test]
    fn classify_impl_is_feature() {
        assert_eq!(
            classify_task("impl-webhook-notifications", AgentRole::Implementer),
            ChangeCategory::Feature
        );
    }

    #[test]
    fn classify_ideas_is_feature() {
        assert_eq!(
            classify_task("ideas-task-graph-export", AgentRole::Ideas),
            ChangeCategory::Feature
        );
    }

    #[test]
    fn classify_fix_is_fix() {
        assert_eq!(
            classify_task("fix-null-pointer", AgentRole::Implementer),
            ChangeCategory::Fix
        );
    }

    #[test]
    fn classify_test_is_test() {
        assert_eq!(
            classify_task("test-webhook-notifications", AgentRole::Evaluator),
            ChangeCategory::Test
        );
    }

    #[test]
    fn classify_security_gate_role_overrides_name() {
        assert_eq!(
            classify_task("impl-something", AgentRole::SecurityGate),
            ChangeCategory::Security
        );
    }

    #[test]
    fn classify_security_prefix_is_security() {
        assert_eq!(
            classify_task("security-audit", AgentRole::Implementer),
            ChangeCategory::Security
        );
    }

    #[test]
    fn classify_refactor_is_refactor() {
        assert_eq!(
            classify_task("refactor-parser", AgentRole::Implementer),
            ChangeCategory::Refactor
        );
    }

    #[test]
    fn classify_eval_is_chore() {
        assert_eq!(
            classify_task("eval-webhook-notifications", AgentRole::Evaluator),
            ChangeCategory::Chore
        );
    }

    // ---- generate_changelog ----

    #[test]
    fn generate_changelog_empty_records_returns_no_changes() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("artefacts.json");
        let output = generate_changelog(&[], &manifest, 1_700_000_000).unwrap();
        assert!(output.entries.is_empty());
        assert!(output.markdown.contains("No changes recorded"));
    }

    #[test]
    fn generate_changelog_only_successful_records_included() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("artefacts.json");
        let records = vec![
            make_record("impl-feat-a", AgentRole::Implementer, "success", 1000),
            make_record("fix-bug-b", AgentRole::Implementer, "failure", 2000),
        ];
        let output = generate_changelog(&records, &manifest, 1_700_000_000).unwrap();
        assert_eq!(output.entries.len(), 1);
        assert_eq!(output.entries[0].task_id, "impl-feat-a");
    }

    #[test]
    fn generate_changelog_deduplicates_keeps_latest_success() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("artefacts.json");
        let records = vec![
            make_record("impl-feat-a", AgentRole::Implementer, "success", 1000),
            make_record("impl-feat-a", AgentRole::Implementer, "success", 3000),
        ];
        let output = generate_changelog(&records, &manifest, 1_700_000_000).unwrap();
        assert_eq!(output.entries.len(), 1);
        assert_eq!(output.entries[0].timestamp, 3000);
    }

    #[test]
    fn generate_changelog_multiple_categories_sorted() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("artefacts.json");
        let records = vec![
            make_record("test-auth", AgentRole::Evaluator, "success", 100),
            make_record("fix-crash", AgentRole::Implementer, "success", 200),
            make_record("impl-login", AgentRole::Implementer, "success", 300),
        ];
        let output = generate_changelog(&records, &manifest, 1_700_000_000).unwrap();
        // Feature first, fix second, test third.
        assert_eq!(output.entries[0].category, ChangeCategory::Feature);
        assert_eq!(output.entries[1].category, ChangeCategory::Fix);
        assert_eq!(output.entries[2].category, ChangeCategory::Test);
    }

    #[test]
    fn generate_changelog_markdown_contains_headings() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let manifest = dir.path().join("artefacts.json");
        let records = vec![
            make_record("impl-api", AgentRole::Implementer, "success", 1000),
            make_record("fix-typo", AgentRole::Implementer, "success", 2000),
        ];
        let output = generate_changelog(&records, &manifest, 1_700_000_000).unwrap();
        assert!(output.markdown.contains("### Features"));
        assert!(output.markdown.contains("### Bug Fixes"));
    }

    // ---- format_date_utc ----

    #[test]
    fn format_date_utc_known_timestamp() {
        // 2023-11-14 00:00:00 UTC
        assert_eq!(format_date_utc(1_699_920_000), "2023-11-14");
    }

    #[test]
    fn format_date_utc_epoch() {
        assert_eq!(format_date_utc(0), "1970-01-01");
    }

    // ---- write_changelog ----

    #[test]
    fn write_changelog_creates_new_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("CHANGELOG.md");
        let output = ChangelogOutput {
            date: "2024-01-01".to_string(),
            entries: vec![],
            markdown: "## [Unreleased] — 2024-01-01\n\nNo changes recorded.\n".to_string(),
        };
        write_changelog(&output, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        assert!(content.contains("# Changelog"));
        assert!(content.contains("## [Unreleased]"));
    }

    #[test]
    fn write_changelog_prepends_to_existing_file() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let path = dir.path().join("CHANGELOG.md");
        std::fs::write(
            &path,
            "# Changelog\n\n## [Unreleased] — 2023-01-01\n\nOld entry.\n",
        )
        .unwrap();
        let output = ChangelogOutput {
            date: "2024-01-01".to_string(),
            entries: vec![],
            markdown: "## [Unreleased] — 2024-01-01\n\nNew entry.\n".to_string(),
        };
        write_changelog(&output, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        // New entry appears before old entry.
        let new_pos = content.find("New entry").unwrap();
        let old_pos = content.find("Old entry").unwrap();
        assert!(new_pos < old_pos, "new entry should precede old entry");
    }

    // ---- load_all_provenance_records ----

    #[test]
    fn load_all_provenance_records_empty_dir() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let records = load_all_provenance_records(dir.path());
        assert!(records.is_empty());
    }

    #[test]
    fn load_all_provenance_records_loads_json_files() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let prov_dir = dir.path().join(".wreck-it-provenance");
        std::fs::create_dir_all(&prov_dir).unwrap();
        let rec = make_record(
            "impl-feat",
            AgentRole::Implementer,
            "success",
            1_700_000_000,
        );
        let content = serde_json::to_string(&rec).unwrap();
        std::fs::write(prov_dir.join("impl-feat-1700000000.json"), content).unwrap();
        let records = load_all_provenance_records(dir.path());
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].task_id, "impl-feat");
    }

    #[test]
    fn load_all_provenance_records_skips_non_json() {
        use tempfile::tempdir;
        let dir = tempdir().unwrap();
        let prov_dir = dir.path().join(".wreck-it-provenance");
        std::fs::create_dir_all(&prov_dir).unwrap();
        std::fs::write(prov_dir.join("README.txt"), "not json").unwrap();
        let records = load_all_provenance_records(dir.path());
        assert!(records.is_empty());
    }
}
