//! Interface-change detector – parse a unified diff and identify additions,
//! removals, and signature modifications of public symbols across multiple
//! languages (Rust, TypeScript/JavaScript, Go, Python).
//!
//! The primary entry point is [`detect_interface_changes`], which accepts the
//! output of `git diff` and returns a [`Vec<InterfaceChange>`].  Each entry
//! describes one changed symbol together with a human-readable description
//! suitable for injecting into a downstream task.

use std::collections::HashMap;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// The kind of change that occurred to a public symbol.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChangeKind {
    /// A new public symbol was added (non-breaking).
    Added,
    /// An existing public symbol was removed (breaking).
    Removed,
    /// The symbol still exists but its signature changed (breaking).
    Modified,
}

/// A single change to a public symbol detected in a git diff.
#[derive(Debug, Clone)]
pub struct InterfaceChange {
    /// Name of the symbol (function, struct, class, …).
    pub symbol: String,
    /// Whether it was added, removed, or modified.
    pub kind: ChangeKind,
    /// Human-readable description of the change, suitable for injecting into a
    /// task description.
    pub description: String,
}

impl InterfaceChange {
    /// Returns `true` when this change may break existing consumers.
    ///
    /// Removals and signature modifications are breaking; additions are not.
    pub fn is_breaking(&self) -> bool {
        matches!(self.kind, ChangeKind::Removed | ChangeKind::Modified)
    }
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Analyse a unified diff and return all detected interface changes.
///
/// The function is language-agnostic: it inspects every file section in the
/// diff and dispatches to the appropriate extractor based on the file extension.
pub fn detect_interface_changes(diff: &str) -> Vec<InterfaceChange> {
    let sections = split_diff_by_file(diff);
    let mut changes = Vec::new();

    for (filename, section) in &sections {
        let removed = extract_public_symbols(section, '-', filename);
        let added = extract_public_symbols(section, '+', filename);

        // Symbols added but not in removed → pure addition (non-breaking).
        for sym in added.keys() {
            if !removed.contains_key(sym) {
                changes.push(InterfaceChange {
                    symbol: sym.clone(),
                    kind: ChangeKind::Added,
                    description: format!("New public symbol `{sym}` added in `{filename}`"),
                });
            }
        }

        // Symbols removed but not in added → removal (breaking).
        for (sym, sig) in &removed {
            if !added.contains_key(sym) {
                changes.push(InterfaceChange {
                    symbol: sym.clone(),
                    kind: ChangeKind::Removed,
                    description: format!(
                        "Public symbol `{sym}` removed from `{filename}` \
                         (was: `{}`)",
                        sig.trim()
                    ),
                });
            }
        }

        // Present in both but signatures differ → modification (breaking).
        for (sym, old_sig) in &removed {
            if let Some(new_sig) = added.get(sym) {
                if old_sig != new_sig {
                    changes.push(InterfaceChange {
                        symbol: sym.clone(),
                        kind: ChangeKind::Modified,
                        description: format!(
                            "Signature of `{sym}` in `{filename}` changed:\n  \
                             before: `{}`\n  after:  `{}`",
                            old_sig.trim(),
                            new_sig.trim(),
                        ),
                    });
                }
            }
        }
    }

    changes
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Split a unified diff into per-file sections.
///
/// Returns a `Vec<(filename, diff_section)>` preserving order.
fn split_diff_by_file(diff: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, String)> = Vec::new();
    let mut current_file = String::new();
    let mut current_lines: Vec<&str> = Vec::new();

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            if !current_file.is_empty() {
                sections.push((current_file.clone(), current_lines.join("\n")));
                current_lines.clear();
            }
            // Extract "b/<path>" from "diff --git a/<path> b/<path>"
            if let Some(b_part) = line.split(" b/").nth(1) {
                current_file = b_part.to_string();
            }
        }
        if !current_file.is_empty() {
            current_lines.push(line);
        }
    }

    if !current_file.is_empty() {
        sections.push((current_file, current_lines.join("\n")));
    }

    sections
}

/// Extract the set of public symbol names (and their signatures) from lines
/// that begin with `marker` ('+' or '-') in `diff_section`.
///
/// Returns `HashMap<symbol_name, signature_line>`.
fn extract_public_symbols(
    diff_section: &str,
    marker: char,
    filename: &str,
) -> HashMap<String, String> {
    let ext = std::path::Path::new(filename)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("");

    let mut result = HashMap::new();

    for line in diff_section.lines() {
        // Only look at added/removed lines (not context lines).
        let first = line.chars().next().unwrap_or(' ');
        if first != marker {
            continue;
        }
        // Strip the diff marker character.
        let content = &line[1..];
        if let Some((sym, sig)) = extract_symbol(content, ext) {
            result.entry(sym).or_insert(sig);
        }
    }

    result
}

/// Try to extract a public symbol name and its signature from a single source
/// line.  Returns `None` when the line does not declare a public symbol.
fn extract_symbol(line: &str, ext: &str) -> Option<(String, String)> {
    let trimmed = line.trim();
    match ext {
        "rs" => extract_rust_symbol(trimmed),
        "ts" | "tsx" | "js" | "jsx" | "mjs" | "cjs" => extract_ts_symbol(trimmed),
        "go" => extract_go_symbol(trimmed),
        "py" => extract_python_symbol(trimmed, line),
        _ => None,
    }
}

// -- Rust --------------------------------------------------------------------

fn extract_rust_symbol(line: &str) -> Option<(String, String)> {
    // Must start with "pub" (handles "pub fn", "pub(crate) fn", etc.)
    if !line.starts_with("pub") {
        return None;
    }

    // Strip "pub" and optional visibility qualifier like pub(crate) / pub(super).
    let after_pub = &line[3..];
    let after_pub = if after_pub.starts_with('(') {
        // Skip over the parenthesised visibility group, e.g. "(crate)".
        let close = after_pub.find(')')?;
        after_pub[close + 1..].trim_start()
    } else if after_pub.starts_with(' ') {
        after_pub.trim_start()
    } else {
        // Not "pub " or "pub(" – not a visibility keyword.
        return None;
    };

    let keywords = [
        "fn ", "struct ", "enum ", "trait ", "type ", "const ", "static ",
    ];
    for kw in &keywords {
        if let Some(rest) = after_pub.strip_prefix(kw) {
            let name = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            if !name.is_empty() {
                return Some((name.to_string(), line.to_string()));
            }
        }
    }
    None
}

// -- TypeScript / JavaScript -------------------------------------------------

fn extract_ts_symbol(line: &str) -> Option<(String, String)> {
    if !line.starts_with("export ") {
        return None;
    }
    let rest = line[7..].trim_start();

    // Strip optional "default" and "async" keywords.
    let rest = rest.strip_prefix("default ").unwrap_or(rest).trim_start();
    let rest = rest.strip_prefix("async ").unwrap_or(rest).trim_start();

    let keywords = [
        "function ",
        "const ",
        "let ",
        "var ",
        "class ",
        "interface ",
        "type ",
        "enum ",
        "abstract class ",
    ];
    for kw in &keywords {
        if let Some(after_kw) = rest.strip_prefix(kw) {
            let name = after_kw
                .split(|c: char| !c.is_alphanumeric() && c != '_' && c != '$')
                .next()?;
            if !name.is_empty() {
                return Some((name.to_string(), line.to_string()));
            }
        }
    }
    None
}

// -- Go ----------------------------------------------------------------------

fn extract_go_symbol(line: &str) -> Option<(String, String)> {
    // Top-level exported declarations (no leading whitespace):
    //   func Foo(…)  /  type Foo …  /  const Foo …  /  var Foo …
    for kw in &["func ", "type ", "const ", "var "] {
        if let Some(rest) = line.strip_prefix(kw) {
            let name = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            // Exported Go symbols start with an uppercase letter.
            if name.chars().next().is_some_and(|c| c.is_uppercase()) {
                return Some((name.to_string(), line.to_string()));
            }
        }
    }
    None
}

// -- Python ------------------------------------------------------------------

fn extract_python_symbol(trimmed: &str, original_line: &str) -> Option<(String, String)> {
    // Only consider top-level declarations (no indentation).
    if original_line.starts_with(' ') || original_line.starts_with('\t') {
        return None;
    }

    for kw in &["def ", "async def ", "class "] {
        if let Some(rest) = trimmed.strip_prefix(kw) {
            let name = rest
                .split(|c: char| !c.is_alphanumeric() && c != '_')
                .next()?;
            // Skip private/internal symbols (leading underscore convention).
            if !name.starts_with('_') && !name.is_empty() {
                return Some((name.to_string(), trimmed.to_string()));
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- extract_rust_symbol -----------------------------------------------

    #[test]
    fn rust_pub_fn_detected() {
        let (name, _) = extract_rust_symbol("pub fn do_thing(x: u32) -> u32").unwrap();
        assert_eq!(name, "do_thing");
    }

    #[test]
    fn rust_pub_struct_detected() {
        let (name, _) = extract_rust_symbol("pub struct MyConfig {").unwrap();
        assert_eq!(name, "MyConfig");
    }

    #[test]
    fn rust_pub_enum_detected() {
        let (name, _) = extract_rust_symbol("pub enum Status {").unwrap();
        assert_eq!(name, "Status");
    }

    #[test]
    fn rust_pub_trait_detected() {
        let (name, _) = extract_rust_symbol("pub trait Provider {").unwrap();
        assert_eq!(name, "Provider");
    }

    #[test]
    fn rust_pub_type_alias_detected() {
        let (name, _) =
            extract_rust_symbol("pub type Result<T> = std::result::Result<T, Error>;").unwrap();
        assert_eq!(name, "Result");
    }

    #[test]
    fn rust_private_fn_ignored() {
        assert!(extract_rust_symbol("fn private_fn()").is_none());
    }

    #[test]
    fn rust_pub_crate_fn_detected() {
        let (name, _) = extract_rust_symbol("pub(crate) fn internal_fn() -> bool").unwrap();
        assert_eq!(name, "internal_fn");
    }

    // ---- extract_ts_symbol -------------------------------------------------

    #[test]
    fn ts_export_function_detected() {
        let (name, _) = extract_ts_symbol("export function fetchUser(id: string): User").unwrap();
        assert_eq!(name, "fetchUser");
    }

    #[test]
    fn ts_export_const_detected() {
        let (name, _) = extract_ts_symbol("export const DEFAULT_TIMEOUT = 5000;").unwrap();
        assert_eq!(name, "DEFAULT_TIMEOUT");
    }

    #[test]
    fn ts_export_interface_detected() {
        let (name, _) = extract_ts_symbol("export interface UserConfig {").unwrap();
        assert_eq!(name, "UserConfig");
    }

    #[test]
    fn ts_export_class_detected() {
        let (name, _) = extract_ts_symbol("export class ApiClient {").unwrap();
        assert_eq!(name, "ApiClient");
    }

    #[test]
    fn ts_non_export_ignored() {
        assert!(extract_ts_symbol("function internalHelper() {}").is_none());
    }

    // ---- extract_go_symbol -------------------------------------------------

    #[test]
    fn go_exported_func_detected() {
        let (name, _) = extract_go_symbol("func NewClient(addr string) *Client").unwrap();
        assert_eq!(name, "NewClient");
    }

    #[test]
    fn go_unexported_func_ignored() {
        assert!(extract_go_symbol("func internalHelper()").is_none());
    }

    // ---- extract_python_symbol ---------------------------------------------

    #[test]
    fn python_def_detected() {
        let line = "def process_data(input: str) -> str:";
        let (name, _) = extract_python_symbol(line, line).unwrap();
        assert_eq!(name, "process_data");
    }

    #[test]
    fn python_private_ignored() {
        let line = "def _helper():";
        assert!(extract_python_symbol(line, line).is_none());
    }

    #[test]
    fn python_indented_ignored() {
        let line = "    def method(self):";
        assert!(extract_python_symbol(line.trim(), line).is_none());
    }

    // ---- detect_interface_changes ------------------------------------------

    #[test]
    fn detects_rust_symbol_removal() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,5 +1,4 @@
-pub fn old_api(x: u32) -> u32 {
-    x * 2
-}
 pub fn new_api(y: u32) -> u32 {
     y + 1
 }
";
        let changes = detect_interface_changes(diff);
        let removed: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Removed)
            .collect();
        assert_eq!(removed.len(), 1);
        assert_eq!(removed[0].symbol, "old_api");
        assert!(removed[0].is_breaking());
    }

    #[test]
    fn detects_rust_symbol_addition() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,6 @@
+pub fn new_fn(z: u32) -> bool {
+    z > 0
+}
 pub fn existing() {}
";
        let changes = detect_interface_changes(diff);
        let added: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Added)
            .collect();
        assert_eq!(added.len(), 1);
        assert_eq!(added[0].symbol, "new_fn");
        assert!(!added[0].is_breaking());
    }

    #[test]
    fn detects_rust_signature_modification() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
-pub fn compute(x: u32) -> u32 {
+pub fn compute(x: u64) -> u64 {
";
        let changes = detect_interface_changes(diff);
        let modified: Vec<_> = changes
            .iter()
            .filter(|c| c.kind == ChangeKind::Modified)
            .collect();
        assert_eq!(modified.len(), 1);
        assert_eq!(modified[0].symbol, "compute");
        assert!(modified[0].is_breaking());
    }

    #[test]
    fn no_changes_for_empty_diff() {
        let changes = detect_interface_changes("");
        assert!(changes.is_empty());
    }

    #[test]
    fn no_false_positives_for_private_symbols() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,3 +1,3 @@
-fn private_fn() {}
+fn other_private() {}
";
        let changes = detect_interface_changes(diff);
        assert!(changes.is_empty());
    }

    #[test]
    fn breaking_change_predicate() {
        let removed = InterfaceChange {
            symbol: "foo".into(),
            kind: ChangeKind::Removed,
            description: String::new(),
        };
        let added = InterfaceChange {
            symbol: "bar".into(),
            kind: ChangeKind::Added,
            description: String::new(),
        };
        let modified = InterfaceChange {
            symbol: "baz".into(),
            kind: ChangeKind::Modified,
            description: String::new(),
        };
        assert!(removed.is_breaking());
        assert!(!added.is_breaking());
        assert!(modified.is_breaking());
    }
}
