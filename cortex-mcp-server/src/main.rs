//! Cortex MCP Server — gives LLMs persistent memory via Model Context Protocol.
//!
//! Communicates over stdio using JSON-RPC 2.0 (MCP transport).
//! Tools: memory_ingest, memory_search, memory_context, belief_observe, belief_list,
//!        person_resolve, fact_add, preference_set

use std::io::{self, BufRead, Write};
use std::sync::Arc;

use clap::{Parser, Subcommand};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, error, info};

use cortex_core::Cortex;

mod tools;

// ── JSON-RPC types ──────────────────────────────────────────────────────────

#[derive(Debug, Deserialize)]
struct JsonRpcRequest {
    #[allow(dead_code)]
    jsonrpc: String,
    id: Option<Value>,
    method: String,
    #[serde(default)]
    params: Value,
}

#[derive(Debug, Serialize)]
struct JsonRpcResponse {
    jsonrpc: String,
    id: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<JsonRpcError>,
}

#[derive(Debug, Serialize)]
struct JsonRpcError {
    code: i64,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    data: Option<Value>,
}

impl JsonRpcResponse {
    fn success(id: Value, result: Value) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: Some(result),
            error: None,
        }
    }

    fn error(id: Value, code: i64, message: String) -> Self {
        Self {
            jsonrpc: "2.0".into(),
            id,
            result: None,
            error: Some(JsonRpcError {
                code,
                message,
                data: None,
            }),
        }
    }
}

// ── MCP Protocol constants ──────────────────────────────────────────────────

fn server_name() -> String {
    std::env::var("CORTEX_SERVER_NAME").unwrap_or_else(|_| "cortex-memory".into())
}
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2024-11-05";

// ── CLI ──────────────────────────────────────────────────────────────────────

#[derive(Parser)]
#[command(
    name = "cortex-mcp-server",
    about = "Cortex memory engine — MCP server & CLI tools",
    version
)]
struct Cli {
    /// Path to the Cortex database file
    #[arg(global = true)]
    db_path: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Store a new memory
    Ingest {
        /// Text to remember
        text: String,
        /// Source channel (default: cli)
        #[arg(short, long, default_value = "cli")]
        channel: String,
    },
    /// Search memories
    Search {
        /// Search query
        query: String,
        /// Max results (default: 5)
        #[arg(short, long, default_value = "5")]
        limit: usize,
    },
    /// Show memory statistics
    Stats,
    /// Show cloud sync status and detected providers
    Sync,
    /// Export all data as JSON
    Export {
        /// Output file (default: stdout)
        #[arg(short, long)]
        output: Option<String>,
    },
    /// Import data from JSON file
    Import {
        /// Input JSON file
        file: String,
    },
    /// Show version, DB path, and capabilities
    Info,
}

// ── Server ──────────────────────────────────────────────────────────────────

struct McpServer {
    cortex: Arc<Cortex>,
}

impl McpServer {
    #[allow(dead_code)]
    fn new(db_path: &str) -> Result<Self, cortex_core::CortexError> {
        let cortex = Cortex::open(db_path)?;
        Ok(Self {
            cortex: Arc::new(cortex),
        })
    }

    fn new_with_plugins(db_path: &str) -> Result<Self, cortex_core::CortexError> {
        use cortex_core::plugins::tag_classifier::TagClassifierPlugin;

        let cortex = Cortex::open(db_path)?
            .with_plugin(Box::new(TagClassifierPlugin::new()));
        Ok(Self {
            cortex: Arc::new(cortex),
        })
    }

    fn handle_request(&self, req: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = req.id.clone().unwrap_or(Value::Null);

        match req.method.as_str() {
            // ── MCP lifecycle ───────────────────────────────────────────
            "initialize" => Some(JsonRpcResponse::success(
                id,
                json!({
                    "protocolVersion": PROTOCOL_VERSION,
                    "capabilities": {
                        "tools": {}
                    },
                    "serverInfo": {
                        "name": server_name(),
                        "version": SERVER_VERSION
                    }
                }),
            )),

            "notifications/initialized" => {
                info!("MCP client initialized");
                None // notification, no response
            }

            "ping" => Some(JsonRpcResponse::success(id, json!({}))),

            // ── Tool discovery ──────────────────────────────────────────
            "tools/list" => Some(JsonRpcResponse::success(
                id,
                json!({ "tools": tools::list_tools_with_plugins(&self.cortex) }),
            )),

            // ── Tool execution ──────────────────────────────────────────
            "tools/call" => {
                let tool_name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = req.params.get("arguments").cloned().unwrap_or(json!({}));

                match tools::call_tool(&self.cortex, tool_name, &args) {
                    Ok(result) => Some(JsonRpcResponse::success(
                        id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": result
                            }]
                        }),
                    )),
                    Err(e) => Some(JsonRpcResponse::success(
                        id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": format!("Error: {}", e)
                            }],
                            "isError": true
                        }),
                    )),
                }
            }

            // ── Unknown method ──────────────────────────────────────────
            _ => {
                debug!(method = %req.method, "unknown method");
                Some(JsonRpcResponse::error(
                    id,
                    -32601,
                    format!("Method not found: {}", req.method),
                ))
            }
        }
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

fn resolve_db_path(cli_path: Option<&str>) -> String {
    if let Some(p) = cli_path {
        return p.to_string();
    }
    std::env::var("CORTEX_DB_PATH").unwrap_or_else(|_| {
        let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
        format!("{home}/.cortex/memory.db")
    })
}

fn ensure_db_dir(db_path: &str) {
    if let Some(parent) = std::path::Path::new(db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }
}

// ── Main ────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    match cli.command {
        Some(Command::Ingest { text, channel }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            let mem = cortex.ingest(&text, &channel, None, None, None).unwrap_or_else(|e| {
                eprintln!("Error ingesting memory: {e}");
                std::process::exit(1);
            });
            println!("✅ Memory stored");
            println!("   ID:      {}", mem.id);
            println!("   Tier:    {}", mem.tier.as_str());
            println!("   Channel: {}", channel);
            if let cortex_core::types::MemContent::Text(ref t) = mem.content {
                let preview = if t.len() > 80 { format!("{}...", &t[..80]) } else { t.clone() };
                println!("   Text:    {}", preview);
            }
        }
        Some(Command::Search { query, limit }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            let results = cortex.retrieve(&query, limit, None, None, None).unwrap_or_else(|e| {
                eprintln!("Error searching: {e}");
                std::process::exit(1);
            });
            if results.is_empty() {
                println!("No results found for: {}", query);
            } else {
                println!("Found {} results for: \"{}\"", results.len(), query);
                println!();
                for (i, r) in results.iter().enumerate() {
                    let content = match &r.memory.content {
                        cortex_core::types::MemContent::Text(t) => {
                            if t.len() > 120 { format!("{}...", &t[..120]) } else { t.clone() }
                        }
                        cortex_core::types::MemContent::Fact { subject, predicate, object } => {
                            format!("{} {} {}", subject, predicate, object)
                        }
                        cortex_core::types::MemContent::Preference { key, value, .. } => {
                            format!("{} = {}", key, value)
                        }
                        other => format!("{:?}", other),
                    };
                    println!("  {}. [score: {:.3}] {}", i + 1, r.score, content);
                    println!("     tier: {} | channel: {} | {}",
                        r.memory.tier.as_str(),
                        r.memory.source.channel,
                        r.memory.temporal.ingestion_time.format("%Y-%m-%d %H:%M"),
                    );
                }
            }
        }
        Some(Command::Sync) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);

            // Detect providers
            let providers = cortex_core::sync::provider::detect_all_providers();
            println!("Cloud Sync Status");
            println!("=================");
            if providers.is_empty() {
                println!("No cloud providers detected.");
                println!("Install iCloud Drive, Google Drive, OneDrive, or Dropbox.");
            } else {
                println!("Detected providers:");
                for p in &providers {
                    let exists = if p.sync_dir.exists() { "✅" } else { "📁" };
                    println!("  {} {} → {}", exists, p.provider.as_str(), p.sync_dir.display());
                }
            }

            // Check if sync is configured for this DB
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            match cortex.sync_status() {
                Some(status) => {
                    println!("\nActive sync:");
                    println!("  Device:  {} ({})", status.device_name, status.device_id);
                    println!("  Dir:     {}", status.sync_dir);
                    println!("  Remotes: {}", status.remote_devices.len());
                }
                None => {
                    println!("\nSync not enabled for this database.");
                }
            }
        }
        Some(Command::Stats) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            let stats = cortex.stats().unwrap_or_else(|e| {
                eprintln!("Error getting stats: {e}");
                std::process::exit(1);
            });

            let db_size = std::fs::metadata(&db_path)
                .map(|m| m.len())
                .unwrap_or(0);

            println!("Cortex Memory Statistics");
            println!("========================");
            println!("Episodic:   {}", stats.episodic);
            println!("Semantic:   {}", stats.semantic);
            println!("Procedural: {}", stats.procedural);
            println!("Archived:   {}", stats.archived);
            println!("Total:      {}", stats.total);
            println!("────────────────────────");
            println!("People:     {}", stats.people);
            println!("Beliefs:    {}", stats.beliefs);
            println!("Index size: {}", stats.index_size);
            println!("DB size:    {}", format_bytes(db_size));
            println!("DB path:    {}", db_path);
        }
        Some(Command::Export { output }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            let data = cortex_core::export::export_all(cortex.storage()).unwrap_or_else(|e| {
                eprintln!("Error exporting: {e}");
                std::process::exit(1);
            });
            let json = serde_json::to_string_pretty(&data).unwrap();
            match output {
                Some(path) => {
                    std::fs::write(&path, &json).unwrap_or_else(|e| {
                        eprintln!("Error writing to {path}: {e}");
                        std::process::exit(1);
                    });
                    eprintln!("Exported to {path}");
                }
                None => println!("{json}"),
            }
        }
        Some(Command::Import { file }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            let json = std::fs::read_to_string(&file).unwrap_or_else(|e| {
                eprintln!("Error reading {file}: {e}");
                std::process::exit(1);
            });
            let data: cortex_core::export::ImportData = serde_json::from_str(&json).unwrap_or_else(|e| {
                eprintln!("Error parsing JSON: {e}");
                std::process::exit(1);
            });
            let report = cortex_core::export::import_all(cortex.storage(), cortex.index(), data)
                .unwrap_or_else(|e| {
                    eprintln!("Error importing: {e}");
                    std::process::exit(1);
                });
            println!("Import complete:");
            println!("  Memories: {}", report.memories);
            println!("  People:   {}", report.people);
            println!("  Beliefs:  {}", report.beliefs);
        }
        Some(Command::Info) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            let has_embeddings = cfg!(feature = "embeddings");
            println!("cortex-mcp-server {}", SERVER_VERSION);
            println!("DB path:    {}", db_path);
            println!("Embeddings: {}", if has_embeddings { "enabled" } else { "disabled (lite)" });
        }
        None => {
            // Default: run MCP server (backward compatible)
            run_mcp_server(cli.db_path.as_deref());
        }
    }
}

fn run_mcp_server(cli_db_path: Option<&str>) {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("cortex_mcp_server=info".parse().unwrap()),
        )
        .with_writer(io::stderr)
        .init();

    let db_path = resolve_db_path(cli_db_path);
    ensure_db_dir(&db_path);

    info!(db = %db_path, "starting cortex-mcp-server");

    let server = match McpServer::new_with_plugins(&db_path) {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "failed to open cortex database");
            std::process::exit(1);
        }
    };

    let stdin = io::stdin();
    let stdout = io::stdout();
    let mut stdout = stdout.lock();

    for line in stdin.lock().lines() {
        let line = match line {
            Ok(l) => l,
            Err(e) => {
                error!(error = %e, "stdin read error");
                break;
            }
        };

        let line = line.trim().to_string();
        if line.is_empty() {
            continue;
        }

        let req: JsonRpcRequest = match serde_json::from_str(&line) {
            Ok(r) => r,
            Err(e) => {
                let resp = JsonRpcResponse::error(Value::Null, -32700, format!("Parse error: {e}"));
                let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
                let _ = stdout.flush();
                continue;
            }
        };

        if let Some(resp) = server.handle_request(&req) {
            let _ = writeln!(stdout, "{}", serde_json::to_string(&resp).unwrap());
            let _ = stdout.flush();
        }
    }
}

fn format_bytes(bytes: u64) -> String {
    if bytes < 1024 {
        format!("{} B", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1} KB", bytes as f64 / 1024.0)
    } else {
        format!("{:.1} MB", bytes as f64 / (1024.0 * 1024.0))
    }
}
