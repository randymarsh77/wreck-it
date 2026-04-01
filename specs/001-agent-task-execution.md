# Spec 001: Agent-Based Task Execution via Cloudflare Durable Objects

## Summary

Add a new Cloudflare Agents / Durable Objects backend for the ralph loop so
that each ralph context can optionally run as a persistent, stateful agent
instance with its own SQLite database.  The existing backends — local CLI
(`ralph_loop` + repo state) and Cloudflare Worker + KV — remain supported.

## Motivation

Today the worker stores ralph task and state data in KV (flat key-value) or
reads/writes files through the GitHub Contents API.  Neither approach supports
long-running execution, real-time progress updates, or local caching of
intermediate state.

Cloudflare Agents provide:

- **Persistent state via SQLite** — each agent instance has a co-located
  SQLite database that survives restarts.
- **Hibernation** — idle agents sleep at zero cost and wake on demand.
- **Massively parallel** — one agent instance per ralph / repo / user.
- **WebSocket support** — built-in bidirectional communication for real-time
  progress updates.

## Proposed Design

### Agent Class Hierarchy

```
Agent<Env, RalphState>
├── RalphAgent          — one per ralph context
│     state: { tasks, execution_state, config }
│     onConnect()       — push live task status to portal clients
│     onMessage()       — handle commands (run, pause, cancel)
│     @callable run()   — trigger an iteration
│     @callable plan()  — generate a task plan from a goal
│     onTask()          — handle scheduled recurring iterations
```

### State Shape

```typescript
interface RalphState {
  tasks: Task[]
  execution: {
    status: 'idle' | 'running' | 'paused'
    current_task_id: string | null
    iteration_count: number
    last_run_at: number | null
  }
  config: {
    task_file: string
    state_file: string
    owner: string
    repo: string
  }
}
```

### Naming Convention

Each agent is addressed by a deterministic name derived from the repository
and ralph:

```
{owner}/{repo}/ralph/{name}
```

### Lifecycle

1. Portal UI connects via WebSocket → `onConnect` sends current state.
2. User triggers "Run" → callable `run()` starts iteration.
3. Agent calls GitHub Models API, updates `this.state`, broadcasts progress.
4. On completion, agent schedules next run if `kind == "recurring"`.
5. When all clients disconnect, agent hibernates.

## Migration Path

1. Implement `RalphAgent` alongside existing KV-based endpoints **and** the
   local CLI headless runner.  All three backends coexist.
2. Add a `POST /api/portal/repos/:owner/:repo/ralphs/:name/migrate` endpoint
   that reads current KV/file data and seeds the agent state.
3. Deprecate KV endpoints once agents are stable.  The local CLI + repo-state
   backend remains as the first-class "local dev" path and is unaffected.

## Wrangler Configuration

```jsonc
{
  "durable_objects": {
    "bindings": [{ "name": "RALPH_AGENT", "class_name": "RalphAgent" }]
  },
  "migrations": [
    { "tag": "v1", "new_sqlite_classes": ["RalphAgent"] }
  ]
}
```

## Open Questions

- Should the agent call the LLM directly (via Workers AI or external API) or
  delegate to a Workflow for durable multi-step execution?
- How do we handle the existing `headless` CLI runner when the agent is the
  primary executor?

## Answers / Decisions

### Ralph backends are additive, not replacements

Durable Objects introduce a third "ralph backend" alongside the two that
already exist:

| Backend | State storage | Executor | Best for |
|---------|--------------|----------|----------|
| **Local CLI** (`ralph_loop` / `headless`) | Repo state branch + local filesystem | Local AI chat loop or headless CI | Local dev, CI cron workflows |
| **Worker + KV** (current) | Cloudflare KV + GitHub Contents API | `processor.rs` iteration via webhook / cron | Lightweight cloud orchestration |
| **Durable Object agent** (new) | Co-located SQLite per instance | `RalphAgent` with hibernation + WebSocket | Portal-driven execution, real-time UI, long-running tasks |

The local CLI headless runner is **not** deprecated.  It remains the primary
path for users who run wreck-it from GitHub Actions or a local terminal.  The
DO agent is an alternative cloud-native backend that the portal can connect to
directly.

### LLM strategy: delegate, don't embed

The `RalphAgent` running on a Durable Object should **not** call LLMs
directly in-process.  Instead it acts as an orchestrator:

1. **Copilot cloud agents** (default) — the agent creates a GitHub issue,
   assigns Copilot, and monitors the resulting PR, exactly as the headless
   runner does today via `CloudAgentClient`.  The DO gives this workflow
   persistent state and WebSocket progress streaming that KV lacks.

2. **Workers AI / external LLM APIs** (optional) — for lightweight tasks
   (e.g. plan generation, semantic evaluation, changelog drafting) the agent
   can call Workers AI or an external model API directly from within the DO.
   These calls are short-lived and do not need Workflow durability.  A
   `ModelRouter` trait can abstract the choice:

   ```typescript
   interface ModelRouter {
     chat(prompt: string, opts?: { model?: string }): Promise<string>
   }
   ```

   Implementations: `CopilotCloudRouter` (creates issue → polls PR),
   `WorkersAIRouter` (calls `env.AI.run()`), `ExternalAPIRouter` (calls
   GitHub Models / OpenAI / Anthropic endpoints).

3. **Workflows** (future, only if needed) — if a single iteration grows
   beyond the DO CPU time limit (~30 s of wall-clock compute) or needs
   guaranteed delivery across multiple external calls, the agent can kick
   off a Cloudflare Workflow.  Until that need is demonstrated, the simpler
   direct-call approach is preferred.

### Headless CLI coexistence

The headless CLI runner (`headless.rs`) is unaffected by this change.  Both
backends share the same core logic via `wreck_it_core::iteration` and the
same state shapes (`HeadlessState`, `Task`, `AgentPhase`).  The key
architectural invariant:

- **`wreck_it_core`** remains the single source of truth for task selection,
  status resolution, recurring-task resets, and phase advancement.
- The DO agent calls the same `advance_iteration` / `select_next_task`
  functions (compiled to WASM or re-implemented in TS from the same spec).
- State can be **synced** between backends via the migrate endpoint, but
  they do not need to run simultaneously against the same ralph context.

A ralph context is "owned" by exactly one backend at a time, configured in
`.wreck-it.toml`:

```toml
[[ralphs]]
name = "feature-dev"
backend = "durable-object"   # or "headless" (default) or "worker-kv"
```
