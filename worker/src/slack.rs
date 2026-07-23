//! Slack Web API client, request-signature verification, and Block Kit
//! message builders.
//!
//! Uses the Cloudflare Worker Fetch API (via the `worker` crate), mirroring
//! the style of [`crate::github`].  Slack's Web API answers HTTP 200 for
//! almost everything and signals failure with `{"ok": false, "error": ...}`,
//! so every call checks the `ok` field.

use serde::{Deserialize, Serialize};
use worker::Fetch;
use wreck_it_core::triage::{TriageItem, TriageSeverity, TriageStatus};

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Maximum allowed clock skew between Slack's request timestamp and now
/// (replay-attack guard).
pub const MAX_SIGNATURE_AGE_SECS: u64 = 300;

// ---------------------------------------------------------------------------
// Request-signature verification
// ---------------------------------------------------------------------------

/// Verify a Slack request signature (v0 scheme).
///
/// `sig_header` is `X-Slack-Signature` (e.g. `"v0=abcdef..."`),
/// `ts_header` is `X-Slack-Request-Timestamp` (unix seconds as text),
/// `secret` is the app's signing secret, `body` the raw request bytes.
/// The signed base string is `"v0:{ts}:{body}"`.  Requests older (or newer)
/// than [`MAX_SIGNATURE_AGE_SECS`] are rejected.
pub fn verify_slack_signature(
    sig_header: &str,
    ts_header: &str,
    secret: &str,
    body: &[u8],
    now_secs: u64,
) -> bool {
    let hex_sig = match sig_header.strip_prefix("v0=") {
        Some(h) => h,
        None => return false,
    };
    let expected = match hex::decode(hex_sig) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let ts: u64 = match ts_header.parse() {
        Ok(t) => t,
        Err(_) => return false,
    };
    if now_secs.abs_diff(ts) > MAX_SIGNATURE_AGE_SECS {
        return false;
    }

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(format!("v0:{ts}:").as_bytes());
    mac.update(body);
    mac.verify_slice(&expected).is_ok()
}

// ---------------------------------------------------------------------------
// Domain types (stored in KV — see kv_store)
// ---------------------------------------------------------------------------

/// An installed Slack workspace (bot token vended via OAuth v2).
///
/// Stored at `_slack/team/{team_id}`.  The bot token has the same trust
/// posture as portal sessions storing GitHub tokens in KV.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlackWorkspace {
    pub team_id: String,
    pub team_name: String,
    pub bot_token: String,
    pub bot_user_id: String,
    pub installed_by_login: String,
    pub installed_at: u64,
}

/// A link from a Slack channel to a GitHub repository.
///
/// Stored at `_slack/link/{team_id}/{channel_id}`, with a reverse index at
/// `{owner}/{repo}/slack_links`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlackChannelLink {
    pub owner: String,
    pub repo: String,
    pub installation_id: u64,
    #[serde(default = "default_true")]
    pub notify_triage: bool,
    #[serde(default = "default_true")]
    pub notify_pr: bool,
    #[serde(default = "default_true")]
    pub notify_security: bool,
}

fn default_true() -> bool {
    true
}

/// Entry in a repository's reverse index of linked channels.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SlackLinkRef {
    pub team_id: String,
    pub channel_id: String,
}

/// A public channel as returned by `conversations.list`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackChannel {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub is_member: bool,
}

/// A message as returned by `conversations.replies`.
#[derive(Debug, Clone, Deserialize)]
pub struct SlackMessage {
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub text: String,
    pub ts: String,
}

/// Result of `oauth.v2.access`.
#[derive(Debug, Clone)]
pub struct OAuthAccess {
    pub team_id: String,
    pub team_name: String,
    pub bot_token: String,
    pub bot_user_id: String,
}

// ---------------------------------------------------------------------------
// Web API client
// ---------------------------------------------------------------------------

/// A lightweight Slack Web API client bound to one bot token.
pub struct SlackClient {
    bot_token: String,
}

impl SlackClient {
    pub fn new(bot_token: impl Into<String>) -> Self {
        Self {
            bot_token: bot_token.into(),
        }
    }

    /// POST a JSON body to a Slack Web API method and return the parsed
    /// response after checking `ok`.
    async fn call(
        &self,
        method: &str,
        body: &serde_json::Value,
    ) -> Result<serde_json::Value, String> {
        let url = format!("https://slack.com/api/{method}");
        let body_json =
            serde_json::to_string(body).map_err(|e| format!("Failed to serialize: {e}"))?;

        let headers = worker::Headers::new();
        headers
            .set("Authorization", &format!("Bearer {}", self.bot_token))
            .ok();
        headers
            .set("Content-Type", "application/json; charset=utf-8")
            .ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Post)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("Slack API request failed: {e}"))?;

        let parsed: serde_json::Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse Slack response: {e}"))?;

        if !parsed["ok"].as_bool().unwrap_or(false) {
            return Err(format!(
                "Slack {method} failed: {}",
                parsed["error"].as_str().unwrap_or("unknown error")
            ));
        }
        Ok(parsed)
    }

    /// `chat.postMessage`.  Returns the posted message's `ts`.
    pub async fn post_message(
        &self,
        channel: &str,
        blocks: &serde_json::Value,
        text_fallback: &str,
        thread_ts: Option<&str>,
    ) -> Result<String, String> {
        let mut body = serde_json::json!({
            "channel": channel,
            "text": text_fallback,
            "blocks": blocks,
            "unfurl_links": false,
        });
        if let Some(ts) = thread_ts {
            body["thread_ts"] = serde_json::Value::String(ts.to_string());
        }
        let response = self.call("chat.postMessage", &body).await?;
        response["ts"]
            .as_str()
            .map(|s| s.to_string())
            .ok_or_else(|| "chat.postMessage response missing ts".to_string())
    }

    /// `conversations.replies` — fetch up to `limit` messages of a thread.
    pub async fn conversations_replies(
        &self,
        channel: &str,
        ts: &str,
        limit: u32,
    ) -> Result<Vec<SlackMessage>, String> {
        // GET-style method; Slack accepts JSON POST for it as well.
        let body = serde_json::json!({ "channel": channel, "ts": ts, "limit": limit });
        let response = self.call("conversations.replies", &body).await?;
        serde_json::from_value(response["messages"].clone())
            .map_err(|e| format!("Failed to parse thread messages: {e}"))
    }

    /// `conversations.list` — public channels visible to the bot.
    pub async fn conversations_list(&self) -> Result<Vec<SlackChannel>, String> {
        let body = serde_json::json!({
            "types": "public_channel",
            "exclude_archived": true,
            "limit": 200,
        });
        let response = self.call("conversations.list", &body).await?;
        serde_json::from_value(response["channels"].clone())
            .map_err(|e| format!("Failed to parse channel list: {e}"))
    }

    /// `conversations.join` — join a public channel so the bot can post.
    pub async fn conversations_join(&self, channel: &str) -> Result<(), String> {
        self.call("conversations.join", &serde_json::json!({ "channel": channel }))
            .await
            .map(|_| ())
    }
}

/// Exchange an OAuth v2 `code` for a bot token (`oauth.v2.access`).
///
/// Unlike the other methods this is form-encoded and unauthenticated
/// (client id/secret travel in the body).
pub async fn oauth_access(
    client_id: &str,
    client_secret: &str,
    code: &str,
) -> Result<OAuthAccess, String> {
    let form = format!(
        "client_id={}&client_secret={}&code={}",
        urlencoding_encode(client_id),
        urlencoding_encode(client_secret),
        urlencoding_encode(code),
    );

    let headers = worker::Headers::new();
    headers
        .set("Content-Type", "application/x-www-form-urlencoded")
        .ok();

    let request = worker::Request::new_with_init(
        "https://slack.com/api/oauth.v2.access",
        worker::RequestInit::new()
            .with_method(worker::Method::Post)
            .with_headers(headers)
            .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&form))),
    )
    .map_err(|e| format!("Failed to create request: {e}"))?;

    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| format!("oauth.v2.access request failed: {e}"))?;

    let parsed: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse oauth response: {e}"))?;

    if !parsed["ok"].as_bool().unwrap_or(false) {
        return Err(format!(
            "oauth.v2.access failed: {}",
            parsed["error"].as_str().unwrap_or("unknown error")
        ));
    }

    Ok(OAuthAccess {
        team_id: parsed
            .pointer("/team/id")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        team_name: parsed
            .pointer("/team/name")
            .and_then(|v| v.as_str())
            .unwrap_or_default()
            .to_string(),
        bot_token: parsed["access_token"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
        bot_user_id: parsed["bot_user_id"]
            .as_str()
            .unwrap_or_default()
            .to_string(),
    })
}

/// Minimal percent-encoding for form values (mirrors the inline helper in
/// the Seq log-source backend; avoids a new dependency).
fn urlencoding_encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Block Kit builders
// ---------------------------------------------------------------------------

/// Human-readable label for a triage status.
fn status_label(status: TriageStatus) -> &'static str {
    match status {
        TriageStatus::New => "New",
        TriageStatus::Investigating => "Investigating",
        TriageStatus::PrOpen => "PR open",
        TriageStatus::Resolved => "Resolved",
        TriageStatus::Dismissed => "Dismissed",
        TriageStatus::Stale => "Stale",
    }
}

/// Emoji for a status transition.
fn status_emoji(status: TriageStatus) -> &'static str {
    match status {
        TriageStatus::New => "🚨",
        TriageStatus::Investigating => "🔍",
        TriageStatus::PrOpen => "🔀",
        TriageStatus::Resolved => "✅",
        TriageStatus::Dismissed => "🚫",
        TriageStatus::Stale => "🕸️",
    }
}

fn severity_label(severity: TriageSeverity) -> &'static str {
    match severity {
        TriageSeverity::Low => "low",
        TriageSeverity::Medium => "medium",
        TriageSeverity::High => "high",
        TriageSeverity::Critical => "critical",
    }
}

/// Build the Block Kit payload announcing a triage item's current status.
///
/// The same builder serves the initial announcement and threaded follow-ups
/// — the message states the item's status, links, and occurrence count.
/// Returns `(blocks, text_fallback)`.
pub fn triage_status_message(
    owner: &str,
    repo: &str,
    item: &TriageItem,
) -> (serde_json::Value, String) {
    let emoji = status_emoji(item.status);
    let status = status_label(item.status);
    let text_fallback = format!("{emoji} [{owner}/{repo}] {status}: {}", item.title);

    let mut context_parts: Vec<String> = vec![
        format!("severity: {}", severity_label(item.severity)),
        format!("occurrences: {}", item.occurrences),
    ];
    if let Some(issue) = item.issue_number {
        context_parts.push(format!(
            "<https://github.com/{owner}/{repo}/issues/{issue}|issue #{issue}>"
        ));
    }
    if let Some(pr) = item.pr_number {
        context_parts.push(format!(
            "<https://github.com/{owner}/{repo}/pull/{pr}|PR #{pr}>"
        ));
    }
    if let wreck_it_core::triage::TriageSource::CiFailure {
        run_url: Some(url), ..
    } = &item.source
    {
        context_parts.push(format!("<{url}|CI run>"));
    }

    let blocks = serde_json::json!([
        {
            "type": "section",
            "text": {
                "type": "mrkdwn",
                "text": format!("{emoji} *{status}* — {} (`{owner}/{repo}`)", item.title),
            }
        },
        {
            "type": "context",
            "elements": [{
                "type": "mrkdwn",
                "text": context_parts.join("  ·  "),
            }]
        }
    ]);
    (blocks, text_fallback)
}

/// Build the in-thread reply confirming a mention was turned into work.
///
/// Returns `(blocks, text_fallback)`.
pub fn mention_ack_message(
    owner: &str,
    repo: &str,
    issue_number: u64,
) -> (serde_json::Value, String) {
    let issue_url = format!("https://github.com/{owner}/{repo}/issues/{issue_number}");
    let text = format!(
        "🔧 On it — filed <{issue_url}|issue #{issue_number}> in `{owner}/{repo}` \
         and dispatched a coding agent. Updates will land in this thread."
    );
    let blocks = serde_json::json!([
        { "type": "section", "text": { "type": "mrkdwn", "text": text } }
    ]);
    (blocks, format!("Filed issue #{issue_number} in {owner}/{repo}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use wreck_it_core::triage::TriageSource;

    fn signed(secret: &str, ts: u64, body: &[u8]) -> String {
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(format!("v0:{ts}:").as_bytes());
        mac.update(body);
        format!("v0={}", hex::encode(mac.finalize().into_bytes()))
    }

    #[test]
    fn valid_signature_passes() {
        let sig = signed("secret", 1000, b"payload");
        assert!(verify_slack_signature(&sig, "1000", "secret", b"payload", 1000));
        // Within the skew window.
        assert!(verify_slack_signature(&sig, "1000", "secret", b"payload", 1299));
    }

    #[test]
    fn expired_timestamp_fails() {
        let sig = signed("secret", 1000, b"payload");
        assert!(!verify_slack_signature(
            &sig, "1000", "secret", b"payload", 1301
        ));
    }

    #[test]
    fn tampered_body_fails() {
        let sig = signed("secret", 1000, b"payload");
        assert!(!verify_slack_signature(
            &sig, "1000", "secret", b"EVIL", 1000
        ));
    }

    #[test]
    fn wrong_prefix_fails() {
        assert!(!verify_slack_signature(
            "sha256=abc",
            "1000",
            "secret",
            b"payload",
            1000
        ));
    }

    #[test]
    fn garbage_timestamp_fails() {
        let sig = signed("secret", 1000, b"payload");
        assert!(!verify_slack_signature(
            &sig,
            "not-a-number",
            "secret",
            b"payload",
            1000
        ));
    }

    #[test]
    fn form_encoding() {
        assert_eq!(urlencoding_encode("abc-123"), "abc-123");
        assert_eq!(urlencoding_encode("a b&c"), "a%20b%26c");
    }

    #[test]
    fn channel_link_defaults_all_notifications_on() {
        let json = r#"{"owner":"o","repo":"r","installation_id":1}"#;
        let link: SlackChannelLink = serde_json::from_str(json).unwrap();
        assert!(link.notify_triage && link.notify_pr && link.notify_security);
    }

    #[test]
    fn triage_status_message_shape() {
        let mut item = TriageItem::new(
            TriageSource::CiFailure {
                run_id: 1,
                workflow_name: "CI".into(),
                branch: "main".into(),
                head_sha: "abc".into(),
                conclusion: "failure".into(),
                run_url: Some("https://github.com/o/r/actions/runs/1".into()),
                run_attempt: 1,
            },
            "CI failure: CI on main".into(),
            None,
            1000,
        );
        item.issue_number = Some(7);
        item.pr_number = Some(9);

        let (blocks, fallback) = triage_status_message("o", "r", &item);
        let rendered = blocks.to_string();
        assert!(rendered.contains("CI failure: CI on main"));
        assert!(rendered.contains("issues/7"));
        assert!(rendered.contains("pull/9"));
        assert!(rendered.contains("actions/runs/1"));
        assert!(fallback.contains("[o/r]"));
    }

    #[test]
    fn mention_ack_links_issue() {
        let (blocks, fallback) = mention_ack_message("o", "r", 42);
        assert!(blocks.to_string().contains("issues/42"));
        assert!(fallback.contains("#42"));
    }
}
