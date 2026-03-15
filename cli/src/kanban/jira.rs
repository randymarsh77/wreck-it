//! Atlassian JIRA backend for the Kanban integration.
//!
//! JIRA uses a **REST API v3** (or v2 for older instances).  Authentication
//! is done via Basic Auth with `email:api_token` for Atlassian Cloud, or a
//! bearer token for Data Center.
//!
//! ## Status mapping
//!
//! | wreck-it status | JIRA transition |
//! |-----------------|-----------------|
//! | Pending         | *To Do*         |
//! | InProgress      | *In Progress*   |
//! | Completed       | *Done*          |
//! | Failed          | *Done*          |

use super::{KanbanIssue, KanbanProvider, KanbanUpdates};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::sync::OnceLock;
use wreck_it_core::types::TaskStatus;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
fn http() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

/// Map a wreck-it [`TaskStatus`] to the JIRA transition name.
///
/// JIRA projects may have custom workflows; these are the default names
/// from the standard Kanban template.
fn status_to_jira_transition(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "To Do",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::Completed | TaskStatus::Failed => "Done",
    }
}

// ---------------------------------------------------------------------------
// JSON shapes
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
#[allow(dead_code)]
struct JiraIssueResponse {
    id: String,
    key: String,
    #[serde(rename = "self")]
    self_url: String,
    fields: JiraFields,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct JiraFields {
    summary: Option<String>,
    description: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct JiraTransition {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct JiraTransitionsResponse {
    transitions: Vec<JiraTransition>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct JiraComment {
    body: Option<serde_json::Value>,
}

#[derive(Deserialize)]
#[allow(dead_code)]
struct JiraCommentsResponse {
    comments: Vec<JiraComment>,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct JiraProvider {
    /// API token used with basic auth.
    api_token: String,
    /// JIRA project key, e.g. `"PROJ"`.
    project_key: String,
    /// Base URL of the JIRA instance, e.g. `https://acme.atlassian.net`.
    base_url: String,
    /// Email for basic auth on Atlassian Cloud.
    user_email: String,
}

impl JiraProvider {
    pub fn new(
        api_token: impl Into<String>,
        project_key: impl Into<String>,
        base_url: impl Into<String>,
        user_email: impl Into<String>,
    ) -> Self {
        let mut base = base_url.into();
        // Strip trailing slash for consistent URL building.
        if base.ends_with('/') {
            base.pop();
        }
        Self {
            api_token: api_token.into(),
            project_key: project_key.into(),
            base_url: base,
            user_email: user_email.into(),
        }
    }

    /// Build the REST API v3 URL for the given path.
    fn url(&self, path: &str) -> String {
        format!("{}/rest/api/3/{path}", self.base_url)
    }

    /// Apply basic-auth headers to a request builder.
    fn auth(&self, builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        if self.user_email.is_empty() {
            // Data Center: bearer token
            builder.bearer_auth(&self.api_token)
        } else {
            // Atlassian Cloud: basic auth with email:token
            builder.basic_auth(&self.user_email, Some(&self.api_token))
        }
    }

    /// Helper to extract plain text from JIRA ADF (Atlassian Document Format).
    ///
    /// ADF is a complex JSON structure; for simplicity we extract all `"text"`
    /// nodes recursively and join them.
    #[allow(dead_code)]
    fn adf_to_text(adf: &serde_json::Value) -> String {
        let mut parts = Vec::new();
        Self::extract_text_nodes(adf, &mut parts);
        parts.join("\n")
    }

    #[allow(dead_code)]
    fn extract_text_nodes(node: &serde_json::Value, out: &mut Vec<String>) {
        if let Some(text) = node["text"].as_str() {
            out.push(text.to_string());
        }
        if let Some(content) = node["content"].as_array() {
            for child in content {
                Self::extract_text_nodes(child, out);
            }
        }
    }

    /// Build a simple ADF document from plain text.
    fn text_to_adf(text: &str) -> serde_json::Value {
        serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [{
                    "type": "text",
                    "text": text
                }]
            }]
        })
    }
}

impl KanbanProvider for JiraProvider {
    fn provider_name(&self) -> &str {
        "Jira"
    }

    async fn create_issue(&self, task_id: &str, description: &str) -> Result<KanbanIssue> {
        let summary = format!("[{task_id}] {description}");
        let body_text =
            format!("Automatically created by wreck-it to track task {task_id}.\n\n{description}");

        let payload = serde_json::json!({
            "fields": {
                "project": { "key": &self.project_key },
                "summary": summary,
                "description": Self::text_to_adf(&body_text),
                "issuetype": { "name": "Task" }
            }
        });

        let resp = self
            .auth(
                http()
                    .post(self.url("issue"))
                    .header("Content-Type", "application/json")
                    .header("Accept", "application/json"),
            )
            .json(&payload)
            .send()
            .await
            .context("JIRA create-issue request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("JIRA create-issue failed ({status}): {text}");
        }

        let created: JiraIssueResponse = resp
            .json()
            .await
            .context("Failed to parse JIRA create-issue response")?;

        let browse_url = format!("{}/browse/{}", self.base_url, created.key);
        Ok(KanbanIssue {
            external_id: created.key,
            url: browse_url,
            title: summary,
            description: body_text,
        })
    }

    async fn transition_issue(&self, external_id: &str, status: TaskStatus) -> Result<()> {
        let target = status_to_jira_transition(status);

        // Fetch available transitions for the issue.
        let resp = self
            .auth(
                http()
                    .get(self.url(&format!("issue/{external_id}/transitions")))
                    .header("Accept", "application/json"),
            )
            .send()
            .await
            .context("JIRA get-transitions failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("JIRA get-transitions failed ({s}): {body}");
        }

        let transitions: JiraTransitionsResponse = resp.json().await?;
        let transition_id = transitions
            .transitions
            .iter()
            .find(|t| t.name.eq_ignore_ascii_case(target))
            .map(|t| t.id.clone())
            .context(format!(
                "JIRA transition '{target}' not found for issue {external_id}"
            ))?;

        // Perform the transition.
        let payload = serde_json::json!({ "transition": { "id": transition_id } });
        let resp = self
            .auth(
                http()
                    .post(self.url(&format!("issue/{external_id}/transitions")))
                    .header("Content-Type", "application/json"),
            )
            .json(&payload)
            .send()
            .await
            .context("JIRA do-transition request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("JIRA do-transition failed ({s}): {body}");
        }

        Ok(())
    }

    async fn add_comment(&self, external_id: &str, comment: &str) -> Result<()> {
        let payload = serde_json::json!({
            "body": Self::text_to_adf(comment)
        });

        let resp = self
            .auth(
                http()
                    .post(self.url(&format!("issue/{external_id}/comment")))
                    .header("Content-Type", "application/json"),
            )
            .json(&payload)
            .send()
            .await
            .context("JIRA add-comment request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("JIRA add-comment failed ({s}): {body}");
        }
        Ok(())
    }

    async fn add_link(&self, external_id: &str, url: &str, title: &str) -> Result<()> {
        // Add a remote link to the JIRA issue.
        let payload = serde_json::json!({
            "object": {
                "url": url,
                "title": title
            }
        });

        let resp = self
            .auth(
                http()
                    .post(self.url(&format!("issue/{external_id}/remotelink")))
                    .header("Content-Type", "application/json"),
            )
            .json(&payload)
            .send()
            .await
            .context("JIRA add-link request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("JIRA add-link failed ({s}): {body}");
        }
        Ok(())
    }

    async fn get_issue(&self, external_id: &str) -> Result<KanbanIssue> {
        let resp = self
            .auth(
                http()
                    .get(self.url(&format!("issue/{external_id}")))
                    .header("Accept", "application/json"),
            )
            .send()
            .await
            .context("JIRA get-issue request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("JIRA get-issue failed ({s}): {body}");
        }

        let issue: JiraIssueResponse = resp.json().await?;
        let desc = issue
            .fields
            .description
            .as_ref()
            .map(Self::adf_to_text)
            .unwrap_or_default();
        let browse_url = format!("{}/browse/{}", self.base_url, issue.key);

        Ok(KanbanIssue {
            external_id: issue.key,
            url: browse_url,
            title: issue.fields.summary.unwrap_or_default(),
            description: desc,
        })
    }

    async fn get_updates(&self, external_id: &str, _since: Option<u64>) -> Result<KanbanUpdates> {
        // Fetch issue + comments.
        let issue = self.get_issue(external_id).await?;

        let resp = self
            .auth(
                http()
                    .get(self.url(&format!("issue/{external_id}/comment")))
                    .header("Accept", "application/json"),
            )
            .send()
            .await
            .context("JIRA get-comments request failed")?;

        let comments = if resp.status().is_success() {
            let cr: JiraCommentsResponse = resp.json().await.unwrap_or(JiraCommentsResponse {
                comments: Vec::new(),
            });
            cr.comments
                .iter()
                .filter_map(|c| c.body.as_ref().map(Self::adf_to_text))
                .collect()
        } else {
            Vec::new()
        };

        Ok(KanbanUpdates {
            description: Some(issue.description),
            comments,
        })
    }

    async fn close_issue(&self, external_id: &str) -> Result<()> {
        self.transition_issue(external_id, TaskStatus::Completed)
            .await
    }

    async fn list_inbound_issues(&self, label: &str) -> Result<Vec<KanbanIssue>> {
        // Use JQL to search for issues in the project with the given label.
        // Escape backslashes and double-quotes to prevent JQL injection.
        let safe_label = label.replace('\\', "\\\\").replace('"', "\\\"");
        let jql = format!(
            "project = \"{}\" AND labels = \"{safe_label}\" ORDER BY created DESC",
            self.project_key
        );

        #[derive(Deserialize)]
        struct JiraSearchResponse {
            issues: Vec<JiraIssueResponse>,
        }

        let resp = self
            .auth(
                http()
                    .get(self.url("search"))
                    .header("Accept", "application/json")
                    .query(&[("jql", jql.as_str()), ("fields", "summary,description")]),
            )
            .send()
            .await
            .context("JIRA search request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("JIRA search failed ({s}): {body}");
        }

        let search: JiraSearchResponse = resp
            .json()
            .await
            .context("Failed to parse JIRA search response")?;

        let issues = search
            .issues
            .into_iter()
            .map(|i| {
                let desc = i
                    .fields
                    .description
                    .as_ref()
                    .map(Self::adf_to_text)
                    .unwrap_or_default();
                let browse_url = format!("{}/browse/{}", self.base_url, i.key);
                KanbanIssue {
                    external_id: i.key,
                    url: browse_url,
                    title: i.fields.summary.unwrap_or_default(),
                    description: desc,
                }
            })
            .collect();
        Ok(issues)
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_mapping_covers_all_variants() {
        assert_eq!(status_to_jira_transition(TaskStatus::Pending), "To Do");
        assert_eq!(
            status_to_jira_transition(TaskStatus::InProgress),
            "In Progress"
        );
        assert_eq!(status_to_jira_transition(TaskStatus::Completed), "Done");
        assert_eq!(status_to_jira_transition(TaskStatus::Failed), "Done");
    }

    #[test]
    fn provider_name_is_jira() {
        let p = JiraProvider::new("tok", "PROJ", "https://acme.atlassian.net", "u@x.com");
        assert_eq!(p.provider_name(), "Jira");
    }

    #[test]
    fn trailing_slash_stripped_from_base_url() {
        let p = JiraProvider::new("tok", "PROJ", "https://acme.atlassian.net/", "u@x.com");
        assert_eq!(p.base_url, "https://acme.atlassian.net");
    }

    #[test]
    fn adf_to_text_extracts_paragraphs() {
        let adf = serde_json::json!({
            "type": "doc",
            "version": 1,
            "content": [{
                "type": "paragraph",
                "content": [
                    { "type": "text", "text": "Hello " },
                    { "type": "text", "text": "world" }
                ]
            }]
        });
        let text = JiraProvider::adf_to_text(&adf);
        assert!(text.contains("Hello "));
        assert!(text.contains("world"));
    }

    #[test]
    fn text_to_adf_produces_valid_adf() {
        let adf = JiraProvider::text_to_adf("test");
        assert_eq!(adf["type"], "doc");
        assert_eq!(adf["content"][0]["content"][0]["text"], "test");
    }

    // ── HTTP integration tests (mock TCP server) ─────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    async fn mock_server(
        status: &'static str,
        body: &'static str,
    ) -> (String, impl std::future::Future<Output = Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");
        let fut = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 32768];
            let n = stream.read(&mut buf).await.unwrap();
            buf.truncate(n);
            let resp = format!(
                "{status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(resp.as_bytes()).await;
            buf
        };
        (url, fut)
    }

    fn extract_body(raw: &[u8]) -> serde_json::Value {
        let text = std::str::from_utf8(raw).unwrap();
        let start = text.find("\r\n\r\n").unwrap() + 4;
        serde_json::from_str(&text[start..]).unwrap()
    }

    #[tokio::test]
    async fn create_issue_sends_post_to_rest_endpoint() {
        let resp = r#"{"id":"10001","key":"PROJ-1","self":"https://acme.atlassian.net/rest/api/3/issue/10001","fields":{"summary":"[t1] desc","description":null}}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 201 Created", resp).await;

        let provider = JiraProvider::new("tok", "PROJ", &url, "u@x.com");
        let (result, raw) = tokio::join!(provider.create_issue("t1", "desc"), req_fut);

        let issue = result.unwrap();
        assert_eq!(issue.external_id, "PROJ-1");
        assert!(issue.url.contains("PROJ-1"));

        let body = extract_body(&raw);
        assert_eq!(body["fields"]["project"]["key"], "PROJ");
    }

    #[tokio::test]
    async fn api_error_returns_err() {
        let resp = r#"{"errorMessages":["Unauthorized"]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 401 Unauthorized", resp).await;

        let provider = JiraProvider::new("bad", "PROJ", &url, "u@x.com");
        let (result, _) = tokio::join!(provider.create_issue("t1", "desc"), req_fut);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("401"));
    }

    #[tokio::test]
    async fn list_inbound_issues_searches_by_label() {
        let resp = r#"{"issues":[{"id":"10001","key":"PROJ-1","self":"https://acme.atlassian.net/rest/api/3/issue/10001","fields":{"summary":"[t1] do thing","description":null}},{"id":"10002","key":"PROJ-2","self":"https://acme.atlassian.net/rest/api/3/issue/10002","fields":{"summary":"[t2] other thing","description":null}}]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", resp).await;

        let provider = JiraProvider::new("tok", "PROJ", &url, "u@x.com");
        let (result, raw) = tokio::join!(provider.list_inbound_issues("wreck-it"), req_fut);

        let issues = result.unwrap();
        assert_eq!(issues.len(), 2);
        assert_eq!(issues[0].external_id, "PROJ-1");
        assert_eq!(issues[1].external_id, "PROJ-2");

        let req = std::str::from_utf8(&raw).unwrap();
        assert!(req.starts_with("GET"));
        assert!(req.contains("/rest/api/3/search"));
        assert!(req.contains("wreck-it"));
    }

    #[tokio::test]
    async fn list_inbound_issues_empty_returns_empty_vec() {
        let resp = r#"{"issues":[]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", resp).await;

        let provider = JiraProvider::new("tok", "PROJ", &url, "u@x.com");
        let (result, _) = tokio::join!(provider.list_inbound_issues("no-match"), req_fut);

        assert_eq!(result.unwrap().len(), 0);
    }
}
