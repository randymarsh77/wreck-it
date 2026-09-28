# Autonomous response meta-harness

Status: implemented and locally verified, 2026-09-27. The runtime and all ten
implementation tasks are complete; live deployment requires provider credentials,
Cloudflare Containers, and repository/deployment configuration. This supersedes the old
Ralph-loop-first roadmap, not the existing compatibility interfaces.

## Product outcome

An error report, support email, or regression detected after deployment starts
a response without someone opening a development task. wreck-it selects an
eligible model and credential-owner account, seeds an official coding harness
with evidence and a goal, coordinates its follow-ups, and tracks the resulting
change through checks, automatic merge, deployment, and production verification.

The incident is resolved only after the deployed revision passes its observation
window. A merged PR is progress, not resolution. A support trigger does not
authorize sending an email: customer communication is a separate configured
policy. The first release prepares a response draft and performs code repair.

wreck-it owns triggers, admission, routing, durable coordination, prompts,
artifacts, and delivery policy. Codex CLI, Claude Code, and Copilot CLI own their
reasoning, tools, context management, and coding loops. New response work must
not extend wreck-it's direct model API tool loop or duplicate a vendor harness.

## Flow and identity

1. Verify a source signature and map it to a configured owner/repository.
   Normalize `error_report`, `support_email`, or `deployment_regression` into a
   signal with source delivery ID, incident fingerprint, timestamp, evidence
   references, repository, and optional deployment ID/revision/environment.
   Bound and redact evidence. Treat messages and logs as untrusted task data.
2. Persist the signal before acknowledging it. Deduplicate source deliveries;
   correlate repeated signals within an incident without spawning duplicate
   sessions. Maintain a bounded attempt budget and one active repair per incident.
3. A repository rule chooses `triage`, `repair`, or `deep_repair`. Use a routed
   triage harness for ambiguous signals. Its structured assessment can request
   escalation; policy validates the class and retry budget before rerouting.
4. Filter the configured account/model pairs by credential owner, provider
   permissions, capability class, fresh usage, cooldown, and concurrency.
   Reserve an account lease atomically before provisioning a session.
5. Check out an exact repository SHA into an isolated sandbox and supply the
   selected official CLI with a versioned prompt, evidence references, desired
   result schema, verification commands, and a branch scoped to this run.
6. Capture vendor session ID, structured output, patch/commit, tests, and errors.
   Follow-ups target that session or explicitly hand off artifacts to another
   harness. Scope session IDs to owner, repository, account, and incident.
7. Publish a PR and wait for configured required checks and review policy on its
   current head SHA. Perform an atomic SHA-checked GitHub merge with branch protections intact.
   The implementation uses the REST compare-and-merge operation rather than
   enqueueing a head-agnostic auto-merge request.
8. Observe the actual merge SHA and the configured deployment pipeline. Follow
   deployment success with a finite health/log observation window for that exact
   revision and environment. Resolve only when this evidence is healthy.
9. A failed deployment or observation produces a bounded follow-up attempt.
   Rollback is allowed only through a configured deployment policy. Exhausted
   attempts, unavailable auth, or policy failures surface `needs_attention`.

## Cloud boundary

The existing Rust Worker remains the authenticated ingress, route-policy, and
GitHub-token control plane. TypeScript Durable Objects in the response service
own incident transitions, account leases, and idempotent dispatch. KV
may hold read projections, but is not the lock or authoritative run queue.
Persist a command/outbox record before each external side effect, attach an
idempotency key, and reconcile after crashes before retrying.

A separate TypeScript Worker using Cloudflare Sandbox SDK owns container
provisioning and official CLI processes. A Worker isolate cannot run a native
CLI. Start with an authenticated service binding between the Rust coordinator
and this runner; keep the process transport replaceable for another sandbox
provider. Pin SDK and CLI versions together and validate their command/output
contracts before updates.

The runner contract has `start`, `status`, `follow_up`, and `cancel` operations.
`start` accepts a run ID, attempt ID, owner, repo/SHA, harness/model, credential
reference, prompt artifact, and timeout. It returns a durable session handle.
Repeated starts with the same attempt ID must return the same handle. Completion
callbacks carry an authenticated attempt ID and monotonically increasing event
sequence. Do not trust a CLI success exit alone as proof of a tested patch.

Use per-run workspace isolation, bounded egress, timeouts, and cleanup. Keep
GitHub merge and deployment credentials in the control plane. CLI access to a
provider credential is an explicit trust boundary: code executed by that CLI
may be able to read its environment/files. Use scoped credentials or a supported
credential proxy where available; never log credentials or bake them into images.

## Accounts, authentication, and usage

The pool is a credential owner's explicitly authorized collection of accounts,
not shared subscriptions across customers. Do not rotate identities to evade a
provider limit. Provider restrictions and organization policies take precedence
over route preferences. Technical authentication support is not blanket approval
for every hosted or multi-tenant use case.

| Harness | Initial hosted auth route | Subscription treatment |
| --- | --- | --- |
| Codex CLI | API key or owner-maintained native CLI login | Official headless login/cache workflows exist; keep native auth in the owner's runner, including refresh ownership. API keys are the documented automation default. |
| Claude Code | End-user-owned API key | Hosted unmodified CLI may support native user sign-in, but wreck-it must not collect/store/broker Claude subscription tokens. Excluded from the central subscription router until a separate native-session integration is established. |
| Copilot CLI | Supported owner token with Copilot entitlement | Use documented token types and organization policy; classic PATs are unsupported. |

No custom Claude.ai login screen or extraction of subscription tokens. Do not
remove authentication choices from an unmodified hosted Claude Code binary.
Provider credentials never appear in repository configuration; `credential_ref`
is an opaque runner/secret reference resolved only after authorization.

Each usage adapter must declare which limits it can observe and provide all
applicable windows, timestamps, cooldowns, and reauthentication state. Use only
documented provider/CLI interfaces. Do not invent a common subscription quota
API or scrape private endpoints. If reliable remaining usage is unavailable,
defer usage-aware routing or use a separately configured API budget with its
own explicit spend cap; never treat unknown usage as unlimited.

The implemented policy (`core/src/routing.rs`) accepts trusted normalized
snapshots. It does not fetch quotas, store secrets, or grant leases. For every
candidate, every window must exceed the configured reserve and remain fresh.
Select the lowest remaining fraction as the limiting window; on a tie use the
later reset. Rank eligible accounts by:

`limiting remaining fraction / seconds until that window resets`

This expresses the requested preference for **more remaining usable allowance
with a nearer reset**, not a preference for already-consumed usage. Account ID
breaks ties deterministically. Capability matching happens first; model order
within a matching account is an operator preference, not a hardcoded claim
about which provider model is strongest. Fractions are a scheduling heuristic,
not interchangeable token or dollar units. Durable admission must recheck and
reserve capacity before launch, accounting for concurrent/in-flight work.

## Delivery state and gates

Persist `received → queued → running → pr_open → awaiting_checks → merging →
deploying → observing → resolved`, plus `deferred`, `needs_attention`, and
`cancelled`. Store prior state, reason, attempt, and revision on each transition.
Ignore stale callbacks and reconcile PR/deployment state after restarts.

Automatic delivery is repository opt-in: approved base branches, required
checks, review requirement, path/risk exclusions, deployment workflow,
environment, observation duration, and retry cap. Missing required checks do
not mean passing checks. Check results for an old PR head cannot authorize
merge. A successful deployment for another SHA cannot resolve this incident.
Keep existing observe-only security/dependency policies unless explicitly
reconfigured. Branch protections remain authoritative.

## Acceptance scenarios

- Deliver the same signed error event twice: one incident and one active run.
- A support email leads to a tested fix PR and a response draft, without an
  implicit outbound email send.
- A higher-capability requirement excludes an otherwise cheaper account.
- A short-window allowance with an exhausted weekly cap cannot launch.
- A passed reset requires a fresh snapshot; 429 responses respect cooldowns.
- Concurrent deliveries cannot exceed an account's reserved concurrency.
- Restart after dispatch or merge: reconcile without duplicate side effects.
- Failed/stale/missing CI blocks merge; passing checks on the current head
  permits the configured atomic SHA-checked merge.
- Merge does not close the incident; a healthy matching deployment does.
- A regression during observation starts one bounded follow-up, then escalates
  when the retry budget is exhausted.

## Implementation sequence

See [the current roadmap](../docs/roadmap.md), the completed task backlog
in `tasks/response-tasks.json`, and the [runtime guide](../response/README.md).
The routing preview is also available:

```sh
cargo run -p wreck-it-core --example route -- examples/routing.json
```

The fixture uses illustrative timestamps and model placeholders. It makes no
network calls and does not activate the cloud flow. Existing worker triage
continues on its compatibility path. Opt-in response policies use the new
service through the authenticated Worker gateway.

## Provider references

Reviewed 2026-09-27; recheck before enabling a new provider auth route.

- [Codex authentication](https://learn.chatgpt.com/docs/auth)
- [Codex non-interactive execution](https://learn.chatgpt.com/docs/non-interactive-mode)
- [Claude Code legal and credential conditions](https://code.claude.com/docs/en/legal-and-compliance)
- [Claude Code programmatic execution](https://code.claude.com/docs/en/headless)
- [Copilot CLI authentication](https://docs.github.com/en/copilot/how-tos/copilot-cli/set-up-copilot-cli/authenticate-copilot-cli)
- [Copilot CLI programmatic reference](https://docs.github.com/en/copilot/reference/copilot-cli-reference/cli-programmatic-reference)
- [Cloudflare Sandbox SDK](https://developers.cloudflare.com/sandbox/)
