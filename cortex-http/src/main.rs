//! Cortex HTTP Server — local-first REST API for persistent memory.
//!
//! Usage: cortex-http [--port 3315] [--host 127.0.0.1] [--db ~/.cortex/memory.db]

use std::sync::Arc;

use axum::{
    Router,
    extract::State,
    routing::{get, post},
};
// Dashboard HTML is embedded via include_str! — no ServeDir needed
use clap::Parser;
use tracing::info;

use cortex_core::Cortex;

mod handlers;

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(name = "cortex-http", about = "Cortex memory engine — HTTP API")]
struct Cli {
    /// Port to listen on
    #[arg(long, default_value = "3315", env = "CORTEX_PORT")]
    port: u16,

    /// Host to bind (127.0.0.1 = local only, 0.0.0.0 = all interfaces)
    #[arg(long, default_value = "127.0.0.1", env = "CORTEX_HOST")]
    host: String,

    /// Path to SQLite database
    #[arg(long, env = "CORTEX_DB_PATH")]
    db: Option<String>,
}

// ── App state ────────────────────────────────────────────────────────────────

pub struct AppState {
    pub cortex: Cortex,
}

/// Host names the API answers to besides `localhost` and IP literals, from
/// `CORTEX_ALLOWED_HOSTS` (comma-separated, e.g. a LAN name for the Docker image).
fn allowed_hosts_from_env() -> Vec<String> {
    std::env::var("CORTEX_ALLOWED_HOSTS")
        .unwrap_or_default()
        .split(',')
        .map(|h| h.trim().to_ascii_lowercase())
        .filter(|h| !h.is_empty())
        .collect()
}

/// `true` if a Host / authority value names this server: `localhost`, an IP literal, or
/// an explicitly allowed name. A DNS-rebinding page always arrives under its own domain.
fn host_allowed(host: &str, extra: &[String]) -> bool {
    let is_port = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit());
    if let Some(rest) = host.strip_prefix('[') {
        // Bracketed IPv6: `[addr]` or `[addr]:port`, nothing else.
        let Some((addr, tail)) = rest.split_once(']') else { return false };
        let tail_ok = tail.is_empty() || tail.strip_prefix(':').is_some_and(is_port);
        return tail_ok && addr.parse::<std::net::Ipv6Addr>().is_ok();
    }
    let name = match host.rsplit_once(':') {
        Some((name, port)) if is_port(port) => name,
        Some(_) => return false,
        None => host,
    };
    name.eq_ignore_ascii_case("localhost")
        || name.parse::<std::net::Ipv4Addr>().is_ok()
        || extra.iter().any(|h| h.eq_ignore_ascii_case(name))
}

/// The API has no authentication and serves every memory (Private included), so it
/// must only ever be reachable by local, non-browser clients and its own dashboard:
/// refuse foreign Host names (DNS rebinding) and any cross-origin browser request
/// (drive-by reads of /v1/export, CSRF into /v1/import). No CORS is granted.
async fn guard_origin(
    State(extra): State<Arc<Vec<String>>>,
    req: axum::extract::Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{header, StatusCode};
    use axum::response::IntoResponse;

    let forbidden = |why: &'static str| (StatusCode::FORBIDDEN, why).into_response();
    let header_host = match req.headers().get(header::HOST).map(|v| v.to_str()) {
        Some(Ok(h)) => Some(h),
        Some(Err(_)) => return forbidden("host not allowed"),
        None => None,
    };
    let authority = req.uri().authority().map(|a| a.as_str());
    // HTTP/2 carries `:authority`; if both are present they must agree.
    if let (Some(h), Some(a)) = (header_host, authority) {
        if !h.eq_ignore_ascii_case(a) {
            return forbidden("host not allowed");
        }
    }
    let host = match header_host.or(authority) {
        Some(h) if host_allowed(h, &extra) => h.to_owned(),
        _ => return forbidden("host not allowed"),
    };
    // Browsers label every cross-site request, even ones that carry no Origin.
    if req
        .headers()
        .get("sec-fetch-site")
        .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"cross-site"))
    {
        return forbidden("cross-origin requests are not allowed");
    }
    if let Some(origin) = req.headers().get(header::ORIGIN) {
        let same_origin = origin.to_str().is_ok_and(|o| {
            o.eq_ignore_ascii_case(&format!("http://{host}"))
                || o.eq_ignore_ascii_case(&format!("https://{host}"))
        });
        if !same_origin {
            return forbidden("cross-origin requests are not allowed");
        }
    }
    next.run(req).await
}

/// Build the HTTP router.
fn app(state: Arc<AppState>, allowed_hosts: Vec<String>) -> Router {
    // Dashboard HTML is embedded at compile time — works in installed binaries.
    const DASHBOARD_HTML: &str = include_str!("../static/index.html");

    Router::new()
        // Health
        .route("/health", get(handlers::health))
        // Stats (dashboard)
        .route("/v1/stats", get(handlers::stats))
        // Memory CRUD
        .route("/v1/memories", post(handlers::ingest))
        .route("/v1/memories/search", post(handlers::search))
        .route("/v1/memories/context", get(handlers::context))
        .route("/v1/memories/consolidate", post(handlers::consolidate))
        .route("/v1/memories/infer", post(handlers::infer))
        // Facts & preferences
        .route("/v1/facts", post(handlers::add_fact))
        .route("/v1/facts/contradictions", post(handlers::check_contradictions))
        .route("/v1/preferences", post(handlers::set_preference))
        // Beliefs
        .route("/v1/beliefs", get(handlers::list_beliefs))
        .route("/v1/beliefs/observe", post(handlers::observe_belief))
        // Recent memories (for dashboard)
        .route("/v1/memories/recent", get(handlers::recent_memories))
        // People
        .route("/v1/people", get(handlers::list_people).post(handlers::resolve_person))
        // Import/Export
        .route("/v1/export", get(handlers::export_all))
        .route("/v1/import", post(handlers::import_all))
        // Phase 5: Compression & Relationships
        .route("/v1/memories/compress", post(handlers::compress))
        .route("/v1/relationships/extract", post(handlers::extract_relationships))
        // Quick note (mobile capture)
        .route("/api/quick-note", post(handlers::quick_note))
        .with_state(state)
        // Dashboard — embedded HTML, no external files needed
        .route("/", get(|| async {
            axum::response::Html(DASHBOARD_HTML)
        }))
        .layer(axum::middleware::from_fn_with_state(Arc::new(allowed_hosts), guard_origin))
        .layer(tower_http::trace::TraceLayer::new_for_http())
}

// ── Main ─────────────────────────────────────────────────────────────────────

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("cortex_http=info".parse().unwrap()),
        )
        .init();

    let cli = Cli::parse();

    let db_path = cli.db.unwrap_or_else(|| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.cortex/memory.db")
    });

    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

    info!(db = %db_path, "opening cortex database");

    let cortex = Cortex::open(&db_path).expect("failed to open cortex database");
    let state = Arc::new(AppState { cortex });

    let app = app(state, allowed_hosts_from_env());

    let addr = format!("{}:{}", cli.host, cli.port);
    info!(addr = %addr, "starting cortex-http server");

    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .expect("failed to bind");

    // Derive the displayed address from the actual bound socket, not the pre-bind
    // CLI string: this reflects the real port when `--port 0` picks an ephemeral
    // one, and rewrites an unspecified bind (0.0.0.0 / ::) to a navigable loopback.
    let local = listener.local_addr().expect("listener missing local addr");
    let port = local.port();
    let ip = local.ip();
    // For a wildcard bind (0.0.0.0 / ::) the externally-reachable host depends on the
    // network, so advertise a clickable loopback for local use AND note the real
    // all-interfaces bind — rather than printing an unclickable 0.0.0.0 (bad locally)
    // or silently rewriting it to loopback (misleading on Docker / a remote host).
    let (display_host, all_interfaces) = if ip.is_unspecified() {
        let loopback = if ip.is_ipv6() { "[::1]" } else { "127.0.0.1" };
        let bind = if ip.is_ipv6() { format!("[{ip}]:{port}") } else { format!("{ip}:{port}") };
        (loopback.to_string(), Some(bind))
    } else if ip.is_ipv6() {
        (format!("[{ip}]"), None)
    } else {
        (ip.to_string(), None)
    };
    let url = format!("http://{display_host}:{port}");

    // First-run banner — dashboard URL + a gentle star nudge.
    println!();
    println!("  🧠 Cortex memory engine v{}", env!("CARGO_PKG_VERSION"));
    println!("     API:       {url}/v1");
    println!("     Dashboard: {url}/");
    if let Some(bind) = all_interfaces {
        // Describe the wildcard bind plainly — 0.0.0.0 / [::] are bind targets, not
        // reachable URLs, so don't format them as a clickable http:// address.
        println!("     (also bound to all interfaces on {bind} — reach it via this host's IP)");
    }
    if !ip.is_loopback() {
        println!("     ⚠  Not loopback-only: this API has NO authentication and serves every");
        println!("        memory (Private included) to anyone who can reach this port.");
    }
    println!("     DB:        {db_path}");
    println!();
    println!("  ⭐ Enjoying Cortex? Star it — it helps others find it:");
    println!("     https://github.com/gambletan/cortex");
    println!();

    axum::serve(listener, app).await.unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use tower::ServiceExt;

    fn test_app() -> Router {
        app(Arc::new(AppState { cortex: Cortex::in_memory().unwrap() }), Vec::new())
    }

    fn get(host: &str, origin: Option<&str>) -> Request<Body> {
        let mut req = Request::get("/v1/export").header("host", host);
        if let Some(o) = origin {
            req = req.header("origin", o);
        }
        req.body(Body::empty()).unwrap()
    }

    /// Any web page the user visits must not be able to read their memories from the
    /// loopback API: no permissive CORS, and a cross-site Origin is refused outright.
    #[tokio::test]
    async fn cross_origin_page_cannot_read_memories() {
        let res = test_app()
            .oneshot(get("127.0.0.1:3315", Some("https://evil.example")))
            .await
            .unwrap();
        assert!(res.headers().get("access-control-allow-origin").is_none());
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    /// Cross-site writes (CSRF into /v1/import etc.) are refused, preflight included.
    #[tokio::test]
    async fn cross_origin_preflight_and_write_refused() {
        let pre = Request::builder()
            .method("OPTIONS")
            .uri("/v1/import")
            .header("host", "localhost:3315")
            .header("origin", "https://evil.example")
            .header("access-control-request-method", "POST")
            .body(Body::empty())
            .unwrap();
        let res = test_app().oneshot(pre).await.unwrap();
        assert!(res.headers().get("access-control-allow-origin").is_none());

        let write = Request::post("/v1/memories")
            .header("host", "localhost:3315")
            .header("origin", "https://evil.example")
            .header("content-type", "text/plain")
            .body(Body::from(r#"{"text":"injected","channel":"x"}"#))
            .unwrap();
        assert_eq!(test_app().oneshot(write).await.unwrap().status(), StatusCode::FORBIDDEN);
    }

    /// A cross-site request without an Origin header is still labelled by the browser.
    #[tokio::test]
    async fn sec_fetch_site_cross_site_refused() {
        let req = Request::get("/v1/export")
            .header("host", "127.0.0.1:3315")
            .header("sec-fetch-site", "cross-site")
            .body(Body::empty())
            .unwrap();
        assert_eq!(test_app().oneshot(req).await.unwrap().status(), StatusCode::FORBIDDEN);
    }

    /// DNS rebinding: a page on evil.com re-pointed at 127.0.0.1 is same-origin to the
    /// browser, but its Host header still names the attacker's domain.
    #[tokio::test]
    async fn dns_rebinding_host_refused() {
        let res = test_app()
            .oneshot(get("rebind.evil.com:3315", Some("http://rebind.evil.com:3315")))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
        let res = test_app().oneshot(get("rebind.evil.com:3315", None)).await.unwrap();
        assert_eq!(res.status(), StatusCode::FORBIDDEN);
    }

    /// Legit clients keep working: curl/SDKs (no Origin), the same-origin dashboard,
    /// IP-literal hosts (Docker / LAN by IP), and explicitly allowed host names.
    #[tokio::test]
    async fn local_clients_and_same_origin_dashboard_allowed() {
        for (host, origin) in [
            ("127.0.0.1:3315", None),
            ("localhost:3315", None),
            ("[::1]:3315", None),
            ("192.168.1.20:3315", None),
            ("127.0.0.1:3315", Some("http://127.0.0.1:3315")),
            ("localhost:3315", Some("http://localhost:3315")),
        ] {
            let res = test_app().oneshot(get(host, origin)).await.unwrap();
            assert_eq!(res.status(), StatusCode::OK, "host={host} origin={origin:?}");
        }
        assert!(host_allowed("cortex.lan:3315", &["cortex.lan".to_string()]));
        assert!(!host_allowed("cortex.lan:3315", &[]));
        // Malformed authorities never pass.
        for bad in ["[::1]garbage", "[::1", "[::1]:", "localhost:", "localhost:x", "evil.com", "[evil]:1"] {
            assert!(!host_allowed(bad, &[]), "{bad}");
        }
        assert!(host_allowed("[::1]", &[]) && host_allowed("LOCALHOST", &[]));
    }
}
