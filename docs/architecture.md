# Ralph Wiggum Loop Architecture

## Overview

The Ralph Wiggum Loop is a continuous execution pattern designed for AI agent workflows. Named after the Simpsons character famous for his persistence ("I'm helping!"), this pattern ensures tasks are completed through persistent iteration.

## Key Concepts

### 1. External Loop Pattern
Unlike internal AI chat loops, this is an **external bash-style loop**:
```
while true:
    if all_tasks_complete():
        break
    execute_next_task()
    run_tests()
    commit_changes()
    if max_iterations_reached():
        break
```

### 2. Filesystem as Memory
- The codebase itself serves as persistent memory
- Task status is saved to `tasks.json` after each iteration
- Git commits provide a history of changes
- No reliance on chat history or session state

### 3. Task Workflow
Each iteration follows this sequence:
1. Load tasks from `tasks.json`
2. Find next pending task
3. Execute task via Copilot SDK (or agent)
4. Run tests to verify changes
5. Commit successful changes to git
6. Update task status
7. Save tasks back to file

### 4. Safety Mechanisms
- **Max Iterations**: Prevents infinite loops and cost overruns
- **Test Validation**: Only commits changes that pass tests
- **Status Tracking**: Failed tasks are marked and can be reviewed
- **Git History**: Every change is tracked and reversible

## Architecture

### Components

#### 1. `RalphLoop`
Core loop implementation that:
- Manages iteration state
- Loads/saves task state
- Orchestrates agent execution
- Controls loop lifecycle

#### 2. `AgentClient`
Interface to the Copilot SDK:
- Executes individual tasks
- Reads codebase context
- Runs tests
- Commits changes

#### 3. `TaskManager`
Handles task persistence:
- Loads tasks from JSON
- Saves task state
- Finds next pending task

#### 4. `TuiApp`
Terminal UI for monitoring:
- Shows current iteration
- Displays task status
- Shows real-time logs
- Allows pause/resume

### Data Flow

```
┌─────────────┐
│ tasks.json  │
└──────┬──────┘
       │
       ▼
┌──────────────────┐
│   RalphLoop      │
│  - iteration     │
│  - state         │
└──────┬───────────┘
       │
       ▼
┌──────────────────┐
│  AgentClient     │◄────── GitHub Models / Copilot SDK
│  - execute_task  │
│  - run_tests     │
│  - commit        │
└──────┬───────────┘
       │
       ▼
┌──────────────────┐
│   Codebase       │
│   (git repo)     │
└──────────────────┘
```

## Usage Patterns

### Simple Task List
```json
[
  {
    "id": "1",
    "description": "Add user authentication",
    "status": "pending"
  },
  {
    "id": "2",
    "description": "Add tests for authentication",
    "status": "pending"
  }
]
```

### Multi-step Engineering
1. Create a comprehensive task list
2. Set appropriate max_iterations
3. Start the loop
4. Monitor progress in TUI
5. Review commits as tasks complete

### Recovery from Failures
- Failed tasks remain in the list
- Review the error logs
- Adjust task description if needed
- Restart the loop

## Configuration

### Environment Variables
- `GITHUB_TOKEN`: GitHub token for GitHub Models API access (recommended)
- `COPILOT_API_TOKEN`: GitHub Copilot API token (when using `--model-provider copilot`)

### Command Line Options
- `--task-file`: Path to task JSON (default: tasks.json)
- `--max-iterations`: Safety limit (default: 100)
- `--work-dir`: Repository directory (default: .)
- `--api-endpoint`: API endpoint
- `--api-token`: API token (or set `COPILOT_API_TOKEN` env var)
- `--model-provider`: `github-models`, `copilot`, or `llama`
- `--verify-command`: Custom verification command
- `--evaluation-mode`: `command` or `agent-file`
- `--headless`: Run without TUI for CI environments
- `--reflection-rounds`: Critic-actor rounds (default: 2, 0 to disable)
- `--replan-threshold`: Failures before re-planning (default: 2, 0 to disable)
- `--ralph`: Named ralph context from `.wreck-it/config.toml`
- `--goal`: Generate tasks from a natural-language goal before starting

## Best Practices

1. **Start Small**: Test with 2-3 simple tasks first
2. **Clear Descriptions**: Make task descriptions specific and actionable
3. **Set Realistic Limits**: Use max_iterations based on task complexity
4. **Monitor Progress**: Watch the TUI for unexpected behavior
5. **Review Commits**: Check git history regularly
6. **Incremental Tasks**: Break large features into smaller tasks

## Limitations

- Requires well-defined tasks
- Best for tasks with clear success criteria
- Testing must be automated
- Works within single repository
- Respects max iterations limit

## Agent Swarm Capabilities

The following features extend the base Ralph Wiggum Loop into a full agent
swarm orchestrator.  All features work together and are exercised by the
end-to-end integration test in `cli/src/integration_eval.rs`.

### Role-Based Routing

Each task carries an `AgentRole` field that determines which type of agent
should handle it:

| Role | Purpose |
|------|---------|
| `ideas` | Research, explore, and generate follow-up tasks |
| `implementer` (default) | Write code and make changes |
| `evaluator` | Review and validate completed work |

Tasks without a `role` field default to `implementer` for backward
compatibility.  The `filter_tasks_by_role` helper routes tasks to the
appropriate agent pool.

### Dynamic Task Generation

An `ideas` (or any) agent can append new tasks to the task file at runtime:

- `generate_task_id(tasks, prefix)` – produces a unique `<prefix>N` ID.
- `append_task(path, task)` – validates and appends a task, enforcing:
  - Duplicate-ID rejection.
  - Circular-dependency detection (DFS).
  - A safety cap of `MAX_TASKS` (500) to prevent runaway generation.

### Intelligent Scheduling (`TaskScheduler`)

`TaskScheduler::schedule` replaces the simple first-pending scan with a
multi-factor scoring algorithm.  Tasks are ordered from highest to lowest
score before each iteration:

| Factor | Effect |
|--------|--------|
| `priority` (×10) | Higher-priority tasks run sooner |
| `complexity` (×2, inverted) | Simpler tasks are preferred (quick wins) |
| Dependency fan-out (×5) | Tasks that unblock more work run first |
| `failed_attempts` (×3, penalty) | Repeatedly-failing tasks back off |
| Time since last attempt (≤60 pts) | Idle tasks avoid starvation |

Only tasks whose `depends_on` list is fully satisfied (all dependencies in
`Completed` status) are eligible.

### Agent Memory Persistence

`HeadlessState.memory` is a free-form string log that grows across cron
invocations.  Each phase handler appends a line describing what happened
(task triggered, PR created, merge result, etc.).  Because `HeadlessState`
is serialised to `.wreck-it-state.json` after every run, subsequent
invocations start with full knowledge of previous actions.

### Headless / Cloud-Agent Mode

In CI environments the loop does not run a local AI model.  Instead it
drives a cloud coding-agent state machine:

```
NeedsTrigger → create GitHub issue → assign Copilot
AgentWorking → poll for linked PR
NeedsVerification → merge PR when checks pass
Completed → mark task done, advance to next
```

State is persisted between cron invocations so the machine resumes
correctly after each scheduled run.

For webhook-driven operation via a GitHub App (real-time event processing
as an alternative or complement to cron), see the
[GitHub App Integration](github-app.md) guide.

### Parallel Task Execution

When `TaskScheduler::schedule` returns more than one ready task, the loop
spawns a separate `AgentClient` per task and executes them concurrently via
`tokio::spawn`.  Results are merged back into the shared `LoopState` once
all handles complete.

### Task Lifecycle Kinds

Each task has a `kind` field that controls its lifecycle after completion:

| Kind | Behaviour |
|------|-----------|
| `milestone` (default) | Completes permanently. Standard one-shot work. |
| `recurring` | Resets to `pending` after completion, subject to an optional `cooldown_seconds` delay. |

Recurring tasks are ideal for long-running goals that periodically need
fresh work — for example keeping documentation up-to-date or maintaining a
test coverage threshold.  Before each scheduling pass the headless runner
calls `reset_recurring_tasks`, which moves any completed recurring task
whose cooldown has elapsed back to `pending`.

Example task file mixing both kinds, with an agent-evaluated precondition
on the docs task so it only runs when source files have changed:

```json
[
  {
    "id": "docs",
    "description": "Review project structure and update documentation",
    "status": "pending",
    "kind": "recurring",
    "cooldown_seconds": 86400,
    "precondition_prompt": "Check if any source files have been modified since the last documentation update."
  },
  {
    "id": "coverage",
    "description": "Review test coverage. If below 90%, create and execute a plan to increase it",
    "status": "pending",
    "kind": "recurring",
    "cooldown_seconds": 604800
  },
  {
    "id": "auth",
    "description": "Implement OAuth2 authentication",
    "status": "pending"
  }
]
```

### Agent-Evaluated Preconditions

While `cooldown_seconds` provides a simple timer-based gate for recurring
tasks, many real-world workflows need more nuanced checks.  The
`precondition_prompt` field lets an **evaluation agent** decide whether a
task should run in a given iteration.

When a task has a `precondition_prompt`, the ralph loop spawns a dedicated
precondition-evaluation agent before execution.  The agent receives the
task description, the precondition criteria, and access to the working
directory.  If the agent determines the precondition is satisfied it writes
a marker file (`.task-precondition-met`); the loop checks for this file
and only proceeds when it is present.  If the precondition is not met the
task is skipped and remains in `pending` status for the next iteration.

This is a key building block for **powerful ralph loops**: combined with
recurring tasks and the intelligent scheduler, agent-evaluated
preconditions allow you to build sophisticated, self-regulating automation
that responds to the actual state of your codebase rather than just a
clock.

#### Examples

A recurring documentation task that only runs when source files have
actually changed:

```json
{
  "id": "docs",
  "description": "Review project structure and update documentation",
  "status": "pending",
  "kind": "recurring",
  "cooldown_seconds": 86400,
  "precondition_prompt": "Check if any .rs source files have been modified since the last documentation update. Only proceed if the documentation may be stale."
}
```

A test-coverage guardian that only activates when coverage drops:

```json
{
  "id": "coverage",
  "description": "Add tests to bring coverage back above 90%",
  "status": "pending",
  "kind": "recurring",
  "cooldown_seconds": 604800,
  "precondition_prompt": "Run the test suite and check if code coverage is below 90%. Only proceed if coverage is insufficient."
}
```

**Implementation**: `cli/src/types.rs` — `Task.precondition_prompt`,
`cli/src/agent.rs` — `AgentClient::evaluate_precondition`,
`cli/src/ralph_loop.rs` — precondition gate in `run_single_task` /
`run_parallel_tasks`.

---

### Per-Task Agent Memory

Each task accumulates a persistent memory log across iterations.  After every
execution attempt, wreck-it appends an entry (outcome + short summary) to a
Markdown file at `.wreck-it-memory/{task_id}.md`.  Before the agent is invoked
for the next attempt, the memory file is loaded and prepended to the prompt,
giving the agent full knowledge of prior outcomes.

```
.wreck-it-memory/
├── auth-impl.md     # "Attempt 1 - Failure: missing import…"
└── auth-tests.md    # "Attempt 1 - Success: all tests pass"
```

This prevents the agent from repeating the same mistakes and enables
incremental progress across many cron invocations.

**Implementation**: `cli/src/agent_memory.rs` — `AgentMemory`,
`load_context`, `record_attempt`.

---

### Epics, Sub-tasks, and the Project Management API

Tasks support a lightweight **epic / sub-task** hierarchy via two optional
fields on `Task`:

| Field | Purpose |
|-------|---------|
| `parent_id` | ID of the parent (epic) task.  When set, this task is a sub-task. |
| `labels` | Free-form strings for categorization (board columns, tags, etc.). |

A task with no `parent_id` that has other tasks pointing to it via `parent_id`
is treated as an **epic**.  The scheduler is unaffected — all tasks are eligible
regardless of hierarchy.  The hierarchy is primarily a data-organization
feature consumed by external tools and the FFI layer.

The `ProjectManager` (`cli/src/project_api.rs`) exposes a high-level CRUD API
on top of the task file:

```
list_tasks / get_task
list_epics / list_sub_tasks / epic_progress
create_task / create_sub_task / update_task / delete_task / move_task
```

A C-compatible **FFI layer** (`cli/src/ffi.rs`) wraps `ProjectManager` as
`extern "C"` functions for consumption from Swift or any other C-ABI consumer.
All data is exchanged as JSON-encoded C strings.  Callers must free returned
strings with `wreck_it_free_string`.

**Implementation**: `cli/src/project_api.rs` — `ProjectManager`, `TaskUpdate`.
`cli/src/ffi.rs` — `wreck_it_list_tasks`, `wreck_it_create_task`, etc.

---

### Parallel Persistent Ralph Loops (Multi-Ralph)

A repository can define multiple independent ralph contexts in
`.wreck-it/config.toml` using `[[ralphs]]` table arrays.  Each ralph has
its own task file and state file so loops are fully isolated.

```toml
state_branch = "wreck-it-state"
state_root   = ".wreck-it"

[[ralphs]]
name       = "docs"
task_file  = "docs-tasks.json"
state_file = ".docs-state.json"

[[ralphs]]
name       = "coverage"
task_file  = "coverage-tasks.json"
state_file = ".coverage-state.json"
```

Select which ralph to run via the `--ralph` CLI flag:

```bash
wreck-it run --headless --ralph docs
wreck-it run --headless --ralph coverage

# Run every ralph defined in config.toml sequentially (headless only)
wreck-it run --headless --ralph all
```

Each ralph can be driven by a separate GitHub Actions workflow so they run
on independent schedules:

```yaml
# .github/workflows/ralph-docs.yml
- run: ./target/release/wreck-it run --headless --ralph docs

# .github/workflows/ralph-coverage.yml
- run: ./target/release/wreck-it run --headless --ralph coverage
```

When no `--ralph` flag is provided, wreck-it falls back to the default
single-ralph behaviour (task file and state file come from the headless
config or CLI flags).

#### Single-Workflow Alternative

Multi-ralph is not required.  A single workflow with a single task file
that contains both `milestone` and `recurring` tasks is often sufficient.
The scheduler handles both kinds transparently, and recurring tasks
automatically reset after their cooldown elapses.

### LLM-Powered Dynamic Task Planning (`wreck-it plan`)

The `wreck-it plan --goal "..."` sub-command (and the optional `--goal` flag
for `wreck-it run`) converts a natural-language goal into a structured
`tasks.json` via the configured LLM.

```bash
wreck-it plan --goal "Build a REST API with authentication" --output tasks.json
```

The planner prompt instructs the model to emit a JSON array of tasks with
`id`, `description`, `phase`, and optional `depends_on` fields.  The output
is validated (no empty IDs, no duplicate IDs, phase ≥ 1) before being written
to disk.

**Implementation**: `cli/src/planner.rs` — `TaskPlanner`, `parse_and_validate_plan`.

---

### Critic-Actor Reflection Loop

After the actor agent completes a task and before tests run, a lightweight
**critic** prompt reads the git diff and evaluates it against the original
task description.  The critic returns a structured
`CriticResult { score, issues, approved }`.  If not approved, the actor is
re-invoked with the critic's issues as additional context (up to
`reflection_rounds`, default 2).

```toml
reflection_rounds = 2   # 0 disables reflection
```

**Implementation**: `cli/src/agent.rs` — `CriticResult`, reflection loop in the
`AgentClient` execution path.

---

### Adaptive Re-Planning on Failure

After `replan_threshold` consecutive task failures (default 2), wreck-it
invokes a **re-planner** agent that receives: the original task list, the
failed task, the error output, and the current git status.  The re-planner
may: (a) rewrite the failed task description, (b) split it into smaller
sub-tasks, or (c) inject a prerequisite task.  The modified task list is
persisted and the loop continues.

```toml
replan_threshold = 2   # 0 disables re-planning
```

**Validation guards**: duplicate IDs, circular dependencies, and completed
tasks that must not be rolled back.

**Implementation**: `cli/src/replanner.rs` — `TaskReplanner`, `parse_and_validate_replan`,
`build_replan_prompt`.

---

### Plan Migration (Cloud Agent Replanning)

Cloud agents (e.g. Copilot coding agent) work on the main branch and
cannot modify the state branch directly.  To support agent-driven
replanning, agents write new or revised task plans as JSON files in
`.wreck-it/plans/` on the main branch.  At the start of each headless
iteration the runner migrates these plans into the state branch:

```
agent PR merges → .wreck-it/plans/feature-dev-tasks.json--batch-01.json lands on main
                          ↓
headless cron fires → migrate_pending_plans() reads plans dir
                          ↓
              merge into state branch task file, remove consumed files
                          ↓
              normal iteration loop picks up new/revised tasks
```

**Targeted routing**: Plan files use a `{target}--{label}.json` naming
convention to specify which task file they target.  For example,
`feature-dev-tasks.json--assessor.json` merges into `feature-dev-tasks.json`
on the state branch.  Plain filenames (no `--` separator) merge into the
current ralph's default task file.

**Merge rules**: New task IDs are appended; existing non-completed tasks
are replaced (allows plan updates); completed tasks are never overwritten.

**Implementation**: `cli/src/plan_migration.rs` — `migrate_pending_plans`;
`core/src/plan_migration.rs` — `merge_pending_tasks`.

---

### Typed Artefact Store / Context Chain

Tasks declare optional `inputs` (references to upstream artefacts) and
`outputs` (artefacts to persist after completion).  When a task completes
its declared outputs are read from disk and stored in
`.wreck-it-artefacts.json`.  Downstream tasks that declare `inputs` have
those artefacts injected into their agent prompt automatically.

```json
{
  "id": "design-1",
  "outputs": [{ "kind": "summary", "name": "spec", "path": "spec.md" }]
}
{
  "id": "impl-1",
  "inputs": ["design-1/spec"],
  "outputs": [{ "kind": "json", "name": "code", "path": "api.rs" }]
}
```

**Implementation**: `cli/src/artefact_store.rs` — `ArtefactManifest`,
`persist_output_artefacts`, `resolve_input_artefacts`.

---

### Gastown Cloud Runtime Integration

Tasks can declare `runtime: "gastown"` to offload execution to the gastown
cloud agent service.  wreck-it acts as a workflow DAG producer: it serialises
the task graph and submits it to the gastown orchestrator.  Gastown handles
horizontal scaling, durable checkpointing, and capability negotiation.

```json
{ "id": "heavy-task", "description": "...", "runtime": "gastown" }
```

Integration is enabled by setting both `gastown_endpoint` and `gastown_token`
in the configuration.  When either is absent, tasks fall back to local
execution.

| Integration point | Implementation |
|---|---|
| wreck-it → gastown | `GastownClient::build_dag` / `serialise_dag` |
| gastown → wreck-it | `GastownClient::apply_status_events` |

**Implementation**: `cli/src/gastown_client.rs` — `GastownClient`, `WorkflowDag`,
`DagNode`, `GastownStatusEvent`.

---

### Openclaw Provenance Tracking and Export

Every task execution is recorded as a provenance entry capturing: task ID,
agent role, model, prompt hash, git diff hash, tool calls, timestamp, and
outcome.  Records are stored in `.wreck-it-provenance/<task-id>-<ts>.json`.

```bash
# Inspect the provenance chain for a single task
wreck-it provenance --task impl-1

# Export the full run as an openclaw-compatible JSON document
wreck-it export-openclaw --output run.openclaw.json
```

The openclaw export (`OpenclawDocument`) contains the complete task graph
annotated with all provenance records and artefact links, ready to load into
the openclaw plan-graph visualiser.

**Implementation**: `cli/src/provenance.rs` — `ProvenanceRecord`,
`persist_provenance_record`, `load_provenance_records`.
`cli/src/openclaw.rs` — `OpenclawDocument`, `build_document`, `serialise_document`.

---

## End-to-End Integration

All Horizon 2–3 features are exercised together in the
`eval7_full_horizon2_horizon3_acceptance_gate` test in
`cli/src/integration_eval.rs`.  The scenario:

1. `wreck-it plan` generates a four-task "Build REST API" plan (impl-5).
2. Role-based routing assigns specialist agents to each task (impl-1).
3. Artefact chaining passes the design spec into the implementation task (impl-8).
4. Provenance records are written for every completed step (impl-10).
5. The `review` task fails twice — adaptive re-planning splits it into two
   smaller tasks (impl-7).
6. Gastown DAG serialisation is verified for the review tasks (impl-9).
7. The full run is exported as an openclaw document (impl-10).
8. Agent memory persists across simulated cron invocations (impl-3).

## Future Enhancements

- Plugin hooks for custom role types
- Task dependency visualization in the TUI
- Interactive task editing from within the TUI
