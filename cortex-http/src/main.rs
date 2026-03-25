//! Cortex HTTP Server — local-first REST API for persistent memory.
//!
//! Usage: cortex-http [--port 3315] [--host 127.0.0.1] [--db ~/.cortex/memory.db]

use std::sync::Arc;

use axum::{
    Router,
    routing::{get, post},
};
use tower_http::services::ServeDir;
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

    // Resolve the static directory relative to the executable, falling back to compile-time path.
    let static_dir = {
        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|p| p.parent().map(|d| d.to_path_buf()));
        let candidates = [
            exe_dir.as_ref().map(|d| d.join("static")),
            exe_dir.as_ref().map(|d| d.join("../static")),
            Some(std::path::PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/static"))),
        ];
        candidates
            .into_iter()
            .flatten()
            .find(|p| p.is_dir())
            .unwrap_or_else(|| std::path::PathBuf::from("static"))
    };

    info!(path = %static_dir.display(), "serving static files");

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
        // People
        .route("/v1/people", get(handlers::list_people).post(handlers::resolve_person))
        // Import/Export
        .route("/v1/export", get(handlers::export_all))
        .route("/v1/import", post(handlers::import_all))
        // Phase 5: Compression & Relationships
        .route("/v1/memories/compress", post(handlers::compress))
        .route("/v1/relationships/extract", post(handlers::extract_relationships))
        .with_state(state)
        // Serve static files (dashboard) — fallback so API routes take priority
        .fallback_service(ServeDir::new(&static_dir))
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

    axum::serve(listener, app).await.unwrap();
}
