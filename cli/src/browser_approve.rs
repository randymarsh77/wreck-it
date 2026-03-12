//! Playwright-based fallback for approving GitHub Actions workflow runs.
//!
//! When the REST API `/approve` endpoint fails (a known issue), this module
//! provides an alternative approach: UI automation through Playwright.  A
//! headless Chromium browser signs into GitHub, navigates to each pending
//! workflow-run page, and clicks the **Approve and run** button.
//!
//! # Required environment variables
//!
//! | Variable              | Required | Description                            |
//! |-----------------------|----------|----------------------------------------|
//! | `GITHUB_USERNAME`     | Yes      | GitHub username for sign-in            |
//! | `GITHUB_PASSWORD`     | Yes      | GitHub password for sign-in            |
//! | `GITHUB_TOTP_SECRET`  | No       | Base-32 TOTP secret for two-factor auth|
//!
//! # Prerequisites
//!
//! * Node.js (≥ 18) must be available on `$PATH`.
//! * The `playwright` npm package must be installed, along with at least one
//!   browser (`npx playwright install chromium`).

use anyhow::{bail, Context, Result};
use std::io::Write;

/// Environment variable names for browser-based GitHub authentication.
const ENV_GITHUB_USERNAME: &str = "GITHUB_USERNAME";
const ENV_GITHUB_PASSWORD: &str = "GITHUB_PASSWORD";
const ENV_GITHUB_TOTP_SECRET: &str = "GITHUB_TOTP_SECRET";

/// Attempt to approve workflow runs via Playwright browser automation.
///
/// Returns the number of runs that were successfully approved.  When the
/// required credentials are not configured or Playwright is unavailable the
/// function returns an error so the caller can log and continue.
pub async fn approve_workflow_runs_via_browser(
    repo_owner: &str,
    repo_name: &str,
    run_ids: &[u64],
) -> Result<usize> {
    if run_ids.is_empty() {
        return Ok(0);
    }

    let username = std::env::var(ENV_GITHUB_USERNAME).context(
        "GITHUB_USERNAME not set — browser-based workflow approval requires GitHub credentials",
    )?;
    let password = std::env::var(ENV_GITHUB_PASSWORD).context(
        "GITHUB_PASSWORD not set — browser-based workflow approval requires GitHub credentials",
    )?;
    let totp_secret = std::env::var(ENV_GITHUB_TOTP_SECRET).ok();

    // Write the Playwright script to a temporary file.
    let script_path = std::env::temp_dir().join("wreck_it_playwright_approve.mjs");
    {
        let mut f = std::fs::File::create(&script_path)
            .context("Failed to create temporary Playwright script")?;
        f.write_all(PLAYWRIGHT_SCRIPT.as_bytes())
            .context("Failed to write Playwright script")?;
    }

    let run_ids_csv: String = run_ids
        .iter()
        .map(|id| id.to_string())
        .collect::<Vec<_>>()
        .join(",");

    let output = tokio::process::Command::new("node")
        .arg(&script_path)
        .env("GITHUB_USERNAME", &username)
        .env("GITHUB_PASSWORD", &password)
        .env("GITHUB_TOTP_SECRET", totp_secret.as_deref().unwrap_or(""))
        .env("REPO_OWNER", repo_owner)
        .env("REPO_NAME", repo_name)
        .env("RUN_IDS", &run_ids_csv)
        .output()
        .await
        .context("Failed to execute Playwright script — is Node.js installed?")?;

    // Clean up the temp script (best-effort).
    let _ = std::fs::remove_file(&script_path);

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);

    if !output.status.success() {
        bail!(
            "Playwright approval script exited with {}: stdout={}, stderr={}",
            output.status,
            stdout.trim(),
            stderr.trim(),
        );
    }

    // Parse the count of approved runs from stdout.
    let approved = parse_approved_count(&stdout);

    tracing::debug!(
        "Playwright script output: stdout={}, stderr={}",
        stdout.trim(),
        stderr.trim(),
    );

    Ok(approved)
}

/// Parse the `TOTAL_APPROVED:<n>` line emitted by the Playwright script.
fn parse_approved_count(stdout: &str) -> usize {
    for line in stdout.lines() {
        if let Some(rest) = line.strip_prefix("TOTAL_APPROVED:") {
            if let Ok(n) = rest.trim().parse::<usize>() {
                return n;
            }
        }
    }
    0
}

// ---------------------------------------------------------------------------
// Embedded Playwright script (ES module)
// ---------------------------------------------------------------------------

/// Self-contained Node.js script that uses Playwright to sign into GitHub and
/// approve pending workflow runs.  TOTP two-factor authentication is handled
/// using only the built-in `node:crypto` module — no extra npm packages are
/// required beyond `playwright` itself.
const PLAYWRIGHT_SCRIPT: &str = r##"
import { chromium } from "playwright";
import { createHmac } from "node:crypto";

// ── TOTP generation (RFC 6238) using only Node.js built-ins ──────────────

function base32Decode(encoded) {
  const alphabet = "ABCDEFGHIJKLMNOPQRSTUVWXYZ234567";
  let bits = "";
  for (const c of encoded.toUpperCase().replace(/=+$/, "")) {
    const val = alphabet.indexOf(c);
    if (val === -1) continue;
    bits += val.toString(2).padStart(5, "0");
  }
  const bytes = [];
  for (let i = 0; i + 8 <= bits.length; i += 8) {
    bytes.push(parseInt(bits.substring(i, i + 8), 2));
  }
  return Buffer.from(bytes);
}

function generateTOTP(secret) {
  const key = base32Decode(secret);
  const time = Math.floor(Date.now() / 1000 / 30);
  const buf = Buffer.alloc(8);
  buf.writeUInt32BE(Math.floor(time / 0x100000000), 0);
  buf.writeUInt32BE(time >>> 0, 4);
  const hmac = createHmac("sha1", key);
  hmac.update(buf);
  const hash = hmac.digest();
  const offset = hash[hash.length - 1] & 0xf;
  const code =
    (((hash[offset] & 0x7f) << 24) |
      ((hash[offset + 1] & 0xff) << 16) |
      ((hash[offset + 2] & 0xff) << 8) |
      (hash[offset + 3] & 0xff)) %
    1000000;
  return code.toString().padStart(6, "0");
}

// ── Main ─────────────────────────────────────────────────────────────────

const {
  GITHUB_USERNAME,
  GITHUB_PASSWORD,
  GITHUB_TOTP_SECRET,
  REPO_OWNER,
  REPO_NAME,
  RUN_IDS,
} = process.env;

const runIds = RUN_IDS.split(",").map((s) => s.trim()).filter(Boolean);

const browser = await chromium.launch({ headless: true });
const context = await browser.newContext();
const page = await context.newPage();

try {
  // ── Sign in ──────────────────────────────────────────────────────────
  await page.goto("https://github.com/login");
  await page.fill("#login_field", GITHUB_USERNAME);
  await page.fill("#password", GITHUB_PASSWORD);
  await page.click('[name="commit"]');

  // Wait for navigation after login submission.
  await page.waitForLoadState("networkidle", { timeout: 15_000 });

  // ── Two-factor authentication ────────────────────────────────────────
  const currentUrl = page.url();
  if (currentUrl.includes("/sessions/two-factor")) {
    if (!GITHUB_TOTP_SECRET) {
      throw new Error("2FA required but GITHUB_TOTP_SECRET is not set");
    }
    const code = generateTOTP(GITHUB_TOTP_SECRET);
    // GitHub may show the TOTP input as #app_totp or a generic OTP field.
    const totpInput = page.locator("#app_totp").or(page.locator('[name="otp"]'));
    await totpInput.fill(code);
    // Some 2FA forms auto-submit; click the submit button if visible.
    const submitBtn = page.locator('button[type="submit"]');
    if (await submitBtn.isVisible({ timeout: 2_000 }).catch(() => false)) {
      await submitBtn.click();
    }
    await page.waitForURL(/github\.com(?!.*two-factor)/, { timeout: 15_000 });
  }

  // ── Approve each workflow run ────────────────────────────────────────
  let approved = 0;
  for (const runId of runIds) {
    const runUrl = `https://github.com/${REPO_OWNER}/${REPO_NAME}/actions/runs/${runId}`;
    await page.goto(runUrl);
    await page.waitForLoadState("networkidle", { timeout: 15_000 });

    // The approval banner typically contains a button labelled
    // "Approve and run".  Try a few common selectors.
    const approveBtn = page
      .getByRole("button", { name: /approve and run/i })
      .or(page.locator('button:has-text("Approve and run")'))
      .first();

    const visible = await approveBtn
      .isVisible({ timeout: 5_000 })
      .catch(() => false);
    if (visible) {
      await approveBtn.click();
      // Wait briefly for the action to take effect.
      await page.waitForTimeout(2_000);
      approved++;
      console.log(`APPROVED:${runId}`);
    } else {
      console.log(`NO_BUTTON:${runId}`);
    }
  }

  console.log(`TOTAL_APPROVED:${approved}`);
} finally {
  await browser.close();
}
"##;

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_approved_count_extracts_number() {
        let out = "APPROVED:12345\nAPPROVED:67890\nTOTAL_APPROVED:2\n";
        assert_eq!(parse_approved_count(out), 2);
    }

    #[test]
    fn parse_approved_count_zero_when_missing() {
        let out = "some other output\n";
        assert_eq!(parse_approved_count(out), 0);
    }

    #[test]
    fn parse_approved_count_handles_no_button_lines() {
        let out = "NO_BUTTON:111\nTOTAL_APPROVED:0\n";
        assert_eq!(parse_approved_count(out), 0);
    }

    #[test]
    fn parse_approved_count_ignores_malformed_line() {
        let out = "TOTAL_APPROVED:abc\n";
        assert_eq!(parse_approved_count(out), 0);
    }

    #[tokio::test]
    async fn browser_approve_returns_ok_for_empty_run_ids() {
        let result = approve_workflow_runs_via_browser("owner", "repo", &[]).await;
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), 0);
    }

    #[tokio::test]
    async fn browser_approve_errors_without_credentials() {
        // Ensure the env vars are NOT set for this test.
        std::env::remove_var(ENV_GITHUB_USERNAME);
        std::env::remove_var(ENV_GITHUB_PASSWORD);

        let result = approve_workflow_runs_via_browser("owner", "repo", &[123]).await;
        assert!(result.is_err());
        let msg = result.unwrap_err().to_string();
        assert!(
            msg.contains("GITHUB_USERNAME"),
            "Error should mention missing GITHUB_USERNAME: {msg}",
        );
    }

    #[test]
    fn playwright_script_is_valid_es_module() {
        // Smoke-check: the embedded script should contain key tokens.
        assert!(PLAYWRIGHT_SCRIPT.contains("chromium"));
        assert!(PLAYWRIGHT_SCRIPT.contains("GITHUB_USERNAME"));
        assert!(PLAYWRIGHT_SCRIPT.contains("TOTAL_APPROVED"));
        assert!(PLAYWRIGHT_SCRIPT.contains("generateTOTP"));
        assert!(PLAYWRIGHT_SCRIPT.contains("Approve and run"));
    }
}
