use crate::types::{ArtefactKind, TaskArtefact};
use anyhow::{bail, Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::path::Path;

/// A single artefact entry persisted in the manifest.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ArtefactEntry {
    pub kind: ArtefactKind,
    /// Logical name (matches the [`TaskArtefact::name`] field).
    pub name: String,
    /// Relative path that was used to read the artefact content.
    pub path: String,
    /// Raw text content of the artefact at the time it was persisted.
    pub content: String,
}

/// Persistent manifest that maps `"task-id/artefact-name"` keys to their
/// stored content.  The manifest file lives alongside `.wreck-it-state.json`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct ArtefactManifest {
    #[serde(default)]
    pub artefacts: HashMap<String, ArtefactEntry>,
}

impl ArtefactManifest {
    /// Return the canonical manifest key for a (task, artefact) pair.
    pub fn key(task_id: &str, artefact_name: &str) -> String {
        format!("{}/{}", task_id, artefact_name)
    }
}

/// Load the artefact manifest from `path`.  Returns an empty manifest when
/// the file does not exist (backward compatibility).
pub fn load_manifest(path: &Path) -> Result<ArtefactManifest> {
    if !path.exists() {
        return Ok(ArtefactManifest::default());
    }
    let content = fs::read_to_string(path).context("Failed to read artefact manifest")?;
    let manifest = serde_json::from_str(&content).context("Failed to parse artefact manifest")?;
    Ok(manifest)
}

/// Save `manifest` to `path`, creating parent directories as needed.
pub fn save_manifest(path: &Path, manifest: &ArtefactManifest) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).context("Failed to create manifest directory")?;
    }
    let content =
        serde_json::to_string_pretty(manifest).context("Failed to serialise artefact manifest")?;
    fs::write(path, content).context("Failed to write artefact manifest")?;
    Ok(())
}

/// Read each declared output artefact from disk (`work_dir/artefact.path`) and
/// store its content in the manifest under the key `task_id/artefact.name`.
///
/// A no-op when `outputs` is empty.
pub fn persist_output_artefacts(
    manifest_path: &Path,
    task_id: &str,
    outputs: &[TaskArtefact],
    work_dir: &Path,
) -> Result<()> {
    if outputs.is_empty() {
        return Ok(());
    }
    let mut manifest = load_manifest(manifest_path)?;
    for artefact in outputs {
        let file_path = work_dir.join(&artefact.path);
        let content = fs::read_to_string(&file_path).with_context(|| {
            format!(
                "Failed to read output artefact '{}' at {}",
                artefact.name,
                file_path.display()
            )
        })?;
        let key = ArtefactManifest::key(task_id, &artefact.name);
        manifest.artefacts.insert(
            key,
            ArtefactEntry {
                kind: artefact.kind,
                name: artefact.name.clone(),
                path: artefact.path.clone(),
                content,
            },
        );
    }
    save_manifest(manifest_path, &manifest)
}

/// Resolve a list of input artefact references (each in the form
/// `"task-id/artefact-name"`) from the manifest.
///
/// Returns a `Vec` of `(reference, content)` pairs in the same order as the
/// input slice.  Returns an error if any reference is missing from the
/// manifest.
///
/// A no-op when `inputs` is empty (returns an empty `Vec`).
pub fn resolve_input_artefacts(
    manifest_path: &Path,
    inputs: &[String],
) -> Result<Vec<(String, String)>> {
    if inputs.is_empty() {
        return Ok(Vec::new());
    }
    let manifest = load_manifest(manifest_path)?;
    let mut resolved = Vec::new();
    for input_ref in inputs {
        match manifest.artefacts.get(input_ref) {
            Some(entry) => resolved.push((input_ref.clone(), entry.content.clone())),
            None => bail!("Artefact '{}' not found in manifest", input_ref),
        }
    }
    Ok(resolved)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ArtefactKind, TaskArtefact};
    use tempfile::tempdir;

    // ---- Serialisation tests ----

    #[test]
    fn artefact_entry_roundtrip() {
        let entry = ArtefactEntry {
            kind: ArtefactKind::Json,
            name: "report".to_string(),
            path: "out/report.json".to_string(),
            content: r#"{"key":"value"}"#.to_string(),
        };
        let json = serde_json::to_string(&entry).unwrap();
        let loaded: ArtefactEntry = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded, entry);
    }

    #[test]
    fn manifest_key_format() {
        assert_eq!(ArtefactManifest::key("task-1", "summary"), "task-1/summary");
        assert_eq!(ArtefactManifest::key("impl-8", "output"), "impl-8/output");
    }

    #[test]
    fn empty_manifest_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("artefacts.json");
        let manifest = ArtefactManifest::default();
        save_manifest(&path, &manifest).unwrap();
        let loaded = load_manifest(&path).unwrap();
        assert!(loaded.artefacts.is_empty());
    }

    #[test]
    fn manifest_with_entries_roundtrip() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("artefacts.json");
        let mut manifest = ArtefactManifest::default();
        manifest.artefacts.insert(
            "task-1/report".to_string(),
            ArtefactEntry {
                kind: ArtefactKind::Summary,
                name: "report".to_string(),
                path: "docs/report.md".to_string(),
                content: "Hello world".to_string(),
            },
        );
        save_manifest(&path, &manifest).unwrap();
        let loaded = load_manifest(&path).unwrap();
        let entry = loaded.artefacts.get("task-1/report").unwrap();
        assert_eq!(entry.kind, ArtefactKind::Summary);
        assert_eq!(entry.content, "Hello world");
    }

    #[test]
    fn load_manifest_returns_default_when_file_missing() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("nonexistent.json");
        let manifest = load_manifest(&path).unwrap();
        assert!(manifest.artefacts.is_empty());
    }

    // ---- persist_output_artefacts tests ----

    #[test]
    fn persist_output_artefacts_writes_to_manifest() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");
        // Create a file that the artefact points to.
        let artefact_file = dir.path().join("result.txt");
        fs::write(&artefact_file, "task output").unwrap();

        let outputs = vec![TaskArtefact {
            kind: ArtefactKind::File,
            name: "result".to_string(),
            path: "result.txt".to_string(),
        }];

        persist_output_artefacts(&manifest_path, "task-1", &outputs, dir.path()).unwrap();

        let manifest = load_manifest(&manifest_path).unwrap();
        let entry = manifest.artefacts.get("task-1/result").unwrap();
        assert_eq!(entry.content, "task output");
        assert_eq!(entry.kind, ArtefactKind::File);
    }

    #[test]
    fn persist_output_artefacts_noop_for_empty_outputs() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");
        persist_output_artefacts(&manifest_path, "task-1", &[], dir.path()).unwrap();
        // Manifest file should not even exist.
        assert!(!manifest_path.exists());
    }

    #[test]
    fn persist_output_artefacts_error_when_file_missing() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");
        let outputs = vec![TaskArtefact {
            kind: ArtefactKind::File,
            name: "missing".to_string(),
            path: "does_not_exist.txt".to_string(),
        }];
        let result = persist_output_artefacts(&manifest_path, "task-1", &outputs, dir.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("missing"));
    }

    #[test]
    fn persist_output_artefacts_accumulates_across_tasks() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");

        fs::write(dir.path().join("a.txt"), "alpha").unwrap();
        fs::write(dir.path().join("b.txt"), "beta").unwrap();

        let outputs_a = vec![TaskArtefact {
            kind: ArtefactKind::File,
            name: "doc".to_string(),
            path: "a.txt".to_string(),
        }];
        let outputs_b = vec![TaskArtefact {
            kind: ArtefactKind::Json,
            name: "doc".to_string(),
            path: "b.txt".to_string(),
        }];

        persist_output_artefacts(&manifest_path, "task-a", &outputs_a, dir.path()).unwrap();
        persist_output_artefacts(&manifest_path, "task-b", &outputs_b, dir.path()).unwrap();

        let manifest = load_manifest(&manifest_path).unwrap();
        assert_eq!(
            manifest.artefacts.get("task-a/doc").unwrap().content,
            "alpha"
        );
        assert_eq!(
            manifest.artefacts.get("task-b/doc").unwrap().content,
            "beta"
        );
    }

    // ---- resolve_input_artefacts tests ----

    #[test]
    fn resolve_input_artefacts_returns_empty_for_no_inputs() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");
        let resolved = resolve_input_artefacts(&manifest_path, &[]).unwrap();
        assert!(resolved.is_empty());
    }

    #[test]
    fn resolve_input_artefacts_returns_content() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");

        let mut manifest = ArtefactManifest::default();
        manifest.artefacts.insert(
            "task-1/summary".to_string(),
            ArtefactEntry {
                kind: ArtefactKind::Summary,
                name: "summary".to_string(),
                path: "summary.md".to_string(),
                content: "The summary content".to_string(),
            },
        );
        save_manifest(&manifest_path, &manifest).unwrap();

        let inputs = vec!["task-1/summary".to_string()];
        let resolved = resolve_input_artefacts(&manifest_path, &inputs).unwrap();
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].0, "task-1/summary");
        assert_eq!(resolved[0].1, "The summary content");
    }

    #[test]
    fn resolve_input_artefacts_error_on_missing_artefact() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");
        let inputs = vec!["task-1/nonexistent".to_string()];
        let result = resolve_input_artefacts(&manifest_path, &inputs);
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("task-1/nonexistent"));
    }

    #[test]
    fn resolve_input_artefacts_error_on_partial_miss() {
        let dir = tempdir().unwrap();
        let manifest_path = dir.path().join("artefacts.json");
        let mut manifest = ArtefactManifest::default();
        manifest.artefacts.insert(
            "task-1/present".to_string(),
            ArtefactEntry {
                kind: ArtefactKind::File,
                name: "present".to_string(),
                path: "f.txt".to_string(),
                content: "content".to_string(),
            },
        );
        save_manifest(&manifest_path, &manifest).unwrap();

        let inputs = vec!["task-1/present".to_string(), "task-1/missing".to_string()];
        let result = resolve_input_artefacts(&manifest_path, &inputs);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("task-1/missing"));
    }
}
