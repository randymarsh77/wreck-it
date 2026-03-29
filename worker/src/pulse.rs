//! Pulse trigger system — scheduled (cron) processing for registered
//! repositories.
//!
//! When a Cloudflare cron trigger fires, the worker iterates over all
//! repositories registered in the pulse registry and runs a processing
//! iteration for each.  This injects entropy into the system: tasks with
//! cooldowns that have expired will be picked up, and idle ralph contexts
//! get a chance to advance.
//!
//! Repositories are automatically registered in the pulse registry when the
//! worker processes webhook events, so no manual setup is required.

use crate::github::GitHubClient;
use crate::github_app;
use crate::kv_store;
use crate::processor;
use crate::types::PulseRegistration;

/// Run a single pulse: iterate over every registered repository and
/// process one iteration for each.
///
/// Returns a summary string describing what happened.
pub async fn run_pulse(env: &worker::Env) -> Result<String, String> {
    let kv = env
        .kv(kv_store::KV_BINDING)
        .map_err(|e| format!("failed to open KV binding: {e}"))?;

    let registrations = kv_store::load_pulse_registry(&kv).await?;

    if registrations.is_empty() {
        return Ok("pulse: no registered repositories".into());
    }

    worker::console_log!(
        "[wreck-it][pulse] processing {} registered repo(s)",
        registrations.len(),
    );

    let app_id = env
        .secret("GITHUB_APP_ID")
        .map(|s| s.to_string())
        .map_err(|_| "Missing GITHUB_APP_ID secret".to_string())?;
    let private_key = env
        .secret("GITHUB_APP_PRIVATE_KEY")
        .map(|s| s.to_string())
        .map_err(|_| "Missing GITHUB_APP_PRIVATE_KEY secret".to_string())?;

    let now_secs = crate::js_sys_now_secs();
    let jwt = github_app::generate_jwt(&app_id, &private_key, now_secs)?;

    let mut summaries = Vec::new();

    for reg in &registrations {
        let result = process_registration(&jwt, reg).await;
        let summary = match result {
            Ok(s) => s,
            Err(e) => {
                worker::console_error!(
                    "[wreck-it][pulse] error processing {}/{}: {e}",
                    reg.owner,
                    reg.repo,
                );
                format!("{}/{}: error: {e}", reg.owner, reg.repo)
            }
        };
        summaries.push(summary);
    }

    Ok(format!("pulse: {}", summaries.join("; ")))
}

/// Process a single registered repository during a pulse.
async fn process_registration(jwt: &str, reg: &PulseRegistration) -> Result<String, String> {
    worker::console_log!(
        "[wreck-it][pulse] processing {}/{} (installation={})",
        reg.owner,
        reg.repo,
        reg.installation_id,
    );

    let token = github_app::vend_installation_token(reg.installation_id, jwt, &reg.repo).await?;
    let client = GitHubClient::new(&reg.owner, &reg.repo, &token);

    match processor::process_iteration(&client, &reg.default_branch).await {
        Ok(result) => {
            let status = if result.changed { "processed" } else { "no-op" };
            Ok(format!(
                "{}/{}: {status}: {}",
                reg.owner, reg.repo, result.summary
            ))
        }
        Err(e) => Err(format!("{}/{}: iteration failed: {e}", reg.owner, reg.repo)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pulse_registration_serde_roundtrip() {
        let reg = PulseRegistration {
            owner: "octo".into(),
            repo: "cat".into(),
            installation_id: 123,
            default_branch: "main".into(),
        };
        let json = serde_json::to_string(&reg).unwrap();
        let loaded: PulseRegistration = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.owner, "octo");
        assert_eq!(loaded.repo, "cat");
        assert_eq!(loaded.installation_id, 123);
        assert_eq!(loaded.default_branch, "main");
    }

    #[test]
    fn pulse_registration_multiple() {
        let regs = vec![
            PulseRegistration {
                owner: "a".into(),
                repo: "b".into(),
                installation_id: 1,
                default_branch: "main".into(),
            },
            PulseRegistration {
                owner: "c".into(),
                repo: "d".into(),
                installation_id: 2,
                default_branch: "develop".into(),
            },
        ];
        let json = serde_json::to_string(&regs).unwrap();
        let loaded: Vec<PulseRegistration> = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.len(), 2);
    }
}
