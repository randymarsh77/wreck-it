//! Authenticated bridge to the response service. Routing stays in shared Rust.
use serde::Deserialize;
use worker::*;
use wreck_it_core::routing::{route, Account, AccountUsage, RouteRequest};

pub fn register(router: Router<()>) -> Router<()> {
    router
        .get_async("/api/portal/repos/:owner/:repo/response/:action", portal)
        .post_async("/api/portal/repos/:owner/:repo/response/:action", portal)
}

async fn portal(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    // Owner-scoped account settings and incident evidence require push access.
    if let Err(r) = crate::portal_api::require_repo_access(&req, &ctx, &owner, &repo, true).await {
        return Ok(r);
    }
    let principal = match crate::portal_api::session_login(&req, &ctx).await {
        Ok(p) => p,
        Err(r) => return Ok(r),
    };
    let action = ctx.param("action").unwrap();
    if ![
        "view",
        "account",
        "policy",
        "quota",
        "codex-quota",
        "cancel",
        "tick",
    ]
    .contains(&action.as_str())
    {
        return Response::error("Not found", 404);
    }
    let body = if req.method() == Method::Get {
        None
    } else {
        Some(req.text().await?)
    };
    if body.as_ref().is_some_and(|b| b.len() > 65536) {
        return Response::error("Payload too large", 413);
    }
    let url = format!(
        "https://response/{action}?owner={}&repo={}",
        urlencoding::encode(&principal),
        urlencoding::encode(&format!("{owner}/{repo}"))
    );
    let response = forward(&ctx.env, &url, req.method(), body, None).await?;
    crate::portal_api::cors_headers(response)
}

async fn forward(
    env: &Env,
    url: &str,
    method: Method,
    body: Option<String>,
    source: Option<&Headers>,
) -> Result<Response> {
    let headers = Headers::new();
    headers.set(
        "Authorization",
        &format!("Bearer {}", env.secret("RESPONSE_INTERNAL_TOKEN")?),
    )?;
    headers.set("Content-Type", "application/json")?;
    if let Some(source) = source {
        for name in ["x-wreckit-timestamp", "x-wreckit-signature"] {
            if let Some(v) = source.get(name)? {
                headers.set(name, &v)?;
            }
        }
    }
    let mut init = RequestInit::new();
    init.with_method(method).with_headers(headers);
    if let Some(body) = body {
        init.with_body(Some(body.into()));
    }
    let request = Request::new_with_init(url, &init)?;
    env.service("RESPONSE_SERVICE")?
        .fetch_request(request)
        .await
}

pub async fn hook(mut req: Request, env: Env) -> Result<Response> {
    if req.method() != Method::Post {
        return Response::error("Method not allowed", 405);
    }
    let url = req.url()?;
    let parts: Vec<_> = url.path().trim_matches('/').split('/').collect();
    if parts.len() != 6 || parts[..2] != ["response", "hooks"] {
        return Response::error("Not found", 404);
    }
    // /response/hooks/{credential-owner}/{repo-owner}/{repo}/{source}
    let body = req.text().await?;
    if body.len() > 65536 {
        return Response::error("Payload too large", 413);
    }
    let target = format!(
        "https://response/hook?owner={}&repo={}&source={}",
        urlencoding::encode(parts[2]),
        urlencoding::encode(&format!("{}/{}", parts[3], parts[4])),
        urlencoding::encode(parts[5])
    );
    forward(&env, &target, Method::Post, Some(body), Some(req.headers())).await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RoutingInput {
    accounts: Vec<Account>,
    usage: Vec<AccountUsage>,
    request: RouteRequest,
    now: u64,
}

pub async fn internal(mut req: Request, env: Env) -> Result<Response> {
    let expected = env.secret("RESPONSE_INTERNAL_TOKEN")?.to_string();
    if expected.is_empty()
        || req.headers().get("Authorization")?.as_deref() != Some(&format!("Bearer {expected}"))
    {
        return Response::error("Unauthorized", 401);
    }
    if req.method() != Method::Post {
        return Response::error("Method not allowed", 405);
    }
    match req.url()?.path() {
        "/internal/response/route" => {
            let input: RoutingInput = req.json().await?;
            match route(&input.accounts, &input.usage, &input.request, input.now) {
                Ok(d) => Response::from_json(&d),
                Err(_) => Response::error("Invalid routing request", 400),
            }
        }
        "/internal/response/token" => {
            #[derive(Deserialize)]
            struct TokenInput {
                repo: String,
                read_only: bool,
            }
            let input: TokenInput = req.json().await?;
            let parts: Vec<_> = input.repo.split('/').collect();
            if parts.len() != 2
                || parts.iter().any(|p| {
                    p.is_empty()
                        || !p
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
                })
            {
                return Response::error("Invalid repository", 400);
            }
            let jwt = crate::github_app::generate_jwt(
                &env.secret("GITHUB_APP_ID")?.to_string(),
                &env.secret("GITHUB_APP_PRIVATE_KEY")?.to_string(),
                js_sys::Date::now() as u64 / 1000,
            )
            .map_err(Error::RustError)?;
            let headers = Headers::new();
            headers.set("Authorization", &format!("Bearer {jwt}"))?;
            headers.set("Accept", "application/vnd.github+json")?;
            headers.set("User-Agent", "wreck-it-response")?;
            let request = Request::new_with_init(
                &format!("https://api.github.com/repos/{}/installation", input.repo),
                RequestInit::new().with_headers(headers.clone()),
            )?;
            let mut install = Fetch::Request(request).send().await?;
            if install.status_code() != 200 {
                return Response::error("Installation unavailable", 403);
            }
            let install: serde_json::Value = install.json().await?;
            let id = install["id"]
                .as_u64()
                .ok_or_else(|| Error::RustError("Invalid installation".into()))?;
            let mut body = serde_json::json!({"repositories":[parts[1]]});
            if input.read_only {
                body["permissions"] = serde_json::json!({"contents":"read"});
            }
            headers.set("Content-Type", "application/json")?;
            let request = Request::new_with_init(
                &format!("https://api.github.com/app/installations/{id}/access_tokens"),
                RequestInit::new()
                    .with_method(Method::Post)
                    .with_headers(headers)
                    .with_body(Some(body.to_string().into())),
            )?;
            let mut response = Fetch::Request(request).send().await?;
            if response.status_code() != 201 {
                return Response::error("Token unavailable", 502);
            }
            let response: serde_json::Value = response.json().await?;
            Response::from_json(&serde_json::json!({"token":response["token"]}))
        }
        _ => Response::error("Not found", 404),
    }
}
