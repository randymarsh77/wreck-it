//! MCP (Model Context Protocol) server mode for wreck-it.
//!
//! Implements the [Model Context Protocol] JSON-RPC 2.0 stdio transport so
//! that AI assistants (Claude Desktop, VS Code Copilot Chat, Cursor, …) can
//! query and manage the wreck-it task pipeline directly.
//!
//! # Exposed tools
//!
//! | Tool                 | Description                                              |
//! |----------------------|----------------------------------------------------------|
//! | `list_tasks`         | List all tasks with id, status, role, and description    |
//! | `add_task`           | Append a new task to the task file                       |
//! | `update_task_status` | Update the lifecycle status of an existing task          |
//! | `read_artefact`      | Read an artefact by `"task-id/artefact-name"` key        |
//! | `trigger_iteration`  | Return the CLI command that runs one loop iteration      |
//!
//! [Model Context Protocol]: https://spec.modelcontextprotocol.io/

use crate::artefact_store;
use crate::task_manager;
use crate::types::{AgentRole, Task, TaskKind, TaskRuntime, TaskStatus};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::io::{self, BufRead, Write};
use std::path::PathBuf;

// ── JSON-RPC 2.0 message types ──────────────────────────────────────────────

/// A JSON-RPC 2.0 request (may also be a notification when `id` is absent).
#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    /// `None` for notifications.
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

/// A JSON-RPC 2.0 success response.
#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: &'static str,
    id: Value,
    result: Value,
}

/// A JSON-RPC 2.0 error response.
#[derive(Debug, Serialize)]
struct JsonRpcError {
    jsonrpc: &'static str,
    id: Value,
    error: JsonRpcErrorObject,
}

#[derive(Debug, Serialize)]
struct JsonRpcErrorObject {
    code: i32,
    message: String,
}

// Standard JSON-RPC error codes used in response dispatch.
const PARSE_ERROR: i32 = -32700;
const METHOD_NOT_FOUND: i32 = -32601;

// ── MCP protocol version ─────────────────────────────────────────────────────

const MCP_PROTOCOL_VERSION: &str = "2024-11-05";

// ── Server context ───────────────────────────────────────────────────────────

/// Runtime state held by the MCP server for the duration of a session.
struct ServerContext {
    task_file: PathBuf,
    work_dir: PathBuf,
}

impl ServerContext {
    fn artefact_manifest_path(&self) -> PathBuf {
        self.work_dir.join(".wreck-it-artefacts.json")
    }
}

// ── Tool definitions ─────────────────────────────────────────────────────────

fn tools_list() -> Value {
    json!({
        "tools": [
            {
                "name": "list_tasks",
                "description": "List all tasks in the wreck-it task pipeline with their ids, statuses, roles, and descriptions.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "status": {
                            "type": "string",
                            "description": "Optional filter: only return tasks with this status (pending, in-progress, completed, failed).",
                            "enum": ["pending", "in-progress", "completed", "failed"]
                        }
                    },
                    "required": []
                }
            },
            {
                "name": "add_task",
                "description": "Append a new task to the wreck-it task file.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "Unique task identifier (slug-style, e.g. 'impl-login-page')."
                        },
                        "description": {
                            "type": "string",
                            "description": "Human-readable description of the task."
                        },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of task IDs this task depends on."
                        }
                    },
                    "required": ["id", "description"]
                }
            },
            {
                "name": "update_task_status",
                "description": "Update the lifecycle status of an existing task.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "ID of the task to update."
                        },
                        "status": {
                            "type": "string",
                            "description": "New status for the task.",
                            "enum": ["pending", "in-progress", "completed", "failed"]
                        }
                    },
                    "required": ["id", "status"]
                }
            },
            {
                "name": "read_artefact",
                "description": "Read the content of a stored artefact using its composite key 'task-id/artefact-name'.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "key": {
                            "type": "string",
                            "description": "Artefact key in the format 'task-id/artefact-name' (e.g. 'impl-login/output')."
                        }
                    },
                    "required": ["key"]
                }
            },
            {
                "name": "trigger_iteration",
                "description": "Return the CLI command that runs one wreck-it loop iteration. Execute this command in a shell to advance the pipeline.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "headless": {
                            "type": "boolean",
                            "description": "When true the command runs in headless mode (no TUI). Defaults to true."
                        }
                    },
                    "required": []
                }
            }
        ]
    })
}

// ── Tool handlers ─────────────────────────────────────────────────────────────

fn handle_list_tasks(ctx: &ServerContext, params: &Value) -> Result<Value, String> {
    let tasks = task_manager::load_tasks(&ctx.task_file)
        .map_err(|e| format!("Failed to load tasks: {e}"))?;

    let status_filter: Option<TaskStatus> = params
        .get("status")
        .and_then(|v| v.as_str())
        .and_then(parse_task_status);

    let rows: Vec<Value> = tasks
        .iter()
        .filter(|t| status_filter.as_ref().is_none_or(|f| &t.status == f))
        .map(task_to_json)
        .collect();

    let text = format_task_table(&rows);
    Ok(mcp_text(text))
}

fn handle_add_task(ctx: &ServerContext, params: &Value) -> Result<Value, String> {
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'id'".to_string())?;
    let description = params
        .get("description")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'description'".to_string())?;

    let depends_on: Vec<String> = params
        .get("depends_on")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let task = Task {
        id: id.to_string(),
        description: description.to_string(),
        status: TaskStatus::Pending,
        role: AgentRole::default(),
        kind: TaskKind::default(),
        cooldown_seconds: None,
        phase: 1,
        depends_on,
        priority: 0,
        complexity: 1,
        timeout_seconds: None,
        max_retries: None,
        failed_attempts: 0,
        last_attempt_at: None,
        inputs: vec![],
        outputs: vec![],
        runtime: TaskRuntime::default(),
        precondition_prompt: None,
        parent_id: None,
        labels: vec![],
        system_prompt_override: None,
        acceptance_criteria: None,
        evaluation: None,
    };

    task_manager::append_task(&ctx.task_file, task)
        .map_err(|e| format!("Failed to add task: {e}"))?;

    Ok(mcp_text(format!("Task '{}' added successfully.", id)))
}

fn handle_update_task_status(ctx: &ServerContext, params: &Value) -> Result<Value, String> {
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'id'".to_string())?;
    let status_str = params
        .get("status")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'status'".to_string())?;
    let status = parse_task_status(status_str).ok_or_else(|| {
        format!(
            "Unknown status '{}'. Valid values: pending, in-progress, completed, failed",
            status_str
        )
    })?;

    task_manager::set_task_status(&ctx.task_file, id, status)
        .map_err(|e| format!("Failed to update task status: {e}"))?;

    Ok(mcp_text(format!(
        "Task '{}' status updated to '{}'.",
        id, status_str
    )))
}

fn handle_read_artefact(ctx: &ServerContext, params: &Value) -> Result<Value, String> {
    let key = params
        .get("key")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'key'".to_string())?;

    let manifest_path = ctx.artefact_manifest_path();
    let manifest = artefact_store::load_manifest(&manifest_path)
        .map_err(|e| format!("Failed to load artefact manifest: {e}"))?;

    match manifest.artefacts.get(key) {
        Some(entry) => Ok(mcp_text(entry.content.clone())),
        None => {
            let keys: Vec<&str> = manifest.artefacts.keys().map(|s| s.as_str()).collect();
            if keys.is_empty() {
                Err(format!("Artefact '{}' not found (manifest is empty).", key))
            } else {
                Err(format!(
                    "Artefact '{}' not found. Available keys: {}",
                    key,
                    keys.join(", ")
                ))
            }
        }
    }
}

fn handle_trigger_iteration(ctx: &ServerContext, params: &Value) -> Result<Value, String> {
    let headless = params
        .get("headless")
        .and_then(|v| v.as_bool())
        .unwrap_or(true);

    let task_file = ctx.task_file.to_string_lossy();
    let work_dir = ctx.work_dir.to_string_lossy();

    let cmd = if headless {
        format!(
            "wreck-it run --task-file {:?} --work-dir {:?} --headless",
            task_file, work_dir
        )
    } else {
        format!(
            "wreck-it run --task-file {:?} --work-dir {:?}",
            task_file, work_dir
        )
    };

    Ok(mcp_text(format!(
        "Run the following command to advance the pipeline:\n\n{cmd}"
    )))
}

// ── Request dispatcher ────────────────────────────────────────────────────────

fn dispatch_tool_call(ctx: &ServerContext, params: &Value) -> (Value, Option<bool>) {
    let name = match params.get("name").and_then(|v| v.as_str()) {
        Some(n) => n,
        None => {
            return (
                json!({
                    "content": [{"type": "text", "text": "Missing tool name in tools/call params"}],
                    "isError": true
                }),
                None,
            )
        }
    };

    let args = params.get("arguments").unwrap_or(&Value::Null);

    let result = match name {
        "list_tasks" => handle_list_tasks(ctx, args),
        "add_task" => handle_add_task(ctx, args),
        "update_task_status" => handle_update_task_status(ctx, args),
        "read_artefact" => handle_read_artefact(ctx, args),
        "trigger_iteration" => handle_trigger_iteration(ctx, args),
        unknown => Err(format!("Unknown tool '{unknown}'")),
    };

    match result {
        Ok(content) => (content, Some(false)),
        Err(msg) => (
            json!({
                "content": [{"type": "text", "text": msg}],
                "isError": true
            }),
            Some(true),
        ),
    }
}

// ── Message processing ────────────────────────────────────────────────────────

fn process_message(ctx: &ServerContext, line: &str) -> Option<String> {
    let req: JsonRpcRequest = match serde_json::from_str(line) {
        Ok(r) => r,
        Err(e) => {
            let err = JsonRpcError {
                jsonrpc: "2.0",
                id: Value::Null,
                error: JsonRpcErrorObject {
                    code: PARSE_ERROR,
                    message: format!("Parse error: {e}"),
                },
            };
            return Some(serde_json::to_string(&err).unwrap());
        }
    };

    // Notifications have no id and expect no response.
    let id = req.id?;

    let result = match req.method.as_str() {
        "initialize" => {
            json!({
                "protocolVersion": MCP_PROTOCOL_VERSION,
                "capabilities": {
                    "tools": {}
                },
                "serverInfo": {
                    "name": "wreck-it",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })
        }

        "tools/list" => tools_list(),

        "tools/call" => {
            let (content, is_error) = dispatch_tool_call(ctx, &req.params);
            if let Some(is_err) = is_error {
                let mut obj = content;
                if let Value::Object(ref mut map) = obj {
                    map.insert("isError".to_string(), Value::Bool(is_err));
                }
                let resp = JsonRpcResponse {
                    jsonrpc: "2.0",
                    id,
                    result: obj,
                };
                return Some(serde_json::to_string(&resp).unwrap());
            }
            content
        }

        "ping" => json!({}),

        unknown => {
            let err = JsonRpcError {
                jsonrpc: "2.0",
                id,
                error: JsonRpcErrorObject {
                    code: METHOD_NOT_FOUND,
                    message: format!("Method not found: '{unknown}'"),
                },
            };
            return Some(serde_json::to_string(&err).unwrap());
        }
    };

    let resp = JsonRpcResponse {
        jsonrpc: "2.0",
        id,
        result,
    };
    Some(serde_json::to_string(&resp).unwrap())
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run the MCP server over stdin/stdout.
///
/// Reads newline-delimited JSON-RPC 2.0 messages from stdin, processes each
/// message, and writes responses to stdout.  The function returns when stdin
/// is closed.
pub fn run_mcp_server(task_file: PathBuf, work_dir: Option<PathBuf>) -> Result<()> {
    let work_dir = work_dir.unwrap_or_else(|| std::env::current_dir().unwrap_or_default());

    let ctx = ServerContext {
        task_file,
        work_dir,
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut out = stdout.lock();

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        if let Some(response) = process_message(&ctx, &line) {
            writeln!(out, "{response}")?;
            out.flush()?;
        }
    }

    Ok(())
}

// ── Helpers ───────────────────────────────────────────────────────────────────

fn parse_task_status(s: &str) -> Option<TaskStatus> {
    match s {
        "pending" => Some(TaskStatus::Pending),
        "in-progress" | "in_progress" => Some(TaskStatus::InProgress),
        "completed" => Some(TaskStatus::Completed),
        "failed" => Some(TaskStatus::Failed),
        _ => None,
    }
}

fn task_to_json(task: &Task) -> Value {
    let status_str = match task.status {
        TaskStatus::Pending => "pending",
        TaskStatus::InProgress => "in-progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
    };
    let role_str = match task.role {
        AgentRole::Ideas => "ideas",
        AgentRole::Implementer => "implementer",
        AgentRole::Evaluator => "evaluator",
        AgentRole::SecurityGate => "security_gate",
        AgentRole::CoverageEnforcer => "coverage_enforcer",
        AgentRole::ChangelogGenerator => "changelog_generator",
    };
    json!({
        "id": task.id,
        "description": task.description,
        "status": status_str,
        "role": role_str,
        "phase": task.phase,
        "priority": task.priority,
        "depends_on": task.depends_on,
        "labels": task.labels,
    })
}

fn format_task_table(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "No tasks found.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<32} {:<12} {:<14} {:<5} {:<8}  DEPENDS_ON",
        "ID", "STATUS", "ROLE", "PHASE", "PRIORITY"
    ));
    lines.push("-".repeat(90));

    for row in rows {
        let id = row["id"].as_str().unwrap_or("");
        let status = row["status"].as_str().unwrap_or("");
        let role = row["role"].as_str().unwrap_or("");
        let phase = row["phase"].as_u64().unwrap_or(0);
        let priority = row["priority"].as_u64().unwrap_or(0);
        let deps: Vec<&str> = row["depends_on"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        lines.push(format!(
            "{:<32} {:<12} {:<14} {:<5} {:<8}  {}",
            id,
            status,
            role,
            phase,
            priority,
            deps.join(", ")
        ));
    }

    lines.join("\n")
}

/// Wrap plain text in an MCP `content` array (tool call result format).
fn mcp_text(text: impl Into<String>) -> Value {
    json!({
        "content": [{"type": "text", "text": text.into()}]
    })
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    fn make_ctx(task_file: PathBuf, work_dir: PathBuf) -> ServerContext {
        ServerContext {
            task_file,
            work_dir,
        }
    }

    fn setup_task_file() -> (tempfile::TempDir, PathBuf) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        task_manager::save_tasks(&path, &[]).unwrap();
        (dir, path)
    }

    // ── initialize ──────────────────────────────────────────────────────────

    #[test]
    fn initialize_returns_protocol_version() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        assert_eq!(v["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(v["result"]["serverInfo"]["name"], "wreck-it");
        assert_eq!(v["id"], 1);
    }

    // ── tools/list ──────────────────────────────────────────────────────────

    #[test]
    fn tools_list_contains_expected_tools() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        let tools = v["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"list_tasks"));
        assert!(names.contains(&"add_task"));
        assert!(names.contains(&"update_task_status"));
        assert!(names.contains(&"read_artefact"));
        assert!(names.contains(&"trigger_iteration"));
    }

    // ── list_tasks ──────────────────────────────────────────────────────────

    #[test]
    fn list_tasks_empty() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "list_tasks",
                "arguments": {}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("No tasks"));
    }

    #[test]
    fn list_tasks_after_add() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Add a task first.
        let add_req = json!({
            "jsonrpc": "2.0",
            "id": 10,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "t1", "description": "Do something"}
            }
        })
        .to_string();
        process_message(&ctx, &add_req).unwrap();

        // Now list.
        let list_req = json!({
            "jsonrpc": "2.0",
            "id": 11,
            "method": "tools/call",
            "params": {
                "name": "list_tasks",
                "arguments": {}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("t1"));
    }

    // ── add_task ─────────────────────────────────────────────────────────────

    #[test]
    fn add_task_success() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0",
            "id": 4,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "my-task", "description": "Do something cool"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("my-task"));
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        // Verify task was written.
        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "my-task");
    }

    #[test]
    fn add_task_missing_id_returns_error() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0",
            "id": 5,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"description": "No id provided"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }

    // ── update_task_status ───────────────────────────────────────────────────

    #[test]
    fn update_task_status_success() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Pre-populate.
        let add_req = json!({
            "jsonrpc": "2.0", "id": 20,
            "method": "tools/call",
            "params": {"name": "add_task", "arguments": {"id": "t2", "description": "Work"}}
        })
        .to_string();
        process_message(&ctx, &add_req).unwrap();

        let upd_req = json!({
            "jsonrpc": "2.0", "id": 21,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "t2", "status": "completed"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &upd_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks[0].status, TaskStatus::Completed);
    }

    #[test]
    fn update_task_status_unknown_status() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 22,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "x", "status": "bogus"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }

    // ── read_artefact ────────────────────────────────────────────────────────

    #[test]
    fn read_artefact_not_found() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 30,
            "method": "tools/call",
            "params": {
                "name": "read_artefact",
                "arguments": {"key": "task-1/output"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }

    #[test]
    fn read_artefact_found() {
        use crate::artefact_store::{save_manifest, ArtefactEntry, ArtefactManifest};
        use crate::types::ArtefactKind;

        let (dir, task_file) = setup_task_file();
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let mut manifest = ArtefactManifest::default();
        manifest.artefacts.insert(
            "task-1/output".to_string(),
            ArtefactEntry {
                kind: ArtefactKind::File,
                name: "output".to_string(),
                path: "out.txt".to_string(),
                content: "hello world".to_string(),
            },
        );
        save_manifest(&manifest_path, &manifest).unwrap();

        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 31,
            "method": "tools/call",
            "params": {
                "name": "read_artefact",
                "arguments": {"key": "task-1/output"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(
            v["result"]["content"][0]["text"].as_str().unwrap(),
            "hello world"
        );
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));
    }

    // ── trigger_iteration ────────────────────────────────────────────────────

    #[test]
    fn trigger_iteration_returns_command() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 40,
            "method": "tools/call",
            "params": {
                "name": "trigger_iteration",
                "arguments": {}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("wreck-it run"));
        assert!(text.contains("--headless"));
    }

    // ── unknown method ───────────────────────────────────────────────────────

    #[test]
    fn unknown_method_returns_method_not_found() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":99,"method":"nonexistent","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], METHOD_NOT_FOUND);
    }

    // ── parse error ──────────────────────────────────────────────────────────

    #[test]
    fn invalid_json_returns_parse_error() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let resp = process_message(&ctx, "not json at all").unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["error"]["code"], PARSE_ERROR);
    }

    // ── notification (no response) ───────────────────────────────────────────

    #[test]
    fn notification_returns_none() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        assert!(process_message(&ctx, req).is_none());
    }

    // ── list_tasks status filter ─────────────────────────────────────────────

    #[test]
    fn list_tasks_status_filter() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Add two tasks.
        for id in &["a", "b"] {
            let add_req = json!({
                "jsonrpc": "2.0", "id": 50,
                "method": "tools/call",
                "params": {"name": "add_task", "arguments": {"id": id, "description": "x"}}
            })
            .to_string();
            process_message(&ctx, &add_req).unwrap();
        }
        // Mark 'a' as completed.
        let upd_req = json!({
            "jsonrpc": "2.0", "id": 51,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "a", "status": "completed"}
            }
        })
        .to_string();
        process_message(&ctx, &upd_req).unwrap();

        // Filter for completed.
        let list_req = json!({
            "jsonrpc": "2.0", "id": 52,
            "method": "tools/call",
            "params": {
                "name": "list_tasks",
                "arguments": {"status": "completed"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        // 'a' (completed) should be present, 'b' (pending) should not.
        assert!(text.contains("a"), "expected task 'a' in output: {text}");
        // Each row starts with the task ID left-padded; 'b' is only present as
        // a subtask that was never moved to completed so it should be absent.
        let lines: Vec<&str> = text
            .lines()
            .filter(|l| !l.starts_with('-') && !l.starts_with("ID"))
            .collect();
        assert_eq!(lines.len(), 1, "expected exactly one data row: {text}");
    }
}
