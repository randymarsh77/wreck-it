---
sidebar_position: 3
---

# Architecture

## Overview

The Ralph Wiggum Loop is a continuous execution pattern designed for AI agent workflows. This document describes the architecture of wreck-it's core components.

## Key Concepts

### External Loop Pattern

Unlike internal AI chat loops, this is an **external bash-style loop**:

```
while true:
    if all_tasks_complete():
        break
    execute_next_task()
    run_tests()
    commit_changes()
    if max_iterations_reached():
        break
```

### Filesystem as Memory

- The codebase itself serves as persistent memory
- Task status is saved to `tasks.json` after each iteration
- Git commits provide a history of changes
- No reliance on chat history or session state

### Task Workflow

Each iteration follows this sequence:

1. Load tasks from `tasks.json`
2. Find next pending task
3. Execute task via Copilot SDK (or agent)
4. Run tests to verify changes
5. Commit successful changes to git
6. Update task status
7. Save tasks back to file

### Safety Mechanisms

- **Max Iterations**: Prevents infinite loops and cost overruns
- **Test Validation**: Only commits changes that pass tests
- **Status Tracking**: Failed tasks are marked and can be reviewed
- **Git History**: Every change is tracked and reversible

## Components

### RalphLoop

Core loop implementation that manages iteration state, loads/saves task state, orchestrates agent execution, and controls the loop lifecycle.

### AgentClient

Interface to the Copilot SDK — executes individual tasks, reads codebase context, runs tests, and commits changes. Supports multiple model providers including GitHub Copilot, local Llama, and GitHub Models.

### CloudAgent

Cloud/LLM agent implementation for headless execution. Integrates with GitHub Models for cloud-scale agent orchestration.

### TaskManager

Handles task persistence — loads tasks from JSON, saves task state, and finds the next pending task.

### TuiApp

Terminal UI for monitoring — shows current iteration, displays task status, streams real-time logs, and allows pause/resume.

### HeadlessRunner

Headless execution mode for CI/CD automation. Reads configuration from `.wreck-it.toml` and runs without user interaction.

## Data Flow

```
┌─────────────┐
│ tasks.json  │
└──────┬──────┘
       │
       ▼
┌──────────────────┐
│   RalphLoop      │
│  - iteration     │
│  - state         │
└──────┬───────────┘
       │
       ▼
┌──────────────────┐
│  AgentClient     │◄────── GitHub Copilot SDK / GitHub Models
│  - execute_task  │
│  - run_tests     │
│  - commit        │
└──────┬───────────┘
       │
       ▼
┌──────────────────┐
│   Codebase       │
│   (git repo)     │
└──────────────────┘
```

## Configuration

### `.wreck-it.toml`

```toml
task_file = "tasks.json"
max_iterations = 50
verify_command = "cargo test && cargo clippy -- -D warnings"
evaluation_mode = "agent_file"
```

### Environment Variables

- `GITHUB_TOKEN`: GitHub token for API access
- `COPILOT_API_TOKEN`: GitHub Copilot API token

### Command Line Options

- `--task-file`: Path to task JSON (default: tasks.json)
- `--max-iterations`: Safety limit (default: 100)
- `--work-dir`: Repository directory (default: .)
- `--model-provider`: copilot, llama, or github-models
- `--headless`: Run without TUI
