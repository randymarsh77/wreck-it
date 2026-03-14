//! GitHub App authentication — JWT generation and installation token vending.
//!
//! A GitHub App authenticates by generating a short-lived JWT signed with its
//! RSA private key, then exchanging that JWT for an installation access token
//! scoped to a specific repository installation.  The installation token can
//! then be used for REST and GraphQL API calls with the permissions granted
//! to the app.
//!
//! Reference: <https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app>

use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use sha2::Sha256;
use signature::SignatureEncoding;
use worker::Fetch;

/// Generate a GitHub App JWT from the app ID and PEM-encoded RSA private key.
///
/// The JWT is valid for 10 minutes (GitHub's maximum) and backdated by 60
/// seconds to account for clock drift.
pub fn generate_jwt(app_id: &str, private_key_pem: &str, now_secs: u64) -> Result<String, String> {
    let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .map_err(|e| format!("Failed to parse RSA private key: {e}"))?;

    let signing_key = rsa::pkcs1v15::SigningKey::<Sha256>::new(private_key);

    // JWT header: RS256
    let header = r#"{"alg":"RS256","typ":"JWT"}"#;

    // JWT payload: iat (issued at, backdated 60s), exp (10 min from now), iss (app id)
    let iat = now_secs.saturating_sub(60);
    let exp = now_secs + 10 * 60;
    let payload = format!(r#"{{"iat":{iat},"exp":{exp},"iss":"{app_id}"}}"#);

    let encoded_header = base64url_encode(header.as_bytes());
    let encoded_payload = base64url_encode(payload.as_bytes());
    let signing_input = format!("{encoded_header}.{encoded_payload}");

    use signature::Signer;
    let sig = signing_key.sign(signing_input.as_bytes()).to_vec();
    let encoded_sig = base64url_encode(&sig);

    Ok(format!("{signing_input}.{encoded_sig}"))
}

/// Exchange a GitHub App JWT for an installation access token scoped to a
/// single repository.
///
/// Calls `POST /app/installations/{installation_id}/access_tokens` with the
/// JWT as a Bearer token and the `repositories` field set to scope the token
/// to the given repository.  Returns the installation token string.
pub async fn vend_installation_token(
    installation_id: u64,
    jwt: &str,
    repo_name: &str,
) -> Result<String, String> {
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");

    let mut headers = worker::Headers::new();
    headers.set("Accept", "application/vnd.github+json").ok();
    headers.set("Authorization", &format!("Bearer {jwt}")).ok();
    headers.set("User-Agent", "wreck-it-worker").ok();
    headers.set("X-GitHub-Api-Version", "2022-11-28").ok();
    headers.set("Content-Type", "application/json").ok();

    // Scope the token to the specific repository from the webhook payload.
    let body = serde_json::json!({ "repositories": [repo_name] });

    let request = worker::Request::new_with_init(
        &url,
        worker::RequestInit::new()
            .with_method(worker::Method::Post)
            .with_headers(headers)
            .with_body(Some(worker::wasm_bindgen::JsValue::from_str(
                &body.to_string(),
            ))),
    )
    .map_err(|e| format!("Failed to create token request: {e}"))?;

    let mut response = Fetch::Request(request)
        .send()
        .await
        .map_err(|e| format!("Token vending request failed: {e}"))?;

    let status = response.status_code();
    if status != 201 {
        let body = response.text().await.unwrap_or_default();
        return Err(format!(
            "Failed to vend installation token ({status}): {body}"
        ));
    }

    let body: serde_json::Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse token response: {e}"))?;

    body["token"]
        .as_str()
        .map(|s| s.to_string())
        .ok_or_else(|| "Missing 'token' field in installation token response".to_string())
}

// ---------------------------------------------------------------------------
// Base64url encoding (RFC 4648 §5, no padding)
// ---------------------------------------------------------------------------

const BASE64URL_CHARS: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url_encode(data: &[u8]) -> String {
    let mut result = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = if chunk.len() > 1 { chunk[1] as u32 } else { 0 };
        let b2 = if chunk.len() > 2 { chunk[2] as u32 } else { 0 };
        let triple = (b0 << 16) | (b1 << 8) | b2;

        result.push(BASE64URL_CHARS[((triple >> 18) & 0x3F) as usize] as char);
        result.push(BASE64URL_CHARS[((triple >> 12) & 0x3F) as usize] as char);
        if chunk.len() > 1 {
            result.push(BASE64URL_CHARS[((triple >> 6) & 0x3F) as usize] as char);
        }
        if chunk.len() > 2 {
            result.push(BASE64URL_CHARS[(triple & 0x3F) as usize] as char);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64url_encode_known() {
        // Standard test vectors adapted for base64url (no padding).
        assert_eq!(base64url_encode(b"Hello"), "SGVsbG8");
        assert_eq!(base64url_encode(b"Hi"), "SGk");
        assert_eq!(base64url_encode(b"abc"), "YWJj");
    }

    #[test]
    fn base64url_encode_uses_url_safe_chars() {
        // Bytes that would produce '+' and '/' in standard base64.
        let data = [0xfb, 0xff, 0xfe];
        let encoded = base64url_encode(&data);
        assert!(!encoded.contains('+'));
        assert!(!encoded.contains('/'));
        assert!(!encoded.contains('='));
        assert!(encoded
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'));
    }

    #[test]
    fn generate_jwt_produces_three_parts() {
        // Generate a test RSA key pair for JWT signing.
        use rsa::rand_core::OsRng;
        let private_key = RsaPrivateKey::new(&mut OsRng, 2048).unwrap();
        let pem =
            rsa::pkcs8::EncodePrivateKey::to_pkcs8_pem(&private_key, rsa::pkcs8::LineEnding::LF)
                .unwrap();

        let jwt = generate_jwt("12345", pem.as_ref(), 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have header.payload.signature");

        // Verify header contains RS256.
        let header_bytes = base64url_decode(parts[0]);
        let header = String::from_utf8(header_bytes).unwrap();
        assert!(header.contains("RS256"));

        // Verify payload contains the app ID.
        let payload_bytes = base64url_decode(parts[1]);
        let payload = String::from_utf8(payload_bytes).unwrap();
        assert!(payload.contains("12345"));
    }

    /// Minimal base64url decoder for test verification only.
    fn base64url_decode(input: &str) -> Vec<u8> {
        let mut buf = Vec::new();
        let mut accum: u32 = 0;
        let mut bits: u32 = 0;
        for ch in input.bytes() {
            let val = match ch {
                b'A'..=b'Z' => ch - b'A',
                b'a'..=b'z' => ch - b'a' + 26,
                b'0'..=b'9' => ch - b'0' + 52,
                b'-' => 62,
                b'_' => 63,
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
        buf
    }
}
