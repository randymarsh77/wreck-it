# Getting Started with wreck-it

This guide will walk you through using wreck-it to automate multi-step development tasks.

## Installation

### Prerequisites

Before installing wreck-it, you need:

1. **GitHub Copilot CLI**: Install and authenticate the GitHub Copilot CLI
   ```bash
   # Follow the installation guide at:
   # https://docs.github.com/en/copilot/how-tos/set-up/install-copilot-cli
   
   # Authenticate with GitHub
   copilot auth login
   
   # Verify it's working
   copilot --version
   ```

2. **GitHub Copilot Subscription**: Required to use the SDK. See [GitHub Copilot pricing](https://github.com/features/copilot#pricing).

### Using Nix (Recommended)

```bash
# Clone the repository
git clone https://github.com/randymarsh77/wreck-it.git
cd wreck-it

# Enter the development environment
nix develop

# Build the project
cargo build --release
```

### Using Cargo

```bash
# Clone the repository
git clone https://github.com/randymarsh77/wreck-it.git
cd wreck-it

# Build the project
cargo build --release

# The binary will be at target/release/wreck-it
```

## Quick Start

### 1. Initialize a Task File

```bash
wreck-it init --output tasks.json
```

This creates a sample task file with three example tasks.

### 2. Customize Your Tasks

Edit `tasks.json` to define your tasks:

```json
[
  {
    "id": "1",
    "description": "Add a new REST endpoint for user profile",
    "status": "pending"
  },
  {
    "id": "2",
    "description": "Write integration tests for user profile endpoint",
    "status": "pending"
  },
  {
    "id": "3",
    "description": "Update API documentation for user profile endpoint",
    "status": "pending"
  }
]
```

### 3. Run the Loop

```bash
wreck-it run --task-file tasks.json --max-iterations 50
```

The TUI will launch and show:
- Current iteration count
- List of tasks with status
- Real-time logs
- Controls (Space to pause, Q to quit)

**Note**: The Copilot CLI must be authenticated (via `copilot auth login`) before running. The SDK will automatically use your Copilot CLI credentials.

## Example Workflow

### Creating a New Feature

1. **Define Tasks**
   ```bash
   wreck-it init --output feature-tasks.json
   ```

2. **Edit Tasks**
   Break down your feature into specific, actionable tasks:
   ```json
   [
     {"id": "1", "description": "Create database migration for new table", "status": "pending"},
     {"id": "2", "description": "Add model class for new entity", "status": "pending"},
     {"id": "3", "description": "Implement CRUD API endpoints", "status": "pending"},
     {"id": "4", "description": "Add unit tests for model", "status": "pending"},
     {"id": "5", "description": "Add integration tests for API", "status": "pending"},
     {"id": "6", "description": "Update OpenAPI/Swagger documentation", "status": "pending"}
   ]
   ```

3. **Run with Monitoring**
   ```bash
   wreck-it run --task-file feature-tasks.json --max-iterations 100
   ```

4. **Review Progress**
   - Watch the TUI as tasks complete
   - Press Space to pause and review logs
   - Check git commits: `git log --oneline`

5. **Handle Failures**
   If a task fails:
   - Review the error in the logs
   - Fix the issue manually if needed
   - Edit the task description to be more specific
   - Resume with Space or restart

## Working Directory

By default, wreck-it works in the current directory. You can specify a different repository:

```bash
wreck-it run --work-dir /path/to/your/project --task-file tasks.json
```

## Safety Features

### Max Iterations

Always set a reasonable max iterations limit:

```bash
# For simple tasks (1-3 tasks)
wreck-it run --max-iterations 10

# For medium complexity (5-10 tasks)
wreck-it run --max-iterations 50

# For complex features (10+ tasks)
wreck-it run --max-iterations 200
```

### Manual Override

You can pause the loop at any time:
- Press **Space** to pause
- Review the current state
- Press **Space** again to resume
- Press **Q** to quit

### Git Safety

All changes are committed to git:
- Review commits with `git log`
- Revert bad changes with `git revert`
- Create a branch before running: `git checkout -b feature/auto-dev`

## Tips for Success

### 1. Write Clear Task Descriptions
❌ Bad: "Add authentication"
✅ Good: "Add JWT-based authentication middleware that validates tokens from Authorization header"

### 2. Keep Tasks Atomic
Each task should be a single, testable unit of work.

### 3. Start Small
Test the loop with 1-2 simple tasks before running complex workflows.

### 4. Monitor Progress
Don't walk away! Watch the TUI to catch issues early.

### 5. Use Git Branches
Always run wreck-it on a feature branch, not main.

## Troubleshooting

### Task Stuck in "inprogress"

If the loop stops with a task stuck:
1. Quit with Q
2. Edit tasks.json and change status back to "pending"
3. Restart

### Tests Keep Failing

If tests repeatedly fail:
1. Pause with Space
2. Run tests manually: `cargo test` or `npm test`
3. Fix any environment issues
4. Resume or restart

### Out of Iterations

If you hit max iterations:
1. Review completed tasks
2. Update tasks.json to remove completed ones
3. Increase --max-iterations if needed
4. Restart

## Advanced Usage

### Custom Test Commands

The agent will try common test commands:
- `cargo test` (Rust)
- `npm test` (Node.js)
- `pytest` (Python)

Make sure your project has working tests before running.

### Multiple Task Files

You can maintain different task files for different workflows:

```bash
wreck-it run --task-file feature-a.json
wreck-it run --task-file bugfix-b.json
wreck-it run --task-file refactor-c.json
```

## Next Steps

- Read [Architecture](architecture.md) to understand how it works
- Check out the [GitHub repository](https://github.com/randymarsh77/wreck-it) if you want to help
- Join discussions in GitHub Issues
