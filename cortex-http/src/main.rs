//! Cortex HTTP Server — local-first REST API for persistent memory.
//!
//! Usage: cortex-http [--port 3315] [--host 127.0.0.1] [--db ~/.cortex/memory.db]

use std::sync::Arc;

use axum::{
    Router,
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

    // Dashboard HTML is embedded at compile time — works in installed binaries.
    const DASHBOARD_HTML: &str = include_str!("../static/index.html");

    let app = Router::new()
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
        .layer(
            tower_http::cors::CorsLayer::new()
                .allow_origin(tower_http::cors::Any)
                .allow_methods(tower_http::cors::Any)
                .allow_headers(tower_http::cors::Any),
        )
        .layer(tower_http::trace::TraceLayer::new_for_http());

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
    println!("     DB:        {db_path}");
    println!();
    println!("  ⭐ Enjoying Cortex? Star it — it helps others find it:");
    println!("     https://github.com/gambletan/cortex");
    println!();

    axum::serve(listener, app).await.unwrap();
}
