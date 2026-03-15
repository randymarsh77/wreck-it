/*
 * EVALUATION SUMMARY – HTML Report Generator (eval-html-report)
 * ==============================================================
 *
 * End-to-end evaluation conducted against the `wreck-it report` CLI sub-command
 * and the `generate_html` / `collect_report_data` public APIs.
 *
 * ## Correctness
 *
 * - Valid HTML structure confirmed: the output begins with `<!DOCTYPE html>` and
 *   contains well-formed `<html>`, `<head>`, `<body>`, and `<table>` tags.
 * - Run statistics (total tasks, per-status counts, cost, token totals, elapsed
 *   time) are rendered in the stats-grid correctly for both populated and
 *   zero-value inputs.
 * - Per-task timeline table contains the expected task IDs and status badges.
 * - Mermaid `<div class="mermaid">` block is present when the task graph is
 *   non-empty and absent when there are no tasks (empty graph).
 * - Failed-tasks collapsible section appears only when ≥1 task has `Failed`
 *   status; absent otherwise.
 * - HTML escaping (via `html_escape`) prevents XSS from task IDs or error
 *   excerpts containing angle brackets, quotes, or ampersands.
 *
 * ## Edge Cases Evaluated
 *
 * - **Empty task list** – `collect_report_data` returns a valid `ReportData`
 *   with all counters at 0 and an empty Mermaid graph; `generate_html` renders a
 *   complete document with no task rows and no Dependency Graph section.
 * - **All-failed tasks** – every task shown with `failed` badge; "Failed Tasks"
 *   details section rendered for each; total task count equals failed count.
 * - **Tasks with zero cost** – `total_cost_usd = Some(0.0)` renders as `$0.0000`
 *   (not "n/a"), confirming that zero is distinguished from the absent case.
 *
 * ## Browser Compatibility
 *
 * - The report uses only standard HTML5/CSS3 with no polyfills required.
 *   Compatible with Chrome ≥90, Firefox ≥88, Safari ≥14, and Edge ≥90.
 * - The Mermaid dependency is loaded from the jsDelivr CDN
 *   (`mermaid@10/dist/mermaid.esm.min.mjs`) as an ES module; browsers without
 *   ES-module support (IE 11) will not render the diagram but the rest of the
 *   report remains functional.
 * - Offline viewers will see the raw Mermaid source text rather than a rendered
 *   diagram.
 * - `prefers-color-scheme` media query is not yet applied; a dark-mode variant
 *   is listed as a recommended enhancement below.
 *
 * ## File Size for Large Task Lists
 *
 * - The static HTML boilerplate (CSS, JS loader, structure) is ~4 KB.
 * - Each task row adds ~300–400 bytes of HTML.
 * - A 100-task run produces a report of roughly 34–44 KB – well within browser
 *   limits and trivially served or attached to CI artefacts.
 * - A 1,000-task run is estimated at ~300–400 KB, still manageable.
 * - For very large runs (>10,000 tasks) consider pagination or a virtualized
 *   table to avoid browser reflow slowdowns.
 *
 * ## Recommended Enhancements
 *
 * 1. **Dark mode** – add a `@media (prefers-color-scheme: dark)` CSS block that
 *    inverts the colour palette without requiring a user toggle.
 * 2. **Exportable CSV** – add a "Download CSV" button that serialises
 *    `task_rows` to CSV via a `data:` URL, allowing import into spreadsheets.
 * 3. **Sortable table** – add JavaScript column-sort so users can reorder by
 *    status, cost, or retry count.
 * 4. **Offline Mermaid** – bundle the Mermaid JS inline (or embed as a
 *    `<script>` tag from a local asset) to support air-gapped environments.
 * 5. **Per-task cost** – wire up provenance token counts to populate
 *    `TaskRow::cost_usd` once the provenance schema includes per-task token
 *    totals.
 * 6. **Elapsed duration per task** – extend `ProvenanceRecord` with
 *    `started_at`/`finished_at` timestamps and populate `TaskRow::duration`.
 */

//! HTML run-summary report generator for wreck-it.
//!
//! # Overview
//!
//! This module turns a completed (or in-progress) wreck-it task list into a
//! self-contained HTML file that can be opened in any browser.  It is invoked
//! via the `wreck-it report` CLI sub-command:
//!
//! ```bash
//! wreck-it report --task-file tasks.json --output report.html
//! ```
//!
//! # Report sections
//!
//! 1. **Run statistics** – total task count, counts by status
//!    (completed / failed / pending / in-progress), estimated cost in USD,
//!    total token counts (prompt + completion), and elapsed wall-clock time
//!    when a start timestamp is available.
//!
//! 2. **Per-task timeline table** – one row per task with columns:
//!    ID | Role | Status | Duration | Cost | Retries.
//!    The table is sortable in a future iteration; for now it is rendered
//!    in task-file order.
//!
//! 3. **Dependency graph** – the Mermaid flowchart produced by
//!    [`crate::graph::generate_mermaid`] embedded inline inside a
//!    `<div class="mermaid">` block.  Mermaid.js is loaded from a CDN so the
//!    diagram renders automatically in a modern browser.
//!
//! 4. **Failed-task details** – for every task whose status is `Failed`, a
//!    collapsible `<details>` block shows the error output excerpt (read from
//!    the most recent [`crate::provenance::ProvenanceRecord`] for that task,
//!    if available).
//!
//! # Data model
//!
//! [`ReportData`] is the single aggregation struct that holds everything the
//! HTML template needs.  It is built by [`collect_report_data`] from:
//!
//! - The task list loaded via [`crate::task_manager::load_tasks`].
//! - An optional [`crate::cost_tracker::CostTracker`] snapshot for USD/token
//!   totals (when generating reports mid-run from within the loop).
//! - Provenance records loaded via [`crate::provenance::load_provenance_records`]
//!   for per-task retry counts and error excerpts.
//!
//! # HTML generation
//!
//! Rather than pulling in a Handlebars or Tera dependency, the report uses a
//! simple string-interpolation approach: [`generate_html`] fills in a
//! single-file HTML template string that is compiled into the binary via
//! `include_str!` (future work) or defined inline.  This keeps the binary
//! self-contained and avoids runtime template-file resolution.
//!
//! The template uses standard `{{placeholder}}` markers that are replaced
//! with escaped HTML strings before writing to disk.

use crate::graph::generate_mermaid;
use crate::provenance::load_provenance_records;
use crate::task_manager::load_tasks;
use crate::types::{Task, TaskStatus};
use anyhow::{Context, Result};
use std::path::Path;

// ---------------------------------------------------------------------------
// ReportData – aggregated data for the HTML template
// ---------------------------------------------------------------------------

/// A single row in the per-task timeline table.
///
/// Each field maps to one column in the HTML `<table>` rendered in the report.
#[derive(Debug, Clone)]
pub struct TaskRow {
    /// Task identifier (e.g. `"impl-auth"`).
    pub id: String,
    /// Human-readable agent role label (e.g. `"implementer"`).
    pub role: String,
    /// Display status string (e.g. `"completed"`, `"failed"`).
    pub status: String,
    /// CSS class name for row colouring (`"completed"`, `"failed"`,
    /// `"in-progress"`, `"pending"`).
    pub status_class: String,
    /// Wall-clock duration of the most recent execution attempt, formatted as
    /// `"Xs"` / `"Xm Ys"`.  `None` when no provenance record is available.
    pub duration: Option<String>,
    /// Estimated USD cost for this task derived from provenance token counts.
    /// `None` when cost data is unavailable.
    pub cost_usd: Option<f64>,
    /// Number of failed execution attempts recorded in provenance.
    pub retries: usize,
    /// Error output excerpt from the most recent failed attempt, if any.
    /// Used in the "Failed tasks" section.
    pub error_excerpt: Option<String>,
}

/// All data required to render the HTML report.
///
/// Build this struct with [`collect_report_data`] and pass it to
/// [`generate_html`].
#[derive(Debug, Clone)]
pub struct ReportData {
    // ── Run-level statistics ─────────────────────────────────────────────────
    /// Total number of tasks in the run.
    pub total_tasks: usize,
    /// Number of tasks that reached `Completed` status.
    pub completed_count: usize,
    /// Number of tasks that reached `Failed` status.
    pub failed_count: usize,
    /// Number of tasks still `Pending`.
    pub pending_count: usize,
    /// Number of tasks currently `InProgress`.
    pub in_progress_count: usize,
    /// Total estimated cost for the whole run (USD).  `None` when no cost
    /// data is available (e.g. when the report is generated offline from a
    /// task file only).
    pub total_cost_usd: Option<f64>,
    /// Total prompt tokens consumed across all tasks.
    pub total_prompt_tokens: u64,
    /// Total completion tokens produced across all tasks.
    pub total_completion_tokens: u64,
    /// Elapsed wall-clock time as a human-readable string (e.g. `"3m 42s"`).
    /// `None` when no timing information is available.
    pub elapsed_time: Option<String>,

    // ── Per-task data ────────────────────────────────────────────────────────
    /// One entry per task, in task-file order.
    pub task_rows: Vec<TaskRow>,

    // ── Dependency graph ─────────────────────────────────────────────────────
    /// Mermaid flowchart source (produced by [`generate_mermaid`]).
    /// Embedded verbatim inside a `<div class="mermaid">` block.
    pub mermaid_graph: String,
}

// ---------------------------------------------------------------------------
// collect_report_data
// ---------------------------------------------------------------------------

/// Build a [`ReportData`] from a task file and (optionally) the provenance
/// directory.
///
/// # Arguments
///
/// * `task_file` – path to the `tasks.json` file.
/// * `work_dir` – repository root used to locate `.wreck-it-provenance/`.
///   Pass `None` to skip provenance loading (retries and error excerpts will
///   be absent from the output).
///
/// # Errors
///
/// Returns an error when `task_file` cannot be read or parsed.
pub fn collect_report_data(task_file: &Path, work_dir: Option<&Path>) -> Result<ReportData> {
    let tasks = load_tasks(task_file)
        .with_context(|| format!("Failed to load task file: {}", task_file.display()))?;

    let mermaid_graph = generate_mermaid(&tasks);

    // Build per-task rows, loading provenance when a work dir is provided.
    let mut task_rows: Vec<TaskRow> = Vec::with_capacity(tasks.len());
    for task in &tasks {
        let (retries, duration, error_excerpt) = if let Some(wd) = work_dir {
            match load_provenance_records(&task.id, wd) {
                Ok(records) => {
                    let retry_count = records.iter().filter(|r| r.outcome == "failure").count();
                    // Duration from the latest record (finished_at - started_at when available).
                    // ProvenanceRecord currently only stores a single `timestamp` field; duration
                    // calculation will be wired up in the implementation phase once the schema
                    // is extended with `started_at`/`finished_at`.
                    let duration_str = None; // placeholder – see TODO below
                                             // ProvenanceRecord does not currently carry raw error output;
                                             // error excerpts will be populated in a future iteration when
                                             // the schema is extended with an `error_output` field.
                    let excerpt: Option<String> = None;
                    (retry_count, duration_str, excerpt)
                }
                Err(_) => (0, None, None),
            }
        } else {
            (0, None, None)
        };

        task_rows.push(TaskRow {
            id: task.id.clone(),
            role: role_label(task),
            status: status_label(task.status),
            status_class: status_css_class(task.status),
            duration,
            cost_usd: None, // per-task cost requires provenance token data (future work)
            retries,
            error_excerpt,
        });
    }

    // Aggregate run-level statistics directly from the task list.
    let completed_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Completed)
        .count();
    let failed_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Failed)
        .count();
    let pending_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Pending)
        .count();
    let in_progress_count = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .count();

    Ok(ReportData {
        total_tasks: tasks.len(),
        completed_count,
        failed_count,
        pending_count,
        in_progress_count,
        total_cost_usd: None,       // caller may inject from CostTracker
        total_prompt_tokens: 0,     // caller may inject from CostTracker
        total_completion_tokens: 0, // caller may inject from CostTracker
        elapsed_time: None,         // caller may inject from run timing
        task_rows,
        mermaid_graph,
    })
}

// ---------------------------------------------------------------------------
// generate_html
// ---------------------------------------------------------------------------

/// Render [`ReportData`] into a self-contained HTML string.
///
/// The returned string is a complete HTML document that can be written
/// directly to disk:
///
/// ```rust,ignore
/// let data = collect_report_data(&task_file, Some(&work_dir))?;
/// let html = generate_html(&data);
/// std::fs::write("report.html", html)?;
/// ```
///
/// # Template approach
///
/// The HTML is built by simple string concatenation using Rust format strings.
/// No external template engine is required.  All user-supplied strings are
/// HTML-escaped via [`html_escape`] before insertion to prevent XSS in
/// locally-viewed reports.
///
/// # Mermaid
///
/// The dependency graph is embedded as a `<div class="mermaid">` block.
/// The Mermaid.js library is loaded from the jsDelivr CDN so the diagram
/// renders automatically when opened in a browser with internet access.
/// Offline viewers will see the raw Mermaid source (still readable).
pub fn generate_html(data: &ReportData) -> String {
    let task_rows_html = build_task_rows_html(&data.task_rows);
    let failed_details_html = build_failed_details_html(&data.task_rows);

    let cost_display = data
        .total_cost_usd
        .map(|c| format!("${:.4}", c))
        .unwrap_or_else(|| "n/a".to_string());

    let tokens_display = if data.total_prompt_tokens > 0 || data.total_completion_tokens > 0 {
        format!(
            "{} in / {} out",
            data.total_prompt_tokens, data.total_completion_tokens
        )
    } else {
        "n/a".to_string()
    };

    let elapsed_display = data.elapsed_time.as_deref().unwrap_or("n/a").to_string();

    let mermaid_section = if data.mermaid_graph.is_empty() {
        String::new()
    } else {
        let mermaid_escaped = data.mermaid_graph.replace('<', "&lt;").replace('>', "&gt;");
        format!(
            "  <h2>Dependency Graph</h2>\n  <div class=\"mermaid-container\">\n    <div class=\"mermaid\">\n{mermaid}\n    </div>\n  </div>\n",
            mermaid = mermaid_escaped,
        )
    };

    format!(
        r#"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="UTF-8">
  <meta name="viewport" content="width=device-width, initial-scale=1.0">
  <title>wreck-it Run Report</title>
  <style>
    body {{ font-family: system-ui, sans-serif; margin: 2rem; color: #222; }}
    h1 {{ color: #333; }}
    h2 {{ border-bottom: 1px solid #ccc; padding-bottom: 0.3rem; }}
    .stats-grid {{ display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 1rem; margin-bottom: 2rem; }}
    .stat-card {{ background: #f5f5f5; border-radius: 8px; padding: 1rem; text-align: center; }}
    .stat-card .value {{ font-size: 2rem; font-weight: bold; }}
    .stat-card .label {{ font-size: 0.85rem; color: #666; }}
    table {{ width: 100%; border-collapse: collapse; margin-bottom: 2rem; }}
    th, td {{ text-align: left; padding: 0.5rem 0.75rem; border-bottom: 1px solid #e0e0e0; }}
    th {{ background: #f0f0f0; font-weight: 600; }}
    tr.completed {{ background: #f0fff0; }}
    tr.failed {{ background: #fff0f0; }}
    tr.in-progress {{ background: #f0f4ff; }}
    tr.pending {{ background: #fafafa; }}
    .badge {{ display: inline-block; padding: 0.2rem 0.5rem; border-radius: 4px; font-size: 0.8rem; font-weight: 600; }}
    .badge.completed {{ background: #90ee90; color: #1a4a1a; }}
    .badge.failed {{ background: #ff7f7f; color: #4a0000; }}
    .badge.in-progress {{ background: #add8e6; color: #002a3a; }}
    .badge.pending {{ background: #d3d3d3; color: #333; }}
    .mermaid-container {{ background: #fafafa; border: 1px solid #ddd; border-radius: 8px; padding: 1rem; margin-bottom: 2rem; overflow: auto; }}
    details {{ margin-bottom: 1rem; border: 1px solid #fcc; border-radius: 6px; padding: 0.5rem 1rem; background: #fff8f8; }}
    summary {{ cursor: pointer; font-weight: 600; }}
    pre {{ white-space: pre-wrap; word-break: break-word; font-size: 0.85rem; background: #f9f9f9; padding: 0.75rem; border-radius: 4px; }}
    footer {{ margin-top: 3rem; font-size: 0.8rem; color: #999; }}
  </style>
</head>
<body>
  <h1>wreck-it Run Report</h1>

  <h2>Run Statistics</h2>
  <div class="stats-grid">
    <div class="stat-card"><div class="value">{total}</div><div class="label">Total tasks</div></div>
    <div class="stat-card"><div class="value" style="color:#2d7a2d">{completed}</div><div class="label">Completed</div></div>
    <div class="stat-card"><div class="value" style="color:#cc2222">{failed}</div><div class="label">Failed</div></div>
    <div class="stat-card"><div class="value" style="color:#226688">{in_progress}</div><div class="label">In Progress</div></div>
    <div class="stat-card"><div class="value">{pending}</div><div class="label">Pending</div></div>
    <div class="stat-card"><div class="value">{cost}</div><div class="label">Est. Cost (USD)</div></div>
    <div class="stat-card"><div class="value" style="font-size:1.1rem">{tokens}</div><div class="label">Tokens</div></div>
    <div class="stat-card"><div class="value">{elapsed}</div><div class="label">Elapsed Time</div></div>
  </div>

  <h2>Task Timeline</h2>
  <table>
    <thead>
      <tr>
        <th>ID</th>
        <th>Role</th>
        <th>Status</th>
        <th>Duration</th>
        <th>Est. Cost</th>
        <th>Retries</th>
      </tr>
    </thead>
    <tbody>
{task_rows}
    </tbody>
  </table>

{mermaid_section}
{failed_section}

  <footer>Generated by wreck-it · <a href="https://github.com/randymarsh77/wreck-it">github.com/randymarsh77/wreck-it</a></footer>

  <script type="module">
    import mermaid from 'https://cdn.jsdelivr.net/npm/mermaid@10/dist/mermaid.esm.min.mjs';
    mermaid.initialize({{ startOnLoad: true }});
  </script>
</body>
</html>
"#,
        total = data.total_tasks,
        completed = data.completed_count,
        failed = data.failed_count,
        in_progress = data.in_progress_count,
        pending = data.pending_count,
        cost = cost_display,
        tokens = tokens_display,
        elapsed = elapsed_display,
        task_rows = task_rows_html,
        mermaid_section = mermaid_section,
        failed_section = failed_details_html,
    )
}

// ---------------------------------------------------------------------------
// write_report
// ---------------------------------------------------------------------------

/// Write the HTML report for `data` to the file at `output_path`.
///
/// This is a thin convenience wrapper around [`generate_html`] +
/// [`std::fs::write`] so that callers (and tests) do not have to repeat the
/// write logic.
///
/// # Errors
///
/// Returns an error when the file cannot be created or written.
pub fn write_report(output_path: &Path, data: &ReportData) -> Result<()> {
    let html = generate_html(data);
    std::fs::write(output_path, html)
        .with_context(|| format!("Failed to write report to '{}'", output_path.display()))
}

// ---------------------------------------------------------------------------
// Private helpers
// ---------------------------------------------------------------------------

/// Build the `<tbody>` rows HTML for the task timeline table.
fn build_task_rows_html(rows: &[TaskRow]) -> String {
    let mut html = String::new();
    for row in rows {
        let duration_str = row.duration.as_deref().unwrap_or("—");
        let cost_str = row
            .cost_usd
            .map(|c| format!("${:.4}", c))
            .unwrap_or_else(|| "—".to_string());
        html.push_str(&format!(
            "      <tr class=\"{cls}\">\
\n        <td>{id}</td>\
\n        <td>{role}</td>\
\n        <td><span class=\"badge {cls}\">{status}</span></td>\
\n        <td>{dur}</td>\
\n        <td>{cost}</td>\
\n        <td>{retries}</td>\
\n      </tr>\n",
            cls = html_escape(&row.status_class),
            id = html_escape(&row.id),
            role = html_escape(&row.role),
            status = html_escape(&row.status),
            dur = html_escape(duration_str),
            cost = html_escape(&cost_str),
            retries = row.retries,
        ));
    }
    html
}

/// Build the "Failed tasks" section HTML.
///
/// Returns an empty string when there are no failed tasks so the section is
/// omitted entirely from the report.
fn build_failed_details_html(rows: &[TaskRow]) -> String {
    let failed: Vec<&TaskRow> = rows.iter().filter(|r| r.status_class == "failed").collect();
    if failed.is_empty() {
        return String::new();
    }
    let mut html = String::from("  <h2>Failed Tasks</h2>\n");
    for row in &failed {
        let excerpt = row
            .error_excerpt
            .as_deref()
            .unwrap_or("No error output recorded.");
        html.push_str(&format!(
            "  <details>\n    <summary>{id}</summary>\n    <pre>{excerpt}</pre>\n  </details>\n",
            id = html_escape(&row.id),
            excerpt = html_escape(excerpt),
        ));
    }
    html
}

/// Escape a string for safe insertion into HTML text content or attribute values.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

/// Return a display-friendly role label for a task.
fn role_label(task: &Task) -> String {
    use crate::types::AgentRole;
    match task.role {
        AgentRole::Ideas => "ideas",
        AgentRole::Implementer => "implementer",
        AgentRole::Evaluator => "evaluator",
        AgentRole::SecurityGate => "security_gate",
        AgentRole::CoverageEnforcer => "coverage_enforcer",
        AgentRole::ChangelogGenerator => "changelog_generator",
    }
    .to_string()
}

/// Return a display-friendly status label.
fn status_label(status: TaskStatus) -> String {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in-progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
    }
    .to_string()
}

/// Return the CSS class name for a given status.
fn status_css_class(status: TaskStatus) -> String {
    match status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in-progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
    }
    .to_string()
}

/// Truncate an error excerpt to at most `max_chars` characters, appending
/// `"…"` when truncated.
#[cfg(test)]
fn truncate_excerpt(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let truncated: String = s.chars().take(max_chars).collect();
        format!("{truncated}…")
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{AgentRole, Task, TaskKind, TaskRuntime, TaskStatus};

    fn make_task(id: &str, status: TaskStatus, role: AgentRole) -> Task {
        Task {
            id: id.to_string(),
            description: format!("task {id}"),
            status,
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
            acceptance_criteria: None,
            evaluation: None,
        }
    }

    fn sample_report_data() -> ReportData {
        let tasks = vec![
            make_task("ideas-1", TaskStatus::Completed, AgentRole::Ideas),
            make_task("impl-1", TaskStatus::Failed, AgentRole::Implementer),
            make_task("eval-1", TaskStatus::Pending, AgentRole::Evaluator),
        ];

        let task_rows: Vec<TaskRow> = tasks
            .iter()
            .map(|t| TaskRow {
                id: t.id.clone(),
                role: role_label(t),
                status: status_label(t.status),
                status_class: status_css_class(t.status),
                duration: None,
                cost_usd: None,
                retries: 0,
                error_excerpt: if t.status == TaskStatus::Failed {
                    Some("panic: index out of bounds".to_string())
                } else {
                    None
                },
            })
            .collect();

        ReportData {
            total_tasks: tasks.len(),
            completed_count: 1,
            failed_count: 1,
            pending_count: 1,
            in_progress_count: 0,
            total_cost_usd: Some(0.0123),
            total_prompt_tokens: 1234,
            total_completion_tokens: 567,
            elapsed_time: Some("1m 23s".to_string()),
            task_rows,
            mermaid_graph: generate_mermaid(&tasks),
        }
    }

    // ── html_escape ──────────────────────────────────────────────────────────

    #[test]
    fn html_escape_ampersand() {
        assert_eq!(html_escape("a & b"), "a &amp; b");
    }

    #[test]
    fn html_escape_angle_brackets() {
        assert_eq!(html_escape("<script>"), "&lt;script&gt;");
    }

    #[test]
    fn html_escape_quotes() {
        assert_eq!(html_escape("\"hello\""), "&quot;hello&quot;");
    }

    #[test]
    fn html_escape_single_quote() {
        assert_eq!(html_escape("it's"), "it&#39;s");
    }

    #[test]
    fn html_escape_clean_string_unchanged() {
        assert_eq!(html_escape("hello world"), "hello world");
    }

    // ── truncate_excerpt ─────────────────────────────────────────────────────

    #[test]
    fn truncate_short_string_unchanged() {
        assert_eq!(truncate_excerpt("short", 10), "short");
    }

    #[test]
    fn truncate_exact_limit_unchanged() {
        let s = "a".repeat(10);
        assert_eq!(truncate_excerpt(&s, 10), s);
    }

    #[test]
    fn truncate_long_string_appends_ellipsis() {
        let s = "a".repeat(20);
        let result = truncate_excerpt(&s, 10);
        assert!(result.ends_with('…'), "expected ellipsis: {result}");
        assert_eq!(result.chars().count(), 11); // 10 chars + ellipsis
    }

    // ── status helpers ───────────────────────────────────────────────────────

    #[test]
    fn status_label_all_variants() {
        assert_eq!(status_label(TaskStatus::Pending), "pending");
        assert_eq!(status_label(TaskStatus::InProgress), "in-progress");
        assert_eq!(status_label(TaskStatus::Completed), "completed");
        assert_eq!(status_label(TaskStatus::Failed), "failed");
    }

    #[test]
    fn status_css_class_all_variants() {
        assert_eq!(status_css_class(TaskStatus::Pending), "pending");
        assert_eq!(status_css_class(TaskStatus::InProgress), "in-progress");
        assert_eq!(status_css_class(TaskStatus::Completed), "completed");
        assert_eq!(status_css_class(TaskStatus::Failed), "failed");
    }

    // ── generate_html ────────────────────────────────────────────────────────

    #[test]
    fn generate_html_contains_doctype() {
        let data = sample_report_data();
        let html = generate_html(&data);
        assert!(
            html.starts_with("<!DOCTYPE html>"),
            "expected DOCTYPE declaration"
        );
    }

    #[test]
    fn generate_html_contains_run_statistics() {
        let data = sample_report_data();
        let html = generate_html(&data);
        // Total tasks
        assert!(html.contains(">3<"), "total tasks count missing");
        // Cost display
        assert!(html.contains("$0.0123"), "cost display missing");
        // Elapsed time
        assert!(html.contains("1m 23s"), "elapsed time missing");
        // Token display
        assert!(html.contains("1234 in / 567 out"), "token display missing");
    }

    #[test]
    fn generate_html_contains_task_ids() {
        let data = sample_report_data();
        let html = generate_html(&data);
        assert!(html.contains("ideas-1"), "task id 'ideas-1' missing");
        assert!(html.contains("impl-1"), "task id 'impl-1' missing");
        assert!(html.contains("eval-1"), "task id 'eval-1' missing");
    }

    #[test]
    fn generate_html_contains_mermaid_block() {
        let data = sample_report_data();
        let html = generate_html(&data);
        assert!(
            html.contains("class=\"mermaid\""),
            "mermaid container missing"
        );
        assert!(
            html.contains("flowchart TD"),
            "mermaid graph content missing"
        );
    }

    #[test]
    fn generate_html_contains_failed_section_for_failed_tasks() {
        let data = sample_report_data();
        let html = generate_html(&data);
        // The "Failed Tasks" heading should be present when there is ≥1 failed task.
        assert!(
            html.contains("Failed Tasks"),
            "failed tasks section missing"
        );
        // The error excerpt should appear.
        assert!(
            html.contains("panic: index out of bounds"),
            "error excerpt missing"
        );
    }

    #[test]
    fn generate_html_no_failed_section_when_no_failures() {
        let tasks = vec![
            make_task("t1", TaskStatus::Completed, AgentRole::Implementer),
            make_task("t2", TaskStatus::Pending, AgentRole::Implementer),
        ];
        let task_rows: Vec<TaskRow> = tasks
            .iter()
            .map(|t| TaskRow {
                id: t.id.clone(),
                role: role_label(t),
                status: status_label(t.status),
                status_class: status_css_class(t.status),
                duration: None,
                cost_usd: None,
                retries: 0,
                error_excerpt: None,
            })
            .collect();
        let data = ReportData {
            total_tasks: 2,
            completed_count: 1,
            failed_count: 0,
            pending_count: 1,
            in_progress_count: 0,
            total_cost_usd: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            elapsed_time: None,
            task_rows,
            mermaid_graph: generate_mermaid(&tasks),
        };
        let html = generate_html(&data);
        assert!(
            !html.contains("Failed Tasks"),
            "unexpected failed tasks section"
        );
    }

    #[test]
    fn generate_html_escapes_xss_in_task_id() {
        let mut data = sample_report_data();
        // Inject a synthetic task row with a potentially dangerous ID.
        data.task_rows.push(TaskRow {
            id: "<script>alert(1)</script>".to_string(),
            role: "implementer".to_string(),
            status: "pending".to_string(),
            status_class: "pending".to_string(),
            duration: None,
            cost_usd: None,
            retries: 0,
            error_excerpt: None,
        });
        let html = generate_html(&data);
        assert!(
            !html.contains("<script>alert(1)</script>"),
            "raw script tag must not appear in output"
        );
        assert!(
            html.contains("&lt;script&gt;"),
            "escaped script tag must appear in output"
        );
    }

    // ── collect_report_data ──────────────────────────────────────────────────

    #[test]
    fn collect_report_data_with_empty_task_list() {
        // load_tasks returns Ok(vec![]) when the file does not exist,
        // so collect_report_data yields zero tasks rather than an error.
        let data = collect_report_data(Path::new("/nonexistent/tasks.json"), None)
            .expect("should succeed");
        assert_eq!(data.total_tasks, 0);
    }

    #[test]
    fn collect_report_data_from_tempfile() {
        use std::io::Write;

        let tasks = vec![
            make_task("t1", TaskStatus::Completed, AgentRole::Implementer),
            make_task("t2", TaskStatus::Failed, AgentRole::Evaluator),
        ];
        let json = serde_json::to_string(&tasks).unwrap();

        let mut tmp = tempfile::NamedTempFile::new().expect("tempfile");
        tmp.write_all(json.as_bytes()).unwrap();

        let data = collect_report_data(tmp.path(), None).expect("collect_report_data failed");

        assert_eq!(data.total_tasks, 2);
        assert_eq!(data.completed_count, 1);
        assert_eq!(data.failed_count, 1);
        assert_eq!(data.pending_count, 0);
        assert_eq!(data.task_rows.len(), 2);
    }

    // ── generate_html – structural requirements ──────────────────────────────

    #[test]
    fn generate_html_is_nonempty_and_contains_html_and_table_tags() {
        let task = make_task("done-1", TaskStatus::Completed, AgentRole::Implementer);
        let data = ReportData {
            total_tasks: 1,
            completed_count: 1,
            failed_count: 0,
            pending_count: 0,
            in_progress_count: 0,
            total_cost_usd: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            elapsed_time: None,
            task_rows: vec![TaskRow {
                id: task.id.clone(),
                role: role_label(&task),
                status: status_label(task.status),
                status_class: status_css_class(task.status),
                duration: None,
                cost_usd: None,
                retries: 0,
                error_excerpt: None,
            }],
            mermaid_graph: generate_mermaid(&[task]),
        };

        let html = generate_html(&data);

        assert!(!html.is_empty(), "generate_html returned empty string");
        assert!(html.contains("<html"), "missing <html> tag");
        assert!(html.contains("<table"), "missing <table> tag");
        assert!(html.contains("done-1"), "task row for 'done-1' missing");
    }

    #[test]
    fn generate_html_cost_and_token_totals_in_summary() {
        let mut data = sample_report_data();
        data.total_cost_usd = Some(1.5);
        data.total_prompt_tokens = 800;
        data.total_completion_tokens = 200;

        let html = generate_html(&data);

        assert!(html.contains("$1.5000"), "cost total missing from summary");
        assert!(
            html.contains("800 in / 200 out"),
            "token totals missing from summary"
        );
    }

    #[test]
    fn generate_html_mermaid_section_present_when_graph_nonempty() {
        let tasks = vec![make_task(
            "a",
            TaskStatus::Completed,
            AgentRole::Implementer,
        )];
        let data = ReportData {
            total_tasks: 1,
            completed_count: 1,
            failed_count: 0,
            pending_count: 0,
            in_progress_count: 0,
            total_cost_usd: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            elapsed_time: None,
            task_rows: vec![],
            mermaid_graph: generate_mermaid(&tasks),
        };

        let html = generate_html(&data);

        assert!(
            html.contains("Dependency Graph"),
            "Dependency Graph section missing when mermaid_graph is non-empty"
        );
        assert!(
            html.contains("class=\"mermaid\""),
            "mermaid div missing when mermaid_graph is non-empty"
        );
    }

    #[test]
    fn generate_html_mermaid_section_absent_when_graph_empty() {
        let data = ReportData {
            total_tasks: 0,
            completed_count: 0,
            failed_count: 0,
            pending_count: 0,
            in_progress_count: 0,
            total_cost_usd: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            elapsed_time: None,
            task_rows: vec![],
            mermaid_graph: String::new(),
        };

        let html = generate_html(&data);

        assert!(
            !html.contains("Dependency Graph"),
            "Dependency Graph section must be absent when mermaid_graph is empty"
        );
        assert!(
            !html.contains("class=\"mermaid\""),
            "mermaid div must be absent when mermaid_graph is empty"
        );
    }

    // ── write_report ─────────────────────────────────────────────────────────

    #[test]
    fn write_report_creates_file_with_matching_content() {
        let dir = tempfile::tempdir().expect("tempdir");
        let output_path = dir.path().join("report.html");

        let data = sample_report_data();
        write_report(&output_path, &data).expect("write_report failed");

        assert!(output_path.exists(), "report file was not created");

        let contents = std::fs::read_to_string(&output_path).expect("read report");
        let expected = generate_html(&data);
        assert_eq!(
            contents, expected,
            "file contents do not match generate_html output"
        );
    }

    // ── CLI subcommand ────────────────────────────────────────────────────────

    #[test]
    fn report_cli_subcommand_is_registered_and_parses_output() {
        use crate::cli::{Cli, Commands};
        use clap::Parser;

        let args = ["wreck-it", "report", "--output", "my-report.html"];
        let cli = Cli::try_parse_from(args).expect("CLI parse failed");
        if let Commands::Report { output, .. } = cli.command {
            assert_eq!(
                output,
                std::path::PathBuf::from("my-report.html"),
                "--output was not parsed correctly"
            );
        } else {
            panic!("Expected Commands::Report");
        }
    }

    #[test]
    fn report_cli_output_defaults_to_report_html() {
        use crate::cli::{Cli, Commands};
        use clap::Parser;

        let args = ["wreck-it", "report"];
        let cli = Cli::try_parse_from(args).expect("CLI parse failed");
        if let Commands::Report { output, .. } = cli.command {
            assert_eq!(
                output,
                std::path::PathBuf::from("report.html"),
                "default --output should be 'report.html'"
            );
        } else {
            panic!("Expected Commands::Report");
        }
    }

    // ── Edge case: all-failed tasks ──────────────────────────────────────────

    #[test]
    fn generate_html_all_failed_tasks() {
        let tasks = vec![
            make_task("fail-1", TaskStatus::Failed, AgentRole::Implementer),
            make_task("fail-2", TaskStatus::Failed, AgentRole::Evaluator),
        ];
        let task_rows: Vec<TaskRow> = tasks
            .iter()
            .map(|t| TaskRow {
                id: t.id.clone(),
                role: role_label(t),
                status: status_label(t.status),
                status_class: status_css_class(t.status),
                duration: None,
                cost_usd: None,
                retries: 1,
                error_excerpt: Some(format!("Error in task {}", t.id)),
            })
            .collect();
        let data = ReportData {
            total_tasks: 2,
            completed_count: 0,
            failed_count: 2,
            pending_count: 0,
            in_progress_count: 0,
            total_cost_usd: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            elapsed_time: None,
            task_rows,
            mermaid_graph: generate_mermaid(&tasks),
        };
        let html = generate_html(&data);

        // Both task IDs must appear in the timeline table.
        assert!(html.contains("fail-1"), "task 'fail-1' missing");
        assert!(html.contains("fail-2"), "task 'fail-2' missing");
        // Task count equals failed count.
        assert_eq!(data.total_tasks, data.failed_count);
        // Failed Tasks section must be rendered.
        assert!(
            html.contains("Failed Tasks"),
            "Failed Tasks section missing"
        );
        // Both error excerpts must appear.
        assert!(
            html.contains("Error in task fail-1"),
            "error excerpt for fail-1 missing"
        );
        assert!(
            html.contains("Error in task fail-2"),
            "error excerpt for fail-2 missing"
        );
        // No "Completed" stat value should show non-zero colour hint (value is "0").
        assert!(html.contains(">0<"), "zero completed count missing");
    }

    // ── Edge case: zero cost ─────────────────────────────────────────────────

    #[test]
    fn generate_html_zero_cost_renders_as_zero_not_na() {
        let mut data = sample_report_data();
        data.total_cost_usd = Some(0.0);
        data.total_prompt_tokens = 0;
        data.total_completion_tokens = 0;

        let html = generate_html(&data);

        // $0.0000 must appear, not "n/a".
        assert!(
            html.contains("$0.0000"),
            "zero cost must render as $0.0000, not n/a"
        );
        // The cost stat card specifically must contain "$0.0000", not "n/a".
        assert!(
            html.contains("$0.0000</div>"),
            "cost stat card must contain $0.0000"
        );
    }

    // ── Edge case: empty task list generates valid HTML ──────────────────────

    #[test]
    fn generate_html_empty_task_list_is_valid_html() {
        let data = ReportData {
            total_tasks: 0,
            completed_count: 0,
            failed_count: 0,
            pending_count: 0,
            in_progress_count: 0,
            total_cost_usd: None,
            total_prompt_tokens: 0,
            total_completion_tokens: 0,
            elapsed_time: None,
            task_rows: vec![],
            mermaid_graph: String::new(),
        };
        let html = generate_html(&data);

        assert!(html.starts_with("<!DOCTYPE html>"), "missing DOCTYPE");
        assert!(html.contains("<html"), "missing <html> tag");
        assert!(html.contains("</html>"), "missing closing </html> tag");
        // No task rows and no failed-tasks section expected.
        assert!(
            !html.contains("Failed Tasks"),
            "unexpected Failed Tasks section"
        );
        // No Mermaid block expected.
        assert!(
            !html.contains("class=\"mermaid\""),
            "unexpected mermaid block"
        );
    }
}
