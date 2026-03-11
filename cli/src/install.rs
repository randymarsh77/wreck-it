use crate::repo_config::{save_repo_config, RepoConfig};
use crate::templates;
use anyhow::{Context, Result};
use std::path::Path;

/// Workflow files embedded at compile time from `templates/workflows/`.
const RALPH_WORKFLOW: &str = include_str!("../../templates/workflows/ralph.yml");
const PLAN_WORKFLOW: &str = include_str!("../../templates/workflows/plan.yml");
const UNSTUCK_WORKFLOW: &str = include_str!("../../templates/workflows/unstuck.yml");

/// Summary of what was created during an install.
#[derive(Debug)]
pub struct InstallResult {
    /// Files written to disk (relative paths for display).
    pub written: Vec<String>,
    /// Files that already existed and were skipped.
    pub skipped: Vec<String>,
    /// Ralph context names added to config.
    pub ralphs_added: Vec<String>,
}

/// Install wreck-it into `target_dir`:
///
/// 1. Creates `.wreck-it/config.toml` with the engineering-team template
///    ralph entries.
/// 2. Creates `.wreck-it/plans/.gitkeep`.
/// 3. Creates `.github/workflows/ralph.yml` (runs the wreck-it action).
/// 4. Creates `.github/workflows/plan.yml` (dispatches the plan command).
pub fn install(target_dir: &Path) -> Result<InstallResult> {
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();

    // --- .wreck-it/config.toml ---
    let config_dir = target_dir.join(".wreck-it");
    std::fs::create_dir_all(&config_dir).context("Failed to create .wreck-it directory")?;

    let tmpl = templates::find_template("engineering-team")
        .ok_or_else(|| anyhow::anyhow!("built-in engineering-team template not found"))?;

    let config_path = config_dir.join("config.toml");
    let mut repo_cfg = if config_path.exists() {
        skipped.push(".wreck-it/config.toml".to_string());
        let content = std::fs::read_to_string(&config_path)
            .context("Failed to read existing .wreck-it/config.toml")?;
        toml::from_str(&content).context("Failed to parse existing .wreck-it/config.toml")?
    } else {
        RepoConfig::default()
    };

    // Merge ralph entries from template.
    let mut ralphs_added: Vec<String> = Vec::new();
    for ralph in &tmpl.manifest.ralphs {
        if repo_cfg.ralphs.iter().any(|r| r.name == ralph.name) {
            continue;
        }
        repo_cfg.ralphs.push(ralph.clone());
        ralphs_added.push(ralph.name.clone());
    }

    if !config_path.exists() {
        save_repo_config(target_dir, &repo_cfg)?;
        written.push(".wreck-it/config.toml".to_string());
    } else if !ralphs_added.is_empty() {
        // Re-save config if we added new ralphs.
        save_repo_config(target_dir, &repo_cfg)?;
    }

    // --- .wreck-it/plans/.gitkeep ---
    let plans_dir = config_dir.join("plans");
    std::fs::create_dir_all(&plans_dir).context("Failed to create .wreck-it/plans directory")?;
    let gitkeep = plans_dir.join(".gitkeep");
    if !gitkeep.exists() {
        std::fs::write(&gitkeep, "").context("Failed to write .wreck-it/plans/.gitkeep")?;
        written.push(".wreck-it/plans/.gitkeep".to_string());
    }

    // --- .github/workflows/ ---
    let workflows_dir = target_dir.join(".github").join("workflows");
    std::fs::create_dir_all(&workflows_dir)
        .context("Failed to create .github/workflows directory")?;

    write_if_missing(
        &workflows_dir.join("ralph.yml"),
        RALPH_WORKFLOW,
        ".github/workflows/ralph.yml",
        &mut written,
        &mut skipped,
    )?;

    write_if_missing(
        &workflows_dir.join("plan.yml"),
        PLAN_WORKFLOW,
        ".github/workflows/plan.yml",
        &mut written,
        &mut skipped,
    )?;

    write_if_missing(
        &workflows_dir.join("unstuck.yml"),
        UNSTUCK_WORKFLOW,
        ".github/workflows/unstuck.yml",
        &mut written,
        &mut skipped,
    )?;

    Ok(InstallResult {
        written,
        skipped,
        ralphs_added,
    })
}

/// Write `content` to `path` if the file does not already exist.
fn write_if_missing(
    path: &Path,
    content: &str,
    display_name: &str,
    written: &mut Vec<String>,
    skipped: &mut Vec<String>,
) -> Result<()> {
    if path.exists() {
        skipped.push(display_name.to_string());
    } else {
        std::fs::write(path, content)
            .with_context(|| format!("Failed to write {}", display_name))?;
        written.push(display_name.to_string());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_install_creates_config() {
        let dir = tempdir().unwrap();
        let result = install(dir.path()).unwrap();

        assert!(result
            .written
            .contains(&".wreck-it/config.toml".to_string()));
        assert!(dir.path().join(".wreck-it/config.toml").exists());

        // Verify the config contains engineering-team ralphs.
        let content = std::fs::read_to_string(dir.path().join(".wreck-it/config.toml")).unwrap();
        let cfg: RepoConfig = toml::from_str(&content).unwrap();
assert_eq!(cfg.ralphs.len(), 6);
    }

    #[test]
    fn test_install_creates_plans_gitkeep() {
        let dir = tempdir().unwrap();
        install(dir.path()).unwrap();
        assert!(dir.path().join(".wreck-it/plans/.gitkeep").exists());
    }

    #[test]
    fn test_install_creates_ralph_workflow() {
        let dir = tempdir().unwrap();
        let result = install(dir.path()).unwrap();

        assert!(result
            .written
            .contains(&".github/workflows/ralph.yml".to_string()));
        assert!(dir.path().join(".github/workflows/ralph.yml").exists());

        let content =
            std::fs::read_to_string(dir.path().join(".github/workflows/ralph.yml")).unwrap();
        assert!(content.contains("randymarsh77/wreck-it/action@main"));
    }

    #[test]
    fn test_install_creates_plan_workflow() {
        let dir = tempdir().unwrap();
        let result = install(dir.path()).unwrap();

        assert!(result
            .written
            .contains(&".github/workflows/plan.yml".to_string()));
        assert!(dir.path().join(".github/workflows/plan.yml").exists());

        let content =
            std::fs::read_to_string(dir.path().join(".github/workflows/plan.yml")).unwrap();
        assert!(content.contains("wreck-it plan"));
    }

    #[test]
    fn test_install_creates_unstuck_workflow() {
        let dir = tempdir().unwrap();
        let result = install(dir.path()).unwrap();

        assert!(result
            .written
            .contains(&".github/workflows/unstuck.yml".to_string()));
        assert!(dir.path().join(".github/workflows/unstuck.yml").exists());

        let content =
            std::fs::read_to_string(dir.path().join(".github/workflows/unstuck.yml")).unwrap();
        assert!(content.contains("Unstuck"));
    }

    #[test]
    fn test_install_skips_existing_files() {
        let dir = tempdir().unwrap();

        // First install.
        install(dir.path()).unwrap();

        // Second install should skip everything.
        let result = install(dir.path()).unwrap();

        assert!(result.written.is_empty());
        assert!(result
            .skipped
            .contains(&".wreck-it/config.toml".to_string()));
        assert!(result
            .skipped
            .contains(&".github/workflows/ralph.yml".to_string()));
        assert!(result
            .skipped
            .contains(&".github/workflows/plan.yml".to_string()));
        assert!(result
            .skipped
            .contains(&".github/workflows/unstuck.yml".to_string()));
        assert!(result.ralphs_added.is_empty());
    }

    #[test]
    fn test_install_ralphs_added() {
        let dir = tempdir().unwrap();
        let result = install(dir.path()).unwrap();

assert_eq!(result.ralphs_added.len(), 6);
        assert!(result.ralphs_added.contains(&"docs".to_string()));
        assert!(result.ralphs_added.contains(&"features".to_string()));
        assert!(result.ralphs_added.contains(&"planner".to_string()));
        assert!(result.ralphs_added.contains(&"cohesiveness".to_string()));
        assert!(result.ralphs_added.contains(&"feature-dev".to_string()));
        assert!(result.ralphs_added.contains(&"merge".to_string()));
    }

    #[test]
    fn test_install_merges_into_existing_config() {
        let dir = tempdir().unwrap();

        // Create a config with only one ralph.
        let config_dir = dir.path().join(".wreck-it");
        std::fs::create_dir_all(&config_dir).unwrap();
        let cfg = RepoConfig {
            ralphs: vec![crate::repo_config::RalphConfig {
                name: "docs".to_string(),
                task_file: "my-docs.json".to_string(),
                state_file: ".my-docs-state.json".to_string(),
                branch: None,
                agent: None,
                reviewers: None,
                command: None,
                brute_mode: None,
                backend: None,
prompt_dir: None,
                validation_command: None,
            }],
            ..RepoConfig::default()
        };
        save_repo_config(dir.path(), &cfg).unwrap();

        let result = install(dir.path()).unwrap();

// "docs" should not be duplicated; 5 new ralphs should be added.
        assert_eq!(result.ralphs_added.len(), 5);
        assert!(!result.ralphs_added.contains(&"docs".to_string()));

        // Verify existing docs config preserved.
        let content = std::fs::read_to_string(dir.path().join(".wreck-it/config.toml")).unwrap();
        let loaded: RepoConfig = toml::from_str(&content).unwrap();
        let docs = loaded.ralphs.iter().find(|r| r.name == "docs").unwrap();
        assert_eq!(docs.task_file, "my-docs.json");
    }
}
