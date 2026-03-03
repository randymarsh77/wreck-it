//! C-compatible FFI layer for the wreck-it project management API.
//!
//! Every public function in this module is `extern "C"` and designed to be
//! called from Swift (or any other C-ABI consumer).  Data is exchanged as
//! JSON-encoded C strings to keep the interface simple and avoid manual
//! struct layout matching.
//!
//! # Memory contract
//!
//! * **Strings returned by `wreck_it_*` functions** are allocated by Rust.
//!   The caller must free them with [`wreck_it_free_string`].
//! * **Strings passed *into* `wreck_it_*` functions** are borrowed — Rust
//!   does **not** free them.

use crate::project_api::{ProjectManager, TaskUpdate};
use crate::types::TaskStatus;
use std::ffi::{CStr, CString};
use std::os::raw::c_char;

// ── Helpers ─────────────────────────────────────────────────────────

/// Convert a raw C string pointer to a `&str`.  Returns `None` for null
/// pointers or invalid UTF-8.
unsafe fn cstr_to_str<'a>(ptr: *const c_char) -> Option<&'a str> {
    if ptr.is_null() {
        return None;
    }
    // SAFETY: caller guarantees `ptr` is a valid, NUL-terminated C string.
    unsafe { CStr::from_ptr(ptr) }.to_str().ok()
}

/// Wrap a `Result<String>` into a heap-allocated C string (caller must free).
fn result_to_cstring(res: anyhow::Result<String>) -> *mut c_char {
    match res {
        Ok(json) => CString::new(json).unwrap_or_default().into_raw(),
        Err(e) => {
            let err_json = serde_json::json!({ "error": e.to_string() }).to_string();
            CString::new(err_json).unwrap_or_default().into_raw()
        }
    }
}

// ── Public FFI functions ────────────────────────────────────────────

/// Free a string previously returned by a `wreck_it_*` function.
///
/// # Safety
///
/// `ptr` must have been obtained from a prior `wreck_it_*` call and must
/// not be used after this function returns.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_free_string(ptr: *mut c_char) {
    if !ptr.is_null() {
        // SAFETY: `ptr` was created by `CString::into_raw` inside this module.
        drop(unsafe { CString::from_raw(ptr) });
    }
}

/// List all tasks.  Returns a JSON array.
///
/// # Safety
///
/// `task_file` must be a valid, NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_list_tasks(task_file: *const c_char) -> *mut c_char {
    let path = match unsafe { cstr_to_str(task_file) } {
        Some(s) => s,
        None => return result_to_cstring(Err(anyhow::anyhow!("null task_file pointer"))),
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(pm.list_tasks().and_then(|t| Ok(serde_json::to_string(&t)?)))
}

/// Get a single task by id.  Returns a JSON object or `{"error":…}`.
///
/// # Safety
///
/// Both pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_get_task(
    task_file: *const c_char,
    id: *const c_char,
) -> *mut c_char {
    let (path, id) = match (unsafe { cstr_to_str(task_file) }, unsafe { cstr_to_str(id) }) {
        (Some(p), Some(i)) => (p, i),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(pm.get_task(id).and_then(|t| Ok(serde_json::to_string(&t)?)))
}

/// List epics (top-level tasks with children).  Returns a JSON array.
///
/// # Safety
///
/// `task_file` must be a valid, NUL-terminated C string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_list_epics(task_file: *const c_char) -> *mut c_char {
    let path = match unsafe { cstr_to_str(task_file) } {
        Some(s) => s,
        None => return result_to_cstring(Err(anyhow::anyhow!("null task_file pointer"))),
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(pm.list_epics().and_then(|t| Ok(serde_json::to_string(&t)?)))
}

/// List sub-tasks of a given parent.  Returns a JSON array.
///
/// # Safety
///
/// Both pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_list_sub_tasks(
    task_file: *const c_char,
    parent_id: *const c_char,
) -> *mut c_char {
    let (path, pid) = match (
        unsafe { cstr_to_str(task_file) },
        unsafe { cstr_to_str(parent_id) },
    ) {
        (Some(p), Some(i)) => (p, i),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(
        pm.list_sub_tasks(pid)
            .and_then(|t| Ok(serde_json::to_string(&t)?)),
    )
}

/// Create a top-level task.  `labels_json` is a JSON array of strings
/// (pass `"[]"` for none).  Returns the created task as JSON.
///
/// # Safety
///
/// All pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_create_task(
    task_file: *const c_char,
    id: *const c_char,
    description: *const c_char,
    labels_json: *const c_char,
) -> *mut c_char {
    let (path, id, desc, labels_raw) = match (
        unsafe { cstr_to_str(task_file) },
        unsafe { cstr_to_str(id) },
        unsafe { cstr_to_str(description) },
        unsafe { cstr_to_str(labels_json) },
    ) {
        (Some(a), Some(b), Some(c), Some(d)) => (a, b, c, d),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let labels: Vec<String> = serde_json::from_str(labels_raw).unwrap_or_default();
    let pm = ProjectManager::new(path);
    result_to_cstring(
        pm.create_task(id, desc, labels)
            .and_then(|t| Ok(serde_json::to_string(&t)?)),
    )
}

/// Create a sub-task under a parent.  Returns the created task as JSON.
///
/// # Safety
///
/// All pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_create_sub_task(
    task_file: *const c_char,
    id: *const c_char,
    parent_id: *const c_char,
    description: *const c_char,
    labels_json: *const c_char,
) -> *mut c_char {
    let (path, id, pid, desc, labels_raw) = match (
        unsafe { cstr_to_str(task_file) },
        unsafe { cstr_to_str(id) },
        unsafe { cstr_to_str(parent_id) },
        unsafe { cstr_to_str(description) },
        unsafe { cstr_to_str(labels_json) },
    ) {
        (Some(a), Some(b), Some(c), Some(d), Some(e)) => (a, b, c, d, e),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let labels: Vec<String> = serde_json::from_str(labels_raw).unwrap_or_default();
    let pm = ProjectManager::new(path);
    result_to_cstring(
        pm.create_sub_task(id, pid, desc, labels)
            .and_then(|t| Ok(serde_json::to_string(&t)?)),
    )
}

/// Update a task.  `update_json` is a JSON object with optional fields:
/// `description`, `status`, `parent_id`, `labels`, `priority`,
/// `complexity`, `phase`, `depends_on`.  Returns the updated task.
///
/// # Safety
///
/// All pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_update_task(
    task_file: *const c_char,
    id: *const c_char,
    update_json: *const c_char,
) -> *mut c_char {
    let (path, id, upd_raw) = match (
        unsafe { cstr_to_str(task_file) },
        unsafe { cstr_to_str(id) },
        unsafe { cstr_to_str(update_json) },
    ) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };

    let parsed: serde_json::Value = match serde_json::from_str(upd_raw) {
        Ok(v) => v,
        Err(e) => return result_to_cstring(Err(anyhow::anyhow!("invalid update JSON: {}", e))),
    };

    let update = TaskUpdate {
        description: parsed
            .get("description")
            .and_then(|v| v.as_str())
            .map(String::from),
        status: parsed
            .get("status")
            .and_then(|v| v.as_str())
            .and_then(|s| serde_json::from_value(serde_json::Value::String(s.to_string())).ok()),
        parent_id: parsed.get("parent_id").map(|v| {
            if v.is_null() {
                None
            } else {
                v.as_str().map(String::from)
            }
        }),
        labels: parsed.get("labels").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        }),
        priority: parsed
            .get("priority")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        complexity: parsed
            .get("complexity")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        phase: parsed
            .get("phase")
            .and_then(|v| v.as_u64())
            .map(|v| v as u32),
        depends_on: parsed.get("depends_on").and_then(|v| {
            v.as_array().map(|arr| {
                arr.iter()
                    .filter_map(|x| x.as_str().map(String::from))
                    .collect()
            })
        }),
    };

    let pm = ProjectManager::new(path);
    result_to_cstring(
        pm.update_task(id, update)
            .and_then(|t| Ok(serde_json::to_string(&t)?)),
    )
}

/// Delete a task (cascades to sub-tasks).  Returns `{"ok":true}` or
/// `{"error":…}`.
///
/// # Safety
///
/// Both pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_delete_task(
    task_file: *const c_char,
    id: *const c_char,
) -> *mut c_char {
    let (path, id) = match (unsafe { cstr_to_str(task_file) }, unsafe { cstr_to_str(id) }) {
        (Some(p), Some(i)) => (p, i),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(pm.delete_task(id).map(|()| r#"{"ok":true}"#.to_string()))
}

/// Move a task to a new status.  `status` must be one of `"pending"`,
/// `"inprogress"`, `"completed"`, `"failed"`.  Returns the updated task.
///
/// # Safety
///
/// All pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_move_task(
    task_file: *const c_char,
    id: *const c_char,
    status: *const c_char,
) -> *mut c_char {
    let (path, id, st) = match (
        unsafe { cstr_to_str(task_file) },
        unsafe { cstr_to_str(id) },
        unsafe { cstr_to_str(status) },
    ) {
        (Some(a), Some(b), Some(c)) => (a, b, c),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let new_status: TaskStatus = match serde_json::from_value(serde_json::Value::String(
        st.to_string(),
    )) {
        Ok(s) => s,
        Err(_) => {
            return result_to_cstring(Err(anyhow::anyhow!(
                "invalid status '{}': expected pending, inprogress, completed, or failed",
                st
            )))
        }
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(
        pm.move_task(id, new_status)
            .and_then(|t| Ok(serde_json::to_string(&t)?)),
    )
}

/// Return the progress of an epic as a JSON object `{"progress":0.5}`
/// or `{"progress":null}` if the epic has no sub-tasks.
///
/// # Safety
///
/// Both pointers must be valid, NUL-terminated C strings.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn wreck_it_epic_progress(
    task_file: *const c_char,
    epic_id: *const c_char,
) -> *mut c_char {
    let (path, id) = match (unsafe { cstr_to_str(task_file) }, unsafe { cstr_to_str(epic_id) }) {
        (Some(p), Some(i)) => (p, i),
        _ => return result_to_cstring(Err(anyhow::anyhow!("null pointer argument"))),
    };
    let pm = ProjectManager::new(path);
    result_to_cstring(pm.epic_progress(id).map(|p| {
        serde_json::json!({ "progress": p }).to_string()
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::CString;
    use tempfile::tempdir;

    fn c(s: &str) -> CString {
        CString::new(s).unwrap()
    }

    fn setup_file() -> (tempfile::TempDir, CString) {
        let dir = tempdir().unwrap();
        let path = dir.path().join("tasks.json");
        std::fs::write(&path, "[]").unwrap();
        let cs = CString::new(path.to_str().unwrap()).unwrap();
        (dir, cs)
    }

    fn read_result(ptr: *mut c_char) -> String {
        let s = unsafe { CStr::from_ptr(ptr) }
            .to_str()
            .unwrap()
            .to_string();
        unsafe { wreck_it_free_string(ptr) };
        s
    }

    #[test]
    fn ffi_create_and_list() {
        let (_dir, path) = setup_file();
        let id = c("t1");
        let desc = c("My task");
        let labels = c("[]");

        let res = unsafe {
            wreck_it_create_task(path.as_ptr(), id.as_ptr(), desc.as_ptr(), labels.as_ptr())
        };
        let json = read_result(res);
        assert!(json.contains("\"id\":\"t1\""));

        let res = unsafe { wreck_it_list_tasks(path.as_ptr()) };
        let json = read_result(res);
        let tasks: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert_eq!(tasks.len(), 1);
    }

    #[test]
    fn ffi_move_task() {
        let (_dir, path) = setup_file();
        let id = c("t1");
        let desc = c("Task");
        let labels = c("[]");
        unsafe {
            wreck_it_free_string(wreck_it_create_task(
                path.as_ptr(),
                id.as_ptr(),
                desc.as_ptr(),
                labels.as_ptr(),
            ));
        }

        let status = c("completed");
        let res = unsafe { wreck_it_move_task(path.as_ptr(), id.as_ptr(), status.as_ptr()) };
        let json = read_result(res);
        assert!(json.contains("\"status\":\"completed\""));
    }

    #[test]
    fn ffi_delete_task() {
        let (_dir, path) = setup_file();
        let id = c("t1");
        let desc = c("Task");
        let labels = c("[]");
        unsafe {
            wreck_it_free_string(wreck_it_create_task(
                path.as_ptr(),
                id.as_ptr(),
                desc.as_ptr(),
                labels.as_ptr(),
            ));
        }

        let res = unsafe { wreck_it_delete_task(path.as_ptr(), id.as_ptr()) };
        let json = read_result(res);
        assert!(json.contains("\"ok\":true"));

        let res = unsafe { wreck_it_list_tasks(path.as_ptr()) };
        let json = read_result(res);
        let tasks: Vec<serde_json::Value> = serde_json::from_str(&json).unwrap();
        assert!(tasks.is_empty());
    }

    #[test]
    fn ffi_null_pointer_returns_error() {
        let res = unsafe { wreck_it_list_tasks(std::ptr::null()) };
        let json = read_result(res);
        assert!(json.contains("\"error\""));
    }
}
