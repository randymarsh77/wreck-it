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

## Future Enhancements

- Parallel task execution
- Task dependencies
- Custom test commands
- Integration with CI/CD
- Task generation from issues
- Smart retry logic
