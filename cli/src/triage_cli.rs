//! CLI helpers for the `wreck-it triage` subcommand.
//!
//! Reads triage items from the wreck-it worker's REST API
//! (`GET /api/repos/{owner}/{repo}/triage`).  The worker base URL and
//! bearer token come from the `WRECK_IT_API_URL` and `WRECK_IT_API_TOKEN`
//! environment variables.

use anyhow::{bail, Context, Result};
use wreck_it_core::triage::{TriageItem, TriageStatus};

/// Environment variable holding the worker base URL
/// (e.g. `https://wreck-it.example.workers.dev`).
pub const API_URL_ENV: &str = "WRECK_IT_API_URL";

/// Environment variable holding the worker `API_TOKEN` bearer token.
pub const API_TOKEN_ENV: &str = "WRECK_IT_API_TOKEN";

/// Split an `owner/repo` argument into its parts.
pub fn parse_repo(repo: &str) -> Result<(&str, &str)> {
    match repo.split_once('/') {
        Some((owner, name)) if !owner.is_empty() && !name.is_empty() => Ok((owner, name)),
        _ => bail!("--repo must be in 'owner/name' form, got '{repo}'"),
    }
}

/// Parse a `--status` filter value into a [`TriageStatus`].
pub fn parse_status_filter(value: &str) -> Result<TriageStatus> {
    match value.to_ascii_lowercase().as_str() {
        "new" => Ok(TriageStatus::New),
        "investigating" => Ok(TriageStatus::Investigating),
        "pr-open" | "pr_open" | "propen" => Ok(TriageStatus::PrOpen),
        "resolved" => Ok(TriageStatus::Resolved),
        "dismissed" => Ok(TriageStatus::Dismissed),
        "stale" => Ok(TriageStatus::Stale),
        other => bail!(
            "unknown status '{other}' (expected: new, investigating, pr-open, \
             resolved, dismissed, stale)"
        ),
    }
}

/// Build the list-endpoint URL for a repository.
pub fn list_url(base: &str, owner: &str, repo: &str) -> String {
    format!(
        "{}/api/repos/{owner}/{repo}/triage",
        base.trim_end_matches('/')
    )
}

/// Short human-readable status label.
pub fn status_label(status: TriageStatus) -> &'static str {
    match status {
        TriageStatus::New => "new",
        TriageStatus::Investigating => "investigating",
        TriageStatus::PrOpen => "pr-open",
        TriageStatus::Resolved => "resolved",
        TriageStatus::Dismissed => "dismissed",
        TriageStatus::Stale => "stale",
    }
}

/// Render items as an aligned table (id, status, severity, occurrences,
/// title).  Returns the rendered string so it is unit-testable.
pub fn render_table(items: &[TriageItem]) -> String {
    if items.is_empty() {
        return "No triage items found.\n".to_string();
    }
    let id_w = items
        .iter()
        .map(|i| i.id.len())
        .max()
        .unwrap_or(2)
        .max("ID".len());
    let status_w = items
        .iter()
        .map(|i| status_label(i.status).len())
        .max()
        .unwrap_or(6)
        .max("STATUS".len());

    let mut out = format!(
        "{:<id_w$}  {:<status_w$}  {:<8}  {:>3}  TITLE\n",
        "ID", "STATUS", "SEVERITY", "N"
    );
    for item in items {
        out.push_str(&format!(
            "{:<id_w$}  {:<status_w$}  {:<8}  {:>3}  {}\n",
            item.id,
            status_label(item.status),
            format!("{:?}", item.severity).to_lowercase(),
            item.occurrences,
            item.title,
        ));
    }
    out
}

/// Render a single item with full detail.
pub fn render_item(item: &TriageItem) -> String {
    let mut out = String::new();
    out.push_str(&format!("id:          {}\n", item.id));
    out.push_str(&format!("title:       {}\n", item.title));
    out.push_str(&format!("status:      {}\n", status_label(item.status)));
    out.push_str(&format!(
        "severity:    {}\n",
        format!("{:?}", item.severity).to_lowercase()
    ));
    out.push_str(&format!("occurrences: {}\n", item.occurrences));
    out.push_str(&format!("correlation: {}\n", item.correlation_key));
    out.push_str(&format!("created_at:  {}\n", item.created_at));
    out.push_str(&format!("updated_at:  {}\n", item.updated_at));
    if let Some(n) = item.issue_number {
        out.push_str(&format!("issue:       #{n}\n"));
    }
    if let Some(n) = item.pr_number {
        out.push_str(&format!("pr:          #{n}\n"));
    }
    out.push_str(&format!("source:      {:?}\n", item.source));
    if let Some(detail) = &item.detail {
        out.push_str("\n--- evidence ---\n");
        out.push_str(detail);
        out.push('\n');
    }
    out
}

/// Resolve the API base URL and token from the environment.
fn resolve_api_env() -> Result<(String, String)> {
    let base = std::env::var(API_URL_ENV)
        .with_context(|| format!("{API_URL_ENV} is not set (worker base URL)"))?;
    let token = std::env::var(API_TOKEN_ENV)
        .with_context(|| format!("{API_TOKEN_ENV} is not set (worker API token)"))?;
    Ok((base, token))
}

/// Fetch all triage items for `owner/repo` from the worker API.
async fn fetch_items(owner: &str, repo: &str) -> Result<Vec<TriageItem>> {
    let (base, token) = resolve_api_env()?;
    let url = list_url(&base, owner, repo);
    let response = reqwest::Client::new()
        .get(&url)
        .bearer_auth(&token)
        .send()
        .await
        .with_context(|| format!("request to {url} failed"))?;

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        bail!("worker API returned {status}: {body}");
    }
    response
        .json::<Vec<TriageItem>>()
        .await
        .context("failed to parse triage items JSON")
}

/// `wreck-it triage list --repo owner/name [--status ...]`
pub async fn run_list(repo: &str, status: Option<&str>) -> Result<()> {
    let (owner, name) = parse_repo(repo)?;
    let filter = status.map(parse_status_filter).transpose()?;
    let mut items = fetch_items(owner, name).await?;
    if let Some(filter) = filter {
        items.retain(|i| i.status == filter);
    }
    items.sort_by(|a, b| b.updated_at.cmp(&a.updated_at));
    print!("{}", render_table(&items));
    Ok(())
}

/// `wreck-it triage show <id> --repo owner/name`
pub async fn run_show(repo: &str, id: &str) -> Result<()> {
    let (owner, name) = parse_repo(repo)?;
    let items = fetch_items(owner, name).await?;
    match items.iter().find(|i| i.id == id) {
        Some(item) => {
            print!("{}", render_item(item));
            Ok(())
        }
        None => bail!("no triage item with id '{id}' in {owner}/{name}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wreck_it_core::triage::TriageSource;

    fn item(id: &str, status: TriageStatus) -> TriageItem {
        let mut item = TriageItem::new(
            TriageSource::CiFailure {
                run_id: 1,
                workflow_name: "CI".into(),
                branch: "main".into(),
                head_sha: "abc".into(),
                conclusion: "failure".into(),
                run_url: None,
                run_attempt: 1,
            },
            format!("CI failure ({id})"),
            None,
            1000,
        );
        item.id = id.to_string();
        item.status = status;
        item
    }

    #[test]
    fn parse_repo_accepts_owner_name() {
        assert_eq!(parse_repo("octo/repo").unwrap(), ("octo", "repo"));
        assert!(parse_repo("nope").is_err());
        assert!(parse_repo("/half").is_err());
        assert!(parse_repo("half/").is_err());
    }

    #[test]
    fn parse_status_filter_variants() {
        assert_eq!(parse_status_filter("new").unwrap(), TriageStatus::New);
        assert_eq!(
            parse_status_filter("PR-Open").unwrap(),
            TriageStatus::PrOpen
        );
        assert_eq!(
            parse_status_filter("pr_open").unwrap(),
            TriageStatus::PrOpen
        );
        assert!(parse_status_filter("bogus").is_err());
    }

    #[test]
    fn list_url_shape() {
        assert_eq!(
            list_url("https://w.example.dev/", "octo", "repo"),
            "https://w.example.dev/api/repos/octo/repo/triage"
        );
    }

    #[test]
    fn render_table_empty() {
        assert_eq!(render_table(&[]), "No triage items found.\n");
    }

    #[test]
    fn render_table_aligns_columns() {
        let items = vec![
            item("tri-1000-1", TriageStatus::New),
            item("tri-1000-22", TriageStatus::Investigating),
        ];
        let out = render_table(&items);
        assert!(out.contains("ID"));
        assert!(out.contains("tri-1000-1"));
        assert!(out.contains("investigating"));
        // Header and two rows.
        assert_eq!(out.lines().count(), 3);
    }

    #[test]
    fn render_item_includes_links_and_evidence() {
        let mut it = item("tri-1", TriageStatus::PrOpen);
        it.issue_number = Some(5);
        it.pr_number = Some(9);
        it.detail = Some("boom".into());
        let out = render_item(&it);
        assert!(out.contains("issue:       #5"));
        assert!(out.contains("pr:          #9"));
        assert!(out.contains("--- evidence ---"));
        assert!(out.contains("boom"));
    }
}
