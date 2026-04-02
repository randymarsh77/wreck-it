//! `SchedulerAgent` — a Cloudflare Durable Object that manages recurring
//! pulse iterations for a GitHub App installation.
//!
//! ## Naming convention
//!
//! Each scheduler is addressed by a deterministic name derived from the
//! installation ID:
//!
//! ```text
//! installation/{installation_id}/scheduler
//! ```
//!
//! ## HTTP routes handled inside the DO
//!
//! | Method | Path        | Description                                  |
//! |--------|-------------|----------------------------------------------|
//! | POST   | `/schedule` | Set or update the pulse schedule              |
//! | POST   | `/disable`  | Disable the pulse schedule (cancel alarm)     |
//! | GET    | `/status`   | Return current scheduler status               |
//!
//! When an alarm fires the DO loads the pulse registry for the installation
//! and runs an iteration for each registered repo, acting as a replacement
//! for the global cron trigger per-installation.

use crate::github::GitHubClient;
use crate::github_app;
use crate::kv_store;
use crate::processor;
use worker::*;

/// Storage key for the scheduler configuration.
const CONFIG_KEY: &str = "scheduler_config";

/// Persistent configuration for the scheduler.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
struct SchedulerConfig {
    installation_id: u64,
    /// Interval in seconds between pulse iterations.
    interval_secs: u64,
    /// Whether the scheduler is currently enabled.
    enabled: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            installation_id: 0,
            interval_secs: 30 * 60, // 30 minutes
            enabled: false,
        }
    }
}

/// The Durable Object class.
///
/// One instance exists per installation (`installation/{id}/scheduler`).
#[durable_object]
pub struct SchedulerAgent {
    state: State,
    env: Env,
}

impl DurableObject for SchedulerAgent {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, mut req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();

        match (req.method(), path) {
            (Method::Post, "/schedule") => {
                let body = req.text().await?;
                self.handle_schedule(&body).await
            }
            (Method::Post, "/disable") => self.handle_disable().await,
            (Method::Get, "/status") => self.handle_status().await,
            _ => Response::error("Not Found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        let config = match self.load_config().await? {
            Some(c) => c,
            None => return Response::ok("scheduler not configured"),
        };

        if !config.enabled {
            console_log!(
                "[wreck-it][scheduler] alarm fired for installation {} but disabled — skipping",
                config.installation_id,
            );
            return Response::ok("scheduler disabled");
        }

        console_log!(
            "[wreck-it][scheduler] alarm fired for installation {} — pulsing repos",
            config.installation_id,
        );

        // Load installation settings to check pulse_enabled.
        let kv_result = self.env.kv(kv_store::KV_BINDING);
        if let Ok(kv) = &kv_result {
            match kv_store::load_installation_settings(kv, config.installation_id).await {
                Ok(settings) if !settings.pulse_enabled => {
                    console_log!(
                        "[wreck-it][scheduler] pulse disabled for installation {} — skipping",
                        config.installation_id,
                    );
                    // Re-schedule alarm for next interval.
                    self.schedule_next_alarm(config.interval_secs).await?;
                    return Response::ok("pulse disabled");
                }
                Err(e) => {
                    console_warn!(
                        "[wreck-it][scheduler] failed to load settings for installation {}: {e}",
                        config.installation_id,
                    );
                    // Continue anyway — default is enabled.
                }
                _ => {}
            }
        }

        // Run pulse for all repos in this installation.
        if let Err(e) = self
            .run_installation_pulse(config.installation_id)
            .await
        {
            console_error!(
                "[wreck-it][scheduler] pulse failed for installation {}: {e}",
                config.installation_id,
            );
        }

        // Re-schedule alarm for next interval.
        self.schedule_next_alarm(config.interval_secs).await?;

        Response::ok("alarm processed")
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl SchedulerAgent {
    async fn load_config(&self) -> Result<Option<SchedulerConfig>> {
        self.state
            .storage()
            .get::<SchedulerConfig>(CONFIG_KEY)
            .await
    }

    async fn save_config(&self, config: &SchedulerConfig) -> Result<()> {
        self.state.storage().put(CONFIG_KEY, config).await
    }

    async fn schedule_next_alarm(&self, interval_secs: u64) -> Result<()> {
        let ms = (interval_secs as i64) * 1000;
        let at = js_sys::Date::now() as i64 + ms;
        self.state
            .storage()
            .set_alarm(at)
            .await
    }

    /// `POST /schedule` — set or update the scheduler configuration.
    ///
    /// Expects JSON: `{ "installation_id": u64, "interval_secs": u64 }`
    async fn handle_schedule(&self, body: &str) -> Result<Response> {
        #[derive(serde::Deserialize)]
        struct ScheduleRequest {
            installation_id: u64,
            interval_secs: u64,
        }

        let req: ScheduleRequest = serde_json::from_str(body)
            .map_err(|e| Error::RustError(format!("Invalid schedule JSON: {e}")))?;

        let config = SchedulerConfig {
            installation_id: req.installation_id,
            interval_secs: req.interval_secs,
            enabled: true,
        };

        self.save_config(&config).await?;
        self.schedule_next_alarm(config.interval_secs).await?;

        Response::from_json(&serde_json::json!({
            "status": "scheduled",
            "installation_id": config.installation_id,
            "interval_secs": config.interval_secs,
        }))
    }

    /// `POST /disable` — disable the scheduler and cancel alarms.
    async fn handle_disable(&self) -> Result<Response> {
        if let Some(mut config) = self.load_config().await? {
            config.enabled = false;
            self.save_config(&config).await?;
        }
        self.state.storage().delete_alarm().await?;
        Response::from_json(&serde_json::json!({ "status": "disabled" }))
    }

    /// `GET /status` — return current scheduler status.
    async fn handle_status(&self) -> Result<Response> {
        match self.load_config().await? {
            Some(config) => Response::from_json(&serde_json::json!({
                "configured": true,
                "installation_id": config.installation_id,
                "interval_secs": config.interval_secs,
                "enabled": config.enabled,
            })),
            None => Response::from_json(&serde_json::json!({
                "configured": false,
            })),
        }
    }

    /// Run pulse iterations for all repos in a given installation.
    async fn run_installation_pulse(&self, installation_id: u64) -> std::result::Result<(), String> {
        let kv = self
            .env
            .kv(kv_store::KV_BINDING)
            .map_err(|e| format!("failed to open KV binding: {e}"))?;

        let all_registrations = kv_store::load_pulse_registry(&kv).await?;
        let registrations: Vec<_> = all_registrations
            .iter()
            .filter(|r| r.installation_id == installation_id)
            .collect();

        if registrations.is_empty() {
            console_log!(
                "[wreck-it][scheduler] no registered repos for installation {}",
                installation_id,
            );
            return Ok(());
        }

        let app_id = self
            .env
            .secret("GITHUB_APP_ID")
            .map(|s| s.to_string())
            .map_err(|_| "Missing GITHUB_APP_ID secret".to_string())?;
        let private_key = self
            .env
            .secret("GITHUB_APP_PRIVATE_KEY")
            .map(|s| s.to_string())
            .map_err(|_| "Missing GITHUB_APP_PRIVATE_KEY secret".to_string())?;

        let now_secs = crate::js_sys_now_secs();
        let jwt = github_app::generate_jwt(&app_id, &private_key, now_secs)?;

        for reg in &registrations {
            console_log!(
                "[wreck-it][scheduler] processing {}/{} (installation={})",
                reg.owner,
                reg.repo,
                reg.installation_id,
            );

            match github_app::vend_installation_token(reg.installation_id, &jwt, &reg.repo).await {
                Ok(token) => {
                    let client = GitHubClient::new(&reg.owner, &reg.repo, &token);
                    match processor::process_iteration(&client, &reg.default_branch).await {
                        Ok(result) => {
                            let status = if result.changed { "processed" } else { "no-op" };
                            console_log!(
                                "[wreck-it][scheduler] {}/{}: {status}: {}",
                                reg.owner,
                                reg.repo,
                                result.summary,
                            );
                        }
                        Err(e) => {
                            console_error!(
                                "[wreck-it][scheduler] {}/{}: iteration failed: {e}",
                                reg.owner,
                                reg.repo,
                            );
                        }
                    }
                }
                Err(e) => {
                    console_error!(
                        "[wreck-it][scheduler] {}/{}: token vending failed: {e}",
                        reg.owner,
                        reg.repo,
                    );
                }
            }
        }

        Ok(())
    }
}

/// Build the deterministic Durable Object name for an installation scheduler.
///
/// Format: `installation/{installation_id}/scheduler`
pub fn scheduler_name(installation_id: u64) -> String {
    format!("installation/{installation_id}/scheduler")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scheduler_name_format() {
        assert_eq!(scheduler_name(42), "installation/42/scheduler");
    }

    #[test]
    fn scheduler_name_large_id() {
        assert_eq!(
            scheduler_name(123456789),
            "installation/123456789/scheduler"
        );
    }

    #[test]
    fn scheduler_config_default() {
        let config = SchedulerConfig::default();
        assert_eq!(config.installation_id, 0);
        assert_eq!(config.interval_secs, 1800);
        assert!(!config.enabled);
    }

    #[test]
    fn scheduler_config_roundtrip() {
        let config = SchedulerConfig {
            installation_id: 42,
            interval_secs: 900,
            enabled: true,
        };
        let json = serde_json::to_string(&config).unwrap();
        let loaded: SchedulerConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(loaded.installation_id, 42);
        assert_eq!(loaded.interval_secs, 900);
        assert!(loaded.enabled);
    }
}
