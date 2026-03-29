# wreck-it Worker

A [Cloudflare Worker](https://developers.cloudflare.com/workers/) that serves as the webhook handler and **pulse trigger** for a **wreck-it** GitHub App. When GitHub events occur (issues labeled `wreck-it`, pushes to the state branch, PRs merged), the worker reads the repository's wreck-it configuration and state via the GitHub API, processes an iteration (selects the next pending task, advances the state machine), triggers cloud agents, manages PRs, and commits the updated state back to the state branch.

In addition to event-driven webhook processing, the worker includes a **pulse trigger system** that fires on a configurable cron schedule, iterating over all registered repositories to inject entropy — picking up tasks whose cooldowns have expired and advancing idle ralph contexts.

The Rust code compiles to WebAssembly and runs on the Cloudflare Workers runtime.

## Architecture

```
GitHub Event (webhook)          Cron Trigger (pulse)
        │                              │
        ▼                              ▼
 ┌─────────────────┐          ┌─────────────────┐
 │  Cloudflare      │          │  Scheduled       │
 │  Worker (WASM)   │          │  Event Handler   │
 │                  │          │                  │
 │  1. Verify sig   │          │  1. Load registry│◄── KV (pulse/repos)
 │  2. Parse event  │          │  2. For each repo│
 │  3. Vend token   │◄── JWT   │  3. Vend token   │◄── JWT
 │  4. Read config  │◄── API   │  4. Process iter │
 │  5. Read state   │          │  5. Trigger agent│
 │  6. Process iter │          └─────────────────┘
 │  7. Trigger agent│────► GitHub API (Issues + GraphQL)
 │  8. Commit state │────► GitHub API (Contents)
 │  9. Merge PRs    │────► GitHub API (REST + GraphQL)
 │ 10. Register repo│────► KV (auto-register for pulse)
 └─────────────────┘
```

### Modules

| Module | Purpose |
|--------|---------|
| `src/lib.rs` | Worker entry point — receives webhooks, vends tokens, routes events, scheduled handler |
| `src/github_app.rs` | GitHub App authentication — JWT generation, installation token vending |
| `src/webhook.rs` | HMAC-SHA256 signature verification |
| `src/github.rs` | GitHub REST + GraphQL API client (file I/O, issues, agents, PRs) |
| `src/processor.rs` | Iteration logic — task selection, agent triggering, state advancement |
| `src/pulse.rs` | Pulse trigger system — cron-driven iteration over registered repos |
| `src/types.rs` | Domain types (Task, HeadlessState, RepoConfig, PulseRegistration, webhook payloads) |
| `src/api.rs` | REST API endpoints for tasks, state, and pulse registry management |
| `src/kv_store.rs` | Cloudflare KV storage abstraction for tasks, state, and pulse registry |

## Handled Events

| Event | Action | Behavior |
|-------|--------|----------|
| `issues` | `opened` / `labeled` | Triggers iteration when issue has `wreck-it` label |
| `push` | any | Triggers iteration (external state updates) |
| `pull_request` | `closed` (merged) | Marks task complete, updates state |
| `pull_request` | `opened` / `ready_for_review` / `synchronize` | Approves pending workflow runs and enables auto-merge for trusted PRs |
| `ping` | — | Responds with `pong` (app setup verification) |
| `scheduled` | cron | Iterates all registered repos (pulse trigger) |

## Pulse Trigger

The pulse trigger system ensures that iterations run even when no webhook events arrive. This is critical for:

- **Tasks with cooldowns** — recurring tasks whose cooldowns have expired need a trigger to be picked up.
- **Idle ralph contexts** — ralph contexts that are waiting for an external event can be nudged forward.
- **Entropy injection** — periodic processing prevents the system from stalling when no webhook activity occurs.

### How it works

1. **Auto-registration** — when the worker processes any accepted webhook event, it records the repository's coordinates (owner, repo, installation ID, default branch) in the KV-backed pulse registry.
2. **Cron trigger** — a Cloudflare cron trigger (default: every 30 minutes) fires the `#[event(scheduled)]` handler.
3. **Iteration** — the handler reads the pulse registry, vends a GitHub App token for each registered repo, and runs `process_iteration()`.

### Configuration

The cron schedule is configured in `wrangler.toml`:

```toml
[triggers]
crons = ["*/30 * * * *"]
```

### Pulse registry API

The pulse registry can also be managed via the REST API:

| Method | Path | Description |
|--------|------|-------------|
| `GET` | `/api/pulse/registrations` | List all pulse registrations |
| `PUT` | `/api/pulse/registrations/{owner}/{repo}` | Upsert a registration |
| `DELETE` | `/api/pulse/registrations/{owner}/{repo}` | Remove a registration |

## Setup

### Prerequisites

- [Rust](https://rustup.rs/) with the `wasm32-unknown-unknown` target
- [wrangler](https://developers.cloudflare.com/workers/wrangler/install-and-update/) CLI
- A GitHub App with the required permissions

### 1. Install the WASM target

```sh
rustup target add wasm32-unknown-unknown
```

### 2. Create a KV namespace

```sh
cd worker
wrangler kv namespace create WRECK_IT_STORE
```

Copy the `id` from the output and replace `REPLACE_WITH_KV_NAMESPACE_ID` in `wrangler.toml`:

```toml
[[kv_namespaces]]
binding = "WRECK_IT_STORE"
id = "<your-namespace-id>"
```

### 3. Configure secrets

```sh
cd worker

# Required
wrangler secret put GITHUB_WEBHOOK_SECRET   # Webhook secret from GitHub App settings
wrangler secret put GITHUB_APP_ID           # Numeric App ID
wrangler secret put GITHUB_APP_PRIVATE_KEY  # PEM-encoded RSA private key
wrangler secret put API_TOKEN               # Bearer token for the REST API endpoints
```

### 4. Deploy

```sh
cd worker
wrangler deploy
```

### 5. Configure the GitHub App

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
