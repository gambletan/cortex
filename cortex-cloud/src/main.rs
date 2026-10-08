//! Cortex Cloud for Muse: an always-on home for the memories a user chose to share with
//! Muse, so Muse works from a phone with the user's computer off. Design:
//! `docs/design/muse-cloud.md`.
//!
//! ```text
//!                      ┌──────────────────── cortex-cloud ────────────────────┐
//! Muse ── /t/<rid>/… ─▶│ tenant router (LRU) ─▶ gateway (unchanged, per dir)   │  lane: 192
//!   /.well-known/…/t/<rid>…  (RFC 8414/9728 inserted forms → same tenant)       │
//! device ── /api/… ───▶│ headers checked BEFORE the body is read; Ed25519-signed│  lane: 64
//!                      │ register · export · enroll · inbox · status · delete   │
//!                      │ <data>/tenants/<rid>/export.db  SQLCipher raw key = HMAC(master, rid)
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
use std::time::{Duration, Instant};

use axum::body::{Body, Bytes};
use axum::extract::{ConnectInfo, Path as UrlPath, Request, State};
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
/// Each cached tenant holds two SQLite connections (writer + one reader).
const TENANT_CACHE: usize = 64;
const MAX_TENANTS: usize = 20_000;
const REGISTER_PER_NET_PER_HOUR: u32 = 5;
const REGISTER_GLOBAL_PER_MINUTE: u32 = 30;
/// Small bodies (register, enroll, ack, …) vs the export push.
const MAX_SMALL_BODY: usize = 8 * 1024;
const MAX_EXPORT_BODY: usize = cloud::MAX_EXPORT_BODY;
const BODY_DEADLINE: Duration = Duration::from_secs(20);
const IDLE_DAYS: i64 = 90;
/// A tenant that never pushed anything within a day is abandoned (or abuse): reclaim it.
const UNUSED_TENANT_SECS: i64 = 24 * 3600;
const API_LANE: usize = 64;
const MUSE_LANE: usize = 192;
const MAX_CONCURRENT_OPENS: usize = 4;

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

/// A tenant's gateway, its database, its state paths, and the public id the router was
/// built for (its OAuth issuer).
type Tenant = (Router, Arc<Cortex>, Paths, String);

struct App {
    base_url: String,
    tenants_dir: PathBuf,
    trash_dir: PathBuf,
    /// `public/<pid>` → management id. Muse only ever sees the public id, which rotates on
    /// every enrollment; the device manages its tenant by the stable management id.
    public_dir: PathBuf,
    master: [u8; 32],
    #[cfg(feature = "embeddings")]
    embedder: Option<cortex_core::embedder::Embedder>,
    cache: Mutex<Cache>,
    /// Bounds concurrent tenant opens (each costs file descriptors and SQLCipher setup).
    opens: tokio::sync::Semaphore,
    /// Per-tenant locks: nonce store read-modify-write and export replacement.
    tenant_locks: Mutex<HashMap<String, Arc<Mutex<()>>>>,
    /// Nonces of registration requests (no tenant yet), persisted like tenant nonces.
    register_lock: Mutex<()>,
    register_rate: Mutex<HashMap<IpAddr, (Instant, u32)>>,
    register_global: Mutex<(Instant, u32)>,
    oauth_rate: Arc<oauth::RateMap>,
}

#[derive(Default)]
struct Cache {
    routers: HashMap<String, Tenant>,
    /// Least recently used first.
    order: VecDeque<String>,
}

impl Cache {
    fn touch(&mut self, rid: &str) {
        self.order.retain(|r| r != rid);
        self.order.push_back(rid.to_string());
    }
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
    std::fs::rename(&tmp, path)?;
    // Durable before we acknowledge: nonces and the export watermark are security state.
    sync_dir(path.parent().unwrap_or(Path::new(".")))
}

fn sync_dir(dir: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    std::fs::File::open(dir)?.sync_all()?;
    #[cfg(not(unix))]
    let _ = dir;
    Ok(())
}

fn parse_hex32(s: &str) -> Option<[u8; 32]> {
    let s = s.trim();
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let v: Vec<u8> = (0..64).step_by(2).filter_map(|i| u8::from_str_radix(&s[i..i + 2], 16).ok()).collect();
    v.try_into().ok()
}

fn load_master(path: &Path) -> [u8; 32] {
    match std::fs::read_to_string(path) {
        Ok(s) => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = std::fs::metadata(path).map(|m| m.permissions().mode()).unwrap_or(0);
                if mode & 0o077 != 0 {
                    die("master key file must not be readable by group or others (chmod 600)");
                }
            }
            parse_hex32(&s).unwrap_or_else(|| die("master key must be exactly 64 hex characters"))
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

/// Raise the open-file limit to the hard limit (tenants hold SQLite files open).
fn raise_fd_limit() {
    #[cfg(unix)]
    unsafe {
        let mut lim = libc::rlimit { rlim_cur: 0, rlim_max: 0 };
        if libc::getrlimit(libc::RLIMIT_NOFILE, &mut lim) == 0 && lim.rlim_cur < lim.rlim_max {
            lim.rlim_cur = lim.rlim_max.min(65_536);
            let _ = libc::setrlimit(libc::RLIMIT_NOFILE, &lim);
        }
    }
}

fn now() -> i64 {
    chrono::Utc::now().timestamp()
}

fn api_error(status: StatusCode, msg: &str) -> Response {
    (status, [("cache-control", "no-store")], Json(json!({ "error": msg }))).into_response()
}

/// Log the detail server-side; tell the client nothing about internals.
fn internal(detail: impl std::fmt::Display) -> Response {
    tracing::error!(error = %detail, "internal error");
    api_error(StatusCode::INTERNAL_SERVER_ERROR, "internal error")
}

impl App {
    fn tenant_dir(&self, rid: &str) -> Option<PathBuf> {
        oauth::is_tenant_id(rid).then(|| self.tenants_dir.join(rid))
    }

    fn mcp_url(&self, pid: &str) -> String {
        format!("{}/t/{pid}/mcp", self.base_url)
    }

    /// The tenant's current public id (what Muse's URL carries).
    fn public_id(&self, mid: &str) -> Option<String> {
        let dir = self.tenant_dir(mid)?;
        let pid = std::fs::read_to_string(dir.join("public_id")).ok()?.trim().to_string();
        oauth::is_tenant_id(&pid).then_some(pid)
    }

    /// Public id → management id, only if that public id is still the tenant's current one.
    fn resolve_public(&self, pid: &str) -> Option<String> {
        if !oauth::is_tenant_id(pid) {
            return None;
        }
        let mid = std::fs::read_to_string(self.public_dir.join(pid)).ok()?.trim().to_string();
        (oauth::is_tenant_id(&mid) && self.public_id(&mid).as_deref() == Some(pid)).then_some(mid)
    }

    /// Give the tenant a fresh public id: new mapping first, then the tenant's pointer, then
    /// drop the old mapping. At every step the device can still reach the tenant by its
    /// management id, and at most the current public id resolves.
    fn rotate_public_id(&self, mid: &str) -> std::io::Result<String> {
        let dir = self.tenant_dir(mid).ok_or_else(|| std::io::Error::other("bad tenant id"))?;
        let old = self.public_id(mid);
        let new = oauth::new_tenant_id();
        private_write(&self.public_dir.join(&new), mid.as_bytes())?;
        private_write(&dir.join("public_id"), new.as_bytes())?;
        if let Some(old) = old {
            let _ = std::fs::remove_file(self.public_dir.join(old));
            let _ = sync_dir(&self.public_dir);
        }
        self.evict(mid);
        Ok(new)
    }

    fn evict(&self, rid: &str) {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        cache.routers.remove(rid);
        cache.order.retain(|r| r != rid);
    }

    fn tenant_lock(&self, rid: &str) -> Arc<Mutex<()>> {
        let mut locks = self.tenant_locks.lock().unwrap_or_else(|p| p.into_inner());
        if locks.len() > 4 * TENANT_CACHE {
            locks.retain(|_, l| Arc::strong_count(l) > 1);
        }
        locks.entry(rid.to_string()).or_default().clone()
    }

    fn cached(&self, rid: &str) -> Option<Tenant> {
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        let t = cache.routers.get(rid).cloned()?;
        cache.touch(rid);
        Some(t)
    }

    /// The tenant's gateway, opening (and caching) it on first use. `None` if no such tenant.
    fn tenant(&self, rid: &str) -> Result<Option<Tenant>, String> {
        if let Some(t) = self.cached(rid) {
            return Ok(Some(t));
        }
        let Some(dir) = self.tenant_dir(rid) else { return Ok(None) };
        if !dir.join("device.pub").is_file() {
            return Ok(None);
        }
        let Ok(_permit) = self.opens.try_acquire() else {
            return Err("busy opening tenants".into());
        };
        // `rid` here is the stable management id: keys never change when the public id rotates.
        let Some(pid) = self.public_id(rid) else { return Ok(None) };
        let db = dir.join("export.db");
        let cortex = Cortex::open_with_raw_key(&db.to_string_lossy(), &derive(&self.master, "db", rid), 1)
            .map_err(|e| format!("tenant storage: {e}"))?;
        #[cfg(feature = "embeddings")]
        let cortex = match &self.embedder {
            Some(e) => cortex.with_embedder(e.clone()),
            // The shared model is unavailable: keyword recall, and no per-tenant downloads.
            None => cortex.without_embedder(),
        };
        let cortex = Arc::new(cortex);
        let paths = Paths::for_dir(&dir, Some(derive(&self.master, "inbox", rid)));
        let cfg = oauth::OAuthConfig::for_tenant(&self.base_url, &pid, self.oauth_rate.clone())?;
        let router = gateway::tenant_router(cortex.clone(), paths.clone(), cfg, &TenantConfig::default());
        let entry: Tenant = (router, cortex, paths, pid.clone());
        let mut cache = self.cache.lock().unwrap_or_else(|p| p.into_inner());
        // Deleted while we were opening? Then don't resurrect it.
        if !dir.join("device.pub").is_file() {
            return Ok(None);
        }
        // Public id rotated while we were opening? This router carries the old issuer:
        // never cache it (rotation evicts only AFTER writing the new id, so checking here,
        // under the cache lock, closes the race).
        if self.public_id(rid).as_deref() != Some(pid.as_str()) {
            return Err("tenant changed while opening; retry".into());
        }
        if let Some(existing) = cache.routers.get(rid).cloned() {
            cache.touch(rid);
            return Ok(Some(existing));
        }
        while cache.routers.len() >= TENANT_CACHE {
            let Some(old) = cache.order.pop_front() else { break };
            cache.routers.remove(&old);
        }
        cache.touch(rid);
        cache.routers.insert(rid.to_string(), entry.clone());
        Ok(Some(entry))
    }

    /// Remove a tenant: atomically move its directory out of the namespace (no request can
    /// resolve it any more), drop it from the cache, then delete it.
    fn remove_tenant(&self, rid: &str) -> std::io::Result<()> {
        let Some(dir) = self.tenant_dir(rid) else { return Ok(()) };
        // Serialize every deletion path (including retention) with active disclosures.
        // Never call OAuth disconnect here: it would recursively acquire this lock.
        let paths = Paths::for_dir(&dir, None);
        let _fence = gateway::budget_file_lock(&paths)?;
        private_write(&dir.join("gateway.disabled"), b"deleted")?;
        if let Some(pid) = self.public_id(rid) {
            match std::fs::remove_file(self.public_dir.join(pid)) {
                Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e),
                _ => sync_dir(&self.public_dir)?,
            }
        }
        let trash = self.trash_dir.join(format!("{rid}-{}", rand::random::<u64>()));
        match std::fs::rename(&dir, &trash) {
            // Durable before we report success: a power loss must not bring it back.
            Ok(()) => {
                sync_dir(&self.tenants_dir)?;
                sync_dir(&self.trash_dir)?;
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        self.evict(rid);
        match std::fs::remove_dir_all(&trash) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(e),
            _ => Ok(()),
        }
    }

    /// Reject a nonce seen within the skew window (persisted, so replay protection survives
    /// a restart). Caller holds the lock guarding `path`.
    fn fresh_nonce_at(path: &Path, nonce: &str) -> Result<bool, String> {
        let mut seen: HashMap<String, i64> = match std::fs::read_to_string(path) {
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
        private_write(path, serde_json::to_string(&seen).unwrap_or_default().as_bytes()).map_err(|e| e.to_string())?;
        Ok(true)
    }
}

fn header_fn(headers: &HeaderMap) -> impl Fn(&str) -> Option<String> + '_ {
    move |name| headers.get(name).and_then(|v| v.to_str().ok()).map(str::to_string)
}

fn target(uri: &Uri) -> String {
    uri.path_and_query().map(|p| p.as_str().to_string()).unwrap_or_else(|| uri.path().to_string())
}

/// Cheap checks on headers only, before any body byte is read: the tenant exists, the
/// declared key is the tenant's key, the timestamp is fresh.
fn precheck(app: &App, rid: &str, headers: &HeaderMap) -> Result<(PathBuf, String), Response> {
    let not_found = || api_error(StatusCode::NOT_FOUND, "no such tenant");
    let dir = app.tenant_dir(rid).ok_or_else(not_found)?;
    let key = std::fs::read_to_string(dir.join("device.pub")).map_err(|_| not_found())?;
    let key = key.trim().to_string();
    let h = header_fn(headers);
    let fresh = h(cloud::H_TS).and_then(|t| t.parse::<i64>().ok()).is_some_and(|t| (now() - t).abs() <= cloud::MAX_SKEW_SECS);
    if h(cloud::H_KEY).as_deref() != Some(key.as_str()) || !fresh {
        return Err(api_error(StatusCode::UNAUTHORIZED, "signature refused"));
    }
    Ok((dir, key))
}

/// Read a body with a size cap and an absolute deadline.
async fn read_body(headers: &HeaderMap, body: Body, max: usize) -> Result<Bytes, Response> {
    let declared = headers.get("content-length").and_then(|v| v.to_str().ok()).and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > max) {
        return Err(api_error(StatusCode::PAYLOAD_TOO_LARGE, "body too large"));
    }
    match tokio::time::timeout(BODY_DEADLINE, axum::body::to_bytes(body, max)).await {
        Ok(Ok(b)) => Ok(b),
        Ok(Err(_)) => Err(api_error(StatusCode::PAYLOAD_TOO_LARGE, "body too large")),
        Err(_) => Err(api_error(StatusCode::REQUEST_TIMEOUT, "body too slow")),
    }
}

/// Open a tenant for an AUTHENTICATED device call. A database that can't be decrypted
/// (e.g. the master key was replaced) holds nothing recoverable: drop it and answer 404,
/// so the device re-registers and pushes its list again.
fn open_or_drop(app: &App, rid: &str) -> Result<Tenant, Response> {
    match app.tenant(rid) {
        Ok(Some(t)) => Ok(t),
        Ok(None) => Err(api_error(StatusCode::NOT_FOUND, "no such tenant")),
        Err(e) if e.contains("not a database") => {
            tracing::warn!(tenant = %rid, "tenant database unreadable; removing it");
            match app.remove_tenant(rid) {
                Ok(()) => Err(api_error(StatusCode::NOT_FOUND, "no such tenant")),
                Err(e) => Err(internal(e)),
            }
        }
        Err(e) => Err(internal(e)),
    }
}

fn client_net(peer: SocketAddr, headers: &HeaderMap) -> IpAddr {
    // Trust X-Forwarded-For only from the local TLS proxy; take the entry it appended.
    let ip = if peer.ip().is_loopback() {
        headers
            .get("x-forwarded-for")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.rsplit(',').next())
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or(peer.ip())
    } else {
        peer.ip()
    };
    // IPv6: one subscriber is a /64.
    match ip {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(std::net::Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

async fn blocking<T: Send + 'static>(f: impl FnOnce() -> T + Send + 'static) -> Result<T, Response> {
    tokio::task::spawn_blocking(f).await.map_err(internal)
}

// ── Device API ───────────────────────────────────────────────────────────────

async fn register(
    State(app): State<Arc<App>>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let net = client_net(peer, &headers);
    {
        // Per-network first: a network that is already over its allowance must not spend
        // the service-wide budget.
        let now_i = Instant::now();
        let mut rate = app.register_rate.lock().unwrap_or_else(|p| p.into_inner());
        rate.retain(|_, (start, _)| now_i.duration_since(*start).as_secs() < 3600);
        let slot = rate.entry(net).or_insert((now_i, 0));
        if slot.1 >= REGISTER_PER_NET_PER_HOUR {
            return api_error(StatusCode::TOO_MANY_REQUESTS, "too many registrations");
        }
        let mut g = app.register_global.lock().unwrap_or_else(|p| p.into_inner());
        if now_i.duration_since(g.0).as_secs() >= 60 {
            *g = (now_i, 0);
        }
        if g.1 >= REGISTER_GLOBAL_PER_MINUTE {
            return api_error(StatusCode::TOO_MANY_REQUESTS, "too many registrations");
        }
        slot.1 += 1;
        g.1 += 1;
    }
    let body = match read_body(&headers, body, MAX_SMALL_BODY).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    // Proof of possession: the request is signed by the key being registered.
    let (key, nonce) = match cloud::verify(method.as_str(), &target(&uri), &body, header_fn(&headers), None, now()) {
        Ok(v) => v,
        Err(e) => return api_error(StatusCode::UNAUTHORIZED, &format!("signature refused: {e:?}")),
    };
    let declared = serde_json::from_slice::<Value>(&body)
        .ok()
        .and_then(|v| v.get("public_key").and_then(Value::as_str).map(str::to_string));
    if declared.as_deref() != Some(key.as_str()) {
        return api_error(StatusCode::BAD_REQUEST, "public_key must be the signing key");
    }
    let app2 = app.clone();
    let out = blocking(move || -> Result<(String, String), Response> {
        {
            let _g = app2.register_lock.lock().unwrap_or_else(|p| p.into_inner());
            match App::fresh_nonce_at(&app2.tenants_dir.with_file_name("register-nonces.json"), &nonce) {
                Ok(true) => {}
                Ok(false) => return Err(api_error(StatusCode::UNAUTHORIZED, "signature refused: replay")),
                Err(e) => return Err(internal(e)),
            }
        }
        let count = std::fs::read_dir(&app2.tenants_dir).map(|d| d.count()).unwrap_or(0);
        if count >= MAX_TENANTS {
            return Err(api_error(StatusCode::SERVICE_UNAVAILABLE, "service is full"));
        }
        // Build the directory aside, then rename it into place: the sweeper never sees a
        // half-made tenant.
        // `rid` (management id) is only ever known to the device; `pid` goes into Muse's URL.
        let rid = oauth::new_tenant_id();
        let pid = oauth::new_tenant_id();
        let staging = app2.trash_dir.join(format!("new-{rid}"));
        let create = || -> std::io::Result<()> {
            std::fs::create_dir(&staging)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&staging, std::fs::Permissions::from_mode(0o700))?;
            }
            private_write(&staging.join("device.pub"), key.as_bytes())?;
            private_write(&staging.join("public_id"), pid.as_bytes())?;
            private_write(&staging.join("last_seen"), now().to_string().as_bytes())?;
            std::fs::rename(&staging, app2.tenants_dir.join(&rid))?;
            sync_dir(&app2.tenants_dir)?;
            private_write(&app2.public_dir.join(&pid), rid.as_bytes())
        };
        create().map_err(|e| {
            let _ = std::fs::remove_dir_all(&staging);
            internal(e)
        })?;
        Ok((rid, pid))
    })
    .await;
    match out {
        Ok(Ok((rid, pid))) => (StatusCode::CREATED, Json(json!({ "rid": rid, "mcp_url": app.mcp_url(&pid) }))).into_response(),
        Ok(Err(r)) | Err(r) => r,
    }
}

#[derive(Deserialize)]
struct ExportBody {
    /// Device clock (ms) when the snapshot was taken; older pushes are refused.
    #[serde(default)]
    version: i64,
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

/// One handler for every signed tenant operation: header precheck, bounded body read,
/// signature + nonce, then dispatch.
async fn tenant_api(
    State(app): State<Arc<App>>,
    UrlPath(params): UrlPath<Vec<(String, String)>>,
    method: Method,
    uri: Uri,
    headers: HeaderMap,
    body: Body,
) -> Response {
    let rid = params.iter().find(|(k, _)| k == "rid").map(|(_, v)| v.clone()).unwrap_or_default();
    let op = params.iter().find(|(k, _)| k == "op").map(|(_, v)| v.clone()).unwrap_or_default();
    let op = if method == Method::POST && uri.path().ends_with("/inbox/ack") { "ack".to_string() } else { op };
    let (dir, key) = match precheck(&app, &rid, &headers) {
        Ok(v) => v,
        Err(r) => return r,
    };
    let max = if op == "export" { MAX_EXPORT_BODY } else { MAX_SMALL_BODY };
    let body = match read_body(&headers, body, max).await {
        Ok(b) => b,
        Err(r) => return r,
    };
    let nonce = match cloud::verify(method.as_str(), &target(&uri), &body, header_fn(&headers), Some(&key), now()) {
        Ok((_, n)) => n,
        Err(_) => return api_error(StatusCode::UNAUTHORIZED, "signature refused"),
    };
    let app2 = app.clone();
    let out = blocking(move || -> Response {
        let lock = app2.tenant_lock(&rid);
        let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
        match App::fresh_nonce_at(&dir.join("nonces.json"), &nonce) {
            Ok(true) => {}
            Ok(false) => return api_error(StatusCode::UNAUTHORIZED, "signature refused: replay"),
            Err(e) => return internal(e),
        }
        let _ = private_write(&dir.join("last_seen"), now().to_string().as_bytes());
        if method == Method::DELETE && op.is_empty() {
            return match app2.remove_tenant(&rid) {
                Ok(()) => Json(json!({ "deleted": true })).into_response(),
                Err(e) => internal(e),
            };
        }
        if method == Method::POST && op == "enroll" {
            // Every enrollment gets a NEW public URL, so any earlier link (used, expired or
            // leaked) stops working for good. The tenant itself doesn't move: the device
            // keeps managing it by its management id, even if this response is lost.
            let pid = match app2.rotate_public_id(&rid) {
                Ok(p) => p,
                Err(e) => return internal(e),
            };
            let paths = match open_or_drop(&app2, &rid) {
                Ok((_, _, p, _)) => p,
                Err(r) => return r,
            };
            return match oauth::open_enrollment(&paths, ENROLL_SECS) {
                Ok(()) => Json(json!({ "mcp_url": app2.mcp_url(&pid), "expires_in": ENROLL_SECS })).into_response(),
                Err(e) => internal(e),
            };
        }
        let (_, cortex, paths, _) = match open_or_drop(&app2, &rid) {
            Ok(t) => t,
            Err(r) => return r,
        };
        match (method.as_str(), op.as_str()) {
            ("PUT", "export") => {
                let Ok(b) = serde_json::from_slice::<ExportBody>(&body) else {
                    return api_error(StatusCode::BAD_REQUEST, "expected {version, items:[{text, embedding?}]}");
                };
                // Serialized by the tenant lock; a snapshot older than the last applied one
                // is refused, so an unshared item can never come back from a stale push.
                let vpath = dir.join("export.version");
                let applied = std::fs::read_to_string(&vpath).ok().and_then(|s| s.trim().parse::<i64>().ok()).unwrap_or(0);
                if b.version <= applied {
                    return api_error(StatusCode::CONFLICT, "stale export snapshot");
                }
                let items: Vec<ExportItem> =
                    b.items.into_iter().map(|i| ExportItem { text: i.text, embedding: i.embedding }).collect();
                if let Err(e) = gateway::validate_export(&items) {
                    return api_error(StatusCode::BAD_REQUEST, &e);
                }
                // A successful unshare must be ordered after any already-running recall.
                let _fence = match gateway::budget_file_lock(&paths) {
                    Ok(f) => f,
                    Err(e) => return internal(e),
                };
                // The watermark is persisted BEFORE the export changes: if we crash or fail
                // midway, no older snapshot can be applied afterwards; the device simply
                // retries with a newer one and converges.
                if let Err(e) = private_write(&vpath, b.version.to_string().as_bytes()) {
                    return internal(e);
                }
                match gateway::replace_export(&cortex, &items) {
                    Ok(n) => {
                        let _ = private_write(&dir.join("pushed"), b"1");
                        Json(json!({ "count": n })).into_response()
                    }
                    Err(e) => internal(e),
                }
            }
            ("GET", "inbox") => match gateway::inbox_items(&paths) {
                Ok(items) => {
                    let items: Vec<Value> =
                        items.into_iter().map(|i| json!({ "id": i.id, "ts": i.ts, "text": i.text })).collect();
                    ([("cache-control", "no-store")], Json(json!({ "items": items }))).into_response()
                }
                Err(e) => internal(e),
            },
            ("POST", "ack") => {
                let Ok(b) = serde_json::from_slice::<AckBody>(&body) else {
                    return api_error(StatusCode::BAD_REQUEST, "expected {ids:[…]}");
                };
                match gateway::inbox_ack(&paths, &b.ids) {
                    Ok(n) => Json(json!({ "removed": n })).into_response(),
                    Err(e) => internal(e),
                }
            }
            ("GET", "status") => {
                let shared = match gateway::export_rows(&cortex) {
                    Ok(rows) => rows.len(),
                    Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "Shared memories are unavailable"),
                };
                let (connected, connected_at, last_used) = match oauth::grant_summary(&paths) {
                    Ok(summary) => summary,
                    Err(_) => return api_error(StatusCode::SERVICE_UNAVAILABLE, "Connection status is unavailable"),
                };
                let reads = gateway::reads_today(&cortex, &paths);
                let read_error = reads.as_ref().err().map(|_| "Read history is unavailable.");
                let read_today = reads.ok().map(|r| r.into_iter()
                    .map(|(text, times)| json!({ "text": text, "times": times }))
                    .collect::<Vec<_>>());
                ([("cache-control", "no-store")],
                Json(json!({
                    "shared": shared,
                    "connected": connected,
                    "connected_at": connected_at,
                    "last_used": last_used,
                    "read_today": read_today,
                    "read_today_error": read_error,
                })))
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
    let Some((pid, inner_path)) = split_tenant(req.uri().path()).map(|(r, p)| (r.to_string(), p)) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Muse addresses the public id; only the tenant's CURRENT public id resolves.
    let Some(rid) = app.resolve_public(&pid) else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let router = match app.cached(&rid) {
        Some(t) => t,
        None => {
            let app2 = app.clone();
            let rid2 = rid.clone();
            match blocking(move || app2.tenant(&rid2)).await {
                Ok(Ok(Some(t))) => t,
                Ok(Ok(None)) => return StatusCode::NOT_FOUND.into_response(),
                Ok(Err(e)) => {
                    tracing::warn!(error = %e, "tenant open failed");
                    return StatusCode::SERVICE_UNAVAILABLE.into_response();
                }
                Err(r) => return r,
            }
        }
    };
    // The router must have been built for exactly the public id in this request: a stale
    // link racing a rotation must never be answered by the new router (whose discovery
    // would reveal the new link).
    let (router, _, _, built_for) = router;
    if built_for != pid {
        return StatusCode::NOT_FOUND.into_response();
    }
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
    // Two lanes: the device API can't starve Muse, and vice versa. Bodies are read inside
    // the handlers, after the header precheck, with per-route caps.
    let api = Router::new()
        .route("/api/tenants", post(register))
        .route("/api/tenants/{rid}", delete(tenant_api))
        .route("/api/tenants/{rid}/{op}", get(tenant_api).put(tenant_api).post(tenant_api))
        .route("/api/tenants/{rid}/inbox/ack", post(tenant_api))
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(API_LANE));
    Router::new()
        .merge(api)
        .route("/healthz", get(|| async { "ok" }))
        .fallback_service(
            Router::new()
                .fallback(route_tenant)
                .layer(tower::limit::GlobalConcurrencyLimitLayer::new(MUSE_LANE))
                .with_state(app.clone()),
        )
        .with_state(app)
}

/// Delete tenants whose device hasn't called in `IDLE_DAYS`, and tenants that never pushed
/// anything within a day of registering. Also empties leftovers in the trash.
fn sweep(app: &App) {
    if let Ok(entries) = std::fs::read_dir(&app.trash_dir) {
        for e in entries.flatten() {
            let old = e.metadata().and_then(|m| m.modified()).ok().and_then(|t| t.elapsed().ok());
            if old.is_some_and(|d| d.as_secs() > 3600) {
                let _ = std::fs::remove_dir_all(e.path());
            }
        }
    }
    // Mappings that no longer point at a tenant's current public id. Checked again under
    // the tenant's lock (rotation holds it), and never touching in-progress temp files.
    if let Ok(entries) = std::fs::read_dir(&app.public_dir) {
        for e in entries.flatten() {
            let pid = e.file_name().to_string_lossy().to_string();
            if !oauth::is_tenant_id(&pid) || app.resolve_public(&pid).is_some() {
                continue;
            }
            let mid = std::fs::read_to_string(e.path()).unwrap_or_default().trim().to_string();
            let lock = app.tenant_lock(&mid);
            let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
            if app.resolve_public(&pid).is_none() {
                let _ = std::fs::remove_file(e.path());
            }
        }
    }
    let Ok(entries) = std::fs::read_dir(&app.tenants_dir) else { return };
    let t = now();
    for e in entries.flatten() {
        let rid = e.file_name().to_string_lossy().to_string();
        if !oauth::is_tenant_id(&rid) {
            continue;
        }
        let expired = || {
            let last = std::fs::read_to_string(e.path().join("last_seen"))
                .ok()
                .and_then(|s| s.trim().parse::<i64>().ok());
            let Some(last) = last else { return false }; // being created or damaged: leave it
            let never_pushed = !e.path().join("pushed").exists();
            t - last > IDLE_DAYS * 86_400 || (never_pushed && t - last > UNUSED_TENANT_SECS)
        };
        if !expired() {
            continue;
        }
        // Re-check under the tenant lock: a device call that just refreshed `last_seen`
        // (and pushed data) holds this lock while it works.
        let lock = app.tenant_lock(&rid);
        let _g = lock.lock().unwrap_or_else(|p| p.into_inner());
        if expired() && app.remove_tenant(&rid).is_ok() {
            tracing::info!(tenant = %rid, "deleted idle tenant");
        }
    }
}

fn build_app(args: &Args) -> Arc<App> {
    let base_url = oauth::OAuthConfig::new(&args.base_url, &[])
        .map(|c| c.issuer)
        .unwrap_or_else(|e| die(&e.replace("--public-url", "--base-url")));
    let tenants_dir = args.data_dir.join("tenants");
    let trash_dir = args.data_dir.join("trash");
    let public_dir = args.data_dir.join("public");
    for d in [&tenants_dir, &trash_dir, &public_dir] {
        std::fs::create_dir_all(d).unwrap_or_else(|e| die(&e.to_string()));
    }
    Arc::new(App {
        base_url,
        tenants_dir,
        trash_dir,
        public_dir,
        master: load_master(&args.master_key),
        #[cfg(feature = "embeddings")]
        embedder: if std::env::var("CORTEX_NO_EMBEDDINGS").is_ok_and(|v| !v.is_empty()) {
            None
        } else {
            // A writable model cache: never the (read-only) working directory.
            if std::env::var_os("FASTEMBED_CACHE_DIR").is_none() {
                std::env::set_var("FASTEMBED_CACHE_DIR", args.data_dir.join("models"));
            }
            match cortex_core::embedder::Embedder::new() {
                Ok(e) => Some(e),
                Err(e) => {
                    eprintln!("warning: embedding model unavailable ({e}); recall falls back to keywords");
                    None
                }
            }
        },
        cache: Mutex::new(Cache::default()),
        opens: tokio::sync::Semaphore::new(MAX_CONCURRENT_OPENS),
        tenant_locks: Mutex::new(HashMap::new()),
        register_lock: Mutex::new(()),
        register_rate: Mutex::new(HashMap::new()),
        register_global: Mutex::new((Instant::now(), 0)),
        oauth_rate: Arc::new(Mutex::new(HashMap::new())),
    })
}

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();
    raise_fd_limit();
    let args = Args::parse();
    let app = build_app(&args);
    let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| die(&e.to_string()));
    rt.block_on(async move {
        let sweeper = app.clone();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(Duration::from_secs(3600));
            loop {
                tick.tick().await;
                let a = sweeper.clone();
                let _ = tokio::task::spawn_blocking(move || sweep(&a)).await;
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

    #[test]
    fn master_key_parsing_is_strict() {
        assert!(parse_hex32(&"ab".repeat(32)).is_some());
        assert!(parse_hex32(&"ab".repeat(31)).is_none());
        assert!(parse_hex32(&format!("{}zz", "ab".repeat(31))).is_none());
    }

    #[test]
    fn ipv6_clients_are_grouped_by_64() {
        let h = HeaderMap::new();
        let a: SocketAddr = "[2001:db8:1:2:aaaa::1]:1".parse().unwrap();
        let b: SocketAddr = "[2001:db8:1:2:bbbb::9]:1".parse().unwrap();
        assert_eq!(client_net(a, &h), client_net(b, &h));
    }
}
