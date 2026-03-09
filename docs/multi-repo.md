# Multi-Repository Orchestration

> **Task**: `impl-multi-repo-orchestration`
> **Status**: Implemented

---

## Overview

Multi-repository orchestration allows a single `wreck-it run` invocation to
coordinate tasks that span **multiple local git repositories**.  Typical
scenarios include:

- A monorepo that was later split into separate repos (e.g. `backend/`,
  `frontend/`, `infra/`).
- A full-stack project where the API and the web client live in separate
  repositories that must be updated together.
- A library + consumer pair where a change in the library requires a matching
  change in its consumer.

The design is deliberately incremental: the existing single-repo workflow
remains unchanged; multi-repo behaviour is opt-in through a new configuration
field.

---

## 1. Configuration — `work_dirs`

The optional `work_dirs` field in the top-level `Config` struct (in
`cli/src/types.rs`) maps a task id or role name to a local path:

```toml
# .wreck-it/run-config.toml  (or passed via --config)
[work_dirs]
frontend = "/home/user/projects/frontend"
backend  = "/home/user/projects/backend"
shared   = "../shared-lib"          # relative paths are resolved against work_dir
```

```rust
// cli/src/types.rs
use std::collections::HashMap;

pub struct Config {
    // ... existing fields ...

    /// Optional per-task or per-role working directory overrides.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub work_dirs: HashMap<String, String>,
}
```

**Key semantics**:

- Keys are arbitrary strings that match a task's `id` or `role` field.  The
  lookup order is: exact `id` match → `role` match → fall back to the
  top-level `work_dir`.
- Values are filesystem paths (absolute or relative to the current `work_dir`).
  They must point to a local git repository root.
- The field is optional and defaults to an empty map, preserving full
  backward compatibility.
- Serialisation uses `skip_serializing_if = "HashMap::is_empty"` so existing
  config files are unaffected.

---

## 2. CLI Flag — `--work-dir-map`

The `wreck-it run` command accepts `--work-dir-map ROLE_OR_ID=PATH` (may be
repeated) to populate `work_dirs` from the command line:

```sh
wreck-it run \
  --ralph feature-dev \
  --work-dir-map frontend=/home/user/my-frontend \
  --work-dir-map backend=/home/user/my-backend
```

Entries that do not match the `KEY=PATH` format are ignored with a warning.

---

## 3. Work-Directory Resolution

Before dispatching each task the loop calls `resolve_work_dir(task)` which
follows this algorithm (implemented in `cli/src/ralph_loop.rs`):

```
fn resolve_work_dir(config: &Config, task: &Task) -> PathBuf {
    // 1. Exact task-id match
    if let Some(p) = config.work_dirs.get(&task.id) { return resolve(p); }
    // 2. Role match (task.role serialised as a lowercase string)
    if let Some(role) = role_string(&task.role) {
        if let Some(p) = config.work_dirs.get(&role) { return resolve(p); }
    }
    // 3. Default
    config.work_dir.clone()
}
```

Relative paths are resolved relative to `config.work_dir`.

The resolved path is applied to the `AgentClient` via `set_work_dir()` before
the agent runs, so all git operations (context gathering, committing) happen
inside the correct repository.  Parallel tasks each receive their own
`AgentClient` constructed with the resolved path.

---

## 4. Per-Repo Test-Runner Detection

`detect_test_command(work_dir)` (in `cli/src/agent.rs`) inspects manifest
files to choose the right test command for each repository:

| Indicator file | Runner command |
|---|---|
| `Cargo.toml` | `cargo test` |
| `package.json` | `npm test` |
| `pyproject.toml` / `setup.py` / `requirements.txt` | `pytest` |
| `go.mod` | `go test ./...` |
| `Makefile` with a `test` target | `make test` |

When no manifest is recognised, `run_tests()` falls back to the legacy
try-every-command approach.

---

## 5. Example Config

```toml
# .wreck-it/run-config.toml
max_iterations = 20
work_dir = "/home/user/projects/my-api"   # primary / default repo

[work_dirs]
frontend = "/home/user/projects/my-frontend"
backend  = "/home/user/projects/my-api"
infra    = "/home/user/projects/my-infra"
```

With tasks in `tasks.json`:

```json
[
  { "id": "update-api-auth", "role": "backend",  "description": "Add OAuth2 endpoints" },
  { "id": "update-login-ui", "role": "frontend", "description": "Update login form"     },
  { "id": "update-k8s-secrets", "role": "infra", "description": "Rotate K8s secrets"   }
]
```

`wreck-it run` will execute each task in its own repository, committing there
independently.

---

## 6. Open Questions / Future Work

1. **Cross-repo artefacts**: how does the artefact store (`cli/src/artefact_store.rs`) work when producer and consumer live in different repos?  Proposal: allow absolute paths in artefact manifests.
2. **Atomic commits**: if a single logical change spans two repos, should wreck-it create linked PRs and track their merge order?
3. **Remote state per repo**: the state branch currently lives in one repo.  Consider a `state_repo` config key that can point to a dedicated state repository.
4. **`work_dir_key` in `Task`**: add an optional `work_dir_key: Option<String>` field to the `Task` type in `core/src/types.rs` to make routing explicit and avoid ambiguity between id-based and role-based lookup.
5. **Validation on startup**: when `work_dirs` is non-empty, validate that every path exists and is a git repository before the first task runs.
6. **Headless / cloud-agent path**: the headless runner (`cli/src/headless.rs`) should detect the target GitHub repository from `work_dir` via `git remote get-url origin` and open PRs against the correct repo.

---

## 7. Implementation Checklist

- [x] Add `work_dirs: HashMap<String, String>` to `Config` in `cli/src/types.rs`
- [x] Add `resolve_work_dir(task)` helper method in `cli/src/ralph_loop.rs`
- [x] Wire `resolve_work_dir` into `run_single_task` (applies `set_work_dir` to `self.agent`)
- [x] Wire `resolve_work_dir` into `run_parallel_tasks` (passes resolved path to each spawned `AgentClient`)
- [x] Add `with_work_dir` / `set_work_dir` builder/mutator to `AgentClient` in `cli/src/agent.rs`
- [x] Replace try-all test-runner probing with `detect_test_command` in `cli/src/agent.rs`
- [x] Add `--work-dir-map <ROLE_OR_ID>=<PATH>` CLI flag to `Run` subcommand in `cli/src/cli.rs`
- [x] Wire `--work-dir-map` values into `Config::work_dirs` in `cli/src/main.rs`
- [x] Update `docs/multi-repo.md` to reflect the implementation
