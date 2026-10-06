//! Muse gateway — explicit, budgeted disclosure of a user-chosen slice of memory to a
//! remote MCP client (Meta Muse). Design: `docs/design/muse-gateway.md`.
//!
//! ```text
//! POST /mcp ─ auth ─ origin ─ JSON-RPC ─ tools/call recall_memory
//!                                            │
//!     kill switch? ──yes──▶ isError (audited)│
//!     budget ok?   ──no───▶ isError (audited)│
//!                                            ▼
//!     SELECT … WHERE namespace = 'muse-export'   (only export rows are ever loaded;
//!       → re-check namespace → score → top-k       no shared index, no cache, so a
//!       → redact → 500-char / 4 KiB caps          `revoke` from another process is
//!     → charge budget → audit (no query/text)     honored on the very next call)
//!     → respond
//! ```
//!
//! Everything fails closed: an unreadable budget file or a failed audit write means
//! nothing is disclosed.

use std::collections::BTreeSet;
use std::io::Write as _;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::Router;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use cortex_core::types::{
    MemContent, MemObject, MemObjectBuilder, MemSource, MemoryTier, PrivacyLevel, MUSE_EXPORT_NAMESPACE,
};
use cortex_core::Cortex;

use crate::tools::{content_to_string, redact_emails};
use crate::{JsonRpcRequest, JsonRpcResponse, SERVER_VERSION};

/// The only namespace the gateway ever reads. Membership is the user's explicit consent;
/// core rejects ordinary ingest and sync writes into it.
pub const EXPORT_NS: &str = MUSE_EXPORT_NAMESPACE;
/// Prefix `allow` puts on the content hash — a marker no ingest path can produce.
const EXPORT_HASH_PREFIX: &str = "muse-export:";
const MAX_AUDIT_BYTES: u64 = 10 * 1024 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const MAX_CONCURRENT_REQUESTS: usize = 16;
const MIN_TOKEN_DISTINCT_CHARS: usize = 8;
const MAX_EXPORT: usize = 1000;
const MAX_LIMIT: u64 = 5;
const DEFAULT_LIMIT: u64 = 3;
const MAX_QUERY_CHARS: usize = 500;
const MAX_SNIPPET_CHARS: usize = 500;
const MAX_RESPONSE_BYTES: usize = 4096;
const MAX_BODY_BYTES: usize = 64 * 1024;
const MIN_TOKEN_LEN: usize = 32;
const MIN_COSINE: f32 = 0.2;
const TOOL_NAME: &str = "recall_memory";
const SUPPORTED_PROTOCOLS: &[&str] = &["2025-06-18", "2025-03-26", "2024-11-05"];
const ALL_TIERS: [MemoryTier; 5] = [
    MemoryTier::Working,
    MemoryTier::Episodic,
    MemoryTier::Semantic,
    MemoryTier::Procedural,
    MemoryTier::Archived,
];

#[derive(Subcommand)]
pub enum GatewayAction {
    /// Serve the Muse MCP endpoint (requires CORTEX_GATEWAY_TOKEN)
    Serve {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 3316)]
        port: u16,
        /// Max tool calls per UTC day
        #[arg(long, default_value_t = 100)]
        daily_requests: u32,
        /// Max distinct memories disclosed per UTC day
        #[arg(long, default_value_t = 30)]
        daily_disclosures: u32,
        /// Browser Origin allowed to call the endpoint (requests without Origin are fine)
        #[arg(long)]
        allow_origin: Vec<String>,
    },
    /// Print a fresh random bearer token
    Token,
    /// Export a memory to Muse: new text, or a copy of an existing memory with --from
    Allow {
        text: Option<String>,
        #[arg(long)]
        from: Option<String>,
    },
    /// List what Muse can see
    List,
    /// Remove an exported memory
    Revoke { id: String },
    /// Show exactly what Muse would receive for a query (no budget, no audit)
    Preview {
        query: String,
        #[arg(long, default_value_t = DEFAULT_LIMIT)]
        limit: u64,
    },
    /// Print the disclosure audit log
    Audit,
    /// Kill switch: refuse every Muse request until `on`
    Off,
    /// Re-enable Muse access
    On,
}

// ── State files ──────────────────────────────────────────────────────────────

struct Paths {
    disabled: PathBuf,
    budget: PathBuf,
    audit: PathBuf,
    lock: PathBuf,
}

impl Paths {
    fn for_db(db_path: &str) -> Self {
        let dir = Path::new(db_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self {
            disabled: dir.join("gateway.disabled"),
            budget: dir.join("gateway-state.json"),
            audit: dir.join("gateway-audit.jsonl"),
            lock: dir.join("gateway.lock"),
        }
    }

    /// Kill switch. An I/O error while checking counts as OFF (fail closed).
    fn is_disabled(&self) -> bool {
        self.disabled.try_exists().unwrap_or(true)
    }
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
struct Budget {
    day: String,
    requests: u32,
    disclosed: BTreeSet<String>,
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Missing file = fresh budget. Unreadable or corrupt = error (fail closed, never reset).
fn load_budget(path: &Path, day: &str) -> Result<Budget, String> {
    let b: Budget = match std::fs::read_to_string(path) {
        Ok(s) => serde_json::from_str(&s).map_err(|_| "budget state is corrupt".to_string())?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Budget::default(),
        Err(_) => return Err("budget state is unreadable".into()),
    };
    if b.day == day {
        Ok(b)
    } else {
        Ok(Budget { day: day.to_string(), ..Budget::default() })
    }
}

/// Owner-only (0600 on Unix): budget and audit reveal when and what Muse asked for.
fn private_options() -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600);
    }
    o
}

/// Atomic + durable: write a temp file, fsync, rename. A crash never leaves a torn file.
fn save_budget(path: &Path, b: &Budget) -> Result<(), String> {
    let tmp = path.with_extension(format!("json.{}.tmp", std::process::id()));
    let data = serde_json::to_vec(b).map_err(|e| e.to_string())?;
    let write = || -> std::io::Result<()> {
        let mut f = private_options().write(true).create(true).truncate(true).open(&tmp)?;
        f.write_all(&data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)
    };
    write().map_err(|e| e.to_string())
}

fn append_audit(path: &Path, entry: &Value) -> Result<(), String> {
    // Keep the log bounded: rotate to `.1` (one generation) past MAX_AUDIT_BYTES.
    if std::fs::metadata(path).is_ok_and(|m| m.len() > MAX_AUDIT_BYTES) {
        let _ = std::fs::rename(path, path.with_extension("jsonl.1"));
    }
    let mut f = private_options().create(true).append(true).open(path).map_err(|e| e.to_string())?;
    writeln!(f, "{entry}").map_err(|e| e.to_string())?;
    f.sync_data().map_err(|e| e.to_string())
}

// ── The disclosure gate ──────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
struct Disclosed {
    #[serde(skip)]
    id: Uuid,
    text: String,
    created_at: String,
}

/// Every memory in the export namespace (most recent first, capped at MAX_EXPORT).
fn export_rows(cortex: &Cortex) -> Result<Vec<MemObject>, String> {
    let mut rows = Vec::new();
    for tier in ALL_TIERS {
        let mut part = cortex
            .storage()
            .list_by_tier_and_namespace(tier, Some(EXPORT_NS), MAX_EXPORT)
            .map_err(|e| e.to_string())?;
        rows.append(&mut part);
    }
    // Defense in depth: never trust the query layer alone, and only serve rows that
    // `allow` itself created (Private + hash marker), whatever else reached the namespace.
    rows.retain(is_export_row);
    rows.sort_by(|a, b| b.temporal.ingestion_time.cmp(&a.temporal.ingestion_time));
    rows.truncate(MAX_EXPORT);
    Ok(rows)
}

fn is_export_row(m: &MemObject) -> bool {
    m.namespace.as_deref() == Some(EXPORT_NS)
        && matches!(m.privacy, PrivacyLevel::Private)
        && m.content_hash.as_deref().is_some_and(|h| h.starts_with(EXPORT_HASH_PREFIX))
}

fn tokens(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|t| t.chars().count() >= 2)
        .map(|t| t.to_lowercase())
        .collect()
}

fn cosine(a: &[f32], b: &[f32]) -> f32 {
    let (mut dot, mut na, mut nb) = (0.0f32, 0.0f32, 0.0f32);
    for (x, y) in a.iter().zip(b) {
        dot += x * y;
        na += x * x;
        nb += y * y;
    }
    if na == 0.0 || nb == 0.0 { 0.0 } else { dot / (na.sqrt() * nb.sqrt()) }
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Pure ranking + shaping over already-gated rows.
fn rank(rows: &[MemObject], query: &str, query_emb: Option<&[f32]>, limit: usize) -> Vec<Disclosed> {
    let q_tokens = tokens(query);
    let mut scored: Vec<(f32, &MemObject)> = rows
        .iter()
        .filter(|m| is_export_row(m))
        .filter_map(|m| {
            let text = content_to_string(&m.content);
            let semantic = match (query_emb, m.embedding.as_deref()) {
                (Some(q), Some(e)) if q.len() == e.len() => Some(cosine(q, e)),
                _ => None,
            };
            let keyword = if q_tokens.is_empty() {
                0.0
            } else {
                let t = tokens(&text);
                q_tokens.iter().filter(|w| t.contains(*w)).count() as f32 / q_tokens.len() as f32
            };
            let score = match semantic {
                Some(s) if s >= MIN_COSINE || keyword > 0.0 => s.max(0.0) + keyword,
                Some(_) => return None,
                None if keyword > 0.0 => keyword,
                None => return None,
            };
            Some((score, m))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    let mut out = Vec::new();
    let mut bytes = 2;
    for (_, m) in scored.into_iter().take(limit) {
        let d = Disclosed {
            id: m.id,
            text: truncate_chars(&redact_emails(&content_to_string(&m.content)), MAX_SNIPPET_CHARS),
            created_at: m.temporal.ingestion_time.to_rfc3339(),
        };
        let size = serde_json::to_string(&d).map(|s| s.len() + 1).unwrap_or(usize::MAX);
        if bytes + size + 16 > MAX_RESPONSE_BYTES {
            break;
        }
        bytes += size;
        out.push(d);
    }
    out
}

fn disclose(cortex: &Cortex, query: &str, limit: usize) -> Result<Vec<Disclosed>, String> {
    let rows = export_rows(cortex)?;
    let emb = cortex.embed_query(query);
    Ok(rank(&rows, query, emb.as_deref(), limit))
}

fn results_json(items: &[Disclosed]) -> String {
    json!({ "results": items }).to_string()
}

// ── Tool call with budget + audit ────────────────────────────────────────────

struct Gateway {
    cortex: Cortex,
    paths: Paths,
    daily_requests: u32,
    daily_disclosures: u32,
    /// Serializes budget read-modify-write across concurrent requests.
    lock: Mutex<()>,
}

fn tool_error(msg: &str) -> Value {
    json!({ "content": [{ "type": "text", "text": msg }], "isError": true })
}

fn parse_args(args: &Value) -> Result<(String, usize), String> {
    let query = args.get("query").and_then(Value::as_str).ok_or("`query` (string) is required")?;
    if query.trim().is_empty() {
        return Err("`query` must not be empty".into());
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(format!("`query` must be at most {MAX_QUERY_CHARS} characters"));
    }
    let limit = match args.get("limit") {
        None | Some(Value::Null) => DEFAULT_LIMIT,
        Some(v) => v
            .as_u64()
            .filter(|n| (1..=MAX_LIMIT).contains(n))
            .ok_or(format!("`limit` must be an integer from 1 to {MAX_LIMIT}"))?,
    };
    Ok((query.to_string(), limit as usize))
}

impl Gateway {
    fn audit(&self, n: usize, ids: &[String], bytes: usize, outcome: &str) -> Result<(), String> {
        append_audit(
            &self.paths.audit,
            &json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "method": "tools/call",
                "tool": TOOL_NAME,
                "n_results": n,
                "memory_ids": ids,
                "bytes": bytes,
                "outcome": outcome,
            }),
        )
    }

    fn deny(&self, outcome: &str, msg: &str) -> Value {
        let _ = self.audit(0, &[], 0, outcome);
        tool_error(msg)
    }

    /// Every authenticated call is charged against the request budget *before* anything
    /// else, including calls that are then refused, so refusals can't be used as a free,
    /// unmetered search oracle. Once the distinct-disclosure budget is spent, unseen
    /// matches are silently withheld (indistinguishable from "no match").
    fn call_recall(&self, args: &Value) -> Value {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());

        let mut budget = match load_budget(&self.paths.budget, &today()) {
            Ok(b) => b,
            Err(e) => return self.deny("denied:budget_state", &format!("Refusing to disclose: {e}.")),
        };
        if budget.requests >= self.daily_requests {
            return self.deny("denied:request_budget", "Daily request budget exhausted.");
        }
        budget.requests += 1;
        if save_budget(&self.paths.budget, &budget).is_err() {
            return tool_error("Refusing to disclose: could not record the request.");
        }

        if self.paths.is_disabled() {
            return self.deny("denied:disabled", "Memory access for Muse is turned off by the user.");
        }
        let (query, limit) = match parse_args(args) {
            Ok(v) => v,
            Err(e) => return self.deny("denied:bad_args", &e),
        };
        let mut items = match disclose(&self.cortex, &query, limit) {
            Ok(v) => v,
            Err(_) => return self.deny("error:storage", "Memory lookup failed."),
        };
        let mut remaining = (self.daily_disclosures as usize).saturating_sub(budget.disclosed.len());
        items.retain(|d| {
            let id = d.id.to_string();
            if budget.disclosed.contains(&id) {
                true
            } else if remaining > 0 {
                remaining -= 1;
                true
            } else {
                false
            }
        });

        let ids: Vec<String> = items.iter().map(|d| d.id.to_string()).collect();
        budget.disclosed.extend(ids.iter().cloned());
        let text = results_json(&items);
        if save_budget(&self.paths.budget, &budget).is_err()
            || self.audit(items.len(), &ids, text.len(), "ok").is_err()
        {
            return tool_error("Refusing to disclose: could not record the disclosure.");
        }
        json!({ "content": [{ "type": "text", "text": text }] })
    }

    fn handle(&self, req: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        let id = req.id.clone()?; // notifications get no response
        Some(match req.method.as_str() {
            "initialize" => {
                let requested = req.params.get("protocolVersion").and_then(Value::as_str);
                let version = requested
                    .filter(|v| SUPPORTED_PROTOCOLS.contains(v))
                    .unwrap_or(SUPPORTED_PROTOCOLS[0]);
                JsonRpcResponse::success(
                    id,
                    json!({
                        "protocolVersion": version,
                        "capabilities": { "tools": {} },
                        "serverInfo": { "name": "cortex-muse-gateway", "version": SERVER_VERSION },
                        "instructions": "Personal memory the user explicitly exported for you. \
                            Call recall_memory when the user's own facts, preferences, or history would help."
                    }),
                )
            }
            "ping" => JsonRpcResponse::success(id, json!({})),
            "tools/list" => JsonRpcResponse::success(id, json!({ "tools": [tool_schema()] })),
            "tools/call" => {
                let name = req.params.get("name").and_then(Value::as_str).unwrap_or("");
                if name != TOOL_NAME {
                    JsonRpcResponse::error(id, -32602, format!("Unknown tool: {name}"))
                } else {
                    let args = req.params.get("arguments").cloned().unwrap_or(json!({}));
                    JsonRpcResponse::success(id, self.call_recall(&args))
                }
            }
            other => JsonRpcResponse::error(id, -32601, format!("Method not found: {other}")),
        })
    }
}

fn tool_schema() -> Value {
    json!({
        "name": TOOL_NAME,
        "description": "Search the user's exported personal memory. Returns only what the user explicitly shared with Muse.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "query": { "type": "string", "maxLength": MAX_QUERY_CHARS },
                "limit": { "type": "integer", "minimum": 1, "maximum": MAX_LIMIT, "default": DEFAULT_LIMIT }
            },
            "required": ["query"]
        }
    })
}

// ── HTTP layer ───────────────────────────────────────────────────────────────

struct HttpState {
    gw: Gateway,
    token: String,
    origins: Vec<String>,
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn rpc_error_response(status: StatusCode, code: i64, msg: &str) -> Response {
    let body = serde_json::to_string(&JsonRpcResponse::error(Value::Null, code, msg.into())).unwrap_or_default();
    (status, [("content-type", "application/json")], body).into_response()
}

async fn handle_post(State(st): State<Arc<HttpState>>, headers: HeaderMap, body: Bytes) -> Response {
    let authorized = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .is_some_and(|t| ct_eq(t.as_bytes(), st.token.as_bytes()));
    if !authorized {
        return (StatusCode::UNAUTHORIZED, [("www-authenticate", "Bearer")]).into_response();
    }
    if let Some(origin) = headers.get("origin") {
        let ok = origin.to_str().is_ok_and(|o| st.origins.iter().any(|a| a == o));
        if !ok {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    let value: Value = match serde_json::from_slice(&body) {
        Ok(v) => v,
        Err(_) => return rpc_error_response(StatusCode::BAD_REQUEST, -32700, "Parse error"),
    };
    if value.is_array() {
        return rpc_error_response(StatusCode::BAD_REQUEST, -32600, "Batch requests are not supported");
    }
    let req: JsonRpcRequest = match serde_json::from_value(value) {
        Ok(r) => r,
        Err(_) => return rpc_error_response(StatusCode::BAD_REQUEST, -32600, "Invalid request"),
    };
    let st2 = st.clone();
    let resp = tokio::task::spawn_blocking(move || st2.gw.handle(&req)).await;
    match resp {
        Ok(Some(r)) => (
            StatusCode::OK,
            [("content-type", "application/json")],
            serde_json::to_string(&r).unwrap_or_default(),
        )
            .into_response(),
        Ok(None) => StatusCode::ACCEPTED.into_response(),
        Err(_) => rpc_error_response(StatusCode::INTERNAL_SERVER_ERROR, -32603, "Internal error"),
    }
}

fn router(state: Arc<HttpState>) -> Router {
    Router::new()
        .route(
            "/mcp",
            post(handle_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
        // Bound slow clients (incl. slow bodies) and parallel load. Header-phase slowloris
        // is absorbed by the HTTPS tunnel in front of this loopback listener.
        .layer(tower_http::timeout::TimeoutLayer::with_status_code(
            StatusCode::REQUEST_TIMEOUT,
            std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS),
        ))
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS))
        .with_state(state)
}

// ── CLI actions ──────────────────────────────────────────────────────────────

fn die(msg: &str) -> ! {
    eprintln!("error: {msg}");
    std::process::exit(1);
}

fn open(db_path: &str) -> Cortex {
    Cortex::open(db_path).unwrap_or_else(|e| die(&format!("failed to open database: {e}")))
}

fn allow(cortex: &Cortex, text: &str) -> Result<Uuid, String> {
    let rows = export_rows(cortex)?;
    if let Some(existing) = rows.iter().find(|m| content_to_string(&m.content) == text) {
        return Ok(existing.id);
    }
    if rows.len() >= MAX_EXPORT {
        return Err(format!("the Muse export already holds {MAX_EXPORT} memories; revoke some first"));
    }
    let source = MemSource {
        channel: "muse-export".into(),
        identity_id: None,
        chat_id: None,
        thread_id: None,
        message_id: None,
    };
    let mut builder = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text(text.to_string()), source)
        .privacy(PrivacyLevel::Private)
        .namespace(EXPORT_NS);
    if let Some(e) = cortex.embed_query(text) {
        builder = builder.embedding(e);
    }
    let mut mem = builder.build();
    // Export copies are deliberate duplicates of existing memories: keep their hash out of
    // the global exact-dedup space so copying never collides with (or hides) the original.
    mem.content_hash = mem.content_hash.map(|h| format!("{EXPORT_NS}:{h}"));
    cortex.storage().store_memory(&mem).map_err(|e| e.to_string())?;
    Ok(mem.id)
}

pub fn run(action: GatewayAction, db_path: &str) {
    let paths = Paths::for_db(db_path);
    match action {
        GatewayAction::Token => {
            use rand::RngCore;
            let mut b = [0u8; 32];
            rand::thread_rng().fill_bytes(&mut b);
            println!("{}", b.iter().map(|x| format!("{x:02x}")).collect::<String>());
        }
        GatewayAction::Allow { text, from } => {
            let cortex = open(db_path);
            let text = match (text, from) {
                (Some(t), None) => t,
                (None, Some(id)) => {
                    let id = Uuid::parse_str(&id).unwrap_or_else(|_| die("invalid memory id"));
                    let mem = cortex
                        .storage()
                        .get_memory(id)
                        .ok()
                        .flatten()
                        .unwrap_or_else(|| die("no memory with that id"));
                    content_to_string(&mem.content)
                }
                _ => die("give either TEXT or --from <ID>"),
            };
            if text.trim().is_empty() {
                die("text must not be empty");
            }
            match allow(&cortex, &text) {
                Ok(id) => println!("{id}"),
                Err(e) => die(&e),
            }
        }
        GatewayAction::List => {
            let cortex = open(db_path);
            for m in export_rows(&cortex).unwrap_or_else(|e| die(&e)) {
                println!("{}\t{}", m.id, content_to_string(&m.content).replace(['\n', '\t'], " "));
            }
        }
        GatewayAction::Revoke { id } => {
            let cortex = open(db_path);
            let id = Uuid::parse_str(&id).unwrap_or_else(|_| die("invalid memory id"));
            match cortex.storage().get_memory(id) {
                Ok(Some(m)) if m.namespace.as_deref() == Some(EXPORT_NS) => {
                    cortex.delete_memory(id).unwrap_or_else(|e| die(&e.to_string()));
                    println!("revoked {id}");
                }
                _ => die("that id is not in the Muse export"),
            }
        }
        GatewayAction::Preview { query, limit } => {
            let cortex = open(db_path);
            let args = json!({ "query": query, "limit": limit });
            let (q, l) = parse_args(&args).unwrap_or_else(|e| die(&e));
            let items = disclose(&cortex, &q, l).unwrap_or_else(|e| die(&e));
            println!("{}", results_json(&items));
        }
        GatewayAction::Audit => match std::fs::read_to_string(&paths.audit) {
            Ok(s) => print!("{s}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => die(&e.to_string()),
        },
        GatewayAction::Off => {
            std::fs::write(&paths.disabled, b"off\n").unwrap_or_else(|e| die(&e.to_string()));
            println!("Muse access is OFF");
        }
        GatewayAction::On => {
            match std::fs::remove_file(&paths.disabled) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => die(&e.to_string()),
            }
            println!("Muse access is ON");
        }
        GatewayAction::Serve { host, port, daily_requests, daily_disclosures, allow_origin } => {
            let token = std::env::var("CORTEX_GATEWAY_TOKEN").unwrap_or_default();
            let distinct = token.chars().collect::<BTreeSet<_>>().len();
            if token.len() < MIN_TOKEN_LEN || distinct < MIN_TOKEN_DISTINCT_CHARS {
                die("CORTEX_GATEWAY_TOKEN must be a random string of at least 32 characters (try `gateway token`)");
            }
            // One server per state dir: two would race on the budget file.
            let lock = private_options()
                .create(true)
                .truncate(false)
                .write(true)
                .open(&paths.lock)
                .unwrap_or_else(|e| die(&e.to_string()));
            if lock.try_lock().is_err() {
                die("another `gateway serve` is already running for this database");
            }
            let cortex = open(db_path);
            let _ = cortex.embed_query("warmup"); // load the model before the first request
            match export_rows(&cortex) {
                Ok(rows) if rows.len() >= MAX_EXPORT => {
                    eprintln!("warning: export holds {MAX_EXPORT}+ memories; only the newest {MAX_EXPORT} are searchable")
                }
                Ok(_) => {}
                Err(e) => die(&e),
            }
            let state = Arc::new(HttpState {
                gw: Gateway { cortex, paths, daily_requests, daily_disclosures, lock: Mutex::new(()) },
                token,
                origins: allow_origin,
            });
            let rt = tokio::runtime::Runtime::new().unwrap_or_else(|e| die(&e.to_string()));
            rt.block_on(async move {
                let listener = tokio::net::TcpListener::bind((host.as_str(), port))
                    .await
                    .unwrap_or_else(|e| die(&format!("bind failed: {e}")));
                let addr: SocketAddr = listener.local_addr().unwrap_or_else(|e| die(&e.to_string()));
                if !addr.ip().is_loopback() {
                    eprintln!("WARNING: bound to non-loopback {addr}; expose it only through an HTTPS tunnel");
                }
                eprintln!("cortex gateway listening on http://{addr}/mcp");
                axum::serve(listener, router(state)).await.unwrap_or_else(|e| die(&e.to_string()));
            });
            drop(lock);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mem(text: &str, ns: Option<&str>, privacy: PrivacyLevel) -> MemObject {
        let source = MemSource { channel: "t".into(), identity_id: None, chat_id: None, thread_id: None, message_id: None };
        let mut b = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text(text.into()), source).privacy(privacy);
        if let Some(ns) = ns {
            b = b.namespace(ns);
        }
        let mut m = b.build();
        if ns == Some(EXPORT_NS) {
            m.content_hash = m.content_hash.map(|h| format!("{EXPORT_HASH_PREFIX}{h}"));
        }
        m
    }

    #[test]
    fn rank_rejects_rows_in_export_namespace_not_created_by_allow() {
        // Reached the namespace some other way (no hash marker), or not Private.
        let mut forged = mem("zebra forged", None, PrivacyLevel::Private);
        forged.namespace = Some(EXPORT_NS.into());
        let mut public = mem("zebra public export", Some(EXPORT_NS), PrivacyLevel::Private);
        public.privacy = PrivacyLevel::Public;
        assert!(rank(&[forged, public], "zebra", None, 5).is_empty());
    }

    #[test]
    fn rank_never_returns_rows_outside_export_namespace() {
        let rows = vec![
            mem("zebra private", None, PrivacyLevel::Private),
            mem("zebra public", None, PrivacyLevel::Public),
            mem("zebra shared", Some("work"), PrivacyLevel::Shared { scope: "all".into() }),
            mem("zebra exported", Some(EXPORT_NS), PrivacyLevel::Private),
        ];
        let out = rank(&rows, "zebra", None, 5);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].text, "zebra exported");
    }

    #[test]
    fn rank_redacts_and_caps_snippets() {
        let long = format!("zebra a@b.com {}", "x".repeat(2000));
        let out = rank(&[mem(&long, Some(EXPORT_NS), PrivacyLevel::Private)], "zebra", None, 5);
        assert!(out[0].text.contains("[redacted-email]") && !out[0].text.contains("a@b.com"));
        assert_eq!(out[0].text.chars().count(), MAX_SNIPPET_CHARS);
    }

    #[test]
    fn rank_caps_total_response_size() {
        let rows: Vec<_> = (0..5)
            .map(|i| mem(&format!("zebra {i} {}", "y".repeat(1500)), Some(EXPORT_NS), PrivacyLevel::Private))
            .collect();
        let out = rank(&rows, "zebra", None, 5);
        assert!(results_json(&out).len() <= MAX_RESPONSE_BYTES);
        assert!(!out.is_empty());
    }

    #[test]
    fn rank_ignores_unrelated_rows() {
        let out = rank(&[mem("apples", Some(EXPORT_NS), PrivacyLevel::Private)], "zebra", None, 5);
        assert!(out.is_empty());
    }

    #[test]
    fn parse_args_bounds() {
        assert!(parse_args(&json!({})).is_err());
        assert!(parse_args(&json!({"query": " "})).is_err());
        assert!(parse_args(&json!({"query": "x".repeat(501)})).is_err());
        assert!(parse_args(&json!({"query": "x", "limit": 0})).is_err());
        assert!(parse_args(&json!({"query": "x", "limit": 6})).is_err());
        assert_eq!(parse_args(&json!({"query": "x"})).unwrap().1, DEFAULT_LIMIT as usize);
    }

    #[test]
    fn budget_resets_on_new_day_but_fails_closed_on_corruption() {
        let dir = std::env::temp_dir().join(format!("gw-budget-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("b.json");
        assert_eq!(load_budget(&p, "2026-01-01").unwrap().requests, 0);
        let b = Budget { day: "2026-01-01".into(), requests: 7, disclosed: ["a".to_string()].into() };
        save_budget(&p, &b).unwrap();
        assert_eq!(load_budget(&p, "2026-01-01").unwrap(), b);
        assert_eq!(load_budget(&p, "2026-01-02").unwrap().requests, 0);
        std::fs::write(&p, "{not json").unwrap();
        assert!(load_budget(&p, "2026-01-01").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }
}
