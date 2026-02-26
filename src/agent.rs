use crate::types::{
    EvaluationMode, ModelProvider, Task, DEFAULT_GITHUB_MODELS_MODEL, DEFAULT_LLAMA_MODEL,
    LLAMA_PROVIDER_TYPE,
};
use anyhow::{bail, Context, Result};
use copilot_sdk_supercharged::*;
use std::path::Path;
use std::process::Command;
use std::sync::Arc;

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
        Self {
            copilot_client: None,
            cli_path: None,
            work_dir,
            model_provider,
            api_endpoint,
            api_token,
            verification_command,
            evaluation_mode,
            completeness_prompt,
            completion_marker_file,
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

        tracing::info!("Initializing Copilot SDK client...");

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
        let prompt = format!(
            "You are an AI coding agent working on a task in a git repository.\n\
             Working directory: {}\n\n\
             Task: {}\n\n\
             Context:\n{}\n\n\
             Please implement the necessary code changes to complete this task. \
             Be specific and provide complete, working code.",
            self.work_dir, task.description, context
        );

        // Send the message and wait for response
        let response = session
            .send_and_wait(
                MessageOptions {
                    prompt,
                    attachments: None,
                    mode: None,
                },
                Some(120_000), // 2 minute timeout
            )
            .await
            .context("Failed to get response from Copilot")?;

        // Extract the response content
        let result = if let Some(event) = response {
            event
                .assistant_message_content()
                .unwrap_or("Task completed")
                .to_string()
        } else {
            "No response from Copilot agent".to_string()
        };

        // Clean up the session
        session.destroy().await.ok();

        Ok(result)
    }

    /// Execute a task via direct HTTP to the GitHub Models API (OpenAI-compatible).
    async fn execute_task_via_http(&self, task: &Task) -> Result<String> {
        let context = self.read_codebase_context()?;
        let prompt = format!(
            "You are an AI coding agent working on a task in a git repository.\n\
             Working directory: {}\n\n\
             Task: {}\n\n\
             Context:\n{}\n\n\
             Please implement the necessary code changes to complete this task. \
             Be specific and provide complete, working code.",
            self.work_dir, task.description, context
        );

        self.chat_via_http(&prompt).await
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

        let content = json["choices"][0]["message"]["content"]
            .as_str()
            .unwrap_or("No response from models API")
            .to_string();

        Ok(content)
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

        // Use direct HTTP for GithubModels provider
        if self.model_provider == ModelProvider::GithubModels {
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

        let response = session
            .send_and_wait(
                MessageOptions {
                    prompt,
                    attachments: None,
                    mode: None,
                },
                Some(120_000),
            )
            .await
            .context("Failed to get evaluation response from agent")?;

        if let Some(event) = &response {
            if let Some(msg) = event.assistant_message_content() {
                tracing::info!("Evaluation agent response: {}", msg);
            }
        }

        session.destroy().await.ok();

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
            phase: 1,
            depends_on: vec![],
        };

        let result = client.execute_task(&task).await;
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("API token is required"));
    }
}
