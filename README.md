# wreck-it 🔧

A TUI agent harness that uses the Copilot SDK to perform Ralph Wiggum loops.

## What is a Ralph Wiggum Loop?

The Ralph Wiggum Loop is a bash-style loop that continuously executes AI agent tasks until completion:

- **External Loop**: Not an internal AI feature, but an external script running `while true`
- **Persistent Memory**: Uses the filesystem (codebase) as memory rather than chat history
- **Workflow**: Reads task file → Implements change → Runs tests → Commits code → Repeats
- **Safety**: Includes max iterations limit to prevent infinite loops and excessive costs

## Features

- 🎨 **TUI Interface**: Beautiful terminal UI showing tasks, progress, and logs
- 🔄 **Continuous Execution**: Runs until all tasks are complete or max iterations reached
- 📝 **Task Management**: JSON-based task tracking with status persistence
- 🧪 **Automatic Testing**: Runs tests after each task execution
- 💾 **Git Integration**: Automatically commits successful changes
- 🔒 **Safety Limits**: Configurable max iterations to prevent runaway costs

## Installation

### Using Nix Flakes (Recommended)

```bash
# Enter development shell
nix develop

# Or build the project
nix build
```

### Using Cargo

```bash
cargo build --release
```

## Usage

### Initialize a Task File

```bash
wreck-it init
```

This creates a sample `tasks.json` file with example tasks.

### Run the Ralph Wiggum Loop

```bash
wreck-it run
```

Options:
- `-t, --task-file <PATH>`: Path to task file (default: tasks.json)
- `-m, --max-iterations <NUM>`: Maximum iterations (default: 100)
- `-w, --work-dir <PATH>`: Working directory (default: .)
- `--api-endpoint <URL>`: Copilot API endpoint
- `--api-token <TOKEN>`: Copilot API token (or set COPILOT_API_TOKEN env var)

### TUI Controls

- **Space**: Pause/Resume the loop
- **Q**: Quit the application

## Task File Format

Tasks are defined in a JSON file:

```json
[
  {
    "id": "1",
    "description": "Implement feature X",
    "status": "pending"
  },
  {
    "id": "2",
    "description": "Add tests for feature X",
    "status": "pending"
  }
]
```

Status values: `pending`, `inprogress`, `completed`, `failed`

## Development

### Build

```bash
cargo build
```

### Test

```bash
cargo test
```

### Run Locally

```bash
cargo run -- run --task-file tasks.json
```

## CI/CD

This project includes GitHub Actions workflows for:
- Building (debug and release)
- Running tests
- Clippy linting
- Format checking

## License

MIT - See [LICENSE](LICENSE) for details
