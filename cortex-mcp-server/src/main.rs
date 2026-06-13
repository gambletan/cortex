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

mod capabilities;
mod tools;

use capabilities::CapabilityPolicy;

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
        /// Privacy level: private (never leaves this device), shared, public
        #[arg(short, long, default_value = "private")]
        privacy: String,
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
    Sync {
        #[command(subcommand)]
        action: Option<SyncAction>,
    },
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
    /// Show weekly memory digest
    Digest {
        /// Number of days to include (default: 7)
        #[arg(short, long, default_value_t = 7)]
        days: u64,
    },
}

#[derive(Subcommand)]
enum SyncAction {
    /// Enable cloud sync (auto-detects provider, generates passphrase)
    Enable {
        /// Cloud provider: icloud, gdrive, onedrive, dropbox (auto-detect if omitted)
        #[arg(short, long)]
        provider: Option<String>,
        /// Encryption passphrase (generated if omitted)
        #[arg(long)]
        passphrase: Option<String>,
        /// Device name (defaults to hostname)
        #[arg(short, long)]
        name: Option<String>,
    },
    /// Pull remote changes from other devices
    Pull,
}

// ── Server ──────────────────────────────────────────────────────────────────

struct McpServer {
    cortex: Arc<Cortex>,
    /// Capability policy gating which tools this instance lists and executes.
    policy: CapabilityPolicy,
}

impl McpServer {
    #[allow(dead_code)]
    fn new(db_path: &str) -> Result<Self, cortex_core::CortexError> {
        let cortex = Cortex::open(db_path)?;
        Ok(Self {
            cortex: Arc::new(cortex),
            policy: CapabilityPolicy::load(db_path),
        })
    }

    fn new_with_plugins(db_path: &str) -> Result<Self, cortex_core::CortexError> {
        use cortex_core::plugins::tag_classifier::TagClassifierPlugin;

        let cortex = Cortex::open(db_path)?
            .with_plugin(Box::new(TagClassifierPlugin::new()));
        Ok(Self {
            cortex: Arc::new(cortex),
            policy: CapabilityPolicy::load(db_path),
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
            // Filtered by the capability policy: ungranted tools are invisible,
            // not just denied (smaller attack surface and token cost).
            "tools/list" => {
                let all = tools::list_tools_with_plugins(&self.cortex);
                let granted: Vec<Value> = all
                    .as_array()
                    .map(|tools| {
                        tools
                            .iter()
                            .filter(|t| {
                                t.get("name")
                                    .and_then(|n| n.as_str())
                                    .is_some_and(|n| self.policy.is_allowed(n))
                            })
                            .cloned()
                            .collect()
                    })
                    .unwrap_or_default();
                Some(JsonRpcResponse::success(id, json!({ "tools": granted })))
            }

            // ── Tool execution ──────────────────────────────────────────
            "tools/call" => {
                let tool_name = req.params.get("name").and_then(|v| v.as_str()).unwrap_or("");
                let args = req.params.get("arguments").cloned().unwrap_or(json!({}));

                if !self.policy.is_allowed(tool_name) {
                    return Some(JsonRpcResponse::success(
                        id,
                        json!({
                            "content": [{
                                "type": "text",
                                "text": format!(
                                    "Error: tool '{tool_name}' not permitted by capability policy"
                                )
                            }],
                            "isError": true
                        }),
                    ));
                }

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

/// Show a one-time star prompt on first interactive CLI use.
fn maybe_show_star_prompt() {
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    let marker = format!("{home}/.cortex/.star-prompted");
    if std::path::Path::new(&marker).exists() {
        return;
    }
    // Ensure directory exists
    let _ = std::fs::create_dir_all(format!("{home}/.cortex"));
    eprintln!();
    eprintln!("  ⭐ Enjoying Cortex? Star us on GitHub — it helps others find the project:");
    eprintln!("     https://github.com/gambletan/cortex");
    eprintln!();
    // Write marker so we only show this once
    let _ = std::fs::write(&marker, "prompted");
}

// ── Main ────────────────────────────────────────────────────────────────────

fn main() {
    let cli = Cli::parse();

    // Show star prompt for interactive CLI commands (not MCP server)
    if cli.command.is_some() {
        maybe_show_star_prompt();
    }

    match cli.command {
        Some(Command::Ingest { text, channel, privacy }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let privacy_level = match privacy.to_lowercase().as_str() {
                "private" => cortex_core::types::PrivacyLevel::Private,
                "shared" => cortex_core::types::PrivacyLevel::Shared { scope: "all".into() },
                "public" => cortex_core::types::PrivacyLevel::Public,
                other => {
                    eprintln!("Invalid privacy '{other}': use private, shared, or public");
                    std::process::exit(1);
                }
            };
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            // Resume sync so Shared ingests are recorded to the oplog.
            let _ = cortex.resume_sync();
            let mem = cortex
                .ingest_with_options(&text, &channel, None, None, None, None, Some(privacy_level))
                .unwrap_or_else(|e| {
                    eprintln!("Error ingesting memory: {e}");
                    std::process::exit(1);
                });
            println!("✅ Memory stored");
            println!("   ID:      {}", mem.id);
            println!("   Tier:    {}", mem.tier.as_str());
            println!("   Channel: {}", channel);
            println!("   Privacy: {}", privacy.to_lowercase());
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
        Some(Command::Sync { action }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });
            // Resume sync from persisted settings so status/pull work across restarts.
            let _ = cortex.resume_sync();

            match action {
                Some(SyncAction::Enable { provider, passphrase, name }) => {
                    use cortex_core::sync::{SyncConfig, provider::{detect_all_providers, CloudProvider}};

                    let providers = detect_all_providers();
                    let detected = if let Some(ref pname) = provider {
                        let target = match pname.to_lowercase().as_str() {
                            "icloud" => CloudProvider::ICloud,
                            "gdrive" | "googledrive" | "google_drive" => CloudProvider::GoogleDrive,
                            "onedrive" => CloudProvider::OneDrive,
                            "dropbox" => CloudProvider::Dropbox,
                            _ => {
                                eprintln!("Unknown provider: {pname}. Use: icloud, gdrive, onedrive, dropbox");
                                std::process::exit(1);
                            }
                        };
                        providers.into_iter()
                            .find(|p| std::mem::discriminant(&p.provider) == std::mem::discriminant(&target))
                            .unwrap_or_else(|| {
                                eprintln!("{} not found on this machine", target.as_str());
                                std::process::exit(1);
                            })
                    } else {
                        providers.into_iter().next().unwrap_or_else(|| {
                            eprintln!("No cloud providers detected. Install iCloud Drive, Google Drive, OneDrive, or Dropbox.");
                            std::process::exit(1);
                        })
                    };

                    let device_name = name.unwrap_or_else(|| {
                        hostname::get()
                            .ok()
                            .and_then(|h| h.into_string().ok())
                            .unwrap_or_else(|| "unknown-device".into())
                    });
                    let device_id = uuid::Uuid::new_v4().to_string();

                    let pass = passphrase.unwrap_or_else(|| {
                        use std::fmt::Write;
                        let bytes: [u8; 24] = std::array::from_fn(|_| rand::random());
                        let mut s = String::with_capacity(48);
                        for b in &bytes {
                            let _ = write!(s, "{:02x}", b);
                        }
                        s
                    });

                    let config = SyncConfig::new(detected.sync_dir.clone(), device_id.clone(), device_name.clone())
                        .with_encryption(&pass);

                    cortex.enable_sync(config).unwrap_or_else(|e| {
                        eprintln!("Error enabling sync: {e}");
                        std::process::exit(1);
                    });

                    let pulled = cortex.sync_pull().unwrap_or(0);

                    println!("Sync enabled!");
                    println!("  Provider:   {}", detected.provider.as_str());
                    println!("  Sync dir:   {}", detected.sync_dir.display());
                    println!("  Device:     {} ({})", device_name, device_id);
                    println!("  Encryption: AES-256-GCM");
                    println!("  Passphrase: {}", pass);
                    println!("  Pulled:     {} remote changes", pulled);
                    println!();
                    println!("IMPORTANT: Save the passphrase above!");
                    println!("You'll need it when setting up sync on another device.");
                }
                Some(SyncAction::Pull) => {
                    let applied = cortex.sync_pull().unwrap_or_else(|e| {
                        eprintln!("Error pulling: {e}");
                        std::process::exit(1);
                    });
                    if applied > 0 {
                        println!("Applied {} remote changes.", applied);
                    } else {
                        println!("Already up to date.");
                    }
                }
                None => {
                    // Default: show status
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
                    match cortex.sync_status() {
                        Some(status) => {
                            println!("\nActive sync:");
                            println!("  Device:  {} ({})", status.device_name, status.device_id);
                            println!("  Dir:     {}", status.sync_dir);
                            println!("  Remotes: {}", status.remote_devices.len());
                        }
                        None => {
                            println!("\nSync not enabled. Run: cortex-mcp-server sync enable");
                        }
                    }
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
        Some(Command::Digest { days }) => {
            let db_path = resolve_db_path(cli.db_path.as_deref());
            ensure_db_dir(&db_path);
            let cortex = Cortex::open(&db_path).unwrap_or_else(|e| {
                eprintln!("Error opening database: {e}");
                std::process::exit(1);
            });

            let stats = cortex.stats().unwrap_or_else(|e| {
                eprintln!("Error: {e}");
                std::process::exit(1);
            });

            let now = chrono::Utc::now();
            let cutoff = now - chrono::Duration::days(days as i64);

            // Two-stage window so the digest scales with the requested period, not total
            // DB size, while still using event-time semantics:
            //   1. Bound the SQL scan by created_at (the only indexed time column) to rows
            //      inserted in [cutoff, now]. created_at >= ingestion_time for live and
            //      imported/synced memories, so this is a small superset of the window.
            //   2. Filter by temporal.ingestion_time (the user-visible event time) so
            //      old history that was *inserted* recently (e.g. a sync onto a new device)
            //      but *happened* long ago is excluded from a "last N days" digest.
            let episodes = cortex.storage()
                .list_in_time_range(cortex_core::types::MemoryTier::Episodic, cutoff, now)
                .unwrap_or_default();
            let recent: Vec<_> = episodes.iter()
                .filter(|m| m.temporal.ingestion_time >= cutoff)
                .collect();

            let semantics = cortex.storage()
                .list_in_time_range(cortex_core::types::MemoryTier::Semantic, cutoff, now)
                .unwrap_or_default();
            let recent_facts: Vec<_> = semantics.iter()
                .filter(|m| m.temporal.ingestion_time >= cutoff)
                .collect();

            // Output Markdown
            println!("# Cortex Memory Digest");
            println!("**Period**: {} days (since {})", days, cutoff.format("%Y-%m-%d"));
            println!("**Total memories**: {} | **People**: {} | **Beliefs**: {}", stats.total, stats.people, stats.beliefs);
            println!();

            if !recent.is_empty() {
                println!("## Recent Memories ({} episodes)", recent.len());
                println!();
                // Group by date
                let mut by_date: std::collections::BTreeMap<String, Vec<&cortex_core::types::MemObject>> = std::collections::BTreeMap::new();
                for mem in &recent {
                    let date = mem.temporal.ingestion_time.format("%Y-%m-%d").to_string();
                    by_date.entry(date).or_default().push(mem);
                }
                for (date, mems) in by_date.iter().rev() {
                    println!("### {}", date);
                    println!();
                    for mem in mems {
                        let time = mem.temporal.ingestion_time.format("%H:%M");
                        let text = match &mem.content {
                            cortex_core::types::MemContent::Text(t) => t.clone(),
                            cortex_core::types::MemContent::Fact { subject, predicate, object } => {
                                format!("{} {} {}", subject, predicate, object)
                            }
                            cortex_core::types::MemContent::Preference { key, value, .. } => {
                                format!("{}: {}", key, value)
                            }
                            _ => "[complex content]".to_string(),
                        };
                        let channel = &mem.source.channel;
                        println!("- {} | {} `[{}]`", time, text, channel);
                    }
                    println!();
                }
            } else {
                println!("## No recent memories in the last {} days", days);
                println!();
            }

            if !recent_facts.is_empty() {
                println!("## New Knowledge ({} facts)", recent_facts.len());
                println!();
                for mem in &recent_facts {
                    let text = match &mem.content {
                        cortex_core::types::MemContent::Fact { subject, predicate, object } => {
                            format!("{} → {} → {}", subject, predicate, object)
                        }
                        cortex_core::types::MemContent::Preference { key, value, .. } => {
                            format!("{}: {}", key, value)
                        }
                        cortex_core::types::MemContent::Text(t) => t.clone(),
                        _ => "[complex]".to_string(),
                    };
                    println!("- {}", text);
                }
                println!();
            }

            println!("---");
            println!("*Generated by Cortex v{}*", env!("CARGO_PKG_VERSION"));
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
    // Honor RUST_LOG when set (so users can silence logs, e.g. RUST_LOG=off in CI/
    // pipelines); default to info only when it is unset. The previous code appended a
    // hardcoded `cortex_mcp_server=info` directive that, being more specific, overrode
    // RUST_LOG entirely — INFO/WARN leaked no matter what the user set.
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("cortex_mcp_server=info"));
    tracing_subscriber::fmt()
        .with_env_filter(env_filter)
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

    // Resume cloud sync from persisted settings (no-op if never enabled), then
    // start background sync (poll + fs-watcher) so remote changes from other
    // devices arrive without manual sync_pull calls.
    match server.cortex.resume_sync() {
        Ok(true) => {
            info!("cloud sync active (resumed from persisted settings)");
            match server.cortex.start_background_sync() {
                Ok(()) => info!("background sync started"),
                Err(e) => error!(error = %e, "failed to start background sync"),
            }
        }
        Ok(false) => {}
        Err(e) => error!(error = %e, "failed to resume sync — continuing without sync"),
    }

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
