//! Supply-chain security ingestion: Dependabot alerts and dependency-update
//! pull requests become triage items.
//!
//! Two entry points:
//!
//! - [`run_security_ingest`] — called from the pulse (cron) loop.  Polls the
//!   Dependabot alerts API and reconciles the triage queue against it.  No
//!   execution environment is needed: GitHub already computes the alerts.
//! - [`handle_dep_update_pr`] — called from the webhook path for pull
//!   requests authored by dependency-update bots (Dependabot/Renovate),
//!   which the normal trust filter deliberately ignores.  Observe-only in
//!   v1: a triage item plus one structured comment; **never** workflow
//!   approval or auto-merge, since dependency PRs change third-party code.
//!
//! Both are gated on `[triage].enabled` so non-opted repos pay zero cost.

use crate::github::{DependabotAlert, GitHubClient};
use crate::kv_store;
use crate::types::PullRequest;
use worker::{console_log, console_warn};
use wreck_it_core::config::TriageConfig;
use wreck_it_core::triage::{upsert_item, TriageItem, TriageSeverity, TriageSource, TriageStatus};

/// `tool` value for Dependabot-alert triage items
/// (correlation key `sec:dependabot:{alert_number}`).
pub const DEPENDABOT_TOOL: &str = "dependabot";

/// `tool` value for dependency-update-PR triage items
/// (correlation key `sec:dep-pr:{pr_number}`).
pub const DEP_PR_TOOL: &str = "dep-pr";

/// PR authors treated as dependency-update bots.  Kept in one place — the
/// exact logins differ between hosted and self-hosted variants.
pub const DEP_UPDATE_AUTHORS: &[&str] = &["dependabot[bot]", "renovate[bot]", "renovate-bot"];

/// Whether `login` is a known dependency-update bot.
pub fn is_dep_update_author(login: &str) -> bool {
    DEP_UPDATE_AUTHORS
        .iter()
        .any(|a| a.eq_ignore_ascii_case(login))
}

/// Map an advisory severity string to a [`TriageSeverity`].
pub fn severity_from_advisory(severity: &str) -> TriageSeverity {
    match severity.to_ascii_lowercase().as_str() {
        "critical" => TriageSeverity::Critical,
        "high" => TriageSeverity::High,
        "medium" | "moderate" => TriageSeverity::Medium,
        _ => TriageSeverity::Low,
    }
}

/// Scan free text (a Dependabot PR body) for a severity hint like
/// `"critical severity"` / `"severity: high"`.
pub fn parse_severity_hint(text: &str) -> Option<TriageSeverity> {
    let lower = text.to_ascii_lowercase();
    for (needle, severity) in [
        ("critical", TriageSeverity::Critical),
        ("high", TriageSeverity::High),
        ("moderate", TriageSeverity::Medium),
        ("medium", TriageSeverity::Medium),
        ("low", TriageSeverity::Low),
    ] {
        if lower.contains(&format!("{needle} severity"))
            || lower.contains(&format!("severity: {needle}"))
        {
            return Some(severity);
        }
    }
    None
}

/// Whether the triage queue has ever seen a Dependabot alert for this repo.
///
/// Used for the first-sync flood cap: legacy repositories can carry
/// hundreds of open alerts, so the initial import takes critical/high only.
/// Once any `dependabot` item exists (whatever its state), subsequent syncs
/// ingest every severity.
pub fn is_first_dependabot_sync(items: &[TriageItem]) -> bool {
    !items
        .iter()
        .any(|i| matches!(&i.source, TriageSource::SecurityFinding { tool, .. } if tool == DEPENDABOT_TOOL))
}

/// Select which open alerts to ingest, applying the first-sync cap.
pub fn select_alerts_for_ingest<'a>(
    alerts: &'a [DependabotAlert],
    first_sync: bool,
) -> Vec<&'a DependabotAlert> {
    alerts
        .iter()
        .filter(|a| {
            !first_sync
                || matches!(
                    severity_from_advisory(&a.security_advisory.severity),
                    TriageSeverity::Critical | TriageSeverity::High
                )
        })
        .collect()
}

/// Build a candidate triage item for an open alert.
pub fn alert_to_item(alert: &DependabotAlert, now: u64) -> TriageItem {
    let package = alert
        .dependency
        .package
        .as_ref()
        .map(|p| p.name.as_str())
        .unwrap_or("(unknown package)");

    let mut detail = format!(
        "Advisory: {} ({})\nSeverity: {}\n",
        alert.security_advisory.ghsa_id,
        alert
            .security_advisory
            .cve_id
            .as_deref()
            .unwrap_or("no CVE"),
        alert.security_advisory.severity,
    );
    if let Some(vuln) = &alert.security_vulnerability {
        if let Some(range) = &vuln.vulnerable_version_range {
            detail.push_str(&format!("Vulnerable range: {range}\n"));
        }
        if let Some(patched) = &vuln.first_patched_version {
            detail.push_str(&format!("First patched version: {}\n", patched.identifier));
        }
    }
    if let Some(path) = &alert.dependency.manifest_path {
        detail.push_str(&format!("Manifest: {path}\n"));
    }
    if let Some(url) = &alert.html_url {
        detail.push_str(&format!("Alert: {url}\n"));
    }

    let mut item = TriageItem::new(
        TriageSource::SecurityFinding {
            tool: DEPENDABOT_TOOL.to_string(),
            finding_id: alert.number.to_string(),
        },
        format!("{package}: {}", alert.security_advisory.summary),
        Some(detail),
        now,
    );
    item.severity = severity_from_advisory(&alert.security_advisory.severity);
    item
}

/// Reconcile open `dependabot` triage items against the set of alert
/// numbers that are still open.  Items whose alert has left the open state
/// (fixed / dismissed / auto-dismissed upstream) are resolved.
///
/// Returns the number of items resolved.
pub fn reconcile_closed_alerts(
    items: &mut [TriageItem],
    open_alert_numbers: &[u64],
    now: u64,
) -> usize {
    let mut resolved = 0;
    for item in items.iter_mut() {
        if item.status.is_terminal() {
            continue;
        }
        let alert_number = match &item.source {
            TriageSource::SecurityFinding { tool, finding_id } if tool == DEPENDABOT_TOOL => {
                match finding_id.parse::<u64>() {
                    Ok(n) => n,
                    Err(_) => continue,
                }
            }
            _ => continue,
        };
        if !open_alert_numbers.contains(&alert_number) {
            item.status = TriageStatus::Resolved;
            item.updated_at = now;
            resolved += 1;
        }
    }
    resolved
}

/// Poll Dependabot alerts for a repository and sync them into the triage
/// queue.  Called from the pulse loop; must never fail the pulse — callers
/// treat the returned `Err` as a warning.
pub async fn run_security_ingest(
    client: &GitHubClient,
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    config: &TriageConfig,
    now: u64,
) -> Result<String, String> {
    if !config.enabled {
        return Ok("security ingest skipped (triage disabled)".to_string());
    }

    let alerts = client.list_dependabot_alerts("open").await?;
    let mut items = kv_store::load_triage(kv, owner, repo).await?;

    let first_sync = is_first_dependabot_sync(&items);
    let selected = select_alerts_for_ingest(&alerts, first_sync);
    let skipped = alerts.len() - selected.len();

    let cap = config.effective_max_items();
    let mut created = 0;
    let mut updated = 0;
    for alert in &selected {
        match upsert_item(&mut items, alert_to_item(alert, now), cap) {
            wreck_it_core::triage::TriageUpsert::Created => created += 1,
            wreck_it_core::triage::TriageUpsert::UpdatedExisting { .. } => updated += 1,
        }
    }

    // Reconcile only when the open set is complete (a single page).  With
    // 100+ open alerts, pagination would make absent-from-page-one look
    // like closed and wrongly resolve items.
    let mut resolved = 0;
    if alerts.len() < 100 {
        let open_numbers: Vec<u64> = alerts.iter().map(|a| a.number).collect();
        resolved = reconcile_closed_alerts(&mut items, &open_numbers, now);
    }

    if created + updated + resolved > 0 {
        kv_store::save_triage(kv, owner, repo, &items).await?;
    }

    let mut summary =
        format!("security: {created} new, {updated} updated, {resolved} resolved alert item(s)");
    if skipped > 0 {
        summary.push_str(&format!(
            " ({skipped} below-high alert(s) skipped on first sync)"
        ));
    }
    Ok(summary)
}

/// Handle a pull request authored by a dependency-update bot.
///
/// Observe-only: creates/updates a triage item and posts one structured
/// comment when the PR opens; resolves (merged) or dismisses (closed
/// unmerged) the item when the PR closes.  Never approves workflows or
/// enables auto-merge.
pub async fn handle_dep_update_pr(
    client: &GitHubClient,
    kv: &worker::kv::KvStore,
    owner: &str,
    repo: &str,
    pr: &PullRequest,
    action: &str,
    config: &TriageConfig,
    now: u64,
) -> Result<String, String> {
    if !config.enabled {
        return Ok("dep-update PR ignored (triage disabled)".to_string());
    }

    let mut items = kv_store::load_triage(kv, owner, repo).await?;
    let correlation_key = format!("sec:{DEP_PR_TOOL}:{}", pr.number);

    match action {
        "opened" | "synchronize" => {
            let title = pr
                .title
                .clone()
                .unwrap_or_else(|| format!("Dependency update PR #{}", pr.number));
            let severity = pr
                .body
                .as_deref()
                .and_then(parse_severity_hint)
                .unwrap_or(TriageSeverity::Low);

            let mut candidate = TriageItem::new(
                TriageSource::SecurityFinding {
                    tool: DEP_PR_TOOL.to_string(),
                    finding_id: pr.number.to_string(),
                },
                format!("Dependency update: {title}"),
                None,
                now,
            );
            candidate.severity = severity;
            candidate.pr_number = Some(pr.number);

            let upsert = upsert_item(&mut items, candidate, config.effective_max_items());
            kv_store::save_triage(kv, owner, repo, &items).await?;

            if action == "opened" && matches!(upsert, wreck_it_core::triage::TriageUpsert::Created)
            {
                let author = pr
                    .user
                    .as_ref()
                    .map(|u| u.login.as_str())
                    .unwrap_or("a dependency bot");
                let comment = build_dep_pr_comment(&title, author, severity);
                if let Err(e) = client.comment_on_pr(pr.number, &comment).await {
                    console_warn!("[wreck-it][security] dep-PR comment failed: {e}");
                }
            }
            console_log!(
                "[wreck-it][security] tracking dep-update PR #{} ({action})",
                pr.number,
            );
            Ok(format!("tracking dependency-update PR #{}", pr.number))
        }
        "closed" => {
            let merged = pr.merged.unwrap_or(false);
            let mut changed = 0;
            for item in items.iter_mut() {
                if item.correlation_key == correlation_key && !item.status.is_terminal() {
                    item.status = if merged {
                        TriageStatus::Resolved
                    } else {
                        TriageStatus::Dismissed
                    };
                    item.updated_at = now;
                    changed += 1;
                }
            }
            if changed > 0 {
                kv_store::save_triage(kv, owner, repo, &items).await?;
            }
            Ok(format!(
                "dep-update PR #{} closed ({}) — {changed} item(s) updated",
                pr.number,
                if merged { "merged" } else { "unmerged" },
            ))
        }
        _ => Ok(format!("dep-update PR action '{action}' ignored")),
    }
}

/// Build the observe-only comment posted on a new dependency-update PR.
pub fn build_dep_pr_comment(title: &str, author: &str, severity: TriageSeverity) -> String {
    format!(
        "🔧 **wreck-it** is tracking this dependency update as a triage item \
         (severity: {severity:?}).\n\n\
         - **Change**: {title}\n\
         - **Author**: {author}\n\n\
         wreck-it will **not** auto-merge or approve workflows for \
         dependency updates — supply-chain PRs bring in third-party code \
         and deserve a human (or explicitly configured) decision. The \
         triage item resolves automatically when this PR is merged or \
         closed.",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::github::{DependabotAdvisory, DependabotDependency, DependabotPackage};

    fn alert(number: u64, severity: &str) -> DependabotAlert {
        DependabotAlert {
            number,
            state: "open".to_string(),
            dependency: DependabotDependency {
                package: Some(DependabotPackage {
                    ecosystem: Some("cargo".to_string()),
                    name: "openssl".to_string(),
                }),
                manifest_path: Some("Cargo.lock".to_string()),
            },
            security_advisory: DependabotAdvisory {
                ghsa_id: format!("GHSA-{number}"),
                cve_id: Some(format!("CVE-2026-{number}")),
                summary: "bad thing".to_string(),
                severity: severity.to_string(),
            },
            security_vulnerability: None,
            html_url: Some(format!(
                "https://github.com/o/r/security/dependabot/{number}"
            )),
        }
    }

    #[test]
    fn dep_update_author_detection() {
        assert!(is_dep_update_author("dependabot[bot]"));
        assert!(is_dep_update_author("Renovate[bot]"));
        assert!(is_dep_update_author("renovate-bot"));
        assert!(!is_dep_update_author("copilot"));
        assert!(!is_dep_update_author("octocat"));
    }

    #[test]
    fn severity_mapping() {
        assert_eq!(severity_from_advisory("critical"), TriageSeverity::Critical);
        assert_eq!(severity_from_advisory("HIGH"), TriageSeverity::High);
        assert_eq!(severity_from_advisory("moderate"), TriageSeverity::Medium);
        assert_eq!(severity_from_advisory("medium"), TriageSeverity::Medium);
        assert_eq!(severity_from_advisory("low"), TriageSeverity::Low);
        assert_eq!(severity_from_advisory("unknown"), TriageSeverity::Low);
    }

    #[test]
    fn severity_hint_parsing() {
        assert_eq!(
            parse_severity_hint("This fixes a critical severity vulnerability"),
            Some(TriageSeverity::Critical)
        );
        assert_eq!(
            parse_severity_hint("Severity: high"),
            Some(TriageSeverity::High)
        );
        assert_eq!(parse_severity_hint("bumps serde from 1 to 2"), None);
    }

    #[test]
    fn first_sync_caps_to_high_and_critical() {
        let alerts = vec![
            alert(1, "critical"),
            alert(2, "high"),
            alert(3, "medium"),
            alert(4, "low"),
        ];
        let selected = select_alerts_for_ingest(&alerts, true);
        assert_eq!(
            selected.iter().map(|a| a.number).collect::<Vec<_>>(),
            vec![1, 2]
        );
        let all = select_alerts_for_ingest(&alerts, false);
        assert_eq!(all.len(), 4);
    }

    #[test]
    fn first_sync_detection() {
        let mut items = Vec::new();
        assert!(is_first_dependabot_sync(&items));
        // A dep-pr item does not count — only real alert items do.
        items.push(TriageItem::new(
            TriageSource::SecurityFinding {
                tool: DEP_PR_TOOL.to_string(),
                finding_id: "5".to_string(),
            },
            "t".to_string(),
            None,
            1000,
        ));
        assert!(is_first_dependabot_sync(&items));
        items.push(alert_to_item(&alert(1, "high"), 1000));
        assert!(!is_first_dependabot_sync(&items));
    }

    #[test]
    fn alert_item_mapping() {
        let item = alert_to_item(&alert(42, "critical"), 1000);
        assert_eq!(item.correlation_key, "sec:dependabot:42");
        assert_eq!(item.severity, TriageSeverity::Critical);
        assert_eq!(item.title, "openssl: bad thing");
        let detail = item.detail.unwrap();
        assert!(detail.contains("GHSA-42"));
        assert!(detail.contains("CVE-2026-42"));
        assert!(detail.contains("Cargo.lock"));
    }

    #[test]
    fn reconcile_resolves_only_closed_alert_items() {
        let mut items = vec![
            alert_to_item(&alert(1, "high"), 1000),
            alert_to_item(&alert(2, "high"), 1000),
        ];
        // A non-dependabot item must be untouched.
        items.push(TriageItem::new(
            TriageSource::LogEvent {
                provider: "seq".to_string(),
                event_id: "9".to_string(),
            },
            "log".to_string(),
            None,
            1000,
        ));

        let resolved = reconcile_closed_alerts(&mut items, &[2], 2000);
        assert_eq!(resolved, 1);
        assert_eq!(items[0].status, TriageStatus::Resolved); // alert 1 closed upstream
        assert_eq!(items[1].status, TriageStatus::New); // alert 2 still open
        assert_eq!(items[2].status, TriageStatus::New); // log item untouched

        // Idempotent: resolving again changes nothing.
        assert_eq!(reconcile_closed_alerts(&mut items, &[2], 3000), 0);
    }

    #[test]
    fn dep_pr_comment_mentions_no_automerge() {
        let comment = build_dep_pr_comment("Bump serde", "dependabot[bot]", TriageSeverity::Low);
        assert!(comment.contains("Bump serde"));
        assert!(comment.contains("not"));
        assert!(comment.contains("auto-merge"));
    }
}
