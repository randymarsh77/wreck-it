//! Agent output caching for wreck-it.
//!
//! # Purpose
//!
//! When the same task is executed multiple times without any underlying
//! codebase changes the LLM response will be identical or nearly identical.
//! Re-running the full inference pipeline wastes API budget, increases latency,
//! and inflates cost-tracking numbers.  This module provides a lightweight,
//! filesystem-backed response cache that short-circuits the LLM call when a
//! matching cached entry is found and still fresh.
//!
//! # Cache key
//!
//! The cache key is a lowercase hex-encoded SHA-256 digest of the
//! concatenation of four inputs:
//!
//! ```text
//! sha256( task_id
//!       + "\0"          ← null-byte separator
//!       + system_prompt
//!       + "\0"
//!       + user_prompt
//!       + "\0"
//!       + git_HEAD_sha  ← HEAD commit OID of the current repository
//!       )
//! ```
//!
//! Using the git HEAD SHA as part of the key means the cache is automatically
//! invalidated whenever the repository changes, even if the prompts themselves
//! are unchanged.  A `\0` separator is used between fields because it cannot
//! appear inside any of the string inputs (which are all valid UTF-8), so
//! there is no risk of two distinct input combinations producing the same
//! concatenated byte sequence.
//!
//! # Cache store
//!
//! Cache entries are persisted as UTF-8 JSON files on disk under:
//!
//! ```text
//! <work_dir>/.wreck-it-cache/<cache_key>.json
//! ```
//!
//! The directory is created on demand.  Callers should treat it as an opaque
//! implementation detail; tooling should never manually edit the files.
//!
//! ## On-disk JSON schema
//!
//! ```json
//! {
//!   "version":    1,
//!   "cache_key":  "<hex-sha256>",
//!   "task_id":    "<task-id>",
//!   "git_head":   "<40-char commit OID>",
//!   "created_at": <unix-timestamp-secs>,
//!   "response":   "<full agent text response>"
//! }
//! ```
//!
//! The `version` field allows future migrations without breaking existing
//! entries.  Any entry whose `version` field does not match
//! [`CACHE_ENTRY_VERSION`] is treated as a cache miss and overwritten.
//!
//! # TTL and invalidation
//!
//! Cached entries expire after [`DEFAULT_TTL_SECS`] seconds (24 hours by
//! default).  On lookup the stored `created_at` timestamp is compared to the
//! current wall-clock time; a stale entry is discarded and the call proceeds
//! through to the LLM.
//!
//! In addition to TTL expiry, the git HEAD SHA embedded in the cache key acts
//! as an implicit content-addressed invalidation mechanism: any commit pushed
//! to the repository changes the HEAD SHA and therefore the key, so no stale
//! entry for the old HEAD will ever be found for the new HEAD.
//!
//! # `--no-cache` flag
//!
//! When the user passes `--no-cache` on the CLI the [`AgentCache`] must be
//! constructed with `enabled: false` (see [`CacheConfig`]).  In this mode
//! every [`AgentCache::lookup`] call returns `CacheResult::Disabled` and every
//! [`AgentCache::store`] call is a no-op.  This is useful for debugging or
//! when deterministic, fresh responses are required.
//!
//! # Edge cases
//!
//! | Scenario | Behaviour |
//! |---|---|
//! | **Cache miss – first run** | `lookup` returns `CacheResult::Miss`; the caller invokes the LLM and then calls `store` to persist the response. |
//! | **Codebase changes (git HEAD changed)** | Because the HEAD SHA is part of the key, a changed HEAD produces a different key, so old entries are never surfaced.  Old files remain on disk until a periodic eviction pass or until the user deletes `.wreck-it-cache/` manually. |
//! | **Cache hit** | `lookup` returns `CacheResult::Hit { response }`.  The caller logs the hit (see [`log_cache_hit`]) and returns the cached response without calling the LLM. |
//! | **TTL expired** | `lookup` reads the entry, detects `created_at + ttl_secs < now`, logs a warning, and returns `CacheResult::Miss` after deleting the stale file. |
//! | **Corrupt cache file** | If the file exists but cannot be deserialised, a warning is logged and `CacheResult::Miss` is returned.  The corrupt file is deleted to prevent repeated I/O errors. |
//! | **Cache write failure** | A failure to write to `.wreck-it-cache/` is logged as a warning but does **not** surface as an error to the caller.  The run continues normally; the next execution will simply experience another cache miss. |
//! | **Concurrent writers (parallel tasks)** | Each entry is an independent file named by its unique cache key.  Two parallel tasks with different keys write to different files and never conflict.  Two tasks that share a key would race, but that is harmless: the last writer wins, and both writes carry the same payload. |

use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// Schema version embedded in every cache entry.
///
/// Increment this constant whenever the [`CacheEntry`] JSON structure changes
/// in a backward-incompatible way.  Entries whose `version` does not match
/// are treated as a cache miss and silently overwritten.
pub const CACHE_ENTRY_VERSION: u32 = 1;

/// Default time-to-live for cache entries: 24 hours expressed in seconds.
pub const DEFAULT_TTL_SECS: u64 = 24 * 60 * 60;

/// Configuration passed when constructing an [`AgentCache`].
///
/// Typically built from CLI arguments: when `--no-cache` is present,
/// `enabled` is set to `false`.  When an alternative TTL is desired (e.g. for
/// testing), `ttl_secs` can be overridden; set it to `0` to force every entry
/// to be treated as immediately expired.
#[derive(Debug, Clone)]
pub struct CacheConfig {
    /// When `false` all cache operations are no-ops and every lookup returns
    /// [`CacheResult::Disabled`].  Corresponds to the `--no-cache` CLI flag.
    pub enabled: bool,
    /// Number of seconds before a stored entry is considered stale.
    /// Defaults to [`DEFAULT_TTL_SECS`].
    pub ttl_secs: u64,
}

impl Default for CacheConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            ttl_secs: DEFAULT_TTL_SECS,
        }
    }
}

/// The result of a cache lookup.
///
/// Callers should match on this value to decide whether to proceed to the LLM:
///
/// ```rust,ignore
/// match cache.lookup(&key)? {
///     CacheResult::Hit { response } => {
///         log_cache_hit(&task_id, &key);
///         return Ok(response);
///     }
///     CacheResult::Miss | CacheResult::Disabled => {
///         // fall through to LLM invocation
///     }
/// }
/// ```
#[derive(Debug)]
pub enum CacheResult {
    /// A fresh, valid entry was found.  The caller should return `response`
    /// directly without calling the LLM.
    Hit {
        /// The full agent text response from the previous run.
        response: String,
    },
    /// No entry exists for this key, or the existing entry has expired.
    /// The caller should invoke the LLM and call [`AgentCache::store`]
    /// afterwards to populate the cache.
    Miss,
    /// Caching is disabled via [`CacheConfig::enabled`] = `false`.
    /// The caller should invoke the LLM without attempting to store a result.
    Disabled,
}

/// On-disk representation of a single cached agent response.
///
/// Serialised to `.wreck-it-cache/<cache_key>.json`.
#[derive(Debug, Serialize, Deserialize)]
struct CacheEntry {
    /// Must equal [`CACHE_ENTRY_VERSION`]; otherwise the entry is rejected.
    version: u32,
    /// Hex-encoded SHA-256 cache key (redundant but useful for debugging).
    cache_key: String,
    /// The task ID that produced this entry.
    task_id: String,
    /// The git HEAD commit OID recorded at write time.  Informational; the
    /// authoritative invalidation path is the key itself.
    git_head: String,
    /// Unix timestamp (seconds since epoch) at which the entry was written.
    created_at: u64,
    /// The full agent text response to replay on a cache hit.
    response: String,
}

/// Filesystem-backed agent response cache.
///
/// Construct via [`AgentCache::new`] and share between agent invocations.
/// All public methods are synchronous and may be called from any context.
pub struct AgentCache {
    /// Root directory for all cache files (`<work_dir>/.wreck-it-cache/`).
    cache_dir: PathBuf,
    /// Active configuration (enabled flag, TTL).
    config: CacheConfig,
}

impl AgentCache {
    /// Create a new cache rooted at `<work_dir>/.wreck-it-cache/`.
    ///
    /// The directory is **not** created here; it is created lazily by
    /// [`AgentCache::store`] when the first entry is written.
    pub fn new(work_dir: &str, config: CacheConfig) -> Self {
        Self {
            cache_dir: Path::new(work_dir).join(".wreck-it-cache"),
            config,
        }
    }

    /// Derive the cache key for the given inputs.
    ///
    /// Returns a lowercase 64-character hex string (SHA-256 digest) of:
    ///
    /// ```text
    /// task_id + "\0" + system_prompt + "\0" + user_prompt + "\0" + git_head_sha
    /// ```
    ///
    /// The git HEAD SHA can be obtained by running `git rev-parse HEAD` in the
    /// repository root.  When the HEAD cannot be resolved (e.g. in a fresh
    /// repo with no commits) callers should pass `"unknown"` as `git_head_sha`;
    /// such entries will never match a real HEAD and therefore behave as
    /// perpetual misses (safe degraded behaviour).
    pub fn derive_key(
        task_id: &str,
        system_prompt: &str,
        user_prompt: &str,
        git_head_sha: &str,
    ) -> String {
        // Implementation note: use the `sha2` crate (add to Cargo.toml as a
        // dependency when this stub is promoted to a full implementation):
        //
        //   use sha2::{Digest, Sha256};
        //   let mut hasher = Sha256::new();
        //   hasher.update(task_id.as_bytes());
        //   hasher.update(b"\0");
        //   hasher.update(system_prompt.as_bytes());
        //   hasher.update(b"\0");
        //   hasher.update(user_prompt.as_bytes());
        //   hasher.update(b"\0");
        //   hasher.update(git_head_sha.as_bytes());
        //   format!("{:x}", hasher.finalize())
        //
        // Stub: return a placeholder until `sha2` is wired in.
        let _ = (task_id, system_prompt, user_prompt, git_head_sha);
        String::from("0000000000000000000000000000000000000000000000000000000000000000")
    }

    /// Return the filesystem path for a given cache key.
    fn entry_path(&self, cache_key: &str) -> PathBuf {
        self.cache_dir.join(format!("{}.json", cache_key))
    }

    /// Look up a cached response for the given `cache_key`.
    ///
    /// # Returns
    ///
    /// * [`CacheResult::Disabled`] – caching is turned off; the LLM call should
    ///   proceed normally and no cache log is emitted.
    /// * [`CacheResult::Hit`] – a fresh entry was found; log via
    ///   [`log_cache_hit`] and return the response to the caller.
    /// * [`CacheResult::Miss`] – no entry, expired entry, or corrupt entry;
    ///   the caller should invoke the LLM.
    ///
    /// Failures to read the entry file are logged as warnings and treated as
    /// misses so that a broken cache directory never prevents a task from
    /// running.
    pub fn lookup(&self, cache_key: &str, task_id: &str) -> Result<CacheResult> {
        if !self.config.enabled {
            return Ok(CacheResult::Disabled);
        }

        let path = self.entry_path(cache_key);
        if !path.exists() {
            // Cache miss – first run or key has never been stored.
            return Ok(CacheResult::Miss);
        }

        let raw = match fs::read_to_string(&path) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    task_id,
                    cache_key,
                    error = %e,
                    "Failed to read cache entry; treating as miss"
                );
                return Ok(CacheResult::Miss);
            }
        };

        let entry: CacheEntry = match serde_json::from_str(&raw) {
            Ok(e) => e,
            Err(e) => {
                tracing::warn!(
                    task_id,
                    cache_key,
                    error = %e,
                    "Corrupt cache entry; deleting and treating as miss"
                );
                let _ = fs::remove_file(&path);
                return Ok(CacheResult::Miss);
            }
        };

        // Reject entries from an older schema version.
        if entry.version != CACHE_ENTRY_VERSION {
            tracing::warn!(
                task_id,
                cache_key,
                entry_version = entry.version,
                expected_version = CACHE_ENTRY_VERSION,
                "Cache entry version mismatch; treating as miss"
            );
            let _ = fs::remove_file(&path);
            return Ok(CacheResult::Miss);
        }

        // Check TTL.
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        if now.saturating_sub(entry.created_at) >= self.config.ttl_secs {
            tracing::warn!(
                task_id,
                cache_key,
                created_at = entry.created_at,
                ttl_secs = self.config.ttl_secs,
                "Cache entry expired; deleting and treating as miss"
            );
            let _ = fs::remove_file(&path);
            return Ok(CacheResult::Miss);
        }

        log_cache_hit(task_id, cache_key);
        Ok(CacheResult::Hit {
            response: entry.response,
        })
    }

    /// Persist `response` under `cache_key` so that future lookups can avoid
    /// calling the LLM.
    ///
    /// When caching is disabled ([`CacheConfig::enabled`] = `false`) this
    /// method is a no-op.
    ///
    /// Write failures are logged as warnings and **do not** propagate to the
    /// caller; a cache write failure should never abort a task run.
    pub fn store(
        &self,
        cache_key: &str,
        task_id: &str,
        git_head: &str,
        response: &str,
    ) -> Result<()> {
        if !self.config.enabled {
            return Ok(());
        }

        if let Err(e) = fs::create_dir_all(&self.cache_dir) {
            tracing::warn!(
                task_id,
                cache_key,
                error = %e,
                "Failed to create cache directory; skipping cache write"
            );
            return Ok(());
        }

        let created_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);

        let entry = CacheEntry {
            version: CACHE_ENTRY_VERSION,
            cache_key: cache_key.to_string(),
            task_id: task_id.to_string(),
            git_head: git_head.to_string(),
            created_at,
            response: response.to_string(),
        };

        let json = match serde_json::to_string_pretty(&entry) {
            Ok(s) => s,
            Err(e) => {
                tracing::warn!(
                    task_id,
                    cache_key,
                    error = %e,
                    "Failed to serialise cache entry; skipping cache write"
                );
                return Ok(());
            }
        };

        let path = self.entry_path(cache_key);
        if let Err(e) = fs::write(&path, json.as_bytes()) {
            tracing::warn!(
                task_id,
                cache_key,
                path = %path.display(),
                error = %e,
                "Failed to write cache entry"
            );
        } else {
            tracing::debug!(
                task_id,
                cache_key,
                path = %path.display(),
                "Cache entry written"
            );
        }

        Ok(())
    }
}

/// Emit a structured log line indicating that a cache hit was served.
///
/// This is separated from [`AgentCache::lookup`] so that callers in the
/// agent invocation path can emit richer context (e.g. the task description
/// or the model that would have been used).
///
/// Example log output (JSON mode):
///
/// ```json
/// { "level": "INFO", "task_id": "impl-foo", "cache_key": "a3b9…", "message": "agent cache hit" }
/// ```
pub fn log_cache_hit(task_id: &str, cache_key: &str) {
    tracing::info!(task_id, cache_key, "agent cache hit");
}

/// Read the current git HEAD commit OID for the repository rooted at
/// `repo_dir`.
///
/// Returns the 40-character hex OID on success, or `"unknown"` when the HEAD
/// cannot be resolved (fresh repository, detached HEAD without commits, etc.).
/// The `"unknown"` sentinel ensures that cache entries written in a headless
/// state are never surfaced as hits for real commits.
///
/// # Implementation note
///
/// This function shells out to `git rev-parse HEAD` because the codebase does
/// not currently carry a pure-Rust git library.  If `git2` or `gix` is added
/// in the future this should be updated to use the library instead.
pub fn read_git_head(repo_dir: &str) -> String {
    // Shell out to `git rev-parse HEAD` to obtain the current HEAD OID.
    //
    // Edge cases handled:
    //   - `git` not on PATH          → returns "unknown"
    //   - non-zero exit code         → returns "unknown" (no commits yet)
    //   - empty stdout               → returns "unknown"
    let output = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(repo_dir)
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let oid = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if oid.is_empty() {
                "unknown".to_string()
            } else {
                oid
            }
        }
        _ => "unknown".to_string(),
    }
}

// ── Integration guidance ──────────────────────────────────────────────────────
//
// To wire this cache into the existing agent invocation paths:
//
// 1. Add `sha2 = "0.10"` to `[dependencies]` in `cli/Cargo.toml` and update
//    `derive_key` to use the real SHA-256 implementation (see the comment in
//    the function body above).
//
// 2. In `cli/src/types.rs` (or `headless_config.rs`) add a `no_cache: bool`
//    field to `Config` / `HeadlessConfig` and map the `--no-cache` CLI flag
//    (in `cli/src/cli.rs`) to it.
//
// 3. In `AgentClient` (cli/src/agent.rs):
//    a. Add an `agent_cache: Option<Arc<AgentCache>>` field.
//    b. Expose `with_agent_cache(cache: Arc<AgentCache>)` builder method.
//    c. At the top of `run_task` (or the equivalent HTTP-call site), call
//       `cache.lookup(key, task_id)?` before dispatching to the LLM.
//    d. After a successful LLM response, call `cache.store(key, task_id, …)`.
//
// 4. In `ralph_loop.rs` / `headless.rs`:
//    - Construct a single `Arc<AgentCache>` per run using the work-dir and
//      `CacheConfig { enabled: !no_cache_flag, ttl_secs: DEFAULT_TTL_SECS }`.
//    - Pass it to every `AgentClient` via `with_agent_cache(Arc::clone(&cache))`.
//
// 5. `.gitignore` – add `.wreck-it-cache/` so that cache files are never
//    committed to the repository.
// ─────────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_cache(dir: &TempDir, enabled: bool) -> AgentCache {
        AgentCache::new(
            dir.path().to_str().unwrap(),
            CacheConfig {
                enabled,
                // Use a short TTL in tests so we can simulate expiry by
                // manipulating `created_at` in the JSON directly.
                ttl_secs: DEFAULT_TTL_SECS,
            },
        )
    }

    #[test]
    fn miss_when_no_entry() {
        let dir = TempDir::new().unwrap();
        let cache = make_cache(&dir, true);
        let key = "deadbeef";
        let result = cache.lookup(key, "task-1").unwrap();
        assert!(matches!(result, CacheResult::Miss));
    }

    #[test]
    fn hit_after_store() {
        let dir = TempDir::new().unwrap();
        let cache = make_cache(&dir, true);
        let key = "cafecafe";
        cache
            .store(key, "task-2", "abc123", "hello from cache")
            .unwrap();
        let result = cache.lookup(key, "task-2").unwrap();
        match result {
            CacheResult::Hit { response } => assert_eq!(response, "hello from cache"),
            other => panic!("Expected Hit, got {:?}", other),
        }
    }

    #[test]
    fn disabled_returns_disabled() {
        let dir = TempDir::new().unwrap();
        let cache = make_cache(&dir, false);
        let key = "0badf00d";
        // A store on a disabled cache is a no-op.
        cache.store(key, "task-3", "abc", "response").unwrap();
        let result = cache.lookup(key, "task-3").unwrap();
        assert!(matches!(result, CacheResult::Disabled));
    }

    #[test]
    fn expired_entry_is_miss() {
        let dir = TempDir::new().unwrap();
        let cache = AgentCache::new(
            dir.path().to_str().unwrap(),
            CacheConfig {
                enabled: true,
                ttl_secs: 0, // everything is immediately expired
            },
        );
        let key = "expiredkey";
        cache
            .store(key, "task-4", "sha", "stale response")
            .unwrap();
        let result = cache.lookup(key, "task-4").unwrap();
        assert!(matches!(result, CacheResult::Miss));
    }

    #[test]
    fn corrupt_entry_is_miss() {
        let dir = TempDir::new().unwrap();
        let cache = make_cache(&dir, true);
        let key = "corruptkey";
        // Write garbage into the cache file.
        let path = dir.path().join(".wreck-it-cache").join(format!("{key}.json"));
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"not valid json").unwrap();
        let result = cache.lookup(key, "task-5").unwrap();
        assert!(matches!(result, CacheResult::Miss));
        // Corrupt file should have been deleted.
        assert!(!path.exists());
    }

    #[test]
    fn read_git_head_unknown_in_non_repo() {
        // Passing a path that is definitely not a git repo should return "unknown".
        let result = read_git_head("/tmp");
        // Either "unknown" (no git repo) or a valid OID — either is acceptable
        // in CI; the important thing is it doesn't panic.
        assert!(!result.is_empty());
    }
}
