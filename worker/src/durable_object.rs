//! `RalphAgent` — a Cloudflare Durable Object that manages a single ralph
//! context with persistent SQLite-backed state, WebSocket support for
//! real-time progress updates, and alarm-based scheduling for recurring
//! iterations.
//!
//! ## Naming convention
//!
//! Each agent is addressed by a deterministic name derived from the
//! repository and ralph:
//!
//! ```text
//! {owner}/{repo}/ralph/{name}
//! ```
//!
//! ## HTTP routes handled inside the DO
//!
//! | Method | Path          | Description                              |
//! |--------|---------------|------------------------------------------|
//! | GET    | `/state`      | Return the current `RalphState`          |
//! | POST   | `/run`        | Trigger one iteration                    |
//! | POST   | `/pause`      | Pause execution                          |
//! | POST   | `/resume`     | Resume execution                         |
//! | POST   | `/migrate`    | Seed state from an external payload      |
//! | GET    | `/websocket`  | Upgrade to WebSocket for live updates    |

use crate::types::{ExecutionStatus, RalphState};
use worker::*;

/// Storage key for the serialized [`RalphState`].
const STATE_KEY: &str = "ralph_state";

/// The Durable Object class.
///
/// One instance exists per ralph context (`{owner}/{repo}/ralph/{name}`).
/// It persists a [`RalphState`] in transactional storage and supports
/// WebSocket connections for live progress streaming.
#[durable_object]
pub struct RalphAgent {
    state: State,
    #[allow(dead_code)]
    env: Env,
}

impl DurableObject for RalphAgent {
    fn new(state: State, env: Env) -> Self {
        Self { state, env }
    }

    async fn fetch(&self, req: Request) -> Result<Response> {
        let url = req.url()?;
        let path = url.path();

        match (req.method(), path) {
            (Method::Get, "/state") => self.handle_get_state().await,
            (Method::Post, "/run") => self.handle_run().await,
            (Method::Post, "/pause") => self.handle_pause().await,
            (Method::Post, "/resume") => self.handle_resume().await,
            (Method::Post, "/migrate") => self.handle_migrate(req).await,
            (Method::Get, "/websocket") => self.handle_websocket().await,
            _ => Response::error("Not Found", 404),
        }
    }

    async fn alarm(&self) -> Result<Response> {
        // Alarm fires for scheduled recurring iterations.  For now we
        // simply mark the agent as idle so the next explicit `/run`
        // request can proceed.  Full LLM-driven iteration will be
        // connected in a follow-up change.
        if let Some(mut rs) = self.load_state().await? {
            if rs.execution.status == ExecutionStatus::Running {
                rs.execution.status = ExecutionStatus::Idle;
                self.save_state(&rs).await?;
                self.broadcast(&rs).await;
            }
        }
        Response::ok("alarm processed")
    }

    async fn websocket_message(
        &self,
        _ws: WebSocket,
        message: WebSocketIncomingMessage,
    ) -> Result<()> {
        // Clients can send JSON commands over the WebSocket.  For now we
        // support a simple `{"command":"get_state"}` message.
        if let WebSocketIncomingMessage::String(text) = message {
            if let Ok(cmd) = serde_json::from_str::<serde_json::Value>(&text) {
                if cmd.get("command").and_then(|c| c.as_str()) == Some("get_state") {
                    if let Some(rs) = self.load_state().await.unwrap_or(None) {
                        let json = serde_json::to_string(&rs).unwrap_or_default();
                        let sockets = self.state.get_websockets();
                        for ws in &sockets {
                            let _ = ws.send_with_str(&json);
                        }
                    }
                }
            }
        }
        Ok(())
    }

    async fn websocket_close(
        &self,
        _ws: WebSocket,
        _code: usize,
        _reason: String,
        _was_clean: bool,
    ) -> Result<()> {
        // Nothing to do — the runtime handles hibernation automatically.
        Ok(())
    }

    async fn websocket_error(&self, _ws: WebSocket, _error: Error) -> Result<()> {
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

impl RalphAgent {
    /// Load the persisted state from transactional storage.
    async fn load_state(&self) -> Result<Option<RalphState>> {
        self.state.storage().get::<RalphState>(STATE_KEY).await
    }

    /// Persist state to transactional storage.
    async fn save_state(&self, rs: &RalphState) -> Result<()> {
        self.state.storage().put(STATE_KEY, rs).await
    }

    /// Broadcast the current state to all connected WebSocket clients.
    async fn broadcast(&self, rs: &RalphState) {
        let json = match serde_json::to_string(rs) {
            Ok(j) => j,
            Err(_) => return,
        };
        for ws in self.state.get_websockets() {
            let _ = ws.send_with_str(&json);
        }
    }

    // -- Route handlers -----------------------------------------------------

    /// `GET /state` — return current state as JSON.
    async fn handle_get_state(&self) -> Result<Response> {
        match self.load_state().await? {
            Some(rs) => Response::from_json(&rs),
            None => Response::from_json(&serde_json::json!({ "initialized": false })),
        }
    }

    /// `POST /run` — trigger one iteration.
    ///
    /// Sets status to `Running`, increments the iteration counter, and
    /// calls `wreck_it_core::iteration::advance_iteration` to select and
    /// start the next task.  The actual LLM execution (e.g. creating an
    /// issue and assigning Copilot) will be wired up in a follow-up.
    async fn handle_run(&self) -> Result<Response> {
        let mut rs = match self.load_state().await? {
            Some(rs) => rs,
            None => return Response::error("Agent not initialized — call /migrate first", 400),
        };

        if rs.execution.status == ExecutionStatus::Running {
            return Response::error("Already running", 409);
        }
        if rs.execution.status == ExecutionStatus::Paused {
            return Response::error("Agent is paused — call /resume first", 409);
        }

        // Use the shared iteration logic from wreck-it-core.
        let now = js_sys::Date::now() as u64 / 1000;
        let mut headless = wreck_it_core::state::HeadlessState::default();
        // Populate the authoritative status map from our task list so that
        // `advance_iteration` picks up the right statuses.
        for task in &rs.tasks {
            headless.task_statuses.insert(task.id.clone(), task.status);
        }

        let outcome =
            wreck_it_core::iteration::advance_iteration(&mut rs.tasks, &mut headless, now);

        // Mirror core changes back into our state.
        rs.execution.iteration_count = headless.iteration;
        rs.execution.last_run_at = Some(now);

        match outcome {
            wreck_it_core::iteration::IterationOutcome::TaskStarted { task_id, .. } => {
                rs.execution.status = ExecutionStatus::Running;
                rs.execution.current_task_id = Some(task_id.clone());
                // Sync task statuses back from core.
                for task in &mut rs.tasks {
                    if let Some(s) = headless.task_statuses.get(&task.id) {
                        task.status = *s;
                    }
                }
                self.save_state(&rs).await?;
                self.broadcast(&rs).await;
                Response::from_json(&serde_json::json!({
                    "result": "task_started",
                    "task_id": task_id,
                }))
            }
            wreck_it_core::iteration::IterationOutcome::AllComplete => {
                rs.execution.status = ExecutionStatus::Idle;
                rs.execution.current_task_id = None;
                self.save_state(&rs).await?;
                self.broadcast(&rs).await;
                Response::from_json(&serde_json::json!({ "result": "all_complete" }))
            }
            wreck_it_core::iteration::IterationOutcome::NoPendingTasks => {
                rs.execution.status = ExecutionStatus::Idle;
                self.save_state(&rs).await?;
                self.broadcast(&rs).await;
                Response::from_json(&serde_json::json!({ "result": "no_pending_tasks" }))
            }
        }
    }

    /// `POST /pause` — pause execution.
    async fn handle_pause(&self) -> Result<Response> {
        let mut rs = match self.load_state().await? {
            Some(rs) => rs,
            None => return Response::error("Agent not initialized", 400),
        };
        rs.execution.status = ExecutionStatus::Paused;
        self.save_state(&rs).await?;
        self.broadcast(&rs).await;
        Response::from_json(&serde_json::json!({ "status": "paused" }))
    }

    /// `POST /resume` — resume execution from paused state.
    async fn handle_resume(&self) -> Result<Response> {
        let mut rs = match self.load_state().await? {
            Some(rs) => rs,
            None => return Response::error("Agent not initialized", 400),
        };
        if rs.execution.status != ExecutionStatus::Paused {
            return Response::error("Agent is not paused", 409);
        }
        rs.execution.status = ExecutionStatus::Idle;
        self.save_state(&rs).await?;
        self.broadcast(&rs).await;
        Response::from_json(&serde_json::json!({ "status": "idle" }))
    }

    /// `POST /migrate` — seed agent state from an external JSON payload.
    ///
    /// Accepts a full [`RalphState`] body and persists it.  This is the
    /// migration path from KV or file-backed state into the Durable
    /// Object.
    async fn handle_migrate(&self, mut req: Request) -> Result<Response> {
        let body = req.text().await?;
        let rs: RalphState = serde_json::from_str(&body)
            .map_err(|e| Error::RustError(format!("Invalid RalphState JSON: {e}")))?;
        self.save_state(&rs).await?;
        self.broadcast(&rs).await;
        Response::from_json(&serde_json::json!({ "migrated": true }))
    }

    /// `GET /websocket` — upgrade to a WebSocket connection.
    ///
    /// The server accepts the WebSocket with hibernation support (tagged
    /// with `"portal"`) and immediately pushes the current state to the
    /// new client.
    async fn handle_websocket(&self) -> Result<Response> {
        let pair = WebSocketPair::new()?;
        let server = pair.server;
        let client = pair.client;

        self.state.accept_websocket_with_tags(&server, &["portal"]);

        // Push current state to the newly connected client.
        if let Ok(Some(rs)) = self.load_state().await {
            let json = serde_json::to_string(&rs).unwrap_or_default();
            let _ = server.send_with_str(&json);
        }

        Response::from_websocket(client)
    }
}

/// Build the deterministic Durable Object name for a ralph context.
///
/// Format: `{owner}/{repo}/ralph/{name}`
pub fn agent_name(owner: &str, repo: &str, name: &str) -> String {
    format!("{owner}/{repo}/ralph/{name}")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn agent_name_format() {
        assert_eq!(agent_name("octo", "repo", "docs"), "octo/repo/ralph/docs");
    }

    #[test]
    fn agent_name_with_special_chars() {
        assert_eq!(
            agent_name("my-org", "my-repo", "feature-dev"),
            "my-org/my-repo/ralph/feature-dev"
        );
    }

    #[test]
    fn state_key_constant() {
        assert_eq!(STATE_KEY, "ralph_state");
    }
}
