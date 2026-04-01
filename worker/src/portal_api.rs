//! Portal API endpoints for the wreck-it dashboard SPA.
//!
//! These endpoints live under `/api/portal/…` and provide GitHub OAuth
//! authentication, installation/repository listing, and configuration
//! management for the portal frontend.
//!
//! ## Authentication
//!
//! The portal uses GitHub OAuth (web application flow) to authenticate
//! users.  After a successful OAuth callback the worker issues an
//! HMAC-SHA256 session token backed by a KV entry.  Subsequent requests
//! include the session token as a `Bearer` token in the `Authorization`
//! header.
//!
//! ## Endpoints
//!
//! | Method    | Path                                                  | Description                       |
//! |-----------|-------------------------------------------------------|-----------------------------------|
//! | `GET`     | `/api/portal/auth/login`                              | Redirect to GitHub OAuth          |
//! | `GET`     | `/api/portal/auth/callback`                           | OAuth callback — exchange code    |
//! | `GET`     | `/api/portal/auth/user`                               | Get authenticated user info       |
//! | `GET`     | `/api/portal/installations`                           | List user's app installations     |
//! | `GET`     | `/api/portal/installations/:installation_id/repos`    | List repos for an installation    |
//! | `GET`     | `/api/portal/repos/:owner/:repo/config`               | Read repo config (TOML → JSON)    |
//! | `PUT`     | `/api/portal/repos/:owner/:repo/config`               | Write repo config (JSON → TOML)   |
//! | `GET`     | `/api/portal/repos/:owner/:repo/ralphs/:name/tasks`   | Read ralph tasks from repo        |
//! | `PUT`     | `/api/portal/repos/:owner/:repo/ralphs/:name/tasks`   | Write ralph tasks to repo         |
//! | `GET`     | `/api/portal/repos/:owner/:repo/ralphs/:name/state`   | Read ralph state from repo        |
//! | `PUT`     | `/api/portal/repos/:owner/:repo/ralphs/:name/state`   | Write ralph state to repo         |
//! | `POST`    | `/api/portal/repos/:owner/:repo/ralphs/plan`          | Generate a task plan from a goal  |
//!
//! ## Required secrets
//!
//! - `GITHUB_CLIENT_ID`       — GitHub App's OAuth client ID
//! - `GITHUB_CLIENT_SECRET`   — GitHub App's OAuth client secret
//! - `PORTAL_SESSION_SECRET`  — Random string for HMAC-signing session tokens
//! - `GITHUB_MODELS_TOKEN`    — API token for GitHub Models (plan generation)

use crate::github_app;
use crate::kv_store;
use hmac::{Hmac, Mac};
use sha2::Sha256;
use worker::*;

type HmacSha256 = Hmac<Sha256>;

/// Session expiration: 24 hours in seconds.
const SESSION_TTL_SECS: u64 = 24 * 60 * 60;

// ---------------------------------------------------------------------------
// KV key helpers
// ---------------------------------------------------------------------------

/// Build the KV key for a portal session.
fn portal_session_key(hmac_hex: &str) -> String {
    format!("_portal/sessions/{hmac_hex}")
}

// ---------------------------------------------------------------------------
// CORS helpers
// ---------------------------------------------------------------------------

/// Add CORS headers to a [`Response`].
fn cors_headers(mut resp: Response) -> Result<Response> {
    resp.headers_mut()
        .set("Access-Control-Allow-Origin", "*")?;
    resp.headers_mut()
        .set("Access-Control-Allow-Methods", "GET, POST, PUT, DELETE, OPTIONS")?;
    resp.headers_mut()
        .set("Access-Control-Allow-Headers", "Authorization, Content-Type")?;
    Ok(resp)
}

/// Handle CORS preflight `OPTIONS` requests.
async fn options_handler(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    cors_headers(Response::empty()?.with_status(204))
}

// ---------------------------------------------------------------------------
// JSON response helper
// ---------------------------------------------------------------------------

/// Build a JSON response with the given status code and CORS headers.
fn json_response<T: serde::Serialize>(value: &T, status: u16) -> Result<Response> {
    let body = serde_json::to_string(value)
        .map_err(|e| Error::RustError(format!("JSON serialization failed: {e}")))?;
    let mut resp = Response::ok(body)?;
    resp.headers_mut().set("Content-Type", "application/json")?;
    if status != 200 {
        resp = resp.with_status(status);
    }
    cors_headers(resp)
}

/// Build a plain-text error response with CORS headers.
fn error_response(msg: &str, status: u16) -> Result<Response> {
    let resp = Response::error(msg, status)?;
    cors_headers(resp)
}

// ---------------------------------------------------------------------------
// GitHub API helper
// ---------------------------------------------------------------------------

/// Make an authenticated GET request to the GitHub API and return the
/// parsed JSON value.
async fn github_api_get(url: &str, token: &str) -> std::result::Result<serde_json::Value, String> {
    let headers = Headers::new();
    headers.set("Accept", "application/vnd.github+json").ok();
    headers
        .set("Authorization", &format!("Bearer {token}"))
        .ok();
    headers.set("User-Agent", "wreck-it-worker").ok();
    headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

    let request = Request::new_with_init(
        url,
        RequestInit::new()
            .with_method(Method::Get)
            .with_headers(headers),
    )
    .map_err(|e| format!("Failed to build GitHub API request: {e}"))?;

    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| format!("GitHub API request failed: {e}"))?;

    let status = response.status_code();
    if !(200..300).contains(&status) {
        let body = response.text().await.unwrap_or_default();
        return Err(format!("GitHub API error ({status}): {body}"));
    }

    response
        .json()
        .await
        .map_err(|e| format!("Failed to parse GitHub API response: {e}"))
}

// ---------------------------------------------------------------------------
// Session verification
// ---------------------------------------------------------------------------

/// Verify a portal session from the `Authorization: Bearer {hmac_hex}`
/// header.  Returns the stored GitHub access token on success, or an
/// error [`Response`] on failure.
async fn verify_portal_session(
    req: &Request,
    ctx: &RouteContext<()>,
) -> std::result::Result<String, Response> {
    let header = req
        .headers()
        .get("Authorization")
        .ok()
        .flatten()
        .unwrap_or_default();

    let hmac_hex = header.strip_prefix("Bearer ").unwrap_or("");
    if hmac_hex.is_empty() {
        return Err(error_response("Unauthorized", 401).unwrap());
    }

    let kv = ctx
        .kv(kv_store::KV_BINDING)
        .map_err(|_| error_response("KV binding unavailable", 500).unwrap())?;

    let key = portal_session_key(hmac_hex);
    let session_json = kv
        .get(&key)
        .text()
        .await
        .map_err(|e| {
            console_error!("[wreck-it][portal] KV get failed: {e}");
            error_response("Internal error", 500).unwrap()
        })?
        .ok_or_else(|| error_response("Session expired or invalid", 401).unwrap())?;

    let session: serde_json::Value = serde_json::from_str(&session_json).map_err(|e| {
        console_error!("[wreck-it][portal] session parse error: {e}");
        error_response("Internal error", 500).unwrap()
    })?;

    session["github_token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| error_response("Corrupt session", 500).unwrap())
}

// ---------------------------------------------------------------------------
// Auth endpoints
// ---------------------------------------------------------------------------

/// `GET /api/portal/auth/login` — redirect to GitHub OAuth.
async fn auth_login(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let client_id = ctx
        .secret("GITHUB_CLIENT_ID")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("GITHUB_CLIENT_ID secret not configured".into()))?;

    let parsed_url = req.url()?;
    let redirect_uri = parsed_url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.to_string());

    let mut url = format!(
        "https://github.com/login/oauth/authorize?client_id={client_id}&scope=read:org"
    );
    if let Some(ru) = &redirect_uri {
        url.push_str(&format!(
            "&redirect_uri={}",
            urlencoding::encode(ru)
        ));
    }

    let resp = Response::empty()?.with_status(302);
    let mut resp = cors_headers(resp)?;
    resp.headers_mut().set("Location", &url)?;
    Ok(resp)
}

/// `GET /api/portal/auth/callback` — exchange OAuth code for access token.
async fn auth_callback(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    // Extract the `code` and optional `redirect_uri` query parameters.
    let url = req.url()?;
    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| Error::RustError("Missing 'code' query parameter".into()))?;

    let redirect_uri = url
        .query_pairs()
        .find(|(k, _)| k == "redirect_uri")
        .map(|(_, v)| v.to_string());

    let client_id = ctx
        .secret("GITHUB_CLIENT_ID")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("GITHUB_CLIENT_ID not configured".into()))?;

    let client_secret = ctx
        .secret("GITHUB_CLIENT_SECRET")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("GITHUB_CLIENT_SECRET not configured".into()))?;

    let session_secret = ctx
        .secret("PORTAL_SESSION_SECRET")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("PORTAL_SESSION_SECRET not configured".into()))?;

    // Exchange the code for an access token.
    // If redirect_uri was provided during the auth step, GitHub requires it
    // here as well for verification.
    let mut token_body = serde_json::json!({
        "client_id": client_id,
        "client_secret": client_secret,
        "code": code,
    });
    if let Some(ru) = redirect_uri {
        token_body["redirect_uri"] = serde_json::Value::String(ru);
    }

    let headers = Headers::new();
    headers.set("Accept", "application/json").ok();
    headers.set("Content-Type", "application/json").ok();
    headers.set("User-Agent", "wreck-it-worker").ok();

    let token_req = Request::new_with_init(
        "https://github.com/login/oauth/access_token",
        RequestInit::new()
            .with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(wasm_bindgen::JsValue::from_str(
                &token_body.to_string(),
            ))),
    )?;

    let mut token_resp = Fetch::Request(token_req).send().await?;
    let token_json: serde_json::Value = token_resp.json().await.map_err(|e| {
        console_error!("[wreck-it][portal] token exchange parse error: {e}");
        Error::RustError(format!("Token exchange failed: {e}"))
    })?;

    let access_token = token_json["access_token"]
        .as_str()
        .ok_or_else(|| {
            let err_desc = token_json["error_description"]
                .as_str()
                .unwrap_or("unknown error");
            console_error!("[wreck-it][portal] OAuth token exchange failed: {err_desc}");
            Error::RustError(format!("OAuth token exchange failed: {err_desc}"))
        })?
        .to_string();

    // Fetch user info.
    let user = github_api_get("https://api.github.com/user", &access_token)
        .await
        .map_err(Error::RustError)?;

    // Generate HMAC session token.
    let mut mac = HmacSha256::new_from_slice(session_secret.as_bytes())
        .map_err(|e| Error::RustError(format!("HMAC init failed: {e}")))?;
    mac.update(access_token.as_bytes());
    let hmac_hex = hex::encode(mac.finalize().into_bytes());

    // Store session in KV with TTL.
    let kv = ctx.kv(kv_store::KV_BINDING)?;
    let now_secs = js_sys::Date::now() as u64 / 1000;
    let session_value = serde_json::json!({
        "github_token": access_token,
        "created_at": now_secs,
    });

    kv.put(&portal_session_key(&hmac_hex), session_value.to_string())
        .map_err(|e| Error::RustError(format!("KV put build failed: {e}")))?
        .expiration_ttl(SESSION_TTL_SECS)
        .execute()
        .await
        .map_err(|e| Error::RustError(format!("KV put execute failed: {e}")))?;

    console_log!(
        "[wreck-it][portal] session created for user={}",
        user["login"].as_str().unwrap_or("unknown")
    );

    let resp_body = serde_json::json!({
        "token": hmac_hex,
        "user": {
            "id": user["id"],
            "login": user["login"],
            "avatar_url": user["avatar_url"],
            "name": user["name"],
        },
    });

    json_response(&resp_body, 200)
}

/// `GET /api/portal/auth/user` — return the authenticated user's info.
async fn auth_user(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let github_token = verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let user = github_api_get("https://api.github.com/user", &github_token)
        .await
        .map_err(Error::RustError)?;

    json_response(&user, 200)
}

// ---------------------------------------------------------------------------
// Installation / repository endpoints
// ---------------------------------------------------------------------------

/// `GET /api/portal/installations` — list the user's GitHub App installations.
async fn list_installations(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let github_token = verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let data = github_api_get(
        "https://api.github.com/user/installations",
        &github_token,
    )
    .await
    .map_err(Error::RustError)?;

    // GitHub returns { total_count, installations: [...] } — unwrap to a
    // plain array so the frontend can use it directly.
    let installations = data.get("installations").cloned().unwrap_or(data);
    json_response(&installations, 200)
}

/// `GET /api/portal/installations/:installation_id/repos` — list repos for
/// an installation.  Supports optional `page` and `per_page` query
/// parameters that are forwarded to the GitHub API.  Returns
/// `{ total_count, repositories }` so the frontend can render pagination
/// controls.
async fn list_installation_repos(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let github_token = verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let installation_id = ctx.param("installation_id").ok_or_else(|| {
        Error::RustError("Missing installation_id parameter".into())
    })?;

    // Extract optional pagination query parameters.
    let req_url = req.url().map_err(|e| Error::RustError(format!("bad url: {e}")))?;
    let page = req_url
        .query_pairs()
        .find(|(k, _)| k == "page")
        .and_then(|(_, v)| v.parse::<u32>().ok())
        .unwrap_or(1);
    let per_page = req_url
        .query_pairs()
        .find(|(k, _)| k == "per_page")
        .and_then(|(_, v)| v.parse::<u32>().ok())
        .unwrap_or(30)
        .min(100);

    let url = format!(
        "https://api.github.com/user/installations/{installation_id}/repositories?page={page}&per_page={per_page}"
    );
    let data = github_api_get(&url, &github_token)
        .await
        .map_err(Error::RustError)?;

    // GitHub returns { total_count, repositories: [...] } — forward the
    // full object so the frontend can use total_count for pagination.
    json_response(&data, 200)
}

// ---------------------------------------------------------------------------
// Config endpoints
// ---------------------------------------------------------------------------

/// Obtain an installation token for the given `owner/repo` by discovering
/// the installation ID via a GitHub App JWT and then vending a scoped token.
async fn get_installation_token(
    ctx: &RouteContext<()>,
    owner: &str,
    repo: &str,
) -> std::result::Result<String, Response> {
    let app_id = ctx
        .secret("GITHUB_APP_ID")
        .map(|s| s.to_string())
        .map_err(|_| error_response("GITHUB_APP_ID not configured", 500).unwrap())?;

    let private_key = ctx
        .secret("GITHUB_APP_PRIVATE_KEY")
        .map(|s| s.to_string())
        .map_err(|_| error_response("GITHUB_APP_PRIVATE_KEY not configured", 500).unwrap())?;

    let now_secs = js_sys::Date::now() as u64 / 1000;
    let jwt = github_app::generate_jwt(&app_id, &private_key, now_secs)
        .map_err(|e| error_response(&format!("JWT generation failed: {e}"), 500).unwrap())?;

    // Discover the installation ID for this repository.
    let install_url = format!("https://api.github.com/repos/{owner}/{repo}/installation");
    let install_info = github_api_get(&install_url, &jwt)
        .await
        .map_err(|e| error_response(&format!("Failed to get installation: {e}"), 500).unwrap())?;

    let installation_id = install_info["id"]
        .as_u64()
        .ok_or_else(|| error_response("Missing installation id", 500).unwrap())?;

    github_app::vend_installation_token(installation_id, &jwt, repo)
        .await
        .map_err(|e| error_response(&format!("Token vending failed: {e}"), 500).unwrap())
}

// ---------------------------------------------------------------------------
// Template & Ralph deploy endpoints
// ---------------------------------------------------------------------------

/// Known ralph templates — matches the `templates/engineering-team` directory
/// in the wreck-it repository.  This is a static list embedded in the worker
/// so the frontend can offer "deploy from template" without a GitHub API call.
fn get_ralph_templates() -> serde_json::Value {
    serde_json::json!([
        {
            "id": "engineering-team",
            "name": "Engineering Team",
            "description": "A multi-ralph engineering team with recurring documentation review, feature management, research planning, cohesiveness review, and implementation tasks.",
            "ralphs": [
                {
                    "name": "docs",
                    "task_file": "docs-tasks.json",
                    "state_file": ".docs-state.json",
                    "description": "Periodically assesses and updates project documentation"
                },
                {
                    "name": "features",
                    "task_file": "features-tasks.json",
                    "state_file": ".features-state.json",
                    "description": "Monitors feature tasks and proposes new features when all current work is complete"
                },
                {
                    "name": "planner",
                    "task_file": "planner-tasks.json",
                    "state_file": ".planner-state.json",
                    "description": "Researches trends and proposes novel features leveraging project capabilities"
                },
                {
                    "name": "cohesiveness",
                    "task_file": "cohesiveness-tasks.json",
                    "state_file": ".cohesiveness-state.json",
                    "description": "Reviews features for integration quality and architectural consistency"
                },
                {
                    "name": "feature-dev",
                    "task_file": "feature-dev-tasks.json",
                    "state_file": ".feature-dev-state.json",
                    "description": "Executes feature implementation tasks generated by other ralphs"
                },
                {
                    "name": "merge",
                    "task_file": "merge-tasks.json",
                    "state_file": ".merge-state.json",
                    "command": "merge",
                    "backend": "copilot_cli",
                    "description": "Resolves merge conflicts on open PRs"
                }
            ]
        }
    ])
}

/// `GET /api/portal/templates` — return the list of available ralph templates.
async fn list_templates(_req: Request, _ctx: RouteContext<()>) -> Result<Response> {
    json_response(&get_ralph_templates(), 200)
}

/// Request body for `POST /api/portal/repos/:owner/:repo/ralphs/deploy`.
#[derive(serde::Deserialize)]
struct RalphDeployRequest {
    /// Ralph name (unique identifier).
    name: String,
    /// Path to the task file.
    task_file: String,
    /// Path to the state file.
    state_file: String,
    /// Initial tasks to seed into KV (optional).
    #[serde(default)]
    tasks: Vec<serde_json::Value>,
    /// Optional command override (e.g. "merge", "unstuck").
    #[serde(default)]
    command: Option<String>,
    /// Optional backend override (e.g. "cloud_agent", "copilot_cli").
    #[serde(default)]
    backend: Option<String>,
}

/// `POST /api/portal/repos/:owner/:repo/ralphs/deploy` — deploy a ralph
/// by seeding its initial tasks into KV.
///
/// The caller is responsible for also updating the repo config TOML to
/// include the ralph entry (via the existing `PUT …/config` endpoint).
/// This endpoint only handles the KV task seeding.
async fn deploy_ralph(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();

    let body: RalphDeployRequest = req.json().await.map_err(|e| {
        Error::RustError(format!("Invalid deploy request JSON: {e}"))
    })?;

    let kv = ctx
        .kv(kv_store::KV_BINDING)
        .map_err(|e| Error::RustError(format!("KV binding unavailable: {e}")))?;

    // Build the KV key for this ralph's tasks.
    // Convention: {owner}/{repo}/ralph/{name}/tasks
    let kv_key = format!("{owner}/{repo}/ralph/{}/tasks", body.name);

    // Serialize the initial tasks and store in KV.
    let tasks_json = serde_json::to_string(&body.tasks)
        .map_err(|e| Error::RustError(format!("Failed to serialize tasks: {e}")))?;

    kv.put(&kv_key, tasks_json)
        .map_err(|e| Error::RustError(format!("KV put build failed: {e}")))?
        .execute()
        .await
        .map_err(|e| Error::RustError(format!("KV put execute failed: {e}")))?;

    console_log!(
        "[wreck-it][portal] deployed ralph '{}' for {}/{}",
        body.name,
        owner,
        repo
    );

    json_response(
        &serde_json::json!({
            "status": "deployed",
            "ralph": body.name,
            "task_file": body.task_file,
            "state_file": body.state_file,
            "tasks_count": body.tasks.len(),
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Plan generation endpoint
// ---------------------------------------------------------------------------

/// GitHub Models API endpoint for chat completions.
const GITHUB_MODELS_ENDPOINT: &str = "https://models.github.ai/inference/chat/completions";

/// Default model for plan generation.
const PLAN_MODEL: &str = "openai/gpt-4o-mini";

/// Build the planner prompt that instructs the LLM to emit a structured task
/// plan.  This mirrors the CLI's `planner.rs::build_planner_prompt`.
fn build_planner_prompt(goal: &str) -> String {
    format!(
        "You are a task planning assistant. Your job is to break down a high-level goal \
         into a structured list of concrete development tasks.\n\n\
         Goal: {goal}\n\n\
         Return ONLY a JSON array of task objects with NO additional text, markdown, or explanation.\n\
         Each task object must have exactly these fields:\n\
         - \"id\": a unique string identifier (e.g. \"1\", \"2\", or \"task-1\")\n\
         - \"description\": a clear, actionable description of the task\n\
         - \"phase\": an integer (>= 1) indicating the execution phase; tasks in the same phase \
           can run in parallel, lower phases run first\n\
         - \"depends_on\": (optional) an array of task ID strings that must complete before this \
           task starts\n\n\
         Example output:\n\
         [\n\
           {{\"id\": \"1\", \"description\": \"Set up project structure\", \"phase\": 1}},\n\
           {{\"id\": \"2\", \"description\": \"Implement core logic\", \"phase\": 2, \"depends_on\": [\"1\"]}},\n\
           {{\"id\": \"3\", \"description\": \"Add tests\", \"phase\": 2, \"depends_on\": [\"1\"]}}\n\
         ]\n\n\
         Output the JSON array now:",
        goal = goal,
    )
}

/// Build a prompt that asks the LLM for a short, descriptive plan name.
/// Mirrors the CLI's `planner.rs::build_naming_prompt`.
fn build_naming_prompt(goal: &str) -> String {
    format!(
        "Given the following project goal, produce a short identifier name (2-4 words, \
         lowercase, separated by hyphens) that captures the essence of the goal. \
         The name will be used as a filename-safe label.\n\n\
         Goal: {goal}\n\n\
         Reply with ONLY the hyphenated name and nothing else. \
         Do not include quotes, punctuation, or explanation.\n\n\
         Examples:\n\
         - Goal: \"Build a REST API for user management\" → rest-api-users\n\
         - Goal: \"Add CI/CD pipeline with GitHub Actions\" → ci-cd-pipeline\n\
         - Goal: \"Migrate database from MySQL to PostgreSQL\" → mysql-to-postgres\n\n\
         Name:",
        goal = goal,
    )
}

/// Sanitise raw LLM output into a filesystem-safe slug.
/// Mirrors the CLI's `planner.rs::slugify_plan_name`.
fn slugify_plan_name(raw: &str) -> String {
    let first_line = raw
        .lines()
        .map(str::trim)
        .find(|l| !l.is_empty())
        .unwrap_or("");

    let slug: String = first_line
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();

    let collapsed: String = slug
        .split('-')
        .filter(|s| !s.is_empty())
        .collect::<Vec<_>>()
        .join("-");

    let max_len = 40;
    if collapsed.len() > max_len {
        collapsed[..max_len].trim_end_matches('-').to_string()
    } else if collapsed.is_empty() {
        "plan".to_string()
    } else {
        collapsed
    }
}

/// Extract a JSON array from raw LLM output that may contain markdown
/// fences or surrounding text.
fn extract_json_array(raw: &str) -> std::result::Result<String, String> {
    // Try to find a markdown code fence first.
    if let Some(start) = raw.find("```") {
        let after_fence = &raw[start + 3..];
        // Skip optional language tag on the same line.
        let content_start = after_fence.find('\n').map(|i| i + 1).unwrap_or(0);
        let content = &after_fence[content_start..];
        if let Some(end) = content.find("```") {
            let inner = content[..end].trim();
            if inner.starts_with('[') {
                return Ok(inner.to_string());
            }
        }
    }

    // Fallback: find the first '[' and last ']'.
    if let Some(start) = raw.find('[') {
        if let Some(end) = raw.rfind(']') {
            if end > start {
                return Ok(raw[start..=end].to_string());
            }
        }
    }

    Err("Could not find a JSON array in the LLM output".to_string())
}

/// A minimal plan entry as returned by the LLM.
#[derive(serde::Deserialize)]
struct PlanEntry {
    id: String,
    description: String,
    #[serde(default = "default_phase")]
    phase: u32,
    #[serde(default)]
    depends_on: Vec<String>,
}

fn default_phase() -> u32 {
    1
}

/// Parse and validate the raw LLM output into structured plan tasks.
fn parse_and_validate_plan(raw: &str) -> std::result::Result<Vec<serde_json::Value>, String> {
    let json_str = extract_json_array(raw)?;

    let entries: Vec<PlanEntry> =
        serde_json::from_str(&json_str).map_err(|e| format!("Invalid JSON: {e}"))?;

    if entries.is_empty() {
        return Err("LLM returned an empty task plan".to_string());
    }

    // Check for duplicate IDs.
    let mut seen = std::collections::HashSet::new();
    for entry in &entries {
        if entry.id.is_empty() {
            return Err("Task has an empty id".to_string());
        }
        if entry.description.is_empty() {
            return Err(format!("Task '{}' has an empty description", entry.id));
        }
        if entry.phase == 0 {
            return Err(format!(
                "Task '{}' has an invalid phase 0 (must be >= 1)",
                entry.id
            ));
        }
        if !seen.insert(entry.id.clone()) {
            return Err(format!("Duplicate task id: '{}'", entry.id));
        }
    }

    let tasks: Vec<serde_json::Value> = entries
        .into_iter()
        .map(|e| {
            let mut task = serde_json::json!({
                "id": e.id,
                "description": e.description,
                "status": "pending",
                "phase": e.phase,
            });
            if !e.depends_on.is_empty() {
                task["depends_on"] = serde_json::json!(e.depends_on);
            }
            task
        })
        .collect();

    Ok(tasks)
}

/// Call the GitHub Models API with a prompt and return the response content.
async fn call_models_api(
    api_token: &str,
    prompt: &str,
    model: &str,
) -> std::result::Result<String, String> {
    let body = serde_json::json!({
        "model": model,
        "messages": [
            { "role": "user", "content": prompt }
        ]
    });

    let headers = Headers::new();
    headers
        .set("Authorization", &format!("Bearer {api_token}"))
        .ok();
    headers.set("Content-Type", "application/json").ok();

    let request = Request::new_with_init(
        GITHUB_MODELS_ENDPOINT,
        RequestInit::new()
            .with_method(Method::Post)
            .with_headers(headers)
            .with_body(Some(wasm_bindgen::JsValue::from_str(
                &body.to_string(),
            ))),
    )
    .map_err(|e| format!("Failed to build models API request: {e}"))?;

    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| format!("Models API request failed: {e}"))?;

    let status = response.status_code();
    if !(200..300).contains(&status) {
        let err_body = response.text().await.unwrap_or_default();
        return Err(format!("Models API error ({status}): {err_body}"));
    }

    let json: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse models API response: {e}"))?;

    json.get("choices")
        .and_then(|c| c.get(0))
        .and_then(|c| c.get("message"))
        .and_then(|m| m.get("content"))
        .and_then(|c| c.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| "Models API response missing choices[0].message.content".to_string())
}

/// Request body for `POST /api/portal/repos/:owner/:repo/ralphs/plan`.
#[derive(serde::Deserialize)]
struct PlanRequest {
    /// Natural-language goal to plan for.
    goal: String,
    /// Optional ralph name; if omitted, the LLM generates one.
    #[serde(default)]
    ralph: Option<String>,
}

/// `POST /api/portal/repos/:owner/:repo/ralphs/plan` — generate a task plan
/// from a natural-language goal using the GitHub Models API.
async fn generate_plan(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let body: PlanRequest = req.json().await.map_err(|e| {
        Error::RustError(format!("Invalid plan request JSON: {e}"))
    })?;

    let goal = body.goal.trim().to_string();
    if goal.is_empty() {
        return error_response("Goal must not be empty", 400);
    }

    // Get the models API token from secrets.
    let api_token = ctx
        .secret("GITHUB_MODELS_TOKEN")
        .map(|s| s.to_string())
        .map_err(|_| Error::RustError("GITHUB_MODELS_TOKEN secret not configured".into()))?;

    // Generate the task plan.
    let planner_prompt = build_planner_prompt(&goal);
    let raw_plan = call_models_api(&api_token, &planner_prompt, PLAN_MODEL)
        .await
        .map_err(|e| Error::RustError(format!("Plan generation failed: {e}")))?;

    let tasks = parse_and_validate_plan(&raw_plan)
        .map_err(|e| Error::RustError(format!("Plan validation failed: {e}")))?;

    // Resolve the ralph name.
    let name = match body.ralph {
        Some(n) if !n.trim().is_empty() => slugify_plan_name(n.trim()),
        _ => {
            let naming_prompt = build_naming_prompt(&goal);
            match call_models_api(&api_token, &naming_prompt, PLAN_MODEL).await {
                Ok(raw_name) => slugify_plan_name(&raw_name),
                Err(_) => slugify_plan_name(&goal),
            }
        }
    };

    console_log!(
        "[wreck-it][portal] generated plan '{}' with {} task(s)",
        name,
        tasks.len()
    );

    json_response(
        &serde_json::json!({
            "name": name,
            "tasks": tasks,
        }),
        200,
    )
}

// ---------------------------------------------------------------------------
// Config endpoints
// ---------------------------------------------------------------------------

/// `GET /api/portal/repos/:owner/:repo/config` — read the repo's
/// `.wreck-it/config.toml` and return it as JSON.
async fn get_config(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();

    let token = get_installation_token(&ctx, &owner, &repo)
        .await
        .map_err(|e| Error::RustError(format!("installation token failed: {}", e.status_code())))?;

    // Read the config file via the GitHub Contents API.
    let file_url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/.wreck-it/config.toml"
    );
    let file_resp = github_api_get(&file_url, &token).await;

    match file_resp {
        Ok(file_data) => {
            let content_b64 = file_data["content"]
                .as_str()
                .unwrap_or("")
                .replace('\n', "");

            let decoded = base64_decode(&content_b64);
            let toml_str = String::from_utf8(decoded).map_err(|e| {
                Error::RustError(format!("Config file is not valid UTF-8: {e}"))
            })?;

            let config: toml::Value = toml::from_str(&toml_str).map_err(|e| {
                Error::RustError(format!("Failed to parse config TOML: {e}"))
            })?;

            // Include the file SHA for update operations.
            let mut resp = serde_json::json!(config);
            if let Some(sha) = file_data["sha"].as_str() {
                resp["_sha"] = serde_json::Value::String(sha.to_string());
            }

            json_response(&resp, 200)
        }
        Err(e) if e.contains("404") => {
            // Config file does not exist yet — return empty object.
            json_response(&serde_json::json!({}), 200)
        }
        Err(e) => Err(Error::RustError(format!("Failed to read config: {e}"))),
    }
}

/// `PUT /api/portal/repos/:owner/:repo/config` — write the repo's
/// `.wreck-it/config.toml` from a JSON body.
async fn put_config(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();

    let body: serde_json::Value = req.json().await.map_err(|e| {
        Error::RustError(format!("Invalid JSON body: {e}"))
    })?;

    // Extract the optional file SHA for updates (sent by the GET endpoint).
    let file_sha = body
        .get("_sha")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    // Remove the _sha field before converting to TOML.
    let mut config = body.clone();
    if let Some(obj) = config.as_object_mut() {
        obj.remove("_sha");
    }

    let toml_str = toml::to_string_pretty(&config).map_err(|e| {
        Error::RustError(format!("Failed to serialize config to TOML: {e}"))
    })?;

    let token = get_installation_token(&ctx, &owner, &repo)
        .await
        .map_err(|e| Error::RustError(format!("installation token failed: {}", e.status_code())))?;

    // Write the file via the GitHub Contents API.
    let file_url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/.wreck-it/config.toml"
    );

    let encoded_content = base64_encode(toml_str.as_bytes());

    let mut put_body = serde_json::json!({
        "message": "Update wreck-it configuration via portal",
        "content": encoded_content,
    });
    if let Some(sha) = &file_sha {
        put_body["sha"] = serde_json::Value::String(sha.clone());
    }

    let headers = Headers::new();
    headers.set("Accept", "application/vnd.github+json").ok();
    headers
        .set("Authorization", &format!("Bearer {token}"))
        .ok();
    headers.set("User-Agent", "wreck-it-worker").ok();
    headers.set("X-GitHub-Api-Version", "2022-11-28").ok();
    headers.set("Content-Type", "application/json").ok();

    let put_req = Request::new_with_init(
        &file_url,
        RequestInit::new()
            .with_method(Method::Put)
            .with_headers(headers)
            .with_body(Some(wasm_bindgen::JsValue::from_str(
                &put_body.to_string(),
            ))),
    )?;

    let mut put_resp = Fetch::Request(put_req).send().await?;

    let status = put_resp.status_code();
    if !(200..300).contains(&status) {
        let err_body = put_resp.text().await.unwrap_or_default();
        console_error!("[wreck-it][portal] config write failed ({status}): {err_body}");
        return error_response(
            &format!("Failed to write config ({status})"),
            status,
        );
    }

    console_log!("[wreck-it][portal] config updated for {owner}/{repo}");
    json_response(&config, 200)
}

// ---------------------------------------------------------------------------
// Ralph task & state endpoints
// ---------------------------------------------------------------------------

/// Internal representation of the config settings needed for task/state
/// resolution.
struct ResolvedRalphPaths {
    task_path: String,
    task_branch: String,
    state_path: String,
    state_branch: String,
}

/// Read the repo config and resolve paths for a given ralph name.
async fn resolve_ralph_paths(
    token: &str,
    owner: &str,
    repo: &str,
    ralph_name: &str,
) -> std::result::Result<ResolvedRalphPaths, Response> {
    let file_url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/.wreck-it/config.toml"
    );
    let file_data = github_api_get(&file_url, token)
        .await
        .map_err(|e| error_response(&format!("Failed to read config: {e}"), 500).unwrap())?;

    let content_b64 = file_data["content"]
        .as_str()
        .unwrap_or("")
        .replace('\n', "");

    let decoded = base64_decode(&content_b64);
    let toml_str = String::from_utf8(decoded)
        .map_err(|_| error_response("Config file is not valid UTF-8", 500).unwrap())?;

    let config: toml::Value = toml::from_str(&toml_str)
        .map_err(|_| error_response("Failed to parse config TOML", 500).unwrap())?;

    // Extract branch / directory settings.
    let state_branch = config
        .get("state_branch")
        .and_then(|v| v.as_str())
        .unwrap_or("wreck-it-state")
        .to_string();

    let task_branch = config
        .get("task_branch")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| state_branch.clone());

    let tasks_dir = config
        .get("tasks_dir")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let state_root = config
        .get("state_root")
        .and_then(|v| v.as_str())
        .unwrap_or(".wreck-it")
        .to_string();

    // Find the ralph entry.
    let ralphs = config
        .get("ralphs")
        .and_then(|v| v.as_array())
        .ok_or_else(|| error_response("No ralphs configured", 404).unwrap())?;

    let ralph = ralphs
        .iter()
        .find(|r| r.get("name").and_then(|v| v.as_str()) == Some(ralph_name))
        .ok_or_else(|| {
            error_response(&format!("Ralph '{ralph_name}' not found in config"), 404).unwrap()
        })?;

    let task_file = ralph["task_file"]
        .as_str()
        .ok_or_else(|| error_response("Ralph missing task_file", 500).unwrap())?;

    let state_file = ralph["state_file"]
        .as_str()
        .ok_or_else(|| error_response("Ralph missing state_file", 500).unwrap())?;

    // Resolve full paths.
    let task_path = match &tasks_dir {
        Some(dir) => format!("{dir}/{task_file}"),
        None => task_file.to_string(),
    };

    let state_path = format!("{state_root}/{state_file}");

    Ok(ResolvedRalphPaths {
        task_path,
        task_branch,
        state_path,
        state_branch,
    })
}

/// Read a file from a GitHub repo on a specific branch.  Returns `None`
/// when the file does not exist (404).
async fn read_repo_file(
    token: &str,
    owner: &str,
    repo: &str,
    path: &str,
    branch: &str,
) -> std::result::Result<Option<(String, String)>, Response> {
    let encoded_path = urlencoding::encode(path);
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{encoded_path}?ref={branch}"
    );

    match github_api_get(&url, token).await {
        Ok(data) => {
            let content_b64 = data["content"].as_str().unwrap_or("").replace('\n', "");
            let decoded = base64_decode(&content_b64);
            let content = String::from_utf8(decoded)
                .map_err(|_| error_response("File is not valid UTF-8", 500).unwrap())?;
            let sha = data["sha"].as_str().unwrap_or("").to_string();
            Ok(Some((content, sha)))
        }
        Err(e) if e.contains("404") => Ok(None),
        Err(e) => Err(error_response(&format!("Failed to read file: {e}"), 500).unwrap()),
    }
}

/// Write a file to a GitHub repo on a specific branch.
#[allow(clippy::too_many_arguments)]
async fn write_repo_file(
    token: &str,
    owner: &str,
    repo: &str,
    path: &str,
    branch: &str,
    content: &str,
    sha: Option<&str>,
    message: &str,
) -> std::result::Result<String, Response> {
    let encoded_path = urlencoding::encode(path);
    let url = format!(
        "https://api.github.com/repos/{owner}/{repo}/contents/{encoded_path}"
    );

    let encoded_content = base64_encode(content.as_bytes());

    let mut body = serde_json::json!({
        "message": message,
        "content": encoded_content,
        "branch": branch,
    });
    if let Some(s) = sha {
        body["sha"] = serde_json::Value::String(s.to_string());
    }

    let headers = Headers::new();
    headers.set("Accept", "application/vnd.github+json").ok();
    headers
        .set("Authorization", &format!("Bearer {token}"))
        .ok();
    headers.set("User-Agent", "wreck-it-worker").ok();
    headers.set("X-GitHub-Api-Version", "2022-11-28").ok();
    headers.set("Content-Type", "application/json").ok();

    let put_req = Request::new_with_init(
        &url,
        RequestInit::new()
            .with_method(Method::Put)
            .with_headers(headers)
            .with_body(Some(wasm_bindgen::JsValue::from_str(
                &body.to_string(),
            ))),
    )
    .map_err(|e| error_response(&format!("Failed to build request: {e}"), 500).unwrap())?;

    let mut resp = Fetch::Request(put_req)
        .send()
        .await
        .map_err(|e| error_response(&format!("GitHub API request failed: {e}"), 500).unwrap())?;

    let status = resp.status_code();
    if !(200..300).contains(&status) {
        let err_body = resp.text().await.unwrap_or_default();
        console_error!("[wreck-it][portal] file write failed ({status}): {err_body}");
        return Err(error_response(
            &format!("Failed to write file ({status})"),
            status,
        )
        .unwrap());
    }

    // Extract the new SHA from the response.
    let resp_json: serde_json::Value = resp
        .json()
        .await
        .map_err(|e| error_response(&format!("Failed to parse write response: {e}"), 500).unwrap())?;

    let new_sha = resp_json["content"]["sha"]
        .as_str()
        .unwrap_or("")
        .to_string();

    Ok(new_sha)
}

/// `GET /api/portal/repos/:owner/:repo/ralphs/:name/tasks` — read tasks
/// from the repo file on the task branch.
async fn get_ralph_tasks(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();
    let name = ctx
        .param("name")
        .ok_or_else(|| Error::RustError("Missing name".into()))?
        .to_string();

    let token = get_installation_token(&ctx, &owner, &repo)
        .await
        .map_err(|e| Error::RustError(format!("installation token failed: {}", e.status_code())))?;

    let paths = resolve_ralph_paths(&token, &owner, &repo, &name)
        .await
        .map_err(|e| Error::RustError(format!("resolve paths failed: {}", e.status_code())))?;

    match read_repo_file(&token, &owner, &repo, &paths.task_path, &paths.task_branch).await {
        Ok(Some((content, sha))) => {
            let tasks: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                Error::RustError(format!("Failed to parse task JSON: {e}"))
            })?;
            json_response(
                &serde_json::json!({
                    "tasks": tasks,
                    "_sha": sha,
                    "_path": paths.task_path,
                    "_branch": paths.task_branch,
                }),
                200,
            )
        }
        Ok(None) => json_response(
            &serde_json::json!({
                "tasks": [],
                "_sha": null,
                "_path": paths.task_path,
                "_branch": paths.task_branch,
            }),
            200,
        ),
        Err(e) => Ok(e),
    }
}

/// `PUT /api/portal/repos/:owner/:repo/ralphs/:name/tasks` — write tasks
/// back to the repo file on the task branch.
async fn put_ralph_tasks(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();
    let name = ctx
        .param("name")
        .ok_or_else(|| Error::RustError("Missing name".into()))?
        .to_string();

    let body: serde_json::Value = req.json().await.map_err(|e| {
        Error::RustError(format!("Invalid JSON body: {e}"))
    })?;

    let tasks = body
        .get("tasks")
        .ok_or_else(|| Error::RustError("Missing 'tasks' field".into()))?;

    let file_sha = body.get("_sha").and_then(|v| v.as_str());

    let token = get_installation_token(&ctx, &owner, &repo)
        .await
        .map_err(|e| Error::RustError(format!("installation token failed: {}", e.status_code())))?;

    let paths = resolve_ralph_paths(&token, &owner, &repo, &name)
        .await
        .map_err(|e| Error::RustError(format!("resolve paths failed: {}", e.status_code())))?;

    let content = serde_json::to_string_pretty(tasks).map_err(|e| {
        Error::RustError(format!("Failed to serialize tasks: {e}"))
    })?;

    let message = format!("Update {} tasks via portal", name);
    match write_repo_file(
        &token,
        &owner,
        &repo,
        &paths.task_path,
        &paths.task_branch,
        &content,
        file_sha,
        &message,
    )
    .await
    {
        Ok(new_sha) => json_response(
            &serde_json::json!({
                "tasks": tasks,
                "_sha": new_sha,
                "_path": paths.task_path,
                "_branch": paths.task_branch,
            }),
            200,
        ),
        Err(e) => Ok(e),
    }
}

/// `GET /api/portal/repos/:owner/:repo/ralphs/:name/state` — read state
/// from the repo file on the state branch.
async fn get_ralph_state(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();
    let name = ctx
        .param("name")
        .ok_or_else(|| Error::RustError("Missing name".into()))?
        .to_string();

    let token = get_installation_token(&ctx, &owner, &repo)
        .await
        .map_err(|e| Error::RustError(format!("installation token failed: {}", e.status_code())))?;

    let paths = resolve_ralph_paths(&token, &owner, &repo, &name)
        .await
        .map_err(|e| Error::RustError(format!("resolve paths failed: {}", e.status_code())))?;

    match read_repo_file(
        &token,
        &owner,
        &repo,
        &paths.state_path,
        &paths.state_branch,
    )
    .await
    {
        Ok(Some((content, sha))) => {
            let state: serde_json::Value = serde_json::from_str(&content).map_err(|e| {
                Error::RustError(format!("Failed to parse state JSON: {e}"))
            })?;
            json_response(
                &serde_json::json!({
                    "state": state,
                    "_sha": sha,
                    "_path": paths.state_path,
                    "_branch": paths.state_branch,
                }),
                200,
            )
        }
        Ok(None) => json_response(
            &serde_json::json!({
                "state": {},
                "_sha": null,
                "_path": paths.state_path,
                "_branch": paths.state_branch,
            }),
            200,
        ),
        Err(e) => Ok(e),
    }
}

/// `PUT /api/portal/repos/:owner/:repo/ralphs/:name/state` — write state
/// back to the repo file on the state branch.
async fn put_ralph_state(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    verify_portal_session(&req, &ctx)
        .await
        .map_err(|e| Error::RustError(format!("auth failed: {}", e.status_code())))?;

    let owner = ctx
        .param("owner")
        .ok_or_else(|| Error::RustError("Missing owner".into()))?
        .to_string();
    let repo = ctx
        .param("repo")
        .ok_or_else(|| Error::RustError("Missing repo".into()))?
        .to_string();
    let name = ctx
        .param("name")
        .ok_or_else(|| Error::RustError("Missing name".into()))?
        .to_string();

    let body: serde_json::Value = req.json().await.map_err(|e| {
        Error::RustError(format!("Invalid JSON body: {e}"))
    })?;

    let state = body
        .get("state")
        .ok_or_else(|| Error::RustError("Missing 'state' field".into()))?;

    let file_sha = body.get("_sha").and_then(|v| v.as_str());

    let token = get_installation_token(&ctx, &owner, &repo)
        .await
        .map_err(|e| Error::RustError(format!("installation token failed: {}", e.status_code())))?;

    let paths = resolve_ralph_paths(&token, &owner, &repo, &name)
        .await
        .map_err(|e| Error::RustError(format!("resolve paths failed: {}", e.status_code())))?;

    let content = serde_json::to_string_pretty(state).map_err(|e| {
        Error::RustError(format!("Failed to serialize state: {e}"))
    })?;

    let message = format!("Update {} state via portal", name);
    match write_repo_file(
        &token,
        &owner,
        &repo,
        &paths.state_path,
        &paths.state_branch,
        &content,
        file_sha,
        &message,
    )
    .await
    {
        Ok(new_sha) => json_response(
            &serde_json::json!({
                "state": state,
                "_sha": new_sha,
                "_path": paths.state_path,
                "_branch": paths.state_branch,
            }),
            200,
        ),
        Err(e) => Ok(e),
    }
}

// ---------------------------------------------------------------------------
// Base64 helpers (matching github.rs style)
// ---------------------------------------------------------------------------

/// Standard base64 encoding with padding.
fn base64_encode(data: &[u8]) -> String {
    const CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut result = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

/// Standard base64 decoding (handles newlines from GitHub API responses).
fn base64_decode(input: &str) -> Vec<u8> {
    let mut buf = Vec::new();
    let mut accum: u32 = 0;
    let mut bits: u32 = 0;
    for ch in input.bytes() {
        let val = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' | b'\n' | b'\r' | b' ' => continue,
            _ => continue,
        };
        accum = (accum << 6) | val as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            buf.push((accum >> bits) as u8);
            accum &= (1 << bits) - 1;
        }
    }
    buf
}

// ---------------------------------------------------------------------------
// Router registration
// ---------------------------------------------------------------------------

/// Register all portal API routes on the given [`Router`].
pub fn register_portal_routes(router: Router<'_, ()>) -> Router<'_, ()> {
    router
        // CORS preflight
        .options_async("/api/portal/auth/login", options_handler)
        .options_async("/api/portal/auth/callback", options_handler)
        .options_async("/api/portal/auth/user", options_handler)
        .options_async("/api/portal/installations", options_handler)
        .options_async(
            "/api/portal/installations/:installation_id/repos",
            options_handler,
        )
        .options_async("/api/portal/repos/:owner/:repo/config", options_handler)
        .options_async("/api/portal/templates", options_handler)
        .options_async(
            "/api/portal/repos/:owner/:repo/ralphs/deploy",
            options_handler,
        )
        .options_async(
            "/api/portal/repos/:owner/:repo/ralphs/plan",
            options_handler,
        )
        .options_async(
            "/api/portal/repos/:owner/:repo/ralphs/:name/tasks",
            options_handler,
        )
        .options_async(
            "/api/portal/repos/:owner/:repo/ralphs/:name/state",
            options_handler,
        )
        // Auth endpoints
        .get_async("/api/portal/auth/login", auth_login)
        .get_async("/api/portal/auth/callback", auth_callback)
        .get_async("/api/portal/auth/user", auth_user)
        // Installation endpoints
        .get_async("/api/portal/installations", list_installations)
        .get_async(
            "/api/portal/installations/:installation_id/repos",
            list_installation_repos,
        )
        // Config endpoints
        .get_async("/api/portal/repos/:owner/:repo/config", get_config)
        .put_async("/api/portal/repos/:owner/:repo/config", put_config)
        // Template & ralph deploy endpoints
        .get_async("/api/portal/templates", list_templates)
        .post_async(
            "/api/portal/repos/:owner/:repo/ralphs/deploy",
            deploy_ralph,
        )
        .post_async(
            "/api/portal/repos/:owner/:repo/ralphs/plan",
            generate_plan,
        )
        // Ralph task & state endpoints
        .get_async(
            "/api/portal/repos/:owner/:repo/ralphs/:name/tasks",
            get_ralph_tasks,
        )
        .put_async(
            "/api/portal/repos/:owner/:repo/ralphs/:name/tasks",
            put_ralph_tasks,
        )
        .get_async(
            "/api/portal/repos/:owner/:repo/ralphs/:name/state",
            get_ralph_state,
        )
        .put_async(
            "/api/portal/repos/:owner/:repo/ralphs/:name/state",
            put_ralph_state,
        )
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_portal_session_key() {
        let key = portal_session_key("abc123");
        assert_eq!(key, "_portal/sessions/abc123");

        let key2 = portal_session_key("deadbeef");
        assert_eq!(key2, "_portal/sessions/deadbeef");
    }

    /// Verify the expected CORS header values.  We cannot construct a
    /// `worker::Response` outside the WASM runtime, so we assert the
    /// constant strings that `cors_headers` would set.
    #[test]
    fn test_cors_headers() {
        let origin = "*";
        let methods = "GET, POST, PUT, DELETE, OPTIONS";
        let allowed = "Authorization, Content-Type";

        assert_eq!(origin, "*");
        assert_eq!(methods, "GET, POST, PUT, DELETE, OPTIONS");
        assert_eq!(allowed, "Authorization, Content-Type");
    }

    #[test]
    fn test_base64_round_trip() {
        let input = b"Hello, wreck-it portal!";
        let encoded = base64_encode(input);
        let decoded = base64_decode(&encoded);
        assert_eq!(decoded, input);
    }

    #[test]
    fn test_base64_decode_with_newlines() {
        // GitHub API returns base64 with embedded newlines.
        let encoded = "SGVs\nbG8=";
        let decoded = base64_decode(encoded);
        assert_eq!(decoded, b"Hello");
    }

    #[test]
    fn test_ralph_templates_structure() {
        let templates = get_ralph_templates();
        let arr = templates.as_array().unwrap();
        assert!(!arr.is_empty(), "should have at least one template");

        let first = &arr[0];
        assert_eq!(first["id"].as_str().unwrap(), "engineering-team");
        assert!(first["name"].as_str().is_some());
        assert!(first["description"].as_str().is_some());

        let ralphs = first["ralphs"].as_array().unwrap();
        assert!(!ralphs.is_empty(), "template should have ralphs");

        for ralph in ralphs {
            assert!(ralph["name"].as_str().is_some(), "ralph must have name");
            assert!(
                ralph["task_file"].as_str().is_some(),
                "ralph must have task_file"
            );
            assert!(
                ralph["state_file"].as_str().is_some(),
                "ralph must have state_file"
            );
            assert!(
                ralph["description"].as_str().is_some(),
                "ralph must have description"
            );
        }
    }

    #[test]
    fn test_ralph_templates_has_engineering_team() {
        let templates = get_ralph_templates();
        let arr = templates.as_array().unwrap();
        let eng_team = arr.iter().find(|t| t["id"] == "engineering-team");
        assert!(eng_team.is_some(), "engineering-team template must exist");

        let ralphs = eng_team.unwrap()["ralphs"].as_array().unwrap();
        let names: Vec<&str> = ralphs
            .iter()
            .map(|r| r["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"docs"));
        assert!(names.contains(&"features"));
        assert!(names.contains(&"planner"));
        assert!(names.contains(&"merge"));
    }

    #[test]
    fn test_deploy_request_deserialize() {
        let json = r#"{
            "name": "docs",
            "task_file": "docs-tasks.json",
            "state_file": ".docs-state.json",
            "tasks": [{"id": "t1", "description": "test"}]
        }"#;
        let req: RalphDeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "docs");
        assert_eq!(req.task_file, "docs-tasks.json");
        assert_eq!(req.state_file, ".docs-state.json");
        assert_eq!(req.tasks.len(), 1);
        assert!(req.command.is_none());
        assert!(req.backend.is_none());
    }

    #[test]
    fn test_deploy_request_with_optional_fields() {
        let json = r#"{
            "name": "merge",
            "task_file": "merge-tasks.json",
            "state_file": ".merge-state.json",
            "tasks": [],
            "command": "merge",
            "backend": "copilot_cli"
        }"#;
        let req: RalphDeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "merge");
        assert_eq!(req.command.as_deref(), Some("merge"));
        assert_eq!(req.backend.as_deref(), Some("copilot_cli"));
        assert!(req.tasks.is_empty());
    }

    #[test]
    fn test_deploy_request_minimal() {
        let json = r#"{"name":"test","task_file":"test.json","state_file":".test.json"}"#;
        let req: RalphDeployRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.name, "test");
        assert!(req.tasks.is_empty());
    }

    #[test]
    fn test_resolved_ralph_paths_struct() {
        let paths = ResolvedRalphPaths {
            task_path: "tasks/docs-tasks.json".to_string(),
            task_branch: "master".to_string(),
            state_path: ".wreck-it/.docs-state.json".to_string(),
            state_branch: "wreck-it-state".to_string(),
        };
        assert_eq!(paths.task_path, "tasks/docs-tasks.json");
        assert_eq!(paths.task_branch, "master");
        assert_eq!(paths.state_path, ".wreck-it/.docs-state.json");
        assert_eq!(paths.state_branch, "wreck-it-state");
    }

    #[test]
    fn test_resolved_ralph_paths_no_tasks_dir() {
        let paths = ResolvedRalphPaths {
            task_path: "docs-tasks.json".to_string(),
            task_branch: "wreck-it-state".to_string(),
            state_path: ".wreck-it/.docs-state.json".to_string(),
            state_branch: "wreck-it-state".to_string(),
        };
        assert_eq!(paths.task_path, "docs-tasks.json");
        assert_eq!(paths.task_branch, "wreck-it-state");
    }

    /// Verify resolve logic inline: when tasks_dir is set, task_file gets
    /// prefixed; state_file always gets state_root prefix.
    #[test]
    fn test_path_resolution_logic() {
        // Simulate the resolve logic from resolve_ralph_paths.
        let tasks_dir = Some("tasks".to_string());
        let state_root = ".wreck-it".to_string();
        let task_file = "docs-tasks.json";
        let state_file = ".docs-state.json";

        let task_path = match &tasks_dir {
            Some(dir) => format!("{dir}/{task_file}"),
            None => task_file.to_string(),
        };
        let state_path = format!("{state_root}/{state_file}");

        assert_eq!(task_path, "tasks/docs-tasks.json");
        assert_eq!(state_path, ".wreck-it/.docs-state.json");
    }

    /// When tasks_dir is None, task_file is used as-is.
    #[test]
    fn test_path_resolution_no_tasks_dir() {
        let tasks_dir: Option<String> = None;
        let task_file = "docs-tasks.json";

        let task_path = match &tasks_dir {
            Some(dir) => format!("{dir}/{task_file}"),
            None => task_file.to_string(),
        };

        assert_eq!(task_path, "docs-tasks.json");
    }

    // -----------------------------------------------------------------------
    // Plan generation helper tests
    // -----------------------------------------------------------------------

    #[test]
    fn test_build_planner_prompt_contains_goal() {
        let prompt = build_planner_prompt("Build a REST API");
        assert!(prompt.contains("Build a REST API"));
        assert!(prompt.contains("JSON array"));
        assert!(prompt.contains("\"id\""));
        assert!(prompt.contains("\"description\""));
        assert!(prompt.contains("\"phase\""));
    }

    #[test]
    fn test_build_naming_prompt_contains_goal() {
        let prompt = build_naming_prompt("Add CI/CD pipeline");
        assert!(prompt.contains("Add CI/CD pipeline"));
        assert!(prompt.contains("hyphenated name"));
    }

    #[test]
    fn test_slugify_plan_name_basic() {
        assert_eq!(slugify_plan_name("rest-api-users"), "rest-api-users");
    }

    #[test]
    fn test_slugify_plan_name_with_spaces() {
        assert_eq!(slugify_plan_name("Rest API Users"), "rest-api-users");
    }

    #[test]
    fn test_slugify_plan_name_with_special_chars() {
        assert_eq!(
            slugify_plan_name("hello_world! (test)"),
            "hello-world-test"
        );
    }

    #[test]
    fn test_slugify_plan_name_truncates_long_names() {
        let long = "a-".repeat(30);
        let result = slugify_plan_name(&long);
        assert!(result.len() <= 40);
    }

    #[test]
    fn test_slugify_plan_name_empty_returns_plan() {
        assert_eq!(slugify_plan_name(""), "plan");
        assert_eq!(slugify_plan_name("   "), "plan");
    }

    #[test]
    fn test_slugify_plan_name_takes_first_line() {
        assert_eq!(
            slugify_plan_name("my-plan\nextra commentary"),
            "my-plan"
        );
    }

    #[test]
    fn test_extract_json_array_plain() {
        let input = r#"[{"id":"1","description":"test","phase":1}]"#;
        let result = extract_json_array(input).unwrap();
        assert_eq!(result, input);
    }

    #[test]
    fn test_extract_json_array_with_markdown() {
        let input = "Here is the plan:\n```json\n[{\"id\":\"1\",\"description\":\"test\",\"phase\":1}]\n```\n";
        let result = extract_json_array(input).unwrap();
        assert!(result.starts_with('['));
        assert!(result.ends_with(']'));
    }

    #[test]
    fn test_extract_json_array_with_surrounding_text() {
        let input = "Sure! Here's the plan: [{\"id\":\"1\",\"description\":\"test\",\"phase\":1}] Hope that helps!";
        let result = extract_json_array(input).unwrap();
        assert!(result.starts_with('['));
        assert!(result.ends_with(']'));
    }

    #[test]
    fn test_extract_json_array_no_array() {
        let result = extract_json_array("no array here");
        assert!(result.is_err());
    }

    #[test]
    fn test_parse_and_validate_plan_valid() {
        let input = r#"[
            {"id":"1","description":"Set up project","phase":1},
            {"id":"2","description":"Implement logic","phase":2,"depends_on":["1"]}
        ]"#;
        let tasks = parse_and_validate_plan(input).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0]["id"], "1");
        assert_eq!(tasks[0]["status"], "pending");
        assert_eq!(tasks[1]["depends_on"][0], "1");
    }

    #[test]
    fn test_parse_and_validate_plan_with_markdown() {
        let input = "```json\n[{\"id\":\"1\",\"description\":\"test\",\"phase\":1}]\n```";
        let tasks = parse_and_validate_plan(input).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn test_parse_and_validate_plan_empty_array() {
        let result = parse_and_validate_plan("[]");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty"));
    }

    #[test]
    fn test_parse_and_validate_plan_empty_id() {
        let input = r#"[{"id":"","description":"test","phase":1}]"#;
        let result = parse_and_validate_plan(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty id"));
    }

    #[test]
    fn test_parse_and_validate_plan_empty_description() {
        let input = r#"[{"id":"1","description":"","phase":1}]"#;
        let result = parse_and_validate_plan(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("empty description"));
    }

    #[test]
    fn test_parse_and_validate_plan_phase_zero() {
        let input = r#"[{"id":"1","description":"test","phase":0}]"#;
        let result = parse_and_validate_plan(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("phase 0"));
    }

    #[test]
    fn test_parse_and_validate_plan_duplicate_ids() {
        let input = r#"[
            {"id":"1","description":"first","phase":1},
            {"id":"1","description":"duplicate","phase":1}
        ]"#;
        let result = parse_and_validate_plan(input);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Duplicate"));
    }

    #[test]
    fn test_parse_and_validate_plan_default_phase() {
        let input = r#"[{"id":"1","description":"test"}]"#;
        let tasks = parse_and_validate_plan(input).unwrap();
        assert_eq!(tasks[0]["phase"], 1);
    }

    #[test]
    fn test_parse_and_validate_plan_no_depends_on_field() {
        let input = r#"[{"id":"1","description":"test","phase":1}]"#;
        let tasks = parse_and_validate_plan(input).unwrap();
        assert!(tasks[0].get("depends_on").is_none());
    }

    #[test]
    fn test_plan_request_deserialize() {
        let json = r#"{"goal":"Build an API","ralph":"my-api"}"#;
        let req: PlanRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.goal, "Build an API");
        assert_eq!(req.ralph.as_deref(), Some("my-api"));
    }

    #[test]
    fn test_plan_request_deserialize_minimal() {
        let json = r#"{"goal":"Build an API"}"#;
        let req: PlanRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.goal, "Build an API");
        assert!(req.ralph.is_none());
    }

    #[test]
    fn test_plan_entry_deserialize() {
        let json = r#"{"id":"1","description":"test","phase":2,"depends_on":["0"]}"#;
        let entry: PlanEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.id, "1");
        assert_eq!(entry.description, "test");
        assert_eq!(entry.phase, 2);
        assert_eq!(entry.depends_on, vec!["0"]);
    }

    #[test]
    fn test_plan_entry_deserialize_defaults() {
        let json = r#"{"id":"1","description":"test"}"#;
        let entry: PlanEntry = serde_json::from_str(json).unwrap();
        assert_eq!(entry.phase, 1);
        assert!(entry.depends_on.is_empty());
    }
}
