//! GitHub App authentication — JWT generation and installation token vending.
//!
//! A GitHub App authenticates by generating a short-lived JWT signed with its
//! RSA private key, then exchanging that JWT for an installation access token
//! scoped to a specific repository installation.  The installation token can
//! then be used for REST and GraphQL API calls with the permissions granted
//! to the app.
//!
//! Reference: <https://docs.github.com/en/apps/creating-github-apps/authenticating-with-a-github-app/generating-a-json-web-token-jwt-for-a-github-app>

use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs8::DecodePrivateKey;
use rsa::RsaPrivateKey;
use sha2::Sha256;
use signature::SignatureEncoding;
use worker::Fetch;

/// Generate a GitHub App JWT from the app ID and PEM-encoded RSA private key.
///
/// The JWT is valid for 10 minutes (GitHub's maximum) and backdated by 60
/// seconds to account for clock drift.  Accepts both PKCS#8
/// (`-----BEGIN PRIVATE KEY-----`) and PKCS#1
/// (`-----BEGIN RSA PRIVATE KEY-----`) PEM formats.
pub fn generate_jwt(app_id: &str, private_key_pem: &str, now_secs: u64) -> Result<String, String> {
    let private_key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
        .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
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
    worker::console_log!(
        "[wreck-it][auth] vending token for installation={} repo={}",
        installation_id,
        repo_name,
    );
    let url = format!("https://api.github.com/app/installations/{installation_id}/access_tokens");

    let headers = worker::Headers::new();
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
        worker::console_error!("[wreck-it][auth] token vending failed ({status}): {body}",);
        return Err(format!(
            "Failed to vend installation token ({status}): {body}"
        ));
    }
    worker::console_log!("[wreck-it][auth] token vended successfully");

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
        // Use a pre-generated 2048-bit RSA test key.  Generating one at
        // runtime via `RsaPrivateKey::new(&mut OsRng, 2048)` interacts
        // badly with the `spin` mutex used by the `rsa` crate and causes
        // the test harness to hang on exit.
        let pem = "-----BEGIN PRIVATE KEY-----\n\
MIIEvAIBADANBgkqhkiG9w0BAQEFAASCBKYwggSiAgEAAoIBAQC5QHAlqN27Tfon\n\
vQxeahClBM3E+a2ts0geMEJkE8YmuOaJrAJU7HvCOgceCjoHNqTuhPjqr0bsXPeP\n\
8XeI5BrfzWg8xbvl1lt+9mi0ppla2ockXzYPnsl131Gfin0Byf2FYR48KJHtbWLg\n\
23KnwfNDy8NvnszmcZ5vW1ia2IlGgGDLytf2HhvFVzEY/M3Sxuou0GfUHeOPdV0n\n\
IYug1z2uyZku75MDdhWyTOubWVosfepmr6dGd0fukNpf80mPsh/ozo/qQTdISrdM\n\
2djaN7Fqc406KqcLMnQP60z6RpMV9+IaranIuNbGY6CwOClyUT7jJTVgxg4abgct\n\
XR4I3aHRAgMBAAECggEACnfWXn/BAoPmLjdZQD86GuO/RPGJm5Z/7W9zW6MKascU\n\
BT+Puit1gC+LQL/523fTsMQZf27Re9XHtNNDmpwD22sIsuEcPGFKi6LHnpMIzhYp\n\
1ieohGVyo5Mvp5ZJzhS98Lrg3IFmYvDwH3NccpexHr71l5R0+6imoqBEzNXjm/TY\n\
VLlrPka9QbyREgoEtAtjlXAv7mneM4C/x3/z2wQxMv/m57SOA77h/qbXMrxbPqFl\n\
N9aL/kKO7IzFRmIsWrHkL45BLoFe2TACFINH3/COqd1/gSzr7xCxXTFA7xqpk3XY\n\
KjceK1FGECkAX01kJwQFmDcENNLL17RLPJpTwgXi4QKBgQD6Ljm+7r3cAYFOyOJh\n\
SZ/0BBMhSvnKzyexjhzR2HA6WTKVYMVeMIpX2A4CrmZVHzYBDucPDa/Ek1cT7MxR\n\
XX3kUpBfsTWY5aCe3FwakuU7qk59cOqFtcvvffBlb/JDCR4u8FcSO1b5h/4WG4eD\n\
90WIEAL1gAoDBT1CvVJkMgRJ/QKBgQC9j5MJGsdfZGgOAVyM1jMD72XxVG8gQqg6\n\
51Wx4adJQST2SJpZfIrX9/VqUHoMiqIEcpMxuE2s/H8w01KnHo4VPJPyaruFsQqO\n\
Gn7SkEpOsAzRkknHAjhzHLK6KxmxMCUqkQSsTmxSed1HRrsw/yiFXA4U2I3poweM\n\
eZyPDGyFZQKBgHBvB9qsFr1iG8fZdgu899rFXgePV3Vy5ebg9EjGmaFPZvFFHU44\n\
SGQ0IA/KawkETtPo66STRRP2F6NHv4ctmh9bj7DBxlGhmS7r36S9sbG/1yh+75cJ\n\
3c4S7k/YIKtJ1LvJnYf/DRZ1rJYo5x1Cqof8kifc1CMJXr+4r+eBpvXNAoGAJy/D\n\
CaLLjGDJUfveEg9FxI582ILH5jdhZ6vi/z7SwkYBShiAL/ebDEJqLWwtjuIp1BmL\n\
bD/ZbuVTtdg5weqDHMjFHNwLn/uVXwMDLKw/cDzcqYZAUi+XU9Se7fVy/johtMb9\n\
3FDp+7LNl6p7kAlvawI4tv59d8sICHYrczbySDECgYA/wnSBmedL9XLCQZpsDPyS\n\
M/JZ+qqV10EJaeUVXyzLLCD284QEeLbYhuWWaBCuNG1nry9mRu7qD5/RrbY2iHqb\n\
TFP9Qi4H/k4ElALICRDApsPXiLYmxvvu7qbMbGqvAqKhHfCLAQRLRPfz1PY0Zoxs\n\
2Rk0jLPIKBsyQ5YIyEYCtw==\n\
-----END PRIVATE KEY-----";

        let jwt = generate_jwt("12345", pem, 1_700_000_000).unwrap();
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

    #[test]
    fn generate_jwt_accepts_pkcs1_pem() {
        // PKCS#1 format key (BEGIN RSA PRIVATE KEY) as GitHub generates.
        let pem = "-----BEGIN RSA PRIVATE KEY-----\n\
MIIEpAIBAAKCAQEAjjRGwEEXjrKk33blQgqIjSOaBbG8TSGFJfQI2rgWVu2+xZr+\n\
jb/EczBSG1FAzIYmnpZgYwqEAeos6utqYouW9P6vPId9d/4JP+Ehb+7ci9Ugtp2F\n\
jnCHNGmfjJOhVwQMdOfSXP5TnksEoOJpOm3g5wSVYnZGB/iEBczyY872v9k61VtL\n\
zOFgquG7v6FM7GKbpr2U0padfLW8WRGJ9gA8+upuZ1yRiZ2ZumQdDUR8lBu4sGhu\n\
JMJe85qc6bjkhXCK1lhDz5zqqw5sUx/sfTtr84z7hX8BAZ7vJ4KfPzKV8c6RSIoZ\n\
Nsdlkh7llV7Xb1ANZ5VyzhItQOuKOPoKXY8LVwIDAQABAoIBAAWTdsn0UEH2Yh3b\n\
e3MS490M7iD9CCvKF0t9FzL99u18fKq8JvEhLOKlAwMyzqSpoGGlkEZlIJBmqB49\n\
RVQNGMoNVorvRFpsXRAV+gngLUiHMeiXPEGkMSXUyNg251bfJl6xj5kf5XpJtl1Z\n\
cDp45oTu50cPtjOuNf14PNgj3Gvg8yblmUaVrpWVgNx1wgyRqmOyUb1GJ61BfNcG\n\
5BVOoyfxzetA8gCfjnSc0D1WCylecLFkvNGPOUYn7na1QvXQiOwcdRT2ZrTLv4u9\n\
ieRQZEkuQ9o10np+5u5PzpDNClAFp6BiTgfZYsIpVREfYhdDzQV9mYgVwx3EFj/F\n\
bkssRBECgYEAyPQV0fl9jaGtTRyu9UCHbuYFCZbNZtvdQfw+VshdrtTVGsQ1jhuV\n\
SQgrViSLBHPkqLwuBYh0aO9tAWa/O/wd60RRpEL6oLZvlVUO/YFLReBdGPcf50KS\n\
YNiDfZf9xO/3GjT2YbKDnv1DcvOdivMRmFCu3Td27BsS5LfvGyWvepkCgYEAtShi\n\
VAMcilPj4EPjfQkbkowDA+RuU4RCNCPUgQAjGMy/zse0vsaY+Tvtf/xSNvlX/XYg\n\
ucpDrVEQMrV4Ks9ikY83+ZpF/s20OUmMejS2XrNwQViCnaEvcI+iqGBqrcu2TqW5\n\
c3EdzOHz+JfylrdjbV6vDf+29+AiARFXomRY228CgYEAvJ73WEcRhX6LV4Uj6Apw\n\
1TRM6CpHlFOthAFLVlPuM2uMt/oRttjHMGzdmJbmcgCCUauImyLw+Yo6zATwXVKR\n\
lsJiy4cfDvkPFaFoV6UjzWwClqtno7+F/CdejOW8ij0fuNabqSpRh0t8IwruBn2P\n\
N2QMLpKgKpBjFJJdeiLOaokCgYBNUgNF4F4aHFwyqEc8YtrF3cSbsK/2LYkkP/a/\n\
aJOSTjG/zDU1CAbaud1Qtx1QIXSQ1g55vf7MxsCnJBU6EHH9tqcpfdNKQfoeSWoP\n\
7te369aJzYFSTi21WVkPjLd7nmsdflZ9E1aoz/gVrqT39yYU1EjbLL2nZp6c3g4N\n\
Xc8fOQKBgQC47rHUJOoFUv3yJWyhj0sIzg/Sul20kVXG3wXUYX669J2uAAqHKthg\n\
MyxJVt5tmhTc0MCb7Ifw2sR68+QY3pcTIff3DK1LMwPJsX0w82Ytf54I4shimsti\n\
P6p7MJfHu0M6fe77vtvnXdeiXvrNg1lgGwb2MZjjk051u0YktWJ5ZA==\n\
-----END RSA PRIVATE KEY-----";

        let jwt = generate_jwt("99999", pem, 1_700_000_000).unwrap();
        let parts: Vec<&str> = jwt.split('.').collect();
        assert_eq!(parts.len(), 3, "JWT must have header.payload.signature");
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
