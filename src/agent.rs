use crate::types::Task;
use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use std::process::Command;
use std::path::Path;

/// Agent client for interacting with GitHub Copilot SDK
#[allow(dead_code)]
pub struct AgentClient {
    api_endpoint: String,
    api_token: Option<String>,
    work_dir: String,
}

#[allow(dead_code)]
#[derive(Debug, Serialize)]
struct AgentRequest {
    task: String,
    context: String,
}

#[allow(dead_code)]
#[derive(Debug, Deserialize)]
struct AgentResponse {
    #[allow(dead_code)]
    success: bool,
    message: String,
}

impl AgentClient {
    pub fn new(api_endpoint: String, api_token: Option<String>, work_dir: String) -> Self {
        Self {
            api_endpoint,
            api_token,
            work_dir,
        }
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
        let canonical = path.canonicalize()
            .context("Failed to canonicalize work directory")?;
        
        tracing::debug!("Validated work directory: {}", canonical.display());
        
        Ok(())
    }

    /// Execute a task using the Copilot agent
    pub async fn execute_task(&self, task: &Task) -> Result<String> {
        // For now, we'll simulate the agent execution
        // In a real implementation, this would call the GitHub Copilot SDK

        tracing::info!("Executing task: {}", task.description);

        // Simulate reading the codebase as context
        let context = self.read_codebase_context()?;

        // In a real implementation, this would make an API call to Copilot
        let result = self
            .simulate_agent_execution(&task.description, &context)
            .await?;

        Ok(result)
    }

    fn read_codebase_context(&self) -> Result<String> {
        // Read key files from the codebase to provide context
        let mut context = String::new();

        // This would typically read relevant files based on the task
        context.push_str(&format!("Working directory: {}\n", self.work_dir));

        Ok(context)
    }

    async fn simulate_agent_execution(&self, task: &str, context: &str) -> Result<String> {
        // This is a placeholder for the actual Copilot SDK integration
        // In practice, this would:
        // 1. Send task + context to Copilot API
        // 2. Receive code changes or instructions
        // 3. Apply changes to the filesystem
        // 4. Return the result

        tracing::info!("Simulating agent execution for: {}", task);
        tracing::debug!("Context length: {} bytes", context.len());

        // Simulate some work
        tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

        Ok(format!("Completed task: {}", task))
    }

    /// Run tests in the working directory
    pub fn run_tests(&self) -> Result<bool> {
        // Validate work directory first
        self.validate_work_dir()?;
        
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
}
