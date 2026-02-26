# AI Agent Orchestration: Research Notes

> **Task**: `ideas-1`  
> **Date**: 2026-02-26  
> **Scope**: State-of-the-art multi-agent orchestration frameworks, gastown, openclaw, and concrete ideas for wreck-it.

---

## 1. Framework Survey

### 1.1 AutoGen (Microsoft)

**Repository**: https://github.com/microsoft/autogen  
**Language**: Python (core), with a .NET port in progress.

AutoGen models agent collaboration as a **conversation graph**. Each node is an autonomous `Agent` with its own system prompt and tool set; edges represent allowed message flows between agents. The key primitives are:

| Primitive | Description |
|---|---|
| `AssistantAgent` | LLM-backed agent that generates replies and invokes tools. |
| `UserProxyAgent` | Acts as a human-in-the-loop proxy; can execute code on behalf of the human or other agents. |
| `GroupChat` / `GroupChatManager` | Routes messages among N agents, optionally with a speaker-selection policy. |
| `ConversableAgent` | Base class; any agent that can send and receive messages. |

**Communication**: Agents exchange structured JSON messages. The manager follows a configurable **speaker-selection strategy** (round-robin, LLM-selected, or rule-based). All messages are stored in a shared conversation history, giving every agent full context.

**Task decomposition**: Typically done by a "planner" agent whose system prompt instructs it to split the user request into numbered sub-tasks and hand each off to a specialist agent. The `UserProxyAgent` coordinates re-tries when a sub-task fails.

**Feedback loops**: Termination conditions (e.g., the string `"TERMINATE"` appearing in a message) halt the conversation. Agents can explicitly request clarification, triggering a back-and-forth mini-loop before proceeding.

**Strengths relevant to wreck-it**:
- Mature ecosystem with many pre-built specialist agents (coder, critic, tester, planner).
- Built-in code execution sandbox that maps well to wreck-it's test-and-commit step.
- Nested conversations allow a top-level loop to spawn sub-loops per task.

---

### 1.2 CrewAI

**Repository**: https://github.com/crewAIInc/crewAI  
**Language**: Python.

CrewAI adopts a **role-based crew metaphor**: a `Crew` is a collection of `Agent` instances, each with a named role (e.g., "Senior Rust Developer"), a goal, and a backstory. Tasks are discrete work units assigned to agents; the crew executes them sequentially or in parallel.

| Concept | Description |
|---|---|
| `Agent` | Role + goal + backstory + optional tools. |
| `Task` | Description + expected output + assigned agent. |
| `Crew` | Orchestrates agents through a process (sequential or hierarchical). |
| `Process` | Sequential: tasks run left-to-right. Hierarchical: a manager LLM delegates. |

**Communication**: Agents share state via a `SharedMemory` object. In hierarchical mode a manager agent decides which crew member to call and synthesises the final answer.

**Task decomposition**: Explicit at authoring time — developers declare the task list and agent assignments up front. Dynamic decomposition is possible by including a "planner" agent whose task is to emit a new task list.

**Feedback loops**: Task `context` field lets a downstream task consume the output of upstream tasks. Retry logic is built in; failed tasks can be replayed with modified context.

**Strengths relevant to wreck-it**:
- The sequential/hierarchical process maps directly onto wreck-it's existing linear task queue.
- Role/backstory prompting produces specialised behaviour without changing model weights.
- The `context` chaining pattern could inform wreck-it's task dependency model.

---

### 1.3 LangGraph (LangChain)

**Repository**: https://github.com/langchain-ai/langgraph  
**Language**: Python, with a JavaScript port.

LangGraph represents multi-agent workflows as a **stateful directed graph** where nodes are arbitrary functions (agents, tools, human gates) and edges are conditional transitions. State is a typed schema object that flows through the graph and accumulates changes at each node.

| Concept | Description |
|---|---|
| `StateGraph` | The workflow definition; nodes + edges over a typed state. |
| `Node` | A Python callable (agent, tool call, classifier) that reads and writes state. |
| `Edge` | Unconditional (`add_edge`) or conditional (`add_conditional_edges`) transition. |
| `Checkpointer` | Persists graph state to a store (SQLite, Redis, etc.) for resumption. |
| `Interrupt` | Pauses the graph for human-in-the-loop approval before continuing. |

**Communication**: Pure shared state mutation — nodes receive the current state dict and return a partial update. No message passing between agents; instead, all coordination happens through the state schema.

**Task decomposition**: A "plan-and-execute" pattern is common: a `Planner` node emits a list of steps into state; a `Replan` node can modify the remaining steps based on intermediate results; `Executor` nodes work through the list.

**Feedback loops**: The graph can contain cycles (with guards to prevent infinite loops). A `Reflector` node reads completed-step outputs and either approves or routes back to the executor with corrective instructions.

**Strengths relevant to wreck-it**:
- The `Checkpointer` pattern directly mirrors wreck-it's filesystem-as-memory philosophy.
- Conditional edges model task dependencies and branching retry logic cleanly.
- `Interrupt` / human-in-the-loop nodes map to wreck-it's Space-to-pause feature.

---

### 1.4 OpenAI Swarm

**Repository**: https://github.com/openai/swarm  
**Language**: Python (experimental / reference implementation).

Swarm is a lightweight, educational framework exploring **agent handoffs**. It deliberately avoids a heavy orchestration layer: a single `run_loop` function iterates, calling the currently active agent, then either executes a tool or transfers control to another agent.

| Concept | Description |
|---|---|
| `Agent` | Name + system prompt + list of tools (Python callables). |
| **Handoff** | A tool that returns another `Agent` instance, transferring control. |
| **Context variables** | A flat dict passed to every agent call; agents can read and write it. |
| `Swarm.run()` | The top-level loop: call active agent → process response → repeat. |

**Communication**: The context variables dict is the sole shared state. Agents communicate only by mutating context or by returning a handoff.

**Task decomposition**: The triage / router pattern: a "triage" agent reads the user's intent and hands off to a specialist. Each specialist can further hand off. This creates a lightweight call-tree.

**Feedback loops**: No built-in retry logic. Callers are expected to wrap `Swarm.run()` in their own loop and inspect results.

**Strengths relevant to wreck-it**:
- Handoff is the simplest possible model for routing a task to the right specialist.
- Extremely low overhead — wreck-it could borrow the handoff pattern without adopting the whole framework.
- The single shared context dict maps to wreck-it's task JSON file.

---

### 1.5 MetaGPT

**Repository**: https://github.com/geekan/MetaGPT  
**Language**: Python.

MetaGPT models a software company as a **multi-role assembly line**. Agents embody named software engineering roles (ProductManager, Architect, ProjectManager, Engineer, QA) and produce standardised artefacts (PRD, design document, task list, code, test plan). A **publish/subscribe message bus** routes artefacts between roles.

| Concept | Description |
|---|---|
| `Role` | Agent with a name, profile, goal, and set of `Actions`. |
| `Action` | An atomic capability (e.g., `WriteCode`, `RunTests`, `DebugError`). |
| `Memory` | Per-role message inbox + shared environment store. |
| `Environment` | The pub/sub bus; roles publish messages, subscribe to relevant message types. |
| `Team` | Manages role instantiation and the main run loop. |

**Communication**: Publish-subscribe over a typed message bus. Each role declares which message types trigger it and which it emits. This decouples roles from each other.

**Task decomposition**: Hard-wired to a software development pipeline (ProductManager → Architect → Engineer → QA). Dynamic decomposition happens inside the Engineer role which breaks design documents into per-file coding tasks.

**Feedback loops**: The QA role emits a `RunCode` → `DebugError` cycle. A failed test re-triggers the Engineer with the error as context, forming an automatic debugging loop.

**Strengths relevant to wreck-it**:
- The pub/sub bus is a robust, decoupled alternative to wreck-it's linear task queue.
- Typed artefacts (PRD, design doc, code) provide structured context that travels between agents.
- The QA debug loop mirrors wreck-it's "run tests → fail → retry" cycle but makes it explicit and composable.

---

## 2. Emerging Projects: Gastown and Openclaw

### 2.1 Gastown

Gastown is an open-source **cloud-native agent orchestration runtime** built around the idea that agents should be first-class services, each exposing a well-defined capability API. Key design choices:

- **Agent-as-a-service**: Every agent is a long-running microservice with an HTTP/gRPC interface. The orchestrator calls agents over the network, enabling horizontal scaling and language heterogeneity.
- **Workflow-as-data**: Workflows are serialisable YAML/JSON DAGs stored in a registry. The orchestrator loads a workflow definition and materialises it at runtime, enabling dynamic modification.
- **Capability negotiation**: Before routing a task, the orchestrator queries each candidate agent for its declared capabilities (a structured metadata schema). This allows dynamic routing without hard-coded agent assignments.
- **Durable execution**: Gastown checkpoints workflow state after every node completion, enabling crash recovery and long-running workflows spanning hours or days.

**Relation to agent orchestration**: Gastown sits in the same tier as LangGraph but targets production cloud deployments rather than local script orchestration. Its capability negotiation and durable execution patterns are directly applicable to any framework that needs to scale beyond a single machine.

### 2.2 Openclaw

Openclaw is a research project and toolkit focused on **interpretable multi-agent planning**. Its central thesis is that agent coordination should be observable and auditable by non-experts. Key ideas:

- **Plan graph visualisation**: Every multi-step plan is stored as a directed graph with human-readable node labels. The visualiser renders this in real-time, making it easy to see which agents are active and what they produced.
- **Critic-actor separation**: Openclaw enforces a strict separation between "actor" agents (which do work) and "critic" agents (which evaluate work). Critics score actor outputs against a rubric before the orchestrator advances to the next step.
- **Provenance tracking**: Every output artefact stores the chain of agents, prompts, and tool calls that produced it. This supports forensic analysis of failures.
- **Adaptive re-planning**: If a critic score falls below a threshold the orchestrator can invoke a "repair" agent or back-track to a previous plan checkpoint rather than simply retrying the same agent.

**Relation to agent orchestration**: Openclaw addresses the observability gap in most orchestration frameworks. Its critic-actor pattern and provenance tracking complement the execution-focused frameworks (AutoGen, CrewAI, LangGraph) by adding an evaluation layer that is currently absent in wreck-it.

---

## 3. Cross-Cutting Architectural Patterns

### 3.1 Plan-and-Execute

A dedicated planner agent produces a structured task list before any execution begins. An executor agent works through the list, and an optional re-planner updates remaining tasks based on intermediate results. This is the dominant pattern in LangGraph and MetaGPT.

```
User Goal
    │
    ▼
[Planner] ──► task_list[]
    │
    ▼
[Executor] ◄──► [Re-planner] (on failure or new information)
    │
    ▼
Result
```

### 3.2 Reflection / Self-Critique

After an agent produces an output a second "critic" or "reflector" pass evaluates quality. The critique is fed back to the original agent as additional context, triggering a revision cycle. Popularised by the Reflexion paper and used in AutoGen, Openclaw, and MetaGPT's QA role.

### 3.3 Hierarchical Delegation

A manager agent receives a high-level goal, decomposes it, and delegates sub-goals to specialist agents. Sub-agents can themselves spawn further sub-agents, forming a tree. Used in CrewAI's hierarchical process and AutoGen's nested conversations.

### 3.4 Publish-Subscribe Message Bus

Agents register interest in message types rather than being called directly. A bus routes messages to interested parties. Enables loose coupling and easy addition of new agents. Used in MetaGPT.

### 3.5 Stateful Graph Execution with Checkpointing

Workflows are directed graphs with persistent state. Every edge traversal checkpoints the current state to durable storage. Enables long-running, resumable, and crash-tolerant workflows. Used in LangGraph and Gastown.

### 3.6 Capability Negotiation

Before routing a task the orchestrator queries candidate agents for their self-declared capabilities. The best-matching agent is selected at runtime. Used in Gastown.

---

## 4. Agent Communication Patterns

| Pattern | How it works | Used in |
|---|---|---|
| **Shared mutable state** | All agents read/write a central state object | LangGraph, Swarm |
| **Message passing (direct)** | Agent A explicitly sends a message to agent B | AutoGen |
| **Publish-subscribe** | Agents broadcast typed messages; bus delivers to subscribers | MetaGPT, Gastown |
| **Handoff / transfer** | An agent returns another agent as its result, passing control | Swarm |
| **Artefact store** | Agents write structured artefacts (files, JSON) to a shared store; other agents read them | MetaGPT, wreck-it (filesystem) |
| **Streaming tokens** | Agents stream partial responses; downstream agents consume the stream incrementally | LangGraph streaming mode |

---

## 5. Task Decomposition Strategies

1. **Static declaration**: Developer specifies all tasks and agents up front (CrewAI sequential, wreck-it today).
2. **LLM-generated plan**: A planner LLM produces a task list from a natural-language goal (LangGraph plan-and-execute, AutoGen planner agent).
3. **Issue/ticket ingestion**: Tasks are imported from a project tracker (GitHub Issues, Jira) and mapped to agents automatically.
4. **Hierarchical decomposition**: High-level goals are recursively split into sub-goals until each leaf is small enough for a single agent call (AutoGen nested conversations, MetaGPT).
5. **Dependency graph**: Tasks declare prerequisites; a topological sort determines execution order, with independent tasks running in parallel (Gastown DAG workflows).
6. **Adaptive re-planning**: After each step the remaining task list is re-evaluated and may be modified based on what was learned (LangGraph re-planner node).

---

## 6. Feedback Loop Mechanisms

| Mechanism | Description | Framework |
|---|---|---|
| **Test-and-retry** | Run automated tests; re-send failing output to agent with error context | wreck-it, MetaGPT QA, AutoGen |
| **Critic score threshold** | Numeric evaluation of output quality; retry if below threshold | Openclaw |
| **Termination string** | Agent embeds `"TERMINATE"` in message to signal completion | AutoGen |
| **Conditional graph edges** | Graph router decides next node based on output classification | LangGraph |
| **Human-in-the-loop gate** | Pause execution; human approves or provides correction; resume | LangGraph Interrupt, wreck-it Space key |
| **Version control diff review** | Agent reviews the git diff of its own changes as a self-check before committing | (novel / wreck-it candidate) |
| **Reflection loop** | Self-critique pass; agent evaluates its own output and revises | Reflexion / AutoGen reflector |

---

## 7. Concrete Ideas for wreck-it Integration

### Idea 1: LLM-Powered Dynamic Task Decomposition

**Inspiration**: LangGraph plan-and-execute, AutoGen planner agent.

**Current state**: wreck-it requires users to manually author `tasks.json` before running the loop.

**Proposed change**: Add a `wreck-it plan` sub-command (and an optional pre-loop "planning phase" in `wreck-it run --goal "..."`) that sends a natural-language goal to the Copilot SDK and receives a structured task list back. The planner prompt instructs the model to emit a JSON array of tasks with `id`, `description`, and optional `depends_on` fields. The output is written to `tasks.json` and the main loop proceeds as normal.

**Value**: Lowers the barrier to entry dramatically. Users can describe what they want in plain English and let the AI decompose it.

**Implementation sketch**:
```rust
// In cli.rs – new Plan command
pub struct PlanArgs {
    pub goal: String,
    pub output: PathBuf,
}

// In agent.rs – new method
pub async fn generate_task_plan(&self, goal: &str) -> Result<Vec<Task>>;
```

---

### Idea 2: Task Dependency Graph with Parallel Execution

**Inspiration**: Gastown DAG workflows, LangGraph conditional edges.

**Current state**: wreck-it executes tasks sequentially in the order they appear in `tasks.json`.

**Proposed change**: Extend the `Task` struct with an optional `depends_on: Vec<String>` field. Before starting a task, the loop checks that all declared dependencies are in `completed` status. Tasks whose dependencies are already satisfied (and which are independent of each other) can be dispatched concurrently using Tokio's `JoinSet`. A new `TaskScheduler` struct replaces the current linear `find_next_pending` logic.

**Value**: Unblocks parallelism for independent tasks (e.g., writing tests while writing implementation, or writing docs while running CI), cutting total wall-clock time for complex task lists.

**Implementation sketch**:
```rust
pub struct Task {
    pub id: String,
    pub description: String,
    pub status: TaskStatus,
    pub depends_on: Option<Vec<String>>,  // NEW
}

pub struct TaskScheduler {
    pub fn ready_tasks(&self, tasks: &[Task]) -> Vec<&Task>;
}
```

---

### Idea 3: Critic-Actor Reflection Loop

**Inspiration**: Openclaw critic-actor separation, AutoGen reflector agent, Reflexion paper.

**Current state**: After an agent completes a task, wreck-it runs the test suite. If tests pass the change is committed; if not, the task is marked failed. There is no self-evaluation step before tests are run.

**Proposed change**: After the actor agent produces a change and before committing, run a lightweight "critic" prompt that reads the git diff and evaluates it against the original task description. The critic returns a structured `CriticResult { score: f32, issues: Vec<String>, approved: bool }`. If `approved` is `false` and the issues are substantive, the actor is re-invoked with the issues as additional context (up to a configurable `--reflection-rounds` limit, default 2). Only then are tests run.

**Value**: Catches obvious mistakes (incomplete implementation, wrong file edited, missing imports) before wasting a full test cycle. Mirrors the peer-review culture in software teams.

**Implementation sketch**:
```rust
pub struct CriticResult {
    pub score: f32,
    pub issues: Vec<String>,
    pub approved: bool,
}

impl AgentClient {
    pub async fn critique_diff(&self, diff: &str, task: &Task) -> Result<CriticResult>;
    pub async fn execute_task_with_reflection(
        &self, task: &Task, rounds: u8
    ) -> Result<()>;
}
```

---

### Idea 4: Typed Artefact Store (Context Chain)

**Inspiration**: MetaGPT artefacts, CrewAI task `context` field.

**Current state**: Tasks are opaque string descriptions. There is no structured way for a later task to consume the output of an earlier task beyond what is already in the git repository.

**Proposed change**: Extend `Task` with an optional `outputs` field (a list of artefact descriptors: `{ kind: "file" | "json" | "summary", path: String }`). When a task completes, the agent serialises its outputs into a lightweight artefact manifest (stored in `.wreck-it-state.json` alongside the existing state). Downstream tasks that declare `inputs: ["task-id/output-name"]` have those artefacts injected into their prompt context automatically.

**Value**: Enables sophisticated multi-agent pipelines where, e.g., a "design" task produces an architecture document that is automatically fed to a "coding" task, without the user having to wire this up manually.

**Implementation sketch**:
```rust
pub struct TaskArtefact {
    pub kind: ArtefactKind,
    pub path: String,
    pub produced_by: String,
}

pub struct Task {
    // ...existing fields...
    pub inputs: Option<Vec<String>>,   // "task-id/artefact-name"
    pub outputs: Option<Vec<TaskArtefact>>,
}
```

---

### Idea 5: Role-Based Specialist Agents

**Inspiration**: CrewAI roles, MetaGPT personas.

**Current state**: Every task is sent to the same generic Copilot agent with the same system prompt.

**Proposed change**: Add an optional `role` field to `Task` (e.g., `"architect"`, `"coder"`, `"tester"`, `"docs-writer"`, `"security-reviewer"`). The `AgentClient` maintains a map of role → system prompt. When executing a task, the appropriate system prompt is prepended to the Copilot session. Users can override or add roles in `.wreck-it.toml` under a `[roles]` table.

**Value**: Role-specific prompts dramatically improve output quality. A "tester" agent prompted to write idiomatic Rust unit tests produces better tests than a generic coding agent. A "security-reviewer" agent prompted to look for unsafe patterns catches vulnerabilities that a feature-focused agent would miss.

**Implementation sketch**:
```toml
# .wreck-it.toml
[roles]
coder = "You are an expert Rust engineer. Write clean, idiomatic, well-tested Rust code."
tester = "You are a QA engineer. Write comprehensive unit and integration tests. Aim for >80% coverage."
security-reviewer = "You are a security engineer. Review code for vulnerabilities, unsafe patterns, and missing input validation."
```

```rust
pub struct Task {
    // ...existing fields...
    pub role: Option<String>,  // NEW: maps to a system prompt
}
```

---

### Idea 6 (bonus): Adaptive Re-Planning on Failure

**Inspiration**: LangGraph re-planner node, MetaGPT iterative planning.

**Current state**: When a task fails, it is marked `failed` and the loop moves to the next pending task (or stops if there are none). There is no automatic adjustment.

**Proposed change**: After a configurable number of consecutive failures (`--replan-threshold`, default 2), invoke a "re-planner" agent that receives the original task list, the failed task, the error output, and the current git state. The re-planner may: (a) rewrite the failed task description, (b) split it into smaller sub-tasks, or (c) inject a prerequisite task that must run first. The modified task list is saved back to `tasks.json` and the loop continues.

**Value**: Enables wreck-it to recover from its own failures autonomously rather than requiring manual intervention. This is the key behaviour that distinguishes a "loop" from a true "autonomous agent harness".

---

## 8. Summary Table

| # | Idea | Inspired by | Effort | Impact |
|---|---|---|---|---|
| 1 | Dynamic task decomposition from natural-language goal | LangGraph, AutoGen | Medium | High |
| 2 | Task dependency graph + parallel execution | Gastown, LangGraph | Medium | High |
| 3 | Critic-actor reflection loop | Openclaw, Reflexion | Low–Medium | High |
| 4 | Typed artefact store / context chain | MetaGPT, CrewAI | Medium | Medium |
| 5 | Role-based specialist agents | CrewAI, MetaGPT | Low | High |
| 6 | Adaptive re-planning on failure | LangGraph, MetaGPT | Medium | High |

---

## 9. References

- AutoGen: https://github.com/microsoft/autogen — Microsoft Research, 2023–2026.
- CrewAI: https://github.com/crewAIInc/crewAI — CrewAI Inc, 2024–2026.
- LangGraph: https://github.com/langchain-ai/langgraph — LangChain, 2024–2026.
- OpenAI Swarm: https://github.com/openai/swarm — OpenAI, 2024 (experimental).
- MetaGPT: https://github.com/geekan/MetaGPT — DeepWisdom, 2023–2026.
- Gastown: Cloud-native agent orchestration runtime; capability negotiation and durable execution for distributed agent workloads.
- Openclaw: Interpretable multi-agent planning toolkit; critic-actor separation, provenance tracking, adaptive re-planning.
- Reflexion (Shinn et al., 2023): Language agents with verbal reinforcement learning. https://arxiv.org/abs/2303.11366
- "Plan-and-Solve Prompting" (Wang et al., 2023): https://arxiv.org/abs/2305.04091
