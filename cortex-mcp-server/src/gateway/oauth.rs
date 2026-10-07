//! Single-user OAuth 2.1 authorization server for the Muse gateway.
//! Design: `docs/design/muse-oauth.md`.
//!
//! ```text
//! Muse ─ GET /.well-known/oauth-protected-resource/mcp ─▶ issuer
//!      ─ GET /.well-known/oauth-authorization-server   ─▶ endpoints
//!      ─ POST /register (redirect must be allowlisted)  ─▶ client_id [+ secret]
//!      ─ GET /authorize (PKCE S256)  ─▶ page: "run `gateway connect ABCD-EFGH`"
//!                                         │ (refreshes /authorize/wait?req=…)
//! user ─ gateway connect ABCD-EFGH ─ y ─▶ pending.status = approved
//!      ─ next poll: delete pending, mint code (hash stored) ─▶ 302 callback?code
//! Muse ─ POST /token (code + verifier) ─▶ access (1 h) + refresh (30 d, rotating)
//!      ─ POST /mcp  Bearer <access>
//! ```
//!
//! Nothing in the browser grants access: only the local `connect` does. State lives in
//! `gateway-oauth.json` (hashes only), every read-modify-write under `gateway-oauth.lock`.

use std::collections::HashMap;
use std::io::Write as _;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::{RawQuery, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use super::{
    create_temp_beside, ct_eq, is_invisible, private_options, read_body, read_private, sync_parent, terminal_safe,
    HttpState, Paths,
};

pub(super) const MUSE_CALLBACK: &str = "https://agent.meta.ai/api/hatch/oauth/callback";
pub(super) const SCOPE: &str = "memory";
const MAX_CLIENTS: usize = 50;
const MAX_PENDING: usize = 10;
const MAX_REDIRECTS: usize = 5;
const MAX_URI_CHARS: usize = 512;
const MAX_NAME_CHARS: usize = 100;
const MAX_FORM_BYTES: usize = 8 * 1024;
const PENDING_TTL: i64 = 10 * 60;
const CODE_TTL: i64 = 60;
const ACCESS_TTL: i64 = 60 * 60;
const REFRESH_TTL: i64 = 30 * 24 * 60 * 60;
const GRANT_MAX_AGE: i64 = 180 * 24 * 60 * 60;
const MAX_USED_REFRESH: usize = 64;
const LAST_USED_RESOLUTION: i64 = 60;
/// No 0/O/1/I: the user reads the code off one screen and types it into another.
const CODE_ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
const AUTH_METHODS: [&str; 3] = ["none", "client_secret_post", "client_secret_basic"];
const MAX_STATE_CHARS: usize = 1024;
const WORKER_WAIT_SECS: u64 = 3;
const RATE_REGISTER: u32 = 10;
const RATE_AUTHORIZE: u32 = 20;
/// The sign-in page refreshes every 3 s (20/min): room for every allowed pending sign-in
/// (MAX_PENDING) with headroom. Over it, the page keeps polling slower instead of stopping.
const RATE_WAIT: u32 = 300;
/// Per registered client (an attacker can't spend a client_id it doesn't know).
const RATE_TOKEN_PER_CLIENT: u32 = 60;

// ── Config ───────────────────────────────────────────────────────────────────

pub(super) struct OAuthConfig {
    pub issuer: String,
    pub resource: String,
    pub redirect_allowlist: Vec<String>,
    /// endpoint → (window start, requests in window): a fixed one-minute window.
    rate: Mutex<HashMap<String, (Instant, u32)>>,
}

impl OAuthConfig {
    pub fn new(public_url: &str, extra_redirects: &[String]) -> Result<Self, String> {
        let issuer = validate_public_url(public_url)?;
        let mut redirect_allowlist = vec![MUSE_CALLBACK.to_string()];
        for r in extra_redirects {
            validate_redirect(r)?;
            if !redirect_allowlist.contains(r) {
                redirect_allowlist.push(r.clone());
            }
        }
        Ok(Self {
            resource: format!("{issuer}/mcp"),
            issuer,
            redirect_allowlist,
            rate: Mutex::new(HashMap::new()),
        })
    }

    fn prm_url(&self) -> String {
        format!("{}/.well-known/oauth-protected-resource/mcp", self.issuer)
    }

    /// The `WWW-Authenticate` challenge on a 401 from `/mcp`.
    pub fn challenge(&self, invalid_token: bool) -> String {
        let mut c = format!("Bearer resource_metadata=\"{}\", scope=\"{SCOPE}\"", self.prm_url());
        if invalid_token {
            c.push_str(", error=\"invalid_token\"");
        }
        c
    }

    fn allow(&self, bucket: &str, per_minute: u32) -> bool {
        let mut rate = self.rate.lock().unwrap_or_else(|p| p.into_inner());
        let now = Instant::now();
        // Bounded: fixed endpoint names plus at most MAX_CLIENTS client buckets live at once.
        if rate.len() > 4 * MAX_CLIENTS {
            rate.retain(|_, v| now.duration_since(v.0).as_secs() < 60);
        }
        let slot = rate.entry(bucket.to_string()).or_insert((now, 0));
        if now.duration_since(slot.0).as_secs() >= 60 {
            *slot = (now, 0);
        }
        slot.1 += 1;
        slot.1 <= per_minute
    }
}

/// `https://host[:port]` with nothing after it (a trailing `/` is dropped).
fn validate_public_url(url: &str) -> Result<String, String> {
    let err = || "--public-url must be https://<host>[:port] with no path, query or fragment".to_string();
    let host = url.strip_prefix("https://").ok_or_else(err)?;
    let host = host.strip_suffix('/').unwrap_or(host);
    let ok = !host.is_empty()
        && host.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    if !ok {
        return Err(err());
    }
    Ok(format!("https://{}", host.to_ascii_lowercase()))
}

/// The authority of an absolute `scheme://authority/...` URI must be a plain host[:port]:
/// no userinfo (`http://localhost:@evil.example/` sends the browser to evil.example).
fn plain_authority(rest: &str) -> Option<&str> {
    let authority = rest.split(['/', '?']).next()?;
    let ok = !authority.is_empty()
        && authority.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']'));
    ok.then_some(authority)
}

fn validate_redirect(uri: &str) -> Result<(), String> {
    let scheme_ok = if let Some(rest) = uri.strip_prefix("https://") {
        plain_authority(rest).is_some()
    } else if let Some(rest) = uri.strip_prefix("http://") {
        plain_authority(rest).is_some_and(|a| {
            let host = a.rsplit_once(':').filter(|(_, p)| p.chars().all(|c| c.is_ascii_digit())).map_or(a, |(h, _)| h);
            host == "127.0.0.1" || host == "localhost"
        })
    } else {
        false
    };
    let ok = uri.chars().count() <= MAX_URI_CHARS
        && !uri.contains('#')
        && !uri.chars().any(|c| c.is_whitespace() || c.is_control())
        && scheme_ok;
    if ok {
        Ok(())
    } else {
        Err(format!("--oauth-redirect {uri:?} must be an https (or http loopback) URI without a fragment"))
    }
}

// ── State ────────────────────────────────────────────────────────────────────

#[derive(Serialize, Deserialize, Default, Debug)]
pub(super) struct OAuthState {
    #[serde(default)]
    clients: Vec<Client>,
    #[serde(default)]
    pending: Vec<Pending>,
    #[serde(default)]
    codes: Vec<Code>,
    #[serde(default)]
    grants: Vec<Grant>,
    #[serde(default)]
    redeemed: Vec<Redeemed>,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Client {
    id: String,
    name: String,
    redirect_uris: Vec<String>,
    auth_method: String,
    secret_hash: Option<String>,
    grant_types: Vec<String>,
    created: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
#[serde(rename_all = "snake_case")]
enum Status {
    Waiting,
    Approved,
    Denied,
}

/// An authorization in progress. Every field except `status` is fixed at `/authorize`.
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Pending {
    req_hash: String,
    display_code: String,
    client_id: String,
    redirect_uri: String,
    state: Option<String>,
    challenge: String,
    scope: String,
    /// The audience the sign-in is for (shown by `connect`).
    resource: String,
    created: i64,
    status: Status,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Code {
    hash: String,
    client_id: String,
    redirect_uri: String,
    challenge: String,
    scope: String,
    resource: String,
    expires: i64,
}

/// A code already exchanged, kept until it would have expired: if it is presented again,
/// someone else has a copy, and the grant it produced is revoked (RFC 6749 §4.1.2).
#[derive(Serialize, Deserialize, Debug, Clone)]
struct Redeemed {
    hash: String,
    grant_id: String,
    expires: i64,
}

#[derive(Serialize, Deserialize, Debug, Clone)]
struct Grant {
    id: String,
    client_id: String,
    scope: String,
    resource: String,
    created: i64,
    last_used: i64,
    access_hash: String,
    access_exp: i64,
    refresh_hash: String,
    refresh_exp: i64,
    /// Refresh tokens already rotated away; presenting one revokes the grant.
    used_refresh: Vec<String>,
}

impl OAuthState {
    /// Drop everything expired. Clients stay (eviction happens only on registration).
    fn sweep(&mut self, now: i64) {
        self.pending.retain(|p| now - p.created < PENDING_TTL);
        self.codes.retain(|c| c.expires > now);
        self.redeemed.retain(|r| r.expires > now);
        self.grants.retain(|g| g.refresh_exp > now && now - g.created < GRANT_MAX_AGE);
    }

    fn client(&self, id: &str) -> Option<&Client> {
        self.clients.iter().find(|c| c.id == id)
    }

    fn client_referenced(&self, id: &str) -> bool {
        self.pending.iter().any(|p| p.client_id == id)
            || self.codes.iter().any(|c| c.client_id == id)
            || self.grants.iter().any(|g| g.client_id == id)
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

pub(super) fn sha256_hex(s: &str) -> String {
    Sha256::digest(s.as_bytes()).iter().map(|b| format!("{b:02x}")).collect()
}

fn random_token() -> String {
    let mut b = [0u8; 32];
    rand::thread_rng().fill_bytes(&mut b);
    URL_SAFE_NO_PAD.encode(b)
}

fn display_code() -> String {
    let mut b = [0u8; 8];
    rand::thread_rng().fill_bytes(&mut b);
    // 32 symbols: every byte maps uniformly.
    let c: String = b.iter().map(|x| CODE_ALPHABET[(*x as usize) % CODE_ALPHABET.len()] as char).collect();
    format!("{}-{}", &c[..4], &c[4..])
}

/// `ABCD-EFGH`, `abcdefgh` and `abcd efgh` all mean the same code.
pub(super) fn normalize_code(s: &str) -> String {
    let c: String = s.chars().filter(|c| c.is_ascii_alphanumeric()).map(|c| c.to_ascii_uppercase()).collect();
    if c.len() == 8 {
        format!("{}-{}", &c[..4], &c[4..])
    } else {
        c
    }
}

fn load(paths: &Paths) -> Result<OAuthState, String> {
    match read_private(&paths.oauth) {
        Ok(s) => serde_json::from_str(&s).map_err(|_| "OAuth state is corrupt".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(OAuthState::default()),
        Err(_) => Err("OAuth state is unreadable".into()),
    }
}

fn save(paths: &Paths, st: &OAuthState) -> Result<(), String> {
    let data = serde_json::to_vec(st).map_err(|e| e.to_string())?;
    let write = || -> std::io::Result<()> {
        let (tmp, mut f) = create_temp_beside(&paths.oauth)?;
        f.write_all(&data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, &paths.oauth)?;
        sync_parent(&paths.oauth)
    };
    write().map_err(|e| e.to_string())
}

/// Read-modify-write under the exclusive lock shared by the server and the CLI. Expired
/// entries are swept first; the state is persisted (durably) before `f`'s result is
/// returned, so nothing is handed out that a crash could forget.
pub(super) fn with_state<T>(paths: &Paths, f: impl FnOnce(&mut OAuthState) -> T) -> Result<T, String> {
    let lock = private_options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.oauth_lock)
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let out = (|| {
        let mut st = load(paths)?;
        st.sweep(now());
        let out = f(&mut st);
        save(paths, &st)?;
        Ok(out)
    })();
    let _ = lock.unlock();
    out
}

/// Same lock, no write unless `f` says so (polls and lookups must not cost an fsync).
/// Callers that need expiry must check it themselves (no sweep without a write).
fn with_state_read<T>(paths: &Paths, f: impl FnOnce(&mut OAuthState) -> (T, bool)) -> Result<T, String> {
    let lock = private_options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.oauth_lock)
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let out = (|| {
        let mut st = load(paths)?;
        let (out, dirty) = f(&mut st);
        if dirty {
            st.sweep(now());
            save(paths, &st)?;
        }
        Ok(out)
    })();
    let _ = lock.unlock();
    out
}

/// Read-only snapshot under a SHARED lock: concurrent token checks don't queue behind each
/// other, only behind writers.
fn read_shared(paths: &Paths) -> Result<OAuthState, String> {
    let lock = private_options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.oauth_lock)
        .map_err(|e| e.to_string())?;
    lock.lock_shared().map_err(|e| e.to_string())?;
    let st = load(paths);
    let _ = lock.unlock();
    st
}

/// Everything we mint (tokens, `req`) is 32 random bytes in unpadded base64url: 43 chars.
/// Anything else is rejected before touching the disk.
pub(super) fn is_token_shaped(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

// ── /mcp authentication ──────────────────────────────────────────────────────

/// The grant id behind a live access token for `resource`, or `None`. Errors (corrupt or
/// unreadable state) also mean "no": OAuth fails closed, the static token is unaffected.
/// The kill switch is NOT checked here: a valid token while OFF is refused by the request
/// handler (403 / tool error), so clients don't mistake "suspended" for "revoked".
pub(super) fn grant_for_access_token(paths: &Paths, resource: &str, token: &str) -> Option<String> {
    if !is_token_shaped(token) {
        return None;
    }
    let h = sha256_hex(token);
    let t = now();
    let live = |g: &Grant| {
        ct_eq(g.access_hash.as_bytes(), h.as_bytes())
            && g.access_exp > t
            && g.resource == resource
            && t - g.created < GRANT_MAX_AGE
    };
    let st = read_shared(paths).ok()?;
    let g = st.grants.iter().find(|g| live(g))?;
    if t - g.last_used >= LAST_USED_RESOLUTION {
        // Best effort, at most once a minute per grant.
        let id = g.id.clone();
        let _ = with_state_read(paths, |s| match s.grants.iter_mut().find(|g| g.id == id) {
            Some(g) => {
                g.last_used = t;
                ((), true)
            }
            None => ((), false),
        });
    }
    Some(g.id.clone())
}

// ── HTTP helpers ─────────────────────────────────────────────────────────────

fn json_response(status: StatusCode, body: Value) -> Response {
    (
        status,
        [("content-type", "application/json"), ("cache-control", "no-store"), ("pragma", "no-cache")],
        body.to_string(),
    )
        .into_response()
}

fn oauth_error(status: StatusCode, error: &str, desc: &str) -> Response {
    json_response(status, json!({ "error": error, "error_description": desc }))
}

fn html_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            c => out.push(c),
        }
    }
    out
}

/// `refresh`: (seconds, URL) for a `<meta http-equiv="refresh">`.
fn html_page(status: StatusCode, title: &str, body_html: &str, refresh: Option<(u32, &str)>) -> Response {
    let meta = refresh
        .map(|(secs, u)| format!("<meta http-equiv=\"refresh\" content=\"{secs};url={}\">", html_escape(u)))
        .unwrap_or_default();
    let page = format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\">\
         <meta name=\"viewport\" content=\"width=device-width,initial-scale=1\">{meta}\
         <title>{}</title><style>body{{font-family:system-ui,sans-serif;max-width:34rem;margin:3rem auto;\
         padding:0 1rem;line-height:1.5;color:#1a1a1a;background:#fff}}code{{background:#f2f2f2;padding:.15rem .35rem;\
         border-radius:4px;word-break:break-all}}.code{{font:700 2rem ui-monospace,monospace;letter-spacing:.1em}}\
         .muted{{color:#666;font-size:.9rem}}</style></head><body>{body_html}</body></html>",
        html_escape(title)
    );
    (
        status,
        [
            ("content-type", "text/html; charset=utf-8"),
            ("cache-control", "no-store"),
            ("x-frame-options", "DENY"),
            (
                "content-security-policy",
                "default-src 'none'; style-src 'unsafe-inline'; frame-ancestors 'none'",
            ),
            ("referrer-policy", "no-referrer"),
            ("x-content-type-options", "nosniff"),
        ],
        page,
    )
        .into_response()
}

fn bad_request_page(msg: &str) -> Response {
    html_page(
        StatusCode::BAD_REQUEST,
        "Cortex",
        &format!("<h1>Can't continue</h1><p>{}</p>", html_escape(msg)),
        None,
    )
}

/// Redirect to a **validated** redirect URI with OAuth response parameters.
fn redirect_with(cfg: &OAuthConfig, redirect_uri: &str, params: &[(&str, &str)], state: Option<&str>) -> Response {
    let mut pairs: Vec<(&str, &str)> = params.to_vec();
    if let Some(s) = state {
        pairs.push(("state", s));
    }
    pairs.push(("iss", &cfg.issuer));
    let query = serde_urlencoded::to_string(&pairs).unwrap_or_default();
    let sep = if redirect_uri.contains('?') { '&' } else { '?' };
    (
        StatusCode::FOUND,
        [
            ("location", format!("{redirect_uri}{sep}{query}")),
            ("cache-control", "no-store".to_string()),
            ("referrer-policy", "no-referrer".to_string()),
        ],
    )
        .into_response()
}

/// Query/form → map. A repeated parameter is an error (RFC 6749 §3.1).
fn parse_params(raw: &str) -> Result<HashMap<String, String>, ()> {
    let pairs: Vec<(String, String)> = serde_urlencoded::from_str(raw).map_err(|_| ())?;
    let mut map = HashMap::new();
    for (k, v) in pairs {
        if map.insert(k, v).is_some() {
            return Err(());
        }
    }
    Ok(map)
}

/// Run state work off the async runtime (the file lock blocks), bounded by the OAuth lane's
/// own worker pool. The work is short, so wait briefly for a worker instead of failing.
async fn blocking(st: &Arc<HttpState>, f: impl FnOnce() -> Response + Send + 'static) -> Response {
    blocking_or(st, f, || oauth_error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable", "Server busy")).await
}

async fn blocking_or(
    st: &Arc<HttpState>,
    f: impl FnOnce() -> Response + Send + 'static,
    busy: impl FnOnce() -> Response,
) -> Response {
    let acquire = st.oauth_workers.clone().acquire_owned();
    let Ok(Ok(permit)) = tokio::time::timeout(std::time::Duration::from_secs(WORKER_WAIT_SECS), acquire).await else {
        return busy();
    };
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        f()
    })
    .await
    .unwrap_or_else(|_| oauth_error(StatusCode::INTERNAL_SERVER_ERROR, "server_error", "Internal error"))
}

fn state_error() -> Response {
    oauth_error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable", "Authorization state unavailable")
}

fn cfg(st: &HttpState) -> &OAuthConfig {
    st.oauth.as_ref().expect("OAuth routes are only mounted with --oauth")
}

// ── Discovery ────────────────────────────────────────────────────────────────

pub(super) async fn protected_resource(State(st): State<Arc<HttpState>>) -> Response {
    let c = cfg(&st);
    json_response(
        StatusCode::OK,
        json!({
            "resource": c.resource,
            "authorization_servers": [c.issuer],
            "bearer_methods_supported": ["header"],
            "scopes_supported": [SCOPE],
        }),
    )
}

pub(super) async fn authorization_server(State(st): State<Arc<HttpState>>) -> Response {
    let c = cfg(&st);
    json_response(
        StatusCode::OK,
        json!({
            "issuer": c.issuer,
            "authorization_endpoint": format!("{}/authorize", c.issuer),
            "token_endpoint": format!("{}/token", c.issuer),
            "registration_endpoint": format!("{}/register", c.issuer),
            "scopes_supported": [SCOPE],
            "response_types_supported": ["code"],
            "response_modes_supported": ["query"],
            "grant_types_supported": ["authorization_code", "refresh_token"],
            "code_challenge_methods_supported": ["S256"],
            "token_endpoint_auth_methods_supported": AUTH_METHODS,
            "authorization_response_iss_parameter_supported": true,
        }),
    )
}

// ── Dynamic client registration (RFC 7591) ───────────────────────────────────

fn invalid_metadata(code: &str, desc: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, code, desc)
}

fn string_list(v: Option<&Value>, default: &[&str]) -> Result<Vec<String>, ()> {
    match v {
        None | Some(Value::Null) => Ok(default.iter().map(|s| s.to_string()).collect()),
        Some(Value::Array(a)) => a.iter().map(|x| x.as_str().map(str::to_string).ok_or(())).collect(),
        Some(_) => Err(()),
    }
}

pub(super) async fn register(State(st): State<Arc<HttpState>>, headers: HeaderMap, body: axum::body::Body) -> Response {
    let c = cfg(&st);
    if !c.allow("register", RATE_REGISTER) {
        return oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable", "Too many registrations");
    }
    let body = match read_body(&headers, body, MAX_FORM_BYTES).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Ok(Value::Object(meta)) = serde_json::from_slice::<Value>(&body) else {
        return invalid_metadata("invalid_client_metadata", "Body must be a JSON object");
    };

    let Ok(redirect_uris) = string_list(meta.get("redirect_uris"), &[]) else {
        return invalid_metadata("invalid_redirect_uri", "redirect_uris must be an array of strings");
    };
    if redirect_uris.is_empty() || redirect_uris.len() > MAX_REDIRECTS {
        return invalid_metadata("invalid_redirect_uri", "Give 1 to 5 redirect_uris");
    }
    if let Some(bad) = redirect_uris.iter().find(|u| !c.redirect_allowlist.contains(u)) {
        tracing::warn!(redirect = %terminal_safe(bad), "refused client registration: redirect not allowlisted");
        return invalid_metadata(
            "invalid_redirect_uri",
            "This redirect URI is not allowed by the gateway owner (see `serve --oauth-redirect`)",
        );
    }
    let name = match meta.get("client_name") {
        None | Some(Value::Null) => "unnamed client".to_string(),
        // No control/bidi/invisible characters: the name is shown on the sign-in page and by
        // `connect`, and must not be able to disguise itself.
        Some(Value::String(s))
            if s.chars().count() <= MAX_NAME_CHARS && !s.chars().any(|c| c.is_control() || is_invisible(c)) =>
        {
            s.clone()
        }
        Some(_) => {
            return invalid_metadata(
                "invalid_client_metadata",
                "client_name must be at most 100 visible characters",
            )
        }
    };
    // Omitted → both (returned as the effective metadata), so a client that leaves it out
    // can still refresh. What is stored is enforced at /token.
    let Ok(grant_types) = string_list(meta.get("grant_types"), &["authorization_code", "refresh_token"]) else {
        return invalid_metadata("invalid_client_metadata", "grant_types must be an array of strings");
    };
    if grant_types.is_empty() || grant_types.iter().any(|g| g != "authorization_code" && g != "refresh_token") {
        return invalid_metadata("invalid_client_metadata", "Only authorization_code and refresh_token are supported");
    }
    let Ok(response_types) = string_list(meta.get("response_types"), &["code"]) else {
        return invalid_metadata("invalid_client_metadata", "response_types must be an array of strings");
    };
    if response_types.iter().any(|r| r != "code") {
        return invalid_metadata("invalid_client_metadata", "Only the code response type is supported");
    }
    // RFC 7591 §2: an omitted method means client_secret_basic.
    let auth_method = match meta.get("token_endpoint_auth_method") {
        None | Some(Value::Null) => "client_secret_basic".to_string(),
        Some(Value::String(m)) if AUTH_METHODS.contains(&m.as_str()) => m.clone(),
        Some(_) => {
            return invalid_metadata(
                "invalid_client_metadata",
                "token_endpoint_auth_method must be none, client_secret_post or client_secret_basic",
            )
        }
    };

    let paths = st.gw.paths.clone();
    blocking(&st, move || {
        let secret = (auth_method != "none").then(random_token);
        let client = Client {
            id: format!("cortex-{}", &random_token()[..22]),
            name,
            redirect_uris,
            auth_method,
            secret_hash: secret.as_deref().map(sha256_hex),
            grant_types: grant_types.clone(),
            created: now(),
        };
        let stored = with_state(&paths, |s| {
            if s.clients.len() >= MAX_CLIENTS {
                // Evict the oldest client nothing refers to; never one mid-authorization.
                let victim = s
                    .clients
                    .iter()
                    .filter(|c| !s.client_referenced(&c.id))
                    .min_by_key(|c| c.created)
                    .map(|c| c.id.clone());
                match victim {
                    Some(id) => s.clients.retain(|c| c.id != id),
                    None => return false,
                }
            }
            s.clients.push(client.clone());
            true
        });
        match stored {
            Ok(true) => {}
            Ok(false) => return oauth_error(StatusCode::SERVICE_UNAVAILABLE, "temporarily_unavailable", "Too many clients"),
            Err(_) => return state_error(),
        }
        let mut body = json!({
            "client_id": client.id,
            "client_id_issued_at": client.created,
            "client_name": client.name,
            "redirect_uris": client.redirect_uris,
            "grant_types": grant_types,
            "response_types": response_types,
            "token_endpoint_auth_method": client.auth_method,
            "scope": SCOPE,
        });
        if let Some(secret) = secret {
            body["client_secret"] = json!(secret);
            body["client_secret_expires_at"] = json!(0);
        }
        json_response(StatusCode::CREATED, body)
    })
    .await
}

// ── Authorization ────────────────────────────────────────────────────────────

fn is_pkce_value(s: &str) -> bool {
    (43..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~'))
}

fn waiting_page(code: &str, client_name: &str, redirect_uri: &str, req: &str) -> Response {
    let host = redirect_uri
        .split("://")
        .nth(1)
        .and_then(|r| r.split(['/', '?']).next())
        .unwrap_or(redirect_uri);
    html_page(
        StatusCode::OK,
        "Approve on your computer",
        &format!(
            "<h1>Approve on your computer</h1>\
             <p>A client calling itself <b>{name}</b> (unverified; sends you back to <code>{host}</code>) \
             wants to read the memories you exported to Muse.</p>\
             <p>On the computer running Cortex, run:</p>\
             <p><code>cortex-mcp-server gateway connect {code}</code></p>\
             <p class=\"code\">{code}</p>\
             <p class=\"muted\">This page continues by itself once you approve. Only approve if you just \
             clicked Connect in Muse yourself. Nothing typed here can grant access.</p>",
            name = html_escape(client_name),
            host = html_escape(host),
            code = html_escape(code),
        ),
        Some((3, &format!("/authorize/wait?req={req}"))),
    )
}

pub(super) async fn authorize(State(st): State<Arc<HttpState>>, RawQuery(q): RawQuery) -> Response {
    let c = cfg(&st);
    if !c.allow("authorize", RATE_AUTHORIZE) {
        return bad_request_page("Too many sign-in attempts. Wait a minute and try again.");
    }
    let Ok(p) = parse_params(q.as_deref().unwrap_or("")) else {
        return bad_request_page("Malformed or repeated parameters.");
    };
    let st2 = st.clone();
    blocking(&st, move || {
        let c = cfg(&st2);
        let paths = &st2.gw.paths;
        // 1. Client and redirect URI first: until both check out, never redirect anywhere.
        let client = match with_state_read(paths, |s| (p.get("client_id").and_then(|id| s.client(id).cloned()), false)) {
            Ok(Some(cl)) => cl,
            Ok(None) => return bad_request_page("Unknown client. Add the connector again in Muse."),
            Err(_) => return bad_request_page("Authorization is unavailable (gateway state error)."),
        };
        let redirect_uri = match p.get("redirect_uri") {
            Some(r) if client.redirect_uris.contains(r) => r.clone(),
            None if client.redirect_uris.len() == 1 => client.redirect_uris[0].clone(),
            _ => return bad_request_page("This redirect URI is not registered for the client."),
        };
        let state = p.get("state").map(String::as_str);
        if state.is_some_and(|s| s.chars().count() > MAX_STATE_CHARS) {
            return bad_request_page("The state parameter is too long.");
        }
        let fail = |e: &str, d: &str| redirect_with(c, &redirect_uri, &[("error", e), ("error_description", d)], state);

        // 2. Everything else is reported to the (validated) client.
        if p.get("response_type").map(String::as_str) != Some("code") {
            return fail("unsupported_response_type", "response_type must be code");
        }
        let challenge = match (p.get("code_challenge"), p.get("code_challenge_method").map(String::as_str)) {
            (Some(ch), Some("S256")) if is_pkce_value(ch) => ch.clone(),
            _ => return fail("invalid_request", "PKCE with code_challenge_method=S256 is required"),
        };
        if p.get("resource").is_some_and(|r| *r != c.resource) {
            return fail("invalid_target", "Unknown resource");
        }
        if paths.is_disabled() {
            return fail("access_denied", "Memory access for Muse is turned off by the user");
        }

        let req = random_token();
        let created = with_state(paths, |s| {
            if s.pending.len() >= MAX_PENDING {
                return None;
            }
            let mut code = display_code();
            while s.pending.iter().any(|x| x.display_code == code) {
                code = display_code();
            }
            s.pending.push(Pending {
                req_hash: sha256_hex(&req),
                display_code: code.clone(),
                client_id: client.id.clone(),
                redirect_uri: redirect_uri.clone(),
                state: state.map(str::to_string),
                challenge: challenge.clone(),
                scope: SCOPE.into(),
                resource: c.resource.clone(),
                created: now(),
                status: Status::Waiting,
            });
            Some(code)
        });
        match created {
            Ok(Some(code)) => waiting_page(&code, &client.name, &redirect_uri, &req),
            Ok(None) => fail("temporarily_unavailable", "Too many sign-ins in progress; try again in a few minutes"),
            Err(_) => fail("server_error", "Gateway state error"),
        }
    })
    .await
}

enum WaitOutcome {
    Unknown,
    Waiting(Pending, String),
    Redirect(Pending, Vec<(&'static str, String)>),
    StateError,
}

pub(super) async fn authorize_wait(State(st): State<Arc<HttpState>>, RawQuery(q): RawQuery) -> Response {
    let Ok(p) = parse_params(q.as_deref().unwrap_or("")) else {
        return bad_request_page("Malformed request.");
    };
    let Some(req) = p.get("req").cloned().filter(|r| is_token_shaped(r)) else {
        return bad_request_page("This sign-in expired or was already used. Start again in Muse.");
    };
    // Busy or rate limited: keep polling (slower) with the same capability; never strand an
    // approval on a page that stopped refreshing.
    let retry_url = format!("/authorize/wait?req={req}");
    let retry = move |status| {
        html_page(
            status,
            "Busy",
            "<h1>Busy</h1><p>Too many requests right now. This page retries by itself.</p>",
            Some((15, &retry_url)),
        )
    };
    if !cfg(&st).allow("wait", RATE_WAIT) {
        return retry(StatusCode::TOO_MANY_REQUESTS);
    }
    let st2 = st.clone();
    blocking_or(&st, move || {
        let c = cfg(&st2);
        let paths = &st2.gw.paths;
        let h = sha256_hex(&req);
        let disabled = paths.is_disabled();
        let t = now();
        // Approval → code is one atomic step: the pending request is consumed and the code's
        // hash persisted before the code leaves this process. A second poll finds nothing.
        // Unknown and still-waiting polls write nothing.
        let outcome = with_state_read(paths, |s| {
            let Some(pos) = s
                .pending
                .iter()
                .position(|x| ct_eq(x.req_hash.as_bytes(), h.as_bytes()) && t - x.created < PENDING_TTL)
            else {
                return (WaitOutcome::Unknown, false);
            };
            let pending = s.pending[pos].clone();
            if disabled || pending.status == Status::Denied {
                s.pending.remove(pos);
                let params =
                    vec![("error", "access_denied".into()), ("error_description", "The user did not approve".into())];
                return (WaitOutcome::Redirect(pending, params), true);
            }
            if pending.status == Status::Waiting {
                let name = s.client(&pending.client_id).map(|cl| cl.name.clone()).unwrap_or_default();
                return (WaitOutcome::Waiting(pending, name), false);
            }
            s.pending.remove(pos);
            let code = random_token();
            s.codes.push(Code {
                hash: sha256_hex(&code),
                client_id: pending.client_id.clone(),
                redirect_uri: pending.redirect_uri.clone(),
                challenge: pending.challenge.clone(),
                scope: pending.scope.clone(),
                resource: pending.resource.clone(),
                expires: t + CODE_TTL,
            });
            (WaitOutcome::Redirect(pending, vec![("code", code)]), true)
        })
        .unwrap_or(WaitOutcome::StateError);
        match outcome {
            WaitOutcome::Unknown => bad_request_page("This sign-in expired or was already used. Start again in Muse."),
            WaitOutcome::StateError => bad_request_page("Authorization is unavailable (gateway state error)."),
            WaitOutcome::Waiting(pending, name) => waiting_page(&pending.display_code, &name, &pending.redirect_uri, &req),
            WaitOutcome::Redirect(pending, params) => {
                let params: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
                redirect_with(c, &pending.redirect_uri, &params, pending.state.as_deref())
            }
        }
    }, || retry(StatusCode::SERVICE_UNAVAILABLE))
    .await
}

// ── Token endpoint ───────────────────────────────────────────────────────────

/// `Authorization: Basic` → (client_id, secret), each form-urlencoded per RFC 6749 §2.3.1.
fn basic_credentials(headers: &HeaderMap) -> Option<(String, String)> {
    let v = headers.get("authorization")?.to_str().ok()?;
    let (scheme, b64) = v.split_once(' ')?;
    if !scheme.eq_ignore_ascii_case("basic") {
        return None;
    }
    let raw = String::from_utf8(STANDARD.decode(b64.trim()).ok()?).ok()?;
    let (id, secret) = raw.split_once(':')?;
    let dec = |s: &str| -> Option<String> {
        let pairs: Vec<(String, String)> = serde_urlencoded::from_str(&format!("v={s}")).ok()?;
        pairs.into_iter().next().map(|(_, v)| v)
    };
    Some((dec(id)?, dec(secret)?))
}

fn invalid_client() -> Response {
    let mut r = oauth_error(StatusCode::UNAUTHORIZED, "invalid_client", "Client authentication failed");
    r.headers_mut().insert("www-authenticate", "Basic realm=\"cortex\"".parse().expect("static header"));
    r
}

fn invalid_grant(desc: &str) -> Response {
    oauth_error(StatusCode::BAD_REQUEST, "invalid_grant", desc)
}

/// Mint a fresh token pair onto `g` and build the response.
fn issue(g: &mut Grant) -> Response {
    let access = random_token();
    let refresh = random_token();
    let t = now();
    g.access_hash = sha256_hex(&access);
    g.access_exp = t + ACCESS_TTL;
    g.refresh_hash = sha256_hex(&refresh);
    g.refresh_exp = (t + REFRESH_TTL).min(g.created + GRANT_MAX_AGE);
    g.last_used = t;
    json_response(
        StatusCode::OK,
        json!({
            "access_token": access,
            "token_type": "Bearer",
            "expires_in": ACCESS_TTL,
            "refresh_token": refresh,
            "scope": g.scope,
        }),
    )
}

pub(super) async fn token(State(st): State<Arc<HttpState>>, headers: HeaderMap, body: axum::body::Body) -> Response {
    let body = match read_body(&headers, body, MAX_FORM_BYTES).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let Ok(p) = std::str::from_utf8(&body).map_err(|_| ()).and_then(parse_params) else {
        return oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "Malformed or repeated parameters");
    };
    let basic = basic_credentials(&headers);
    let client_id = match (&basic, p.get("client_id")) {
        (Some((id, _)), Some(form_id)) if id != form_id => return invalid_client(),
        (Some((id, _)), _) => id.clone(),
        (None, Some(id)) => id.clone(),
        (None, None) => return invalid_client(),
    };
    // Client ids are ours (`cortex-` + 22 base64url chars): anything else never reaches disk.
    let id_shaped = client_id
        .strip_prefix("cortex-")
        .is_some_and(|r| r.len() == 22 && r.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    if !id_shaped {
        return invalid_client();
    }
    let st2 = st.clone();
    blocking(&st, move || {
        let c = cfg(&st2);
        let paths = &st2.gw.paths;
        let disabled = paths.is_disabled();
        let resource_ok = p.get("resource").is_none_or(|r| *r == c.resource);
        let t = now();
        // Failures that change nothing are not persisted (no fsync for junk requests).
        let out = with_state_read(paths, |s| {
            // Client authentication, by the registered method only (no downgrade).
            let Some(client) = s.client(&client_id).cloned() else {
                return (invalid_client(), false);
            };
            // Per client: a stranger can't spend Muse's budget without knowing its client_id.
            if !c.allow(&format!("token:{}", client.id), RATE_TOKEN_PER_CLIENT) {
                return (
                    oauth_error(StatusCode::TOO_MANY_REQUESTS, "temporarily_unavailable", "Too many token requests"),
                    false,
                );
            }
            let presented = match client.auth_method.as_str() {
                "none" => None,
                "client_secret_basic" => match &basic {
                    Some((_, secret)) => Some(secret.clone()),
                    None => return (invalid_client(), false),
                },
                _ => match (&basic, p.get("client_secret")) {
                    (None, Some(secret)) => Some(secret.clone()),
                    _ => return (invalid_client(), false),
                },
            };
            match (&presented, &client.secret_hash) {
                (Some(secret), Some(expected)) if ct_eq(sha256_hex(secret).as_bytes(), expected.as_bytes()) => {}
                (None, None) => {}
                _ => return (invalid_client(), false),
            }
            if disabled {
                // Transient on purpose: the grant is suspended, not revoked, so the client
                // must not discard its refresh token.
                return (
                    oauth_error(
                        StatusCode::SERVICE_UNAVAILABLE,
                        "temporarily_unavailable",
                        "Memory access for Muse is turned off by the user",
                    ),
                    false,
                );
            }
            if !resource_ok {
                return (oauth_error(StatusCode::BAD_REQUEST, "invalid_target", "Unknown resource"), false);
            }
            let grant_type = p.get("grant_type").map(String::as_str).unwrap_or("");
            if !client.grant_types.iter().any(|g| g == grant_type) {
                let r = if grant_type == "authorization_code" || grant_type == "refresh_token" {
                    oauth_error(StatusCode::BAD_REQUEST, "unauthorized_client", "Grant type not registered for this client")
                } else {
                    oauth_error(StatusCode::BAD_REQUEST, "unsupported_grant_type", "Use authorization_code or refresh_token")
                };
                return (r, false);
            }
            match grant_type {
                "authorization_code" => {
                    let (Some(code), Some(verifier)) = (p.get("code"), p.get("code_verifier")) else {
                        let r = oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "code and code_verifier are required");
                        return (r, false);
                    };
                    let h = sha256_hex(code);
                    let Some(pos) = s.codes.iter().position(|x| ct_eq(x.hash.as_bytes(), h.as_bytes())) else {
                        // A code that was already exchanged: whoever redeemed it first may not
                        // be the client. Revoke what it produced.
                        if let Some(r) = s.redeemed.iter().find(|r| ct_eq(r.hash.as_bytes(), h.as_bytes())) {
                            let gid = r.grant_id.clone();
                            s.grants.retain(|g| g.id != gid);
                            tracing::warn!(grant = %gid, "authorization code replayed: grant revoked");
                            return (invalid_grant("Invalid or expired code"), true);
                        }
                        return (invalid_grant("Invalid or expired code"), false);
                    };
                    // Single use: consumed by any attempt, successful or not.
                    let entry = s.codes.remove(pos);
                    let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
                    let redirect_ok = p.get("redirect_uri").map_or(
                        // Optional only if it could not have differed (one registered URI).
                        client.redirect_uris.len() == 1,
                        |r| *r == entry.redirect_uri,
                    );
                    if entry.client_id != client.id
                        || entry.expires <= t
                        || !redirect_ok
                        || entry.resource != c.resource
                        || !is_pkce_value(verifier)
                        || !ct_eq(challenge.as_bytes(), entry.challenge.as_bytes())
                    {
                        return (invalid_grant("Invalid code, redirect_uri or code_verifier"), true);
                    }
                    let mut g = Grant {
                        id: format!("g-{}", &random_token()[..16]),
                        client_id: client.id.clone(),
                        scope: entry.scope.clone(),
                        resource: entry.resource.clone(),
                        created: t,
                        last_used: t,
                        access_hash: String::new(),
                        access_exp: 0,
                        refresh_hash: String::new(),
                        refresh_exp: 0,
                        used_refresh: Vec::new(),
                    };
                    let resp = issue(&mut g);
                    s.redeemed.push(Redeemed { hash: entry.hash, grant_id: g.id.clone(), expires: entry.expires });
                    s.grants.push(g);
                    (resp, true)
                }
                _ => {
                    let Some(rt) = p.get("refresh_token") else {
                        return (oauth_error(StatusCode::BAD_REQUEST, "invalid_request", "refresh_token is required"), false);
                    };
                    let h = sha256_hex(rt);
                    // Same audience too: after a `--public-url` change the old grant can't
                    // mint usable tokens, so the client must sign in again.
                    let live = |g: &Grant| g.refresh_exp > t && t - g.created < GRANT_MAX_AGE && g.resource == c.resource;
                    if let Some(g) = s.grants.iter_mut().find(|g| ct_eq(g.refresh_hash.as_bytes(), h.as_bytes())) {
                        if g.client_id != client.id || !live(g) {
                            return (invalid_grant("Invalid refresh token"), false);
                        }
                        let old = std::mem::take(&mut g.refresh_hash);
                        g.used_refresh.push(old);
                        if g.used_refresh.len() > MAX_USED_REFRESH {
                            g.used_refresh.remove(0);
                        }
                        return (issue(g), true);
                    }
                    // A rotated-away token came back: someone holds a copy. Revoke the grant.
                    if let Some(pos) =
                        s.grants.iter().position(|g| g.used_refresh.iter().any(|u| ct_eq(u.as_bytes(), h.as_bytes())))
                    {
                        let g = s.grants.remove(pos);
                        tracing::warn!(grant = %g.id, "refresh token reuse: grant revoked");
                        return (invalid_grant("Invalid refresh token"), true);
                    }
                    (invalid_grant("Invalid refresh token"), false)
                }
            }
        });
        out.unwrap_or_else(|_| state_error())
    })
    .await
}

// ── CLI ──────────────────────────────────────────────────────────────────────

fn ts(t: i64) -> String {
    chrono::DateTime::from_timestamp(t, 0).map(|d| d.to_rfc3339()).unwrap_or_default()
}

/// `gateway connect <CODE>`: show the request, ask, then record the answer. The lock is
/// not held while waiting for the user (the server keeps serving).
pub(super) fn connect(paths: &Paths, code: &str, remember_hint: bool) -> Result<bool, String> {
    if paths.is_disabled() {
        return Err("Muse access is OFF (`gateway on` first)".into());
    }
    let code = normalize_code(code);
    let (pending, client) = with_state(paths, |s| {
        let p = s.pending.iter().find(|p| p.display_code == code && p.status == Status::Waiting).cloned()?;
        let cl = s.client(&p.client_id).cloned()?;
        Some((p, cl))
    })?
    .ok_or("no sign-in is waiting for that code (expired or already answered)")?;

    let age = now() - pending.created;
    println!("A client wants to connect to your Cortex gateway:");
    println!("  client name : {}   (UNVERIFIED — any client can call itself anything)", terminal_safe(&client.name));
    println!("  sends you to: {}", terminal_safe(&pending.redirect_uri));
    println!("  resource    : {}", terminal_safe(&pending.resource));
    println!("  access      : read the memories you exported with `gateway allow`{}",
        if remember_hint { " (and add to your review inbox, if remember is on)" } else { "" });
    println!("  started     : {age}s ago");
    println!();
    println!("Only approve if YOU just clicked Connect in Muse and this code is on YOUR screen.");
    print!("Approve? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut answer = String::new();
    let _ = std::io::stdin().read_line(&mut answer);
    let approved = matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes");
    if approved && paths.is_disabled() {
        return Err("Muse access was turned OFF; not approved".into());
    }
    let recorded = with_state(paths, |s| {
        match s.pending.iter_mut().find(|p| p.req_hash == pending.req_hash && p.status == Status::Waiting) {
            Some(p) => {
                p.status = if approved { Status::Approved } else { Status::Denied };
                true
            }
            None => false,
        }
    })?;
    if !recorded {
        return Err("that sign-in expired while waiting".into());
    }
    Ok(approved)
}

pub(super) fn list_grants(paths: &Paths) -> Result<Vec<String>, String> {
    with_state(paths, |s| {
        s.grants
            .iter()
            .map(|g| {
                let name = s.client(&g.client_id).map(|c| c.name.as_str()).unwrap_or("?");
                format!("{}\t{}\tcreated {}\tlast used {}", g.id, terminal_safe(name), ts(g.created), ts(g.last_used))
            })
            .collect()
    })
}

pub(super) fn disconnect(paths: &Paths, id: Option<&str>) -> Result<usize, String> {
    with_state(paths, |s| {
        let before = s.grants.len();
        match id {
            Some(id) => s.grants.retain(|g| g.id != id),
            None => {
                s.grants.clear();
                // "Sign everyone out" must not leave an approved-but-unclaimed sign-in or an
                // unredeemed code that becomes a new grant a moment later.
                s.pending.retain(|p| p.status == Status::Waiting);
                s.codes.clear();
            }
        }
        before - s.grants.len()
    })
}

/// Kill switch: drop every sign-in in flight. Grants stay (suspended while off).
pub(super) fn cancel_in_flight(paths: &Paths) -> Result<(), String> {
    if !paths.oauth.exists() {
        return Ok(());
    }
    with_state(paths, |s| {
        s.pending.clear();
        s.codes.clear();
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_url_validation() {
        assert_eq!(validate_public_url("https://Me.ts.net/").unwrap(), "https://me.ts.net");
        assert_eq!(validate_public_url("https://h:8443").unwrap(), "https://h:8443");
        for bad in ["http://h", "https://", "https://h/mcp", "https://h?x", "https://h#f", "https://u@h"] {
            assert!(validate_public_url(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn redirect_validation() {
        assert!(validate_redirect("https://x.example/cb").is_ok());
        assert!(validate_redirect("http://127.0.0.1:9/cb").is_ok());
        assert!(validate_redirect("http://localhost/cb").is_ok());
        assert!(validate_redirect("http://evil.example/cb").is_err());
        assert!(validate_redirect("http://127.0.0.1.evil.example/cb").is_err());
        assert!(validate_redirect("https://x.example/cb#f").is_err());
        // userinfo would send the browser elsewhere
        assert!(validate_redirect("http://localhost:@evil.example/cb").is_err());
        assert!(validate_redirect("http://127.0.0.1:x@evil.example/").is_err());
        assert!(validate_redirect("https://agent.meta.ai@evil.example/cb").is_err());
        assert!(validate_redirect("http://localhost:8080/cb").is_ok());
    }

    #[test]
    fn display_codes_are_unambiguous_and_normalize() {
        for _ in 0..200 {
            let c = display_code();
            assert_eq!(c.len(), 9);
            assert!(!c.contains(['0', 'O', '1', 'I']));
            assert_eq!(normalize_code(&c.to_lowercase().replace('-', " ")), c);
        }
    }

    #[test]
    fn token_shape() {
        assert!(is_token_shaped(&random_token()));
        assert!(!is_token_shaped("short"));
        assert!(!is_token_shaped(&"a".repeat(42)));
        assert!(!is_token_shaped(&format!("{}+", "a".repeat(42))));
    }

    #[test]
    fn html_is_escaped() {
        assert_eq!(html_escape("<script>\"'&"), "&lt;script&gt;&quot;&#39;&amp;");
    }

    #[test]
    fn pkce_value_bounds() {
        assert!(is_pkce_value(&"a".repeat(43)));
        assert!(!is_pkce_value(&"a".repeat(42)));
        assert!(!is_pkce_value(&"a".repeat(129)));
        assert!(!is_pkce_value(&format!("{}+", "a".repeat(43))));
    }

    #[test]
    fn basic_credentials_are_form_decoded() {
        let mut h = HeaderMap::new();
        let v = format!("Basic {}", STANDARD.encode("my%20id:s%3Acret"));
        h.insert("authorization", v.parse().unwrap());
        assert_eq!(basic_credentials(&h), Some(("my id".into(), "s:cret".into())));
    }

    #[test]
    fn repeated_params_are_rejected() {
        assert!(parse_params("a=1&b=2").is_ok());
        assert!(parse_params("a=1&a=2").is_err());
    }

    #[test]
    fn sweep_drops_expired() {
        let mut s = OAuthState::default();
        let t = now();
        s.pending.push(Pending {
            req_hash: "h".into(),
            display_code: "AAAA-AAAA".into(),
            client_id: "c".into(),
            redirect_uri: MUSE_CALLBACK.into(),
            state: None,
            challenge: "x".into(),
            scope: SCOPE.into(),
            resource: "https://h/mcp".into(),
            created: t - PENDING_TTL,
            status: Status::Waiting,
        });
        s.codes.push(Code {
            hash: "h".into(),
            client_id: "c".into(),
            redirect_uri: "r".into(),
            challenge: "x".into(),
            scope: SCOPE.into(),
            resource: "r".into(),
            expires: t,
        });
        s.redeemed.push(Redeemed { hash: "h".into(), grant_id: "g".into(), expires: t });
        s.sweep(t);
        assert!(s.pending.is_empty() && s.codes.is_empty() && s.redeemed.is_empty());
    }

    #[test]
    fn rate_limit_window() {
        let c = OAuthConfig::new("https://h", &[]).unwrap();
        assert!((0..3).all(|_| c.allow("x", 3)));
        assert!(!c.allow("x", 3));
        assert!(c.allow("y", 3));
    }
}
