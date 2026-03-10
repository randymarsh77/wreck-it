---
sidebar_position: 1
slug: /
---

# Introduction

**wreck-it** is an autonomous AI agent orchestrator powered by GitHub Models (or the Copilot SDK). It runs Ralph Wiggum loops — continuous, external bash-style loops that execute AI agent tasks to completion — either **headless in CI** (GitHub Actions, cron schedules) or **interactively via a terminal UI**.

## Ralph Wiggum. Web Scale.

wreck-it brings autonomous AI agent orchestration to your CI pipeline and your terminal. Define tasks, let the agents work, and watch your codebase evolve — on a schedule, in the cloud, or right from your laptop.

### Headless CI & Cloud Agents

The headline feature: run wreck-it in **GitHub Actions** on a cron schedule. In headless mode the loop drives a cloud-agent state machine — creating issues, assigning Copilot, polling for PRs, and merging them when checks pass. State persists between runs on a dedicated branch, so each invocation picks up where the last one left off.

```yaml
# .github/workflows/wreck-it.yml
- uses: actions/checkout@v4
  with:
    token: ${{ secrets.PAT_TOKEN }}
    fetch-depth: 0
- uses: randymarsh77/wreck-it/action@main
  env:
    GITHUB_TOKEN: ${{ secrets.PAT_TOKEN }}
```

> A Personal Access Token (`PAT_TOKEN`) is required because wreck-it assigns coding agents to issues and merges their PRs — operations the default `GITHUB_TOKEN` cannot perform.

👉 **[CI & Headless Guide](ci-headless.md)** — full setup instructions and example workflows.

### Interactive TUI

For local development, wreck-it provides a rich terminal UI showing tasks, progress, and real-time logs with pause/resume controls.

## What is a Ralph Wiggum Loop?

The Ralph Wiggum Loop is a continuous execution pattern designed for AI agent workflows. Named after the Simpsons character famous for his persistence ("I'm helping!"), this pattern ensures tasks are completed through persistent iteration.

- **External Loop**: Not an internal AI feature, but an external script running `while true`
- **Persistent Memory**: Uses the filesystem (codebase) as memory rather than chat history
- **Workflow**: Reads task file → Implements change → Runs tests → Commits code → Repeats
- **Safety**: Includes max iterations limit to prevent infinite loops and excessive costs

## Features

- ⚡ **GitHub Action** — Use wreck-it in CI via the bundled Docker action
- 🤖 **Headless Mode** — Run without TUI for CI/CD automation
- ☁️ **Cloud Agents** — GitHub Models integration for cloud-scale agent execution
- 🐕 **Dogfooding** — wreck-it develops itself via scheduled agent swarms
- 🧠 **LLM Task Planning** — Generate structured task plans from natural-language goals
- 🎨 **TUI Interface** — Beautiful terminal UI showing tasks, progress, and logs
- 🔄 **Continuous Execution** — Runs until all tasks are complete or max iterations reached
- 📝 **Task Management** — JSON-based task tracking with status persistence, phases, and dependencies
- 🧪 **Automatic Testing** — Runs tests after each task execution (cargo, npm, pytest)
- 💾 **Git Integration** — Automatically commits successful changes
- 🔒 **Safety Limits** — Configurable max iterations to prevent runaway costs
- 🎭 **Role-Based Agents** — Assign `ideas`, `implementer`, or `evaluator` roles to tasks
- 🔁 **Critic-Actor Reflection** — Optional critic feedback loop to refine agent output
- 🛠️ **Adaptive Re-Planning** — Automatically restructure tasks after consecutive failures
- 📦 **Artefact Store** — Chain task outputs as inputs to downstream tasks
- 🔍 **Provenance Tracking** — Full audit trail of every agent execution, exportable as openclaw JSON
- 🔂 **Recurring Tasks** — Tasks that automatically reset after a configurable cooldown
- 🏗️ **Parallel Execution** — Phase-based concurrent task execution
- 📊 **Intelligent Scheduling** — Multi-factor scoring for task ordering
- 🌐 **Gastown Cloud Runtime** — Offload tasks to the gastown cloud agent service
- 🎯 **Multi-Ralph Contexts** — Run independent loops per context
- 🧐 **Agent-Evaluated Preconditions** — Let an agent decide whether a task should run, for nuanced recurring task control in powerful ralph loops
- 🏷️ **Epics & Sub-tasks** — Organize tasks into epics with hierarchical sub-tasks and progress tracking
- 💡 **Per-Task Agent Memory** — Agents learn from prior attempts via persistent per-task memory files
- 🔔 **Webhook Notifications** — HTTP POST alerts on task status transitions; failures are logged as warnings and never abort the loop
- 🐙 **GitHub Issues Integration** — Automatically open/close GitHub Issues as tasks start and finish for in-repo progress tracking
