use crate::types::TaskStatus;
use reqwest::Client;
use serde::Serialize;
use std::sync::OnceLock;
use tracing::warn;

static HTTP_CLIENT: OnceLock<Client> = OnceLock::new();

fn http_client() -> &'static Client {
    HTTP_CLIENT.get_or_init(Client::new)
}

#[derive(Debug, Serialize)]
struct WebhookPayload<'a> {
    task_id: &'a str,
    status: &'a str,
    timestamp: u64,
    description: &'a str,
}

/// Send a webhook notification for a task status transition to every configured URL.
/// Failures are logged as warnings but do not abort the caller.
pub async fn notify(
    urls: &[String],
    task_id: &str,
    status: TaskStatus,
    timestamp: u64,
    description: &str,
) {
    if urls.is_empty() {
        return;
    }

    let status_str = match status {
        TaskStatus::InProgress => "in_progress",
        TaskStatus::Completed => "completed",
        TaskStatus::Failed => "failed",
        TaskStatus::Pending => "pending",
    };

    let payload = WebhookPayload {
        task_id,
        status: status_str,
        timestamp,
        description,
    };

    let client = http_client();
    for url in urls {
        match client.post(url).json(&payload).send().await {
            Ok(resp) if resp.status().is_success() => {}
            Ok(resp) => {
                warn!(
                    "Webhook {} returned non-success status: {}",
                    url,
                    resp.status()
                );
            }
            Err(e) => {
                warn!("Failed to reach webhook {}: {}", url, e);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::TcpListener;

    /// Spin up a minimal HTTP/1.1 server on a random localhost port.
    /// Returns the URL and a future that accepts one connection, reads the raw
    /// request bytes, sends back the given status line, and returns the body.
    async fn mock_server(
        status_line: &'static str,
    ) -> (String, impl std::future::Future<Output = Vec<u8>>) {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let url = format!("http://127.0.0.1:{port}/hook");

        let fut = async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = vec![0u8; 8192];
            let n = stream.read(&mut buf).await.unwrap();
            buf.truncate(n);
            let response = format!(
                "{}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
                status_line
            );
            let _ = stream.write_all(response.as_bytes()).await;
            buf
        };

        (url, fut)
    }

    /// Extract the JSON body from a raw HTTP request captured by `mock_server`.
    fn extract_body(raw: &[u8]) -> serde_json::Value {
        let text = std::str::from_utf8(raw).expect("request is not valid UTF-8");
        let body_start = text.find("\r\n\r\n").expect("no header/body separator in request");
        let body = &text[body_start + 4..];
        serde_json::from_str(body).expect("body is not valid JSON")
    }

    #[tokio::test]
    async fn notify_empty_urls_is_noop() {
        // Should complete without panicking when no URLs are configured.
        notify(&[], "task-1", TaskStatus::Completed, 0, "a task").await;
    }

    #[tokio::test]
    async fn notify_sends_post_to_each_url() {
        let (url1, req1_fut) = mock_server("HTTP/1.1 200 OK").await;
        let (url2, req2_fut) = mock_server("HTTP/1.1 200 OK").await;
        let urls = vec![url1, url2];

        let ((), raw1, raw2) = tokio::join!(
            notify(&urls, "task-42", TaskStatus::InProgress, 1_700_000_000, "doing work"),
            req1_fut,
            req2_fut,
        );

        assert!(!raw1.is_empty(), "server 1 received no request");
        assert!(!raw2.is_empty(), "server 2 received no request");
    }

    #[tokio::test]
    async fn notify_payload_has_correct_fields() {
        let (url, req_fut) = mock_server("HTTP/1.1 200 OK").await;
        let ts = 1_700_000_000u64;
        let urls = vec![url];

        let ((), raw) = tokio::join!(
            notify(&urls, "task-99", TaskStatus::Completed, ts, "all done"),
            req_fut,
        );

        let v = extract_body(&raw);
        assert_eq!(v["task_id"], "task-99");
        assert_eq!(v["status"], "completed");
        assert_eq!(v["timestamp"], ts);
    }

    #[tokio::test]
    async fn notify_all_statuses_serialize() {
        let statuses = [
            (TaskStatus::Pending, "pending"),
            (TaskStatus::InProgress, "in_progress"),
            (TaskStatus::Completed, "completed"),
            (TaskStatus::Failed, "failed"),
        ];

        for (status, expected_str) in statuses {
            let (url, req_fut) = mock_server("HTTP/1.1 200 OK").await;
            let urls = vec![url];
            let ((), raw) = tokio::join!(
                notify(&urls, "task-s", status, 0, "status test"),
                req_fut,
            );
            let v = extract_body(&raw);
            assert_eq!(
                v["status"], expected_str,
                "wrong status string for {expected_str}"
            );
        }
    }

    #[tokio::test]
    async fn notify_non_success_response_does_not_propagate() {
        // A 500 response should be logged as a warning but must not panic or return Err.
        let (url, req_fut) = mock_server("HTTP/1.1 500 Internal Server Error").await;
        let urls = vec![url];
        let ((), _raw) = tokio::join!(
            notify(&urls, "task-5xx", TaskStatus::Failed, 0, "oops"),
            req_fut,
        );
        // Reaching here means no panic and no propagated error.
    }

    #[tokio::test]
    async fn notify_connection_refused_does_not_propagate() {
        // Bind to a random port then immediately drop the listener so that any
        // subsequent connection attempt is refused.
        let addr = {
            let l = TcpListener::bind("127.0.0.1:0").await.unwrap();
            l.local_addr().unwrap()
        };
        let url = format!("http://{}/hook", addr);

        // Must complete without panicking or returning an error.
        notify(&[url], "task-err", TaskStatus::Failed, 0, "failed").await;
    }
}
