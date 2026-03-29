//! REST API handlers for task management and state persistence.
//!
//! All endpoints live under `/api/repos/{owner}/{repo}/…` and use Cloudflare
//! KV for storage.  An `API_TOKEN` secret (bearer token) is required for
//! authentication.
//!
//! ## Endpoints
//!
//! | Method   | Path                                               | Description                  |
//! |----------|----------------------------------------------------|-----------------------------|
//! | `GET`    | `/api/repos/{owner}/{repo}/tasks`                  | List all tasks               |
//! | `GET`    | `/api/repos/{owner}/{repo}/tasks/{id}`             | Get a single task            |
//! | `POST`   | `/api/repos/{owner}/{repo}/tasks`                  | Create a new task            |
//! | `PUT`    | `/api/repos/{owner}/{repo}/tasks/{id}`             | Replace a task               |
//! | `PATCH`  | `/api/repos/{owner}/{repo}/tasks/{id}/status`      | Update task status only      |
//! | `DELETE` | `/api/repos/{owner}/{repo}/tasks/{id}`             | Delete a task                |
//! | `GET`    | `/api/repos/{owner}/{repo}/state/{context}`        | Get headless state           |
//! | `PUT`    | `/api/repos/{owner}/{repo}/state/{context}`        | Replace headless state       |
//! | `DELETE` | `/api/repos/{owner}/{repo}/state/{context}`        | Delete headless state        |

use crate::kv_store;
use crate::types::{HeadlessState, Task, TaskStatus};
use worker::*;

// ---------------------------------------------------------------------------
// Auth helper
// ---------------------------------------------------------------------------

/// Verify the bearer token in the `Authorization` header against the
/// `API_TOKEN` secret.  Returns an error `Response` when auth fails.
fn verify_api_token(req: &Request, ctx: &RouteContext<()>) -> std::result::Result<(), Response> {
    let expected = ctx
        .secret("API_TOKEN")
        .map(|s| s.to_string())
        .map_err(|_| Response::error("API_TOKEN secret not configured", 500).unwrap())?;

    let header = req
        .headers()
        .get("Authorization")
        .ok()
        .flatten()
        .unwrap_or_default();

    let token = header.strip_prefix("Bearer ").unwrap_or("");

    if token.is_empty() || token != expected {
        return Err(Response::error("Unauthorized", 401).unwrap());
    }
    Ok(())
}

/// Obtain the KV store binding from the route context.
fn get_kv(ctx: &RouteContext<()>) -> Result<kv::KvStore> {
    ctx.kv(kv_store::KV_BINDING)
}

// ---------------------------------------------------------------------------
// JSON helpers
// ---------------------------------------------------------------------------

fn json_response<T: serde::Serialize>(value: &T, status: u16) -> Result<Response> {
    let body = serde_json::to_string(value)
        .map_err(|e| Error::RustError(format!("JSON serialization failed: {e}")))?;
    let mut resp = Response::ok(body)?;
    resp.headers_mut().set("Content-Type", "application/json")?;
    if status != 200 {
        resp = resp.with_status(status);
    }
    Ok(resp)
}

// ---------------------------------------------------------------------------
// Task endpoints
// ---------------------------------------------------------------------------

/// `GET /api/repos/:owner/:repo/tasks`
pub async fn list_tasks(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap();
    let repo = ctx.param("repo").unwrap();
    let kv = get_kv(&ctx)?;

    match kv_store::load_tasks(&kv, owner, repo).await {
        Ok(tasks) => json_response(&tasks, 200),
        Err(e) => Response::error(e, 500),
    }
}

/// `GET /api/repos/:owner/:repo/tasks/:task_id`
pub async fn get_task(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap();
    let repo = ctx.param("repo").unwrap();
    let task_id = ctx.param("task_id").unwrap();
    let kv = get_kv(&ctx)?;

    match kv_store::load_tasks(&kv, owner, repo).await {
        Ok(tasks) => match tasks.iter().find(|t| t.id == *task_id) {
            Some(task) => json_response(task, 200),
            None => Response::error("Task not found", 404),
        },
        Err(e) => Response::error(e, 500),
    }
}

/// `POST /api/repos/:owner/:repo/tasks`
pub async fn create_task(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    let kv = get_kv(&ctx)?;

    let task: Task = match req.json().await {
        Ok(t) => t,
        Err(e) => return Response::error(format!("Invalid task JSON: {e}"), 400),
    };

    let mut tasks = kv_store::load_tasks(&kv, &owner, &repo)
        .await
        .map_err(Error::RustError)?;

    // Reject duplicate IDs.
    if tasks.iter().any(|t| t.id == task.id) {
        return Response::error(format!("Task with id '{}' already exists", task.id), 409);
    }

    tasks.push(task.clone());
    kv_store::save_tasks(&kv, &owner, &repo, &tasks)
        .await
        .map_err(Error::RustError)?;

    json_response(&task, 201)
}

/// `PUT /api/repos/:owner/:repo/tasks/:task_id`
pub async fn update_task(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    let task_id = ctx.param("task_id").unwrap().clone();
    let kv = get_kv(&ctx)?;

    let task: Task = match req.json().await {
        Ok(t) => t,
        Err(e) => return Response::error(format!("Invalid task JSON: {e}"), 400),
    };

    // Ensure path ID matches body ID.
    if task.id != task_id {
        return Response::error(
            format!(
                "Task id in body ('{}') does not match URL ('{}')",
                task.id, task_id
            ),
            400,
        );
    }

    let mut tasks = kv_store::load_tasks(&kv, &owner, &repo)
        .await
        .map_err(Error::RustError)?;

    if let Some(pos) = tasks.iter().position(|t| t.id == task_id) {
        tasks[pos] = task.clone();
    } else {
        return Response::error("Task not found", 404);
    }

    kv_store::save_tasks(&kv, &owner, &repo, &tasks)
        .await
        .map_err(Error::RustError)?;

    json_response(&task, 200)
}

/// Request body for `PATCH …/tasks/:id/status`.
#[derive(serde::Deserialize)]
struct StatusPatch {
    status: TaskStatus,
}

/// `PATCH /api/repos/:owner/:repo/tasks/:task_id/status`
pub async fn patch_task_status(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    let task_id = ctx.param("task_id").unwrap().clone();
    let kv = get_kv(&ctx)?;

    let patch: StatusPatch = match req.json().await {
        Ok(p) => p,
        Err(e) => return Response::error(format!("Invalid JSON: {e}"), 400),
    };

    let mut tasks = kv_store::load_tasks(&kv, &owner, &repo)
        .await
        .map_err(Error::RustError)?;

    if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
        task.status = patch.status;
        let updated = task.clone();
        kv_store::save_tasks(&kv, &owner, &repo, &tasks)
            .await
            .map_err(Error::RustError)?;
        json_response(&updated, 200)
    } else {
        Response::error("Task not found", 404)
    }
}

/// `DELETE /api/repos/:owner/:repo/tasks/:task_id`
pub async fn delete_task(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    let task_id = ctx.param("task_id").unwrap().clone();
    let kv = get_kv(&ctx)?;

    let mut tasks = kv_store::load_tasks(&kv, &owner, &repo)
        .await
        .map_err(Error::RustError)?;

    let len_before = tasks.len();
    tasks.retain(|t| t.id != task_id);

    if tasks.len() == len_before {
        return Response::error("Task not found", 404);
    }

    kv_store::save_tasks(&kv, &owner, &repo, &tasks)
        .await
        .map_err(Error::RustError)?;

    Response::empty().map(|r| r.with_status(204))
}

// ---------------------------------------------------------------------------
// State endpoints
// ---------------------------------------------------------------------------

/// `GET /api/repos/:owner/:repo/state/:context`
pub async fn get_state(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap();
    let repo = ctx.param("repo").unwrap();
    let context = ctx.param("context").unwrap();
    let kv = get_kv(&ctx)?;

    match kv_store::load_state(&kv, owner, repo, context).await {
        Ok(state) => json_response(&state, 200),
        Err(e) => Response::error(e, 500),
    }
}

/// `PUT /api/repos/:owner/:repo/state/:context`
pub async fn put_state(mut req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    let context = ctx.param("context").unwrap().clone();
    let kv = get_kv(&ctx)?;

    let state: HeadlessState = match req.json().await {
        Ok(s) => s,
        Err(e) => return Response::error(format!("Invalid state JSON: {e}"), 400),
    };

    kv_store::save_state(&kv, &owner, &repo, &context, &state)
        .await
        .map_err(Error::RustError)?;

    json_response(&state, 200)
}

/// `DELETE /api/repos/:owner/:repo/state/:context`
pub async fn delete_state(req: Request, ctx: RouteContext<()>) -> Result<Response> {
    if let Err(r) = verify_api_token(&req, &ctx) {
        return Ok(r);
    }
    let owner = ctx.param("owner").unwrap().clone();
    let repo = ctx.param("repo").unwrap().clone();
    let context = ctx.param("context").unwrap().clone();
    let kv = get_kv(&ctx)?;

    kv_store::delete_state(&kv, &owner, &repo, &context)
        .await
        .map_err(Error::RustError)?;

    Response::empty().map(|r| r.with_status(204))
}

// ---------------------------------------------------------------------------
// Router registration
// ---------------------------------------------------------------------------

/// Register all API routes on the given [`Router`].
pub fn register_routes(router: Router<'_, ()>) -> Router<'_, ()> {
    router
        // Task endpoints
        .get_async(
            "/api/repos/:owner/:repo/tasks",
            list_tasks,
        )
        .get_async(
            "/api/repos/:owner/:repo/tasks/:task_id",
            get_task,
        )
        .post_async(
            "/api/repos/:owner/:repo/tasks",
            create_task,
        )
        .put_async(
            "/api/repos/:owner/:repo/tasks/:task_id",
            update_task,
        )
        .patch_async(
            "/api/repos/:owner/:repo/tasks/:task_id/status",
            patch_task_status,
        )
        .delete_async(
            "/api/repos/:owner/:repo/tasks/:task_id",
            delete_task,
        )
        // State endpoints
        .get_async(
            "/api/repos/:owner/:repo/state/:context",
            get_state,
        )
        .put_async(
            "/api/repos/:owner/:repo/state/:context",
            put_state,
        )
        .delete_async(
            "/api/repos/:owner/:repo/state/:context",
            delete_state,
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_patch_deserializes() {
        let json = r#"{"status":"completed"}"#;
        let patch: StatusPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.status, TaskStatus::Completed);
    }

    #[test]
    fn status_patch_pending() {
        let json = r#"{"status":"pending"}"#;
        let patch: StatusPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.status, TaskStatus::Pending);
    }

    #[test]
    fn status_patch_failed() {
        let json = r#"{"status":"failed"}"#;
        let patch: StatusPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.status, TaskStatus::Failed);
    }

    #[test]
    fn status_patch_in_progress() {
        let json = r#"{"status":"inprogress"}"#;
        let patch: StatusPatch = serde_json::from_str(json).unwrap();
        assert_eq!(patch.status, TaskStatus::InProgress);
    }

    #[test]
    fn status_patch_rejects_invalid() {
        let json = r#"{"status":"unknown"}"#;
        assert!(serde_json::from_str::<StatusPatch>(json).is_err());
    }
}
