---
sidebar_position: 4
---

# Roadmap

Prioritized roadmap of features and improvements based on multi-agent orchestration research.

## Horizon 1: Foundation (In Progress)

Core agent swarm capabilities currently being implemented.

| Feature | Inspired By | Status |
|---|---|---|
| Role-based specialist agents | CrewAI, MetaGPT | Planned |
| Dynamic task generation at runtime | AutoGen, LangGraph | Planned |
| Per-task agent memory / context persistence | LangGraph, MetaGPT | Planned |
| Intelligent task routing and scheduling | Gastown | Planned |

## Horizon 2: Intelligence

Self-awareness features — planning, self-evaluation, and autonomous recovery.

### Critic-Actor Reflection Loop

After an actor agent completes a task, a lightweight **critic** prompt reads the git diff and evaluates it against the task description. If not approved, the actor is re-invoked with the critic's issues as context.

### Adaptive Re-Planning on Failure

After consecutive failures, a **re-planner** agent rewrites the failed task, splits it into sub-tasks, or injects prerequisites. This transforms wreck-it from a "loop" into a true autonomous agent harness.

### LLM-Powered Task Planning

A `wreck-it plan --goal "..."` sub-command sends a natural-language goal to the Copilot SDK and receives a structured task list back.

### Typed Artefact Store

Tasks declare `inputs` and `outputs` fields. Completed task outputs are automatically injected into downstream task contexts.

## Horizon 3: Integration

Cloud-native execution and observability.

### Gastown Cloud Runtime

Offload tasks to Gastown cloud services for horizontal scaling, durable checkpointing, and multi-model routing.

### Openclaw Provenance Tracking

Record agent ID, model, prompts, tool calls, and git diffs for every task execution. Export provenance for visualization in the Openclaw UI.

## Vision

Combining these features produces a qualitatively different tool:

1. `wreck-it plan --goal "build a REST API"` → LLM generates a task plan
2. **Role-based routing** → tasks assigned to specialist agents
3. **Critic-actor loop** → each task output is reviewed before tests run
4. **Adaptive re-planning** → failures trigger automatic restructuring
5. **Artefact chaining** → design docs flow into coding tasks
6. **Gastown offload** → long-running tasks execute in the cloud
7. **Openclaw audit** → every decision is provenance-tracked

This moves wreck-it from a task runner to a **fully autonomous, self-improving agent orchestration platform**.
