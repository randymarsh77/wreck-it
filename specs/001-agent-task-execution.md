# Spec 001: Agent-Based Task Execution via Cloudflare Durable Objects

## Summary

Replace the current Cloudflare Worker + KV architecture with Cloudflare Agents
backed by Durable Objects so that each ralph context becomes a persistent,
stateful agent instance with its own SQLite database.

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

1. Implement `RalphAgent` alongside existing KV-based endpoints.
2. Add a `POST /api/portal/repos/:owner/:repo/ralphs/:name/migrate` endpoint
   that reads current KV/file data and seeds the agent state.
3. Deprecate KV endpoints once agents are stable.

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
