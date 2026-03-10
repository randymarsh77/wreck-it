use crate::repo_config::{RalphConfig, RepoConfig};
use anyhow::{Context, Result};
use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;

/// Metadata for a built-in template, parsed from `template.toml`.
#[derive(Debug, Clone, Deserialize)]
pub struct TemplateManifest {
    /// Human-readable template name (e.g. `"engineering-team"`).
    pub name: String,

    /// One-line description of the template.
    pub description: String,

    /// Ralph contexts the template provides.
    #[serde(default)]
    pub ralphs: Vec<RalphConfig>,
}

/// A bundled template containing its manifest and all associated files.
pub struct Template {
    pub manifest: TemplateManifest,
    /// Map of filename → file contents (embedded at compile time).
    pub files: HashMap<String, &'static str>,
}

/// Return all built-in templates shipped with wreck-it.
pub fn builtin_templates() -> Vec<Template> {
    vec![engineering_team()]
}

/// The **engineering-team** template: a multi-ralph team with recurring
/// documentation review, feature management, and research planning tasks.
fn engineering_team() -> Template {
    let manifest_str = include_str!("../../templates/engineering-team/template.toml");
    let manifest: TemplateManifest =
        toml::from_str(manifest_str).expect("invalid engineering-team template.toml");

    let mut files = HashMap::new();
    files.insert(
        "docs-tasks.json".to_string(),
        include_str!("../../templates/engineering-team/docs-tasks.json"),
    );
    files.insert(
        "features-tasks.json".to_string(),
        include_str!("../../templates/engineering-team/features-tasks.json"),
    );
    files.insert(
        "feature-dev-tasks.json".to_string(),
        include_str!("../../templates/engineering-team/feature-dev-tasks.json"),
    );
    files.insert(
        "planner-tasks.json".to_string(),
        include_str!("../../templates/engineering-team/planner-tasks.json"),
    );
    files.insert(
        "cohesiveness-tasks.json".to_string(),
        include_str!("../../templates/engineering-team/cohesiveness-tasks.json"),
    );

    Template { manifest, files }
}

/// Find a built-in template by name.
pub fn find_template(name: &str) -> Option<Template> {
    builtin_templates()
        .into_iter()
        .find(|t| t.manifest.name == name)
}

/// Apply a template to the project: write task files into `state_dir` and
/// merge ralph entries into `config`.
///
/// Files that already exist in the state directory are **not** overwritten
/// (the caller can inform the user).  Ralph entries whose name already
/// appears in `config.ralphs` are likewise skipped.
pub fn apply_template(
    template: &Template,
    state_dir: &Path,
    config: &mut RepoConfig,
) -> Result<ApplyResult> {
    let mut written: Vec<String> = Vec::new();
    let mut skipped: Vec<String> = Vec::new();
    let mut ralphs_added: Vec<String> = Vec::new();

    // Write task files.
    for (name, content) in &template.files {
        let path = state_dir.join(name);
        if path.exists() {
            skipped.push(name.clone());
        } else {
            std::fs::write(&path, content)
                .with_context(|| format!("Failed to write template file: {}", name))?;
            written.push(name.clone());
        }
    }

    // Merge ralph entries.
    for ralph in &template.manifest.ralphs {
        if config.ralphs.iter().any(|r| r.name == ralph.name) {
            continue;
        }
        config.ralphs.push(ralph.clone());
        ralphs_added.push(ralph.name.clone());
    }

    Ok(ApplyResult {
        written,
        skipped,
        ralphs_added,
    })
}

/// Summary of what changed when a template was applied.
#[derive(Debug)]
pub struct ApplyResult {
    /// Files that were written to the state directory.
    pub written: Vec<String>,
    /// Files that already existed and were skipped.
    pub skipped: Vec<String>,
    /// Ralph context names that were added to the config.
    pub ralphs_added: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_builtin_templates_not_empty() {
        let templates = builtin_templates();
        assert!(!templates.is_empty());
    }

    #[test]
    fn test_engineering_team_template_metadata() {
        let tmpl = find_template("engineering-team").expect("template should exist");
        assert_eq!(tmpl.manifest.name, "engineering-team");
        assert!(!tmpl.manifest.description.is_empty());
        assert_eq!(tmpl.manifest.ralphs.len(), 6);

        let names: Vec<&str> = tmpl
            .manifest
            .ralphs
            .iter()
            .map(|r| r.name.as_str())
            .collect();
        assert!(names.contains(&"docs"));
        assert!(names.contains(&"features"));
        assert!(names.contains(&"planner"));
        assert!(names.contains(&"cohesiveness"));
        assert!(names.contains(&"feature-dev"));
        assert!(names.contains(&"merge"));
    }

    #[test]
    fn test_engineering_team_template_has_task_files() {
        let tmpl = find_template("engineering-team").unwrap();
        assert!(tmpl.files.contains_key("docs-tasks.json"));
        assert!(tmpl.files.contains_key("features-tasks.json"));
        assert!(tmpl.files.contains_key("feature-dev-tasks.json"));
        assert!(tmpl.files.contains_key("planner-tasks.json"));
        assert!(tmpl.files.contains_key("cohesiveness-tasks.json"));
    }

    #[test]
    fn test_engineering_team_task_files_are_valid_json() {
        let tmpl = find_template("engineering-team").unwrap();
        for (name, content) in &tmpl.files {
            let parsed: serde_json::Value = serde_json::from_str(content)
                .unwrap_or_else(|e| panic!("{} is invalid JSON: {}", name, e));
            assert!(parsed.is_array(), "{} should be a JSON array", name);
        }
    }

    #[test]
    fn test_find_template_returns_none_for_unknown() {
        assert!(find_template("nonexistent").is_none());
    }

    #[test]
    fn test_apply_template_writes_files() {
        let dir = tempdir().unwrap();
        let tmpl = find_template("engineering-team").unwrap();
        let mut config = RepoConfig::default();

        let result = apply_template(&tmpl, dir.path(), &mut config).unwrap();

        assert!(!result.written.is_empty());
        assert!(result.skipped.is_empty());
        assert_eq!(result.ralphs_added.len(), 6);
        assert_eq!(config.ralphs.len(), 6);

        // Verify files exist on disk.
        for name in &result.written {
            assert!(dir.path().join(name).exists(), "file {} should exist", name);
        }
    }

    #[test]
    fn test_apply_template_skips_existing_files() {
        let dir = tempdir().unwrap();
        // Pre-create one file.
        std::fs::write(dir.path().join("docs-tasks.json"), "[]").unwrap();

        let tmpl = find_template("engineering-team").unwrap();
        let mut config = RepoConfig::default();

        let result = apply_template(&tmpl, dir.path(), &mut config).unwrap();

        assert!(result.skipped.contains(&"docs-tasks.json".to_string()));
        assert!(!result.written.contains(&"docs-tasks.json".to_string()));

        // Existing file should not be overwritten.
        let content = std::fs::read_to_string(dir.path().join("docs-tasks.json")).unwrap();
        assert_eq!(content, "[]");
    }

    #[test]
    fn test_apply_template_skips_existing_ralphs() {
        let dir = tempdir().unwrap();
        let tmpl = find_template("engineering-team").unwrap();
        let mut config = RepoConfig {
            ralphs: vec![RalphConfig {
                name: "docs".to_string(),
                task_file: "custom-docs.json".to_string(),
                state_file: ".custom-docs-state.json".to_string(),
                branch: None,
                agent: None,
                reviewers: None,
                command: None,
                brute_mode: None,
                backend: None,

                prompt_dir: None,
            }],
            ..RepoConfig::default()
        };

        let result = apply_template(&tmpl, dir.path(), &mut config).unwrap();

        // "docs" ralph should not be duplicated.
        assert_eq!(config.ralphs.iter().filter(|r| r.name == "docs").count(), 1);
        // The existing custom path should be preserved.
        assert_eq!(config.ralphs[0].task_file, "custom-docs.json");

        // "features", "planner", and "feature-dev" should be added.
        assert_eq!(result.ralphs_added.len(), 5);
        assert!(result.ralphs_added.contains(&"features".to_string()));
        assert!(result.ralphs_added.contains(&"planner".to_string()));
        assert!(result.ralphs_added.contains(&"cohesiveness".to_string()));
        assert!(result.ralphs_added.contains(&"feature-dev".to_string()));
        assert!(result.ralphs_added.contains(&"merge".to_string()));
    }

    #[test]
    fn test_apply_template_idempotent() {
        let dir = tempdir().unwrap();
        let tmpl = find_template("engineering-team").unwrap();
        let mut config = RepoConfig::default();

        // First apply.
        apply_template(&tmpl, dir.path(), &mut config).unwrap();

        // Second apply should skip everything.
        let tmpl2 = find_template("engineering-team").unwrap();
        let result = apply_template(&tmpl2, dir.path(), &mut config).unwrap();

        assert!(result.written.is_empty());
        assert!(!result.skipped.is_empty());
        assert!(result.ralphs_added.is_empty());
        assert_eq!(config.ralphs.len(), 6);
    }
}
