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

---

---

# AI-Driven Code Development Tools: Research Notes

> **Task**: `ideas-2`  
> **Date**: 2026-02-27  
> **Scope**: How Devin, SWE-Agent, OpenHands, Aider, and Cursor agent mode handle long-running autonomous coding sessions, self-healing/retry strategies, context management across iterations, and quality feedback mechanisms. Updated observations on gastown and openclaw. Actionable improvements for wreck-it's Ralph Wiggum loop.

---

## 1. Tool Survey

### 1.1 Devin (Cognition AI)

**URL**: https://www.cognition.ai/blog/introducing-devin  
**Type**: End-to-end autonomous software engineer with a dedicated sandboxed computer.

Devin is the first commercial product to package an LLM agent together with a complete, persistent **developer workstation**: a shell, an editor, a browser, and a private network. The agent controls all of these tools through structured action sequences.

#### Long-running session management

Devin maintains a session that can span hours. It achieves this through:

- **Sandboxed VM persistence**: The agent's entire working environment (open files, running processes, browser tabs, shell history) is preserved in a VM snapshot. A session can be paused and resumed without losing state.
- **Session timeline**: Every action is recorded in a linear timeline visible to both the user and the agent. The agent can scroll back through this timeline to recall what it did earlier, effectively using it as an external memory.
- **Explicit "scratchpad" notes**: Devin maintains a running planning document inside its editor. Before starting work the agent writes a multi-step plan; as steps complete it checks them off. This externalises working memory and survives context-window limits.

#### Self-healing / retry strategy

- **Iterative debugging**: When tests fail, Devin reads the test output, forms a hypothesis about the root cause, edits code, and re-runs tests. It repeats this loop autonomously until tests pass or it decides it needs human input.
- **Fallback to search**: If a compilation error references an unfamiliar API, Devin opens the browser and searches the documentation before retrying.
- **Explicit failure escalation**: After a configurable number of failed attempts, Devin posts a message to the human asking for clarification rather than looping indefinitely.

#### Context management

- **Scratchpad plan file** keeps structured state outside the LLM context window.
- **Session timeline** allows "look back" without replaying the full conversation.
- **Minimal context injection**: Only the most relevant recent events and the current scratchpad are injected into each LLM call; older events are summarised to a few sentences.

#### Quality feedback

- **Integrated test runner**: Devin runs the project's own test suite after every code change.
- **Browser-based smoke tests**: For web projects, Devin opens the result in a browser and visually inspects it.
- **User review gate**: The agent presents its work-in-progress to the user at configurable checkpoints (e.g., after completing each top-level plan step) and waits for explicit approval before continuing.

---

### 1.2 SWE-Agent (Princeton NLP)

**Repository**: https://github.com/SWE-agent/SWE-agent  
**Type**: Research agent that solves GitHub issues against real codebases.

SWE-Agent wraps an LLM with a purpose-built **Agent-Computer Interface (ACI)** — a constrained set of commands (open, view, edit, search, run) that keeps the action space small and predictable.

#### Long-running session management

- **Trajectory files**: Every agent run produces a `.traj` JSON file that records every observation, action, and LLM call. Trajectories are the canonical record of what the agent did and can be replayed or inspected offline.
- **Action budget**: Each run has a configurable maximum number of steps. The agent is aware of its remaining budget and adjusts strategy (e.g., switches from exploration to targeted editing) as the budget shrinks.
- **Structured task framing**: The issue text is injected once at the start as a "system context" block that never scrolls out of the context window. Only the running action/observation interleave is trimmed to fit.

#### Self-healing / retry strategy

- **Linting before submission**: SWE-Agent runs a project linter after every edit. Lint errors are fed back as observations and the agent edits again. This is the primary feedback loop.
- **Search-then-edit discipline**: The ACI forces the agent to search for the relevant file/function before editing, reducing "edit the wrong file" errors.
- **Graceful degradation**: If the agent cannot reproduce the bug after N attempts it explicitly marks the issue as "unable to reproduce" and submits a partial patch with explanatory comments.

#### Context management

- **Strict ACI command set** limits hallucinations about available tools.
- **Summarise-on-truncate**: When the rolling context approaches the model limit, SWE-Agent summarises the last N observations into a paragraph and drops the raw observations.
- **Persistent issue context**: The original GitHub issue text is always present at position 0 of the prompt; it acts as a stable anchor.

#### Quality feedback

- **Test suite execution**: SWE-Agent runs the project's full test suite at the end of each editing cycle and reports pass/fail counts.
- **Diff inspection**: Before finalising, the agent reads its own `git diff` and checks that the changed lines are plausibly related to the issue description.
- **SWE-bench evaluation**: External CI re-runs the agent's patch on the benchmark harness and scores it (resolved / unresolved) — a post-hoc quality signal used during research evaluation.

---

### 1.3 OpenHands (formerly OpenDevin)

**Repository**: https://github.com/All-Hands-AI/OpenHands  
**Type**: Open-source autonomous software development platform.

OpenHands runs agents inside a **sandboxed Docker container** and provides three interchangeable agent implementations (CodeAct, ReAct, and a Browsing agent) over the same runtime.

#### Long-running session management

- **Event stream architecture**: All interactions between the agent and the runtime are serialised to an **event stream** (not a chat history). The event stream is the ground truth; the LLM context is rebuilt by replaying recent events.
- **Session persistence**: Event streams are stored on disk and can be resumed across restarts. The agent re-reads recent events on startup and continues as if uninterrupted.
- **Microagent knowledge base**: Domain-specific facts (e.g., "this repo uses pytest, not unittest") are stored in a `microagents/` directory and injected as compact context blocks at the start of each LLM call, keeping episodic knowledge outside the rolling window.

#### Self-healing / retry strategy

- **CodeAct re-execution**: The CodeAct agent generates Python code that it runs in the sandbox. If the code raises an exception, the exception is the next observation; the agent diagnoses it and generates corrected code. This tight read-eval-print loop is the core self-healing mechanism.
- **Configurable max retries**: `AgentController` wraps each agent step in a retry loop (`max_retries`, default 5) with exponential back-off before raising a hard error.
- **Delegating to sub-agents**: When a task is out of scope for the current agent, the `DelegateAction` primitive hands off to a specialised sub-agent (e.g., browsing to research a dependency, then back to coding).

#### Context management

- **Condensation pass**: When the event stream exceeds a token budget, a background `CondenserAgent` summarises older events into a compact "memory block" inserted at the top of the context.
- **Microagent injection**: Short, curated knowledge fragments are injected per-repo, per-language, or per-framework at session start — static context that never consumes rolling-window tokens.
- **Structured observations**: Every tool call returns a typed `Observation` struct with a `content` string and metadata. Agents can filter or truncate observation content without losing the structured metadata.

#### Quality feedback

- **Integrated test runner**: `CmdRunAction` runs the project's test command; the `CmdOutputObservation` carries stdout/stderr. The agent inspects the output and decides whether to commit or iterate.
- **Agent-written evaluation scripts**: The CodeAct agent can generate and run ad-hoc validation scripts (e.g., import the module, call the function with known inputs, assert the expected output).
- **Human-in-the-loop pause**: If the agent detects ambiguity (e.g., multiple valid interpretations of a requirement) it emits a `MessageAction` addressed to the user and waits for an `AgentMessageObservation` before continuing.

---

### 1.4 Aider

**Repository**: https://github.com/Aider-AI/aider  
**Type**: Interactive CLI coding assistant with strong git integration.

Aider is session-based but designed for short-to-medium interactive coding tasks. Its primary innovation is a **git-native workflow** — every accepted change is immediately committed with an AI-generated message, and the model always sees the current repo map.

#### Long-running session management

- **Repo map**: Aider builds a compact, structured summary of the repository (file names, class and function signatures) and injects it into every prompt. The repo map stays current after each edit, giving the model accurate structural context without sending all source files.
- **Chat history file**: Conversations are persisted to `.aider.chat.history.md`. On restart the user can `/load` the history to resume context.
- **In-context file management**: Users explicitly `/add` files to the "active set"; only those files are sent in full. The repo map covers the rest. This is a manual but effective form of context window management.

#### Self-healing / retry strategy

- **Linting on save**: After applying an edit, Aider runs the repo's linter. Lint errors are shown to the user (and optionally auto-sent back to the model as a follow-up message) for immediate correction.
- **Test command integration**: `--test-cmd` runs tests after each commit. Failures are shown and, in `--auto-test` mode, automatically sent back to the model for fixing.
- **Watch mode**: `--watch-files` monitors the codebase for changes made by the user or other tools. When it detects a change it updates the repo map and optionally asks the model what to do next.
- **`/undo`**: Reverts the last AI commit if the user is unsatisfied. Clean git history means rollback is always safe.

#### Context management

- **Repo map algorithm**: Uses tree-sitter to extract symbols and ranks files by relevance to the current conversation topic using a graph-based PageRank-style algorithm. Only the top-ranked subset is included in the map.
- **`/tokens`** command: Shows current prompt token usage so the user can actively manage the context budget.
- **Architect mode**: A separate "architect" model first produces a plan with file edits, then a cheaper "editor" model applies them. The architect never needs to see full file contents, only the plan-level abstraction.

#### Quality feedback

- **Commit message generation**: Every commit message is written by the model to accurately describe the change, creating a human-readable audit trail.
- **Diff display**: Aider shows the exact diff before applying it and asks for confirmation in interactive mode.
- **Benchmark-driven development**: Aider's authors run the SWE-bench benchmark continuously against new releases; results are published publicly, providing an external quality signal.

---

### 1.5 Cursor Agent Mode

**URL**: https://www.cursor.com  
**Type**: IDE-integrated autonomous coding agent.

Cursor's agent mode operates inside the IDE, with access to the full file tree, terminal, and LSP diagnostics. It is distinguished by its tight feedback loop with the language server.

#### Long-running session management

- **Checkpoint system**: Agent mode automatically creates a git stash (labelled "Cursor checkpoint") before starting a multi-file task. Users can revert to any checkpoint if the agent's changes diverge from intent.
- **Apply-then-verify loop**: Rather than editing files directly, the agent generates a diff, applies it via the IDE's editor API, then reads the resulting LSP diagnostics (red squiggles) before continuing. This closes the write-check loop much tighter than terminal-based tools.
- **Task scoping**: Users frame tasks as "composer" sessions with a clear intent string. The agent decomposes the task into steps and tracks progress internally, summarising completed steps in the chat pane.

#### Self-healing / retry strategy

- **LSP error feedback**: After every file save, Cursor reads the language-server diagnostics for the changed files. Type errors, missing imports, and undefined symbols are fed back to the model automatically as observations, triggering immediate correction attempts.
- **Terminal output observation**: The agent runs build/test commands in an embedded terminal and reads output. On failure it parses the error, targets the relevant file/line, and issues a corrective edit.
- **Shadow workspace**: Edits are applied in a hidden "shadow" workspace first; only after the shadow validates (LSP clean, optional tests pass) are they written to the real workspace.

#### Context management

- **Codebase index**: Cursor maintains a vector-embedded index of the entire codebase. At each agent step it retrieves the top-k most semantically relevant code snippets to inject as context — similar to RAG but over code.
- **`@file` / `@symbol` references**: Users and the agent explicitly cite files and symbols; cited artifacts are injected in full. Uncited code is represented only by the indexed summaries.
- **Progressive context expansion**: If the agent cannot answer a question from indexed summaries it requests the full content of specific files, expanding context on demand rather than front-loading everything.

#### Quality feedback

- **Inline diff review**: All proposed changes appear as inline diffs in the editor with Accept/Reject buttons. The user can accept individually per change hunk.
- **LSP-gated commits**: In strict mode, Cursor refuses to commit a change that has LSP errors in the modified files.
- **Test panel integration**: Results from the IDE's test runner appear in the agent chat pane alongside the diff, making pass/fail immediately visible without switching context.

---

## 2. Updated Observations: Gastown and Openclaw

### 2.1 Gastown — Durable Execution and Session Resumption

Recent Gastown developments emphasise **session durability** for long-running coding workflows:

- **Workflow versioning**: Workflow definitions are now versioned (semver). Running instances can be migrated to a new workflow version mid-execution using a schema-compatible upgrade path. This is critical for coding sessions that span days while the orchestrator is being actively developed.
- **Agent heartbeats and watchdog**: Each agent service emits a heartbeat every N seconds. If the orchestrator misses K consecutive heartbeats it marks the agent as unhealthy, reschedules the current task onto a healthy replica, and increments a `retry_count` field on the task. Tasks exceeding `max_retries` are moved to a dead-letter queue for human review.
- **Capability versioning**: Agent capability schemas now include a `min_version` field. The orchestrator rejects task routing to agents whose capability version is below the task's requirement, preventing subtle version-mismatch failures that only surface at runtime.
- **Streaming checkpoints**: Rather than checkpointing only after node completion, Gastown can be configured to emit micro-checkpoints every `checkpoint_interval_ms` milliseconds. This reduces the maximum re-execution window from "the entire node" to a user-configurable slice.

### 2.2 Openclaw — Adaptive Re-planning and Provenance

Recent Openclaw work focuses on making **failure recovery** first-class:

- **Re-plan triggers**: Openclaw now supports three distinct re-plan triggers beyond the original critic-score threshold: (a) consecutive failure count, (b) external signal (a CI webhook), and (c) time budget exhaustion. Each trigger produces a structured `ReplannedEvent` that is stored in the provenance graph.
- **Counterfactual analysis**: When a critic rejects an output, Openclaw's new `CounterfactualExplainer` generates a structured "what should the actor have done instead?" explanation. This is injected into the next actor call alongside the original task — effectively teaching the actor from its own failure.
- **Provenance diffing**: Two execution traces can now be structurally diffed to identify which step diverged. This is invaluable for comparing a successful baseline run with a failing re-run after a model or prompt change.
- **Policy-based escalation**: Admins configure an escalation policy (YAML) that specifies when failures should be escalated to a human, when they should be automatically re-tried with a different actor, and when they should silently fail. This replaces the previous hard-coded escalation logic.

---

## 3. Cross-Cutting Patterns for Long-Running Sessions

The tools surveyed above reveal several common patterns that specifically address long-running autonomous coding sessions:

| Pattern | Tools using it | Core idea |
|---|---|---|
| **Externalised scratchpad** | Devin, Aider | Agent writes a plan to a file; plan file persists across context window resets |
| **Event stream / trajectory** | OpenHands, SWE-Agent | All actions logged to disk; context rebuilt from recent events, not chat history |
| **Codebase index / repo map** | Cursor, Aider | Compact structural summary of the whole repo injected each call; full files fetched on demand |
| **Sandbox snapshots** | Devin, Cursor | VM or workspace state persisted so sessions survive restarts |
| **Action budget awareness** | SWE-Agent, Devin | Agent knows its remaining step/cost budget and shifts strategy accordingly |
| **Heartbeat + watchdog** | Gastown | Infrastructure-level health monitoring with automatic task rescheduling |

### Self-Healing Pattern Taxonomy

```
Tier 1 – Immediate (same step)
  └── LSP / lint feedback → edit → re-check  [Cursor, Aider]

Tier 2 – Short loop (2-5 steps)  
  └── Test failure → diagnose → edit → re-run tests  [all tools]

Tier 3 – Medium loop (task-level)
  └── Critic rejection + counterfactual → re-execute task  [Openclaw]
  └── Failure count threshold → re-plan task  [Devin, LangGraph]

Tier 4 – Session-level
  └── Dead-letter queue + human review  [Gastown]
  └── Human-in-the-loop escalation  [Devin, OpenHands, Cursor]
```

---

## 4. Context Management Strategies — Comparative Summary

| Strategy | Reduces tokens? | Survives restart? | Handles large repos? | Used in |
|---|---|---|---|---|
| Rolling window truncation | ✓ | ✗ | ✓ (lossy) | SWE-Agent |
| Summarise-on-truncate | ✓ | ✗ | ✓ | OpenHands |
| Event stream replay | ✗ | ✓ | ✓ | OpenHands |
| Repo map (symbol index) | ✓✓ | ✓ | ✓✓ | Aider, Cursor |
| Scratchpad plan file | ✓✓ | ✓ | ✓ | Devin |
| Microagent knowledge injection | ✓✓ | ✓ | ✓✓ | OpenHands |
| Vector RAG over codebase | ✓✓ | ✓ | ✓✓ | Cursor |
| Explicit file set management | ✓ | partial | ✓ | Aider |

---

## 5. Quality Feedback Mechanism Summary

| Mechanism | Latency | Automation | Strength |
|---|---|---|---|
| LSP diagnostics after each save | Milliseconds | Full | Type/syntax errors before tests |
| Linter on edit | Seconds | Full | Style + common bugs |
| Test suite on commit | Seconds–minutes | Full | Regression detection |
| Critic score (agent) | Seconds–minutes | Full | Semantic correctness beyond syntax |
| Diff review by same agent | Seconds | Full | Catches "wrong file" / incomplete edits |
| Human checkpoint approval | Minutes–hours | None | High-level intent validation |
| External CI / SWE-bench | Minutes–hours | Full | Ground-truth benchmark evaluation |
| Counterfactual explanation | Seconds | Full | Structured "what to do differently" |

---

## 6. Actionable Improvements for wreck-it's Ralph Wiggum Loop

### Improvement 1: Tiered Self-Healing Retry Strategy

**Inspired by**: Devin escalation model, Gastown dead-letter queue, Openclaw re-plan triggers.

**Problem**: Today wreck-it marks a task `failed` after a single unsuccessful attempt and moves on. There is no automatic retry with corrective context, and failures do not accumulate into a structured escalation path.

**Proposed change**: Replace the single `Failed` state with a three-tier retry system:

- **Tier 1 — Immediate retry** (up to 2 rounds): Re-invoke the agent with the test/evaluation error output appended to the original task description. This costs one extra LLM call but resolves most transient failures.
- **Tier 2 — Re-planned retry** (up to 1 round): If Tier 1 retries are exhausted, invoke a lightweight "re-planner" prompt that rewrites the task description based on the accumulated error context, then try again.
- **Tier 3 — Dead-letter**: If all retries are exhausted, mark the task `failed` and append a structured `FailureSummary` (error text, attempted fixes, re-planner reasoning) to the task JSON so the human has a clear audit trail.

```rust
// types.rs additions
pub struct RetryContext {
    pub attempt: u8,
    pub error_output: String,
}

pub enum TaskStatus {
    Pending,
    InProgress,
    Completed,
    Failed,
    // (internal during loop execution)
    RetryingTier1 { attempt: u8 },
    RetryingTier2,
}
```

**Configuration** (`.wreck-it.toml`):
```toml
[retry]
tier1_max_attempts = 2    # immediate retry with error context
tier2_enabled = true      # enable re-planner rewrite
```

**Value**: Transforms wreck-it from "fail fast" to "fail smart". The majority of real-world task failures (wrong import, off-by-one, missing file) are solvable with one retry; adding error context costs little and saves human intervention.

---

### Improvement 2: Externalised Agent Scratchpad

**Inspired by**: Devin's plan file, Aider's chat history file.

**Problem**: wreck-it's loop restarts the agent for every task iteration. The agent has no memory of what it tried in previous iterations of the same task. Any reasoning it did (e.g., "I need to refactor X before implementing Y") is lost between iterations.

**Proposed change**: Before calling `execute_task`, write a structured **scratchpad file** (`.wreck-it-scratchpad-<task-id>.md`) to the working directory. The scratchpad includes:

1. The original task description.
2. A chronological list of previous attempts (iteration number, what was tried, test outcome).
3. A free-form "notes" section that the agent can update.

The agent's task prompt is extended: *"Review the scratchpad file `.wreck-it-scratchpad-<id>.md` before starting work. After completing your work, update the scratchpad with what you did and what the outcome was."*

On task completion (success or exhausted retries) the scratchpad is deleted.

**Value**: Gives the agent persistent episodic memory for multi-iteration tasks without any changes to the underlying LLM. Directly reduces repeated mistakes across loop iterations.

**Implementation note**: The scratchpad is managed by wreck-it in Rust (create before call, read after call to update state), not by the agent itself. The agent simply reads and writes a plain markdown file.

---

### Improvement 3: Repo Map Context Injection

**Inspired by**: Aider's repo map, Cursor's codebase index.

**Problem**: Each task prompt sent to the Copilot agent contains only the task description and any manually specified context. The agent must spend tokens "exploring" the repository (reading file lists, grepping for symbols) that it would not need to spend if it had a compact structural overview upfront.

**Proposed change**: Before executing any task, wreck-it generates a compact **repo map** and prepends it to the task prompt. The repo map is a plain-text summary of:

- All source files with their sizes (in lines).
- For `.rs` / `.ts` / `.py` files: top-level struct/class/function names (extracted via a simple regex or `tree-sitter` binding).
- Current `git status` (modified/new/deleted files) so the agent knows what has already changed.

The repo map is regenerated at the start of each iteration (cheap — it is just a shell script or a few hundred lines of Rust).

```rust
// agent.rs additions
pub fn generate_repo_map(work_dir: &Path) -> String;

// called in execute_task():
let map = generate_repo_map(&self.work_dir);
let augmented_prompt = format!("## Repo Map\n{}\n\n## Task\n{}", map, task.description);
```

**Value**: Reduces the number of exploratory tool calls per task (cheaper, faster). Gives the agent immediate structural awareness — it will correctly target `cli/src/ralph_loop.rs` rather than hunting for it.

---

### Improvement 4: Structured Evaluation Rubric per Task

**Inspired by**: Openclaw critic-actor pattern, Devin user review gate.

**Problem**: wreck-it's current evaluation modes are binary: either the shell verify command passes (exit 0) or it doesn't. The agent gets no structured feedback about *why* the task is incomplete, only that it failed.

**Proposed change**: Extend the `Task` struct with an optional `acceptance_criteria` field — a list of human-readable conditions that must be true for the task to be considered done:

```json
{
  "id": "3",
  "description": "Add retry logic to the Ralph loop",
  "status": "pending",
  "acceptance_criteria": [
    "A failed task is retried at least once before being marked failed",
    "The retry attempt count is visible in the TUI log",
    "Existing tests continue to pass"
  ]
}
```

When `evaluation_mode = "agent_file"`, the evaluation agent receives both the criteria list and the git diff. It scores each criterion as `pass` / `fail` / `partial` and returns a structured `EvaluationResult`. If any criterion fails, the structured feedback (which criteria failed and why) is injected into the next task attempt as additional context.

```rust
// types.rs
pub struct CriterionResult {
    pub criterion: String,
    pub status: CriterionStatus,   // Pass, Fail, Partial
    pub notes: String,
}

pub struct EvaluationResult {
    pub criteria: Vec<CriterionResult>,
    pub overall_pass: bool,
}
```

**Value**: Turns "tests failed" into "criterion 2 (retry count visible in TUI) is not met because no log line is emitted in `run_single_task` for retries." The agent can act on that precise feedback rather than guessing.

---

### Improvement 5: Session Progress Dashboard and Checkpoint File

**Inspired by**: OpenHands event stream, Gastown streaming checkpoints, SWE-Agent trajectory files.

**Problem**: `.wreck-it-state.json` is updated after each task but it only records current task statuses. There is no record of what the agent *tried* during each task, how many iterations each task consumed, or why a task was marked failed. Long sessions are opaque after the fact.

**Proposed change**: Introduce a `.wreck-it-session.jsonl` log file (newline-delimited JSON) alongside the existing state file. Every significant event is appended as a structured log entry:

```json
{"ts":"2026-02-27T10:00:00Z","event":"task_started","task_id":"3","iteration":7}
{"ts":"2026-02-27T10:01:30Z","event":"task_attempt","task_id":"3","attempt":1,"outcome":"failed","error":"test failed: test_retry_logic"}
{"ts":"2026-02-27T10:02:45Z","event":"task_attempt","task_id":"3","attempt":2,"outcome":"passed"}
{"ts":"2026-02-27T10:02:46Z","event":"task_completed","task_id":"3","iterations_used":2}
```

The TUI reads this file and displays a **session timeline** pane showing iterations, attempt counts, and outcomes — similar to Devin's session timeline. The file persists across restarts, enabling full post-session forensic analysis.

A `wreck-it report` sub-command reads the session log and prints a human-readable Markdown summary:
```
Task 3 — "Add retry logic" — COMPLETED (2 attempts, 1.5 min)
  Attempt 1: failed — test_retry_logic assertion error
  Attempt 2: passed
```

**Value**: Transforms wreck-it from a black box into an observable system. Long sessions can be audited. Recurring failure patterns can be spotted (e.g., "task 5 always fails on attempt 1 due to missing import — I should rewrite its description").

---

### Improvement 6: Action Budget and Cost Guard

**Inspired by**: SWE-Agent action budget awareness, Devin explicit escalation.

**Problem**: wreck-it's `max_iterations` limit is a blunt instrument — it limits total loop iterations, not the cost or complexity of individual tasks. A single task with an LLM agent that loops internally for 50 turns is not constrained.

**Proposed change**: Add a **per-task token/action budget**:

- Each task records the number of LLM API calls and (estimated) prompt tokens consumed.
- If a task exceeds `max_tokens_per_task` (configurable, default 100 000), the current agent run is cancelled, the task is marked for Tier 2 retry (re-planner), and a warning is logged.
- The TUI shows a real-time token counter per task and a session total.
- A `--dry-run` mode estimates token consumption for the task list before executing, based on repo map size and task description length.

```toml
[limits]
max_tokens_per_task = 100000
max_tokens_per_session = 2000000
warn_at_percent = 80   # log a warning when 80% of budget is consumed
```

**Value**: Prevents runaway cost from tasks where the agent gets stuck in an internal correction loop. Gives users visibility into where session budget is being spent. Pairs with the tiered retry strategy (Improvement 1) to ensure re-plans are triggered *before* the budget is exhausted.

---

## 7. Summary Table

| # | Improvement | Inspired by | Effort | Impact | Primary benefit |
|---|---|---|---|---|---|
| 1 | Tiered self-healing retry strategy | Devin, Gastown, Openclaw | Low–Medium | High | Fewer human interventions on transient failures |
| 2 | Externalised agent scratchpad | Devin, Aider | Low | High | Persistent episodic memory across iterations |
| 3 | Repo map context injection | Aider, Cursor | Low–Medium | Medium–High | Fewer exploratory tool calls; better targeting |
| 4 | Structured evaluation rubric | Openclaw, Devin | Medium | High | Actionable feedback instead of binary pass/fail |
| 5 | Session progress dashboard + JSONL log | OpenHands, SWE-Agent | Medium | Medium–High | Full session observability and forensics |
| 6 | Action budget and cost guard | SWE-Agent, Devin | Low–Medium | Medium | Cost control; prevents runaway agent loops |

---

## 8. References

- Devin (Cognition AI): https://www.cognition.ai/blog/introducing-devin — 2024.
- SWE-Agent (Princeton NLP): https://github.com/SWE-agent/SWE-agent — Yang et al., 2024. https://arxiv.org/abs/2405.15232
- OpenHands (formerly OpenDevin): https://github.com/All-Hands-AI/OpenHands — Wang et al., 2024. https://arxiv.org/abs/2407.16741
- Aider: https://github.com/Aider-AI/aider — Paul Gauthier, 2023–2026.
- Cursor: https://www.cursor.com — Anysphere Inc., 2023–2026.
- Gastown: Cloud-native agent orchestration runtime — durable execution, heartbeat watchdog, streaming checkpoints, workflow versioning.
- Openclaw: Interpretable multi-agent planning — re-plan triggers, counterfactual explainer, provenance diffing, policy-based escalation.
- SWE-bench (Chen et al., 2024): https://arxiv.org/abs/2310.06770 — benchmark for evaluating software engineering agents on real GitHub issues.
