# Spec 006: Plugin / Integration Trigger System

## Summary

Add a trigger webhook API and plugin system that lets external services fire
Ralph iterations on demand.  Any event source — CI pipelines, monitoring
alerts, chat-ops bots, third-party webhooks — can POST a signed payload to a
new `/api/trigger` endpoint, which the worker verifies, resolves to one or
more ralph contexts, and executes.

## Motivation

Today Ralph iterations are triggered by exactly two mechanisms:

1. **GitHub webhooks** — issues, pushes, PRs, workflow runs.
2. **Scheduled pulse** — recurring cron via SchedulerAgent.

Both are tightly coupled to the GitHub event model.  Users have asked to
trigger ralphs from systems that are not GitHub:

- A Slack slash-command (`/ralph run feature-dev`).
- A monitoring alert (PagerDuty, Datadog) creating a remediation task.
- A CI/CD pipeline stage in Jenkins, GitLab CI, or Buildkite.
- A custom internal tool or dashboard button.
- An IFTTT / Zapier / n8n automation.

Without a generic trigger surface these integrations require bespoke glue
code.  A first-class trigger webhook with authentication, payload validation,
and plugin-style routing removes that barrier.

## Proposed Design

### Trigger Webhook Endpoint

**`POST /api/trigger`**

Request headers:

| Header | Required | Description |
|--------|----------|-------------|
| `X-Trigger-Signature` | Yes | HMAC-SHA256 signature of the raw body, hex-encoded, prefixed with `sha256=`. Computed using the installation's trigger secret. Example: `sha256=a1b2c3d4e5f6...`. Case-insensitive prefix matching. |
| `X-Trigger-Source` | No | Human-readable source identifier (e.g. `slack`, `pagerduty`, `jenkins`). Logged for audit. |
| `Content-Type` | Yes | `application/json` |

Request body:

```json
{
  "installation_id": 12345,
  "owner": "acme",
  "repo": "widgets",
  "ralph": "feature-dev",
  "event": "custom:deploy-complete",
  "payload": {
    "environment": "staging",
    "version": "1.4.2",
    "url": "https://staging.acme.dev"
  }
}
```

| Field | Required | Description |
|-------|----------|-------------|
| `installation_id` | Yes | GitHub App installation ID. Used to look up credentials and settings. |
| `owner` | Yes | Repository owner. |
| `repo` | Yes | Repository name. |
| `ralph` | No | Ralph context name. If omitted, the trigger is routed using the trigger routing rules (see below). If provided, the named ralph is triggered directly and routing rules are bypassed. |
| `event` | Yes | Namespaced event name. Convention: `source:event-name`. |
| `payload` | No | Arbitrary JSON object forwarded to the ralph context as trigger metadata. |

Response:

```json
{
  "status": "ok",
  "iterations": [
    {
      "ralph": "feature-dev",
      "result": "TaskStarted",
      "task_id": "3"
    }
  ]
}
```

Error responses use standard HTTP status codes:

| Code | Meaning |
|------|---------|
| `400` | Malformed body, missing required fields. |
| `401` | Missing or invalid `X-Trigger-Signature`. |
| `403` | Installation does not have triggers enabled. |
| `404` | Repository or ralph context not found. |
| `429` | Rate limit exceeded for this installation. |

### Authentication & Verification

The trigger endpoint uses the same HMAC-SHA256 pattern as the existing GitHub
webhook handler, but with a **separate per-installation secret** so that
trigger callers do not need access to the GitHub webhook secret.

1. **Trigger secret provisioning** — When an installation enables triggers
   (via the portal), the worker generates a random 32-byte secret, stores it
   in KV at `_installation/{id}/trigger_secret`, and displays it once to the
   user.

2. **Signature verification** — Identical algorithm to `verify_signature` in
   `webhook.rs`: HMAC-SHA256 of the raw request body using the trigger secret,
   compared against the `X-Trigger-Signature` header.

3. **Installation validation** — After signature verification, the worker
   confirms the `installation_id` exists in the pulse registry and that
   `triggers_enabled` is `true` in `InstallationSettings`.

4. **Rate limiting** — Per-installation rate limit stored in KV with a
   sliding-window counter using 1-minute buckets.  The limit applies
   globally per installation (across all repos).  Default: 60 requests
   per minute.

### Trigger Routing

When the `ralph` field is omitted, the worker consults **trigger routing
rules** to determine which ralph context(s) to execute.

Rules are stored per-repo in `.wreck-it/triggers.toml`:

```toml
[[triggers]]
event = "custom:deploy-complete"
ralph = "post-deploy-checks"

[[triggers]]
event = "custom:alert-fired"
ralph = "incident-responder"
conditions = { severity = "critical" }

[[triggers]]
event = "custom:*"
ralph = "catch-all"
```

| Field | Required | Description |
|-------|----------|-------------|
| `event` | Yes | Event name or glob pattern to match against the incoming `event` field. |
| `ralph` | Yes | Ralph context to trigger. |
| `conditions` | No | Key-value pairs that must match top-level fields inside `payload`. Simple equality matching only (no nested path lookups, no regex). Values are compared as strings. |

Routing algorithm:

1. Fetch `.wreck-it/triggers.toml` from the repo's default branch via
   GitHub Contents API (authenticated with the installation token).
2. Find all rules where `event` matches (exact or glob) and `conditions`
   match.
3. Execute an iteration for each matched ralph context.
4. If no rules match and `ralph` was not provided, return `200` with an
   empty `iterations` array and `"matched_rules": 0` to distinguish from
   a successful routing.

### Installation Settings Extension

Extend `InstallationSettings` with:

```rust
pub struct InstallationSettings {
    // ... existing fields ...
    pub triggers_enabled: bool,      // default: false
    pub trigger_rate_limit: u32,     // requests per minute, default: 60
}
```

### Portal UI: Trigger Management

New section on the Installation settings page:

- **Enable Triggers** toggle.
- **Trigger Secret** — generated on first enable, displayed once, with a
  "Regenerate" button.
- **Trigger URL** — read-only field showing the full endpoint URL.
- **Usage examples** — copyable `curl` command and code snippets for common
  languages (Node.js, Python, Go).

### Plugin Registry (Future Extension)

The trigger routing rules file (`.wreck-it/triggers.toml`) serves as a
lightweight plugin manifest.  Future iterations can extend this to support:

- **Pre-trigger hooks** — validate or transform the incoming payload before
  routing (e.g. extract a Jira issue key from a Jira webhook payload).
- **Post-trigger hooks** — notify an external service after the iteration
  completes (e.g. post back to Slack).
- **Payload schemas** — optional JSON Schema definitions per event type for
  input validation.
- **Built-in source adapters** — first-party adapters that translate native
  webhook payloads from popular services (Slack, PagerDuty, Jira, Linear)
  into the wreck-it trigger format, so users can point those services
  directly at the trigger endpoint without writing glue code.

These extensions are out of scope for the initial implementation but the
`triggers.toml` format is designed to accommodate them.

## Security Considerations

- **Separate secret per installation** — compromise of one installation's
  trigger secret does not affect others or the GitHub webhook secret.
- **Signature required on every request** — prevents replay and tampering.
- **Rate limiting** — protects against accidental or malicious flood.
- **`triggers_enabled` defaults to `false`** — opt-in, not opt-out.
- **No shell execution** — trigger payloads are passed as structured metadata;
  they are never interpolated into commands.
- **Audit logging** — every trigger invocation is logged with source,
  event, timestamp, and result for post-incident review.

## Implementation Checklist

- [ ] Add `TriggerRequest` / `TriggerResponse` types to `types.rs`
- [ ] Add `triggers_enabled` and `trigger_rate_limit` to `InstallationSettings`
- [ ] Add trigger secret generation and KV storage helpers
- [ ] Implement `POST /api/trigger` handler in `portal_api.rs`
- [ ] Implement HMAC-SHA256 signature verification (reuse `verify_signature`)
- [ ] Implement trigger routing (parse `triggers.toml`, glob matching)
- [ ] Add rate limiting middleware for trigger endpoint
- [ ] Add portal UI: trigger settings section on Installation page
- [ ] Add portal UI: trigger secret display and regenerate
- [ ] Add worker unit tests for trigger request parsing and routing
- [ ] Add worker unit tests for signature verification with trigger secret
- [ ] Add integration test: end-to-end trigger → iteration flow
- [ ] Document trigger API in README / docs

## Open Questions

- Should trigger routing rules support more complex condition matching
  (regex, numeric comparisons) or is simple equality sufficient for v1?
- Should the trigger endpoint support batch triggers (array of events in a
  single request) for efficiency?
- Should we provide a "test trigger" button in the portal that sends a
  synthetic event to verify the integration is wired correctly?
