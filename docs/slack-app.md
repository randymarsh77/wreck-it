# Slack Integration

wreck-it's Slack app turns linked channels into a triage surface:

- **Outbound** — triage items (CI failures, callouts, high/critical security
  findings) are announced in linked channels; every later status change
  threads onto the item's announcement, so one incident = one thread.
- **Inbound** — mention `@wreck-it` in a linked channel to query status or
  file work. Anything that isn't a `status`/`help` command becomes a triage
  item and a dispatched fix issue, with the reply (and all later lifecycle
  updates) landing in the thread where you asked.

## Creating the Slack app

Create the app at <https://api.slack.com/apps> → *From an app manifest*,
substituting your worker's hostname:

```yaml
display_information:
  name: wreck-it
  description: Autonomous triage for your codebase
  background_color: "#1a1a2e"
features:
  bot_user:
    display_name: wreck-it
    always_online: true
oauth_config:
  redirect_urls:
    - https://<your-worker-host>/slack/oauth/callback
  scopes:
    bot:
      - app_mentions:read
      - chat:write
      - channels:read
      - channels:join
      - channels:history
settings:
  event_subscriptions:
    request_url: https://<your-worker-host>/slack/events
    bot_events:
      - app_mention
  interactivity:
    is_enabled: false
  org_deploy_enabled: false
  socket_mode_enabled: false
```

Slack verifies the events URL with a `url_verification` challenge — the
worker answers it automatically once the signing secret is configured.

## Worker configuration

```bash
cd worker
wrangler secret put SLACK_CLIENT_ID       # Basic Information → App Credentials
wrangler secret put SLACK_CLIENT_SECRET
wrangler secret put SLACK_SIGNING_SECRET
```

`PORTAL_SESSION_SECRET` must also be set — it signs the OAuth `state`.

## Connecting a workspace and linking channels

1. In the portal, open a repository's **Repo Config** page → **Slack** panel
   → *Connect a Slack workspace*. This opens Slack's consent screen; on
   approval the worker stores the workspace's bot token in KV.
2. Back in the panel, pick the workspace and a public channel and click
   *Link channel*. The worker resolves the repo's GitHub App installation,
   joins the channel, and stores the link.

A channel can serve multiple repositories (each repo links it separately).
Per-link flags control what gets announced: `notify_triage` (CI failures,
log events, callouts), `notify_security` (high/critical findings only),
`notify_pr` (reserved).

## Using it

In a linked channel:

- `@wreck-it status` — open triage items for the linked repo.
- `@wreck-it help` — usage.
- `@wreck-it the login flow 500s after deploy` — files a triage item and a
  fix issue (assigned to a cloud coding agent) and replies in-thread.
  Mentioning it inside an existing thread attaches up to 20 messages of
  thread context to the issue. Repeated mentions in the same thread update
  the existing item instead of duplicating.

### Security notes

- Every request to `/slack/events` is verified against the signing secret
  (v0 HMAC, ±5-minute replay guard).
- Slack message text is untrusted input. It is embedded in issues fenced and
  explicitly labeled as an untrusted report, which also prevents GitHub
  `@`-mentions inside it from pinging users.
- Bot tokens are stored in Cloudflare KV with the same trust posture as
  portal GitHub tokens. Encryption-at-rest for both is tracked as follow-up
  work.

### v1 limitations

- Public channels only (no `groups:*` scopes).
- No slash commands, shortcuts, or interactivity — mentions cover the flows.
- One announcement thread per item (the first eligible linked channel wins
  when several are linked).
