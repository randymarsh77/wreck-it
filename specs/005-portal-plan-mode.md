# Spec 005: Plan Mode via Portal Web UI

## Summary

Add a plan generation feature to the portal web UI that mirrors the CLI's
`wreck-it plan` command.  Users enter a natural-language goal in the browser,
the worker calls the GitHub Models API to generate a structured task plan, and
the resulting tasks are displayed for review before being saved to the repo.

## Motivation

The CLI `plan` command requires local installation and terminal access.  Many
users interact with wreck-it exclusively through the portal.  Bringing plan
mode to the web UI:

- Lowers the barrier to creating new ralphs.
- Enables non-technical stakeholders to define goals.
- Keeps the full workflow (plan → configure → run) in one interface.

## Proposed Design

### Backend: Worker Endpoint

**`POST /api/portal/repos/:owner/:repo/ralphs/plan`**

Request body:

```json
{
  "goal": "Build a REST API for user management",
  "ralph": "optional-ralph-name"
}
```

Response:

```json
{
  "name": "rest-api-users",
  "tasks": [
    {
      "id": "1",
      "description": "Set up project structure",
      "phase": 1,
      "depends_on": []
    },
    {
      "id": "2",
      "description": "Implement user CRUD endpoints",
      "phase": 2,
      "depends_on": ["1"]
    }
  ]
}
```

The endpoint:

1. Authenticates the user via the portal session.
2. Sends the goal to the GitHub Models API using the planner prompt
   (same prompt as the CLI `planner.rs`).
3. Parses and validates the response (extract JSON, check schema).
4. Returns the plan without writing to the repo (the user reviews first).

A follow-up call to `POST .../ralphs/deploy` + `PUT .../config` saves the
plan and configures the ralph, using the existing endpoints.

### Frontend: Plan Page

New route: `/repos/:owner/:repo/plan`

**UI Flow:**

1. **Goal Input** — Text area for the natural-language goal.
2. **Generate** — Button triggers the plan endpoint; shows a spinner.
3. **Review** — Rendered task list with id, description, phase, dependencies.
4. **Edit** — Inline editing of task descriptions, reordering, deletion.
5. **Deploy** — Saves tasks via the deploy endpoint and updates config.

### API Client Addition

```typescript
interface PlanRequest {
  goal: string
  ralph?: string
}

interface PlanResponse {
  name: string
  tasks: RalphTask[]
}

export async function generatePlan(
  owner: string,
  repo: string,
  request: PlanRequest,
): Promise<PlanResponse> {
  return request<PlanResponse>(
    `/api/portal/repos/${owner}/${repo}/ralphs/plan`,
    { method: 'POST', body: JSON.stringify(request) },
  )
}
```

### Worker Implementation

The worker replicates the CLI planner logic:

1. Build the planner prompt (same template as `planner.rs::build_planner_prompt`).
2. Call GitHub Models API (`https://models.github.ai/inference/chat/completions`)
   using a configured API token (new secret: `GITHUB_MODELS_TOKEN`).
3. Parse the LLM response, extract JSON, validate schema.
4. If no ralph name is provided, call a second LLM request with the naming
   prompt to generate a slug.
5. Return the structured plan.

### Security

- The plan endpoint requires an authenticated portal session.
- The GitHub Models API token is a worker secret, not exposed to the client.
- User input (the goal) is passed as the LLM prompt content; no shell
  execution or file system access is involved.

## Implementation Checklist

- [x] Add `POST .../ralphs/plan` endpoint to `portal_api.rs`
- [x] Add `generatePlan()` to `api/client.ts`
- [x] Add Plan page component (`pages/Plan.tsx`)
- [x] Add route in `App.tsx`
- [x] Add navigation link from RepoConfig
- [x] Add worker unit tests for plan request/response parsing
- [x] Validate portal lint and build

## Open Questions

- Should the plan endpoint also support generating a plan name (second LLM
  call) or should the user always provide a name?
- Should we stream the LLM response for real-time plan generation feedback,
  or is a single request/response sufficient for typical plan sizes?
