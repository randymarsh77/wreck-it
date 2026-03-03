# wreck-it Worker

A [Cloudflare Worker](https://developers.cloudflare.com/workers/) that serves as the webhook handler for a **wreck-it** GitHub App. When GitHub events occur (issues labeled `wreck-it`, pushes to the state branch, PRs merged), the worker reads the repository's wreck-it configuration and state via the GitHub API, processes an iteration (selects the next pending task, advances the state machine), triggers cloud agents, manages PRs, and commits the updated state back to the state branch.

The Rust code compiles to WebAssembly and runs on the Cloudflare Workers runtime.

## Architecture

```
GitHub Event (webhook)
        │
        ▼
 ┌─────────────────┐
 │  Cloudflare      │
 │  Worker (WASM)   │
 │                  │
 │  1. Verify sig   │
 │  2. Parse event  │
 │  3. Vend token   │◄──── GitHub App (JWT → installation token)
 │  4. Read config  │◄──── GitHub API (Contents)
 │  5. Read state   │◄──── .wreck-it/config.toml + state branch
 │  6. Process iter │
 │  7. Trigger agent│────► GitHub API (Issues + GraphQL)
 │  8. Commit state │────► GitHub API (Contents)
 │  9. Merge PRs    │────► GitHub API (REST + GraphQL)
 └─────────────────┘
```

### Modules

| Module | Purpose |
|--------|---------|
| `src/lib.rs` | Worker entry point — receives webhooks, vends tokens, routes events |
| `src/github_app.rs` | GitHub App authentication — JWT generation, installation token vending |
| `src/webhook.rs` | HMAC-SHA256 signature verification |
| `src/github.rs` | GitHub REST + GraphQL API client (file I/O, issues, agents, PRs) |
| `src/processor.rs` | Iteration logic — task selection, agent triggering, state advancement |
| `src/types.rs` | Domain types (Task, HeadlessState, RepoConfig, webhook payloads) |

## Handled Events

| Event | Action | Behavior |
|-------|--------|----------|
| `issues` | `opened` / `labeled` | Triggers iteration when issue has `wreck-it` label |
| `push` | any | Triggers iteration (external state updates) |
| `pull_request` | `closed` (merged) | Marks task complete, updates state |
| `ping` | — | Responds with `pong` (app setup verification) |

## Setup

### Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target
- [wrangler](https://developers.cloudflare.com/workers/wrangler/install-and-update/) CLI
- A GitHub App with the required permissions

### 1. Install the WASM target

```sh
rustup target add wasm32-unknown-unknown
```

### 2. Configure secrets

```sh
cd worker

# Required
wrangler secret put GITHUB_WEBHOOK_SECRET   # Webhook secret from GitHub App settings

# Recommended: App credentials (enables full capabilities)
wrangler secret put GITHUB_APP_ID           # Numeric App ID
wrangler secret put GITHUB_APP_PRIVATE_KEY  # PEM-encoded RSA private key

# Alternative: Static token (legacy, limited capabilities)
# wrangler secret put GITHUB_APP_TOKEN      # Installation token or PAT
```

### 3. Deploy

```sh
cd worker
wrangler deploy
```

### 4. Configure the GitHub App

Point the GitHub App's webhook URL to your deployed worker URL (e.g. `https://wreck-it-worker.<your-subdomain>.workers.dev`).

**Required permissions:**
- **Contents** — Read & write
- **Issues** — Read & write
- **Pull requests** — Read & write
- **Actions** — Read & write

**Subscribe to these events:**
- **Issues** — to trigger on issue creation / labeling
- **Push** — to react to state branch changes
- **Pull requests** — to detect merged PRs

## Development

### Build (check)

```sh
cd worker
cargo check
```

### Run tests

```sh
cd worker
cargo test
```

### Format

```sh
cd worker
cargo fmt
```

## How It Works

1. **Webhook arrives** — the worker verifies the `X-Hub-Signature-256` header using HMAC-SHA256.
2. **Token vending** — generates a JWT from the app's private key and exchanges it for an installation access token scoped to the repository.
3. **Event routing** — only relevant events (issues with `wreck-it` label, pushes, merged PRs) trigger processing.
4. **Read config** — `.wreck-it/config.toml` is fetched from the repository's default branch to discover the state branch and ralph contexts.
5. **Read state** — for each ralph context, the task file and state file are read from the state branch via the GitHub Contents API.
6. **Process iteration** — the next eligible pending task is selected (respecting phases, dependencies, priority, complexity), marked as in-progress, and the iteration counter is bumped.
7. **Trigger agent** — a GitHub issue is created with `wreck-it` + `copilot` labels and a coding agent is assigned via the GraphQL API.
8. **Commit state** — updated task and state files are written back to the state branch via the GitHub Contents API.
9. **PR management** — when a PR is merged, the corresponding task is marked complete and state is updated.
