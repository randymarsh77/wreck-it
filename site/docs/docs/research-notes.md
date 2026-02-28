---
sidebar_position: 5
---

# Research Notes

> Research into state-of-the-art multi-agent orchestration frameworks and concrete ideas for wreck-it.

## Framework Survey

### AutoGen (Microsoft)

AutoGen models agent collaboration as a **conversation graph**. Each node is an autonomous agent with its own system prompt and tool set. Key primitives include `AssistantAgent`, `UserProxyAgent`, `GroupChat`, and `ConversableAgent`.

**Relevance to wreck-it**: Mature ecosystem with pre-built specialist agents, built-in code execution sandbox, and nested conversations for sub-loops per task.

### CrewAI

CrewAI adopts a **role-based crew metaphor**: a `Crew` is a collection of `Agent` instances, each with a named role, goal, and backstory. Tasks are executed sequentially or in parallel.

**Relevance to wreck-it**: Sequential/hierarchical process maps to wreck-it's task queue. Role/backstory prompting produces specialized behaviour.

### LangGraph (LangChain)

LangGraph represents workflows as a **stateful directed graph** where nodes are arbitrary functions and edges are conditional transitions. State is a typed schema object that flows through the graph.

**Relevance to wreck-it**: Checkpointer pattern mirrors wreck-it's filesystem-as-memory philosophy. Conditional edges model task dependencies cleanly.

### OpenAI Swarm

Swarm is a lightweight framework exploring **agent handoffs**. A single `run_loop` function iterates, calling the currently active agent, then either executes a tool or transfers control to another agent.

**Relevance to wreck-it**: Handoff is the simplest model for routing tasks to specialists. Low overhead pattern wreck-it could borrow.

### MetaGPT

MetaGPT models a software company as a **multi-role assembly line** with a publish/subscribe message bus routing artefacts between roles.

**Relevance to wreck-it**: Pub/sub bus is a robust alternative to linear task queues. QA debug loop mirrors wreck-it's test-retry cycle.

## Cross-Cutting Patterns

| Pattern | Description | Used In |
|---|---|---|
| Plan-and-Execute | Planner produces task list, executor works through it | LangGraph, MetaGPT |
| Reflection / Self-Critique | Critic evaluates output, feeds back to original agent | Reflexion, AutoGen |
| Hierarchical Delegation | Manager decomposes goals, delegates to specialists | CrewAI, AutoGen |
| Publish-Subscribe | Agents register interest in message types | MetaGPT |
| Stateful Graph + Checkpointing | Persistent state with crash recovery | LangGraph, Gastown |
| Capability Negotiation | Orchestrator queries agents for capabilities | Gastown |

## Ideas for wreck-it

1. **Dynamic Task Decomposition** — LLM generates task plans from natural language goals
2. **Task Dependency Graph** — Parallel execution of independent tasks via `depends_on` fields
3. **Critic-Actor Reflection** — Self-evaluation before committing, catching mistakes early
4. **Typed Artefact Store** — Structured output chaining between tasks
5. **Role-Based Agents** — Specialist system prompts per task role
6. **Adaptive Re-Planning** — Automatic task restructuring on failure

See [Roadmap](roadmap.md) for the prioritized implementation plan.
