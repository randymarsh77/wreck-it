//! Webhook signature verification and event routing.
//!
//! GitHub signs every webhook delivery with HMAC-SHA256.  This module
//! validates the `X-Hub-Signature-256` header to ensure the payload was
//! sent by GitHub and has not been tampered with.

use hmac::{Hmac, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// Verify the `X-Hub-Signature-256` header against the raw request body.
///
/// `signature` is the full header value, e.g. `"sha256=abcdef..."`.
/// `secret` is the webhook secret configured in the GitHub App settings.
/// `body` is the raw request body bytes.
///
/// Returns `true` when the signature is valid.
pub fn verify_signature(signature: &str, secret: &str, body: &[u8]) -> bool {
    let hex_sig = match signature.strip_prefix("sha256=") {
        Some(h) => h,
        None => return false,
    };

    let expected = match hex::decode(hex_sig) {
        Ok(b) => b,
        Err(_) => return false,
    };

    let mut mac = match HmacSha256::new_from_slice(secret.as_bytes()) {
        Ok(m) => m,
        Err(_) => return false,
    };
    mac.update(body);

    mac.verify_slice(&expected).is_ok()
}

/// GitHub webhook event types that we handle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WebhookEvent {
    /// Issue created or modified.
    Issues,
    /// Push to a branch.
    Push,
    /// Pull request created, closed, merged, etc.
    PullRequest,
    /// An event type we do not handle.
    Other(String),
}

impl WebhookEvent {
    /// Parse the `X-GitHub-Event` header value.
    pub fn from_header(value: &str) -> Self {
        match value {
            "issues" => Self::Issues,
            "push" => Self::Push,
            "pull_request" => Self::PullRequest,
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_signature_passes() {
        let secret = "test-secret";
        let body = b"hello world";

        // Compute expected signature.
        let mut mac = HmacSha256::new_from_slice(secret.as_bytes()).unwrap();
        mac.update(body);
        let result = mac.finalize().into_bytes();
        let sig = format!("sha256={}", hex::encode(result));

        assert!(verify_signature(&sig, secret, body));
    }

    #[test]
    fn invalid_signature_fails() {
        assert!(!verify_signature("sha256=deadbeef", "secret", b"payload"));
    }

    #[test]
    fn missing_prefix_fails() {
        assert!(!verify_signature("abcdef", "secret", b"payload"));
    }

    #[test]
    fn event_parsing() {
        assert_eq!(WebhookEvent::from_header("issues"), WebhookEvent::Issues);
        assert_eq!(WebhookEvent::from_header("push"), WebhookEvent::Push);
        assert_eq!(
            WebhookEvent::from_header("pull_request"),
            WebhookEvent::PullRequest
        );
        assert_eq!(
            WebhookEvent::from_header("ping"),
            WebhookEvent::Other("ping".into())
        );
    }
}
