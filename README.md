# wreck-it 🔧

**Ralph Wiggum. Web Scale.**

Autonomous AI agent orchestration for your codebase. Run headless in GitHub Actions on a cron schedule, or interactively via the terminal UI — powered by GitHub Models or the Copilot SDK.

🌐 **[wreckit.app](https://wreckit.app)** · 📖 **[Documentation](https://wreckit.app/docs/)** · 🤖 **[CI & Headless Guide](https://wreckit.app/docs/ci-headless)**

## What is a Ralph Wiggum Loop?

The Ralph Wiggum Loop is a bash-style loop that continuously executes AI agent tasks until completion:

- **External Loop**: Not an internal AI feature, but an external script running `while true`
- **Persistent Memory**: Uses the filesystem (codebase) as memory rather than chat history
- **Workflow**: Reads task file → Implements change → Runs tests → Commits code → Repeats
- **Safety**: Includes max iterations limit to prevent infinite loops and excessive costs

## Features

- ⚡ **GitHub Action**: Use wreck-it in CI via the bundled Docker action
- 🤖 **Headless Mode**: Run without TUI for CI/CD automation
- ☁️ **Cloud Agents**: GitHub Models integration for cloud-scale agent execution
- 🐕 **Dogfooding**: wreck-it develops itself via scheduled agent swarms
- 🧠 **LLM Task Planning**: Generate structured task plans from natural-language goals (`wreck-it plan`)
- 🎨 **TUI Interface**: Beautiful terminal UI showing tasks, progress, and real-time logs
- 🔄 **Continuous Execution**: Runs until all tasks are complete or max iterations reached
- 📝 **Task Management**: JSON-based task tracking with status persistence, phases, and dependencies
- 🧪 **Automatic Testing**: Runs tests after each task execution (cargo, npm, pytest)
- 💾 **Git Integration**: Automatically commits successful changes
- 🔒 **Safety Limits**: Configurable max iterations to prevent runaway costs
- 🎭 **Role-Based Agents**: Assign `ideas`, `implementer`, or `evaluator` roles to tasks
- 🔁 **Critic-Actor Reflection**: Optional critic feedback loop to refine agent output before tests
- 🛠️ **Adaptive Re-Planning**: Automatically restructure tasks after consecutive failures
- 📦 **Artefact Store**: Chain task outputs as inputs to downstream tasks
- 🔍 **Provenance Tracking**: Full audit trail of every agent execution, exportable as openclaw JSON
- 🔂 **Recurring Tasks**: Tasks that automatically reset after a configurable cooldown
- 🏗️ **Parallel Execution**: Phase-based concurrent task execution via tokio
- 📊 **Intelligent Scheduling**: Multi-factor scoring (priority, complexity, fan-out, failure history)
- 🌐 **Gastown Cloud Runtime**: Offload tasks to the gastown cloud agent service
- 🎯 **Multi-Ralph Contexts**: Run independent loops with separate task/state files per context
- 🧐 **Agent-Evaluated Preconditions**: Let an agent decide whether a task should run, enabling nuanced re-run criteria for recurring tasks in powerful ralph loops
- 🏷️ **Epics & Sub-tasks**: Organize tasks into epics with hierarchical sub-tasks and progress tracking
- 💡 **Per-Task Agent Memory**: Agents learn from prior attempts via persistent per-task memory files
- 🔔 **Webhook Notifications**: HTTP POST alerts on task status transitions (`--notify-webhook <URL>`); failures are logged as warnings and never abort the loop
- 🐛 **GitHub Issues Integration**: Automatically open a GitHub Issue when a task moves to `InProgress` and close it when the task reaches `Completed` or `Failed` (`--github-issues --github-repo owner/repo`)

## Installation

### Prerequisites

wreck-it supports multiple model providers. Choose one:

1. **GitHub Models** *(recommended for CI)*: Use GitHub's hosted model inference. Requires a `GITHUB_TOKEN` with `models:read` permission — no extra subscription needed.

2. **Copilot SDK**: Use the GitHub Copilot CLI. Requires a [GitHub Copilot subscription](https://github.com/features/copilot#pricing) and `copilot auth login`.

3. **Local Llama**: Use a local Ollama instance. No subscription needed.

### Using Nix Flakes (Recommended)

```bash
# Enter development shell
nix develop

# Or build the project
nix build
```

### Using Cargo

```bash
cargo build --release
```

## Usage

### Setup

Choose a model provider:

- **GitHub Models** *(recommended)*: Set `GITHUB_TOKEN` in your environment.
- **Copilot SDK**: Run `copilot auth login` and verify with `copilot --version`.
- **Local Llama**: Start Ollama and use `--model-provider llama --api-endpoint http://localhost:11434/v1`.

### Initialize a Task File

```bash
wreck-it init
```

This creates a sample `tasks.json` file with example tasks.

### Run the Ralph Wiggum Loop

```bash
wreck-it run
```

Options:
- `-t, --task-file <PATH>`: Path to task file (default: `tasks.json`)
- `-m, --max-iterations <NUM>`: Maximum iterations (default: `100`)
- `-w, --work-dir <PATH>`: Working directory (default: `.`)
- `--model-provider <github-models|copilot|llama>`: Model provider
- `--api-endpoint <URL>`: Provider endpoint (for local llama use `http://localhost:11434/v1`)
- `--api-token <TOKEN>`: API token (can also be set via `COPILOT_API_TOKEN` env var)
- `--verify-command <COMMAND>`: Custom shell command/script to verify completion (non-zero exit marks task failed; only use trusted commands)
- `--evaluation-mode <command|agent-file>`: How task completeness is checked (default: `command`)
- `--completeness-prompt <PROMPT>`: Prompt for the evaluation agent (used with `--evaluation-mode=agent-file`)
- `--completion-marker-file <PATH>`: Marker file the evaluation agent writes when done (default: `.task-complete`)
- `--headless`: Run without TUI for CI environments
- `--reflection-rounds <NUM>`: Max critic-actor reflection rounds (default: `2`, `0` to disable)
- `--replan-threshold <NUM>`: Consecutive failures before adaptive re-planning (default: `2`, `0` to disable)
- `--ralph <NAME>`: Named ralph context from `.wreck-it/config.toml`; use `--ralph all` (headless only) to run every ralph sequentially
- `--goal <TEXT>`: Generate a task plan from a natural-language goal before starting
- `--github-issues`: Enable GitHub Issues integration — open an issue when a task moves to `InProgress` and close it when it reaches `Completed` or `Failed`
- `--github-repo <OWNER/REPO>`: GitHub repository for the Issues integration (e.g. `acme/my-project`); required when `--github-issues` is set
- `--github-token <TOKEN>`: GitHub personal-access token or fine-grained token with `issues: write` permission; falls back to the `GITHUB_TOKEN` environment variable when not provided

**Note**: When using `--model-provider copilot`, the Copilot CLI must be authenticated and available in your PATH. When using `--model-provider github-models`, set `GITHUB_TOKEN` in your environment.

### Plan Tasks from a Goal

```bash
wreck-it plan --goal "Build a REST API with authentication" --output tasks.json
```

Generates a structured `tasks.json` from a natural-language description using the configured LLM.

### Apply a Built-in Template

```bash
# List available templates
wreck-it template list

# Apply the "engineering-team" template (multi-ralph setup with recurring docs,
# feature management, and research planning tasks)
wreck-it template apply engineering-team
```

The `engineering-team` template configures four ralph contexts (`docs`, `features`, `planner`, `feature-dev`) with recurring tasks and writes the corresponding task files into the state worktree. Ralph entries that already exist in `.wreck-it/config.toml` are left untouched (idempotent).

### Install wreck-it into a Project

Bootstrap a new project with the full `engineering-team` template and ready-to-use GitHub Actions workflows in a single step:

```bash
wreck-it install
```

This creates:
- `.wreck-it/config.toml` — pre-populated with the four `engineering-team` ralphs (`docs`, `features`, `planner`, `feature-dev`)
- `.wreck-it/plans/` — directory for cloud-agent plan files
- `.github/workflows/ralph.yml` — the main cron-driven wreck-it workflow
- `.github/workflows/plan.yml` — a workflow for running `wreck-it plan` on demand

Existing files are never overwritten; the command is safe to re-run (idempotent).

### Inspect Provenance

```bash
# Display the audit trail for a specific task
wreck-it provenance --task impl-1

# Export the full run as an openclaw-compatible JSON document
wreck-it export-openclaw --task-file tasks.json --output run.openclaw.json
```

### Export the Dependency Graph

Visualize task dependencies as a Mermaid flowchart or GraphViz DOT diagram:

```bash
# Print Mermaid flowchart to stdout (default)
wreck-it graph

# Write Mermaid output to a file
wreck-it graph --output graph.mmd

# Generate a GraphViz DOT diagram
wreck-it graph --format dot --output graph.dot

# Use a non-default task file
wreck-it graph --task-file .wreck-it/my-tasks.json
```

Paste the Mermaid output into [mermaid.live](https://mermaid.live) to render it interactively.  
Nodes are colour-coded by status: **gray** = pending, **blue** = in-progress, **green** = completed, **red** = failed.  
If circular dependencies are detected, a warning is printed to stderr before the graph is emitted.

Options:
- `-t, --task-file <PATH>`: Task file to read (default: `tasks.json`)
- `-f, --format <mermaid|dot>`: Output format (default: `mermaid`)
- `-o, --output <PATH>`: Write output to file instead of stdout

### TUI Controls

- **Space**: Pause/Resume the loop
- **Q**: Quit the application

## Task File Format

Tasks are defined in a JSON file. A minimal task only requires `id`, `description`, and `status`:

```json
[
  {
    "id": "1",
    "description": "Implement feature X",
    "status": "pending"
  }
]
```

A task with all available fields:

```json
{
  "id": "impl-1",
  "description": "Implement the user API endpoint",
  "status": "pending",
  "role": "implementer",
  "kind": "milestone",
  "phase": 2,
  "depends_on": ["design-1"],
  "priority": 5,
  "complexity": 3,
  "inputs": ["design-1/spec"],
  "outputs": [
    { "kind": "file", "name": "api", "path": "cli/src/api.rs" }
  ],
  "runtime": "local",
  "parent_id": "epic-auth",
  "labels": ["backend", "api"],
  "timeout_seconds": 300,
  "max_retries": 2
}
```

| Field | Description | Default |
|-------|-------------|---------|
| `id` | Unique task identifier | *(required)* |
| `description` | What the agent should do | *(required)* |
| `status` | `pending`, `inprogress`, `completed`, or `failed` | *(required)* |
| `role` | Agent role: `ideas`, `implementer`, `evaluator` | `implementer` |
| `kind` | `milestone` (one-shot) or `recurring` (resets after cooldown) | `milestone` |
| `cooldown_seconds` | Seconds before a recurring task resets (optional, only used with `recurring`) | *(none)* |
| `phase` | Execution phase (lower runs first; same phase may run in parallel) | `1` |
| `depends_on` | IDs of tasks that must complete first | `[]` |
| `priority` | Scheduling priority (higher = sooner) | `0` |
| `complexity` | Estimated complexity 1–10 (lower preferred for quick wins) | `1` |
| `inputs` | Artefact references (`"task-id/artefact-name"`) injected into prompt | `[]` |
| `outputs` | Artefacts to persist on completion (`kind`, `name`, `path`) | `[]` |
| `runtime` | `local` or `gastown` (cloud execution) | `local` |
| `precondition_prompt` | Agent-evaluated precondition; task is skipped when the agent determines the condition is not met | *(none)* |
| `parent_id` | ID of the parent task (epic); marks this task as a sub-task | *(none)* |
| `labels` | Free-form labels for categorization (e.g. board columns, tags) | `[]` |
| `timeout_seconds` | Maximum wall-clock seconds the agent may run; the task is marked failed if the limit is exceeded | *(none)* |
| `max_retries` | Maximum number of automatic retries after failure (`N` means up to `N+1` total attempts); auto-retry is skipped when the adaptive re-planner takes over | *(none)* |

## GitHub Action

wreck-it ships a Docker-based GitHub Action for headless CI use. Add it to any workflow to run autonomous agent loops on a schedule. See [`action/`](action/) for the Dockerfile, entrypoint, and a sample workflow.

### Quick Start

```yaml
# .github/workflows/wreck-it.yml
name: wreck-it loop

on:
  schedule:
    - cron: '*/30 * * * *'
  workflow_dispatch:

permissions:
  contents: write
  pull-requests: write
  issues: write
  models: read

jobs:
  wreck-it:
    runs-on: ubuntu-latest
    steps:
      - uses: actions/checkout@v4
        with:
          token: ${{ secrets.PAT_TOKEN }}
          fetch-depth: 0

      - name: Run wreck-it
        uses: randymarsh77/wreck-it/action@main
        env:
          GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}
```

A **Personal Access Token** (`PAT_TOKEN` secret) with `repo` and `models:read` scopes is required. wreck-it assigns coding agents to issues and merges their PRs — operations the default `GITHUB_TOKEN` cannot perform. See [Creating a PAT](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens).

### Inputs

| Input | Description | Default |
|-------|-------------|---------|
| `model_provider` | Model provider (`github-models`, `copilot`, or `llama`) | `github-models` |
| `max_iterations` | Maximum loop iterations | `100` |
| `verify_command` | Shell command to verify task completion | *(none)* |
| `state_branch` | Git branch for wreck-it state | `wreck-it-state` |

For more examples — including building from source, using GitHub Models, multi-ralph workflows, and custom verification commands — see the full [CI & Headless Guide](https://wreckit.app/docs/ci-headless).

## Development

### Build

```bash
cargo build
```

### Test

```bash
cargo test
```

### Run Locally

```bash
cargo run -- run --task-file tasks.json
```

## CI/CD

This project includes GitHub Actions workflows for:
- **CI** (`ci.yml`): Build (debug + release), tests, Clippy linting, format checking
- **Site deployment** to [wreckit.app](https://wreckit.app) via Cloudflare Pages on push to master
- **Dogfooding** (`ralph.yml`): Scheduled agent swarms that run wreck-it on itself every 10 minutes

## Documentation

Full documentation is available at [wreckit.app/docs/](https://wreckit.app/docs/), covering:
- [Introduction](https://wreckit.app/docs/)
- [Getting Started](https://wreckit.app/docs/getting-started)
- [CI & Headless Mode](https://wreckit.app/docs/ci-headless)
- [GitHub App Integration](https://wreckit.app/docs/github-app) — Webhook events, label behavior, state commit safeguards
- [Architecture](https://wreckit.app/docs/architecture)
- [Roadmap](https://wreckit.app/docs/roadmap)
- [Research Notes](https://wreckit.app/docs/research-notes)

## License

MIT - See [LICENSE](LICENSE) for details
