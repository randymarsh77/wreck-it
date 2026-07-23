//! Slack OAuth v2 install flow and portal endpoints for channel linking.
//!
//! Install sequence:
//!
//! 1. Portal calls `GET /api/portal/slack/install-url?owner=&repo=`
//!    (session-authed).  The worker answers with a `slack.com/oauth/v2/
//!    authorize` URL carrying an HMAC-signed `state` (owner, repo, login,
//!    expiry — signed with `PORTAL_SESSION_SECRET`, the same secret backing
//!    portal sessions).
//! 2. Slack redirects to `GET /slack/oauth/callback?code=&state=` (handled
//!    outside the Router).  The worker verifies the state, exchanges the
//!    code via `oauth.v2.access`, stores the workspace bot token in KV, and
//!    renders a small success page.
//! 3. The portal then links channels via
//!    `PUT /api/portal/repos/:owner/:repo/slack-link` (the worker joins the
//!    channel so the bot can post).
//!
//! Required scopes (see docs/slack-app.md): `app_mentions:read`,
//! `chat:write`, `channels:read`, `channels:join`, `channels:history`.

use crate::kv_store;
use crate::slack::{self, SlackChannelLink, SlackClient, SlackWorkspace};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use worker::{console_log, console_warn, Env, Request, Response, RouteContext};

type HmacSha256 = Hmac<Sha256>;

/// Bot scopes requested at install time.
pub const BOT_SCOPES: &str =
    "app_mentions:read,chat:write,channels:read,channels:join,channels:history";

/// State lifetime: 15 minutes.
const STATE_TTL_SECS: u64 = 15 * 60;

/// Signed state payload round-tripped through Slack's OAuth redirect.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct InstallState {
    pub owner: String,
    pub repo: String,
    pub login: String,
    pub exp: u64,
}

// ---------------------------------------------------------------------------
// State signing (pure, testable)
// ---------------------------------------------------------------------------

fn base64url_encode(data: &[u8]) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = u32::from_be_bytes([0, b[0], b[1], b[2]]);
        out.push(ALPHABET[((n >> 18) & 63) as usize] as char);
        out.push(ALPHABET[((n >> 12) & 63) as usize] as char);
        if chunk.len() > 1 {
            out.push(ALPHABET[((n >> 6) & 63) as usize] as char);
        }
        if chunk.len() > 2 {
            out.push(ALPHABET[(n & 63) as usize] as char);
        }
    }
    out
}

fn base64url_decode(s: &str) -> Option<Vec<u8>> {
    fn value(c: u8) -> Option<u32> {
        match c {
            b'A'..=b'Z' => Some((c - b'A') as u32),
            b'a'..=b'z' => Some((c - b'a' + 26) as u32),
            b'0'..=b'9' => Some((c - b'0' + 52) as u32),
            b'-' => Some(62),
            b'_' => Some(63),
            _ => None,
        }
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len() * 3 / 4);
    let mut accum: u32 = 0;
    let mut bits = 0u32;
    for &c in bytes {
        accum = (accum << 6) | value(c)?;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((accum >> bits) as u8);
            accum &= (1 << bits) - 1;
        }
    }
    Some(out)
}

/// Sign an install state: `base64url(json).hex(hmac)`.
pub fn sign_state(state: &InstallState, secret: &str) -> Result<String, String> {
    let payload = serde_json::to_vec(state).map_err(|e| format!("state serialize: {e}"))?;
    let encoded = base64url_encode(&payload);
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|e| format!("HMAC init failed: {e}"))?;
    mac.update(encoded.as_bytes());
    Ok(format!(
        "{encoded}.{}",
        hex::encode(mac.finalize().into_bytes())
    ))
}

/// Verify and decode an install state.  Checks the HMAC and expiry.
pub fn verify_state(state: &str, secret: &str, now: u64) -> Option<InstallState> {
    let (encoded, sig_hex) = state.split_once('.')?;
    let expected = hex::decode(sig_hex).ok()?;
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).ok()?;
    mac.update(encoded.as_bytes());
    mac.verify_slice(&expected).ok()?;

    let payload = base64url_decode(encoded)?;
    let state: InstallState = serde_json::from_slice(&payload).ok()?;
    if state.exp < now {
        return None;
    }
    Some(state)
}

/// Build the Slack authorize URL.
pub fn authorize_url(client_id: &str, redirect_uri: &str, state: &str) -> String {
    format!(
        "https://slack.com/oauth/v2/authorize?client_id={}&scope={}&redirect_uri={}&state={}",
        urlencoding::encode(client_id),
        urlencoding::encode(BOT_SCOPES),
        urlencoding::encode(redirect_uri),
        urlencoding::encode(state),
    )
}

/// Derive this worker's OAuth callback URL from an incoming request.
fn callback_url_from(req: &Request) -> worker::Result<String> {
    let url = req.url()?;
    let host = url
        .host_str()
        .ok_or_else(|| worker::Error::RustError("request has no host".into()))?;
    Ok(format!("https://{host}/slack/oauth/callback"))
}

// ---------------------------------------------------------------------------
// OAuth callback (outside the Router)
// ---------------------------------------------------------------------------

/// Handle `GET /slack/oauth/callback?code=&state=`.
pub async fn handle_callback(req: Request, env: Env) -> worker::Result<Response> {
    let url = req.url()?;
    let query = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
    };

    if let Some(error) = query("error") {
        return html_page(&format!("Slack install was not completed: {error}"), 400);
    }
    let code = match query("code") {
        Some(c) => c,
        None => return html_page("Missing 'code' parameter.", 400),
    };
    let state_raw = query("state").unwrap_or_default();

    let session_secret = env
        .secret("PORTAL_SESSION_SECRET")
        .map(|s| s.to_string())
        .map_err(|_| worker::Error::RustError("Missing PORTAL_SESSION_SECRET".into()))?;
    let state = match verify_state(&state_raw, &session_secret, crate::js_sys_now_secs()) {
        Some(s) => s,
        None => {
            return html_page(
                "Invalid or expired install link — restart from the portal.",
                400,
            )
        }
    };

    let client_id = env
        .secret("SLACK_CLIENT_ID")
        .map(|s| s.to_string())
        .map_err(|_| worker::Error::RustError("Missing SLACK_CLIENT_ID".into()))?;
    let client_secret = env
        .secret("SLACK_CLIENT_SECRET")
        .map(|s| s.to_string())
        .map_err(|_| worker::Error::RustError("Missing SLACK_CLIENT_SECRET".into()))?;

    let redirect_uri = callback_url_from(&req)?;
    let access =
        match slack::oauth_access(&client_id, &client_secret, &code, Some(&redirect_uri)).await {
            Ok(a) => a,
            Err(e) => {
                console_warn!("[wreck-it][slack] oauth exchange failed: {e}");
                return html_page("Token exchange with Slack failed — try again.", 502);
            }
        };

    let kv = env
        .kv(kv_store::KV_BINDING)
        .map_err(|e| worker::Error::RustError(format!("KV binding unavailable: {e}")))?;

    let workspace = SlackWorkspace {
        team_id: access.team_id.clone(),
        team_name: access.team_name.clone(),
        bot_token: access.bot_token,
        bot_user_id: access.bot_user_id,
        installed_by_login: state.login.clone(),
        installed_at: crate::js_sys_now_secs(),
    };
    kv_store::save_slack_workspace(&kv, &workspace)
        .await
        .map_err(worker::Error::RustError)?;
    kv_store::upsert_slack_team(&kv, &access.team_id, &access.team_name)
        .await
        .map_err(worker::Error::RustError)?;

    console_log!(
        "[wreck-it][slack] workspace '{}' ({}) installed by {} for {}/{}",
        access.team_name,
        access.team_id,
        state.login,
        state.owner,
        state.repo,
    );

    html_page(
        &format!(
            "✅ Slack workspace <b>{}</b> connected. Return to the wreck-it \
             portal and link a channel to <b>{}/{}</b> (Repo Config → Slack).",
            access.team_name, state.owner, state.repo,
        ),
        200,
    )
}

fn html_page(message: &str, status: u16) -> worker::Result<Response> {
    let html = format!(
        "<!doctype html><meta charset=\"utf-8\"><title>wreck-it × Slack</title>\
         <body style=\"font-family:system-ui;max-width:32rem;margin:4rem auto\">\
         <h2>wreck-it × Slack</h2><p>{message}</p></body>"
    );
    let mut resp = Response::from_html(html)?;
    if status != 200 {
        resp = resp.with_status(status);
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Portal endpoints (session-authed; registered from portal_api)
// ---------------------------------------------------------------------------

/// Request body for `PUT /api/portal/repos/:owner/:repo/slack-link`.
#[derive(Debug, Deserialize)]
pub struct SlackLinkRequest {
    pub team_id: String,
    pub channel_id: String,
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

/// `GET /api/portal/slack/install-url?owner=&repo=`
pub async fn install_url(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let login = match crate::portal_api::session_login(&req, &ctx).await {
        Ok(login) => login,
        Err(resp) => return Ok(resp),
    };

    let url = req.url()?;
    let query = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
    };
    let (owner, repo) = match (query("owner"), query("repo")) {
        (Some(o), Some(r)) if !o.is_empty() && !r.is_empty() => (o, r),
        _ => return crate::portal_api::error_response("owner and repo are required", 400),
    };

    let client_id = ctx
        .secret("SLACK_CLIENT_ID")
        .map(|s| s.to_string())
        .map_err(|_| worker::Error::RustError("SLACK_CLIENT_ID not configured".into()))?;
    let session_secret = ctx
        .secret("PORTAL_SESSION_SECRET")
        .map(|s| s.to_string())
        .map_err(|_| worker::Error::RustError("PORTAL_SESSION_SECRET not configured".into()))?;

    let state = InstallState {
        owner,
        repo,
        login,
        exp: crate::js_sys_now_secs() + STATE_TTL_SECS,
    };
    let signed = sign_state(&state, &session_secret).map_err(worker::Error::RustError)?;
    let redirect_uri = callback_url_from(&req)?;

    crate::portal_api::json_ok(
        &serde_json::json!({ "url": authorize_url(&client_id, &redirect_uri, &signed) }),
    )
}

/// `GET /api/portal/slack/workspaces`
pub async fn list_workspaces(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    if let Err(resp) = crate::portal_api::session_login(&req, &ctx).await {
        return Ok(resp);
    }
    let kv = ctx.kv(kv_store::KV_BINDING)?;
    match kv_store::load_slack_teams(&kv).await {
        Ok(teams) => crate::portal_api::json_ok(&teams),
        Err(e) => crate::portal_api::error_response(&e, 500),
    }
}

/// `GET /api/portal/slack/:team_id/channels`
pub async fn list_channels(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    if let Err(resp) = crate::portal_api::session_login(&req, &ctx).await {
        return Ok(resp);
    }
    let team_id = ctx.param("team_id").unwrap().clone();
    let kv = ctx.kv(kv_store::KV_BINDING)?;

    let workspace = match kv_store::load_slack_workspace(&kv, &team_id).await {
        Ok(Some(w)) => w,
        Ok(None) => return crate::portal_api::error_response("Workspace not installed", 404),
        Err(e) => return crate::portal_api::error_response(&e, 500),
    };
    match SlackClient::new(&workspace.bot_token)
        .conversations_list()
        .await
    {
        Ok(channels) => crate::portal_api::json_ok(&channels),
        Err(e) => crate::portal_api::error_response(&e, 502),
    }
}

/// `GET /api/portal/repos/:owner/:repo/slack-links`
pub async fn list_repo_links(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    if let Err(resp) =
        crate::portal_api::require_repo_access(&req, &ctx, &owner, &repo, false).await
    {
        return Ok(resp);
    }
    let kv = ctx.kv(kv_store::KV_BINDING)?;

    let refs = match kv_store::load_slack_links_for_repo(&kv, &owner, &repo).await {
        Ok(refs) => refs,
        Err(e) => return crate::portal_api::error_response(&e, 500),
    };
    let mut out = Vec::new();
    for r in refs {
        if let Ok(Some(link)) = kv_store::load_slack_link(&kv, &r.team_id, &r.channel_id).await {
            out.push(serde_json::json!({
                "team_id": r.team_id,
                "channel_id": r.channel_id,
                "notify_triage": link.notify_triage,
                "notify_pr": link.notify_pr,
                "notify_security": link.notify_security,
            }));
        }
    }
    crate::portal_api::json_ok(&out)
}

/// `PUT /api/portal/repos/:owner/:repo/slack-link`
pub async fn put_repo_link(mut req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    if let Err(resp) = crate::portal_api::require_repo_access(&req, &ctx, &owner, &repo, true).await
    {
        return Ok(resp);
    }

    let body: SlackLinkRequest = match req.json().await {
        Ok(b) => b,
        Err(e) => return crate::portal_api::error_response(&format!("Invalid JSON: {e}"), 400),
    };

    let kv = ctx.kv(kv_store::KV_BINDING)?;
    let workspace = match kv_store::load_slack_workspace(&kv, &body.team_id).await {
        Ok(Some(w)) => w,
        Ok(None) => return crate::portal_api::error_response("Workspace not installed", 404),
        Err(e) => return crate::portal_api::error_response(&e, 500),
    };

    let installation_id =
        match crate::portal_api::discover_installation_id(&ctx, &owner, &repo).await {
            Ok(id) => id,
            Err(resp) => return Ok(resp),
        };

    // Join the channel so the bot can post (public channels only in v1).
    if let Err(e) = SlackClient::new(&workspace.bot_token)
        .conversations_join(&body.channel_id)
        .await
    {
        console_warn!("[wreck-it][slack] conversations.join failed: {e}");
    }

    let link = SlackChannelLink {
        owner: owner.clone(),
        repo: repo.clone(),
        installation_id,
        notify_triage: body.notify_triage,
        notify_pr: body.notify_pr,
        notify_security: body.notify_security,
    };
    match kv_store::save_slack_link(&kv, &body.team_id, &body.channel_id, &link).await {
        Ok(()) => crate::portal_api::json_ok(&serde_json::json!({
            "team_id": body.team_id,
            "channel_id": body.channel_id,
            "linked": true,
        })),
        Err(e) => crate::portal_api::error_response(&e, 500),
    }
}

/// `DELETE /api/portal/repos/:owner/:repo/slack-link?team_id=&channel_id=`
pub async fn delete_repo_link(req: Request, ctx: RouteContext<()>) -> worker::Result<Response> {
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    if let Err(resp) = crate::portal_api::require_repo_access(&req, &ctx, &owner, &repo, true).await
    {
        return Ok(resp);
    }

    let url = req.url()?;
    let query = |name: &str| {
        url.query_pairs()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.to_string())
    };
    let (team_id, channel_id) = match (query("team_id"), query("channel_id")) {
        (Some(t), Some(c)) => (t, c),
        _ => return crate::portal_api::error_response("team_id and channel_id are required", 400),
    };

    let kv = ctx.kv(kv_store::KV_BINDING)?;
    match kv_store::delete_slack_link(&kv, &team_id, &channel_id).await {
        Ok(true) => crate::portal_api::json_ok(&serde_json::json!({ "deleted": true })),
        Ok(false) => crate::portal_api::error_response("Link not found", 404),
        Err(e) => crate::portal_api::error_response(&e, 500),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state() -> InstallState {
        InstallState {
            owner: "octo".into(),
            repo: "repo".into(),
            login: "matt".into(),
            exp: 2000,
        }
    }

    #[test]
    fn state_roundtrip() {
        let signed = sign_state(&state(), "secret").unwrap();
        let decoded = verify_state(&signed, "secret", 1000).unwrap();
        assert_eq!(decoded, state());
    }

    #[test]
    fn state_rejects_expired() {
        let signed = sign_state(&state(), "secret").unwrap();
        assert!(verify_state(&signed, "secret", 2001).is_none());
    }

    #[test]
    fn state_rejects_tampering() {
        let signed = sign_state(&state(), "secret").unwrap();
        // Flip a character in the payload half.
        let mut tampered: Vec<char> = signed.chars().collect();
        tampered[3] = if tampered[3] == 'A' { 'B' } else { 'A' };
        let tampered: String = tampered.into_iter().collect();
        assert!(verify_state(&tampered, "secret", 1000).is_none());
        // Wrong secret.
        assert!(verify_state(&signed, "other", 1000).is_none());
        // Garbage.
        assert!(verify_state("garbage", "secret", 1000).is_none());
        assert!(verify_state("a.b", "secret", 1000).is_none());
    }

    #[test]
    fn base64url_roundtrip() {
        for input in [&b""[..], b"a", b"ab", b"abc", b"hello world \xff\x00"] {
            let encoded = base64url_encode(input);
            assert!(!encoded.contains('='));
            assert_eq!(base64url_decode(&encoded).unwrap(), input);
        }
    }

    #[test]
    fn authorize_url_contains_scopes_and_state() {
        let url = authorize_url("cid", "https://w.dev/slack/oauth/callback", "st.ate");
        assert!(url.starts_with("https://slack.com/oauth/v2/authorize?"));
        assert!(url.contains("app_mentions%3Aread"));
        assert!(url.contains("state=st.ate"));
    }

    #[test]
    fn link_request_defaults() {
        let body: SlackLinkRequest =
            serde_json::from_str(r#"{"team_id":"T1","channel_id":"C1"}"#).unwrap();
        assert!(body.notify_triage && body.notify_pr && body.notify_security);
    }
}
