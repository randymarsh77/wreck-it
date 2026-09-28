# Autonomous response architecture

wreck-it is a webhook-driven cloud meta-harness. It coordinates official coding
CLIs from a production signal through a verified deployment.

```text
Errors / support email / deployment logs
  → Worker: authenticate and normalize
  → Durable coordinator: correlate, route, reserve
  → Cloudflare Sandbox: official CLI
  → PR → required checks → automatic merge
  → deployment → observe deployed revision
  → resolve, or bounded repair follow-up
```

The Worker is the control plane. Native Codex, Claude Code, and Copilot CLI
processes run in containers through a separate Sandbox runner. wreck-it provides
prompts, evidence, coordination, and delivery gates; the official harness owns
its agent loop.

The authoritative design is [spec 008](https://github.com/randymarsh77/wreck-it/blob/master/specs/008-autonomous-response.md), which
defines auth boundaries, usage scoring, run identity, retries, and delivery gates.
The [roadmap](roadmap.md) records the delivered implementation and its verification boundaries.

## Current implementation and migration

- `worker/src/lib.rs`, `webhook.rs`, and `github_app.rs`: existing signed GitHub
  ingress and repository-scoped GitHub App access.
- `worker/src/log_ingest.rs`, `triage.rs`, and `slack_events.rs`: existing signal
  handling to adapt into the response lifecycle.
- `response/src/worker.ts` and `engine.ts`: owner-scoped durable incidents,
  atomic admission, replay tombstones, persisted side-effect intent, and reconciliation.
- `worker/src/response.rs`: authenticated gateway, repository access checks,
  shared Rust routing, and repository-scoped GitHub App token vending.
- `core/src/routing.rs`: implemented pure owner-scoped capability/usage routing.
  An offline example is available; it does not launch sessions or query quotas.
- `response/src/session.ts` and `response/runner/`: durable sandbox sessions,
  official CLI adapters, owner-managed native Codex execution, signed source adapters,
  and continuous quota reporting.
- `response/src/github.ts`: deterministic PR publication, current-head merge gates,
  and exact-revision deployment correlation.
- Portal Autonomous responses page: policy/account configuration, observed quota,
  route reasons, lifecycle history, links, drafts, and cancellation.

The old CLI/TUI and GitHub issue-assignment paths remain functional during
migration. Legacy triage can currently resolve on merge or green CI; the new
response lifecycle resolves only on verified deployment. Do not present
legacy behavior as end-to-end autonomous response.

The [legacy architecture](legacy-architecture.md) documents existing Ralph-loop
internals for maintenance. It is not the design target for new response work.

The response service uses TypeScript Durable Objects to share the Sandbox SDK runtime;
the Rust Worker retains authentication, GitHub token vending, and the Rust route policy.
See the [runtime operations guide](https://github.com/randymarsh77/wreck-it/blob/master/response/README.md)
for provider limitations, signed adapters, provisioning, and live smoke tests.
