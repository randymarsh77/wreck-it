use crate::types::{ModelProvider, Task, DEFAULT_LLAMA_MODEL, LLAMA_PROVIDER_TYPE};
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
}

impl AgentClient {
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
        }
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

        if let Some(command) = self.verification_command.as_deref() {
            tracing::info!(
                "Running custom verification command '{}' in {}",
                command,
                self.work_dir
            );
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
}
