# Multi-Repository Orchestration

> **Task**: `ideas-multi-repo-orchestration`
> **Status**: Design stub — implementation pending

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

A new optional field is added to the top-level `Config` struct in
`cli/src/types.rs`:

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
    pub work_dirs: HashMap<String, String>,
}

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

## 2. Agent Context Assembly

When the agent runner selects the working directory for a task it follows this
resolution algorithm (to be implemented in `cli/src/agent.rs`):

```
fn resolve_work_dir(config: &Config, task: &Task) -> PathBuf {
    // 1. Exact task-id match
    if let Some(p) = config.work_dirs.get(&task.id) { return resolve(p); }
    // 2. Role match (task.role is Option<AgentRole> serialised as a string)
    if let Some(role) = &task.role {
        if let Some(p) = config.work_dirs.get(&role.to_string()) { return resolve(p); }
    }
    // 3. Default
    config.work_dir.clone()
}
```

Once the per-task `work_dir` is determined, the context assembly in
`AgentClient::read_codebase_context` should:

1. Run `git status --short` and `git log --oneline -5` **in the task's
   `work_dir`** (already parameterised through `self.work_dir`).
2. Optionally run `git diff HEAD` to surface unstaged changes in that repo.
3. Prefix the context block with the repo path so the agent knows which
   repository it is operating in.

No structural changes to `AgentClient` are required; the `work_dir` field
already drives all git commands.  The caller (main loop / `RalphLoop`) simply
needs to pass the resolved path when constructing the `AgentClient`.

---

## 3. Per-Repo Test-Runner Detection

The test-runner probing in `AgentClient::run_tests` currently tries
`cargo test`, `npm test`, and `pytest` in sequence.  For multi-repo mode the
same logic applies, but probing happens **inside the resolved `work_dir`**
for each task.

Detection heuristics (to be implemented):

| Indicator file | Runner command |
|---|---|
| `Cargo.toml` | `cargo test` |
| `package.json` | `npm test` |
| `pyproject.toml` / `setup.py` / `requirements.txt` | `pytest` |
| `go.mod` | `go test ./...` |
| `Makefile` with a `test` target | `make test` |

The probe order should prefer **manifest-file detection** over the current
try-every-command approach to avoid false positives (e.g. a repo that has
`cargo` on PATH but no `Cargo.toml`).

Proposed helper (to be added to `cli/src/agent.rs`):

```rust
fn detect_test_command(work_dir: &Path) -> Option<(&'static str, &'static [&'static str])> {
    if work_dir.join("Cargo.toml").exists() {
        return Some(("cargo", &["test"]));
    }
    if work_dir.join("package.json").exists() {
        return Some(("npm", &["test"]));
    }
    if work_dir.join("pyproject.toml").exists()
        || work_dir.join("setup.py").exists()
        || work_dir.join("requirements.txt").exists()
    {
        return Some(("pytest", &[]));
    }
    if work_dir.join("go.mod").exists() {
        return Some(("go", &["test", "./..."]));
    }
    None
}
```

---

## 4. Committing to the Correct Repository

Commits are today made by the agent (Copilot CLI subprocess) inside the
session's working directory.  In multi-repo mode the agent already operates
inside the correct repo because the `work_dir` is set per-task (see §2).

For the **headless / cloud-agent path** (`cli/src/state_worktree.rs`,
`cli/src/headless.rs`) the situation is more nuanced:

- The *state* branch lives in the **primary** repo (the one `wreck-it init`
  was run against).  This does not change.
- PRs are opened against the **per-task repo**.  The headless runner must
  be extended to detect which GitHub repository corresponds to the resolved
  `work_dir` (via `git remote get-url origin`) and call `CloudAgentClient`
  with that repo's `owner/name` instead of the primary repo.

Proposed fields on the task (future work):

```json
{
  "id": "frontend-auth",
  "role": "frontend",
  "work_dir_key": "frontend",
  ...
}
```

`work_dir_key` (a string key into `Config.work_dirs`) makes the per-task
routing explicit and avoids ambiguity between id-based and role-based lookup.

---

## 5. Open Questions / Future Work

1. **Cross-repo artefacts**: how does the artefact store (`cli/src/artefact_store.rs`) work when producer and consumer live in different repos?  Proposal: allow absolute paths in artefact manifests.
2. **Atomic commits**: if a single logical change spans two repos, should wreck-it create linked PRs and track their merge order?
3. **Remote state per repo**: the state branch currently lives in one repo.  Consider a `state_repo` config key that can point to a dedicated state repository.
4. **`work_dir_key` in `Task`**: add an optional `work_dir_key: Option<String>` field to the `Task` type in `core/src/types.rs` to make routing explicit.
5. **Validation on startup**: when `work_dirs` is non-empty, validate that every path exists and is a git repository before the first task runs.

---

## 6. Minimal Implementation Checklist

- [x] Add `work_dirs: HashMap<String, String>` to `Config` in `cli/src/types.rs`
- [ ] Add `resolve_work_dir(config, task) -> PathBuf` helper in `cli/src/agent.rs` or a new `cli/src/multi_repo.rs` module
- [ ] Wire `resolve_work_dir` into the main loop (`cli/src/ralph_loop.rs`) when constructing `AgentClient`
- [ ] Replace try-all test-runner probing with `detect_test_command` in `cli/src/agent.rs`
- [ ] Extend headless runner to resolve the target GitHub repo from `work_dir` via `git remote get-url origin`
- [ ] Add `work_dir_key: Option<String>` to `Task` in `core/src/types.rs`
- [ ] Write integration tests covering the `resolve_work_dir` and `detect_test_command` helpers
