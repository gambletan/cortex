//! Cortex Cloud for Muse: an always-on home for the memories a user chose to share with
//! Muse, so Muse works from a phone with the user's computer off. Design:
//! `docs/design/muse-cloud.md`.
//!
//! ```text
//!                      ┌──────────────────── cortex-cloud ────────────────────┐
//! Muse ── /t/<rid>/… ─▶│ tenant router (cache) ─▶ gateway (unchanged, per dir) │
//!   /.well-known/…/t/<rid>…  (RFC 8414/9728 inserted forms → same tenant)       │
//! device ── /api/… ───▶│ Ed25519-signed: register · export · enroll · inbox ·  │
//!                      │ status · delete                                        │
//!                      │ <data>/tenants/<rid>/export.db  SQLCipher, key = HMAC(master, rid)
//!                      └────────────────────────────────────────────────────────┘
//! ```
//!
//! The server never holds anything but the shared slice. Each tenant is a directory; a
//! request resolves exactly one directory from its URL and never touches another.

// Handlers return early with a ready `Response` as the error; boxing it buys nothing here.
#![allow(clippy::result_large_err)]

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::body::Bytes;
use axum::extract::{ConnectInfo, DefaultBodyLimit, Path as UrlPath, Request, State};
use axum::http::{HeaderMap, Method, StatusCode, Uri};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use clap::Parser;
use hmac::{Hmac, Mac};
use serde::Deserialize;
use serde_json::{json, Value};
use sha2::Sha256;
use tower::ServiceExt as _;

use cortex_core::Cortex;
use cortex_mcp_server::gateway::{self, cloud, oauth, ExportItem, Paths, TenantConfig};

const ENROLL_SECS: i64 = 30 * 60;
const TENANT_CACHE: usize = 256;
const MAX_TENANTS: usize = 100_000;
const REGISTER_PER_IP_PER_HOUR: u32 = 5;
const MAX_API_BODY: usize = 16 * 1024 * 1024;
const IDLE_DAYS: u64 = 90;
const MAX_CONCURRENT: usize = 256;

#[derive(Parser)]
#[command(name = "cortex-cloud", about = "Cortex Cloud for Muse (hosts only shared memories)")]
struct Args {
    /// Public https URL of this service (what Muse and devices reach), e.g. https://muse.example.com
    #[arg(long, env = "CORTEX_CLOUD_BASE_URL")]
    base_url: String,
    /// Listen address (put a TLS proxy such as Caddy in front)
    #[arg(long, default_value = "127.0.0.1:8787")]
    listen: SocketAddr,
    /// Data directory (one subdirectory per tenant)
    #[arg(long, default_value = "/var/lib/cortex-cloud")]
    data_dir: PathBuf,
    /// 32-byte master key file (hex); created with 0600 if missing. Keep it out of backups.
    #[arg(long, default_value = "/etc/cortex-cloud/master.key")]
    master_key: PathBuf,
}

struct App {
    base_url: String,
    tenants_dir: PathBuf,
    master: [u8; 32],
    #[cfg(feature = "embeddings")]
    embedder: Option<cortex_core::embedder::Embedder>,
    cache: Mutex<Cache>,
    /// Serializes nonce-file read-modify-writes (one process owns the data dir).
    nonce_lock: Mutex<()>,
    register_rate: Mutex<HashMap<IpAddr, (Instant, u32)>>,
}

#[derive(Default)]
struct Cache {
    routers: HashMap<String, (Router, Arc<Cortex>, Paths)>,
    order: VecDeque<String>,
}

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn derive(master: &[u8; 32], label: &str, rid: &str) -> [u8; 32] {
    let mut mac = <Hmac<Sha256> as Mac>::new_from_slice(master).expect("any key length");
    mac.update(label.as_bytes());
    mac.update(b"\0");
    mac.update(rid.as_bytes());
    mac.finalize().into_bytes().into()
}

fn private_write(path: &Path, data: &[u8]) -> std::io::Result<()> {
    use std::io::Write as _;
    let tmp = path.with_extension(format!("tmp{}", rand::random::<u64>()));
    {
        let mut o = std::fs::OpenOptions::new();
        o.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
        }
        let mut f = o.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp, path)
}

fn load_master(path: &Path) -> [u8; 32] {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            let s = s.trim();
            let bytes: Vec<u8> = (0..s.len())
                .step_by(2)
                .filter_map(|i| s.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
                .collect();
            bytes.try_into().unwrap_or_else(|_| die("master key must be 64 hex characters"))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let key: [u8; 32] = rand::random();
            if let Some(dir) = path.parent() {
                std::fs::create_dir_all(dir).unwrap_or_else(|e| die(&e.to_string()));
            }
            private_write(path, hex(&key).as_bytes()).unwrap_or_else(|e| die(&e.to_string()));
            eprintln!("created master key at {}", path.display());
            key
        }
        Err(e) => die(&format!("cannot read master key: {e}")),
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn api_error(status: StatusCode, msg: &str) -> Response {
    (status, [("cache-control", "no-store")], Json(json!({ "error": msg }))).into_response()
}

impl App {
    fn tenant_dir(&self, rid: &str) -> Option<PathBuf> {
        oauth::is_tenant_id(rid).then(|| self.tenants_dir.join(rid))
    }

    fn mcp_url(&self, rid: &str) -> String {
        format!("{}/t/{rid}/mcp", self.base_url)
    }

    /// The tenant's gateway, opening (and caching) it on first use. `None` if no such tenant.
    fn tenant(&self, rid: &str) -> Result<Option<(Router, Arc<Cortex>, Paths)>, String> {
        if let Some(t) = self.cache.lock().unwrap_or_else(|p| p.into_inner()).routers.get(rid) {
            return Ok(Some(t.clone()));
        }
        let Some(dir) = self.tenant_dir(rid) else { return Ok(None) };
        if !dir.join("device.pub").is_file() {
            return Ok(None);
        }
        let db = dir.join("export.db");
        let cortex = Cortex::open_encrypted(&db.to_string_lossy(), &hex(&derive(&self.master, "db", rid)))
            .map_err(|e| format!("tenant storage: {e}"))?;
        #[cfg(feature = "embeddings")]
        let cortex = match &self.embedder {
            Some(e) => cortex.with_embedder(e.clone()),
            None => cortex,
        };
        let cortex = Arc::new(cortex);
        let paths = Paths::for_dir(&dir, Some(derive(&self.master, "inbox", rid)));
        let cfg = oauth::OAuthConfig::for_tenant(&self.base_url, rid)?;
        let router = gateway::tenant_router(cortex.clone(), paths.clone(), cfg, &TenantConfig::default());
        let entry = (router, cortex, paths);
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        if cache.routers.len() >= TENANT_CACHE {
            if let Some(old) = cache.order.pop_front() {
                cache.routers.remove(&old);
            }
        }
        cache.order.push_back(rid.to_string());
        cache.routers.insert(rid.to_string(), entry.clone());
        Ok(Some(entry))
    }

    fn evict(&self, rid: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.routers.remove(rid);
        cache.order.retain(|r| r != rid);
    }

    /// Reject a nonce seen within the skew window (persisted, so replay protection survives
    /// a restart).
    fn fresh_nonce(&self, dir: &Path, nonce: &str) -> Result<bool, String> {
        let _g = self.nonce_lock.lock().unwrap_or_else(|p| p.into_inner());
        let path = dir.join("nonces.json");
        let mut seen: HashMap<String, i64> = match std::fs::read_to_string(&path) {
            Ok(s) => serde_json::from_str(&s).map_err(|_| "nonce store corrupt".to_string())?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => HashMap::new(),
            Err(e) => return Err(e.to_string()),
        };
        let t = now();
        seen.retain(|_, ts| t - *ts <= 2 * cloud::MAX_SKEW_SECS);
        if seen.contains_key(nonce) {
            return Ok(false);
        }
        seen.insert(nonce.to_string(), t);
        private_write(&path, serde_json::to_string(&seen).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())?;
        Ok(true)
    }
}

fn header_fn(headers: &HeaderMap) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn target(uri: &Uri) -> String {
    uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| uri.path().to_string())
}

/// Authenticate a device call for tenant `rid`: signature by the tenant's registered key,
/// fresh timestamp, unseen nonce. Returns the tenant dir.
fn authenticate(app: &App, rid: &str, method: &Method, uri: &Uri, headers: &HeaderMap, body: &[u8]) -> Result<PathBuf, Response> {
    let not_found = || api_error(StatusCode::NOT_FOUND, "no such tenant");
    let dir = app.tenant_dir(rid).ok_or_else(not_found)?;
    let key = std::fs::read_to_string(dir.join("device.pub")).map_err(|_| not_found())?;
    let (_, nonce) = cloud::verify(method.as_str(), &target(uri), body, header_fn(headers), Some(key.trim()), now())
        .map_err(|e| api_error(StatusCode::UNAUTHORIZED, &format!("signature refused: {e:?}")))?;
    match app.fresh_nonce(&dir, &nonce) {
        Ok(true) => {}
        Ok(false) => return Err(api_error(StatusCode::UNAUTHORIZED, "signature refused: replay")),
        Err(e) => return Err(api_error(StatusCode::INTERNAL_SERVER_ERROR, &e)),
    }
    let _ = std::fs::write(dir.join("last_seen"), now().to_string());
    Ok(dir)
}

fn client_ip(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    // Trust X-Forwarded-For only from the local TLS proxy; take the entry it appended.
    if peer.ip().is_loopback() {
        if let Some(ip) = headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse().ok())
        {
            return ip;
        }
    }
    peer.ip()
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, Response> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|_| api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal error"))
}

// ── Device API ───────────────────────────────────────────────────────────────

async fn register(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let ip = client_ip(peer, &headers);
    {
        let mut rate = app.register_rate.lock().unwrap_or_else(|p| p.into_inner());
        let now_i = Instant::now();
        rate.retain(|_, (start, _)| now_i.duration_since(*start).as_secs() < 3600);
        let slot = rate.entry(ip).or_insert((now_i, 0));
        slot.1 += 1;
        if slot.1 > REGISTER_PER_IP_PER_HOUR {
            return api_error(StatusCode::TOO_MANY_REQUESTS, "too many registrations");
        }
    }
    // Proof of possession: the request is signed by the key being registered.
    let key = match cloud::verify(method.as_str(), &target(&uri), &body, header_fn(&headers), None, now()) {
        Ok((k, _)) => k,
        Err(e) => return api_error(StatusCode::UNAUTHORIZED, &format!("signature refused: {e:?}")),
    };
    let declared = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("public_key").and_then(Value::as_str).map(str::to_string));
    if declared.as_deref() != Some(key.as_str()) {
        return api_error(StatusCode::BAD_REQUEST, "public_key must be the signing key");
    }
    let app2 = app.clone();
    let out = blocking(move || -> Result<String, Response> {
        let count = std::fs::read_dir(&app2.tenants_dir).map(|d| d.count()).unwrap_or(0);
        if count >= MAX_TENANTS {
            return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "service is full"));
        }
        let rid = oauth::new_tenant_id();
        let dir = app2.tenants_dir.join(&rid);
        std::fs::create_dir(&dir).map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let _ = std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700));
        }
        private_write(&dir.join("device.pub"), key.as_bytes())
            .map_err(|e| api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()))?;
        let _ = std::fs::write(dir.join("last_seen"), now().to_string());
        Ok(rid)
    })
    .await;
    match out {
        Ok(Ok(rid)) => (
            StatusCode::CREATED,
            Json(json!({ "rid": rid, "mcp_url": app.mcp_url(&rid) })),
        )
            .into_response(),
        Ok(Err(r)) | Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ExportBody {
    items: Vec<ExportBodyItem>,
}

#[derive(Deserialize)]
struct ExportBodyItem {
    text: String,
    #[serde(default)]
    embedding: Option<Vec<f32>>,
}

#[derive(Deserialize)]
struct AckBody {
    ids: Vec<String>,
}

/// One handler for every signed tenant operation: authenticate, then dispatch.
async fn tenant_api(
    State(app): State<Arc<App>>,
    UrlPath(params): UrlPath<Vec<(String, String)>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let rid = params.iter().find(|(k, _)| k == "rid").map(|(_, v)| v.clone()).unwrap_or_default();
    let op = params.iter().find(|(k, _)| k == "op").map(|(_, v)| v.clone()).unwrap_or_default();
    let op = match (method.as_str(), uri.path().ends_with("/inbox/ack"), op.as_str()) {
        ("POST", true, _) => "ack".to_string(),
        (_, _, op) => op.to_string(),
    };
    let app2 = app.clone();
    let out = blocking(move || -> Response {
        let dir = match authenticate(&app2, &rid, &method, &uri, &headers, &body) {
            Ok(d) => d,
            Err(r) => return r,
        };
        if method == Method::DELETE && op.is_empty() {
            app2.evict(&rid);
            return match std::fs::remove_dir_all(&dir) {
                Ok(()) => api_error(StatusCode::OK, "deleted").into_response(),
                Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e.to_string()),
            };
        }
        let (_, cortex, paths) = match app2.tenant(&rid) {
            Ok(Some(t)) => t,
            Ok(None) => return api_error(StatusCode::NOT_FOUND, "no such tenant"),
            Err(e) => return api_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
        };
        match (method.as_str(), op.as_str()) {
            ("PUT", "export") => {
                let Ok(b) = serde_json::from_slice::<ExportBody>(&body) else {
                    return api_error(StatusCode::BAD_REQUEST, "expected {items:[{text, embedding?}]}");
                };
                let items: Vec<ExportItem> =
                    b.items.into_iter().map(|i| ExportItem { text: i.text, embedding: i.embedding }).collect();
                match gateway::replace_export(&cortex, &items) {
                    Ok(n) => Json(json!({ "count": n })).into_response(),
                    Err(e) => api_error(StatusCode::BAD_REQUEST, &e),
                }
            }
            ("POST", "enroll") => match oauth::open_enrollment(&paths, ENROLL_SECS) {
                Ok(()) => Json(json!({ "mcp_url": app2.mcp_url(&rid), "expires_in": ENROLL_SECS })).into_response(),
                Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            },
            ("GET", "inbox") => match gateway::inbox_items(&paths) {
                Ok(items) => {
                    let items: Vec<Value> =
                        items.into_iter().map(|i| json!({ "id": i.id, "ts": i.ts, "text": i.text })).collect();
                    ([("cache-control", "no-store")], Json(json!({ "items": items }))).into_response()
                }
                Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
            },
            ("POST", "ack") => {
                let Ok(b) = serde_json::from_slice::<AckBody>(&body) else {
                    return api_error(StatusCode::BAD_REQUEST, "expected {ids:[…]}");
                };
                match gateway::inbox_ack(&paths, &b.ids) {
                    Ok(n) => Json(json!({ "removed": n })).into_response(),
                    Err(e) => api_error(StatusCode::INTERNAL_SERVER_ERROR, &e),
                }
            }
            ("GET", "status") => {
                let (connected, last_used) = oauth::grant_summary(&paths).unwrap_or((0, None));
                Json(json!({
                    "shared": gateway::export_count(&cortex),
                    "connected": connected,
                    "last_used": last_used,
                }))
                .into_response()
            }
            _ => api_error(StatusCode::NOT_FOUND, "unknown operation"),
        }
    })
    .await;
    out.unwrap_or_else(|r| r)
}

// ── Muse-facing: route to the tenant's gateway ───────────────────────────────

/// `/t/<rid>/<rest>` → (`rid`, `/<rest>`); the RFC 8414 / 9728 "inserted" well-known
/// forms map to the same tenant paths.
fn split_tenant(path: &str) -> Option<(&str, String)> {
    const AS: &str = "/.well-known/oauth-authorization-server/t/";
    const PRM: &str = "/.well-known/oauth-protected-resource/t/";
    if let Some(rest) = path.strip_prefix(AS) {
        return (oauth::is_tenant_id(rest)).then(|| (rest, "/.well-known/oauth-authorization-server".to_string()));
    }
    if let Some(rest) = path.strip_prefix(PRM) {
        let (rid, tail) = rest.split_once('/')?;
        return (oauth::is_tenant_id(rid) && tail == "mcp")
            .then(|| (rid, "/.well-known/oauth-protected-resource/mcp".to_string()));
    }
    let rest = path.strip_prefix("/t/")?;
    let (rid, tail) = rest.split_once('/').unwrap_or((rest, ""));
    oauth::is_tenant_id(rid).then(|| (rid, format!("/{tail}")))
}

async fn route_tenant(State(app): State<Arc<App>>, req: Request) -> Response {
    let Some((rid, inner_path)) = split_tenant(req.uri().path()).map(|(r, p)| (r.to_string(), p)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let app2 = app.clone();
    let rid2 = rid.clone();
    let router = match blocking(move || app2.tenant(&rid2)).await {
        Ok(Ok(Some((router, _, _)))) => router,
        Ok(Ok(None)) => return StatusCode::NOT_FOUND.into_response(),
        Ok(Err(e)) => {
            tracing::error!(error = %e, "tenant open failed");
            return StatusCode::SERVICE_UNAVAILABLE.into_response();
        }
        Err(r) => return r,
    };
    let (mut parts, body) = req.into_parts();
    let pq = match parts.uri.query() {
        Some(q) => format!("{inner_path}?{q}"),
        None => inner_path,
    };
    let Ok(uri) = pq.parse::<Uri>() else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    parts.uri = uri;
    match router.oneshot(Request::from_parts(parts, body)).await {
        Ok(r) => r,
        Err(never) => match never {},
    }
}

fn app_router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/api/tenants", post(register))
        .route("/api/tenants/{rid}", delete(tenant_api))
        .route("/api/tenants/{rid}/{op}", get(tenant_api).put(tenant_api).post(tenant_api))
        .route("/api/tenants/{rid}/inbox/ack", post(tenant_api))
        .layer(DefaultBodyLimit::max(MAX_API_BODY));
    Router::new()
        .merge(api)
        .route("/healthz", get(|| async { "ok" }))
        .fallback(route_tenant)
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(MAX_CONCURRENT))
        .with_state(app)
}

/// Delete tenants whose device hasn't called in `IDLE_DAYS`.
fn sweep_idle(tenants_dir: &Path, app: &App) {
    let Ok(entries) = std::fs::read_dir(tenants_dir) else { return };
    let cutoff = now() - (IDLE_DAYS * 86_400) as i64;
    for e in entries.flatten() {
        let rid = e.file_name().to_string_lossy().to_string();
        let last = std::fs::read_to_string(e.path().join("last_seen"))
            .ok()
            .and_then(|s| s.trim().parse::<i64>().ok())
            .unwrap_or(0);
        if oauth::is_tenant_id(&rid) && last < cutoff {
            app.evict(&rid);
            let _ = std::fs::remove_dir_all(e.path());
            tracing::info!(tenant = %rid, "deleted idle tenant");
        }
    }
}

fn build_app(args: &Args) -> Arc<App> {
    let base_url = oauth::OAuthConfig::new(&args.base_url, &[])
        .map(|c| c.issuer)
        .unwrap_or_else(|e| die(&e.replace("--public-url", "--base-url")));
    let tenants_dir = args.data_dir.join("tenants");
    std::fs::create_dir_all(&tenants_dir).unwrap_or_else(|e| die(&e.to_string()));
    Arc::new(App {
        base_url,
        tenants_dir,
        master: load_master(&args.master_key),
        #[cfg(feature = "embeddings")]
        embedder: if std::env::var("CORTEX_NO_EMBEDDINGS").is_ok_and(|v| !v.is_empty()) {
            None
        } else {
            cortex_core::embedder::Embedder::new().ok()
        },
        cache: Mutex::new(Cache::default()),
        nonce_lock: Mutex::new(()),
        register_rate: Mutex::new(HashMap::new()),
    })
}

fn main() {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env()).init();
    let args = Args::parse();
    let app = build_app(&args);
    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| die(&e.to_string()));
    rt.block_on(async move {
        let sweeper = app.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_secs(86_400));
            loop {
                tick.tick().await;
                let a = sweeper.clone();
                let dir = a.tenants_dir.clone();
                let _ = tokio::task::spawn_blocking(move || sweep_idle(&dir, &a)).await;
            }
        });
        let listener = tokio::net::TcpListener::bind(args.listen).await.unwrap_or_else(|e| die(&e.to_string()));
        eprintln!("cortex-cloud listening on http://{} for {}", args.listen, app.base_url);
        axum::serve(listener, app_router(app).into_make_service_with_connect_info::<SocketAddr>())
            .await
            .unwrap_or_else(|e| die(&e.to_string()));
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_paths_split() {
        let rid = "AbCdEfGhIjKlMnOpQrStUv";
        assert_eq!(split_tenant(&format!("/t/{rid}/mcp")), Some((rid, "/mcp".into())));
        assert_eq!(split_tenant(&format!("/t/{rid}")), Some((rid, "/".into())));
        assert_eq!(
            split_tenant(&format!("/.well-known/oauth-authorization-server/t/{rid}")),
            Some((rid, "/.well-known/oauth-authorization-server".into()))
        );
        assert_eq!(
            split_tenant(&format!("/.well-known/oauth-protected-resource/t/{rid}/mcp")),
            Some((rid, "/.well-known/oauth-protected-resource/mcp".into()))
        );
        assert_eq!(split_tenant("/t/../etc/mcp"), None);
        assert_eq!(split_tenant("/t/short/mcp"), None);
        assert_eq!(split_tenant("/mcp"), None);
    }

    #[test]
    fn keys_differ_per_tenant_and_purpose() {
        let m = [9u8; 32];
        assert_ne!(derive(&m, "db", "a"), derive(&m, "db", "b"));
        assert_ne!(derive(&m, "db", "a"), derive(&m, "inbox", "a"));
    }
}
