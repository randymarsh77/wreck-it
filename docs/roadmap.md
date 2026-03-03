# wreck-it Feature Roadmap

> **Task**: `ideas-3`  
> **Date**: 2026-02-27  
> **Scope**: Prioritized roadmap of new features and improvements based on research in `docs/research-notes.md`.

---

## Overview

This roadmap translates the findings from the multi-agent orchestration research (`ideas-1`, `ideas-2`) into a prioritized sequence of concrete improvements for wreck-it. Features are ordered by the product of **impact** and **effort-efficiency** (high impact, lower effort first). The roadmap is organized into three horizons:

- **Horizon 1 (Phase 2–3, already planned)**: Foundation — role-based agents, dynamic task generation, agent memory, intelligent scheduling.
- **Horizon 2 (Phase 4–5)**: Intelligence — critic-actor reflection, adaptive re-planning, typed artefact store, LLM-powered planning.
- **Horizon 3 (Phase 4–6)**: Integration — gastown cloud runtime, openclaw provenance & visualisation.

---

## Horizon 1: Foundation (Phases 2–3 — ✅ Complete)

These tasks are implemented and cover the core agent swarm capabilities.

| ID | Feature | Inspired By | Status |
|---|---|---|---|
| impl-1 | Role-based specialist agents | CrewAI, MetaGPT | ✅ complete |
| impl-2 | Dynamic task generation by agents at runtime | AutoGen, LangGraph | ✅ complete |
| impl-3 | Per-task agent memory / context persistence | LangGraph Checkpointer, MetaGPT Memory | ✅ complete |
| impl-4 | Intelligent task routing and scheduling | Gastown capability negotiation | ✅ complete |
| eval-1 | Evaluate impl-1 + impl-2 | — | ✅ complete |
| eval-2 | Evaluate impl-3 + impl-4 | — | ✅ complete |
| eval-3 | End-to-end integration of Horizon 1 features | — | ✅ complete |

---

## Horizon 2: Intelligence (Phases 4–5 — ✅ Complete)

These features make wreck-it self-aware: it can plan from a goal, evaluate its own work, and recover from failures autonomously.

### Priority 1 — Critic-Actor Reflection Loop (High Impact, Low–Medium Effort) — ✅ Implemented

**Inspired by**: Openclaw critic-actor separation, AutoGen reflector agent, Reflexion paper.

After an actor agent completes a task and before committing, a lightweight **critic** prompt reads the git diff and evaluates it against the original task description. The critic returns a structured `CriticResult { score, issues, approved }`. If not approved, the actor is re-invoked with the critic's issues as additional context (up to a configurable `--reflection-rounds` limit, default 2). Only after approval are tests run.

**Why first**: Lowest implementation effort among Horizon 2 features; highest quality-of-output multiplier. Catches obvious mistakes (wrong file edited, missing imports, incomplete implementation) before wasting a full test cycle.

---

### Priority 2 — Adaptive Re-Planning on Failure (High Impact, Medium Effort) — ✅ Implemented

**Inspired by**: LangGraph re-planner node, MetaGPT iterative planning, Openclaw adaptive re-planning.

After a configurable number of consecutive failures (`--replan-threshold`, default 2), wreck-it invokes a **re-planner** agent that receives: the original task list, the failed task, the error output, and the current git state. The re-planner may: (a) rewrite the failed task description, (b) split it into smaller sub-tasks, or (c) inject a prerequisite task. The modified task list is persisted and the loop continues.

**Why second**: Directly addresses the biggest practical pain-point — wreck-it currently requires manual intervention after failures. This transforms it from a "loop" into a true autonomous agent harness.

---

### Priority 3 — LLM-Powered Dynamic Task Planning (High Impact, Medium Effort) — ✅ Implemented

**Inspired by**: LangGraph plan-and-execute, AutoGen planner agent, CrewAI hierarchical process.

Add a `wreck-it plan --goal "..."` sub-command (and an optional pre-loop planning phase in `wreck-it run --goal "..."`) that sends a natural-language goal to the Copilot SDK and receives a structured `tasks.json` back. The planner prompt instructs the model to emit a JSON array of tasks with `id`, `description`, `phase`, and optional `depends_on` fields.

**Why third**: Dramatically lowers the barrier to entry — users describe what they want in plain English. Builds on impl-2 (dynamic task generation at runtime) but extends it to the initial bootstrapping step.

---

### Priority 4 — Typed Artefact Store / Context Chain (Medium Impact, Medium Effort) — ✅ Implemented

**Inspired by**: MetaGPT artefacts, CrewAI task `context` field.

Extend `Task` with optional `inputs: Vec<String>` (e.g., `"task-id/artefact-name"`) and `outputs: Vec<TaskArtefact>` fields. When a task completes, the agent serialises its declared outputs into a lightweight manifest stored in `.wreck-it-state.json`. Downstream tasks that declare `inputs` have those artefacts injected into their prompt context automatically.

**Why fourth**: Enables sophisticated multi-agent pipelines (design → code → test, where each stage automatically receives structured output from the previous) without manual wiring.

---

## Horizon 3: Integration (Phases 4–6 — ✅ Complete)

These features connect wreck-it to the broader ecosystem: cloud-native execution via gastown and observability/auditability via openclaw.

### Priority 5 — Gastown Cloud Runtime Integration (High Impact, Medium–High Effort)

**Inspired by**: Gastown agent-as-a-service, workflow-as-data, durable execution patterns.

**Status**: ✅ Implemented (`impl-9`)

Add a `gastown` runtime backend. Tasks can declare `runtime: "gastown"` to offload execution to gastown cloud services. wreck-it acts as a workflow DAG producer: it serialises the task graph as a gastown-compatible YAML/JSON workflow and submits it to the gastown orchestrator. Gastown handles horizontal scaling, durable checkpointing (crash recovery), and capability negotiation for multi-model routing.

**Key integration points**:
- wreck-it → gastown: task graph serialisation (DAG export) via `GastownClient::build_dag` / `serialise_dag`.
- gastown → wreck-it: status callbacks (task completed/failed events) applied to `.wreck-it-state.json` via `GastownClient::apply_status_events`.
- Capability negotiation: wreck-it queries the gastown agent registry to route tasks to the best-matched agent.

**Configuration** (`Config` in `types.rs`):
```toml
gastown_endpoint = "https://my-gastown-host/api"   # enables integration
gastown_token    = "tok_…"                          # auth token
```

**Task declaration** (`tasks.json`):
```json
{ "id": "my-task", "description": "...", "runtime": "gastown" }
```

When either `gastown_endpoint` or `gastown_token` is absent, gastown integration is disabled and tasks run locally as before.

**Implementation**: `cli/src/gastown_client.rs` — `GastownClient`, `WorkflowDag`, `DagNode`, `GastownStatusEvent`.

---

### Priority 6 — Openclaw Provenance Tracking and Visualisation Integration (Medium Impact, Medium–High Effort) — ✅ Implemented

**Inspired by**: Openclaw provenance tracking, plan graph visualisation, critic-actor separation.

Add provenance metadata to every task execution: record the agent ID, model, prompts, tool calls, and git diff that produced each output. Store provenance in a `.wreck-it-provenance/` directory (one JSON file per task). Expose this data in a format compatible with the openclaw plan graph visualiser so users can inspect the full audit trail of a wreck-it run.

**Key integration points**:
- wreck-it records provenance during execution (extends the state machine in `ralph_loop.rs`).
- A `wreck-it provenance --task <id>` sub-command prints the provenance chain for a task.
- An optional `wreck-it export-openclaw` command exports the full run provenance in openclaw-compatible JSON for visualisation in the openclaw UI.

---

## Summary Prioritization Table

| Priority | ID | Feature | Inspired by | Effort | Impact | Status |
|---|---|---|---|---|---|---|
| 1 | impl-6 | Critic-actor reflection loop | Openclaw, Reflexion | Low–Medium | High | ✅ Implemented |
| 2 | impl-7 | Adaptive re-planning on failure | LangGraph, MetaGPT, Openclaw | Medium | High | ✅ Implemented |
| 3 | impl-5 | LLM-powered dynamic task planning | LangGraph, AutoGen | Medium | High | ✅ Implemented |
| 4 | impl-8 | Typed artefact store / context chain | MetaGPT, CrewAI | Medium | Medium | ✅ Implemented |
| 5 | impl-9 | Gastown cloud runtime integration | Gastown | Medium–High | High | ✅ Implemented |
| 6 | impl-10 | Openclaw provenance tracking | Openclaw | Medium–High | Medium | ✅ Implemented |

---

## Inter-Feature Dependencies

```
eval-3 (Horizon 1 complete)
    │
    ├── impl-5 (LLM planning) ──────────────────────────► eval-4
    ├── impl-6 (critic-actor) ──────────────────────────► eval-4
    │
    ├── impl-7 (adaptive re-planning) ─────────────────► eval-5
    ├── impl-8 (artefact store) ───────────────────────► eval-5
    │
    ├── impl-9 (gastown integration) ──────────────────► eval-6
    └── impl-10 (openclaw integration) ────────────────► eval-6
                                                             │
                                                     eval-4 ─┤
                                                     eval-5 ─┤
                                                             ▼
                                                          eval-7
                                                   (full integration)
```

---

## Breakthrough Potential

Combining these features produces a qualitatively different tool:

1. **`wreck-it plan --goal "build a REST API"`** → LLM generates a task plan.
2. **Role-based routing** → tasks assigned to specialist agents (architect, coder, tester).
3. **Critic-actor loop** → each task output is reviewed before tests run.
4. **Adaptive re-planning** → failures trigger automatic task restructuring.
5. **Artefact chaining** → design docs flow automatically into coding tasks.
6. **Gastown offload** → long-running tasks execute in the cloud with durable state.
7. **Openclaw audit** → every decision is provenance-tracked and visualisable.

This moves wreck-it from a "task runner with an AI backend" to a **fully autonomous, self-improving agent orchestration platform**.
