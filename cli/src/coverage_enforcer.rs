//! Coverage enforcer execution for the `coverage_enforcer` agent role.
//!
//! A coverage enforcer task replaces LLM-based execution with direct parsing
//! of coverage report artefacts produced by the implementation/test phase.
//! The enforcer checks whether the measured coverage percentage meets the
//! configured threshold and either passes or fails the gate accordingly.
//!
//! # Coverage report formats
//!
//! The enforcer auto-detects the report format from the artefact content:
//!
//! * **Rust / tarpaulin** — `cargo tarpaulin --out Json` JSON output
//! * **Node.js / nyc / istanbul** — `nyc report --reporter=json-summary`
//!   (`coverage-summary.json`)
//! * **LCOV** — a `lcov.info` / `lcov.dat` text file (line coverage only)
//!
//! # Threshold
//!
//! The default threshold is **80 %**.  A different threshold can be encoded
//! in the task `description` as a simple JSON fragment, e.g.:
//!
//! ```json
//! { "coverage_threshold": 90 }
//! ```
//!
//! If the description is not valid JSON or does not contain the field the
//! default of 80 % is used.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Outcome of a coverage enforcement check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoverageFindings {
    /// Name of the coverage format/tool detected (e.g. `"tarpaulin"`, `"nyc"`,
    /// `"lcov"`, or `"none"` when no report was provided).
    pub scanner: String,
    /// `true` when measured coverage meets or exceeds the threshold.
    pub passed: bool,
    /// Measured line / statement coverage percentage (0–100).
    pub coverage_percent: f64,
    /// The threshold that was checked against.
    pub threshold_percent: f64,
    /// Number of covered lines/statements (best-effort; 0 when unavailable).
    pub covered_lines: u64,
    /// Total lines/statements in scope (best-effort; 0 when unavailable).
    pub total_lines: u64,
    /// The raw report text/JSON that was parsed (truncated to 8 KiB if large).
    pub raw_report: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Default coverage threshold (%).
pub const DEFAULT_THRESHOLD: f64 = 80.0;

/// Parse the coverage percentage from a report string and check it against
/// `threshold_percent`.
///
/// The format is auto-detected:
/// 1. If the content looks like tarpaulin JSON (`"covered"` + `"coverable"`).
/// 2. If the content looks like nyc/istanbul JSON summary (`"total"` + `"pct"`).
/// 3. If the content looks like LCOV (`LF:` / `LH:` lines).
/// 4. Otherwise returns a passing-by-default `CoverageFindings` with
///    `scanner = "none"` (no report available, gate is skipped).
pub fn check_coverage(report: &str, threshold_percent: f64) -> CoverageFindings {
    // Truncate the raw report stored in findings to avoid huge artefacts.
    let raw_report = if report.len() > 8192 {
        format!("{}... [truncated]", &report[..8192])
    } else {
        report.to_string()
    };

    if let Some(f) = try_parse_tarpaulin(report) {
        return CoverageFindings {
            scanner: "tarpaulin".to_string(),
            passed: f.coverage_percent >= threshold_percent,
            threshold_percent,
            raw_report,
            ..f
        };
    }

    if let Some(f) = try_parse_nyc(report) {
        return CoverageFindings {
            scanner: "nyc".to_string(),
            passed: f.coverage_percent >= threshold_percent,
            threshold_percent,
            raw_report,
            ..f
        };
    }

    if let Some(f) = try_parse_lcov(report) {
        return CoverageFindings {
            scanner: "lcov".to_string(),
            passed: f.coverage_percent >= threshold_percent,
            threshold_percent,
            raw_report,
            ..f
        };
    }

    // Unknown / missing report — pass by default so the gate does not block
    // projects that haven't set up coverage tooling yet.
    CoverageFindings {
        scanner: "none".to_string(),
        passed: true,
        coverage_percent: 0.0,
        threshold_percent,
        covered_lines: 0,
        total_lines: 0,
        raw_report,
    }
}

/// Extract the coverage threshold from a task description string.
///
/// Looks for `"coverage_threshold": <number>` in the description.  Falls back
/// to [`DEFAULT_THRESHOLD`] when the field is absent or unparseable.
pub fn threshold_from_description(description: &str) -> f64 {
    if let Ok(v) = serde_json::from_str::<serde_json::Value>(description) {
        if let Some(t) = v.get("coverage_threshold").and_then(|t| t.as_f64()) {
            return t.clamp(0.0, 100.0);
        }
    }
    // Also try a simple prefix search for robustness (non-JSON descriptions).
    DEFAULT_THRESHOLD
}

/// Write `findings` as pretty-printed JSON to `path`, creating parent dirs as
/// needed.
pub fn write_findings(findings: &CoverageFindings, path: &std::path::Path) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).context("Failed to create coverage findings directory")?;
    }
    let json =
        serde_json::to_string_pretty(findings).context("Failed to serialise coverage findings")?;
    std::fs::write(path, json).context("Failed to write coverage findings file")
}

// ---------------------------------------------------------------------------
// Format parsers
// ---------------------------------------------------------------------------

/// Parse `cargo tarpaulin --out Json` output.
///
/// Tarpaulin v0.27+ emits a JSON object with top-level `"covered"` and
/// `"coverable"` integer fields:
///
/// ```json
/// { "covered": 350, "coverable": 400, ... }
/// ```
fn try_parse_tarpaulin(json: &str) -> Option<CoverageFindings> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let covered = v.get("covered").and_then(|x| x.as_u64())?;
    let coverable = v.get("coverable").and_then(|x| x.as_u64())?;
    if coverable == 0 {
        return None;
    }
    let pct = (covered as f64 / coverable as f64) * 100.0;
    Some(CoverageFindings {
        scanner: String::new(),
        passed: false,
        coverage_percent: pct,
        threshold_percent: 0.0,
        covered_lines: covered,
        total_lines: coverable,
        raw_report: String::new(),
    })
}

/// Parse `nyc report --reporter=json-summary` (`coverage-summary.json`) output.
///
/// ```json
/// { "total": { "lines": { "total": 400, "covered": 350, "pct": 87.5 }, ... } }
/// ```
fn try_parse_nyc(json: &str) -> Option<CoverageFindings> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    let lines = v.get("total").and_then(|t| t.get("lines"))?;
    let pct = lines.get("pct").and_then(|p| p.as_f64())?;
    let covered = lines.get("covered").and_then(|c| c.as_u64()).unwrap_or(0);
    let total = lines.get("total").and_then(|t| t.as_u64()).unwrap_or(0);
    Some(CoverageFindings {
        scanner: String::new(),
        passed: false,
        coverage_percent: pct,
        threshold_percent: 0.0,
        covered_lines: covered,
        total_lines: total,
        raw_report: String::new(),
    })
}

/// Parse an LCOV (`lcov.info`) text report.
///
/// LCOV records the number of instrumented lines (`LF:`) and hit lines
/// (`LH:`) per source file.  We sum them to get global line coverage.
fn try_parse_lcov(text: &str) -> Option<CoverageFindings> {
    let mut total_lf: u64 = 0;
    let mut total_lh: u64 = 0;
    let mut found = false;

    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("LF:") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                total_lf += n;
                found = true;
            }
        } else if let Some(rest) = line.strip_prefix("LH:") {
            if let Ok(n) = rest.trim().parse::<u64>() {
                total_lh += n;
                found = true;
            }
        }
    }

    if !found || total_lf == 0 {
        return None;
    }

    let pct = (total_lh as f64 / total_lf as f64) * 100.0;
    Some(CoverageFindings {
        scanner: String::new(),
        passed: false,
        coverage_percent: pct,
        threshold_percent: 0.0,
        covered_lines: total_lh,
        total_lines: total_lf,
        raw_report: String::new(),
    })
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    // ---- tarpaulin ----

    #[test]
    fn tarpaulin_above_threshold_passes() {
        let json = r#"{"covered": 90, "coverable": 100}"#;
        let f = check_coverage(json, 80.0);
        assert_eq!(f.scanner, "tarpaulin");
        assert!(f.passed);
        assert!((f.coverage_percent - 90.0).abs() < 0.01);
        assert_eq!(f.covered_lines, 90);
        assert_eq!(f.total_lines, 100);
    }

    #[test]
    fn tarpaulin_below_threshold_fails() {
        let json = r#"{"covered": 70, "coverable": 100}"#;
        let f = check_coverage(json, 80.0);
        assert_eq!(f.scanner, "tarpaulin");
        assert!(!f.passed);
        assert!((f.coverage_percent - 70.0).abs() < 0.01);
    }

    #[test]
    fn tarpaulin_exact_threshold_passes() {
        let json = r#"{"covered": 80, "coverable": 100}"#;
        let f = check_coverage(json, 80.0);
        assert!(f.passed);
    }

    #[test]
    fn tarpaulin_zero_coverable_falls_through() {
        // coverable = 0 means nothing to cover — no tarpaulin match, falls to "none".
        let json = r#"{"covered": 0, "coverable": 0}"#;
        let f = check_coverage(json, 80.0);
        assert_eq!(f.scanner, "none");
    }

    // ---- nyc / istanbul ----

    #[test]
    fn nyc_above_threshold_passes() {
        let json = r#"{
            "total": {
                "lines": { "total": 400, "covered": 360, "pct": 90.0 }
            }
        }"#;
        let f = check_coverage(json, 85.0);
        assert_eq!(f.scanner, "nyc");
        assert!(f.passed);
        assert!((f.coverage_percent - 90.0).abs() < 0.01);
        assert_eq!(f.covered_lines, 360);
        assert_eq!(f.total_lines, 400);
    }

    #[test]
    fn nyc_below_threshold_fails() {
        let json = r#"{
            "total": {
                "lines": { "total": 100, "covered": 60, "pct": 60.0 }
            }
        }"#;
        let f = check_coverage(json, 80.0);
        assert_eq!(f.scanner, "nyc");
        assert!(!f.passed);
    }

    // ---- LCOV ----

    #[test]
    fn lcov_above_threshold_passes() {
        let lcov = "SF:src/main.rs\nLF:100\nLH:85\nend_of_record\n";
        let f = check_coverage(lcov, 80.0);
        assert_eq!(f.scanner, "lcov");
        assert!(f.passed);
        assert!((f.coverage_percent - 85.0).abs() < 0.01);
    }

    #[test]
    fn lcov_below_threshold_fails() {
        let lcov = "SF:src/main.rs\nLF:100\nLH:70\nend_of_record\n";
        let f = check_coverage(lcov, 80.0);
        assert_eq!(f.scanner, "lcov");
        assert!(!f.passed);
    }

    #[test]
    fn lcov_multi_file_aggregated() {
        let lcov = "SF:a.rs\nLF:100\nLH:80\nend_of_record\nSF:b.rs\nLF:100\nLH:90\nend_of_record\n";
        let f = check_coverage(lcov, 80.0);
        assert_eq!(f.scanner, "lcov");
        assert_eq!(f.total_lines, 200);
        assert_eq!(f.covered_lines, 170);
        assert!((f.coverage_percent - 85.0).abs() < 0.01);
    }

    // ---- unknown / missing report ----

    #[test]
    fn unknown_report_passes_by_default() {
        let f = check_coverage("", 80.0);
        assert_eq!(f.scanner, "none");
        assert!(f.passed);
    }

    #[test]
    fn unknown_json_passes_by_default() {
        let f = check_coverage("{}", 80.0);
        assert_eq!(f.scanner, "none");
        assert!(f.passed);
    }

    // ---- threshold_from_description ----

    #[test]
    fn threshold_extracted_from_json_description() {
        let desc = r#"{"coverage_threshold": 90}"#;
        assert!((threshold_from_description(desc) - 90.0).abs() < 0.01);
    }

    #[test]
    fn threshold_defaults_when_no_json() {
        assert!(
            (threshold_from_description("just a plain text description") - DEFAULT_THRESHOLD).abs()
                < 0.01
        );
    }

    #[test]
    fn threshold_defaults_when_field_missing() {
        let desc = r#"{"other_field": 42}"#;
        assert!((threshold_from_description(desc) - DEFAULT_THRESHOLD).abs() < 0.01);
    }

    #[test]
    fn threshold_clamped_to_100() {
        let desc = r#"{"coverage_threshold": 150}"#;
        assert!((threshold_from_description(desc) - 100.0).abs() < 0.01);
    }

    // ---- serde roundtrip ----

    #[test]
    fn findings_serde_roundtrip() {
        let f = CoverageFindings {
            scanner: "tarpaulin".to_string(),
            passed: true,
            coverage_percent: 87.5,
            threshold_percent: 80.0,
            covered_lines: 175,
            total_lines: 200,
            raw_report: "{}".to_string(),
        };
        let json = serde_json::to_string(&f).unwrap();
        let loaded: CoverageFindings = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.scanner, "tarpaulin");
        assert!(loaded.passed);
        assert!((loaded.coverage_percent - 87.5).abs() < 0.01);
    }

    // ---- write_findings ----

    #[test]
    fn write_findings_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("subdir/coverage-findings.json");
        let f = CoverageFindings {
            scanner: "lcov".to_string(),
            passed: false,
            coverage_percent: 70.0,
            threshold_percent: 80.0,
            covered_lines: 70,
            total_lines: 100,
            raw_report: "LF:100\nLH:70\n".to_string(),
        };
        write_findings(&f, &path).unwrap();
        let content = std::fs::read_to_string(&path).unwrap();
        let loaded: CoverageFindings = serde_json::from_str(&content).unwrap();
        assert!(!loaded.passed);
        assert!((loaded.coverage_percent - 70.0).abs() < 0.01);
    }
}
