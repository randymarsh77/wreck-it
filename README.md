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

### Prerequisites

1. **GitHub Copilot CLI**: Install the GitHub Copilot CLI and ensure it's available in your PATH:
   ```bash
   # Follow the GitHub Copilot CLI installation guide
   # https://docs.github.com/en/copilot/how-tos/set-up/install-copilot-cli
   ```

2. **GitHub Copilot Subscription**: A GitHub Copilot subscription is required to use the SDK. See [GitHub Copilot pricing](https://github.com/features/copilot#pricing) for details.

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

### Setup

1. **Authenticate with GitHub Copilot**:
   ```bash
   # Login to GitHub Copilot CLI
   copilot auth login
   ```

2. **Verify Copilot is working**:
   ```bash
   copilot --version
   ```

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
- `-t, --task-file <PATH>`: Path to task file (defaults from `~/.wreck-it/config.json`)
- `-m, --max-iterations <NUM>`: Maximum iterations (defaults from `~/.wreck-it/config.json`)
- `-w, --work-dir <PATH>`: Working directory (defaults from `~/.wreck-it/config.json`)
- `--model-provider <copilot|llama>`: Model provider (saved to `~/.wreck-it/config.json`)
- `--api-endpoint <URL>`: Provider endpoint (for local llama use `http://localhost:11434/v1`)

**Note**: The Copilot CLI must be authenticated and available in your PATH. The SDK will automatically connect to the Copilot CLI server.

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
