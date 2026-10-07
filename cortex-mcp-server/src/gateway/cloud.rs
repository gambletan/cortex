//! Cortex Cloud device protocol, shared by the device (client, here) and the hosted
//! service (`cortex-cloud`, which verifies with [`verify`]). Design:
//! `docs/design/muse-cloud.md`.
//!
//! ```text
//! device ──signed──▶ POST   /api/tenants                {public_key}      → {rid, mcp_url}
//!                    PUT    /api/tenants/<rid>/export   {items:[…]}       → {count}
//!                    POST   /api/tenants/<rid>/enroll                     → {mcp_url, expires_in}
//!                    GET    /api/tenants/<rid>/inbox                      → {items:[…]}
//!                    POST   /api/tenants/<rid>/inbox/ack {ids:[…]}        → {removed}
//!                    GET    /api/tenants/<rid>/status                     → {shared, connected, last_used}
//!                    DELETE /api/tenants/<rid>                            → wiped
//! ```
//!
//! Every request carries `x-cortex-key` (base64 Ed25519 public key), `x-cortex-ts` (unix
//! seconds), `x-cortex-nonce` (base64url, 16 bytes) and `x-cortex-sig` over
//! [`canonical`]. No bearer secret ever crosses the wire.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub const MAX_SKEW_SECS: i64 = 300;
pub const H_KEY: &str = "x-cortex-key";
pub const H_TS: &str = "x-cortex-ts";
pub const H_NONCE: &str = "x-cortex-nonce";
pub const H_SIG: &str = "x-cortex-sig";
/// Where the hosted service lives; `CORTEX_CLOUD_URL` overrides (self-hosting, tests).
pub const DEFAULT_CLOUD_URL: &str = "https://studio.alvinsclub.ai";
const PROTOCOL: &str = "cortex-cloud-v1";
/// Error text when the service no longer knows this device's tenant (deleted or expired).
pub const GONE: &str = "Cortex Cloud no longer has this connection";

/// The exact bytes that are signed. `path_and_query` is the request target as sent.
pub fn canonical(method: &str, path_and_query: &str, body: &[u8], ts: i64, nonce: &str) -> Vec<u8> {
    let body_hash: String = Sha256::digest(body).iter().map(|b| format!("{b:02x}")).collect();
    format!("{PROTOCOL}\n{}\n{path_and_query}\n{body_hash}\n{ts}\n{nonce}", method.to_ascii_uppercase()).into_bytes()
}

/// A nonce is 16 random bytes, base64url: 22 chars.
pub fn is_nonce(s: &str) -> bool {
    s.len() == 22 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Why a signed request was refused (server side).
#[derive(Debug, PartialEq)]
pub enum VerifyError {
    Missing,
    BadKey,
    Stale,
    BadSignature,
}

/// Verify a signed request against `expected_key` (base64), or against the key in the
/// request when `expected_key` is `None` (registration: proof of possession).
/// Returns the verified public key (base64) and the nonce; the caller must reject a nonce
/// it has seen inside the skew window.
pub fn verify(
    method: &str,
    path_and_query: &str,
    body: &[u8],
    header: impl Fn(&str) -> Option<String>,
    expected_key: Option<&str>,
    now: i64,
) -> Result<(String, String), VerifyError> {
    let (Some(key_b64), Some(ts), Some(nonce), Some(sig)) = (header(H_KEY), header(H_TS), header(H_NONCE), header(H_SIG))
    else {
        return Err(VerifyError::Missing);
    };
    if expected_key.is_some_and(|k| k != key_b64) {
        return Err(VerifyError::BadKey);
    }
    let key_bytes: [u8; 32] = STANDARD
        .decode(&key_b64)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(VerifyError::BadKey)?;
    let key = VerifyingKey::from_bytes(&key_bytes).map_err(|_| VerifyError::BadKey)?;
    let ts: i64 = ts.parse().map_err(|_| VerifyError::Stale)?;
    if (now - ts).abs() > MAX_SKEW_SECS || !is_nonce(&nonce) {
        return Err(VerifyError::Stale);
    }
    let sig_bytes: [u8; 64] = STANDARD
        .decode(&sig)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(VerifyError::BadSignature)?;
    key.verify_strict(&canonical(method, path_and_query, body, ts, &nonce), &Signature::from_bytes(&sig_bytes))
        .map_err(|_| VerifyError::BadSignature)?; // strict: rejects malleable signatures
    Ok((key_b64, nonce))
}

// ── Device side ──────────────────────────────────────────────────────────────

/// The device's identity: its Ed25519 key, the tenant it registered, and the service URL.
pub struct Device {
    key: SigningKey,
    pub rid: Option<String>,
    pub base_url: String,
}

impl Device {
    pub fn new(secret: [u8; 32], rid: Option<String>, base_url: String) -> Self {
        Self { key: SigningKey::from_bytes(&secret), rid, base_url }
    }

    pub fn generate_secret() -> [u8; 32] {
        let mut b = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut b);
        b
    }

    pub fn public_key_b64(&self) -> String {
        STANDARD.encode(self.key.verifying_key().as_bytes())
    }

    fn call(&self, method: &str, path: &str, body: Option<&Value>) -> Result<Value, String> {
        let body_bytes = body.map(|b| b.to_string().into_bytes()).unwrap_or_default();
        let ts = chrono::Utc::now().timestamp();
        let mut n = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut n);
        let nonce = URL_SAFE_NO_PAD.encode(n);
        let sig = self.key.sign(&canonical(method, path, &body_bytes, ts, &nonce));
        let url = format!("{}{path}", self.base_url.trim_end_matches('/'));
        let req = ureq::request(method, &url)
            .timeout(std::time::Duration::from_secs(20))
            .set(H_KEY, &self.public_key_b64())
            .set(H_TS, &ts.to_string())
            .set(H_NONCE, &nonce)
            .set(H_SIG, &STANDARD.encode(sig.to_bytes()))
            .set("content-type", "application/json");
        let resp = if body.is_some() { req.send_bytes(&body_bytes) } else { req.call() };
        match resp {
            Ok(r) => {
                let text = r.into_string().map_err(|e| e.to_string())?;
                if text.trim().is_empty() {
                    Ok(json!({}))
                } else {
                    serde_json::from_str(&text).map_err(|_| "unexpected response from Cortex Cloud".to_string())
                }
            }
            Err(ureq::Error::Status(404, _)) => Err(GONE.to_string()),
            Err(ureq::Error::Status(code, r)) => {
                let msg = r
                    .into_string()
                    .ok()
                    .and_then(|t| serde_json::from_str::<Value>(&t).ok())
                    .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
                    .unwrap_or_default();
                Err(format!("Cortex Cloud refused the request ({code}) {msg}").trim().to_string())
            }
            Err(e) => Err(format!("could not reach Cortex Cloud: {}", e.kind())),
        }
    }

    fn rid(&self) -> Result<&str, String> {
        self.rid.as_deref().ok_or_else(|| "not connected to Cortex Cloud yet".to_string())
    }

    /// Create the tenant (proof of possession of the key). Returns the tenant id.
    pub fn register(&mut self) -> Result<String, String> {
        let v = self.call("POST", "/api/tenants", Some(&json!({ "public_key": self.public_key_b64() })))?;
        let rid = v.get("rid").and_then(Value::as_str).ok_or("bad registration response")?.to_string();
        self.rid = Some(rid.clone());
        Ok(rid)
    }

    /// Replace the cloud copy of the shared list. `version` is when the snapshot was taken
    /// (ms); the service refuses anything older than what it already applied.
    pub fn push_export(&self, version: i64, items: &[(String, Option<Vec<f32>>)]) -> Result<u64, String> {
        let items: Vec<Value> = items.iter().map(|(t, e)| json!({ "text": t, "embedding": e })).collect();
        let body = json!({ "version": version, "items": items });
        let v = self.call("PUT", &format!("/api/tenants/{}/export", self.rid()?), Some(&body))?;
        Ok(v.get("count").and_then(Value::as_u64).unwrap_or(0))
    }

    /// Open a one-sign-in window; returns the link to paste into Muse.
    pub fn enroll(&self) -> Result<(String, u64), String> {
        let v = self.call("POST", &format!("/api/tenants/{}/enroll", self.rid()?), Some(&json!({})))?;
        let url = v.get("mcp_url").and_then(Value::as_str).ok_or("bad enroll response")?.to_string();
        Ok((url, v.get("expires_in").and_then(Value::as_u64).unwrap_or(0)))
    }

    pub fn inbox(&self) -> Result<Vec<(String, String)>, String> {
        let v = self.call("GET", &format!("/api/tenants/{}/inbox", self.rid()?), None)?;
        Ok(v.get("items")
            .and_then(Value::as_array)
            .map(|a| {
                a.iter()
                    .filter_map(|i| Some((i.get("id")?.as_str()?.to_string(), i.get("text")?.as_str()?.to_string())))
                    .collect()
            })
            .unwrap_or_default())
    }

    pub fn inbox_ack(&self, ids: &[String]) -> Result<u64, String> {
        let v = self.call("POST", &format!("/api/tenants/{}/inbox/ack", self.rid()?), Some(&json!({ "ids": ids })))?;
        Ok(v.get("removed").and_then(Value::as_u64).unwrap_or(0))
    }

    pub fn status(&self) -> Result<Value, String> {
        self.call("GET", &format!("/api/tenants/{}/status", self.rid()?), None)
    }

    pub fn delete(&self) -> Result<(), String> {
        self.call("DELETE", &format!("/api/tenants/{}", self.rid()?), None).map(|_| ())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn signed(dev: &Device, method: &str, path: &str, body: &[u8], ts: i64) -> impl Fn(&str) -> Option<String> {
        let nonce = URL_SAFE_NO_PAD.encode([7u8; 16]);
        let sig = STANDARD.encode(dev.key.sign(&canonical(method, path, body, ts, &nonce)).to_bytes());
        let key = dev.public_key_b64();
        move |h: &str| match h {
            H_KEY => Some(key.clone()),
            H_TS => Some(ts.to_string()),
            H_NONCE => Some(nonce.clone()),
            H_SIG => Some(sig.clone()),
            _ => None,
        }
    }

    #[test]
    fn verify_accepts_exactly_what_was_signed() {
        let dev = Device::new(Device::generate_secret(), None, "https://x".into());
        let other = Device::new(Device::generate_secret(), None, "https://x".into());
        let t = 1_800_000_000;
        let h = signed(&dev, "PUT", "/api/tenants/a/export", b"{}", t);
        assert!(verify("PUT", "/api/tenants/a/export", b"{}", &h, None, t).is_ok());
        assert!(verify("PUT", "/api/tenants/a/export", b"{}", &h, Some(&dev.public_key_b64()), t + 10).is_ok());
        // Any change → refused.
        assert_eq!(verify("PUT", "/api/tenants/b/export", b"{}", &h, None, t), Err(VerifyError::BadSignature));
        assert_eq!(verify("PUT", "/api/tenants/a/export", b"{ }", &h, None, t), Err(VerifyError::BadSignature));
        assert_eq!(verify("POST", "/api/tenants/a/export", b"{}", &h, None, t), Err(VerifyError::BadSignature));
        assert_eq!(verify("PUT", "/api/tenants/a/export", b"{}", &h, Some(&other.public_key_b64()), t), Err(VerifyError::BadKey));
        assert_eq!(verify("PUT", "/api/tenants/a/export", b"{}", &h, None, t + MAX_SKEW_SECS + 1), Err(VerifyError::Stale));
        assert_eq!(verify("PUT", "/api/tenants/a/export", b"{}", |_| None, None, t), Err(VerifyError::Missing));
    }
}
