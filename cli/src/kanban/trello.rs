//! Trello backend for the Kanban integration.
//!
//! Trello uses a **REST API** authenticated with an API key + token pair.
//! The `api_token` config value is expected in `key:token` format.
//!
//! ## Concepts
//!
//! | wreck-it            | Trello         |
//! |---------------------|----------------|
//! | project / repo      | Board          |
//! | task                | Card           |
//! | task status          | List on board  |
//!
//! ## Status mapping
//!
//! | wreck-it status | Trello list name |
//! |-----------------|------------------|
//! | Pending         | *To Do*          |
//! | InProgress      | *In Progress*    |
//! | Completed       | *Done*           |
//! | Failed          | *Done*           |

use super::{KanbanIssue, KanbanProvider, KanbanUpdates};
use anyhow::{bail, Context, Result};
use reqwest::Client;
use serde::Deserialize;
use std::sync::OnceLock;
use wreck_it_core::types::TaskStatus;

/// Default Trello REST API base URL.
pub const DEFAULT_TRELLO_API: &str = "https://api.trello.com/1";

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();
fn http() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

/// Map a wreck-it [`TaskStatus`] to the Trello list name.
fn status_to_trello_list(status: TaskStatus) -> &'static str {
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
struct TrelloCard {
    id: String,
    name: String,
    desc: String,
    url: String,
    #[serde(rename = "idList")]
    id_list: String,
}

#[derive(Deserialize)]
struct TrelloList {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct TrelloComment {
    data: TrelloCommentData,
}

#[derive(Deserialize)]
struct TrelloCommentData {
    text: Option<String>,
}

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

pub struct TrelloProvider {
    /// Trello API key.
    api_key: String,
    /// Trello API token.
    api_token: String,
    /// Board ID.
    board_id: String,
    /// REST API base URL.
    api_base: String,
}

impl TrelloProvider {
    /// Create a new Trello provider.
    ///
    /// `key_token` must be in `key:token` format.  `board_id` is the Trello
    /// board identifier.
    pub fn new(
        key_token: impl Into<String>,
        board_id: impl Into<String>,
        api_base: impl Into<String>,
    ) -> Self {
        let kt: String = key_token.into();
        let (key, token) = kt.split_once(':').unwrap_or((&kt, ""));
        Self {
            api_key: key.to_string(),
            api_token: token.to_string(),
            board_id: board_id.into(),
            api_base: api_base.into(),
        }
    }

    /// Append auth query parameters.
    fn auth_params(&self) -> [(&str, &str); 2] {
        [("key", &self.api_key), ("token", &self.api_token)]
    }

    /// Find the list ID on the board that matches `list_name`.
    async fn find_list_id(&self, list_name: &str) -> Result<String> {
        let url = format!("{}/boards/{}/lists", self.api_base, self.board_id);
        let resp = http()
            .get(&url)
            .query(&self.auth_params())
            .send()
            .await
            .context("Trello get-lists request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello get-lists failed ({s}): {body}");
        }

        let lists: Vec<TrelloList> = resp.json().await?;
        lists
            .iter()
            .find(|l| l.name.eq_ignore_ascii_case(list_name))
            .map(|l| l.id.clone())
            .context(format!(
                "Trello list '{list_name}' not found on board {}",
                self.board_id
            ))
    }
}

impl KanbanProvider for TrelloProvider {
    fn provider_name(&self) -> &str {
        "Trello"
    }

    async fn create_issue(&self, task_id: &str, description: &str) -> Result<KanbanIssue> {
        let list_name = status_to_trello_list(TaskStatus::Pending);
        let list_id = self.find_list_id(list_name).await?;

        let name = format!("[{task_id}] {description}");
        let desc = format!(
            "Automatically created by wreck-it to track task `{task_id}`.\n\n{description}"
        );

        let url = format!("{}/cards", self.api_base);
        let resp = http()
            .post(&url)
            .query(&self.auth_params())
            .query(&[("idList", &list_id), ("name", &name), ("desc", &desc)])
            .send()
            .await
            .context("Trello create-card request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello create-card failed ({s}): {body}");
        }

        let card: TrelloCard = resp.json().await?;
        Ok(KanbanIssue {
            external_id: card.id,
            url: card.url,
            title: card.name,
            description: card.desc,
        })
    }

    async fn transition_issue(&self, external_id: &str, status: TaskStatus) -> Result<()> {
        let list_name = status_to_trello_list(status);
        let list_id = self.find_list_id(list_name).await?;

        let url = format!("{}/cards/{external_id}", self.api_base);
        let resp = http()
            .put(&url)
            .query(&self.auth_params())
            .query(&[("idList", &list_id)])
            .send()
            .await
            .context("Trello move-card request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello move-card failed ({s}): {body}");
        }
        Ok(())
    }

    async fn add_comment(&self, external_id: &str, comment: &str) -> Result<()> {
        let url = format!("{}/cards/{external_id}/actions/comments", self.api_base);
        let resp = http()
            .post(&url)
            .query(&self.auth_params())
            .query(&[("text", comment)])
            .send()
            .await
            .context("Trello add-comment request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello add-comment failed ({s}): {body}");
        }
        Ok(())
    }

    async fn add_link(&self, external_id: &str, url: &str, title: &str) -> Result<()> {
        // Trello supports URL attachments on cards.
        let endpoint = format!("{}/cards/{external_id}/attachments", self.api_base);
        let resp = http()
            .post(&endpoint)
            .query(&self.auth_params())
            .query(&[("url", url), ("name", title)])
            .send()
            .await
            .context("Trello add-attachment request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello add-attachment failed ({s}): {body}");
        }
        Ok(())
    }

    async fn get_issue(&self, external_id: &str) -> Result<KanbanIssue> {
        let url = format!("{}/cards/{external_id}", self.api_base);
        let resp = http()
            .get(&url)
            .query(&self.auth_params())
            .send()
            .await
            .context("Trello get-card request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello get-card failed ({s}): {body}");
        }

        let card: TrelloCard = resp.json().await?;
        Ok(KanbanIssue {
            external_id: card.id,
            url: card.url,
            title: card.name,
            description: card.desc,
        })
    }

    async fn get_updates(
        &self,
        external_id: &str,
        _since: Option<u64>,
    ) -> Result<KanbanUpdates> {
        let card = self.get_issue(external_id).await?;

        // Fetch comments (commentCard actions).
        let url = format!(
            "{}/cards/{external_id}/actions",
            self.api_base
        );
        let resp = http()
            .get(&url)
            .query(&self.auth_params())
            .query(&[("filter", "commentCard")])
            .send()
            .await
            .context("Trello get-comments request failed")?;

        let comments = if resp.status().is_success() {
            let actions: Vec<TrelloComment> = resp.json().await.unwrap_or_default();
            actions
                .iter()
                .filter_map(|a| a.data.text.clone())
                .collect()
        } else {
            Vec::new()
        };

        Ok(KanbanUpdates {
            description: Some(card.description),
            comments,
        })
    }

    async fn close_issue(&self, external_id: &str) -> Result<()> {
        // Archive the card.
        let url = format!("{}/cards/{external_id}", self.api_base);
        let resp = http()
            .put(&url)
            .query(&self.auth_params())
            .query(&[("closed", "true")])
            .send()
            .await
            .context("Trello archive-card request failed")?;

        if !resp.status().is_success() {
            let s = resp.status();
            let body = resp.text().await.unwrap_or_default();
            bail!("Trello archive-card failed ({s}): {body}");
        }
        Ok(())
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
        assert_eq!(status_to_trello_list(TaskStatus::Pending), "To Do");
        assert_eq!(status_to_trello_list(TaskStatus::InProgress), "In Progress");
        assert_eq!(status_to_trello_list(TaskStatus::Completed), "Done");
        assert_eq!(status_to_trello_list(TaskStatus::Failed), "Done");
    }

    #[test]
    fn provider_name_is_trello() {
        let p = TrelloProvider::new("key:token", "board123", DEFAULT_TRELLO_API);
        assert_eq!(p.provider_name(), "Trello");
    }

    #[test]
    fn key_token_split() {
        let p = TrelloProvider::new("mykey:mytoken", "b1", DEFAULT_TRELLO_API);
        assert_eq!(p.api_key, "mykey");
        assert_eq!(p.api_token, "mytoken");
    }

    #[test]
    fn key_token_no_colon_uses_full_string_as_key() {
        let p = TrelloProvider::new("justkey", "b1", DEFAULT_TRELLO_API);
        assert_eq!(p.api_key, "justkey");
        assert_eq!(p.api_token, "");
    }

    #[test]
    fn default_api_url_constant() {
        assert_eq!(DEFAULT_TRELLO_API, "https://api.trello.com/1");
    }

    // ── HTTP integration tests (mock TCP server) ─────────────────────────

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Multi-request mock server that handles two sequential requests.
    /// First returns `first_body`, second returns `second_body`.
    async fn mock_server_multi(
        first_status: &'static str,
        first_body: &'static str,
        second_status: &'static str,
        second_body: &'static str,
    ) -> (String, impl std::future::Future<Output = (Vec<u8>, Vec<u8>)>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}");
        let fut = async move {
            // First request
            let (mut s1, _) = listener.accept().await.unwrap();
            let mut buf1 = vec![0u8; 32768];
            let n = s1.read(&mut buf1).await.unwrap();
            buf1.truncate(n);
            let resp1 = format!(
                "{first_status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{first_body}",
                first_body.len()
            );
            let _ = s1.write_all(resp1.as_bytes()).await;
            drop(s1);

            // Second request
            let (mut s2, _) = listener.accept().await.unwrap();
            let mut buf2 = vec![0u8; 32768];
            let n = s2.read(&mut buf2).await.unwrap();
            buf2.truncate(n);
            let resp2 = format!(
                "{second_status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{second_body}",
                second_body.len()
            );
            let _ = s2.write_all(resp2.as_bytes()).await;
            (buf1, buf2)
        };
        (url, fut)
    }

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

    #[tokio::test]
    async fn create_card_sends_post() {
        let lists_body = r#"[{"id":"list1","name":"To Do"},{"id":"list2","name":"In Progress"}]"#;
        let card_body = r#"{"id":"card1","name":"[t1] desc","desc":"desc","url":"https://trello.com/c/card1","idList":"list1"}"#;

        let (url, req_fut) = mock_server_multi(
            "HTTP/1.1 200 OK",
            lists_body,
            "HTTP/1.1 200 OK",
            card_body,
        )
        .await;

        let provider = TrelloProvider::new("key:token", "board1", &url);
        let (result, (raw1, _raw2)) = tokio::join!(provider.create_issue("t1", "desc"), req_fut);

        let issue = result.unwrap();
        assert_eq!(issue.external_id, "card1");
        assert_eq!(issue.url, "https://trello.com/c/card1");

        // First request should be GET /boards/{board}/lists
        let req1 = std::str::from_utf8(&raw1).unwrap();
        assert!(req1.starts_with("GET"));
        assert!(req1.contains("/boards/board1/lists"));
    }

    #[tokio::test]
    async fn add_comment_sends_post() {
        let resp = r#"{"id":"action1"}"#;
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK", resp).await;

        let provider = TrelloProvider::new("key:token", "board1", &url);
        let (result, raw) = tokio::join!(
            provider.add_comment("card1", "Hello from wreck-it"),
            req_fut
        );

        result.unwrap();
        let req = std::str::from_utf8(&raw).unwrap();
        assert!(req.starts_with("POST"));
        assert!(req.contains("/cards/card1/actions/comments"));
    }

    #[tokio::test]
    async fn api_error_returns_err() {
        let resp = r#"unauthorized permission requested"#;
        let (url, req_fut) = mock_server("HTTP/1.1 401 Unauthorized", resp).await;

        let provider = TrelloProvider::new("bad:bad", "board1", &url);
        let (result, _) = tokio::join!(
            provider.add_comment("card1", "Hello"),
            req_fut
        );

        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("401"));
    }
}
