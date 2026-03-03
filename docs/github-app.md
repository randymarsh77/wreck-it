---
sidebar_position: 4
---

# GitHub App Integration

wreck-it can be driven by a **GitHub App webhook** in addition to (or instead of) the cron-based headless runner. The webhook approach uses a [Cloudflare Worker](https://developers.cloudflare.com/workers/) that reacts to GitHub events in real time, advancing the task state machine without waiting for the next scheduled run.

This document covers the full integration surface: which events trigger iterations, how state updates propagate, how labels are used, and how the webhook and cron modes interact.

## Overview

```
GitHub Event (issues / push / pull_request)
        │
        ▼
 ┌─────────────────┐
 │  Cloudflare      │
 │  Worker (WASM)   │
 │                  │
 │  1. Verify sig   │
 │  2. Parse event  │
 │  3. Read config  │◄──── GitHub API (Contents)
 │  4. Read state   │◄──── .wreck-it/config.toml + state branch
 │  5. Process iter │
 │  6. Commit state │────► GitHub API (Contents) ─── only when a task is started
 └─────────────────┘
```

The worker reads the repository's `.wreck-it/config.toml` (from the default branch) to discover the state branch and ralph contexts. It then reads the task and state files from the state branch, selects the next eligible task, and writes updated files back — all via the GitHub Contents API.

## Events That Trigger Iterations

The worker subscribes to three GitHub webhook event types. All other events are ignored with an `event ignored` response.

| Event | Action filter | When it fires |
|-------|---------------|---------------|
| `issues` | `opened` or `labeled` | An issue is created or labeled **and** carries the `wreck-it` label |
| `push` | *(any)* | A push to any branch (typically the state branch after a state commit) |
| `pull_request` | `closed` with `merged: true` | A pull request is merged (signals task completion) |
| `ping` | — | GitHub's setup verification ping; the worker responds with `pong` |

### Issue events

The worker only processes issue events when the issue has the `wreck-it` label. Both the `opened` and `labeled` actions are handled:

- **`opened` + has label**: Fires when wreck-it (or a user) creates an issue with the `wreck-it` label already applied.
- **`labeled` + has label**: Fires when the `wreck-it` label is added to an existing issue.

This means creating an issue with the `wreck-it` label — which is exactly what the headless runner does (see [Label Behavior](#label-behavior) below) — automatically triggers a webhook iteration.

### Push events

Any push to the repository triggers an iteration. This is the primary mechanism for **chaining**: when the worker commits updated state to the state branch, the resulting push event fires another iteration, allowing the system to advance through multiple tasks in quick succession without waiting for the next cron tick.

### Pull request events

Only merged PRs trigger an iteration. This signals that a cloud agent has completed its work and the task can be marked done.

## Label Behavior

### Labels added on issue creation

When the headless runner triggers a cloud coding agent, it creates a GitHub issue via the REST API with two labels:

```json
{
  "title": "[wreck-it] <task-id>",
  "body": "<task description + memory context>",
  "labels": ["wreck-it", "copilot"]
}
```

| Label | Purpose |
|-------|---------|
| `wreck-it` | Identifies the issue as wreck-it managed. Required for the webhook worker to process the corresponding `issues` event. |
| `copilot` | Conventional label indicating the issue is intended for a coding agent. |

Both labels are applied at creation time (not added after the fact), so the `issues.opened` event already contains the labels. The webhook worker sees the `wreck-it` label and processes the event.

### Labels on pull requests

wreck-it does **not** add labels to pull requests. PRs are created by the cloud coding agent (e.g. GitHub Copilot), not by wreck-it itself. The worker identifies relevant PRs by the `closed` + `merged` action, not by labels.

### Labels on tasks

Tasks in `tasks.json` have an optional `labels` field for organizational metadata. These labels are local to the task file and are **not** automatically synced to GitHub issues or PRs.

## State Commit Behavior

A key design concern is whether state commits can cause infinite event loops. The short answer: **no** — the worker only commits when there is real work to do.

### When the worker commits

The worker writes updated task and state files to the state branch **only** when a task is started (`IterationOutcome::TaskStarted`). In the other two outcomes:

| Outcome | Files written | Commits | Triggers push event |
|---------|--------------|---------|---------------------|
| `AllComplete` | None | No | No |
| `NoPendingTasks` | None | No | No |
| `TaskStarted` | Task file + state file | Yes | Yes |

This means the event chain self-terminates: once all eligible tasks have been started (or all tasks are complete), the worker stops committing, which stops generating push events.

### Event chain lifecycle

A typical chain looks like this:

```
1. Issue created with "wreck-it" label
   └─► issues webhook fires
       └─► Worker selects task A, commits state
           └─► push webhook fires
               └─► Worker selects task B, commits state
                   └─► push webhook fires
                       └─► Worker finds no pending tasks → no commit → chain ends
```

### Headless CLI (cron mode) behavior

The cron-based headless runner (`wreck-it run --headless`) also avoids unnecessary commits:

1. After the state machine loop, the state is serialized to disk via `save_headless_state`.
2. `commit_and_push_state` checks for actual git changes (`git diff --quiet` + untracked file check).
3. If the serialized state is byte-for-byte identical to what was already on disk, git detects no changes and **no commit is made**.
4. Only when the state actually changed (e.g. a phase transition, a new task started) does a commit and push occur.

This ensures that a cron run that finds nothing to do does not produce a spurious commit that would trigger the webhook worker.

## Worker vs. Cron: Two Modes of Operation

wreck-it supports two complementary ways to advance the state machine:

| Aspect | Webhook Worker | Cron Headless Runner |
|--------|---------------|---------------------|
| **Trigger** | GitHub events (real-time) | Scheduled cron (e.g. every 10 min) |
| **Runtime** | Cloudflare Worker (WASM) | GitHub Actions runner (native binary) |
| **State access** | GitHub Contents API | Git worktree on local filesystem |
| **Can trigger agents** | No (advances state only) | Yes (creates issues, assigns Copilot) |
| **Can merge PRs** | No | Yes (via GitHub API) |

In practice, the **headless runner** (cron mode) is the primary driver: it creates issues, assigns agents, polls for PRs, and merges them. The **webhook worker** provides faster state advancement by reacting to events immediately rather than waiting for the next cron tick.

Both modes are safe to run simultaneously. They operate on the same state branch but through different mechanisms (API vs. git). The concurrency group in the GitHub Actions workflow (`cancel-in-progress: false`) prevents overlapping cron runs.

## Setup

### Prerequisites

- A [GitHub App](https://docs.github.com/en/apps/creating-github-apps) with webhook permissions
- A [Cloudflare Workers](https://developers.cloudflare.com/workers/) account
- Rust with the `wasm32-unknown-unknown` target

### 1. Create a GitHub App

Create a GitHub App with the following webhook event subscriptions:

- **Issues** — to trigger on issue creation / labeling
- **Push** — to react to state branch changes
- **Pull requests** — to detect merged PRs

Set the webhook URL to your deployed worker URL.

### 2. Configure and deploy the worker

```sh
# Install the WASM target
rustup target add wasm32-unknown-unknown

# Set secrets
cd worker
wrangler secret put GITHUB_WEBHOOK_SECRET   # Webhook secret from GitHub App settings
wrangler secret put GITHUB_APP_TOKEN        # Installation token or PAT with repo contents access

# Deploy
wrangler deploy
```

### 3. Required secrets

| Secret | Purpose |
|--------|---------|
| `GITHUB_WEBHOOK_SECRET` | HMAC-SHA256 secret for verifying webhook payload signatures |
| `GITHUB_APP_TOKEN` | GitHub token with read/write access to repository contents on the state branch |

## Source Code Reference

| File | Purpose |
|------|---------|
| `worker/src/lib.rs` | Worker entry point — receives webhooks, verifies signatures, routes events |
| `worker/src/webhook.rs` | HMAC-SHA256 signature verification, event type parsing |
| `worker/src/github.rs` | GitHub REST API client (file read/write via Contents API) |
| `worker/src/processor.rs` | Iteration logic — reads config/state, advances task machine, commits results |
| `worker/src/types.rs` | Domain types (re-exports from `wreck-it-core`) and webhook payload types |
| `cli/src/cloud_agent.rs` | Cloud agent client — creates issues with labels, assigns Copilot |
| `cli/src/headless.rs` | Headless state machine — drives the full agent lifecycle in cron mode |
| `cli/src/state_worktree.rs` | Git worktree management — commit/push with no-change detection |
| `core/src/iteration.rs` | Shared iteration logic used by both the worker and CLI |

## Next Steps

- [CI & Headless Mode](ci-headless.md) — Cron-based headless operation in GitHub Actions
- [Architecture](architecture.md) — Ralph Wiggum loop internals
- [Getting Started](getting-started.md) — Local installation and TUI usage
