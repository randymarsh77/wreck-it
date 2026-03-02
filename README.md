# wreck-it 🔧

**Ralph Wiggum. Cloud Scale.**

A TUI agent harness that uses the Copilot SDK to perform Ralph Wiggum loops — autonomous AI agent orchestration for your codebase.

🌐 **[wreckit.app](https://wreckit.app)** · 📖 **[Documentation](https://wreckit.app/docs/)**

## What is a Ralph Wiggum Loop?

The Ralph Wiggum Loop is a bash-style loop that continuously executes AI agent tasks until completion:

- **External Loop**: Not an internal AI feature, but an external script running `while true`
- **Persistent Memory**: Uses the filesystem (codebase) as memory rather than chat history
- **Workflow**: Reads task file → Implements change → Runs tests → Commits code → Repeats
- **Safety**: Includes max iterations limit to prevent infinite loops and excessive costs

## Features

- 🎨 **TUI Interface**: Beautiful terminal UI showing tasks, progress, and real-time logs
- 🔄 **Continuous Execution**: Runs until all tasks are complete or max iterations reached
- 📝 **Task Management**: JSON-based task tracking with status persistence, phases, and dependencies
- 🧪 **Automatic Testing**: Runs tests after each task execution (cargo, npm, pytest)
- 💾 **Git Integration**: Automatically commits successful changes
- 🔒 **Safety Limits**: Configurable max iterations to prevent runaway costs
- 🤖 **Headless Mode**: Run without TUI for CI/CD automation
- ☁️ **Cloud Agents**: GitHub Models integration for cloud-scale agent execution
- 🐕 **Dogfooding**: wreck-it develops itself via scheduled agent swarms
- 🧠 **LLM Task Planning**: Generate structured task plans from natural-language goals (`wreck-it plan`)
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
- ⚡ **GitHub Action**: Use wreck-it in CI via the bundled Docker action

## Installation

### Prerequisites

1. **GitHub Copilot CLI**: Install the GitHub Copilot CLI and ensure it's available in your PATH:
   ```bash
   # Follow the GitHub Copilot CLI installation guide
   # https://docs.github.com/en/copilot/how-tos/set-up/install-copilot-cli
   ```

2. **GitHub Copilot Subscription**: A GitHub Copilot subscription is required to use the SDK. See [GitHub Copilot pricing](https://github.com/features/copilot#pricing) for details.

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

1. **Authenticate with GitHub Copilot**:
   ```bash
   # Login to GitHub Copilot CLI
   copilot auth login
   ```

2. **Verify Copilot is working**:
   ```bash
   copilot --version
   ```

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
- `--model-provider <copilot|llama|github-models>`: Model provider
- `--api-endpoint <URL>`: Provider endpoint (for local llama use `http://localhost:11434/v1`)
- `--api-token <TOKEN>`: API token (can also be set via `COPILOT_API_TOKEN` env var)
- `--verify-command <COMMAND>`: Custom shell command/script to verify completion (non-zero exit marks task failed; only use trusted commands)
- `--evaluation-mode <command|agent-file>`: How task completeness is checked (default: `command`)
- `--completeness-prompt <PROMPT>`: Prompt for the evaluation agent (used with `--evaluation-mode=agent-file`)
- `--completion-marker-file <PATH>`: Marker file the evaluation agent writes when done (default: `.task-complete`)
- `--headless`: Run without TUI for CI environments
- `--reflection-rounds <NUM>`: Max critic-actor reflection rounds (default: `2`, `0` to disable)
- `--replan-threshold <NUM>`: Consecutive failures before adaptive re-planning (default: `2`, `0` to disable)
- `--ralph <NAME>`: Named ralph context from `.wreck-it/config.toml`
- `--goal <TEXT>`: Generate a task plan from a natural-language goal before starting

**Note**: The Copilot CLI must be authenticated and available in your PATH. The SDK will automatically connect to the Copilot CLI server.

### Plan Tasks from a Goal

```bash
wreck-it plan --goal "Build a REST API with authentication" --output tasks.json
```

Generates a structured `tasks.json` from a natural-language description using the configured LLM.

### Inspect Provenance

```bash
# Display the audit trail for a specific task
wreck-it provenance --task impl-1

# Export the full run as an openclaw-compatible JSON document
wreck-it export-openclaw --task-file tasks.json --output run.openclaw.json
```

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
    { "kind": "file", "name": "api", "path": "src/api.rs" }
  ],
  "runtime": "local"
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

## GitHub Action

wreck-it ships a Docker-based GitHub Action for headless CI use. See [`action/`](action/) for the Dockerfile, entrypoint, and a sample workflow.

```yaml
- uses: randymarsh77/wreck-it/action@main
  env:
    COPILOT_API_TOKEN: ${{ secrets.COPILOT_API_TOKEN }}
```

Inputs:
- `max_iterations` — Maximum loop iterations (default: `100`)
- `verify_command` — Shell command to verify task completion
- `state_branch` — Git branch for wreck-it state (default: `wreck-it-state`)

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
- [Architecture](https://wreckit.app/docs/architecture)
- [Roadmap](https://wreckit.app/docs/roadmap)
- [Research Notes](https://wreckit.app/docs/research-notes)

## License

MIT - See [LICENSE](LICENSE) for details
