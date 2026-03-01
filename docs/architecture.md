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
│  AgentClient     │◄────── GitHub Copilot SDK
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
- `COPILOT_API_TOKEN`: GitHub Copilot API token

### Command Line Options
- `--task-file`: Path to task JSON (default: tasks.json)
- `--max-iterations`: Safety limit (default: 100)
- `--work-dir`: Repository directory (default: .)
- `--api-endpoint`: Copilot API endpoint

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
end-to-end integration test in `src/integration_eval.rs`.

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

Example task file mixing both kinds:

```json
[
  {
    "id": "docs",
    "description": "Review project structure and update documentation",
    "status": "pending",
    "kind": "recurring",
    "cooldown_seconds": 86400
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

## Future Enhancements

- Custom test commands
- Integration with CI/CD webhooks
- Plugin hooks for custom role types
