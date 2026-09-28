# Autonomous response runtime

This service implements the ten tasks in `tasks/response-tasks.json`. It runs
alongside the existing Rust Worker. The Rust gateway authenticates portal
requests, checks GitHub push permission, vends repository-scoped GitHub App
tokens, and executes the shared Rust routing policy. This TypeScript service
owns durable incident/account coordination and the Cloudflare Sandbox SDK.

## What runs

- Signed error, support-email, deployment-log, and health signals enter one
  durable coordinator per credential owner. Source IDs fix the repository and
  permitted capability; payloads cannot choose credentials or delivery policy.
- The coordinator deduplicates deliveries, records intent before external work,
  reserves accounts across that owner's repositories, and reconciles through
  alarms. Missing quotas defer. A lease is never recycled until cancellation is
  confirmed or the runner returns a terminal result. Sessions have hard deadlines.
- Pinned Codex, Claude Code, and Copilot executables receive prompts and argv.
  The job checks out an exact SHA, runs repository verification commands, and
  returns a bounded patch. Merge/deployment credentials never enter the sandbox.
- The control plane publishes a deterministic branch/commit and correlated PR.
  Automatic merge uses GitHub's atomic SHA-checked merge endpoint and respects
  branch protections. This implements automatic delivery rather than enqueueing
  GitHub's separate auto-merge feature, avoiding a stale-head race.
- A configured push-triggered GitHub Actions deployment workflow must publish a
  GitHub Deployment for the merged SHA/environment. A successful workflow alone
  is insufficient. Its deployment status `log_url` must reference that run.
- Signed health samples must match the deployment ID, SHA, and environment and
  cover the entire observation window without gaps. Regressions trigger bounded
  follow-ups; missing evidence or exhausted attempts require attention. No
  automatic rollback is performed. Support responses remain unsent drafts.

## Verify locally

```sh
cargo build -p wreck-it-core --example route --locked
cd response
npm ci
npm run check
```

Tests use the real Rust router, real Git repositories, fake CLI executables,
mocked GitHub/deployments, and Cloudflare's local Durable Object runtime. They do
not consume subscriptions, merge live PRs, or deploy production. The portal has
its own `npm run build` under `site/portal`; the Rust Worker uses
`cargo check --manifest-path worker/Cargo.toml --locked`.

The container image pins Sandbox SDK/image 0.12.10, Codex 0.157.1, Claude Code
2.1.283, and Copilot 1.0.88. Upgrade their command/output contracts together.

## Provision and deploy

1. Configure the existing GitHub App installation and Worker secrets. It needs
   repository contents, pull requests, checks/status read, Actions read, and
   deployments read. Use branch protections and required checks on the base branch.
2. Set `RESPONSE_INTERNAL_TOKEN` on the Rust Worker and the same value as
   `INTERNAL_TOKEN` on this service. Neither is a model credential. The services
   call one another through the configured private service bindings.
3. Set the response service's `CREDENTIALS` secret to a JSON reference registry:

   ```json
   {
     "secret:CODEX_PERSONAL": {"owner":"YOUR_GITHUB_LOGIN","value":"PROVIDER_API_KEY"},
     "secret:COPILOT_PERSONAL": {"owner":"YOUR_GITHUB_LOGIN","value":"SUPPORTED_COPILOT_TOKEN"},
     "secret:ERROR_SIGNING": {"owner":"YOUR_GITHUB_LOGIN","value":"INDEPENDENT_RANDOM_SECRET"},
     "secret:HEALTH_SIGNING": {"owner":"YOUR_GITHUB_LOGIN","value":"ANOTHER_RANDOM_SECRET"}
   }
   ```

   Enter values through Wrangler's secret prompt or a secret manager, not source
   files or shell arguments. Claude Code uses an end-user-owned API key. Copilot
   accepts documented OAuth/fine-grained tokens; classic PATs are rejected.
4. Deploy this service and the Rust Worker together. First-time mutual service
   binding setup may require provisioning placeholder services before deployment.
   The response service has `workers_dev: false`; only the Rust gateway is public.
   Cloudflare Containers billing and Docker are required to build/run the image.
5. Open a repository's **Autonomous responses** page in the portal. Save the
   account references, exact model IDs/capabilities, source keys, verification
   commands, checks, and deployment policy. Start with `enabled: false` and
   `auto_merge: false`, then enable the configured flow for a test repository.
6. The Cloudflare sandbox denies general internet access and admits only the
   configured `allowed_hosts`. Add the exact provider/package hosts your
   repository needs. Arrange equivalent controls on the native Docker network. Provider
   credentials supplied to an official CLI remain readable by code running in
   that environment. Isolation prevents cross-run access; it does not hide a
   credential from the process that must use it. Use scoped provider keys and a
   provider-supported credential proxy where available.

## Usage reporting

The runtime does not scrape undocumented subscription endpoints. For Codex,
run `node runner/codex-usage.mjs` in the owner's authenticated CLI environment.
It calls official app-server `account/rateLimits/read`. POST its output as
`{"account_id":"...","rate_limits":<output>}` to the authenticated portal
`response/codex-quota` endpoint. Include all returned buckets; missing data
remains unknown. An external owner-side scheduler can refresh this report.

The portal accepts complete owner-observed reports for providers whose remaining
allowance is available through their supported tools. Declare the report source,
observation timestamp, and every relevant quota window. It also accepts explicit
unavailable reports. Copilot and Claude do not have a fabricated generic quota
collector here. Without a fresh complete report they stay deferred. API billing
is separate from subscription allowance; this release does not infer a dollar
spend cap from token counts or request-limit headers.

Every completed/failed session invalidates its snapshot. Refresh it before the
next admission. A provider rate-limit failure adds a cooldown that fresh reports
cannot shorten. Registration changes invalidate old reports. Disabling/changing
an active account requires cancelling its sessions first.

## Sources and health evidence

Public ingress:

`POST /response/hooks/{credential-owner}/{repository-owner}/{repository}/{source-id}`

Sign the exact body using HMAC-SHA256 over `timestamp + "." + body`, with headers
`X-Wreckit-Timestamp` (Unix seconds) and `X-Wreckit-Signature: sha256=<hex>`.
Signatures expire after five minutes; delivery tombstones prevent replays within
that window. Source secrets are independent from portal/model credentials.

`runner/signal.mjs` adapts Sentry event JSON, authenticated email records,
deployment log records, and health records from stdin. Configure
`WRECKIT_SIGNAL_URL` and `WRECKIT_SIGNAL_SECRET` in the trusted upstream handler:

```sh
node runner/signal.mjs sentry < event.json
node runner/signal.mjs email < authenticated-mail.json
node runner/signal.mjs logs < deployment-error.json
node runner/signal.mjs health < observation.json
```

The adapter is not a public webhook receiver. The mail/error integration must
verify its provider's original signature before invoking it. The cloud endpoint
then verifies this adapter's signature. Normalized email records require
`message_id`, `from`, `subject`, `text` and optionally `thread_id`; configured
sender allowlists are enforced. Logs require deployment `{id,sha,environment}`.
Health records require `{sha,environment,deployment_id,healthy}` and must be
emitted repeatedly throughout the observation window. Do not send a single green
sample and assume silence means success.

## Native Codex subscriptions

For native owner-maintained authentication, use `auth: native_subscription`, a
`runner:` reference, and one concurrent session. Run
`runner/native-server.mjs` on owner-controlled compute with Docker, the built
runner image, and an existing Codex auth home. Configure:

- `WRECKIT_NATIVE_STATE`: absolute durable per-attempt workspace directory.
- `WRECKIT_NATIVE_TOKEN`: same internal service token, stored as a secret.
- `WRECKIT_NATIVE_ACCOUNTS`: JSON array of `{owner,ref,home}`; `home` is an
  absolute directory populated by the owner's official `codex login` flow,
  readable/writable by container UID 1000.
- `WRECKIT_NATIVE_IMAGE`: pinned locally built runner image.
- `WRECKIT_NATIVE_NETWORK`: Docker network enforcing the operator's egress policy.

Expose the loopback server through a private tunnel; set `NATIVE_RUNNER_URL` on
this service to that HTTPS endpoint, or bind `NATIVE_RUNNER` to a private proxy
Worker. Native auth stays on that machine and in its mounted per-owner home.
Read-only GitHub tokens are sent transiently to fetch private repository content.
No Claude subscription token can use this path.

Native Codex follow-ups can resume the recorded vendor session when the same
account is selected. Ephemeral cloud sessions are cleaned up and follow-ups use
an explicit handoff: the prior session identity, summary, regression evidence,
and newly checked-out repository state seed a fresh official harness. This is
intentional; a destroyed CLI home cannot truthfully promise native resume.

## Operations and limits

Pause a repository by setting `enabled: false`; cancel in-flight responses before
removing credentials or services. Disabling auto-merge retains PR generation and
observation state. The coordinator keeps up to 500 incidents and 10,000 receipt
tombstones per owner, failing admission rather than silently dropping replay
protection. Export/archive the owner state with an operator migration when those
bounds are approached. Terminal session metadata remains for idempotency; native
workspace retention is operator-managed. Patches are at most 50 regular UTF-8
files / 24 KB; binary files, symlinks, and submodules require attention.

Rollback the deployment of the services independently of repository code changes.
Do not clear durable storage during rollback. Live provider logins, Cloudflare
container execution, and real GitHub merge/deploy behavior must be smoke-tested
in a configured repository before production enablement; local tests exercise
these boundaries with fakes and do not establish provider authorization.

For continuous owner-side quota refresh, configure a dedicated source with
`kind: usage` and an independent signing key. Run `node runner/usage-reporter.mjs`
in the authenticated owner environment with `WRECKIT_ACCOUNT_ID`,
`WRECKIT_USAGE_URL` (the dedicated source's hook URL), and
`WRECKIT_USAGE_SECRET`. It reports Codex's official limits every 60 seconds by
default. `WRECKIT_USAGE_COMMAND` can supply an argv JSON array for another
operator-controlled, documented collector returning a normalized quota report.
Collector failures publish unavailable data, not guessed capacity. Reports
observed before a completed run cannot restore its invalidated allowance.
