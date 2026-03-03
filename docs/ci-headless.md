---
sidebar_position: 3
---

# CI & Headless Mode

wreck-it is designed to run autonomously in CI environments. In **headless mode** the TUI is disabled and the Ralph Wiggum loop drives a cloud-agent state machine — creating GitHub issues, assigning Copilot, polling for PRs, and merging them when checks pass — all on a cron schedule.

The bundled Docker action uses **GitHub Models** by default, so no extra subscription or SDK setup is needed — just a Personal Access Token with `repo` and `models:read` scopes.

> **Why a PAT?** wreck-it assigns coding agents to issues (via GraphQL) and merges the PRs they produce. The default `GITHUB_TOKEN` does not have permission for these operations, so a Personal Access Token is always required.

## How It Works

In a CI environment the loop does not run a local AI model. Instead it orchestrates a cloud coding-agent through a state machine:

```
NeedsTrigger  → create GitHub issue → assign Copilot
AgentWorking  → poll for linked PR
NeedsVerification → merge PR when checks pass
Completed     → mark task done, advance to next
```

State is persisted to a dedicated orphan branch (`wreck-it-state` by default) via a git worktree. Each scheduled workflow run picks up where the last one left off — no state is lost between cron invocations.

## Quick Start with the GitHub Action

The fastest way to add wreck-it to your project is the bundled Docker-based GitHub Action. It uses GitHub Models by default.

### 1. Add a workflow file

Create `.github/workflows/wreck-it.yml`:

```yaml
name: wreck-it loop

on:
  schedule:
    - cron: '*/30 * * * *'   # every 30 minutes
  workflow_dispatch:          # allow manual triggers

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
          fetch-depth: 0    # needed to access the state branch

      - name: Run wreck-it
        uses: randymarsh77/wreck-it/action@main
        env:
          GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}
```

### 2. Create a `PAT_TOKEN` secret

Create a [Personal Access Token](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens) with `repo` and `models:read` scopes. Then go to **Settings → Secrets and variables → Actions** in your repository and add it as a `PAT_TOKEN` secret.

A PAT is required because wreck-it assigns coding agents to issues and merges their PRs — operations the default `GITHUB_TOKEN` cannot perform.

### 3. Push and trigger

Push the workflow file. You can trigger the first run immediately via **Actions → wreck-it loop → Run workflow**.

## Action Inputs

| Input | Description | Default |
|-------|-------------|---------|
| `model_provider` | Model provider (`github-models`, `copilot`, or `llama`) | `github-models` |
| `max_iterations` | Maximum loop iterations per run | `100` |
| `verify_command` | Shell command to verify task completion | *(none)* |
| `state_branch` | Orphan branch used to persist state between runs | `wreck-it-state` |

> **Security note**: The `verify_command` input is executed as a shell command inside the runner. Only use trusted commands — never pass untrusted or user-supplied input.

## Example Workflows

### Basic Scheduled Loop

The simplest setup — run wreck-it every 30 minutes using the action:

```yaml
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

### Custom Verify Command

Use a custom verification command so wreck-it can confirm tasks are really done:

```yaml
name: wreck-it with verification

on:
  schedule:
    - cron: '0 */2 * * *'   # every 2 hours
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
        with:
          max_iterations: '50'
          verify_command: 'cargo test'
        env:
          GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}
```

### Using Copilot SDK

To use the Copilot SDK instead of GitHub Models, set the `model_provider` input:

```yaml
      - name: Run wreck-it
        uses: randymarsh77/wreck-it/action@main
        with:
          model_provider: 'copilot'
        env:
          GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}
          COPILOT_API_TOKEN: ${{ secrets.COPILOT_API_TOKEN }}
```

### Build from Source

If you need a specific wreck-it version or want full control over the build, you can build from source instead of using the Docker action:

```yaml
name: wreck-it (from source)

on:
  schedule:
    - cron: '*/10 * * * *'
  workflow_dispatch:
    inputs:
      max_iterations:
        description: 'Override max iterations'
        required: false
        default: '5'

concurrency:
  group: wreck-it-loop
  cancel-in-progress: false

permissions:
  contents: write
  pull-requests: write
  issues: write
  models: read

jobs:
  wreck-it:
    runs-on: ubuntu-latest
    timeout-minutes: 30
    steps:
      - uses: actions/checkout@v4
        with:
          token: ${{ secrets.PAT_TOKEN }}
          fetch-depth: 0

      - name: Configure git
        run: |
          git config user.name "wreck-it[bot]"
          git config user.email "wreck-it[bot]@users.noreply.github.com"

      - uses: dtolnay/rust-toolchain@stable

      - uses: Swatinem/rust-cache@v2

      - name: Build wreck-it
        run: cargo build --release

      - name: Run wreck-it
        run: |
          ./target/release/wreck-it run \
            --headless \
            --work-dir . \
            --model-provider github-models \
            ${MAX_ITER:+--max-iterations "$MAX_ITER"}
        env:
          GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}
          MAX_ITER: ${{ inputs.max_iterations }}

      - name: Push state branch
        run: |
          STATE_BRANCH="wreck-it-state"
          if git rev-parse --verify "refs/heads/${STATE_BRANCH}" >/dev/null 2>&1; then
            git push origin "${STATE_BRANCH}"
          fi
```

### Multiple Independent Loops (Multi-Ralph)

Run separate agent loops for different concerns — for example one for documentation and one for test coverage:

```toml
# .wreck-it/config.toml
state_branch = "wreck-it-state"
state_root   = ".wreck-it"

[[ralphs]]
name       = "docs"
task_file  = "docs-tasks.json"
state_file = ".docs-state.json"

[[ralphs]]
name       = "coverage"
task_file  = "coverage-tasks.json"
state_file = ".coverage-state.json"
```

Each ralph gets its own workflow:

```yaml
# .github/workflows/ralph-docs.yml
name: wreck-it docs loop

on:
  schedule:
    - cron: '0 */6 * * *'   # every 6 hours
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

      - uses: dtolnay/rust-toolchain@stable
      - uses: Swatinem/rust-cache@v2

      - run: cargo build --release

      - name: Run docs ralph
        run: ./target/release/wreck-it run --headless --ralph docs
        env:
          GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}

      - name: Push state
        run: |
          if git rev-parse --verify refs/heads/wreck-it-state >/dev/null 2>&1; then
            git push origin wreck-it-state
          fi
```

## Running Headless Locally

You don't need GitHub Actions to use headless mode. It works anywhere you can run wreck-it:

```bash
# Run headless with the default task file
wreck-it run --headless

# Specify a task file and iteration limit
wreck-it run --headless --task-file tasks.json --max-iterations 20

# Generate a plan and run headless in one step
wreck-it run --headless --goal "Refactor the auth module" --max-iterations 30

# Use a named ralph context
wreck-it run --headless --ralph docs
```

The `--headless` flag disables the TUI and writes structured log output to stdout, making it suitable for any CI system — not just GitHub Actions.

## State Persistence

wreck-it stores all state (task status, agent memory, artefacts, provenance) on a dedicated orphan branch via a git worktree:

- **Branch**: `wreck-it-state` (configurable via `--state-branch` or action input)
- **Location**: `.wreck-it/state/` worktree in the repo checkout
- **Contents**: `tasks.json`, `.wreck-it-state.json`, artefacts, provenance records

This means every cron invocation resumes exactly where the previous run left off. There is no external database or service to configure.

## Required Permissions

A **Personal Access Token** is always required for wreck-it CI workflows. The default `GITHUB_TOKEN` cannot assign agents to issues or merge agent-produced PRs.

Create a [fine-grained or classic PAT](https://docs.github.com/en/authentication/keeping-your-account-and-data-secure/managing-your-personal-access-tokens) with these scopes and store it as a `PAT_TOKEN` repository secret:

| Scope | Reason |
|-------|--------|
| `repo` | Push commits, create/merge PRs, create/assign issues |
| `models:read` | *(only for `github-models` provider)* Access GitHub Models API |

The workflow also needs these `permissions` declarations:

| Permission | Reason |
|------------|--------|
| `contents: write` | Push state branch commits and agent code changes |
| `pull-requests: write` | Cloud agents create PRs against the default branch |
| `issues: write` | wreck-it creates issues to trigger cloud agents |
| `models: read` | *(only for `github-models` provider)* Access GitHub Models API |

## Tips

- **Start with `workflow_dispatch`**: Enable manual triggering so you can test before enabling the cron schedule.
- **Set `concurrency`**: Use a concurrency group with `cancel-in-progress: false` to prevent overlapping runs.
- **Set `timeout-minutes`**: Prevent runaway jobs from consuming runner minutes.
- **Use recurring tasks**: Combine `milestone` tasks (one-shot work) with `recurring` tasks (periodic maintenance) in a single task file for a self-improving codebase.
- **Monitor via provenance**: Export the openclaw audit trail to inspect what the agent did across runs.

## Next Steps

- [Getting Started](getting-started.md) — Local installation and TUI usage
- [Architecture](architecture.md) — How the Ralph Wiggum loop, scheduling, and cloud agents work under the hood
- [GitHub App Integration](github-app.md) — Webhook-driven operation via a GitHub App for real-time event processing
