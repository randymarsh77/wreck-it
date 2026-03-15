//! Security gate execution for the `security_gate` agent role.
//!
//! A security gate task replaces LLM-based execution with a direct invocation
//! of an appropriate security scanner for the project type:
//!
//! * **Rust** (`Cargo.toml` present) → `cargo audit --json`
//! * **Node.js** (`package.json` present) → `npm audit --json`
//!
//! The scan findings are serialised to JSON and written to the path declared
//! in the task's first output artefact (defaulting to
//! `.wreck-it/security-findings.json` when no outputs are declared).  The
//! artefact is persisted to the manifest even when the gate fails so that
//! downstream implementation tasks can consume the findings and self-remediate.
//!
//! The task **fails** (returns `Err`) when one or more critical or high
//! severity vulnerabilities are found.  Medium and low findings are reported
//! but do not block the gate.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::process::Command;

// ---------------------------------------------------------------------------
// Public types
// ---------------------------------------------------------------------------

/// Aggregated findings produced by a security audit scan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SecurityGateFindings {
    /// Name of the scanner that produced these findings.
    pub scanner: String,
    /// `true` when no blocking (critical or high) vulnerabilities were found.
    pub passed: bool,
    pub critical: u32,
    pub high: u32,
    pub medium: u32,
    pub low: u32,
    /// Total number of vulnerabilities across all severities.
    pub total: u32,
    /// Raw text/JSON output captured from the scanner.
    pub raw_output: String,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Detect the project type in `work_dir` and run the appropriate security
/// scanner.
///
/// Returns `Ok(findings)` with `findings.passed = false` when blocking
/// vulnerabilities are found.  Returns `Err` only when the scanner could not
/// be executed (e.g. tool not installed, I/O error).
///
/// When no recognised project file is found the function returns a trivially
/// passing result so that unknown project types are not blocked.
pub fn run_security_scan(work_dir: &Path) -> Result<SecurityGateFindings> {
    if work_dir.join("Cargo.toml").exists() {
        run_cargo_audit(work_dir)
    } else if work_dir.join("package.json").exists() {
        run_npm_audit(work_dir)
    } else {
        Ok(SecurityGateFindings {
            scanner: "none".to_string(),
            passed: true,
            critical: 0,
            high: 0,
            medium: 0,
            low: 0,
            total: 0,
            raw_output: "No supported project file found (Cargo.toml / package.json); \
                         security scan skipped."
                .to_string(),
        })
    }
}

// ---------------------------------------------------------------------------
// Scanner implementations
// ---------------------------------------------------------------------------

fn run_cargo_audit(work_dir: &Path) -> Result<SecurityGateFindings> {
    let output = Command::new("cargo")
        .args(["audit", "--json"])
        .current_dir(work_dir)
        .output()
        .context("Failed to run `cargo audit` — install it with: cargo install cargo-audit")?;

    let raw_output = String::from_utf8_lossy(&output.stdout).to_string();
    let counts = parse_cargo_audit_counts(&raw_output);
    // cargo audit exits with a non-zero status when vulnerabilities are found.
    // Belt-and-suspenders: treat either a non-zero exit code OR parsed
    // critical/high counts as a failing gate.
    let passed = output.status.success() && counts.critical == 0 && counts.high == 0;

    Ok(SecurityGateFindings {
        scanner: "cargo-audit".to_string(),
        passed,
        critical: counts.critical,
        high: counts.high,
        medium: counts.medium,
        low: counts.low,
        total: counts.total,
        raw_output,
    })
}

fn run_npm_audit(work_dir: &Path) -> Result<SecurityGateFindings> {
    let output = Command::new("npm")
        .args(["audit", "--json"])
        .current_dir(work_dir)
        .output()
        .context("Failed to run `npm audit`")?;

    let raw_output = String::from_utf8_lossy(&output.stdout).to_string();
    let counts = parse_npm_audit_counts(&raw_output);
    let passed = counts.critical == 0 && counts.high == 0;

    Ok(SecurityGateFindings {
        scanner: "npm-audit".to_string(),
        passed,
        critical: counts.critical,
        high: counts.high,
        medium: counts.medium,
        low: counts.low,
        total: counts.total,
        raw_output,
    })
}

// ---------------------------------------------------------------------------
// Output parsing helpers
// ---------------------------------------------------------------------------

struct VulnCounts {
    critical: u32,
    high: u32,
    medium: u32,
    low: u32,
    total: u32,
}

impl VulnCounts {
    fn zero() -> Self {
        Self {
            critical: 0,
            high: 0,
            medium: 0,
            low: 0,
            total: 0,
        }
    }
}

/// Parse `cargo audit --json` output.
///
/// Expected shape:
/// ```json
/// {
///   "vulnerabilities": {
///     "found": true,
///     "count": 2,
///     "list": [
///       {
///         "advisory": {
///           "id": "RUSTSEC-...",
///           "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
///           ...
///         },
///         "package": { "name": "..." },
///         ...
///       }
///     ]
///   }
/// }
/// ```
fn parse_cargo_audit_counts(json: &str) -> VulnCounts {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return VulnCounts::zero(),
    };

    let list = match v
        .get("vulnerabilities")
        .and_then(|v| v.get("list"))
        .and_then(|l| l.as_array())
    {
        Some(l) => l,
        None => return VulnCounts::zero(),
    };

    let mut counts = VulnCounts {
        total: list.len() as u32,
        ..VulnCounts::zero()
    };

    for vuln in list {
        let cvss = vuln
            .get("advisory")
            .and_then(|a| a.get("cvss"))
            .and_then(|c| c.as_str())
            .unwrap_or("");
        match cvss_severity(cvss) {
            Severity::Critical => counts.critical += 1,
            Severity::High => counts.high += 1,
            Severity::Medium => counts.medium += 1,
            Severity::Low => counts.low += 1,
        }
    }

    counts
}

/// Parse `npm audit --json` output (npm v7+).
///
/// Expected shape:
/// ```json
/// {
///   "metadata": {
///     "vulnerabilities": {
///       "info": 0, "low": 0, "moderate": 0,
///       "high": 0, "critical": 0, "total": 0
///     }
///   }
/// }
/// ```
fn parse_npm_audit_counts(json: &str) -> VulnCounts {
    let v: serde_json::Value = match serde_json::from_str(json) {
        Ok(v) => v,
        Err(_) => return VulnCounts::zero(),
    };

    let meta = match v.get("metadata").and_then(|m| m.get("vulnerabilities")) {
        Some(m) => m,
        None => return VulnCounts::zero(),
    };

    let get_u32 = |key: &str| meta.get(key).and_then(|v| v.as_u64()).unwrap_or(0) as u32;
    VulnCounts {
        critical: get_u32("critical"),
        high: get_u32("high"),
        medium: get_u32("moderate"),
        low: get_u32("low"),
        total: get_u32("total"),
    }
}

// ---------------------------------------------------------------------------
// CVSS severity helper
// ---------------------------------------------------------------------------

enum Severity {
    Critical,
    High,
    Medium,
    Low,
}

/// Derive a coarse severity from a CVSS v3 vector string.
///
/// The CVSS base score components used here are the three impact sub-scores
/// (Confidentiality, Integrity, Availability).  When no CVSS string is
/// present the vulnerability is conservatively classified as `High`.
///
/// Components are always preceded by `/` in the CVSS vector (e.g. `/C:H`),
/// which prevents false matches such as `AC:H` (Attack Complexity) being
/// mistaken for `C:H` (Confidentiality: High).
fn cvss_severity(cvss: &str) -> Severity {
    if cvss.is_empty() {
        return Severity::High;
    }
    let c_high = cvss.contains("/C:H");
    let i_high = cvss.contains("/I:H");
    let a_high = cvss.contains("/A:H");
    let c_low = cvss.contains("/C:L");
    let i_low = cvss.contains("/I:L");
    let a_low = cvss.contains("/A:L");
    let any_high = c_high || i_high || a_high;
    let any_impact = any_high || c_low || i_low || a_low;

    if c_high && i_high {
        Severity::Critical
    } else if any_high {
        Severity::High
    } else if any_impact {
        Severity::Medium
    } else {
        Severity::Low
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_cargo_audit_empty_json_returns_zero_counts() {
        let counts = parse_cargo_audit_counts("{}");
        assert_eq!(counts.critical, 0);
        assert_eq!(counts.high, 0);
        assert_eq!(counts.total, 0);
    }

    #[test]
    fn parse_cargo_audit_invalid_json_returns_zero_counts() {
        let counts = parse_cargo_audit_counts("not json at all");
        assert_eq!(counts.total, 0);
    }

    #[test]
    fn parse_cargo_audit_counts_vulnerabilities() {
        // Minimal cargo-audit --json output with two vulnerabilities.
        let json = r#"{
            "vulnerabilities": {
                "found": true,
                "count": 2,
                "list": [
                    {
                        "advisory": {
                            "id": "RUSTSEC-2024-0001",
                            "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H",
                            "title": "Critical vuln",
                            "description": "desc"
                        }
                    },
                    {
                        "advisory": {
                            "id": "RUSTSEC-2024-0002",
                            "cvss": "CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:L/I:L/A:N",
                            "title": "Medium vuln",
                            "description": "desc"
                        }
                    }
                ]
            }
        }"#;
        let counts = parse_cargo_audit_counts(json);
        assert_eq!(counts.critical, 1);
        assert_eq!(counts.medium, 1);
        assert_eq!(counts.total, 2);
    }

    #[test]
    fn parse_npm_audit_counts_from_metadata() {
        let json = r#"{
            "auditReportVersion": 2,
            "metadata": {
                "vulnerabilities": {
                    "info": 0,
                    "low": 1,
                    "moderate": 2,
                    "high": 3,
                    "critical": 1,
                    "total": 7
                }
            }
        }"#;
        let counts = parse_npm_audit_counts(json);
        assert_eq!(counts.critical, 1);
        assert_eq!(counts.high, 3);
        assert_eq!(counts.medium, 2);
        assert_eq!(counts.low, 1);
        assert_eq!(counts.total, 7);
    }

    #[test]
    fn parse_npm_audit_empty_json_returns_zero_counts() {
        let counts = parse_npm_audit_counts("{}");
        assert_eq!(counts.critical, 0);
        assert_eq!(counts.total, 0);
    }

    #[test]
    fn no_project_file_returns_passing_findings() {
        let dir = tempfile::tempdir().unwrap();
        let findings = run_security_scan(dir.path()).unwrap();
        assert!(findings.passed);
        assert_eq!(findings.scanner, "none");
        assert_eq!(findings.total, 0);
    }

    #[test]
    fn cvss_empty_string_is_high() {
        assert!(matches!(cvss_severity(""), Severity::High));
    }

    #[test]
    fn cvss_critical_when_confidentiality_and_integrity_both_high() {
        assert!(matches!(
            cvss_severity("CVSS:3.1/AV:N/AC:L/PR:N/UI:N/S:U/C:H/I:H/A:H"),
            Severity::Critical
        ));
    }

    #[test]
    fn cvss_high_when_only_availability_high() {
        assert!(matches!(
            cvss_severity("CVSS:3.1/AV:L/AC:L/PR:N/UI:N/S:U/C:N/I:N/A:H"),
            Severity::High
        ));
    }

    #[test]
    fn cvss_medium_when_only_low_impacts() {
        assert!(matches!(
            cvss_severity("CVSS:3.1/AV:N/AC:H/PR:L/UI:R/S:U/C:L/I:L/A:N"),
            Severity::Medium
        ));
    }
}
