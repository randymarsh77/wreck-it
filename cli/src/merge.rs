//! "Merge" command: scan open PRs for merge conflicts with the base branch
//! and resolve them.
//!
//! When `backend = "cloud_agent"` (the default), the command creates a GitHub
//! issue describing the conflict with full context (PR body, conflicting
//! commit messages, and code diff) and assigns a coding agent to fix it.
//!
//! When `backend = "cli"`, the command performs a local `git merge` of the
//! base branch into the PR branch and pushes the result directly.
//!
//! This can be used as a standalone CLI command (`wreck-it merge`) or as a
//! ralph command (`command = "merge"` in `[[ralphs]]`).

use crate::cloud_agent::{resolve_repo_info, CloudAgentClient};
use crate::headless_config::{load_headless_config, HeadlessConfig};
use crate::state_worktree::ensure_state_worktree;
use crate::types::Config;
use anyhow::{Context, Result};
use std::path::Path;

/// Default name for the repo-committed config file.
const DEFAULT_CONFIG_FILE: &str = ".wreck-it.toml";

/// Supported backend values.
const BACKEND_CLOUD_AGENT: &str = "cloud_agent";
const BACKEND_CLI: &str = "cli";

/// Run the merge logic: find open PRs with merge conflicts and resolve them.
///
/// `backend` selects how conflicts are resolved – `"cloud_agent"` (default)
/// assigns a coding agent via a new issue, while `"cli"` merges locally and
/// pushes.
pub async fn run_merge(config: &Config, backend: Option<&str>) -> Result<()> {
    let work_dir = &config.work_dir;
    let backend = backend.unwrap_or(BACKEND_CLOUD_AGENT);

    let github_token = config
        .api_token
        .clone()
        .or_else(|| std::env::var("GITHUB_TOKEN").ok())
        .context("GitHub token required for merge command")?;

    let headless_cfg = load_headless_cfg(work_dir)?;

    let (repo_owner, repo_name) = resolve_repo_info(
        headless_cfg.repo_owner.as_deref(),
        headless_cfg.repo_name.as_deref(),
        work_dir,
    )?;

    println!(
        "[wreck-it] merge: scanning {}/{} for PRs with merge conflicts (backend={})",
        repo_owner, repo_name, backend,
    );

    let mut client =
        CloudAgentClient::new(github_token.clone(), repo_owner.clone(), repo_name.clone());
    client.resolve_authenticated_login().await;

    let prs = client.list_open_prs().await?;
    if prs.is_empty() {
        println!("[wreck-it] merge: no open PRs found");
        return Ok(());
    }

    println!("[wreck-it] merge: found {} open PR(s)", prs.len());

    let mut fixed = 0u32;
    for pr in &prs {
        let pr_detail = fetch_pr_detail(&client, &repo_owner, &repo_name, pr.number).await;
        let pr_detail = match pr_detail {
            Ok(d) => d,
            Err(e) => {
                println!(
                    "[wreck-it] merge: failed to fetch details for PR #{}: {}",
                    pr.number, e,
                );
                continue;
            }
        };

        if !pr_detail.has_conflicts {
            println!(
                "[wreck-it] merge: PR #{} ({}) — no conflicts, skipping",
                pr.number, pr.title,
            );
            continue;
        }

        println!(
            "[wreck-it] merge: PR #{} ({}) has merge conflicts — resolving via {}",
            pr.number, pr.title, backend,
        );

        let result = match backend {
            BACKEND_CLI => {
                resolve_via_cli(work_dir, &pr_detail, &github_token, &repo_owner, &repo_name).await
            }
            _ => resolve_via_cloud_agent(&client, &pr_detail).await,
        };

        match result {
            Ok(()) => {
                fixed += 1;
            }
            Err(e) => {
                println!(
                    "[wreck-it] merge: failed to resolve conflicts on PR #{}: {}",
                    pr.number, e,
                );
            }
        }
    }

    println!(
        "[wreck-it] merge: done — initiated conflict resolution on {} PR(s)",
        fixed,
    );
    Ok(())
}

/// Detailed information about a PR needed for conflict resolution.
#[derive(Debug)]
struct PrDetail {
    number: u64,
    title: String,
    body: String,
    head_ref: String,
    base_ref: String,
    has_conflicts: bool,
}

/// Fetch PR details from the GitHub REST API.
async fn fetch_pr_detail(
    client: &CloudAgentClient,
    repo_owner: &str,
    repo_name: &str,
    pr_number: u64,
) -> Result<PrDetail> {
    let pr_json = client.fetch_pr_json(pr_number).await?;

    let title = pr_json["title"].as_str().unwrap_or("").to_string();
    let body = pr_json["body"].as_str().unwrap_or("").to_string();
    let head_ref = pr_json
        .pointer("/head/ref")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let base_ref = pr_json
        .pointer("/base/ref")
        .and_then(|v| v.as_str())
        .unwrap_or("main")
        .to_string();

    // `mergeable` is null while GitHub is still computing it; treat null as
    // "no conflicts detected yet".
    let mergeable = pr_json["mergeable"].as_bool().unwrap_or(true);
    let mergeable_state = pr_json["mergeable_state"].as_str().unwrap_or("unknown");
    let has_conflicts = !mergeable || mergeable_state == "dirty";

    let _ = (repo_owner, repo_name); // used by the caller context

    Ok(PrDetail {
        number: pr_number,
        title,
        body,
        head_ref,
        base_ref,
        has_conflicts,
    })
}

/// Build the context string for a merge conflict resolution prompt.
fn build_merge_context(detail: &PrDetail, recent_base_commits: &str, diff_summary: &str) -> String {
    let mut ctx = String::new();

    ctx.push_str(&format!("## PR #{}: {}\n\n", detail.number, detail.title,));

    if !detail.body.is_empty() {
        ctx.push_str("### PR Description\n\n");
        ctx.push_str(&detail.body);
        ctx.push_str("\n\n");
    }

    ctx.push_str(&format!(
        "### Branches\n\n- Head: `{}`\n- Base: `{}`\n\n",
        detail.head_ref, detail.base_ref,
    ));

    if !recent_base_commits.is_empty() {
        ctx.push_str(&format!(
            "### Recent commits on `{}` that may conflict\n\n```\n{}\n```\n\n",
            detail.base_ref, recent_base_commits,
        ));
    }

    if !diff_summary.is_empty() {
        ctx.push_str(&format!(
            "### Diff summary (base vs head)\n\n```\n{}\n```\n\n",
            diff_summary,
        ));
    }

    ctx
}

/// Resolve merge conflicts by creating an issue and assigning a cloud agent.
async fn resolve_via_cloud_agent(client: &CloudAgentClient, detail: &PrDetail) -> Result<()> {
    // Gather context: recent base commits and diff.
    let recent_base_commits = fetch_recent_base_commits_via_api(client, &detail.base_ref).await;
    let diff_summary = fetch_diff_summary_via_api(client, detail.number).await;

    let context = build_merge_context(detail, &recent_base_commits, &diff_summary);

    let issue_body = format!(
        "PR #{} (`{}` → `{}`) has merge conflicts that need to be resolved.\n\n\
         Please check out the `{}` branch, merge `{}` into it, resolve all \
         conflicts, and push the result.\n\n\
         {}\n\n\
         ---\n\
         *Triggered by wreck-it merge ralph*",
        detail.number, detail.head_ref, detail.base_ref, detail.head_ref, detail.base_ref, context,
    );

    let result = client
        .trigger_agent(
            "merge",
            &format!("merge-pr-{}", detail.number),
            &issue_body,
            &[],
            Some(&detail.head_ref),
        )
        .await?;

    println!(
        "[wreck-it] merge: created issue #{} for PR #{} conflict resolution ({})",
        result.issue_number, detail.number, result.issue_url,
    );
    Ok(())
}

/// Resolve merge conflicts locally via git and push the fix.
async fn resolve_via_cli(
    work_dir: &Path,
    detail: &PrDetail,
    _github_token: &str,
    _repo_owner: &str,
    _repo_name: &str,
) -> Result<()> {
    use std::process::Command;

    // Fetch latest refs.
    let fetch_status = Command::new("git")
        .args(["fetch", "origin"])
        .current_dir(work_dir)
        .status()
        .context("Failed to run `git fetch origin`")?;
    if !fetch_status.success() {
        anyhow::bail!("git fetch origin failed");
    }

    // Check out the PR branch.
    let checkout_status = Command::new("git")
        .args(["checkout", &detail.head_ref])
        .current_dir(work_dir)
        .status()
        .context("Failed to checkout PR branch")?;
    if !checkout_status.success() {
        anyhow::bail!("git checkout {} failed", detail.head_ref);
    }

    // Attempt the merge.
    let merge_output = Command::new("git")
        .args(["merge", &format!("origin/{}", detail.base_ref), "--no-edit"])
        .current_dir(work_dir)
        .output()
        .context("Failed to run git merge")?;

    if merge_output.status.success() {
        // Merge succeeded without conflicts — push.
        let push_status = Command::new("git")
            .args(["push", "origin", &detail.head_ref])
            .current_dir(work_dir)
            .status()
            .context("Failed to push merged branch")?;
        if !push_status.success() {
            anyhow::bail!("git push origin {} failed", detail.head_ref);
        }
        println!(
            "[wreck-it] merge: cleanly merged {} into {} and pushed",
            detail.base_ref, detail.head_ref,
        );
    } else {
        // Merge has conflicts.  Abort and report.
        let _ = Command::new("git")
            .args(["merge", "--abort"])
            .current_dir(work_dir)
            .status();

        anyhow::bail!(
            "git merge of {} into {} produced conflicts that require manual resolution; \
             consider using backend = \"cloud_agent\" to assign a coding agent",
            detail.base_ref,
            detail.head_ref,
        );
    }

    Ok(())
}

/// Fetch recent commit messages on the base branch via the GitHub REST API.
async fn fetch_recent_base_commits_via_api(client: &CloudAgentClient, base_ref: &str) -> String {
    match client.fetch_recent_commits(base_ref, 10).await {
        Ok(commits) => commits,
        Err(e) => {
            println!(
                "[wreck-it] merge: could not fetch recent base commits: {}",
                e,
            );
            String::new()
        }
    }
}

/// Fetch a summary of the PR diff via the GitHub REST API.
async fn fetch_diff_summary_via_api(client: &CloudAgentClient, pr_number: u64) -> String {
    match client.fetch_pr_files_summary(pr_number).await {
        Ok(summary) => summary,
        Err(e) => {
            println!(
                "[wreck-it] merge: could not fetch diff summary for PR #{}: {}",
                pr_number, e,
            );
            String::new()
        }
    }
}

/// Load the headless config, preferring the state worktree copy when the state
/// branch can be determined.
fn load_headless_cfg(work_dir: &Path) -> Result<HeadlessConfig> {
    // Try the main checkout first.
    let bootstrap_path = work_dir.join(DEFAULT_CONFIG_FILE);
    let bootstrap = if bootstrap_path.exists() {
        load_headless_config(&bootstrap_path)?
    } else {
        HeadlessConfig::default()
    };

    // Attempt to set up the state worktree so we can read a more up-to-date
    // config from there (mirrors the pattern in run_headless).
    let repo_cfg = crate::repo_config::load_repo_config(work_dir)
        .ok()
        .flatten();
    let state_branch = repo_cfg
        .as_ref()
        .map(|c| c.state_branch.clone())
        .unwrap_or_else(|| bootstrap.state_branch.clone());

    if let Ok(state_dir) = ensure_state_worktree(work_dir, &state_branch) {
        let state_cfg_path = state_dir.join(DEFAULT_CONFIG_FILE);
        if state_cfg_path.exists() {
            return load_headless_config(&state_cfg_path)
                .context("Failed to load .wreck-it.toml from state worktree");
        }
    }

    Ok(bootstrap)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Verify that the default headless config is returned when no config file
    /// exists (no panic).
    #[test]
    fn load_headless_cfg_defaults_without_config_file() {
        let dir = tempfile::tempdir().unwrap();
        let cfg = load_headless_cfg(dir.path()).unwrap();
        assert_eq!(cfg.state_branch, "wreck-it-state");
    }

    #[test]
    fn build_merge_context_includes_all_sections() {
        let detail = PrDetail {
            number: 42,
            title: "Add feature X".to_string(),
            body: "This PR adds feature X.".to_string(),
            head_ref: "feature-x".to_string(),
            base_ref: "main".to_string(),
            has_conflicts: true,
        };
        let commits = "abc1234 fix: update config\ndef5678 feat: add new module";
        let diff = "src/main.rs | 10 +++++-----\nsrc/lib.rs  |  3 +++";

        let ctx = build_merge_context(&detail, commits, diff);

        assert!(ctx.contains("PR #42: Add feature X"));
        assert!(ctx.contains("This PR adds feature X."));
        assert!(ctx.contains("Head: `feature-x`"));
        assert!(ctx.contains("Base: `main`"));
        assert!(ctx.contains("abc1234 fix: update config"));
        assert!(ctx.contains("src/main.rs | 10"));
    }

    #[test]
    fn build_merge_context_omits_empty_sections() {
        let detail = PrDetail {
            number: 7,
            title: "Small fix".to_string(),
            body: String::new(),
            head_ref: "fix-typo".to_string(),
            base_ref: "main".to_string(),
            has_conflicts: true,
        };

        let ctx = build_merge_context(&detail, "", "");

        assert!(ctx.contains("PR #7: Small fix"));
        assert!(!ctx.contains("PR Description"));
        assert!(!ctx.contains("Recent commits"));
        assert!(!ctx.contains("Diff summary"));
    }

    #[test]
    fn backend_constants_are_valid() {
        assert_eq!(BACKEND_CLOUD_AGENT, "cloud_agent");
        assert_eq!(BACKEND_CLI, "cli");
    }
}
