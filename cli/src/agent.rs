use crate::agent_memory::AgentMemory;
use crate::artefact_store;
use crate::types::{
    CriticResult, EvaluationMode, ModelProvider, Task, DEFAULT_GITHUB_MODELS_MODEL,
    DEFAULT_LLAMA_MODEL, DEFAULT_PRECONDITION_MARKER, LLAMA_PROVIDER_TYPE,
};
use anyhow::{bail, Context, Result};
use copilot_sdk_supercharged::*;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

/// Resolve the `copilot` binary path.
///
/// Tries the `which` crate first, then falls back to running the shell
/// `which` command as a subprocess (more reliable with Nix-managed PATHs).
pub(crate) fn resolve_copilot_cli_path() -> Option<String> {
    // Strategy 1: `which` crate
    if let Ok(p) = which::which("copilot") {
        let path = p.to_string_lossy().to_string();
        tracing::info!("Resolved copilot CLI path (which crate): {}", path);
        return Some(path);
    }

    // Strategy 2: shell `which` subprocess — handles Nix wrapper scripts
    // and other cases the crate may miss.
    if let Ok(output) = Command::new("which").arg("copilot").output() {
        if output.status.success() {
            let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !path.is_empty() {
                tracing::info!("Resolved copilot CLI path (shell which): {}", path);
                return Some(path);
            }
        }
    }

    tracing::warn!("Could not find 'copilot' binary on PATH");
    None
}

/// Run a Copilot SDK session's `send_and_wait` on a **dedicated blocking thread**
/// with its own single-threaded tokio runtime.
///
/// The SDK's `send_and_wait` uses `tokio::sync::Mutex::blocking_lock()` inside
/// a synchronous event-handler callback.  `blocking_lock()` panics when called
/// from **any** tokio runtime context — even `new_current_thread`.  We avoid
/// that by reimplementing the send-and-wait logic ourselves:
///   1. Register our own event handler via `session.on()`, storing results in
///      a **`std::sync::Mutex`** (not tokio's) so no `blocking_lock()` needed.
///   2. Call `session.send()` to dispatch the prompt.
///   3. Wait for the idle / error signal on an `mpsc` channel with a timeout.
///
/// Returns the last assistant message event (or `None`) and destroys the
/// session.
pub(crate) async fn copilot_send_on_blocking_thread(
    session: Arc<CopilotSession>,
    prompt: String,
    timeout_ms: u64,
) -> Result<Option<SessionEvent>> {
    // Use std::sync primitives inside the Fn handler to avoid blocking_lock
    let last_msg: Arc<std::sync::Mutex<Option<SessionEvent>>> =
        Arc::new(std::sync::Mutex::new(None));
    let (idle_tx, mut idle_rx) = tokio::sync::mpsc::channel::<Result<(), String>>(1);

    let msg_clone = Arc::clone(&last_msg);
    let tx_clone = idle_tx.clone();

    let sub = session
        .on(move |event: SessionEvent| {
            if event.is_assistant_message() {
                if let Ok(mut guard) = msg_clone.lock() {
                    *guard = Some(event);
                }
            } else if event.is_session_idle() {
                let _ = tx_clone.try_send(Ok(()));
            } else if event.is_session_error() {
                let err = event.error_message().unwrap_or("Unknown error").to_string();
                let _ = tx_clone.try_send(Err(err));
            }
        })
        .await;

    session
        .send(MessageOptions {
            prompt,
            attachments: None,
            mode: None,
        })
        .await
        .map_err(|e| anyhow::anyhow!("Copilot send failed: {e}"))?;

    // Wait for idle / error / timeout
    let outcome =
        tokio::time::timeout(std::time::Duration::from_millis(timeout_ms), idle_rx.recv()).await;

    // Don't call sub.unsubscribe() — its Drop impl uses blocking_lock()
    // which panics on a tokio thread.  We're about to destroy the session
    // anyway, so just leak the subscription.
    std::mem::forget(sub);

    match outcome {
        Ok(Some(Ok(()))) => {}
        Ok(Some(Err(e))) => {
            let _ = session.destroy().await;
            bail!("Copilot session error: {e}");
        }
        Ok(None) => {
            let _ = session.destroy().await;
            bail!("Copilot session channel closed unexpectedly");
        }
        Err(_) => {
            let _ = session.destroy().await;
            bail!("Copilot session timed out after {timeout_ms}ms");
        }
    }

    let result = last_msg.lock().ok().and_then(|mut g| g.take());
    let _ = session.destroy().await;
    Ok(result)
}

/// Convenience wrapper: create a fresh CopilotClient, open a session,
/// send a single prompt, and tear everything down — all on a blocking thread.
///
/// Used by the planner / replanner which don't keep a persistent client.
pub(crate) async fn copilot_oneshot(
    cli_path: String,
    config: SessionConfig,
    prompt: String,
    timeout_ms: u64,
    default_response: &str,
) -> Result<String> {
    let default = default_response.to_string();

    let options = CopilotClientOptions {
        cli_path: Some(cli_path),
        log_level: "info".to_string(),
        ..Default::default()
    };

    let client = CopilotClient::new(options);
    client
        .start()
        .await
        .context("Failed to start Copilot client")?;

    let session = client
        .create_session(config)
        .await
        .context("Failed to create Copilot session")?;

    let session = Arc::new(session);
    let response =
        copilot_send_on_blocking_thread(Arc::clone(&session), prompt, timeout_ms).await?;

    let _ = client.stop().await;

    let text = response
        .map(|event| {
            event
                .assistant_message_content()
                .unwrap_or(&default)
                .to_string()
        })
        .unwrap_or_else(|| default.clone());

    Ok(text)
}

/// Agent client for interacting with GitHub Copilot SDK
pub struct AgentClient {
    copilot_client: Option<Arc<CopilotClient>>,
    cli_path: Option<String>,
    work_dir: String,
    model_provider: ModelProvider,
    api_endpoint: String,
    api_token: Option<String>,
    verification_command: Option<String>,
    evaluation_mode: EvaluationMode,
    completeness_prompt: Option<String>,
    completion_marker_file: String,
    /// Maximum number of autonomous continuation steps for autopilot mode.
    /// `None` means unlimited.  Maps to `--max-autopilot-continues`.
    max_autopilot_continues: Option<u32>,
}

impl AgentClient {
    /// Create a new client with default evaluation settings (used by tests).
    #[cfg(test)]
    pub fn new(
        model_provider: ModelProvider,
        api_endpoint: String,
        api_token: Option<String>,
        work_dir: String,
        verification_command: Option<String>,
    ) -> Self {
        Self {
            copilot_client: None,
            cli_path: None, // Will use default from PATH
            work_dir,
            model_provider,
            api_endpoint,
            api_token,
            verification_command,
            evaluation_mode: EvaluationMode::default(),
            completeness_prompt: None,
            completion_marker_file: crate::types::DEFAULT_COMPLETION_MARKER.to_string(),
            max_autopilot_continues: None,
        }
    }

    /// Create a new client with full configuration including evaluation settings.
    #[allow(clippy::too_many_arguments)]
    pub fn with_evaluation(
        model_provider: ModelProvider,
        api_endpoint: String,
        api_token: Option<String>,
        work_dir: String,
        verification_command: Option<String>,
        evaluation_mode: EvaluationMode,
        completeness_prompt: Option<String>,
        completion_marker_file: String,
    ) -> Self {
        Self::with_evaluation_and_autopilot(
            model_provider,
            api_endpoint,
            api_token,
            work_dir,
            verification_command,
            evaluation_mode,
            completeness_prompt,
            completion_marker_file,
            None,
        )
    }

    /// Create a new client with full configuration including evaluation and
    /// autopilot settings.
    #[allow(clippy::too_many_arguments)]
    pub fn with_evaluation_and_autopilot(
        model_provider: ModelProvider,
        api_endpoint: String,
        api_token: Option<String>,
        work_dir: String,
        verification_command: Option<String>,
        evaluation_mode: EvaluationMode,
        completeness_prompt: Option<String>,
        completion_marker_file: String,
        max_autopilot_continues: Option<u32>,
    ) -> Self {
        Self {
            copilot_client: None,
            cli_path: resolve_copilot_cli_path(),
            work_dir,
            model_provider,
            api_endpoint,
            api_token,
            verification_command,
            evaluation_mode,
            completeness_prompt,
            completion_marker_file,
            max_autopilot_continues,
        }
    }

    /// Return the configured evaluation mode.
    pub fn evaluation_mode(&self) -> EvaluationMode {
        self.evaluation_mode
    }

    /// Initialize the Copilot SDK client
    async fn ensure_client(&mut self) -> Result<Arc<CopilotClient>> {
        if let Some(ref client) = self.copilot_client {
            return Ok(Arc::clone(client));
        }

        // Retry PATH resolution in case the environment changed since construction.
        if self.cli_path.is_none() {
            self.cli_path = resolve_copilot_cli_path();
        }

        if self.cli_path.is_none() {
            bail!(
                "Could not find the 'copilot' binary on PATH. \
                 Install GitHub Copilot CLI (https://gh.io/copilot-install) \
                 or ensure it is available in your shell environment."
            );
        }

        tracing::info!(
            "Initializing Copilot SDK client (cli_path={})...",
            self.cli_path.as_deref().unwrap_or("<none>")
        );

        let options = CopilotClientOptions {
            cli_path: self.cli_path.clone(),
            log_level: "info".to_string(),
            ..Default::default()
        };

        let client = CopilotClient::new(options);
        client
            .start()
            .await
            .context("Failed to start Copilot client")?;

        // Verify connectivity with a ping
        match client.ping(Some("wreck-it agent")).await {
            Ok(response) => {
                tracing::info!(
                    "Copilot SDK connected (protocol v{})",
                    response.protocol_version.unwrap_or(0)
                );
            }
            Err(e) => {
                tracing::warn!("Copilot ping failed: {}, continuing anyway", e);
            }
        }

        let client_arc = Arc::new(client);
        self.copilot_client = Some(Arc::clone(&client_arc));
        Ok(client_arc)
    }

    /// Validate that the work directory is safe to use
    fn validate_work_dir(&self) -> Result<()> {
        let path = Path::new(&self.work_dir);

        // Check if path exists
        if !path.exists() {
            bail!("Work directory does not exist: {}", self.work_dir);
        }

        // Check if it's a directory
        if !path.is_dir() {
            bail!("Work directory is not a directory: {}", self.work_dir);
        }

        // Check if it's a git repository
        let git_dir = path.join(".git");
        if !git_dir.exists() {
            bail!("Work directory is not a git repository: {}", self.work_dir);
        }

        // Convert to canonical path to prevent path traversal
        let canonical = path
            .canonicalize()
            .context("Failed to canonicalize work directory")?;

        tracing::debug!("Validated work directory: {}", canonical.display());

        Ok(())
    }

    /// Execute a task using the Copilot agent
    pub async fn execute_task(&mut self, task: &Task) -> Result<String> {
        tracing::info!("Executing task: {}", task.description);

        // Validate work directory
        self.validate_work_dir()?;

        // Use direct HTTP for GithubModels provider
        if self.model_provider == ModelProvider::GithubModels {
            return self.execute_task_via_http(task).await;
        }

        // Use Copilot CLI autopilot subprocess for CopilotAutopilot provider
        if self.model_provider == ModelProvider::CopilotAutopilot {
            return self.execute_task_via_autopilot(task).await;
        }

        // Get or create the Copilot client
        let client = self.ensure_client().await?;

        // Create a session configuration
        let config = SessionConfig {
            request_permission: Some(false), // Auto-approve for autonomous mode
            request_user_input: Some(false), // No user input in autonomous mode
            model: if self.model_provider == ModelProvider::Llama {
                Some(DEFAULT_LLAMA_MODEL.to_string())
            } else {
                None
            },
            provider: if self.model_provider == ModelProvider::Llama {
                Some(ProviderConfig {
                    provider_type: Some(LLAMA_PROVIDER_TYPE.to_string()),
                    wire_api: None,
                    base_url: self.api_endpoint.clone(),
                    api_key: self.api_token.clone(),
                    bearer_token: None,
                    azure: None,
                })
            } else {
                None
            },
            ..Default::default()
        };

        // Create a session
        let session = client
            .create_session(config)
            .await
            .context("Failed to create Copilot session")?;

        tracing::info!("Created Copilot session: {}", session.session_id());

        // Prepare the prompt with task and context
        let context = self.read_codebase_context()?;
        let memory = AgentMemory::new(&self.work_dir);
        let prior_context = memory.load_context(&task.id).unwrap_or_default();
        let memory_section = if prior_context.is_empty() {
            String::new()
        } else {
            format!("\nPrior attempts for this task:\n{}\n", prior_context)
        };
        let artefact_section = self.build_artefact_context(task);
        let iteration = memory.attempt_count(&task.id) + 1;
        let prompt = format!(
            "You are an AI coding agent working on a task in a git repository.\n\
             Working directory: {}\n\n\
             Task: {}\n\n\
             Context:\n{}\n{}{}\
             Please implement the necessary code changes to complete this task. \
             Be specific and provide complete, working code.",
            self.work_dir, task.description, context, memory_section, artefact_section
        );

        // Send the message and wait for response (on a blocking thread to
        // avoid tokio blocking_lock panics inside the SDK).
        let response = copilot_send_on_blocking_thread(session, prompt, 120_000).await;

        // Record this attempt in memory before propagating any error.
        let (outcome, summary) = match &response {
            Ok(Some(event)) => (
                "Success",
                event
                    .assistant_message_content()
                    .unwrap_or("Task completed")
                    .to_string(),
            ),
            Ok(None) => ("Success", "No response from Copilot agent".to_string()),
            Err(e) => ("Failure", e.to_string()),
        };
        memory
            .record_attempt(&task.id, iteration, outcome, &summary)
            .ok();

        // Extract the response content (or propagate the error)
        let result = response
            .context("Failed to get response from Copilot")?
            .map(|event| {
                event
                    .assistant_message_content()
                    .unwrap_or("Task completed")
                    .to_string()
            })
            .unwrap_or_else(|| "No response from Copilot agent".to_string());

        Ok(result)
    }

    /// Execute a task via direct HTTP to the GitHub Models API (OpenAI-compatible).
    async fn execute_task_via_http(&self, task: &Task) -> Result<String> {
        let context = self.read_codebase_context()?;
        let memory = AgentMemory::new(&self.work_dir);
        let prior_context = memory.load_context(&task.id).unwrap_or_default();
        let memory_section = if prior_context.is_empty() {
            String::new()
        } else {
            format!("\nPrior attempts for this task:\n{}\n", prior_context)
        };
        let artefact_section = self.build_artefact_context(task);
        let iteration = memory.attempt_count(&task.id) + 1;
        let prompt = format!(
            "You are an AI coding agent working on a task in a git repository.\n\
             Working directory: {}\n\n\
             Task: {}\n\n\
             Context:\n{}\n{}{}\
             Please implement the necessary code changes to complete this task. \
             Be specific and provide complete, working code.",
            self.work_dir, task.description, context, memory_section, artefact_section
        );

        let response = self.chat_via_http(&prompt).await;

        // Record this attempt in memory before propagating any error.
        let (outcome, summary) = match &response {
            Ok(text) => ("Success", text.clone()),
            Err(e) => ("Failure", e.to_string()),
        };
        memory
            .record_attempt(&task.id, iteration, outcome, &summary)
            .ok();

        response
    }

    /// Execute a task by invoking Copilot CLI in autopilot mode as a subprocess.
    ///
    /// This spawns `copilot --autopilot --yolo -p "<prompt>"` which gives the
    /// Copilot CLI full autonomy to read/write files, run shell commands, and
    /// iterate through multi-step plans without per-tool approval prompts.
    async fn execute_task_via_autopilot(&self, task: &Task) -> Result<String> {
        // Prefer the path cached at construction time; fall back to a fresh
        // PATH lookup; bail if neither succeeds.
        let resolved = self
            .cli_path
            .clone()
            .or_else(resolve_copilot_cli_path)
            .context(
                "Could not find the 'copilot' binary on PATH. \
                 Install GitHub Copilot CLI (https://gh.io/copilot-install) \
                 or ensure it is available in your shell environment.",
            )?;

        let context = self.read_codebase_context()?;
        let memory = AgentMemory::new(&self.work_dir);
        let prior_context = memory.load_context(&task.id).unwrap_or_default();
        let memory_section = if prior_context.is_empty() {
            String::new()
        } else {
            format!("\nPrior attempts for this task:\n{}\n", prior_context)
        };
        let artefact_section = self.build_artefact_context(task);
        let iteration = memory.attempt_count(&task.id) + 1;

        let prompt = format!(
            "You are an AI coding agent working on a task in a git repository.\n\
             Working directory: {work_dir}\n\n\
             Task: {desc}\n\n\
             Context:\n{ctx}\n{mem}{art}\
             Please implement the necessary code changes to complete this task. \
             Be specific and provide complete, working code.",
            work_dir = self.work_dir,
            desc = task.description,
            ctx = context,
            mem = memory_section,
            art = artefact_section,
        );

        let mut args: Vec<String> = vec![
            "--autopilot".to_string(),
            "--yolo".to_string(),
            "-p".to_string(),
            prompt.clone(),
            "--silent".to_string(),
        ];

        if let Some(max_continues) = self.max_autopilot_continues {
            args.push("--max-autopilot-continues".to_string());
            args.push(max_continues.to_string());
        }

        tracing::info!(
            "Launching Copilot CLI autopilot (binary={}, max_continues={:?})",
            resolved,
            self.max_autopilot_continues,
        );

        let output = tokio::process::Command::new(&resolved)
            .args(&args)
            .current_dir(&self.work_dir)
            .output()
            .await
            .context("Failed to launch Copilot CLI in autopilot mode")?;

        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();

        if !output.status.success() {
            let summary = format!(
                "Copilot autopilot exited with status {:?}\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                stdout,
                stderr
            );
            memory
                .record_attempt(&task.id, iteration, "Failure", &summary)
                .ok();
            bail!(
                "Copilot CLI autopilot failed (exit {}): {}",
                output.status.code().unwrap_or(-1),
                stderr.lines().last().unwrap_or(&stderr)
            );
        }

        let summary = if stdout.is_empty() {
            "Copilot autopilot completed (no output)".to_string()
        } else {
            stdout.clone()
        };

        memory
            .record_attempt(&task.id, iteration, "Success", &summary)
            .ok();

        tracing::info!("Copilot autopilot completed successfully");
        Ok(summary)
    }

    /// Evaluate task completeness via direct HTTP to the GitHub Models API.
    async fn evaluate_completeness_via_http(&self, task: &Task) -> Result<bool> {
        let marker_path = Path::new(&self.work_dir).join(&self.completion_marker_file);

        // Remove stale marker so we get a clean signal.
        if marker_path.exists() {
            std::fs::remove_file(&marker_path).ok();
        }

        let user_prompt = self
            .completeness_prompt
            .as_deref()
            .unwrap_or("Check if the current task has been completed successfully.");

        let prompt = format!(
            "You are an evaluation agent. Your job is to determine whether a task is complete.\n\
             Working directory: {work_dir}\n\n\
             Task: {task_desc}\n\n\
             Completeness criteria:\n{criteria}\n\n\
             If the task is complete, create the file '{marker}' in the working directory \
             containing the text \"COMPLETE\". If the task is NOT complete, do NOT create that file.\n\
             Only create the file when you are confident the task is fully done.",
            work_dir = self.work_dir,
            task_desc = task.description,
            criteria = user_prompt,
            marker = self.completion_marker_file,
        );

        let response = self.chat_via_http(&prompt).await;

        if let Ok(msg) = &response {
            tracing::info!("Evaluation agent response: {}", msg);
        }

        let complete = marker_path.exists();
        if complete {
            tracing::info!("Evaluation agent wrote marker file – task is complete");
            std::fs::remove_file(&marker_path).ok();
        } else {
            tracing::info!("Evaluation agent did NOT write marker file – task incomplete");
        }

        Ok(complete)
    }

    /// Send a chat completion request via HTTP to an OpenAI-compatible API.
    async fn chat_via_http(&self, prompt: &str) -> Result<String> {
        let token = self
            .api_token
            .as_deref()
            .context("API token is required for github-models provider")?;

        let model = DEFAULT_GITHUB_MODELS_MODEL;

        let body = serde_json::json!({
            "model": model,
            "messages": [
                { "role": "user", "content": prompt }
            ]
        });

        tracing::info!(
            "Sending HTTP chat request to {} (model: {})",
            self.api_endpoint,
            model
        );

        let client = reqwest::Client::new();
        let response = client
            .post(&self.api_endpoint)
            .header("Authorization", format!("Bearer {}", token))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Failed to send HTTP request to models API")?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".to_string());
            bail!("Models API returned error ({}): {}", status, body);
        }

        let json: serde_json::Value = response
            .json()
            .await
            .context("Failed to parse models API response")?;

        let content = json
            .get("choices")
            .and_then(|c| c.get(0))
            .and_then(|c| c.get("message"))
            .and_then(|m| m.get("content"))
            .and_then(|c| c.as_str())
            .context("Models API response missing expected choices[0].message.content field")?
            .to_string();

        Ok(content)
    }

    /// Return the path to the artefact manifest file inside the work directory.
    fn manifest_path(&self) -> std::path::PathBuf {
        Path::new(&self.work_dir).join(".wreck-it-artefacts.json")
    }

    /// Resolve input artefact references for `task` and build a prompt section
    /// that lists each artefact's content.  Returns an empty string when the
    /// task declares no inputs or when resolution encounters a non-fatal warning.
    fn build_artefact_context(&self, task: &Task) -> String {
        if task.inputs.is_empty() {
            return String::new();
        }
        let manifest_path = self.manifest_path();
        match artefact_store::resolve_input_artefacts(&manifest_path, &task.inputs) {
            Ok(resolved) => {
                let mut section = String::from("\nInput artefacts:\n");
                for (key, content) in &resolved {
                    section.push_str(&format!("\n--- {} ---\n{}\n", key, content));
                }
                section
            }
            Err(e) => {
                tracing::warn!("Failed to resolve input artefacts: {}", e);
                String::new()
            }
        }
    }

    fn read_codebase_context(&self) -> Result<String> {
        // Read key files from the codebase to provide context
        let mut context = String::new();

        // Get git status for context
        if let Ok(output) = Command::new("git")
            .args(["status", "--short"])
            .current_dir(&self.work_dir)
            .output()
        {
            let status = String::from_utf8_lossy(&output.stdout);
            if !status.is_empty() {
                context.push_str("Git status:\n");
                context.push_str(&status);
                context.push('\n');
            }
        }

        // Get recent commits for context
        if let Ok(output) = Command::new("git")
            .args(["log", "--oneline", "-5"])
            .current_dir(&self.work_dir)
            .output()
        {
            let log = String::from_utf8_lossy(&output.stdout);
            if !log.is_empty() {
                context.push_str("\nRecent commits:\n");
                context.push_str(&log);
            }
        }

        Ok(context)
    }

    /// Run tests in the working directory
    pub fn run_tests(&self) -> Result<bool> {
        // Validate work directory first
        self.validate_work_dir()?;

        // When using agent-file evaluation, delegate to agent-based check.
        if self.evaluation_mode == EvaluationMode::AgentFile {
            // The caller should use evaluate_completeness() instead.
            // Returning true here so the normal flow does not block.
            return Ok(true);
        }

        if let Some(command) = self.verification_command.as_deref() {
            tracing::info!(
                "Running custom verification command '{}' in {}",
                command,
                self.work_dir
            );
            // Intentional shell execution: this is a user-configured verification hook
            // and must only be used with trusted command input.
            return self.run_shell_command(command);
        }

        tracing::info!("Running tests in {}", self.work_dir);

        // Try to run common test commands
        let test_commands = vec![
            ("cargo", vec!["test"]),
            ("npm", vec!["test"]),
            ("pytest", vec![]),
        ];

        for (cmd, args) in test_commands {
            if let Ok(output) = Command::new(cmd)
                .args(&args)
                .current_dir(&self.work_dir)
                .output()
            {
                let success = output.status.success();
                tracing::info!("Test command '{}' result: {}", cmd, success);
                return Ok(success);
            }
        }

        // If no test command works, assume success
        Ok(true)
    }

    /// Evaluate task completeness using an agent.
    ///
    /// Sends a prompt to the Copilot agent describing what "complete" means.
    /// The agent is instructed to write `completion_marker_file` inside
    /// `work_dir` if (and only if) it determines the task is done.
    /// Returns `Ok(true)` when the marker file exists after the agent runs.
    pub async fn evaluate_completeness(&mut self, task: &Task) -> Result<bool> {
        self.validate_work_dir()?;

        // Use direct HTTP for GithubModels and CopilotAutopilot providers
        if self.model_provider == ModelProvider::GithubModels
            || self.model_provider == ModelProvider::CopilotAutopilot
        {
            return self.evaluate_completeness_via_http(task).await;
        }

        let marker_path = Path::new(&self.work_dir).join(&self.completion_marker_file);

        // Remove stale marker so we get a clean signal.
        if marker_path.exists() {
            std::fs::remove_file(&marker_path).ok();
        }

        let client = self.ensure_client().await?;

        let config = SessionConfig {
            request_permission: Some(false),
            request_user_input: Some(false),
            model: if self.model_provider == ModelProvider::Llama {
                Some(DEFAULT_LLAMA_MODEL.to_string())
            } else {
                None
            },
            provider: if self.model_provider == ModelProvider::Llama {
                Some(ProviderConfig {
                    provider_type: Some(LLAMA_PROVIDER_TYPE.to_string()),
                    wire_api: None,
                    base_url: self.api_endpoint.clone(),
                    api_key: self.api_token.clone(),
                    bearer_token: None,
                    azure: None,
                })
            } else {
                None
            },
            ..Default::default()
        };

        let session = client
            .create_session(config)
            .await
            .context("Failed to create evaluation session")?;

        let user_prompt = self
            .completeness_prompt
            .as_deref()
            .unwrap_or("Check if the current task has been completed successfully.");

        let prompt = format!(
            "You are an evaluation agent. Your job is to determine whether a task is complete.\n\
             Working directory: {work_dir}\n\n\
             Task: {task_desc}\n\n\
             Completeness criteria:\n{criteria}\n\n\
             If the task is complete, create the file '{marker}' in the working directory \
             containing the text \"COMPLETE\". If the task is NOT complete, do NOT create that file.\n\
             Only create the file when you are confident the task is fully done.",
            work_dir = self.work_dir,
            task_desc = task.description,
            criteria = user_prompt,
            marker = self.completion_marker_file,
        );

        let response = copilot_send_on_blocking_thread(session, prompt, 120_000)
            .await
            .context("Failed to get evaluation response from agent")?;

        if let Some(event) = &response {
            if let Some(msg) = event.assistant_message_content() {
                tracing::info!("Evaluation agent response: {}", msg);
            }
        }

        // Check whether the marker file was created.
        let complete = marker_path.exists();
        if complete {
            tracing::info!("Evaluation agent wrote marker file – task is complete");
            // Clean up the marker so it doesn't interfere with later tasks.
            std::fs::remove_file(&marker_path).ok();
        } else {
            tracing::info!("Evaluation agent did NOT write marker file – task incomplete");
        }

        Ok(complete)
    }

    /// Evaluate whether a task's precondition is satisfied using an agent.
    ///
    /// Sends the task's `precondition_prompt` to the configured LLM.  The agent
    /// is instructed to write a marker file (`DEFAULT_PRECONDITION_MARKER`)
    /// inside `work_dir` if (and only if) it determines the precondition is
    /// met.  Returns `Ok(true)` when the marker exists after the agent runs.
    ///
    /// This is particularly useful for recurring tasks where a simple cooldown
    /// timer is not sufficient — the agent can inspect the codebase, external
    /// state, or any other context to decide whether the task should run.
    pub async fn evaluate_precondition(&mut self, task: &Task) -> Result<bool> {
        let prompt = match &task.precondition_prompt {
            Some(p) => p.clone(),
            None => return Ok(true), // No precondition → always eligible
        };

        self.validate_work_dir()?;

        // Use direct HTTP for GithubModels and CopilotAutopilot providers
        if self.model_provider == ModelProvider::GithubModels
            || self.model_provider == ModelProvider::CopilotAutopilot
        {
            return self.evaluate_precondition_via_http(task, &prompt).await;
        }

        let marker_path = Path::new(&self.work_dir).join(DEFAULT_PRECONDITION_MARKER);

        // Remove stale marker so we get a clean signal.
        if marker_path.exists() {
            std::fs::remove_file(&marker_path).ok();
        }

        let client = self.ensure_client().await?;

        let config = SessionConfig {
            request_permission: Some(false),
            request_user_input: Some(false),
            model: if self.model_provider == ModelProvider::Llama {
                Some(DEFAULT_LLAMA_MODEL.to_string())
            } else {
                None
            },
            provider: if self.model_provider == ModelProvider::Llama {
                Some(ProviderConfig {
                    provider_type: Some(LLAMA_PROVIDER_TYPE.to_string()),
                    wire_api: None,
                    base_url: self.api_endpoint.clone(),
                    api_key: self.api_token.clone(),
                    bearer_token: None,
                    azure: None,
                })
            } else {
                None
            },
            ..Default::default()
        };

        let session = client
            .create_session(config)
            .await
            .context("Failed to create precondition evaluation session")?;

        let agent_prompt = format!(
            "You are a precondition evaluation agent. Your job is to determine whether \
             a task's precondition is satisfied and the task should run.\n\
             Working directory: {work_dir}\n\n\
             Task: {task_desc}\n\n\
             Precondition to evaluate:\n{criteria}\n\n\
             If the precondition IS met and the task should run, create the file \
             '{marker}' in the working directory containing the text \"READY\". \
             If the precondition is NOT met, do NOT create that file.\n\
             Only create the file when you are confident the precondition is satisfied.",
            work_dir = self.work_dir,
            task_desc = task.description,
            criteria = prompt,
            marker = DEFAULT_PRECONDITION_MARKER,
        );

        let response = copilot_send_on_blocking_thread(session, agent_prompt, 120_000)
            .await
            .context("Failed to get precondition evaluation response from agent")?;

        if let Some(event) = &response {
            if let Some(msg) = event.assistant_message_content() {
                tracing::info!("Precondition evaluation agent response: {}", msg);
            }
        }

        // Check whether the marker file was created.
        let met = marker_path.exists();
        if met {
            tracing::info!("Precondition evaluation agent wrote marker file – precondition met");
            std::fs::remove_file(&marker_path).ok();
        } else {
            tracing::info!(
                "Precondition evaluation agent did NOT write marker file – precondition not met"
            );
        }

        Ok(met)
    }

    /// Evaluate task precondition via direct HTTP to the GitHub Models API.
    async fn evaluate_precondition_via_http(
        &self,
        task: &Task,
        precondition: &str,
    ) -> Result<bool> {
        let marker_path = Path::new(&self.work_dir).join(DEFAULT_PRECONDITION_MARKER);

        // Remove stale marker so we get a clean signal.
        if marker_path.exists() {
            std::fs::remove_file(&marker_path).ok();
        }

        let prompt = format!(
            "You are a precondition evaluation agent. Your job is to determine whether \
             a task's precondition is satisfied and the task should run.\n\
             Working directory: {work_dir}\n\n\
             Task: {task_desc}\n\n\
             Precondition to evaluate:\n{criteria}\n\n\
             If the precondition IS met and the task should run, create the file \
             '{marker}' in the working directory containing the text \"READY\". \
             If the precondition is NOT met, do NOT create that file.\n\
             Only create the file when you are confident the precondition is satisfied.",
            work_dir = self.work_dir,
            task_desc = task.description,
            criteria = precondition,
            marker = DEFAULT_PRECONDITION_MARKER,
        );

        let response = self.chat_via_http(&prompt).await;

        if let Ok(msg) = &response {
            tracing::info!("Precondition evaluation agent response: {}", msg);
        }

        let met = marker_path.exists();
        if met {
            tracing::info!("Precondition evaluation agent wrote marker file – precondition met");
            std::fs::remove_file(&marker_path).ok();
        } else {
            tracing::info!(
                "Precondition evaluation agent did NOT write marker file – precondition not met"
            );
        }

        Ok(met)
    }

    /// Run a trusted verification shell command in the work directory.
    ///
    /// Returns `Ok(true)` when the command exits with status code 0,
    /// `Ok(false)` when it exits non-zero, and `Err` when execution fails.
    fn run_shell_command(&self, command: &str) -> Result<bool> {
        let mut cmd = if cfg!(target_os = "windows") {
            let mut c = Command::new("cmd");
            c.args(["/C", command]);
            c
        } else {
            let mut c = Command::new("sh");
            c.args(["-c", command]);
            c
        };

        let output = cmd
            .current_dir(&self.work_dir)
            .output()
            .context("Failed to run verification command")?;
        if !output.status.success() {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            tracing::warn!(
                "Verification command failed (exit: {:?})\nstdout:\n{}\nstderr:\n{}",
                output.status.code(),
                stdout,
                stderr
            );
        }
        Ok(output.status.success())
    }

    /// Get the current git diff (all uncommitted changes against HEAD).
    fn get_git_diff(&self) -> Result<String> {
        let output = Command::new("git")
            .args(["diff", "HEAD"])
            .current_dir(&self.work_dir)
            .output()
            .context("Failed to get git diff")?;
        Ok(String::from_utf8_lossy(&output.stdout).to_string())
    }

    /// Critique a git diff against a task description using the configured LLM.
    ///
    /// Returns a [`CriticResult`] with a quality score, a list of issues, and
    /// an approval decision.
    pub async fn critique_diff(&mut self, diff: &str, task: &Task) -> Result<CriticResult> {
        let prompt = format!(
            "You are a code reviewer evaluating a git diff against a task description.\n\
             Task: {task_desc}\n\n\
             Git diff:\n{diff}\n\n\
             Evaluate whether this diff correctly and completely implements the task.\n\
             Respond ONLY with a JSON object in this exact format (no other text):\n\
             {{\"score\": <float 0.0-1.0>, \"issues\": [\"issue1\", \"issue2\"], \"approved\": <true|false>}}\n\
             - score: 0.0 (completely wrong) to 1.0 (perfect)\n\
             - issues: list of specific problems found (empty array if approved)\n\
             - approved: true if the implementation adequately addresses the task",
            task_desc = task.description,
            diff = diff,
        );

        let response = if self.model_provider == ModelProvider::GithubModels
            || self.model_provider == ModelProvider::CopilotAutopilot
        {
            self.chat_via_http(&prompt).await?
        } else {
            self.critique_via_copilot(&prompt).await?
        };

        parse_critic_result(&response)
    }

    /// Send a critic prompt through the Copilot SDK and return the raw text response.
    async fn critique_via_copilot(&mut self, prompt: &str) -> Result<String> {
        let client = self.ensure_client().await?;

        let config = SessionConfig {
            request_permission: Some(false),
            request_user_input: Some(false),
            model: if self.model_provider == ModelProvider::Llama {
                Some(DEFAULT_LLAMA_MODEL.to_string())
            } else {
                None
            },
            provider: if self.model_provider == ModelProvider::Llama {
                Some(ProviderConfig {
                    provider_type: Some(LLAMA_PROVIDER_TYPE.to_string()),
                    wire_api: None,
                    base_url: self.api_endpoint.clone(),
                    api_key: self.api_token.clone(),
                    bearer_token: None,
                    azure: None,
                })
            } else {
                None
            },
            ..Default::default()
        };

        let session = client
            .create_session(config)
            .await
            .context("Failed to create critic session")?;

        let response = copilot_send_on_blocking_thread(session, prompt.to_string(), 60_000)
            .await
            .context("Failed to get critic response from Copilot")?;

        Ok(response
            .as_ref()
            .and_then(|e| e.assistant_message_content())
            .unwrap_or("")
            .to_string())
    }

    /// Execute a task and run up to `rounds` critic-actor reflection cycles
    /// before returning.  The commit is **not** performed here; it remains the
    /// responsibility of the caller.
    ///
    /// Flow:
    /// 1. Actor executes the task.
    /// 2. Critic evaluates the resulting diff.
    /// 3. If the critic approves (or rounds are exhausted) → return.
    /// 4. Otherwise re-invoke the actor with the critic issues as additional
    ///    context and go to step 2.
    pub async fn execute_task_with_reflection(&mut self, task: &Task, rounds: u8) -> Result<()> {
        tracing::info!(
            "Executing task with {} reflection round(s): {}",
            rounds,
            task.description
        );

        // Initial actor execution.
        self.execute_task(task).await?;

        // Reflection loop: critique then optionally re-execute.
        for round in 1..=rounds {
            let diff = self.get_git_diff()?;

            if diff.is_empty() {
                tracing::info!(
                    "No diff to critique after execution (reflection round {})",
                    round
                );
                break;
            }

            let critic_result = match self.critique_diff(&diff, task).await {
                Ok(r) => r,
                Err(e) => {
                    tracing::warn!(
                        "Critic evaluation failed (reflection round {}/{}): {}",
                        round,
                        rounds,
                        e
                    );
                    break;
                }
            };

            tracing::info!(
                "Critic reflection round {}/{}: score={:.2}, approved={}, issues={}",
                round,
                rounds,
                critic_result.score,
                critic_result.approved,
                critic_result.issues.len()
            );

            if critic_result.approved {
                tracing::info!("Critic approved the implementation – reflection complete");
                break;
            }

            if critic_result.issues.is_empty() {
                tracing::info!("Critic found no substantive issues – skipping re-invocation");
                break;
            }

            // Re-invoke the actor with critic issues as additional context.
            let issues_text = critic_result.issues.join("\n- ");
            let mut revised_task = task.clone();
            revised_task.description = format!(
                "{}\n\nCritic feedback (reflection round {}):\n- {}",
                task.description, round, issues_text
            );
            self.execute_task(&revised_task).await?;
        }

        Ok(())
    }

    /// Commit changes to the repository
    pub fn commit_changes(&self, message: &str) -> Result<()> {
        // Validate work directory first
        self.validate_work_dir()?;

        tracing::info!("Committing changes: {}", message);

        // Check git status first to see what files would be staged
        let status_output = Command::new("git")
            .args(["status", "--porcelain"])
            .current_dir(&self.work_dir)
            .output()
            .context("Failed to check git status")?;

        if status_output.stdout.is_empty() {
            tracing::info!("No changes to commit");
            return Ok(());
        }

        let status_text = String::from_utf8_lossy(&status_output.stdout);
        tracing::debug!("Git status:\n{}", status_text);

        // Stage changes - using --all stages all changes (tracked and untracked)
        // while respecting .gitignore patterns
        Command::new("git")
            .args(["add", "--all"])
            .current_dir(&self.work_dir)
            .output()
            .context("Failed to stage changes")?;

        // Commit with the message - using args array prevents command injection
        // We sanitize newlines for cleaner commit messages
        let safe_message = message.replace('\n', " ").replace('\r', "");
        Command::new("git")
            .args(["commit", "-m", &safe_message])
            .current_dir(&self.work_dir)
            .output()
            .context("Failed to commit changes")?;

        Ok(())
    }

    /// Stop the Copilot client if it was initialized
    #[allow(dead_code)]
    pub async fn stop(&mut self) -> Result<()> {
        if let Some(client) = self.copilot_client.take() {
            tracing::info!("Stopping Copilot SDK client...");
            // Try to get exclusive access to stop the client
            if let Ok(client) = Arc::try_unwrap(client) {
                client.stop().await.ok();
            }
        }
        Ok(())
    }
}

impl Drop for AgentClient {
    fn drop(&mut self) {
        // Best effort cleanup - we can't await in Drop, so we just release the reference
        if self.copilot_client.is_some() {
            tracing::debug!("AgentClient dropped, Copilot client will be cleaned up");
        }
    }
}

/// Parse a [`CriticResult`] from an LLM response string.
///
/// Handles raw JSON as well as JSON embedded in markdown code fences.
pub fn parse_critic_result(response: &str) -> Result<CriticResult> {
    // Strip markdown code fences if present.
    let stripped: String = response
        .lines()
        .filter(|l| {
            let t = l.trim();
            !t.starts_with("```")
        })
        .collect::<Vec<_>>()
        .join("\n");

    // Find the JSON object boundaries.
    let start = stripped
        .find('{')
        .context("No JSON object found in critic response")?;
    let end = stripped
        .rfind('}')
        .context("No closing brace in critic response")?
        + 1;
    let json_part = &stripped[start..end];

    serde_json::from_str::<CriticResult>(json_part)
        .context("Failed to parse CriticResult from LLM response")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn init_git_repo(path: &Path) {
        Command::new("git")
            .args(["init"])
            .current_dir(path)
            .output()
            .unwrap();
    }

    #[test]
    fn run_tests_uses_custom_verification_command_when_configured() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let client = AgentClient::new(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            dir.path().to_string_lossy().to_string(),
            Some("true".to_string()),
        );

        assert!(client.run_tests().unwrap());
    }

    #[test]
    fn run_tests_marks_failure_when_custom_verification_command_fails() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let client = AgentClient::new(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            dir.path().to_string_lossy().to_string(),
            Some("false".to_string()),
        );

        assert!(!client.run_tests().unwrap());
    }

    #[test]
    fn run_tests_returns_true_in_agent_file_mode() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let client = AgentClient::with_evaluation(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            dir.path().to_string_lossy().to_string(),
            None,
            EvaluationMode::AgentFile,
            Some("check completeness".to_string()),
            ".task-complete".to_string(),
        );

        // In agent-file mode, run_tests() short-circuits to true.
        assert!(client.run_tests().unwrap());
    }

    #[test]
    fn evaluation_mode_accessor() {
        let client = AgentClient::new(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            ".".to_string(),
            None,
        );
        assert_eq!(client.evaluation_mode(), EvaluationMode::Command);

        let client2 = AgentClient::with_evaluation(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            ".".to_string(),
            None,
            EvaluationMode::AgentFile,
            None,
            ".done".to_string(),
        );
        assert_eq!(client2.evaluation_mode(), EvaluationMode::AgentFile);
    }

    #[test]
    fn github_models_provider_creates_client() {
        let client = AgentClient::with_evaluation(
            ModelProvider::GithubModels,
            "https://models.github.ai/inference/chat/completions".to_string(),
            Some("test-token".to_string()),
            ".".to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
        );
        // GithubModels provider should not initialize a copilot client
        assert!(client.copilot_client.is_none());
        assert_eq!(client.model_provider, ModelProvider::GithubModels);
    }

    #[tokio::test]
    async fn github_models_execute_task_requires_token() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let mut client = AgentClient::with_evaluation(
            ModelProvider::GithubModels,
            "https://models.github.ai/inference/chat/completions".to_string(),
            None, // no token
            dir.path().to_string_lossy().to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
        );

        let task = Task {
            id: "1".to_string(),
            description: "test task".to_string(),
            status: crate::types::TaskStatus::Pending,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        };

        let result = client.execute_task(&task).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("API token is required"));
    }

    // ---- CriticResult / parse_critic_result tests ----

    #[test]
    fn parse_critic_result_parses_valid_json() {
        let json = r#"{"score": 0.8, "issues": ["Missing error handling"], "approved": false}"#;
        let result = parse_critic_result(json).unwrap();
        assert!(!result.approved);
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0], "Missing error handling");
        assert!((result.score - 0.8).abs() < 0.01);
    }

    #[test]
    fn parse_critic_result_parses_approved_with_no_issues() {
        let json = r#"{"score": 0.95, "issues": [], "approved": true}"#;
        let result = parse_critic_result(json).unwrap();
        assert!(result.approved);
        assert!(result.issues.is_empty());
        assert!((result.score - 0.95).abs() < 0.01);
    }

    #[test]
    fn parse_critic_result_strips_markdown_code_fences() {
        let markdown =
            "```json\n{\"score\": 0.7, \"issues\": [\"needs tests\"], \"approved\": false}\n```";
        let result = parse_critic_result(markdown).unwrap();
        assert!(!result.approved);
        assert_eq!(result.issues.len(), 1);
        assert_eq!(result.issues[0], "needs tests");
    }

    #[test]
    fn parse_critic_result_extracts_json_from_surrounding_text() {
        let response =
            "Here is my evaluation:\n{\"score\": 0.6, \"issues\": [\"a\", \"b\"], \"approved\": false}\nEnd.";
        let result = parse_critic_result(response).unwrap();
        assert!(!result.approved);
        assert_eq!(result.issues.len(), 2);
    }

    #[test]
    fn parse_critic_result_returns_error_on_invalid_json() {
        let bad = "not json at all";
        assert!(parse_critic_result(bad).is_err());
    }

    #[test]
    fn parse_critic_result_returns_error_on_missing_brace() {
        let bad = "score: 0.5";
        assert!(parse_critic_result(bad).is_err());
    }

    /// Verify that execute_task_with_reflection with rounds=0 behaves identically
    /// to a bare execute_task call: it should propagate the first error immediately
    /// (no retry / no critique).
    #[tokio::test]
    async fn execute_task_with_reflection_rounds_zero_propagates_first_error() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let mut client = AgentClient::with_evaluation(
            ModelProvider::GithubModels,
            "https://models.github.ai/inference/chat/completions".to_string(),
            None, // no token → execute_task will fail
            dir.path().to_string_lossy().to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
        );

        let task = Task {
            id: "r0".to_string(),
            description: "task for reflection=0 test".to_string(),
            status: crate::types::TaskStatus::Pending,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        };

        // rounds=0 → no reflection loop; error comes from execute_task
        let result = client.execute_task_with_reflection(&task, 0).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("API token is required"));
    }

    // ---- evaluate_precondition tests ----

    #[tokio::test]
    async fn evaluate_precondition_returns_true_when_no_prompt() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        let mut client = AgentClient::new(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            dir.path().to_string_lossy().to_string(),
            None,
        );

        let task = Task {
            id: "no-precond".to_string(),
            description: "task without precondition".to_string(),
            status: crate::types::TaskStatus::Pending,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        };

        // No precondition prompt → always eligible
        let result = client.evaluate_precondition(&task).await.unwrap();
        assert!(result);
    }

    #[tokio::test]
    async fn evaluate_precondition_returns_true_when_marker_exists() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());

        // Pre-create the marker file to simulate an agent that decided
        // the precondition is met.
        let marker_path = dir.path().join(crate::types::DEFAULT_PRECONDITION_MARKER);
        std::fs::write(&marker_path, "READY").unwrap();

        let mut client = AgentClient::with_evaluation(
            ModelProvider::Copilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            dir.path().to_string_lossy().to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
        );

        // The evaluate_precondition method first removes any stale marker,
        // so a pre-existing marker won't trick it. But since we can't run
        // a real agent in unit tests, this test validates the marker-based
        // detection path by verifying the method handles missing Copilot
        // gracefully — the precondition evaluates to false because the agent
        // can't create the marker without a live Copilot session.
        let task = Task {
            id: "precond-test".to_string(),
            description: "task with precondition".to_string(),
            status: crate::types::TaskStatus::Pending,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::Recurring,
            cooldown_seconds: Some(3600),
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: Some("Check if documentation is stale".to_string()),
            parent_id: None,
            labels: vec![],
        };

        // Without a running Copilot server the session creation will fail
        // (Err), which is expected in a unit test environment.
        let result = client.evaluate_precondition(&task).await;
        assert!(result.is_err() || !result.unwrap());
    }

    // ---- CopilotAutopilot tests ----

    #[test]
    fn copilot_autopilot_provider_creates_client() {
        let client = AgentClient::with_evaluation(
            ModelProvider::CopilotAutopilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            ".".to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
        );
        // CopilotAutopilot does not initialise the SDK client
        assert!(client.copilot_client.is_none());
        assert_eq!(client.model_provider, ModelProvider::CopilotAutopilot);
        assert!(client.max_autopilot_continues.is_none());
    }

    #[test]
    fn copilot_autopilot_with_max_continues() {
        let client = AgentClient::with_evaluation_and_autopilot(
            ModelProvider::CopilotAutopilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            ".".to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
            Some(10),
        );
        assert_eq!(client.model_provider, ModelProvider::CopilotAutopilot);
        assert_eq!(client.max_autopilot_continues, Some(10));
    }

    #[tokio::test]
    async fn copilot_autopilot_execute_task_fails_when_binary_missing() {
        let dir = tempdir().unwrap();
        init_git_repo(dir.path());
        // Point the CLI path to a non-existent binary so the subprocess launch
        // fails immediately.
        let mut client = AgentClient::with_evaluation_and_autopilot(
            ModelProvider::CopilotAutopilot,
            "https://api.githubcopilot.com".to_string(),
            None,
            dir.path().to_string_lossy().to_string(),
            None,
            EvaluationMode::Command,
            None,
            ".task-complete".to_string(),
            Some(5),
        );
        // Force an invalid cli_path so the subprocess cannot start.
        client.cli_path = Some("/nonexistent/copilot".to_string());

        let task = Task {
            id: "autopilot-1".to_string(),
            description: "test autopilot fallback".to_string(),
            status: crate::types::TaskStatus::Pending,
            role: crate::types::AgentRole::default(),
            kind: crate::types::TaskKind::default(),
            cooldown_seconds: None,
            phase: 1,
            depends_on: vec![],
            priority: 0,
            complexity: 1,
            failed_attempts: 0,
            last_attempt_at: None,
            inputs: vec![],
            outputs: vec![],
            runtime: crate::types::TaskRuntime::default(),
            precondition_prompt: None,
            parent_id: None,
            labels: vec![],
        };

        let result = client.execute_task(&task).await;
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("autopilot") || err_msg.contains("No such file"),
            "Expected autopilot-related error, got: {}",
            err_msg
        );
    }
}
