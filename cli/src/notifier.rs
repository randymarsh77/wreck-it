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

    #[tokio::test]
    async fn notify_empty_urls_is_noop() {
        // Should complete without panicking when no URLs are configured.
        notify(&[], "task-1", TaskStatus::Completed, 0, "a task").await;
    }
}
