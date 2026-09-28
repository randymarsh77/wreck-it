# Autonomous response roadmap

Updated 2026-09-27. The product goal is **signal → official coding harness →
validated PR → automatic merge → deploy → verify production**. See
[spec 008](https://github.com/randymarsh77/wreck-it/blob/master/specs/008-autonomous-response.md) for the accepted design.

## Delivery sequence

| Stage | Deliverable | Status / completion gate |
| --- | --- | --- |
| 0 | Product direction and deterministic routing | Implemented; Rust routing tests and preview |
| 1 | Normalized signals and durable incidents | Implemented; signed ingress, deduplication, intent persistence, restart tests |
| 2 | Owner accounts, quota adapters, atomic leases | Implemented; shared Rust router, fresh reports, cooldowns, continuous owner-side reporting |
| 3 | Sandbox runner and official CLI adapters | Implemented; pinned image/CLIs, process deadlines, cancellation, native Codex runner |
| 4 | Error-to-PR slice | Implemented; exact-SHA checkout, verification, bounded artifacts, deterministic publication |
| 5 | Merge and deployment gates | Implemented; atomic head-checked merge and matching deployment workflow/environment |
| 6 | Production observation and follow-ups | Implemented; contiguous signed health evidence, bounded retries, escalation |
| 7 | Email/log integrations | Implemented; trusted source adapters, independent signatures, sender rules, unsent drafts |
| 8 | Portal and acceptance | Implemented; configuration, quota freshness, route reasons, history, cancellation, local runtime tests |

Implementation tasks and dependencies live in `tasks/response-tasks.json`.
The repository's `feature-dev` context now targets that backlog with a separate
`.response-state.json` runtime state file. Prior feature
tasks remain in `tasks/feature-dev-tasks.json` for reference and deliberate
migration; their statuses are not rewritten. Recurring feature discovery and
planning must work within this roadmap, not generate unrelated swarm features.

## Verification and rollout

The local acceptance suite exercises all three signal types with the real Rust
router, fake CLI executables in real Git repositories, mocked GitHub/deployment
services, and the Cloudflare Durable Object runtime. Provider logins, a live
Cloudflare container, and a production repository are not part of those tests.
Provision services and secrets using the
[runtime guide](https://github.com/randymarsh77/wreck-it/blob/master/response/README.md),
then smoke-test an isolated repository before enabling automatic merge.

## Priorities and boundaries

The end-to-end flow is implemented. Keep legacy interfaces working alongside the new run lifecycle.
Use official CLIs rather than extending direct inference/tool loops. Do not build
a cross-customer subscription broker. Treat unknown usage as unknown and leave
provider-specific auth/entitlement decisions visible.

The routing preview can be run without credentials:

```sh
cargo run -p wreck-it-core --example route -- examples/routing.json
```

The preview chooses among configured capability matches using available allowance
and time to reset. It does not reserve quota, provision a sandbox, or change code.

The [previous roadmap](legacy-roadmap.md) is retained as historical context.
