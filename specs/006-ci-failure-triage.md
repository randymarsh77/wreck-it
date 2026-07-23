# Spec 006: Triage Items & CI-Failure Triage (Mendral-style Cloud Harness)

**Status:** Phases 1 (CI-failure triage), 4 (supply-chain), and 2 (Slack) implemented
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

## Phase 2: Slack app (implemented)

See `docs/slack-app.md` for the manifest and runbook. Modules:
`worker/src/slack.rs` (client + v0 signature verification + Block Kit),
`slack_events.rs` (`/slack/events`: challenge, retry/event-id dedup,
3-second ack with processing in `ctx.wait_until`), `slack_oauth.rs`
(HMAC-signed-state OAuth v2 install + portal channel-linking endpoints),
`slack_notify.rs` (outbound lifecycle announcements).

Key decisions vs. the original sketch:

- **Notifications are a sweep, not per-event plumbing**:
  `slack_notify::sync_and_save` replaces `save_triage` at every transition
  site — it announces any item whose status differs from
  `slack_thread.last_notified_status`, threads onto the item's announcement
  message, and persists the dedup state atomically with the items.
  Security findings announce only at high/critical to `notify_security`
  links.
- **Mention-created issues use `wreck-it-triage` + direct agent
  assignment** (not the `wreck-it` label): a ralph iteration would not have
  dispatched an arbitrary new issue; direct `assign_agent` is the proven
  triage-dispatch path.
- Mention text is fenced and framed as an untrusted report in issue bodies
  (prompt-injection guard; fencing also disarms GitHub `@`-mentions).
- KV: `_slack/team/{id}`, `_slack/link/{team}/{channel}`,
  `{owner}/{repo}/slack_links` reverse index, `_slack/teams` index (KV
  `list()` is avoided), `_slack/event/{id}` retry markers (1h TTL).
- Secrets: `SLACK_CLIENT_ID`, `SLACK_CLIENT_SECRET`, `SLACK_SIGNING_SECRET`.

Deferred to v2: private channels, slash commands/shortcuts/interactivity,
`ModelRouter` intent parsing, multi-channel announcements per item,
bot-token encryption at rest (shared question with portal sessions).

## Phase 3: Sentry + server-side log ingestion (planned)

`core/src/log_source.rs` gains pure Sentry request builders/parsers shared
by both transports; `cli/src/log_source/sentry.rs` implements
`LogSourceProvider` (`LogSourceBackend::Sentry`, config gains
`organization`/`project`). Worker-side: pulse-driven polling per configured
repo creating `log_event` triage items (dedup by Sentry issue id); the auth
token is stored only in KV via a write-only portal endpoint, never in the
repo config. Polling over Sentry webhooks in v1 (30-min cron latency is
acceptable; per-org internal integrations are not).

## Phase 4: Supply-chain security (implemented)

`worker/src/security_ingest.rs`, hooked into the pulse loop and the webhook
path; both entry points gated on `[triage].enabled`.

- **Alerts**: each pulse polls `GET /repos/{o}/{r}/dependabot/alerts?state=open`
  (`Dependabot alerts: Read` App permission; 403/404 degrades to a warning)
  and upserts `security_finding` items keyed `sec:dependabot:{number}` with
  advisory severity and GHSA/CVE/range/patched-version detail.
  Reconciliation resolves items whose alerts left the open state — skipped
  when a full 100-alert page suggests pagination, so an incomplete open set
  can never mass-resolve items. First sync ingests critical/high only
  (flood cap for legacy repos); later syncs ingest everything.
- **Dependency-update PRs**: `dependabot[bot]`/`renovate[bot]`/`renovate-bot`
  authors (one tested const list) branch to observe-only handling **before**
  the trusted-PR machinery: a `sec:dep-pr:{pr_number}` item (severity hint
  parsed from the PR body) plus one structured comment on open; merged →
  resolved, closed unmerged → dismissed. Explicitly no workflow approval or
  auto-merge; a guard test asserts these authors never pass
  `should_process_pr_event`.
- The local `security_gate` role is unchanged — complementary.

Deferred to v2: lockfile-diff review via cloud agent
(`[security] review_dependency_prs`), opt-in patch-semver auto-merge
(`[security] auto_merge_patch_updates`), `workflow_dispatch`-based scans
with a findings-callback endpoint for private-registry repos, Slack
notification of new critical/high findings (lands with phase 2).

## Execution order

Phase 1 ✅ → Phase 4 ✅ → Phase 2 (Slack) → Phase 3 (Sentry).
