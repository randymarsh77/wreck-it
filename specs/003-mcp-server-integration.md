# Spec 003: MCP Server Integration via Cloudflare Agents

## Summary

Expose wreck-it's task management tools as an MCP (Model Context Protocol)
server running on Cloudflare Agents, enabling any MCP-compatible AI client
(Claude, Copilot, custom agents) to discover and invoke wreck-it operations
without direct CLI access.

## Motivation

The CLI already includes a local MCP server (`wreck-it mcp`) with 7 tools:
`list_tasks`, `get_task`, `add_task`, `update_task_status`, `read_artefact`,
`list_artefacts`, and `trigger_iteration`.  However, this requires running
the CLI locally.

A cloud-hosted MCP server would allow:

- Remote AI agents (e.g. Copilot in the browser) to manage tasks.
- Multi-user collaboration through a shared MCP endpoint.
- Integration with other MCP clients without local tool installation.

## Proposed Design

### Agent Class

```typescript
import { McpAgent } from 'agents/mcp'
import { McpServer } from '@modelcontextprotocol/sdk/server/mcp.js'

export class WreckItMcpAgent extends McpAgent<Env, State> {
  server = new McpServer({
    name: 'wreck-it',
    version: '1.0.0'
  })

  async init() {
    this.server.registerTool('list_tasks', { ... }, handler)
    this.server.registerTool('get_task',   { ... }, handler)
    this.server.registerTool('add_task',   { ... }, handler)
    this.server.registerTool('update_task_status', { ... }, handler)
    this.server.registerTool('generate_plan',      { ... }, handler)
    this.server.registerTool('trigger_iteration',  { ... }, handler)
  }
}
```

### Tool Definitions

Each tool mirrors the CLI MCP server's interface:

| Tool                  | Input Schema                               | Description                        |
|-----------------------|--------------------------------------------|------------------------------------|
| `list_tasks`          | `{ ralph?: string }`                       | List all tasks with status summary |
| `get_task`            | `{ task_id: string }`                      | Get a single task's details        |
| `add_task`            | `{ id, description, phase?, depends_on? }` | Add a new task                     |
| `update_task_status`  | `{ task_id, status }`                      | Change a task's status             |
| `generate_plan`       | `{ goal: string, ralph?: string }`         | Generate a task plan from a goal   |
| `trigger_iteration`   | `{ ralph: string }`                        | Trigger one iteration cycle        |

### Endpoint

```
/mcp   — streamable HTTP MCP endpoint (also supports SSE)
```

Configured via:

```typescript
export default WreckItMcpAgent.serve('/mcp', { binding: 'MCP_AGENT' })
```

### Authentication

MCP requests are authenticated via the same portal session token mechanism.
The agent verifies the `Authorization` header before processing any tool call.

## Relationship to CLI MCP Server

- The CLI MCP server (`wreck-it mcp`) operates on local files.
- The cloud MCP server operates via GitHub Contents API and KV/DO state.
- Both expose the same logical tool interface.

## Open Questions

- Should the cloud MCP server support `read_artefact` / `list_artefacts`?
  These currently read from the local filesystem; the cloud equivalent would
  need to pull from the repo.
- Should the MCP agent share state with the `RalphAgent` (spec 001) or
  maintain its own connection to the GitHub API?
