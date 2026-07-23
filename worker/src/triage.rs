//! CI-failure triage: turn failing `workflow_run` webhook events into
//! triage items and dispatch cloud coding agents to fix them.
//!
//! Honors the spec-001 "delegate, don't embed" principle: the worker never
//! diagnoses failures with an in-process LLM.  It collects evidence (failed
//! jobs, steps, and log tails), records a [`TriageItem`], and hands the fix
//! to a cloud coding agent through the existing issue-creation + agent-
//! assignment machinery.
//!
//! Issues created here carry the [`TRIAGE_ISSUE_LABEL`] label — deliberately
//! **not** the `wreck-it` label, which would trigger a full ralph iteration
//! when the issue-opened webhook loops back to this worker.

use crate::github::GitHubClient;
use crate::kv_store;
use crate::types::WorkflowRunPayload;
use worker::{console_log, console_warn};
use wreck_it_core::config::TriageConfig;
use wreck_it_core::triage::{
    resolve_ci_items_for_success, resolve_items_for_pr, truncate_detail, upsert_item, TriageItem,
    TriageSource, TriageStatus, TriageUpsert, MAX_DETAIL_BYTES,
};

/// Label applied to triage-dispatched fix issues.
///
/// Must stay distinct from `"wreck-it"` — see the module docs.
pub const TRIAGE_ISSUE_LABEL: &str = "wreck-it-triage";

/// Maximum number of failed jobs whose logs are embedded as evidence.
const MAX_EVIDENCE_JOBS: usize = 3;

/// Log tail captured per failed job.
const PER_JOB_LOG_BYTES: usize = 4 * 1024;

/// What to do with a `workflow_run` webhook event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkflowRunDisposition {
    /// A failing run on a triaged branch — create/update a triage item.
    Triage,
    /// A successful run on a triaged branch — resolve matching open items.
    ResolveOnSuccess,
    /// Not relevant to triage.
    Ignore,
}

/// Classify a `workflow_run` event.
///
/// Only `completed` runs on a triaged branch are considered; the state
/// branch is always excluded (see [`TriageConfig::triages_branch`]).
pub fn workflow_run_disposition(
    action: &str,
    run: &WorkflowRunPayload,
    config: &TriageConfig,
    default_branch: &str,
    state_branch: &str,
) -> WorkflowRunDisposition {
    if !config.enabled || action != "completed" {
        return WorkflowRunDisposition::Ignore;
    }
    let branch = match run.head_branch.as_deref() {
        Some(b) => b,
        None => return WorkflowRunDisposition::Ignore,
    };
    if !config.triages_branch(branch, default_branch, state_branch) {
        return WorkflowRunDisposition::Ignore;
    }
    match run.conclusion.as_deref() {
        Some("failure") | Some("timed_out") => WorkflowRunDisposition::Triage,
        Some("success") => WorkflowRunDisposition::ResolveOnSuccess,
        _ => WorkflowRunDisposition::Ignore,
    }
}

/// Handle a classified `workflow_run` event end-to-end.
///
/// Returns a short human-readable summary for the webhook response.
#[allow(clippy::too_many_arguments)]
pub async fn handle_workflow_run(
    client: &GitHubClient,
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    config: &TriageConfig,
    run: &WorkflowRunPayload,
    disposition: WorkflowRunDisposition,
    now: u64,
) -> Result<String, String> {
    let workflow_name = run.name.as_deref().unwrap_or("(unnamed workflow)");
    let branch = run.head_branch.as_deref().unwrap_or_default();

    match disposition {
        WorkflowRunDisposition::Ignore => Ok("workflow run ignored".to_string()),
        WorkflowRunDisposition::ResolveOnSuccess => {
            let mut items = kv_store::load_triage(kv, owner, repo).await?;
            let resolved = resolve_ci_items_for_success(&mut items, workflow_name, branch, now);
            if resolved > 0 {
                kv_store::save_triage(kv, owner, repo, &items).await?;
            }
            Ok(format!(
                "success run: resolved {resolved} triage item(s) for '{workflow_name}' on {branch}"
            ))
        }
        WorkflowRunDisposition::Triage => {
            let mut items = kv_store::load_triage(kv, owner, repo).await?;
            let candidate = TriageItem::new(
                workflow_run_source(run),
                format!("CI failure: {workflow_name} on {branch}"),
                None,
                now,
            );
            let candidate_id = candidate.id.clone();
            let cap = config.effective_max_items();

            match upsert_item(&mut items, candidate, cap) {
                TriageUpsert::UpdatedExisting { id } => {
                    // Repeat failure of an open item: the occurrence counter
                    // was bumped; the existing issue (if any) stays in play.
                    kv_store::save_triage(kv, owner, repo, &items).await?;
                    console_log!(
                        "[wreck-it][triage] repeat failure absorbed by {id} \
                         ('{workflow_name}' on {branch})",
                    );
                    Ok(format!("repeat failure recorded on triage item {id}"))
                }
                TriageUpsert::Created => {
                    let evidence = collect_evidence(client, run).await;
                    let item = items
                        .iter_mut()
                        .find(|i| i.id == candidate_id)
                        .ok_or_else(|| "freshly upserted triage item vanished".to_string())?;
                    item.detail = Some(truncate_detail(evidence.clone()));

                    let mut summary = format!("created triage item {candidate_id}");
                    if config.auto_dispatch {
                        match dispatch_fix_issue(client, item, &evidence).await {
                            Ok(issue_number) => {
                                summary = format!(
                                    "created triage item {candidate_id}, \
                                     dispatched fix issue #{issue_number}"
                                );
                            }
                            Err(e) => {
                                // Keep the item in `New`; the portal can
                                // retry dispatch later.
                                console_warn!("[wreck-it][triage] dispatch failed: {e}");
                                summary =
                                    format!("created triage item {candidate_id} (dispatch failed)");
                            }
                        }
                    }
                    kv_store::save_triage(kv, owner, repo, &items).await?;
                    Ok(summary)
                }
            }
        }
    }
}

/// Build the [`TriageSource`] for a workflow run payload.
fn workflow_run_source(run: &WorkflowRunPayload) -> TriageSource {
    TriageSource::CiFailure {
        run_id: run.id,
        workflow_name: run
            .name
            .clone()
            .unwrap_or_else(|| "(unnamed workflow)".to_string()),
        branch: run.head_branch.clone().unwrap_or_default(),
        head_sha: run.head_sha.clone(),
        conclusion: run
            .conclusion
            .clone()
            .unwrap_or_else(|| "failure".to_string()),
        run_url: run.html_url.clone(),
        run_attempt: run.run_attempt.unwrap_or(1),
    }
}

/// Create the fix issue and assign a coding agent for a new triage item.
///
/// On success the item transitions to `Investigating` with the issue number
/// recorded.  Assignment failure still counts as dispatched (the issue
/// exists and can be picked up manually).
async fn dispatch_fix_issue(
    client: &GitHubClient,
    item: &mut TriageItem,
    evidence: &str,
) -> Result<u64, String> {
    let (workflow_name, branch) = match &item.source {
        TriageSource::CiFailure {
            workflow_name,
            branch,
            ..
        } => (workflow_name.clone(), branch.clone()),
        other => return Err(format!("not a CI-failure item: {other:?}")),
    };

    let title = format!("[wreck-it] Fix failing workflow '{workflow_name}' on {branch}");
    let body = build_issue_body(item, evidence);

    let (issue_number, node_id) = client
        .create_issue(&title, &body, &[TRIAGE_ISSUE_LABEL])
        .await?;

    if !client.assign_agent(issue_number, node_id.as_deref()).await {
        console_warn!(
            "[wreck-it][triage] could not assign a coding agent to issue #{issue_number}",
        );
    }

    item.status = TriageStatus::Investigating;
    item.issue_number = Some(issue_number);
    Ok(issue_number)
}

/// Build the body of a dispatched fix issue.
///
/// Includes a summary table, the evidence excerpt, agent instructions, and a
/// stable correlation-key marker comment for future cross-referencing.
pub fn build_issue_body(item: &TriageItem, evidence: &str) -> String {
    let (run_url, head_sha, conclusion, run_attempt) = match &item.source {
        TriageSource::CiFailure {
            run_url,
            head_sha,
            conclusion,
            run_attempt,
            ..
        } => (
            run_url.clone().unwrap_or_else(|| "(unknown)".to_string()),
            head_sha.clone(),
            conclusion.clone(),
            *run_attempt,
        ),
        _ => (String::new(), String::new(), String::new(), 1),
    };

    format!(
        "A CI workflow run has failed and needs to be fixed.\n\n\
         | | |\n\
         |---|---|\n\
         | **Run** | {run_url} |\n\
         | **Commit** | `{head_sha}` |\n\
         | **Conclusion** | {conclusion} |\n\
         | **Attempt** | {run_attempt} |\n\
         | **Occurrences** | {occurrences} |\n\n\
         ## Evidence\n\n\
         {evidence}\n\n\
         ## Instructions\n\n\
         Investigate the failure using the evidence above, fix the root \
         cause, and open a pull request that references this issue \
         (e.g. `Fixes #<this issue>`). If the failure looks flaky rather \
         than a real defect, fix the source of the flakiness.\n\n\
         <!-- wreck-it-triage:{correlation_key} -->\n",
        occurrences = item.occurrences,
        correlation_key = item.correlation_key,
    )
}

/// Collect evidence for a failing run: failed job names, failed steps, and
/// per-job log tails.  Degrades gracefully — any fetch failure yields a
/// summary-only section rather than an error.
async fn collect_evidence(client: &GitHubClient, run: &WorkflowRunPayload) -> String {
    let jobs = match client.list_run_jobs(run.id).await {
        Ok(jobs) => jobs,
        Err(e) => {
            console_warn!("[wreck-it][triage] failed to list run jobs: {e}");
            return format!(
                "(could not fetch job details for run {}; see the run link above)",
                run.id
            );
        }
    };

    let failed: Vec<_> = jobs
        .iter()
        .filter(|j| matches!(j.conclusion.as_deref(), Some("failure") | Some("timed_out")))
        .collect();

    if failed.is_empty() {
        return "(no failed jobs reported; the run-level conclusion was failing)".to_string();
    }

    let mut sections = Vec::new();
    for job in failed.iter().take(MAX_EVIDENCE_JOBS) {
        let failed_steps: Vec<&str> = job
            .steps
            .iter()
            .filter(|s| matches!(s.conclusion.as_deref(), Some("failure") | Some("timed_out")))
            .map(|s| s.name.as_str())
            .collect();

        let mut section = format!(
            "### Job `{}` — {}\n\nFailed steps: {}\n",
            job.name,
            job.conclusion.as_deref().unwrap_or("unknown"),
            if failed_steps.is_empty() {
                "(none reported)".to_string()
            } else {
                failed_steps.join(", ")
            },
        );

        match client.get_job_log_tail(job.id, PER_JOB_LOG_BYTES).await {
            Ok(log) => {
                let cleaned = strip_ansi(&log);
                section.push_str(&format!("\n```text\n{}\n```\n", cleaned.trim_end()));
            }
            Err(e) => {
                console_warn!(
                    "[wreck-it][triage] log fetch failed for job {}: {e}",
                    job.id,
                );
                section.push_str("\n(log unavailable)\n");
            }
        }
        sections.push(section);
    }

    if failed.len() > MAX_EVIDENCE_JOBS {
        sections.push(format!(
            "({} additional failed job(s) omitted)",
            failed.len() - MAX_EVIDENCE_JOBS
        ));
    }

    let evidence = sections.join("\n");
    // Bound the total evidence to the same cap used for stored detail.
    truncate_detail(evidence)
}

/// Resolve open triage items linked to a merged PR.
///
/// Returns the number of items resolved.  Best-effort caller side: a KV
/// failure here must not fail the surrounding webhook handling.
pub async fn handle_merged_pr(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    pr_number: u64,
    now: u64,
) -> Result<usize, String> {
    let mut items = kv_store::load_triage(kv, owner, repo).await?;
    let resolved = resolve_items_for_pr(&mut items, pr_number, now);
    if resolved > 0 {
        kv_store::save_triage(kv, owner, repo, &items).await?;
    }
    Ok(resolved)
}

/// Link a trusted PR to the triage items whose fix issues it references.
///
/// Scans the PR body for `#N` issue references (coding agents reliably
/// write `Fixes #N` for assigned issues) and transitions matching
/// `Investigating` items to `PrOpen`.  Returns the number of items linked.
pub async fn handle_pr_linkage(
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    pr_number: u64,
    pr_body: Option<&str>,
    now: u64,
) -> Result<usize, String> {
    let refs = match pr_body {
        Some(body) => extract_issue_refs(body),
        None => return Ok(0),
    };
    if refs.is_empty() {
        return Ok(0);
    }
    let mut items = kv_store::load_triage(kv, owner, repo).await?;
    let linked = link_pr_to_items(&mut items, pr_number, &refs, now);
    if linked > 0 {
        kv_store::save_triage(kv, owner, repo, &items).await?;
    }
    Ok(linked)
}

/// Extract `#123`-style issue references from text (deduplicated, in order).
pub fn extract_issue_refs(text: &str) -> Vec<u64> {
    let mut refs = Vec::new();
    let bytes = text.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'#' {
            let start = i + 1;
            let mut end = start;
            while end < bytes.len() && bytes[end].is_ascii_digit() {
                end += 1;
            }
            if end > start {
                if let Ok(n) = text[start..end].parse::<u64>() {
                    if !refs.contains(&n) {
                        refs.push(n);
                    }
                }
            }
            i = end;
        } else {
            i += 1;
        }
    }
    refs
}

/// Transition `Investigating` items whose fix issue appears in `issue_refs`
/// to `PrOpen`, recording the PR number.  Returns the number linked.
pub fn link_pr_to_items(
    items: &mut [TriageItem],
    pr_number: u64,
    issue_refs: &[u64],
    now: u64,
) -> usize {
    let mut linked = 0;
    for item in items.iter_mut() {
        if item.status == TriageStatus::Investigating {
            if let Some(issue) = item.issue_number {
                if issue_refs.contains(&issue) {
                    item.status = TriageStatus::PrOpen;
                    item.pr_number = Some(pr_number);
                    item.updated_at = now;
                    linked += 1;
                }
            }
        }
    }
    linked
}

/// Strip ANSI escape sequences (CSI and simple two-byte escapes) from `s`.
///
/// Job logs are frequently colorized; the sequences are noise in issue
/// bodies and stored evidence.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        if c != '\u{1b}' {
            out.push(c);
            continue;
        }
        match chars.peek() {
            // CSI sequence: ESC [ ... final byte in @..~
            Some('[') => {
                chars.next();
                for c2 in chars.by_ref() {
                    if ('\u{40}'..='\u{7e}').contains(&c2) {
                        break;
                    }
                }
            }
            // Two-byte escape (ESC c, ESC M, ...): drop the next char.
            Some(_) => {
                chars.next();
            }
            None => {}
        }
    }
    out
}

/// Ensure evidence-cap constants stay consistent with the core detail cap.
const _: () = assert!(MAX_EVIDENCE_JOBS * PER_JOB_LOG_BYTES <= MAX_DETAIL_BYTES * 2);

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::User;

    fn run(conclusion: Option<&str>, branch: Option<&str>) -> WorkflowRunPayload {
        WorkflowRunPayload {
            id: 123,
            name: Some("CI".to_string()),
            head_branch: branch.map(|b| b.to_string()),
            head_sha: "abc123".to_string(),
            status: Some("completed".to_string()),
            conclusion: conclusion.map(|c| c.to_string()),
            html_url: Some("https://github.com/o/r/actions/runs/123".to_string()),
            run_attempt: Some(1),
            event: Some("push".to_string()),
            actor: Some(User {
                login: "octocat".to_string(),
                user_type: Some("User".to_string()),
            }),
            pull_requests: Vec::new(),
        }
    }

    fn enabled_config() -> TriageConfig {
        TriageConfig {
            enabled: true,
            ..TriageConfig::default()
        }
    }

    #[test]
    fn disposition_failure_on_default_branch_triages() {
        let d = workflow_run_disposition(
            "completed",
            &run(Some("failure"), Some("main")),
            &enabled_config(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Triage);
    }

    #[test]
    fn disposition_timed_out_triages() {
        let d = workflow_run_disposition(
            "completed",
            &run(Some("timed_out"), Some("main")),
            &enabled_config(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Triage);
    }

    #[test]
    fn disposition_success_resolves() {
        let d = workflow_run_disposition(
            "completed",
            &run(Some("success"), Some("main")),
            &enabled_config(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::ResolveOnSuccess);
    }

    #[test]
    fn disposition_ignores_wrong_action() {
        let d = workflow_run_disposition(
            "requested",
            &run(Some("failure"), Some("main")),
            &enabled_config(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Ignore);
    }

    #[test]
    fn disposition_ignores_cancelled() {
        let d = workflow_run_disposition(
            "completed",
            &run(Some("cancelled"), Some("main")),
            &enabled_config(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Ignore);
    }

    #[test]
    fn disposition_ignores_non_default_branch() {
        let d = workflow_run_disposition(
            "completed",
            &run(Some("failure"), Some("feature/x")),
            &enabled_config(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Ignore);
    }

    #[test]
    fn disposition_ignores_state_branch_even_if_listed() {
        let config = TriageConfig {
            enabled: true,
            branches: vec!["wreck-it-state".to_string()],
            ..TriageConfig::default()
        };
        let d = workflow_run_disposition(
            "completed",
            &run(Some("failure"), Some("wreck-it-state")),
            &config,
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Ignore);
    }

    #[test]
    fn disposition_ignores_when_disabled() {
        let d = workflow_run_disposition(
            "completed",
            &run(Some("failure"), Some("main")),
            &TriageConfig::default(),
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Ignore);
    }

    #[test]
    fn disposition_respects_explicit_branch_list() {
        let config = TriageConfig {
            enabled: true,
            branches: vec!["release".to_string()],
            ..TriageConfig::default()
        };
        let d = workflow_run_disposition(
            "completed",
            &run(Some("failure"), Some("release")),
            &config,
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Triage);
        // Default branch not in the explicit list → ignored.
        let d = workflow_run_disposition(
            "completed",
            &run(Some("failure"), Some("main")),
            &config,
            "main",
            "wreck-it-state",
        );
        assert_eq!(d, WorkflowRunDisposition::Ignore);
    }

    #[test]
    fn issue_body_contains_marker_and_evidence() {
        let item = TriageItem::new(
            workflow_run_source(&run(Some("failure"), Some("main"))),
            "CI failure: CI on main".to_string(),
            None,
            1000,
        );
        let body = build_issue_body(&item, "```text\nerror: boom\n```");
        assert!(body.contains("<!-- wreck-it-triage:ci:CI:main -->"));
        assert!(body.contains("error: boom"));
        assert!(body.contains("https://github.com/o/r/actions/runs/123"));
        assert!(body.contains("`abc123`"));
        // The label constant must never be the ralph-iteration label.
        assert_ne!(TRIAGE_ISSUE_LABEL, "wreck-it");
    }

    #[test]
    fn strip_ansi_removes_color_codes() {
        let colored = "\u{1b}[31merror\u{1b}[0m: boom \u{1b}[1;32mok\u{1b}[0m";
        assert_eq!(strip_ansi(colored), "error: boom ok");
    }

    #[test]
    fn strip_ansi_passes_plain_text() {
        assert_eq!(strip_ansi("plain text\nline 2"), "plain text\nline 2");
    }

    #[test]
    fn strip_ansi_handles_trailing_escape() {
        assert_eq!(strip_ansi("abc\u{1b}"), "abc");
        assert_eq!(strip_ansi("abc\u{1b}[31"), "abc");
    }

    #[test]
    fn extract_issue_refs_finds_and_dedupes() {
        assert_eq!(
            extract_issue_refs("Fixes #12 and closes #7, also #12 again"),
            vec![12, 7]
        );
        assert_eq!(
            extract_issue_refs("no refs here # or #x"),
            Vec::<u64>::new()
        );
        assert_eq!(extract_issue_refs(""), Vec::<u64>::new());
    }

    #[test]
    fn link_pr_transitions_only_investigating_items() {
        let mut item = TriageItem::new(
            workflow_run_source(&run(Some("failure"), Some("main"))),
            "t".to_string(),
            None,
            1000,
        );
        item.status = TriageStatus::Investigating;
        item.issue_number = Some(55);

        let mut other = TriageItem::new(
            TriageSource::LogEvent {
                provider: "seq".to_string(),
                event_id: "9".to_string(),
            },
            "other".to_string(),
            None,
            1000,
        );
        other.issue_number = Some(56); // still New — must not link

        let mut items = vec![item, other];
        let linked = link_pr_to_items(&mut items, 88, &[55, 56], 2000);
        assert_eq!(linked, 1);
        assert_eq!(items[0].status, TriageStatus::PrOpen);
        assert_eq!(items[0].pr_number, Some(88));
        assert_eq!(items[0].updated_at, 2000);
        assert_eq!(items[1].status, TriageStatus::New);
        assert_eq!(items[1].pr_number, None);
    }

    #[test]
    fn link_pr_ignores_unreferenced_issues() {
        let mut item = TriageItem::new(
            workflow_run_source(&run(Some("failure"), Some("main"))),
            "t".to_string(),
            None,
            1000,
        );
        item.status = TriageStatus::Investigating;
        item.issue_number = Some(55);
        let mut items = vec![item];
        assert_eq!(link_pr_to_items(&mut items, 88, &[99], 2000), 0);
        assert_eq!(items[0].status, TriageStatus::Investigating);
    }

    #[test]
    fn workflow_run_source_maps_fields() {
        let source = workflow_run_source(&run(Some("failure"), Some("main")));
        match source {
            TriageSource::CiFailure {
                run_id,
                workflow_name,
                branch,
                conclusion,
                run_attempt,
                ..
            } => {
                assert_eq!(run_id, 123);
                assert_eq!(workflow_name, "CI");
                assert_eq!(branch, "main");
                assert_eq!(conclusion, "failure");
                assert_eq!(run_attempt, 1);
            }
            other => panic!("unexpected source: {other:?}"),
        }
    }
}
