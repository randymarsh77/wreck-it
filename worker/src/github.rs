//! GitHub API client for reading and writing repository contents.
//!
//! Uses the Cloudflare Worker Fetch API (via the `worker` crate) to
//! communicate with the GitHub REST API.  This avoids pulling in `reqwest`
//! or other HTTP clients that do not compile to WASM.

use serde::{Deserialize, Serialize};
use worker::Fetch;

/// A lightweight GitHub API client scoped to a single repository.
pub struct GitHubClient {
    owner: String,
    repo: String,
    token: String,
}

// ---------------------------------------------------------------------------
// GitHub REST API response types
// ---------------------------------------------------------------------------

/// Represents a file returned by the Contents API.
#[derive(Debug, Deserialize)]
pub struct ContentFile {
    /// Base64-encoded file content (present for files, absent for dirs).
    pub content: Option<String>,
    /// Git blob SHA — required when updating an existing file.
    pub sha: String,
}

/// Payload sent to the Contents API to create or update a file.
#[derive(Serialize)]
struct UpsertFileRequest<'a> {
    message: &'a str,
    content: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    sha: Option<&'a str>,
    branch: &'a str,
}

/// Represents a Git reference (branch pointer).
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitRef {
    #[serde(rename = "ref")]
    pub ref_name: String,
    pub object: GitRefObject,
}

#[derive(Debug, Deserialize)]
#[allow(dead_code)]
pub struct GitRefObject {
    pub sha: String,
}

// ---------------------------------------------------------------------------
// Implementation
// ---------------------------------------------------------------------------

impl GitHubClient {
    /// Create a new client for the given repository.
    pub fn new(
        owner: impl Into<String>,
        repo: impl Into<String>,
        token: impl Into<String>,
    ) -> Self {
        Self {
            owner: owner.into(),
            repo: repo.into(),
            token: token.into(),
        }
    }

    /// Read a file from the repository.  Returns `None` if the file does not
    /// exist (404).
    pub async fn get_file(&self, path: &str, branch: &str) -> Result<Option<ContentFile>, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}?ref={}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(path),
            url_encode(branch),
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        if response.status_code() == 404 {
            return Ok(None);
        }

        if response.status_code() != 200 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!(
                "GitHub API returned {}: {}",
                response.status_code(),
                body
            ));
        }

        let file: ContentFile = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse GitHub response: {e}"))?;

        Ok(Some(file))
    }

    /// Decode the base64 content from a [`ContentFile`].
    pub fn decode_content(file: &ContentFile) -> Result<String, String> {
        let encoded = file.content.as_deref().unwrap_or("");
        // GitHub returns base64 with newlines; strip them before decoding.
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let bytes = base64_decode(&cleaned)?;
        String::from_utf8(bytes).map_err(|e| format!("Content is not valid UTF-8: {e}"))
    }

    /// Create or update a file on the given branch.
    ///
    /// If `sha` is `Some`, this is an update to an existing file; otherwise a
    /// new file is created.
    pub async fn put_file(
        &self,
        path: &str,
        branch: &str,
        content: &str,
        message: &str,
        sha: Option<&str>,
    ) -> Result<(), String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/contents/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(path),
        );

        let encoded = base64_encode(content.as_bytes());
        let body = UpsertFileRequest {
            message,
            content: &encoded,
            sha,
            branch,
        };
        let body_json =
            serde_json::to_string(&body).map_err(|e| format!("Failed to serialize body: {e}"))?;

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("Content-Type", "application/json").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Put)
                .with_headers(headers)
                .with_body(Some(worker::wasm_bindgen::JsValue::from_str(&body_json))),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let mut response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        let status = response.status_code();
        if status != 200 && status != 201 {
            let body = response.text().await.unwrap_or_default();
            return Err(format!("GitHub API returned {status}: {body}"));
        }

        Ok(())
    }

    /// Check whether a branch exists on the remote.
    #[allow(dead_code)]
    pub async fn branch_exists(&self, branch: &str) -> Result<bool, String> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/git/ref/heads/{}",
            url_encode(&self.owner),
            url_encode(&self.repo),
            url_encode(branch),
        );

        let mut headers = worker::Headers::new();
        headers.set("Accept", "application/vnd.github+json").ok();
        headers
            .set("Authorization", &format!("Bearer {}", self.token))
            .ok();
        headers.set("User-Agent", "wreck-it-worker").ok();
        headers.set("X-GitHub-Api-Version", "2022-11-28").ok();

        let request = worker::Request::new_with_init(
            &url,
            worker::RequestInit::new()
                .with_method(worker::Method::Get)
                .with_headers(headers),
        )
        .map_err(|e| format!("Failed to create request: {e}"))?;

        let response = Fetch::Request(request)
            .send()
            .await
            .map_err(|e| format!("GitHub API request failed: {e}"))?;

        Ok(response.status_code() == 200)
    }

    #[allow(dead_code)]
    pub fn owner(&self) -> &str {
        &self.owner
    }

    #[allow(dead_code)]
    pub fn repo(&self) -> &str {
        &self.repo
    }
}

// ---------------------------------------------------------------------------
// URL encoding helper
// ---------------------------------------------------------------------------

/// Percent-encode a string for use in a URL path segment or query value.
/// Encodes all characters except unreserved ones (RFC 3986 §2.3): A-Z a-z 0-9 - . _ ~
/// Also preserves `/` in path segments since the GitHub Contents API expects paths like `dir/file`.
fn url_encode(s: &str) -> String {
    let mut result = String::with_capacity(s.len());
    for byte in s.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'/' => {
                result.push(byte as char)
            }
            _ => {
                result.push('%');
                result.push(HEX_CHARS[(byte >> 4) as usize] as char);
                result.push(HEX_CHARS[(byte & 0x0F) as usize] as char);
            }
        }
    }
    result
}

const HEX_CHARS: &[u8; 16] = b"0123456789ABCDEF";

// ---------------------------------------------------------------------------
// Base64 helpers (no external crate needed for this minimal subset)
// ---------------------------------------------------------------------------

const BASE64_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

fn base64_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(BASE64_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
        if chunk.len() > 2 {
            result.push(BASE64_CHARS[(triple & 0x3F) as usize] as char);
        } else {
            result.push('=');
        }
    }
    result
}

fn base64_decode(input: &str) -> Result<Vec<u8>, String> {
    let mut buf = Vec::with_capacity(input.len() * 3 / 4);
    let mut accum: u32 = 0;
    let mut bits: u32 = 0;

    for ch in input.bytes() {
        let val = match ch {
            b'A'..=b'Z' => ch - b'A',
            b'a'..=b'z' => ch - b'a' + 26,
            b'0'..=b'9' => ch - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
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
    Ok(buf)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_roundtrip() {
        let original = "Hello, wreck-it!";
        let encoded = base64_encode(original.as_bytes());
        let decoded = base64_decode(&encoded).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), original);
    }

    #[test]
    fn base64_encode_known() {
        assert_eq!(base64_encode(b"Hello"), "SGVsbG8=");
        assert_eq!(base64_encode(b"Hi"), "SGk=");
        assert_eq!(base64_encode(b"abc"), "YWJj");
    }

    #[test]
    fn base64_decode_with_newlines() {
        // GitHub API returns base64 with embedded newlines.
        let encoded = "SGVs\nbG8=";
        let cleaned: String = encoded.chars().filter(|c| !c.is_whitespace()).collect();
        let decoded = base64_decode(&cleaned).unwrap();
        assert_eq!(String::from_utf8(decoded).unwrap(), "Hello");
    }

    #[test]
    fn url_encode_preserves_safe_chars() {
        assert_eq!(url_encode("simple"), "simple");
        assert_eq!(url_encode("dir/file.json"), "dir/file.json");
        assert_eq!(url_encode("a-b_c.d~e"), "a-b_c.d~e");
    }

    #[test]
    fn url_encode_encodes_special_chars() {
        assert_eq!(url_encode("a b"), "a%20b");
        assert_eq!(url_encode("a?b=c"), "a%3Fb%3Dc");
        assert_eq!(url_encode("a#b"), "a%23b");
        assert_eq!(url_encode("a&b"), "a%26b");
    }
}
