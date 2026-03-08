# Getting Started with wreck-it

This guide covers local installation and TUI usage. If you want to run wreck-it in **GitHub Actions or another CI system**, see the [CI & Headless Guide](ci-headless.md).

## Installation

### Prerequisites

Before installing wreck-it, choose a model provider:

1. **GitHub Models** *(recommended)*: Use GitHub's hosted model inference. Set `GITHUB_TOKEN` in your environment — no extra subscription needed.

2. **Copilot SDK**: Install and authenticate the GitHub Copilot CLI:
   ```bash
   # Follow the installation guide at:
   # https://docs.github.com/en/copilot/how-tos/set-up/install-copilot-cli
   
   # Authenticate with GitHub
   copilot auth login
   
   # Verify it's working
   copilot --version
   ```
   A [GitHub Copilot subscription](https://github.com/features/copilot#pricing) is required.

3. **Local Llama**: Run a local Ollama instance. No subscription needed.

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

**Note**: When using GitHub Models (the default), set `GITHUB_TOKEN` in your environment. When using the Copilot SDK, authenticate via `copilot auth login` first.

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

### Plan Tasks from a Goal

Use the `plan` command to generate a task file from a natural-language description:

```bash
wreck-it plan --goal "Build a REST API with authentication and tests" --output tasks.json
```

You can also plan and run in a single step with the `--goal` flag on `run`:

```bash
wreck-it run --goal "Refactor the auth module into separate files" --max-iterations 30
```

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

### Role-Based Tasks

Assign agent roles to tasks for specialised handling:

```json
[
  { "id": "research", "description": "Explore auth libraries and suggest an approach", "status": "pending", "role": "ideas" },
  { "id": "impl", "description": "Implement JWT authentication", "status": "pending", "depends_on": ["research"] },
  { "id": "review", "description": "Evaluate the auth implementation for security issues", "status": "pending", "role": "evaluator", "depends_on": ["impl"] }
]
```

| Role | Purpose |
|------|---------|
| `ideas` | Research, explore, and generate follow-up tasks |
| `implementer` (default) | Write code and make changes |
| `evaluator` | Review and validate completed work |

### Artefact Chaining

Tasks can produce outputs that are automatically injected into downstream tasks:

```json
[
  {
    "id": "design",
    "description": "Write a design spec for the user API",
    "status": "pending",
    "outputs": [{ "kind": "summary", "name": "spec", "path": "spec.md" }]
  },
  {
    "id": "impl",
    "description": "Implement the user API based on the design spec",
    "status": "pending",
    "inputs": ["design/spec"],
    "depends_on": ["design"]
  }
]
```

### Reflection and Re-Planning

Configure the critic-actor reflection loop and adaptive re-planner:

```bash
# Enable 3 reflection rounds (critic reviews each diff before tests)
wreck-it run --reflection-rounds 3

# Trigger re-planning after 3 consecutive failures (default: 2)
wreck-it run --replan-threshold 3

# Disable reflection and re-planning
wreck-it run --reflection-rounds 0 --replan-threshold 0
```

### Provenance and Audit Trail

Inspect the execution history for any task, or export the full run:

```bash
wreck-it provenance --task impl-1
wreck-it export-openclaw --task-file tasks.json --output run.openclaw.json
```

The openclaw export is compatible with the openclaw plan-graph visualiser.

### Recurring Tasks

Use `"kind": "recurring"` to create tasks that automatically reset to
`pending` after completion.  An optional `cooldown_seconds` field sets the
minimum wait between runs:

```json
[
  {
    "id": "docs",
    "description": "Review project structure and update documentation to reflect the current state",
    "status": "pending",
    "kind": "recurring",
    "cooldown_seconds": 86400
  },
  {
    "id": "coverage",
    "description": "Review test coverage. If below 90%, create and execute a plan to increase it",
    "status": "pending",
    "kind": "recurring",
    "cooldown_seconds": 604800
  }
]
```

Tasks without a `kind` field default to `"milestone"` (one-shot) for
backward compatibility.

### Built-in Templates

wreck-it ships with built-in templates that configure a ready-made multi-ralph setup for common workflows.

```bash
# List available templates
wreck-it template list

# Apply a template to the current project
wreck-it template apply engineering-team
```

The **`engineering-team`** template creates four independent ralph contexts:

| Ralph | Task file | Purpose |
|-------|-----------|---------|
| `docs` | `docs-tasks.json` | Periodically reviews and updates project documentation |
| `features` | `features-tasks.json` | Monitors feature work and proposes new features when the backlog is clear |
| `planner` | `planner-tasks.json` | Researches trends and proposes novel features |
| `feature-dev` | `feature-dev-tasks.json` | Executes feature implementation tasks generated by the `features` and `planner` ralphs |

`wreck-it template apply` writes task files into the state worktree and merges ralph entries into `.wreck-it/config.toml`. Files and ralph names that already exist are left untouched (the command is idempotent).

### Epics and Sub-tasks

Group related tasks under a parent **epic** using the `parent_id` field. Use `labels` for free-form categorization.

```json
[
  {
    "id": "epic-auth",
    "description": "Implement full authentication system",
    "status": "pending"
  },
  {
    "id": "auth-design",
    "description": "Write design spec for JWT authentication",
    "status": "pending",
    "parent_id": "epic-auth",
    "labels": ["design"]
  },
  {
    "id": "auth-impl",
    "description": "Implement JWT middleware based on design spec",
    "status": "pending",
    "parent_id": "epic-auth",
    "labels": ["backend"],
    "depends_on": ["auth-design"]
  },
  {
    "id": "auth-tests",
    "description": "Write integration tests for auth endpoints",
    "status": "pending",
    "parent_id": "epic-auth",
    "labels": ["testing"],
    "depends_on": ["auth-impl"]
  }
]
```

A task with no `parent_id` that has other tasks pointing to it via `parent_id` is treated as an **epic**. Sub-tasks can have their own `depends_on`, `role`, and other fields independently. Labels are purely organizational metadata and are not used by the scheduler.

### Per-Task Agent Memory

wreck-it automatically maintains a persistent memory log for each task. After every execution attempt, the outcome and a short summary are appended to `.wreck-it-memory/{task_id}.md`. Before the next attempt, this history is prepended to the agent's prompt so it can learn from prior outcomes and avoid repeating the same mistakes.

```
.wreck-it-memory/
├── auth-impl.md     # "Attempt 1 - Failure: missing import…"
└── auth-tests.md    # "Attempt 1 - Success: all tests pass"
```

This is especially useful for tasks that span multiple cron invocations or require several iterations to complete — the agent accumulates knowledge across runs without any manual intervention.

### Named Ralph Contexts (Multi-Ralph)

For fully independent loops, define named ralphs in `.wreck-it/config.toml`:

```toml
[[ralphs]]
name       = "docs"
task_file  = "docs-tasks.json"
state_file = ".docs-state.json"

[[ralphs]]
name       = "coverage"
task_file  = "coverage-tasks.json"
state_file = ".coverage-state.json"
```

Then run a specific ralph:

```bash
wreck-it run --headless --ralph docs
wreck-it run --headless --ralph coverage
```

Each ralph can have its own GitHub Actions workflow with a separate
schedule.

### Install wreck-it into a Project

The `wreck-it install` command bootstraps a project with the full `engineering-team` setup and ready-to-use GitHub Actions workflows in one step:

```bash
wreck-it install
```

This creates (existing files are never overwritten — safe to re-run):

| File | Description |
|------|-------------|
| `.wreck-it/config.toml` | Pre-populated with the four `engineering-team` ralphs |
| `.wreck-it/plans/` | Directory for cloud-agent plan files |
| `.github/workflows/ralph.yml` | Cron-driven wreck-it workflow |
| `.github/workflows/plan.yml` | On-demand `wreck-it plan` workflow |

### Webhook Notifications

Use `--notify-webhook` to send HTTP POST notifications to one or more URLs whenever a task changes status:

```bash
wreck-it run --notify-webhook https://hooks.example.com/wreck-it
```

You can specify the flag multiple times to notify several endpoints. Failures are logged as warnings and never abort the loop.

Each notification is a JSON object:

```json
{
  "task_id":    "impl-1",
  "status":     "completed",
  "timestamp":  1700000000,
  "description": "Implement the user API endpoint"
}
```

Webhooks can also be configured statically in `.wreck-it/config.toml`:

```toml
notify_webhooks = ["https://hooks.example.com/wreck-it"]
```

### GitHub Issues Integration

wreck-it can automatically open a GitHub Issue when a task starts and close it when the task finishes. This gives you a lightweight dashboard of in-progress and recently completed work inside GitHub.

Enable in `.wreck-it/config.toml`:

```toml
github_issues_enabled = true
github_repo           = "owner/repo"
# github_token is optional if GITHUB_TOKEN is set in the environment
```

When enabled:
- A new issue titled `[wreck-it] <task-id>: <description>` is opened when a task moves to `InProgress`.
- The issue is closed automatically when the task reaches `Completed` or `Failed`.

## Next Steps

- Read [CI & Headless Mode](ci-headless.md) to run wreck-it in GitHub Actions
- Read [Architecture](architecture.md) to understand how it works
- Check out the [GitHub repository](https://github.com/randymarsh77/wreck-it) if you want to help
- Join discussions in GitHub Issues
