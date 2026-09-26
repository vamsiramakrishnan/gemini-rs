//! Who may open a session: bearer tokens for clients, signatures for
//! Twilio's webhook, and one-time stream tokens for Twilio's media socket.

use std::collections::HashMap;

use axum::http::HeaderMap;
use base64::Engine as _;
use hmac::{Hmac, Mac};
use parking_lot::Mutex;

/// The WebSocket subprotocol the runtime speaks. A browser cannot set an
/// `Authorization` header on a WebSocket, so it offers
/// `["adk.v1", "adk.token.<token>"]` and the server selects `adk.v1`.
pub const SUBPROTOCOL: &str = "adk.v1";
/// The prefix of the subprotocol entry that carries a token.
pub const TOKEN_SUBPROTOCOL_PREFIX: &str = "adk.token.";

/// Every credential a request presents: `Authorization: Bearer <t>`,
/// `X-API-Key: <t>`, or a `Sec-WebSocket-Protocol` entry `adk.token.<t>`.
pub(crate) fn presented_tokens(headers: &HeaderMap) -> Vec<String> {
    let mut tokens = Vec::new();
    for value in headers.get_all(axum::http::header::AUTHORIZATION) {
        if let Some(token) = value.to_str().ok().and_then(|v| {
            v.strip_prefix("Bearer ")
                .or_else(|| v.strip_prefix("bearer "))
        }) {
            tokens.push(token.trim().to_string());
        }
    }
    for value in headers.get_all("x-api-key") {
        if let Ok(token) = value.to_str() {
            tokens.push(token.trim().to_string());
        }
    }
    for value in headers.get_all(axum::http::header::SEC_WEBSOCKET_PROTOCOL) {
        if let Ok(list) = value.to_str() {
            tokens.extend(
                list.split(',')
                    .filter_map(|p| p.trim().strip_prefix(TOKEN_SUBPROTOCOL_PREFIX))
                    .map(str::to_string),
            );
        }
    }
    tokens
}

/// Whether any presented credential equals any accepted token.
pub(crate) fn authorized(headers: &HeaderMap, accepted: &[String]) -> bool {
    let presented = presented_tokens(headers);
    // Compare against every accepted token so timing does not reveal which
    // one (or whether an earlier one) nearly matched.
    let mut ok = false;
    for p in &presented {
        for a in accepted {
            ok |= constant_time_eq(p.as_bytes(), a.as_bytes());
        }
    }
    ok
}

fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

// ── Twilio request validation ───────────────────────────────────────────────

/// Twilio's `X-Twilio-Signature` for a POST to `url` with form `params`:
/// base64 of HMAC-SHA1, keyed with the account's auth token, over the full
/// URL followed by every parameter name and value, sorted by name.
pub fn twilio_signature(auth_token: &str, url: &str, params: &[(String, String)]) -> String {
    let mut sorted: Vec<&(String, String)> = params.iter().collect();
    sorted.sort();
    let mut mac = Hmac::<sha1::Sha1>::new_from_slice(auth_token.as_bytes())
        .expect("HMAC accepts any key length");
    mac.update(url.as_bytes());
    for (name, value) in sorted {
        mac.update(name.as_bytes());
        mac.update(value.as_bytes());
    }
    base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes())
}

/// Whether `signature` is Twilio's signature for this request. Twilio may
/// sign an `https` URL with or without its default port, so both forms are
/// tried.
pub fn twilio_signature_valid(
    auth_token: &str,
    url: &str,
    params: &[(String, String)],
    signature: &str,
) -> bool {
    url_variants(url).iter().any(|candidate| {
        constant_time_eq(
            twilio_signature(auth_token, candidate, params).as_bytes(),
            signature.trim().as_bytes(),
        )
    })
}

/// `url` as given, plus the same URL with its port written out if it had
/// none, or removed if it had one. (`url::Url` drops default ports when it
/// normalizes, so this edits the string.)
fn url_variants(url: &str) -> Vec<String> {
    let mut variants = vec![url.to_string()];
    if let Ok(parsed) = url::Url::parse(url)
        && let (Some(host), Some(port)) = (parsed.host_str(), parsed.port_or_known_default())
    {
        let with_port = format!("{host}:{port}");
        variants.push(if url.contains(&with_port) {
            url.replacen(&with_port, host, 1)
        } else {
            url.replacen(host, &with_port, 1)
        });
    }
    variants
}

// ── Stream tokens ───────────────────────────────────────────────────────────

/// How long a stream token minted for a call stays valid. Twilio opens the
/// media socket right after it receives the TwiML.
pub(crate) const STREAM_TOKEN_TTL_SECS: u64 = 60;

/// Why a stream token was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StreamTokenError {
    Malformed,
    Expired,
    BadSignature,
    AlreadyUsed,
}

/// One-time tokens that put Twilio's media socket behind the signed
/// webhook.
///
/// Twilio sends no custom headers on the media WebSocket, and `<Stream>`
/// URLs cannot carry a query string, so the token rides in the path:
/// `<expiry>.<nonce>.<mac>`, where the MAC (HMAC-SHA256 keyed with the
/// Twilio auth token) covers the bundle, the call SID, the expiry and the
/// nonce. The call SID is not in the URL: it is checked against the
/// stream's `start` frame, so a leaked URL alone does not open a session.
/// Tokens are stateless, so any instance can redeem one; each instance also
/// refuses a token it has already redeemed.
pub(crate) struct StreamTokens {
    key: Vec<u8>,
    used: Mutex<HashMap<String, u64>>,
}

impl StreamTokens {
    pub(crate) fn new(twilio_auth_token: &str) -> Self {
        Self {
            key: twilio_auth_token.as_bytes().to_vec(),
            used: Mutex::new(HashMap::new()),
        }
    }

    fn mac(&self, bundle: &str, call_sid: &str, expiry: u64, nonce: &str) -> String {
        let mut mac =
            Hmac::<sha2::Sha256>::new_from_slice(&self.key).expect("HMAC accepts any key length");
        mac.update(
            format!("adk-runtime stream v1\n{bundle}\n{call_sid}\n{expiry}\n{nonce}").as_bytes(),
        );
        mac.finalize()
            .into_bytes()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect()
    }

    /// A token for `call_sid` on `bundle`, valid for
    /// [`STREAM_TOKEN_TTL_SECS`] from `now`.
    pub(crate) fn mint(&self, bundle: &str, call_sid: &str, now: u64) -> String {
        let expiry = now + STREAM_TOKEN_TTL_SECS;
        let nonce = uuid::Uuid::new_v4().simple().to_string();
        let mac = self.mac(bundle, call_sid, expiry, &nonce);
        format!("{expiry}.{nonce}.{mac}")
    }

    fn parse(token: &str) -> Result<(u64, &str, &str), StreamTokenError> {
        let mut parts = token.split('.');
        let (Some(expiry), Some(nonce), Some(mac), None) =
            (parts.next(), parts.next(), parts.next(), parts.next())
        else {
            return Err(StreamTokenError::Malformed);
        };
        let hex = |s: &str, len: usize| s.len() == len && s.bytes().all(|b| b.is_ascii_hexdigit());
        if !hex(nonce, 32) || !hex(mac, 64) {
            return Err(StreamTokenError::Malformed);
        }
        let expiry = expiry.parse().map_err(|_| StreamTokenError::Malformed)?;
        Ok((expiry, nonce, mac))
    }

    /// A cheap check before the upgrade: well-formed and not expired.
    pub(crate) fn precheck(token: &str, now: u64) -> Result<(), StreamTokenError> {
        let (expiry, _, _) = Self::parse(token)?;
        if now > expiry {
            return Err(StreamTokenError::Expired);
        }
        Ok(())
    }

    /// Accept `token` for this bundle and call, once.
    pub(crate) fn redeem(
        &self,
        token: &str,
        bundle: &str,
        call_sid: &str,
        now: u64,
    ) -> Result<(), StreamTokenError> {
        let (expiry, nonce, mac) = Self::parse(token)?;
        if now > expiry {
            return Err(StreamTokenError::Expired);
        }
        let expected = self.mac(bundle, call_sid, expiry, nonce);
        if !constant_time_eq(expected.as_bytes(), mac.to_ascii_lowercase().as_bytes()) {
            return Err(StreamTokenError::BadSignature);
        }
        let mut used = self.used.lock();
        used.retain(|_, until| *until >= now);
        if used.insert(nonce.to_string(), expiry).is_some() {
            return Err(StreamTokenError::AlreadyUsed);
        }
        Ok(())
    }
}

/// Seconds since the Unix epoch.
pub(crate) fn unix_now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| d.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, &str)]) -> Vec<(String, String)> {
        pairs
            .iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect()
    }

    /// The worked example in Twilio's webhook security documentation.
    #[test]
    fn twilio_signature_matches_the_documented_example() {
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        let params = params(&[
            ("CallSid", "CA1234567890ABCDE"),
            ("Caller", "+12349013030"),
            ("Digits", "1234"),
            ("From", "+12349013030"),
            ("To", "+18005551212"),
        ]);
        assert_eq!(
            twilio_signature("12345", url, &params),
            "0/KCTR6DLpKmkAf8muzZqo1nDgQ="
        );
        assert!(twilio_signature_valid(
            "12345",
            url,
            &params,
            "0/KCTR6DLpKmkAf8muzZqo1nDgQ="
        ));
    }

    #[test]
    fn a_changed_parameter_url_or_token_fails_the_signature() {
        let url = "https://mycompany.com/myapp.php?foo=1&bar=2";
        let good = params(&[("CallSid", "CA1234567890ABCDE"), ("Digits", "1234")]);
        let signature = twilio_signature("12345", url, &good);
        let tampered = params(&[("CallSid", "CA1234567890ABCDE"), ("Digits", "9999")]);
        assert!(!twilio_signature_valid("12345", url, &tampered, &signature));
        assert!(!twilio_signature_valid(
            "12345",
            "https://evil.example/myapp.php?foo=1&bar=2",
            &good,
            &signature
        ));
        assert!(!twilio_signature_valid("54321", url, &good, &signature));
        assert!(!twilio_signature_valid("12345", url, &good, ""));
    }

    #[test]
    fn a_signature_over_the_url_with_its_default_port_is_accepted() {
        let params = params(&[("CallSid", "CA1")]);
        let signed = twilio_signature("t", "https://svc.example:443/twilio/voice/b", &params);
        assert!(twilio_signature_valid(
            "t",
            "https://svc.example/twilio/voice/b",
            &params,
            &signed
        ));
        let signed = twilio_signature("t", "https://svc.example/twilio/voice/b", &params);
        assert!(twilio_signature_valid(
            "t",
            "https://svc.example:443/twilio/voice/b",
            &params,
            &signed
        ));
    }

    #[test]
    fn bearer_api_key_and_subprotocol_tokens_are_read() {
        let mut headers = HeaderMap::new();
        headers.insert("authorization", "Bearer one".parse().unwrap());
        headers.insert("x-api-key", "two".parse().unwrap());
        headers.insert(
            "sec-websocket-protocol",
            "adk.v1, adk.token.three".parse().unwrap(),
        );
        assert_eq!(presented_tokens(&headers), ["one", "two", "three"]);
        assert!(authorized(&headers, &["three".into()]));
        assert!(!authorized(&headers, &["four".into()]));
        assert!(!authorized(&HeaderMap::new(), &["four".into()]));
    }

    #[test]
    fn a_stream_token_is_bound_to_its_call_and_used_once() {
        let tokens = StreamTokens::new("auth-token");
        let now = 1_000;
        let token = tokens.mint("booking", "CA1", now);
        assert_eq!(StreamTokens::precheck(&token, now), Ok(()));
        assert_eq!(
            tokens.redeem(&token, "booking", "CA2", now),
            Err(StreamTokenError::BadSignature)
        );
        assert_eq!(
            tokens.redeem(&token, "clinic", "CA1", now),
            Err(StreamTokenError::BadSignature)
        );
        assert_eq!(tokens.redeem(&token, "booking", "CA1", now), Ok(()));
        assert_eq!(
            tokens.redeem(&token, "booking", "CA1", now),
            Err(StreamTokenError::AlreadyUsed)
        );
    }

    #[test]
    fn a_stream_token_expires_and_needs_the_right_key() {
        let tokens = StreamTokens::new("auth-token");
        let token = tokens.mint("booking", "CA1", 1_000);
        let late = 1_000 + STREAM_TOKEN_TTL_SECS + 1;
        assert_eq!(
            StreamTokens::precheck(&token, late),
            Err(StreamTokenError::Expired)
        );
        assert_eq!(
            StreamTokens::new("other").redeem(&token, "booking", "CA1", 1_000),
            Err(StreamTokenError::BadSignature)
        );
        assert_eq!(
            StreamTokens::precheck("not-a-token", 1_000),
            Err(StreamTokenError::Malformed)
        );
    }
}
