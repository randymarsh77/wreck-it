# Spec 006: Triage Items & CI-Failure Triage (Mendral-style Cloud Harness)

**Status:** Phase 1 implemented
**Depends on:** GitHub App worker (docs/github-app.md), spec 001 (LLM strategy)

## Motivation

Mendral.ai's hosted "AI DevOps engineer" — autonomous CI-failure diagnosis
with fix PRs, incident/log triage, supply-chain security review, and
Slack-native interaction — is winding down. wreck-it already has most of the
underlying machinery (GitHub App + worker, cloud-agent PR pipeline,
`LogSourceProvider`/`KanbanProvider` traits, security gate, cron `unstuck`).
This spec integrates the missing pieces as a coherent triage layer so that
installing the wreck-it app turns a repository into a monitored, self-healing
codebase.

## Core model: `TriageItem` (implemented)

`core/src/triage.rs` — WASM-safe, shared by CLI and worker.

- **Sources** (internally tagged enum): `ci_failure`, `log_event`,
  `slack_mention`, `security_finding`. Designed for extension.
- **Lifecycle**: `new → investigating → pr_open → resolved`, with
  `dismissed`/`stale` as manual/aged terminals.
- **Dedup**: `correlation_key()` per source (`ci:{workflow}:{branch}`,
  `log:{provider}:{event_id}`, ...). `upsert_item` collapses repeat signals
  into the open item (bumps `occurrences`, refreshes evidence) instead of
  duplicating; terminal items beyond the per-repo cap (default 200) are
  pruned oldest-first, open items never.
- **Storage**: one KV JSON document per repo (`{owner}/{repo}/triage`),
  matching the tasks-document pattern. Known read-modify-write race across
  concurrent deliveries is accepted (self-healing); the spec-001 Durable
  Object backend is the long-term fix.

## Phase 1: CI-failure triage (implemented)

- **Config**: `[triage]` in `.wreck-it/config.toml` (`enabled`,
  `auto_dispatch`, `branches` — empty = default branch only, `max_items`,
  `agent`). The state branch is always excluded. PR-branch failures are not
  triaged: the approve/auto-merge/unstuck machinery already covers them and
  double-dispatch would collide with the agent working the PR.
- **Ingestion**: the worker subscribes to `workflow_run`. Completed runs
  with `failure`/`timed_out` on a triaged branch upsert an item; `success`
  auto-resolves open items for that workflow+branch.
- **Evidence**: failed jobs (≤3) with failed step names and ~4 KB
  ANSI-stripped log tails (Actions API; 302-redirected log downloads capped
  at 2 MB, degrading to a steps-only summary). Total detail ≤8 KB.
- **Dispatch** (spec-001 "delegate, don't embed"): no in-process LLM. The
  worker opens a fix issue — labeled **`wreck-it-triage`**, deliberately not
  `wreck-it`, which would trigger a full ralph iteration — containing a
  summary table, fenced evidence, instructions, and a
  `<!-- wreck-it-triage:{correlation_key} -->` marker, then assigns a cloud
  coding agent (`suggestedActors` flow shared with the processor).
- **Resolution**: (1) trusted PR events scan the body for `#N` references to
  dispatched issues → `pr_open`; (2) merged PRs resolve linked items;
  (3) green runs resolve by workflow+branch (backstop when linkage fails).
- **Surfaces**: portal Triage page (list / evidence / dismiss / retry with
  explicit repo-access checks), `API_TOKEN` read-only mirrors, and
  `wreck-it triage list|show` (env: `WRECK_IT_API_URL`,
  `WRECK_IT_API_TOKEN`).
- **App settings** (manual): subscribe **Workflow runs**; Actions read
  access is already required. Existing installations re-approve on
  permission changes; until then events simply never arrive.

Deferred within phase 1: flaky-vs-real classification (the `occurrences` +
`run_attempt` fields already signal "possibly flaky"; a lightweight
`ModelRouter` call can be added per spec 001), staleness marking via pulse,
commenting new run links on existing issues, cross-workflow correlation by
`head_sha`.

## Phase 2: Slack app (planned)

Events API endpoint (`/slack/events`) in the worker with v0 signature
verification and 3-second ack (`ctx.wait_until` for processing); OAuth v2
install storing bot tokens in KV (`_slack/team/{team_id}`); channel↔repo
links (`_slack/link/{team_id}/{channel_id}` + reverse index) managed from
the portal. Outbound: Block Kit lifecycle notifications for triage items and
PRs, threaded per item (`TriageItem` gains an optional `slack_thread` ref).
Inbound `app_mention`: keyword commands in v1 (`status`, `help`, anything
else creates a `slack_mention` triage item + a `wreck-it`-labeled issue via
the installation token — the existing issues webhook then dispatches the
agent — and replies in-thread). Slack-origin text is wrapped in a delimited
"untrusted user report" section of issue bodies (prompt-injection guard).
Secrets: `SLACK_CLIENT_ID`, `SLACK_CLIENT_SECRET`, `SLACK_SIGNING_SECRET`.

## Phase 3: Sentry + server-side log ingestion (planned)

`core/src/log_source.rs` gains pure Sentry request builders/parsers shared
by both transports; `cli/src/log_source/sentry.rs` implements
`LogSourceProvider` (`LogSourceBackend::Sentry`, config gains
`organization`/`project`). Worker-side: pulse-driven polling per configured
repo creating `log_event` triage items (dedup by Sentry issue id); the auth
token is stored only in KV via a write-only portal endpoint, never in the
repo config. Polling over Sentry webhooks in v1 (30-min cron latency is
acceptable; per-org internal integrations are not).

## Phase 4: Supply-chain security (planned)

Dependabot alerts API (`Dependabot alerts: Read` App permission) polled from
pulse → `security_finding` triage items with advisory severity;
reconciliation resolves items whose alerts are fixed/dismissed; first-sync
capped to critical/high. Dependency-update PRs (dependabot/renovate authors,
currently ignored by the trust filter) get a dedicated observe-only branch:
triage item + one structured comment; **no** workflow approval or auto-merge
in v1. The local `security_gate` role is unchanged — complementary.

## Execution order

Phase 1 ✅ → Phase 4 (smallest; needs only an alerts client) → Phase 2 →
Phase 3.
