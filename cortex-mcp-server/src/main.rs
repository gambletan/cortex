//! Cortex MCP Server — gives LLMs persistent memory via Model Context Protocol.
//!
//! Communicates over stdio using JSON-RPC 2.0 (MCP transport).
//! Tools: memory_ingest, memory_search, memory_context, belief_observe, belief_list,
//!        person_resolve, fact_add, preference_set

use std::io::{self, BufRead, Write};
use std::sync::Arc;

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

const SERVER_NAME: &str = "cortex-memory";
const SERVER_VERSION: &str = env!("CARGO_PKG_VERSION");
const PROTOCOL_VERSION: &str = "2024-11-05";

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
                        "name": SERVER_NAME,
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

// ── Main ────────────────────────────────────────────────────────────────────

fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("cortex_mcp_server=info".parse().unwrap()),
        )
        .with_writer(io::stderr)
        .init();

    let db_path = std::env::args()
        .nth(1)
        .or_else(|| std::env::var("CORTEX_DB_PATH").ok())
        .unwrap_or_else(|| {
            let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
            format!("{home}/.cortex/memory.db")
        });

    // Ensure parent directory exists
    if let Some(parent) = std::path::Path::new(&db_path).parent() {
        let _ = std::fs::create_dir_all(parent);
    }

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
