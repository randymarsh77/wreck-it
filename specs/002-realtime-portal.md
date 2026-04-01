# Spec 002: Real-Time Portal Updates via WebSockets

## Summary

Add WebSocket-based real-time communication between the portal frontend and
the Cloudflare Agent backend so that task status, execution progress, and plan
generation results stream to all connected clients instantly.

## Motivation

The portal currently polls REST endpoints for task and state data.  This
results in stale UI, wasted requests, and poor UX during long-running
operations like plan generation or task execution.

Cloudflare Agents include built-in WebSocket support with:

- Automatic connection tracking
- Bidirectional messaging via `onConnect` / `onMessage` / `onClose`
- Server-initiated pushes via `connection.send()`
- Broadcast to all connected clients

## Proposed Design

### Connection Flow

```
Portal (React)                    Agent (Durable Object)
     │                                    │
     ├──── WebSocket upgrade ────────────►│ onConnect()
     │                                    │  → send current state
     │◄──── { type: "state", data } ──────┤
     │                                    │
     ├──── { type: "run", task_id } ─────►│ onMessage()
     │                                    │  → start execution
     │◄──── { type: "progress", ... } ────┤
     │◄──── { type: "progress", ... } ────┤
     │◄──── { type: "complete", ... } ────┤
     │                                    │
     ├──── close ────────────────────────►│ onClose()
```

### Message Protocol

All messages are JSON with a `type` discriminator:

**Client → Server:**

| Type      | Payload                    | Description                    |
|-----------|----------------------------|--------------------------------|
| `run`     | `{ task_id?: string }`     | Trigger a run (optional task)  |
| `plan`    | `{ goal: string }`         | Generate a task plan           |
| `pause`   | `{}`                       | Pause current execution        |
| `cancel`  | `{}`                       | Cancel current execution       |

**Server → Client:**

| Type       | Payload                              | Description                  |
|------------|--------------------------------------|------------------------------|
| `state`    | `RalphState`                         | Full state snapshot          |
| `progress` | `{ task_id, status, message }`       | Incremental progress update  |
| `plan`     | `{ tasks: Task[], name: string }`    | Plan generation result       |
| `error`    | `{ message: string }`               | Error notification           |

### React Integration

```tsx
import { useAgent } from 'agents/react'

function RalphPanel({ owner, repo, ralph }) {
  const agent = useAgent<RalphState>({
    agent: 'RalphAgent',
    name: `${owner}/${repo}/ralph/${ralph}`,
    onStateUpdate: (state) => {
      // Reactive — re-renders automatically
    }
  })

  return (
    <div>
      {agent.state.tasks.map(task => (
        <TaskCard key={task.id} task={task} />
      ))}
      <button onClick={() => agent.stub.run()}>Run</button>
    </div>
  )
}
```

## Fallback

For browsers or environments that do not support WebSockets, keep the
existing REST endpoints as a degraded polling fallback.

## Open Questions

- Should we use the Cloudflare `agents/react` SDK directly or build a thin
  wrapper to match the portal's existing `api/client.ts` patterns?
- How do we authenticate WebSocket connections (token in query string vs
  first-message handshake)?
