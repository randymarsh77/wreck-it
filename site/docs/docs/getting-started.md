---
sidebar_position: 2
---

# Getting Started

This guide walks you through installing and using wreck-it to automate multi-step development tasks.

## Prerequisites

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

## Installation

### Using Nix (Recommended)

```bash
git clone https://github.com/randymarsh77/wreck-it.git
cd wreck-it
nix develop
cargo build --release
```

### Using Cargo

```bash
git clone https://github.com/randymarsh77/wreck-it.git
cd wreck-it
cargo build --release
# Binary at target/release/wreck-it
```

## Quick Start

### 1. Initialize a Task File

```bash
wreck-it init --output tasks.json
```

### 2. Customize Your Tasks

Edit `tasks.json`:

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
  }
]
```

### 3. Run the Loop

```bash
wreck-it run --task-file tasks.json --max-iterations 50
```

The TUI will launch and show current iteration count, task status, real-time logs, and controls.

## TUI Controls

- **Space**: Pause/Resume the loop
- **Q**: Quit the application

## CLI Options

| Option | Description |
|---|---|
| `-t, --task-file <PATH>` | Path to task file |
| `-m, --max-iterations <NUM>` | Maximum iterations |
| `-w, --work-dir <PATH>` | Working directory |
| `--model-provider <copilot\|llama\|github-models>` | Model provider |
| `--api-endpoint <URL>` | Provider endpoint |
| `--verify-command <COMMAND>` | Custom verification command |
| `--headless` | Run without TUI (for CI/CD) |

## Example Workflow

1. **Define Tasks** — Break your feature into specific, actionable tasks
2. **Run with Monitoring** — `wreck-it run --task-file tasks.json`
3. **Review Progress** — Watch the TUI, check `git log --oneline`
4. **Handle Failures** — Pause, review, adjust task descriptions, resume

## Tips for Success

- ✅ Write clear, specific task descriptions
- ✅ Keep tasks atomic — one testable unit per task
- ✅ Start small with 1–2 tasks before complex workflows
- ✅ Always run on a feature branch
- ✅ Monitor progress — don't walk away
