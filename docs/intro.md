---
sidebar_position: 1
slug: /
---

# Introduction

**wreck-it** is a TUI agent harness that uses the GitHub Copilot SDK to perform Ralph Wiggum loops — continuous, external bash-style loops that run AI agent tasks to completion.

## Ralph Wiggum. Cloud Scale.

wreck-it brings autonomous AI agent orchestration to your terminal. Define tasks, let the agents work, and watch your codebase evolve.

## What is a Ralph Wiggum Loop?

The Ralph Wiggum Loop is a continuous execution pattern designed for AI agent workflows. Named after the Simpsons character famous for his persistence ("I'm helping!"), this pattern ensures tasks are completed through persistent iteration.

- **External Loop**: Not an internal AI feature, but an external script running `while true`
- **Persistent Memory**: Uses the filesystem (codebase) as memory rather than chat history
- **Workflow**: Reads task file → Implements change → Runs tests → Commits code → Repeats
- **Safety**: Includes max iterations limit to prevent infinite loops and excessive costs

## Features

- 🎨 **TUI Interface** — Beautiful terminal UI showing tasks, progress, and logs
- 🔄 **Continuous Execution** — Runs until all tasks are complete or max iterations reached
- 📝 **Task Management** — JSON-based task tracking with status persistence, phases, and dependencies
- 🧪 **Automatic Testing** — Runs tests after each task execution (cargo, npm, pytest)
- 💾 **Git Integration** — Automatically commits successful changes
- 🔒 **Safety Limits** — Configurable max iterations to prevent runaway costs
- 🤖 **Headless Mode** — Run without TUI for CI/CD automation
- ☁️ **Cloud Agents** — GitHub Models integration for cloud-scale agent execution
- 🐕 **Dogfooding** — wreck-it develops itself via scheduled agent swarms
- 🧠 **LLM Task Planning** — Generate structured task plans from natural-language goals
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
- ⚡ **GitHub Action** — Use wreck-it in CI via the bundled Docker action
