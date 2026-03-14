//! Linear backend for the Kanban integration.
//!
//! Linear uses a **GraphQL API** (`https://api.linear.app/graphql`).  All
//! mutations and queries go through a single endpoint authenticated with a
//! bearer token.
//!
//! ## Status mapping
//!
//! | wreck-it status | Linear state name |
//! |-----------------|-------------------|
//! | Pending         | *Todo*            |
//! | InProgress      | *In Progress*     |
//! | Completed       | *Done*            |
//! | Failed          | *Canceled*        |

use super::{KanbanIssue, KanbanProvider, KanbanUpdates};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use std::sync::OnceLock;
use wreck_it_core::types::TaskStatus;

/// Default Linear GraphQL endpoint.
pub const DEFAULT_LINEAR_API: &str = "https://api.linear.app/graphql";

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
fn http() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

// ---------------------------------------------------------------------------
// GraphQL helpers
// ---------------------------------------------------------------------------

#[derive(Serialize)]
struct GqlRequest<'a> {
    query: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    variables: Option<serde_json::Value>,
}

#[derive(Deserialize)]
struct GqlResponse {
    data: Option<serde_json::Value>,
    errors: Option<Vec<GqlError>>,
}

#[derive(Deserialize)]
struct GqlError {
    message: String,
}

/// Map a wreck-it [`TaskStatus`] to the Linear workflow state **name**.
///
/// Linear teams have customisable workflow states.  We use the conventional
/// names here; if a team renames its states, the transition will fail and
/// the error will be logged as a warning.
fn status_to_linear_state(status: TaskStatus) -> &'static str {
    match status {
        TaskStatus::Pending => "Todo",
        TaskStatus::InProgress => "In Progress",
        TaskStatus::Completed => "Done",
        TaskStatus::Failed => "Canceled",
    }
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct LinearProvider {
    token: String,
    /// Linear team ID or key used to create issues.
    team_id: String,
    /// GraphQL endpoint (overridable for tests).
    api_url: String,
}

impl LinearProvider {
    pub fn new(
        token: impl Into<String>,
        team_id: impl Into<String>,
        api_url: impl Into<String>,
    ) -> Self {
        Self {
            token: token.into(),
            team_id: team_id.into(),
            api_url: api_url.into(),
        }
    }

    /// Execute a GraphQL query/mutation and return the `data` field.
    async fn gql(
        &self,
        query: &str,
        variables: Option<serde_json::Value>,
    ) -> Result<serde_json::Value> {
        let body = GqlRequest { query, variables };
        let resp = http()
            .post(&self.api_url)
            .bearer_auth(&self.token)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("Linear GraphQL request failed")?;

        if !resp.status().is_success() {
            let status = resp.status();
            let text = resp.text().await.unwrap_or_default();
            bail!("Linear API error ({status}): {text}");
        }

        let gql: GqlResponse = resp
            .json()
            .await
            .context("Failed to parse Linear response")?;
        if let Some(errors) = gql.errors {
            let msgs: Vec<_> = errors.iter().map(|e| e.message.as_str()).collect();
            bail!("Linear GraphQL errors: {}", msgs.join("; "));
        }

        gql.data.context("Linear response contained no data")
    }
}

impl KanbanProvider for LinearProvider {
    fn provider_name(&self) -> &str {
        "Linear"
    }

    async fn create_issue(&self, task_id: &str, description: &str) -> Result<KanbanIssue> {
        let title = format!("[{task_id}] {description}");
        let body = format!(
            "Automatically created by **wreck-it** to track task `{task_id}`.\n\n{description}"
        );
        let query = r#"
            mutation CreateIssue($teamId: String!, $title: String!, $description: String) {
                issueCreate(input: { teamId: $teamId, title: $title, description: $description }) {
                    success
                    issue { id identifier url title description }
                }
            }
        "#;
        let vars = serde_json::json!({
            "teamId": self.team_id,
            "title": title,
            "description": body,
        });

        let data = self.gql(query, Some(vars)).await?;
        let issue = &data["issueCreate"]["issue"];
        Ok(KanbanIssue {
            external_id: issue["id"].as_str().unwrap_or_default().to_string(),
            url: issue["url"].as_str().unwrap_or_default().to_string(),
            title: issue["title"].as_str().unwrap_or_default().to_string(),
            description: issue["description"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }

    async fn transition_issue(&self, external_id: &str, status: TaskStatus) -> Result<()> {
        let state_name = status_to_linear_state(status);

        // First, look up the workflow state id by name for the issue's team.
        let find_state = r#"
            query FindState($issueId: String!) {
                issue(id: $issueId) {
                    team { states { nodes { id name } } }
                }
            }
        "#;
        let data = self
            .gql(
                find_state,
                Some(serde_json::json!({ "issueId": external_id })),
            )
            .await?;

        let states = data["issue"]["team"]["states"]["nodes"]
            .as_array()
            .context("could not read workflow states from Linear")?;
        let state_id = states
            .iter()
            .find(|s| s["name"].as_str() == Some(state_name))
            .and_then(|s| s["id"].as_str())
            .context(format!(
                "Linear workflow state '{state_name}' not found for issue {external_id}"
            ))?;

        // Now update the issue.
        let update = r#"
            mutation UpdateIssue($issueId: String!, $stateId: String!) {
                issueUpdate(id: $issueId, input: { stateId: $stateId }) {
                    success
                }
            }
        "#;
        self.gql(
            update,
            Some(serde_json::json!({ "issueId": external_id, "stateId": state_id })),
        )
        .await?;
        Ok(())
    }

    async fn add_comment(&self, external_id: &str, comment: &str) -> Result<()> {
        let query = r#"
            mutation AddComment($issueId: String!, $body: String!) {
                commentCreate(input: { issueId: $issueId, body: $body }) {
                    success
                }
            }
        "#;
        self.gql(
            query,
            Some(serde_json::json!({ "issueId": external_id, "body": comment })),
        )
        .await?;
        Ok(())
    }

    async fn add_link(&self, external_id: &str, url: &str, title: &str) -> Result<()> {
        // Linear supports attachments with URLs on issues.
        let query = r#"
            mutation AddLink($issueId: String!, $url: String!, $title: String!) {
                attachmentCreate(input: { issueId: $issueId, url: $url, title: $title }) {
                    success
                }
            }
        "#;
        self.gql(
            query,
            Some(serde_json::json!({
                "issueId": external_id,
                "url": url,
                "title": title,
            })),
        )
        .await?;
        Ok(())
    }

    async fn get_issue(&self, external_id: &str) -> Result<KanbanIssue> {
        let query = r#"
            query GetIssue($id: String!) {
                issue(id: $id) { id identifier url title description }
            }
        "#;
        let data = self
            .gql(query, Some(serde_json::json!({ "id": external_id })))
            .await?;
        let issue = &data["issue"];
        Ok(KanbanIssue {
            external_id: issue["id"].as_str().unwrap_or_default().to_string(),
            url: issue["url"].as_str().unwrap_or_default().to_string(),
            title: issue["title"].as_str().unwrap_or_default().to_string(),
            description: issue["description"]
                .as_str()
                .unwrap_or_default()
                .to_string(),
        })
    }

    async fn get_updates(&self, external_id: &str, _since: Option<u64>) -> Result<KanbanUpdates> {
        // Fetch the issue description + comments; caller compares with local state.
        let query = r#"
            query GetUpdates($id: String!) {
                issue(id: $id) {
                    description
                    comments { nodes { body createdAt } }
                }
            }
        "#;
        let data = self
            .gql(query, Some(serde_json::json!({ "id": external_id })))
            .await?;
        let issue = &data["issue"];
        let description = issue["description"].as_str().map(|s| s.to_string());
        let comments = issue["comments"]["nodes"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|c| c["body"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(KanbanUpdates {
            description,
            comments,
        })
    }

    async fn close_issue(&self, external_id: &str) -> Result<()> {
        self.transition_issue(external_id, TaskStatus::Completed)
            .await
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
        assert_eq!(status_to_linear_state(TaskStatus::Pending), "Todo");
        assert_eq!(
            status_to_linear_state(TaskStatus::InProgress),
            "In Progress"
        );
        assert_eq!(status_to_linear_state(TaskStatus::Completed), "Done");
        assert_eq!(status_to_linear_state(TaskStatus::Failed), "Canceled");
    }

    #[test]
    fn provider_name_is_linear() {
        let p = LinearProvider::new("tok", "team", DEFAULT_LINEAR_API);
        assert_eq!(p.provider_name(), "Linear");
    }

    #[test]
    fn default_api_url_constant() {
        assert_eq!(DEFAULT_LINEAR_API, "https://api.linear.app/graphql");
    }

    // ── HTTP integration tests (mock TCP server) ─────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spin up a minimal HTTP/1.1 server that returns the given response.
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
    async fn create_issue_sends_graphql_mutation() {
        let resp = r#"{"data":{"issueCreate":{"success":true,"issue":{"id":"abc","identifier":"LIN-1","url":"https://linear.app/t/LIN-1","title":"[t1] desc","description":"desc"}}}}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", resp).await;

        let provider = LinearProvider::new("tok", "team-1", &url);
        let (result, raw) = tokio::join!(provider.create_issue("t1", "desc"), req_fut);

        let issue = result.unwrap();
        assert_eq!(issue.external_id, "abc");
        assert_eq!(issue.url, "https://linear.app/t/LIN-1");

        let body = extract_body(&raw);
        assert!(body["query"].as_str().unwrap().contains("issueCreate"));
        let vars = &body["variables"];
        assert_eq!(vars["teamId"], "team-1");
    }

    #[tokio::test]
    async fn add_comment_sends_graphql_mutation() {
        let resp = r#"{"data":{"commentCreate":{"success":true}}}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", resp).await;

        let provider = LinearProvider::new("tok", "team-1", &url);
        let (result, raw) = tokio::join!(
            provider.add_comment("issue-id", "Hello from wreck-it"),
            req_fut
        );

        result.unwrap();
        let body = extract_body(&raw);
        assert!(body["query"].as_str().unwrap().contains("commentCreate"));
        assert_eq!(body["variables"]["body"], "Hello from wreck-it");
    }

    #[tokio::test]
    async fn api_error_returns_err() {
        let resp = r#"{"message":"Unauthorized"}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 401 Unauthorized", resp).await;

        let provider = LinearProvider::new("bad", "team-1", &url);
        let (result, _) = tokio::join!(provider.create_issue("t1", "desc"), req_fut);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("401"));
    }

    #[tokio::test]
    async fn graphql_errors_returned_as_err() {
        let resp = r#"{"errors":[{"message":"Team not found"}]}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", resp).await;

        let provider = LinearProvider::new("tok", "bad-team", &url);
        let (result, _) = tokio::join!(provider.create_issue("t1", "desc"), req_fut);

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("Team not found"));
    }
}
