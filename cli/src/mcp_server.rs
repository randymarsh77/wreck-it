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
//! | `get_task`           | Get full details of a single task by id                  |
//! | `add_task`           | Append a new task to the task file                       |
//! | `update_task_status` | Update the lifecycle status of an existing task          |
//! | `read_artefact`      | Read an artefact by `"task-id/artefact-name"` key        |
//! | `list_artefacts`     | List all artefact keys in the artefact manifest          |
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
                        "role": {
                            "type": "string",
                            "description": "Agent role for this task. Defaults to 'implementer'.",
                            "enum": ["ideas", "implementer", "evaluator", "security_gate", "coverage_enforcer", "changelog_generator"]
                        },
                        "depends_on": {
                            "type": "array",
                            "items": { "type": "string" },
                            "description": "Optional list of task IDs this task depends on."
                        },
                        "priority": {
                            "type": "integer",
                            "description": "Optional task priority (higher number = higher priority). Defaults to 0."
                        },
                        "acceptance_criteria": {
                            "type": "string",
                            "description": "Optional acceptance criteria describing what constitutes successful completion of this task."
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
                "name": "get_task",
                "description": "Get the full details of a single task by its ID.",
                "inputSchema": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "type": "string",
                            "description": "ID of the task to retrieve."
                        }
                    },
                    "required": ["id"]
                }
            },
            {
                "name": "list_artefacts",
                "description": "List all artefact keys stored in the artefact manifest.",
                "inputSchema": {
                    "type": "object",
                    "properties": {},
                    "required": []
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

    let role: AgentRole = params
        .get("role")
        .and_then(|v| v.as_str())
        .map(|r| {
            parse_agent_role(r).ok_or_else(|| {
                format!(
                    "Unknown role '{}'. Valid values: ideas, implementer, evaluator, security_gate, coverage_enforcer, changelog_generator",
                    r
                )
            })
        })
        .transpose()?
        .unwrap_or_default();

    let depends_on: Vec<String> = params
        .get("depends_on")
        .and_then(|v| v.as_array())
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.to_string()))
                .collect()
        })
        .unwrap_or_default();

    let priority: u32 = params
        .get("priority")
        .and_then(|v| v.as_u64())
        .map(|p| p as u32)
        .unwrap_or(0);

    let acceptance_criteria: Option<String> = params
        .get("acceptance_criteria")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let task = Task {
        id: id.to_string(),
        description: description.to_string(),
        status: TaskStatus::Pending,
        role,
        kind: TaskKind::default(),
        cooldown_seconds: None,
        phase: 1,
        depends_on,
        priority,
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
        acceptance_criteria,
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

    let task_file = shell_quote(&ctx.task_file.to_string_lossy());
    let work_dir = shell_quote(&ctx.work_dir.to_string_lossy());

    let cmd = if headless {
        format!("wreck-it run --task-file {task_file} --work-dir {work_dir} --headless")
    } else {
        format!("wreck-it run --task-file {task_file} --work-dir {work_dir}")
    };

    Ok(mcp_text(format!(
        "Run the following command to advance the pipeline:\n\n{cmd}"
    )))
}

fn handle_get_task(ctx: &ServerContext, params: &Value) -> Result<Value, String> {
    let id = params
        .get("id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "Missing required parameter 'id'".to_string())?;

    let tasks = task_manager::load_tasks(&ctx.task_file)
        .map_err(|e| format!("Failed to load tasks: {e}"))?;

    match tasks.iter().find(|t| t.id == id) {
        Some(task) => {
            let json = task_to_json(task);
            Ok(mcp_text(
                serde_json::to_string_pretty(&json).unwrap_or_default(),
            ))
        }
        None => Err(format!("Task '{}' not found.", id)),
    }
}

fn handle_list_artefacts(ctx: &ServerContext) -> Result<Value, String> {
    let manifest_path = ctx.artefact_manifest_path();
    let manifest = artefact_store::load_manifest(&manifest_path)
        .map_err(|e| format!("Failed to load artefact manifest: {e}"))?;

    if manifest.artefacts.is_empty() {
        return Ok(mcp_text("No artefacts found."));
    }

    let mut keys: Vec<&str> = manifest.artefacts.keys().map(|s| s.as_str()).collect();
    keys.sort_unstable();
    Ok(mcp_text(keys.join("\n")))
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
        "get_task" => handle_get_task(ctx, args),
        "add_task" => handle_add_task(ctx, args),
        "update_task_status" => handle_update_task_status(ctx, args),
        "read_artefact" => handle_read_artefact(ctx, args),
        "list_artefacts" => handle_list_artefacts(ctx),
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
                    "tools": {},
                    "resources": {},
                    "prompts": {}
                },
                "serverInfo": {
                    "name": "wreck-it",
                    "version": env!("CARGO_PKG_VERSION")
                }
            })
        }

        "tools/list" => tools_list(),

        // Return empty lists for resources and prompts so that MCP clients that
        // send these discovery requests during their initial handshake do not
        // receive a METHOD_NOT_FOUND error.
        "resources/list" => json!({ "resources": [] }),

        "prompts/list" => json!({ "prompts": [] }),

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

fn parse_agent_role(s: &str) -> Option<AgentRole> {
    match s {
        "ideas" => Some(AgentRole::Ideas),
        "implementer" => Some(AgentRole::Implementer),
        "evaluator" => Some(AgentRole::Evaluator),
        "security_gate" => Some(AgentRole::SecurityGate),
        "coverage_enforcer" => Some(AgentRole::CoverageEnforcer),
        "changelog_generator" => Some(AgentRole::ChangelogGenerator),
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

/// Truncate a task description to at most 42 characters for table display.
///
/// Uses char-boundary–safe iteration so that multi-byte Unicode characters are
/// never split.  Descriptions longer than 42 chars are shortened and a
/// horizontal-ellipsis (U+2026 '…') is appended.
fn truncate_description(description: &str) -> String {
    // 42 chars fits within the 44-wide DESCRIPTION column (42 chars + 1 ellipsis + 1 space).
    const MAX_CHARS: usize = 42;
    let mut chars = description.chars().peekable();
    let prefix: String = chars.by_ref().take(MAX_CHARS).collect();
    if chars.peek().is_some() {
        format!("{prefix}…")
    } else {
        prefix
    }
}

fn format_task_table(rows: &[Value]) -> String {
    if rows.is_empty() {
        return "No tasks found.".to_string();
    }

    let mut lines = Vec::new();
    lines.push(format!(
        "{:<32} {:<12} {:<14} {:<44} DEPENDS_ON",
        "ID", "STATUS", "ROLE", "DESCRIPTION"
    ));
    // Separator width: 32 + 1 + 12 + 1 + 14 + 1 + 44 + 1 + "DEPENDS_ON" (10) = 116,
    // but we use 110 to keep the line compact for typical terminal widths.
    lines.push("-".repeat(110));

    for row in rows {
        let id = row["id"].as_str().unwrap_or("");
        let status = row["status"].as_str().unwrap_or("");
        let role = row["role"].as_str().unwrap_or("");
        let description = row["description"].as_str().unwrap_or("");
        let description_truncated = truncate_description(description);
        let deps: Vec<&str> = row["depends_on"]
            .as_array()
            .map(|a| a.iter().filter_map(|v| v.as_str()).collect())
            .unwrap_or_default();
        lines.push(format!(
            "{:<32} {:<12} {:<14} {:<44} {}",
            id,
            status,
            role,
            description_truncated,
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

/// Return a shell-safe representation of `s`.
///
/// Paths that contain no shell-special characters are returned as-is.
/// All other strings are wrapped in single quotes with any embedded single
/// quotes escaped using the `'\''` idiom.
fn shell_quote(s: &str) -> String {
    let needs_quoting = s.chars().any(|c| {
        c.is_whitespace()
            || matches!(
                c,
                '\'' | '"'
                    | '\\'
                    | '$'
                    | '!'
                    | '&'
                    | '|'
                    | ';'
                    | '('
                    | ')'
                    | '<'
                    | '>'
                    | '`'
                    | '{'
                    | '}'
                    | '*'
                    | '?'
                    | '['
                    | ']'
                    | '#'
                    | '~'
            )
    });
    if needs_quoting {
        format!("'{}'", s.replace('\'', r"'\''"))
    } else {
        s.to_string()
    }
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

    /// Return only the data rows from a `list_tasks` table response text,
    /// skipping the header and separator lines.
    fn data_rows(text: &str) -> Vec<&str> {
        text.lines()
            .filter(|l| !l.starts_with('-') && !l.starts_with("ID"))
            .collect()
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

    #[test]
    fn initialize_declares_tools_resources_prompts_capabilities() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        let caps = &v["result"]["capabilities"];
        assert!(caps["tools"].is_object());
        assert!(caps["resources"].is_object());
        assert!(caps["prompts"].is_object());
    }

    // ── resources/list ──────────────────────────────────────────────────────

    #[test]
    fn resources_list_returns_empty_array() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":5,"method":"resources/list","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        // Should be a success response with an empty resources array.
        assert!(v["error"].is_null());
        assert_eq!(v["result"]["resources"], json!([]));
        assert_eq!(v["id"], 5);
    }

    // ── prompts/list ────────────────────────────────────────────────────────

    #[test]
    fn prompts_list_returns_empty_array() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":6,"method":"prompts/list","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        // Should be a success response with an empty prompts array.
        assert!(v["error"].is_null());
        assert_eq!(v["result"]["prompts"], json!([]));
        assert_eq!(v["id"], 6);
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
        assert!(
            text.contains("Do something"),
            "description should appear in list output"
        );
    }

    #[test]
    fn list_tasks_description_truncated() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Use a 50-character ASCII description (exceeds 42-char limit).
        let long_desc = "A".repeat(50);
        let add_req = json!({
            "jsonrpc": "2.0", "id": 60,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "long-desc-task", "description": long_desc}
            }
        })
        .to_string();
        process_message(&ctx, &add_req).unwrap();

        let list_req = json!({
            "jsonrpc": "2.0", "id": 61,
            "method": "tools/call",
            "params": {"name": "list_tasks", "arguments": {}}
        })
        .to_string();
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        // The description should be truncated with an ellipsis.
        assert!(
            text.contains('…'),
            "long description should be truncated with ellipsis: {text}"
        );
        // The full 50-char description should NOT appear verbatim.
        assert!(
            !text.contains(&"A".repeat(50)),
            "full long description should be truncated: {text}"
        );
        // The truncated prefix should be exactly 42 characters.
        assert!(
            text.contains(&"A".repeat(42)),
            "truncated description should be 42 chars: {text}"
        );
    }

    #[test]
    fn list_tasks_description_unicode_safe() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Use multi-byte Unicode characters to ensure truncation is char-safe.
        let long_desc = "é".repeat(50); // 'é' is 2 bytes in UTF-8
        let add_req = json!({
            "jsonrpc": "2.0", "id": 62,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "unicode-task", "description": long_desc}
            }
        })
        .to_string();
        process_message(&ctx, &add_req).unwrap();

        let list_req = json!({
            "jsonrpc": "2.0", "id": 63,
            "method": "tools/call",
            "params": {"name": "list_tasks", "arguments": {}}
        })
        .to_string();
        // Should not panic on multi-byte characters.
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains('…'),
            "unicode description should be truncated: {text}"
        );
    }

    // ── truncate_description ─────────────────────────────────────────────────

    #[test]
    fn truncate_description_short() {
        assert_eq!(truncate_description("hello"), "hello");
    }

    #[test]
    fn truncate_description_exact_limit() {
        let s = "A".repeat(42);
        assert_eq!(
            truncate_description(&s),
            s,
            "exactly 42 chars should not be truncated"
        );
    }

    #[test]
    fn truncate_description_over_limit() {
        let s = "A".repeat(50);
        let result = truncate_description(&s);
        let prefix: String = result.chars().take_while(|&c| c == 'A').collect();
        assert_eq!(
            prefix.chars().count(),
            42,
            "prefix should be exactly 42 chars"
        );
        assert!(result.ends_with('…'));
    }

    #[test]
    fn truncate_description_unicode() {
        // 'é' is 2 bytes in UTF-8; truncating by byte index would panic at odd positions.
        let s = "é".repeat(50);
        let result = truncate_description(&s);
        let prefix: String = result.chars().take_while(|&c| c == 'é').collect();
        assert_eq!(prefix.chars().count(), 42);
        assert!(result.ends_with('…'));
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

    #[test]
    fn add_task_with_depends_on() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 6,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {
                    "id": "child-task",
                    "description": "Depends on parent",
                    "depends_on": ["parent-task"]
                }
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        // Verify that depends_on was persisted.
        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].depends_on, vec!["parent-task".to_string()]);
    }

    #[test]
    fn add_task_with_role_ideas() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 7,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "ideas-task", "description": "Brainstorm ideas", "role": "ideas"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].role, AgentRole::Ideas);
    }

    #[test]
    fn add_task_with_role_evaluator() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 8,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "eval-task", "description": "Evaluate results", "role": "evaluator"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].role, AgentRole::Evaluator);
    }

    #[test]
    fn add_task_default_role_is_implementer() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 9,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "impl-task", "description": "Implement something"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].role, AgentRole::default());
    }

    #[test]
    fn add_task_with_unknown_role_returns_error() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 10,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "t", "description": "desc", "role": "bogus_role"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(text.contains("bogus_role"));
    }

    #[test]
    fn add_task_with_priority() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 11,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "high-prio", "description": "Critical fix", "priority": 10}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks[0].priority, 10);
    }

    #[test]
    fn add_task_with_acceptance_criteria() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 12,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {
                    "id": "impl-feature",
                    "description": "Implement the feature",
                    "acceptance_criteria": "All unit tests pass and the feature is documented"
                }
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(
            tasks[0].acceptance_criteria.as_deref(),
            Some("All unit tests pass and the feature is documented")
        );
    }

    #[test]
    fn add_task_default_priority_is_zero() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 13,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "normal-task", "description": "Normal task"}
            }
        })
        .to_string();
        process_message(&ctx, &req).unwrap();

        let tasks = task_manager::load_tasks(&task_file).unwrap();
        assert_eq!(tasks[0].priority, 0);
        assert!(tasks[0].acceptance_criteria.is_none());
    }

    #[test]
    fn add_task_schema_includes_priority_and_acceptance_criteria() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":14,"method":"tools/list","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        let add_task_tool = v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .find(|t| t["name"] == "add_task")
            .expect("add_task tool must exist");

        let props = &add_task_tool["inputSchema"]["properties"];
        assert!(props["priority"].is_object(), "priority property missing");
        assert!(
            props["acceptance_criteria"].is_object(),
            "acceptance_criteria property missing"
        );
    }

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

    #[test]
    fn update_task_status_task_not_found() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        // Try to update a task that doesn't exist in the (empty) task file.
        let req = json!({
            "jsonrpc": "2.0", "id": 23,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "nonexistent-task", "status": "completed"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("nonexistent-task"),
            "error message should mention the missing task id: {text}"
        );
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

    // ── ping ─────────────────────────────────────────────────────────────────

    #[test]
    fn ping_returns_empty_result() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":98,"method":"ping","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["id"], 98);
        assert_eq!(v["result"], json!({}));
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
        let lines = data_rows(text);
        assert_eq!(lines.len(), 1, "expected exactly one data row: {text}");
    }

    #[test]
    fn list_tasks_in_progress_filter() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Add two tasks.
        for id in &["task-x", "task-y"] {
            let add_req = json!({
                "jsonrpc": "2.0", "id": 53,
                "method": "tools/call",
                "params": {"name": "add_task", "arguments": {"id": id, "description": "work"}}
            })
            .to_string();
            process_message(&ctx, &add_req).unwrap();
        }
        // Mark 'task-x' as in-progress.
        let upd_req = json!({
            "jsonrpc": "2.0", "id": 54,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "task-x", "status": "in-progress"}
            }
        })
        .to_string();
        process_message(&ctx, &upd_req).unwrap();

        // Filter for in-progress.
        let list_req = json!({
            "jsonrpc": "2.0", "id": 55,
            "method": "tools/call",
            "params": {
                "name": "list_tasks",
                "arguments": {"status": "in-progress"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        // task-x (in-progress) should appear; task-y (pending) should not.
        let data_lines = data_rows(text);
        assert_eq!(
            data_lines.len(),
            1,
            "expected exactly one in-progress row: {text}"
        );
        assert!(text.contains("task-x"), "expected task-x in output: {text}");
        assert!(
            !text.contains("task-y"),
            "task-y should be filtered out: {text}"
        );
    }

    #[test]
    fn list_tasks_failed_filter() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Add two tasks.
        for id in &["ok-task", "bad-task"] {
            let add_req = json!({
                "jsonrpc": "2.0", "id": 56,
                "method": "tools/call",
                "params": {"name": "add_task", "arguments": {"id": id, "description": "desc"}}
            })
            .to_string();
            process_message(&ctx, &add_req).unwrap();
        }
        // Mark 'bad-task' as failed.
        let upd_req = json!({
            "jsonrpc": "2.0", "id": 57,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "bad-task", "status": "failed"}
            }
        })
        .to_string();
        process_message(&ctx, &upd_req).unwrap();

        // Filter for failed.
        let list_req = json!({
            "jsonrpc": "2.0", "id": 58,
            "method": "tools/call",
            "params": {
                "name": "list_tasks",
                "arguments": {"status": "failed"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        let data_lines = data_rows(text);
        assert_eq!(
            data_lines.len(),
            1,
            "expected exactly one failed row: {text}"
        );
        assert!(
            text.contains("bad-task"),
            "expected bad-task in output: {text}"
        );
        assert!(
            !text.contains("ok-task"),
            "ok-task should be filtered out: {text}"
        );
    }

    #[test]
    fn list_tasks_in_progress_underscore_alias() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let add_req = json!({
            "jsonrpc": "2.0", "id": 59,
            "method": "tools/call",
            "params": {"name": "add_task", "arguments": {"id": "alias-task", "description": "test"}}
        })
        .to_string();
        process_message(&ctx, &add_req).unwrap();

        // Set status to in-progress using hyphen form.
        let upd_req = json!({
            "jsonrpc": "2.0", "id": 60,
            "method": "tools/call",
            "params": {
                "name": "update_task_status",
                "arguments": {"id": "alias-task", "status": "in-progress"}
            }
        })
        .to_string();
        process_message(&ctx, &upd_req).unwrap();

        // Query using underscore alias (in_progress).
        let list_req = json!({
            "jsonrpc": "2.0", "id": 61,
            "method": "tools/call",
            "params": {
                "name": "list_tasks",
                "arguments": {"status": "in_progress"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &list_req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("alias-task"),
            "in_progress alias should match in-progress tasks: {text}"
        );
    }

    #[test]
    fn add_task_duplicate_id_returns_error() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        // Add a task successfully.
        let first = json!({
            "jsonrpc": "2.0", "id": 62,
            "method": "tools/call",
            "params": {"name": "add_task", "arguments": {"id": "dup-task", "description": "first"}}
        })
        .to_string();
        let resp = process_message(&ctx, &first).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));

        // Try to add a second task with the same ID.
        let second = json!({
            "jsonrpc": "2.0", "id": 63,
            "method": "tools/call",
            "params": {"name": "add_task", "arguments": {"id": "dup-task", "description": "duplicate"}}
        })
        .to_string();
        let resp = process_message(&ctx, &second).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert!(
            v["result"]["isError"].as_bool().unwrap_or(false),
            "adding a duplicate task ID should return an error"
        );
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("dup-task"),
            "error message should mention the duplicate id: {text}"
        );
    }

    // ── shell_quote ──────────────────────────────────────────────────────────

    #[test]
    fn shell_quote_plain_path() {
        assert_eq!(shell_quote("/path/to/tasks.json"), "/path/to/tasks.json");
    }

    #[test]
    fn shell_quote_path_with_spaces() {
        assert_eq!(
            shell_quote("/my projects/tasks.json"),
            "'/my projects/tasks.json'"
        );
    }

    #[test]
    fn shell_quote_path_with_single_quote() {
        assert_eq!(shell_quote("/it's/tasks.json"), "'/it'\\''s/tasks.json'");
    }

    #[test]
    fn shell_quote_dollar_sign() {
        assert_eq!(
            shell_quote("/path/$HOME/tasks.json"),
            "'/path/$HOME/tasks.json'"
        );
    }

    #[test]
    fn shell_quote_ampersand() {
        assert_eq!(
            shell_quote("/path/a&b/tasks.json"),
            "'/path/a&b/tasks.json'"
        );
    }

    #[test]
    fn shell_quote_semicolon() {
        assert_eq!(
            shell_quote("/path/a;b/tasks.json"),
            "'/path/a;b/tasks.json'"
        );
    }

    // ── trigger_iteration path formatting ────────────────────────────────────

    #[test]
    fn trigger_iteration_no_extra_quotes_for_plain_paths() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 41,
            "method": "tools/call",
            "params": {
                "name": "trigger_iteration",
                "arguments": {"headless": false}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        // Plain paths should not be wrapped in extra Rust debug quotes.
        assert!(!text.contains("\\\""), "unexpected escaped quotes: {text}");
        assert!(text.contains("wreck-it run"));
        assert!(!text.contains("--headless"));
    }

    // ── get_task ─────────────────────────────────────────────────────────────

    #[test]
    fn get_task_returns_full_details() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // Add a task.
        let add_req = json!({
            "jsonrpc": "2.0", "id": 70,
            "method": "tools/call",
            "params": {
                "name": "add_task",
                "arguments": {"id": "detail-task", "description": "A task with details"}
            }
        })
        .to_string();
        process_message(&ctx, &add_req).unwrap();

        // Retrieve it via get_task.
        let req = json!({
            "jsonrpc": "2.0", "id": 71,
            "method": "tools/call",
            "params": {
                "name": "get_task",
                "arguments": {"id": "detail-task"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("detail-task"),
            "id should appear in output: {text}"
        );
        assert!(
            text.contains("A task with details"),
            "full description should appear (no truncation): {text}"
        );
        assert!(!v["result"]["isError"].as_bool().unwrap_or(false));
    }

    #[test]
    fn get_task_missing_id_returns_error() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 72,
            "method": "tools/call",
            "params": {
                "name": "get_task",
                "arguments": {}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
    }

    #[test]
    fn get_task_not_found_returns_error() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 73,
            "method": "tools/call",
            "params": {
                "name": "get_task",
                "arguments": {"id": "ghost-task"}
            }
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(v["result"]["isError"], true);
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("ghost-task"),
            "error should mention the missing id: {text}"
        );
    }

    // ── list_artefacts ───────────────────────────────────────────────────────

    #[test]
    fn list_artefacts_empty() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 80,
            "method": "tools/call",
            "params": {"name": "list_artefacts", "arguments": {}}
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            text.contains("No artefacts"),
            "expected empty message: {text}"
        );
    }

    #[test]
    fn list_artefacts_returns_sorted_keys() {
        use crate::artefact_store::{save_manifest, ArtefactEntry, ArtefactManifest};
        use crate::types::ArtefactKind;

        let (dir, task_file) = setup_task_file();
        let manifest_path = dir.path().join(".wreck-it-artefacts.json");
        let mut manifest = ArtefactManifest::default();
        for key in &["z-task/output", "a-task/result", "m-task/data"] {
            manifest.artefacts.insert(
                key.to_string(),
                ArtefactEntry {
                    kind: ArtefactKind::File,
                    name: "output".to_string(),
                    path: "out.txt".to_string(),
                    content: "content".to_string(),
                },
            );
        }
        save_manifest(&manifest_path, &manifest).unwrap();

        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = json!({
            "jsonrpc": "2.0", "id": 81,
            "method": "tools/call",
            "params": {"name": "list_artefacts", "arguments": {}}
        })
        .to_string();
        let resp = process_message(&ctx, &req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();
        let text = v["result"]["content"][0]["text"].as_str().unwrap();
        // All three keys should appear.
        assert!(
            text.contains("a-task/result"),
            "missing a-task/result: {text}"
        );
        assert!(text.contains("m-task/data"), "missing m-task/data: {text}");
        assert!(
            text.contains("z-task/output"),
            "missing z-task/output: {text}"
        );
        // They should be in sorted order.
        let a_pos = text.find("a-task/result").unwrap();
        let m_pos = text.find("m-task/data").unwrap();
        let z_pos = text.find("z-task/output").unwrap();
        assert!(
            a_pos < m_pos && m_pos < z_pos,
            "keys should be sorted: {text}"
        );
    }

    // ── tools/list includes new tools ────────────────────────────────────────

    #[test]
    fn tools_list_contains_get_task_and_list_artefacts() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        let req = r#"{"jsonrpc":"2.0","id":90,"method":"tools/list","params":{}}"#;
        let resp = process_message(&ctx, req).unwrap();
        let v: Value = serde_json::from_str(&resp).unwrap();

        let tools = v["result"]["tools"].as_array().unwrap();
        let names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
        assert!(names.contains(&"get_task"), "missing get_task: {names:?}");
        assert!(
            names.contains(&"list_artefacts"),
            "missing list_artefacts: {names:?}"
        );
    }

    // ── notifications/initialized ────────────────────────────────────────────

    #[test]
    fn initialized_notification_produces_no_response() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file, dir.path().to_path_buf());

        // The `notifications/initialized` message from the client is a notification
        // (no `id` field). The server must not produce a response for notifications.
        let notif = r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#;
        let result = process_message(&ctx, notif);
        assert!(
            result.is_none(),
            "notifications should produce no response, got: {result:?}"
        );
    }

    // ── full MCP session lifecycle ────────────────────────────────────────────

    /// Exercise the complete MCP handshake and a realistic tool-call sequence:
    ///   initialize → notifications/initialized → tools/list → add_task
    ///   → list_tasks → update_task_status → get_task → trigger_iteration
    #[test]
    fn full_mcp_session_lifecycle() {
        let (dir, task_file) = setup_task_file();
        let ctx = make_ctx(task_file.clone(), dir.path().to_path_buf());

        // 1. initialize
        let init_resp = process_message(
            &ctx,
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}"#,
        )
        .unwrap();
        let init_v: Value = serde_json::from_str(&init_resp).unwrap();
        assert_eq!(init_v["result"]["protocolVersion"], MCP_PROTOCOL_VERSION);
        assert_eq!(init_v["result"]["serverInfo"]["name"], "wreck-it");

        // 2. notifications/initialized — must return nothing
        let notif_result = process_message(
            &ctx,
            r#"{"jsonrpc":"2.0","method":"notifications/initialized","params":{}}"#,
        );
        assert!(notif_result.is_none());

        // 3. tools/list — all 7 tools must be advertised
        let tlist_resp = process_message(
            &ctx,
            r#"{"jsonrpc":"2.0","id":2,"method":"tools/list","params":{}}"#,
        )
        .unwrap();
        let tlist_v: Value = serde_json::from_str(&tlist_resp).unwrap();
        let tool_names: Vec<&str> = tlist_v["result"]["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        for expected in &[
            "list_tasks",
            "get_task",
            "add_task",
            "update_task_status",
            "read_artefact",
            "list_artefacts",
            "trigger_iteration",
        ] {
            assert!(
                tool_names.contains(expected),
                "tool '{expected}' not advertised"
            );
        }

        // 4. add_task
        let add_resp = process_message(
            &ctx,
            &json!({
                "jsonrpc": "2.0", "id": 3,
                "method": "tools/call",
                "params": {
                    "name": "add_task",
                    "arguments": {
                        "id": "lifecycle-task",
                        "description": "Created during MCP session lifecycle test",
                        "role": "implementer"
                    }
                }
            })
            .to_string(),
        )
        .unwrap();
        let add_v: Value = serde_json::from_str(&add_resp).unwrap();
        assert!(
            !add_v["result"]["isError"].as_bool().unwrap_or(false),
            "add_task should succeed: {add_v}"
        );

        // 5. list_tasks — task must appear
        let list_resp = process_message(
            &ctx,
            &json!({
                "jsonrpc": "2.0", "id": 4,
                "method": "tools/call",
                "params": {"name": "list_tasks", "arguments": {}}
            })
            .to_string(),
        )
        .unwrap();
        let list_v: Value = serde_json::from_str(&list_resp).unwrap();
        let list_text = list_v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            list_text.contains("lifecycle-task"),
            "task should appear in list: {list_text}"
        );
        assert!(
            list_text.contains("pending"),
            "new task should have pending status: {list_text}"
        );

        // 6. update_task_status
        let upd_resp = process_message(
            &ctx,
            &json!({
                "jsonrpc": "2.0", "id": 5,
                "method": "tools/call",
                "params": {
                    "name": "update_task_status",
                    "arguments": {"id": "lifecycle-task", "status": "in-progress"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let upd_v: Value = serde_json::from_str(&upd_resp).unwrap();
        assert!(
            !upd_v["result"]["isError"].as_bool().unwrap_or(false),
            "update_task_status should succeed: {upd_v}"
        );

        // 7. get_task — verify updated status
        let get_resp = process_message(
            &ctx,
            &json!({
                "jsonrpc": "2.0", "id": 6,
                "method": "tools/call",
                "params": {
                    "name": "get_task",
                    "arguments": {"id": "lifecycle-task"}
                }
            })
            .to_string(),
        )
        .unwrap();
        let get_v: Value = serde_json::from_str(&get_resp).unwrap();
        let get_text = get_v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            get_text.contains("in-progress"),
            "get_task should reflect updated status: {get_text}"
        );

        // 8. trigger_iteration — must return a valid CLI command
        let trig_resp = process_message(
            &ctx,
            &json!({
                "jsonrpc": "2.0", "id": 7,
                "method": "tools/call",
                "params": {
                    "name": "trigger_iteration",
                    "arguments": {"headless": true}
                }
            })
            .to_string(),
        )
        .unwrap();
        let trig_v: Value = serde_json::from_str(&trig_resp).unwrap();
        let trig_text = trig_v["result"]["content"][0]["text"].as_str().unwrap();
        assert!(
            trig_text.contains("wreck-it run"),
            "trigger_iteration should return a CLI command: {trig_text}"
        );
        assert!(
            trig_text.contains("--headless"),
            "headless=true should add --headless flag: {trig_text}"
        );
    }
}
