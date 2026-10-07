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
use axum::routing::{get, post};
use axum::Router;
use clap::Subcommand;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use cortex_core::types::{
    MemContent, MemObject, MemObjectBuilder, MemSource, MemoryTier, PrivacyLevel, MUSE_EXPORT_NAMESPACE,
};
use cortex_core::storage::memory_index::cosine_similarity;
use cortex_core::Cortex;

use crate::tools::{content_to_string, redact_emails};
use crate::{JsonRpcRequest, JsonRpcResponse, SERVER_VERSION};

pub mod cloud;
pub mod muse_tools;
pub mod oauth;

/// The only namespace the gateway ever reads. Membership is the user's explicit consent;
/// core rejects ordinary ingest and sync writes into it.
pub const EXPORT_NS: &str = MUSE_EXPORT_NAMESPACE;
/// Most memories one export may hold.
pub const MAX_EXPORT_ITEMS: usize = MAX_EXPORT;
/// Prefix `allow` puts on the content hash — a marker no ingest path can produce.
const EXPORT_HASH_PREFIX: &str = "muse-export:";
const MAX_AUDIT_BYTES: u64 = 10 * 1024 * 1024;
const REQUEST_TIMEOUT_SECS: u64 = 10;
const MAX_CONCURRENT_REQUESTS: usize = 16;
/// OAuth endpoints are unauthenticated by nature: they get their own, smaller lane so a
/// flood (or trickled bodies) there can never take capacity from `/mcp`.
const MAX_OAUTH_CONCURRENT: usize = 8;
const MAX_OAUTH_WORKERS: usize = 4;
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
const REMEMBER_TOOL: &str = "remember";
const MAX_REMEMBER_CHARS: usize = 1000;
const MAX_INBOX: usize = 200;
const REMEMBER_ACK: &str = "Saved for the user's review.";
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
    /// Serve the Muse MCP endpoint (CORTEX_GATEWAY_TOKEN and/or --oauth)
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
        /// Offer the `remember` tool: Muse can add items to a review inbox (append-only)
        #[arg(long)]
        enable_remember: bool,
        /// Max `remember` items accepted per UTC day
        #[arg(long, default_value_t = 20)]
        daily_remembers: u32,
        /// Let clients sign in with OAuth (what Muse uses); each sign-in needs `gateway connect`
        #[arg(long, requires = "public_url")]
        oauth: bool,
        /// The public https URL of this gateway (your tunnel), e.g. https://me.tail1234.ts.net
        #[arg(long, requires = "oauth")]
        public_url: Option<String>,
        /// Extra OAuth redirect URI to allow (Muse's callback is always allowed)
        #[arg(long, requires = "oauth")]
        oauth_redirect: Vec<String>,
    },
    /// Approve (or refuse) an OAuth sign-in: the code shown on the sign-in page
    Connect { code: String },
    /// List clients signed in with OAuth
    Clients,
    /// Sign out an OAuth client (or all of them with --all)
    Disconnect {
        id: Option<String>,
        #[arg(long)]
        all: bool,
    },
    /// List items Muse asked to remember, pending your review
    Inbox,
    /// Approve an inbox item: becomes a Private memory and is exported to Muse
    Approve { id: String },
    /// Discard an inbox item (or all of them with --all)
    Reject {
        id: Option<String>,
        #[arg(long)]
        all: bool,
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

/// Every state file of one gateway (self-hosted: next to the DB; cloud: one tenant dir).
#[derive(Clone)]
pub struct Paths {
    disabled: PathBuf,
    budget: PathBuf,
    audit: PathBuf,
    lock: PathBuf,
    inbox: PathBuf,
    inbox_lock: PathBuf,
    oauth: PathBuf,
    oauth_lock: PathBuf,
    /// Cloud: inbox text is encrypted at rest with this tenant key (AES-256-GCM).
    inbox_key: Option<[u8; 32]>,
}

impl Paths {
    fn for_db(db_path: &str) -> Self {
        let dir = Path::new(db_path)
            .parent()
            .filter(|p| !p.as_os_str().is_empty())
            .unwrap_or(Path::new("."))
            .to_path_buf();
        Self::for_dir(&dir, None)
    }

    /// State files inside `dir` (a Cortex Cloud tenant directory).
    pub fn for_dir(dir: &Path, inbox_key: Option<[u8; 32]>) -> Self {
        let dir = dir.to_path_buf();
        Self {
            disabled: dir.join("gateway.disabled"),
            budget: dir.join("gateway-state.json"),
            audit: dir.join("gateway-audit.jsonl"),
            lock: dir.join("gateway.lock"),
            inbox: dir.join("gateway-inbox.jsonl"),
            inbox_lock: dir.join("gateway-inbox.lock"),
            oauth: dir.join("gateway-oauth.json"),
            oauth_lock: dir.join("gateway-oauth.lock"),
            inbox_key,
        }
    }

    /// Kill switch. An I/O error while checking counts as OFF (fail closed).
    fn is_disabled(&self) -> bool {
        // Anything at the path (file, dir, even a dangling link) means OFF.
        match std::fs::symlink_metadata(&self.disabled) {
            Ok(_) => true,
            Err(e) => e.kind() != std::io::ErrorKind::NotFound,
        }
    }
}

#[derive(Serialize, Deserialize, Default, Debug, PartialEq)]
struct Budget {
    day: String,
    requests: u32,
    disclosed: BTreeSet<String>,
    /// Over-budget refusals are audited once per day, not per call: they are unmetered,
    /// and auditing each one would let a caller rotate real disclosure records away.
    #[serde(default)]
    over_budget_logged: bool,
    /// `remember` calls stored today.
    #[serde(default)]
    remembers: u32,
}

fn today() -> String {
    chrono::Utc::now().format("%Y-%m-%d").to_string()
}

/// Missing file = fresh budget. Unreadable or corrupt = error (fail closed, never reset).
fn load_budget(path: &Path, day: &str) -> Result<Budget, String> {
    let b: Budget = match read_private(path) {
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
///
/// Never follows a symlink: the state files sit next to the DB, and a planted link must not
/// redirect budget/audit/inbox reads or writes elsewhere.
fn private_options() -> std::fs::OpenOptions {
    let mut o = std::fs::OpenOptions::new();
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        o.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    o
}

/// Read a state file without following symlinks; NotFound passes through.
fn read_private(path: &Path) -> std::io::Result<String> {
    use std::io::Read;
    let mut f = private_options().read(true).open(path)?;
    if !f.metadata()?.is_file() {
        return Err(std::io::Error::other("not a regular file"));
    }
    let mut s = String::new();
    f.read_to_string(&mut s)?;
    Ok(s)
}

/// fsync the directory holding `path`, so a rename/create survives power loss.
fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
        std::fs::File::open(dir)?.sync_all()?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// A fresh, unpredictable temp path next to `path`, created exclusively (no clobbering of a
/// pre-planted file or link).
fn create_temp_beside(path: &Path) -> std::io::Result<(PathBuf, std::fs::File)> {
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    let f = private_options().write(true).create_new(true).open(&tmp)?;
    Ok((tmp, f))
}

/// Atomic + durable: write a temp file, fsync, rename. A crash never leaves a torn file.
fn save_budget(path: &Path, b: &Budget) -> Result<(), String> {
    let data = serde_json::to_vec(b).map_err(|e| e.to_string())?;
    let write = || -> std::io::Result<()> {
        let (tmp, mut f) = create_temp_beside(path)?;
        f.write_all(&data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        sync_parent(path)
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
pub fn export_rows(cortex: &Cortex) -> Result<Vec<MemObject>, String> {
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
    rows.sort_by_key(|m| std::cmp::Reverse(m.temporal.ingestion_time));
    rows.truncate(MAX_EXPORT);
    Ok(rows)
}

fn is_export_row(m: &MemObject) -> bool {
    m.namespace.as_deref() == Some(EXPORT_NS)
        && matches!(m.privacy, PrivacyLevel::Private)
        && m.content_hash.as_deref().is_some_and(|h| h.starts_with(EXPORT_HASH_PREFIX))
}

/// Han / Kana / Hangul: scripts written without spaces between words.
fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3040..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xAC00..=0xD7AF | 0xF900..=0xFAFF | 0x20000..=0x2FFFF)
}

/// Lowercased word tokens (2+ chars). CJK runs have no word boundaries, so they are
/// split into character bigrams (a lone CJK char is kept as-is).
/// Function words that would otherwise make any question match any memory
/// ("what is my name" must not disclose "my coffee is black").
const STOPWORDS: &[&str] = &[
    "a", "about", "am", "an", "and", "are", "as", "at", "be", "by", "can", "do", "does", "for",
    "from", "have", "how", "i", "in", "is", "it", "me", "my", "of", "on", "or", "our", "so",
    "that", "the", "their", "this", "to", "was", "we", "what", "when", "where", "which", "who",
    "why", "will", "with", "you", "your", "的", "了", "是", "我", "你", "吗", "什么",
];

/// Query terms that carry meaning: tokens minus stopwords.
fn query_terms(query: &str) -> BTreeSet<String> {
    tokens(query).into_iter().filter(|t| !STOPWORDS.contains(&t.as_str())).collect()
}

fn tokens(s: &str) -> BTreeSet<String> {
    let mut out = BTreeSet::new();
    for seg in s.split(|c: char| !c.is_alphanumeric()).filter(|t| !t.is_empty()) {
        let chars: Vec<char> = seg.chars().collect();
        let mut i = 0;
        while i < chars.len() {
            let cjk = is_cjk(chars[i]);
            let start = i;
            while i < chars.len() && is_cjk(chars[i]) == cjk {
                i += 1;
            }
            let run = &chars[start..i];
            if cjk && run.len() > 1 {
                out.extend(run.windows(2).map(|w| w.iter().collect::<String>()));
            } else if cjk || run.len() >= 2 {
                out.insert(run.iter().collect::<String>().to_lowercase());
            }
        }
    }
    out
}

fn truncate_chars(s: &str, max: usize) -> String {
    match s.char_indices().nth(max) {
        Some((i, _)) => s[..i].to_string(),
        None => s.to_string(),
    }
}

/// Pure ranking + shaping over already-gated rows.
fn rank(rows: &[MemObject], query: &str, query_emb: Option<&[f32]>, limit: usize) -> Vec<Disclosed> {
    let q_tokens = query_terms(query);
    let mut scored: Vec<(f32, &MemObject)> = rows
        .iter()
        .filter(|m| is_export_row(m))
        .filter_map(|m| {
            let text = content_to_string(&m.content);
            let semantic = match (query_emb, m.embedding.as_deref()) {
                (Some(q), Some(e)) if q.len() == e.len() => Some(cosine_similarity(q, e)),
                _ => None,
            };
            let keyword = if q_tokens.is_empty() {
                0.0
            } else {
                let t = tokens(&text);
                q_tokens.iter().filter(|w| t.contains(*w)).count() as f32 / q_tokens.len() as f32
            };
            let score = match semantic {
                // A keyword hit only rescues a weak semantic match if it covers half the
                // meaningful query terms.
                Some(s) if s >= MIN_COSINE || keyword >= 0.5 => s.max(0.0) + keyword,
                Some(_) => return None,
                None if keyword > 0.0 => keyword,
                None => return None,
            };
            Some((score, m))
        })
        .collect();
    scored.sort_by(|a, b| b.0.total_cmp(&a.0));

    // The cap applies to the tool result as actually serialized on the wire (the results
    // JSON is embedded as a string, so quotes/backslashes are escaped twice).
    let mut out = Vec::new();
    for (_, m) in scored.into_iter().take(limit) {
        out.push(Disclosed {
            id: m.id,
            text: truncate_chars(&redact_emails(&content_to_string(&m.content)), MAX_SNIPPET_CHARS),
            created_at: m.temporal.ingestion_time.to_rfc3339(),
        });
        if tool_result(&out).to_string().len() > MAX_RESPONSE_BYTES {
            out.pop();
            break;
        }
    }
    out
}

fn tool_result(items: &[Disclosed]) -> Value {
    json!({ "content": [{ "type": "text", "text": results_json(items) }] })
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
    cortex: Arc<Cortex>,
    paths: Paths,
    daily_requests: u32,
    daily_disclosures: u32,
    /// `remember` is opt-in (`serve --enable-remember`); without it the tool doesn't exist.
    remember_enabled: bool,
    daily_remembers: u32,
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

std::thread_local! {
    /// Who the request on this (blocking) thread came from: `static` or an OAuth grant id.
    /// Set by `handle_as` around one request; read only by `audit`.
    static CALLER: std::cell::RefCell<String> = const { std::cell::RefCell::new(String::new()) };
}

impl Gateway {
    fn handle_as(&self, caller: &str, req: &JsonRpcRequest) -> Option<JsonRpcResponse> {
        CALLER.with(|c| *c.borrow_mut() = caller.to_string());
        let out = self.handle(req);
        CALLER.with(|c| c.borrow_mut().clear());
        out
    }

    fn audit(&self, tool: &str, n: usize, ids: &[String], bytes: usize, outcome: &str) -> Result<(), String> {
        append_audit(
            &self.paths.audit,
            &json!({
                "ts": chrono::Utc::now().to_rfc3339(),
                "method": "tools/call",
                "client": CALLER.with(|c| c.borrow().clone()),
                "tool": tool,
                "n_results": n,
                "memory_ids": ids,
                "bytes": bytes,
                "outcome": outcome,
            }),
        )
    }

    fn deny(&self, tool: &str, outcome: &str, msg: &str) -> Value {
        let _ = self.audit(tool, 0, &[], 0, outcome);
        tool_error(msg)
    }

    /// Every authenticated call is charged against the request budget *before* anything
    /// else, including calls that are then refused, so refusals can't be used as a free,
    /// unmetered oracle. Then the kill switch. Caller must hold `self.lock`.
    fn charge(&self, tool: &str) -> Result<Budget, Value> {
        let mut budget = load_budget(&self.paths.budget, &today())
            .map_err(|e| self.deny(tool, "denied:budget_state", &format!("Refusing: {e}.")))?;
        if budget.requests >= self.daily_requests {
            const MSG: &str = "Daily request budget exhausted.";
            if budget.over_budget_logged {
                return Err(tool_error(MSG));
            }
            budget.over_budget_logged = true;
            let _ = save_budget(&self.paths.budget, &budget);
            return Err(self.deny(tool, "denied:request_budget", MSG));
        }
        budget.requests += 1;
        if save_budget(&self.paths.budget, &budget).is_err() {
            return Err(tool_error("Refusing: could not record the request."));
        }
        if self.paths.is_disabled() {
            return Err(self.deny(tool, "denied:disabled", "Memory access for Muse is turned off by the user."));
        }
        Ok(budget)
    }

    /// Once the distinct-disclosure budget is spent, unseen matches are silently withheld
    /// (indistinguishable from "no match").
    fn call_recall(&self, args: &Value) -> Value {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let Ok(_file_guard) = budget_file_lock(&self.paths) else {
            return tool_error("Refusing: could not lock the budget.");
        };
        let mut budget = match self.charge(TOOL_NAME) {
            Ok(b) => b,
            Err(v) => return v,
        };
        let (query, limit) = match parse_args(args) {
            Ok(v) => v,
            Err(e) => return self.deny(TOOL_NAME, "denied:bad_args", &e),
        };
        let mut items = match disclose(&self.cortex, &query, limit) {
            Ok(v) => v,
            Err(_) => return self.deny(TOOL_NAME, "error:storage", "Memory lookup failed."),
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
        let result = tool_result(&items);
        let bytes = result.to_string().len();
        if save_budget(&self.paths.budget, &budget).is_err()
            || self.audit(TOOL_NAME, items.len(), &ids, bytes, "ok").is_err()
        {
            return tool_error("Refusing to disclose: could not record the disclosure.");
        }
        result
    }

    /// Append-only capture into the quarantined inbox. Muse gets an acknowledgement with no
    /// id and no echo; nothing here is readable by Muse until the user approves it.
    fn call_remember(&self, args: &Value) -> Value {
        let _guard = self.lock.lock().unwrap_or_else(|p| p.into_inner());
        let Ok(_file_guard) = budget_file_lock(&self.paths) else {
            return tool_error("Refusing: could not lock the budget.");
        };
        let mut budget = match self.charge(REMEMBER_TOOL) {
            Ok(b) => b,
            Err(v) => return v,
        };
        let text = match args.get("text").and_then(Value::as_str) {
            Some(t) if !t.trim().is_empty() && t.chars().count() <= MAX_REMEMBER_CHARS => t,
            _ => {
                return self.deny(
                    REMEMBER_TOOL,
                    "denied:bad_args",
                    &format!("`text` must be a non-empty string of at most {MAX_REMEMBER_CHARS} characters"),
                )
            }
        };
        if budget.remembers >= self.daily_remembers {
            return self.deny(REMEMBER_TOOL, "denied:remember_budget", "Daily remember budget exhausted.");
        }
        // Account first, durably, and fail closed: an item is only ever stored after it has
        // been charged against the daily cap AND recorded in the audit log.
        budget.remembers += 1;
        if save_budget(&self.paths.budget, &budget).is_err() {
            return tool_error("Refusing: could not record the request.");
        }
        if self.audit(REMEMBER_TOOL, 0, &[], text.len(), "accepted").is_err() {
            return tool_error("Refusing: could not record the request.");
        }
        let stored_text = match &self.paths.inbox_key {
            Some(key) => match seal(key, text) {
                Ok(t) => t,
                Err(_) => return self.deny(REMEMBER_TOOL, "error:inbox", "Could not save."),
            },
            None => text.to_string(),
        };
        let item = InboxItem {
            id: Uuid::new_v4().to_string(),
            ts: chrono::Utc::now().to_rfc3339(),
            text: stored_text,
            source: "muse".into(),
        };
        let stored = with_inbox_lock(&self.paths, || {
            let pending = read_inbox(&self.paths.inbox)?;
            if pending.len() >= MAX_INBOX {
                return Err("inbox_full".to_string());
            }
            append_inbox(&self.paths.inbox, &item)
        });
        match stored {
            Ok(()) => json!({ "content": [{ "type": "text", "text": REMEMBER_ACK }] }),
            // Same reply (and the same charge, above) as success: a distinct "full" answer or
            // an uncharged cap would tell Muse whether the user has been draining the inbox.
            // The audit log records the drop.
            Err(e) if e == "inbox_full" => {
                let _ = self.audit(REMEMBER_TOOL, 0, &[], 0, "dropped:inbox_full");
                json!({ "content": [{ "type": "text", "text": REMEMBER_ACK }] })
            }
            // The charge stands (conservative): a failed write never refunds the cap.
            Err(_) => self.deny(REMEMBER_TOOL, "error:inbox", "Could not save."),
        }
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
            "tools/list" => {
                let mut tools = vec![tool_schema()];
                if self.remember_enabled {
                    tools.push(remember_schema());
                }
                JsonRpcResponse::success(id, json!({ "tools": tools }))
            }
            "tools/call" => {
                let name = req.params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = req.params.get("arguments").cloned().unwrap_or(json!({}));
                match name {
                    TOOL_NAME => JsonRpcResponse::success(id, self.call_recall(&args)),
                    REMEMBER_TOOL if self.remember_enabled => {
                        JsonRpcResponse::success(id, self.call_remember(&args))
                    }
                    _ => JsonRpcResponse::error(id, -32602, format!("Unknown tool: {name}")),
                }
            }
            other => JsonRpcResponse::error(id, -32601, format!("Method not found: {other}")),
        })
    }
}

fn remember_schema() -> Value {
    json!({
        "name": REMEMBER_TOOL,
        "description": "Save something worth remembering about the user to the user's own private memory. \
            The user reviews it first: you cannot read it back until they approve it, after which recall_memory can find it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "text": { "type": "string", "minLength": 1, "maxLength": MAX_REMEMBER_CHARS }
            },
            "required": ["text"]
        }
    })
}

// ── Remember inbox (quarantine) ──────────────────────────────────────────────

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct InboxItem {
    pub id: String,
    pub ts: String,
    pub text: String,
    pub source: String,
}

/// Held (as an open, locked file) for the whole charge-disclose-record sequence. An OS file
/// lock, not just the in-process mutex: two gateway instances for the same directory (e.g.
/// a cloud tenant reopened after cache eviction while old requests still run) must never
/// interleave budget updates.
fn budget_file_lock(paths: &Paths) -> std::io::Result<std::fs::File> {
    let f = private_options().create(true).truncate(false).write(true).open(paths.budget.with_extension("lock"))?;
    f.lock()?;
    Ok(f)
}

/// Exclusive lock shared by the server (append) and the CLI (approve/reject), so the two
/// processes never interleave a read-modify-write.
fn with_inbox_lock<T>(paths: &Paths, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let lock = private_options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(&paths.inbox_lock)
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let out = f();
    let _ = lock.unlock();
    out
}

/// Missing = empty. Any unparsable line = error (fail closed).
fn read_inbox(path: &Path) -> Result<Vec<InboxItem>, String> {
    let s = match read_private(path) {
        Ok(s) => s,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e.to_string()),
    };
    s.lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).map_err(|_| "inbox is corrupt".to_string()))
        .collect()
}

fn append_inbox(path: &Path, item: &InboxItem) -> Result<(), String> {
    let line = serde_json::to_string(item).map_err(|e| e.to_string())?;
    let mut f = private_options().create(true).append(true).open(path).map_err(|e| e.to_string())?;
    writeln!(f, "{line}").map_err(|e| e.to_string())?;
    f.sync_data().map_err(|e| e.to_string())?;
    sync_parent(path).map_err(|e| e.to_string()) // the file may have just been created
}

fn write_inbox(path: &Path, items: &[InboxItem]) -> Result<(), String> {
    let write = || -> std::io::Result<()> {
        let (tmp, mut f) = create_temp_beside(path)?;
        for item in items {
            writeln!(f, "{}", serde_json::to_string(item).map_err(std::io::Error::other)?)?;
        }
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        sync_parent(path)
    };
    write().map_err(|e| e.to_string())
}

/// Bidi controls, zero-width and other default-ignorable code points: invisible in a
/// terminal, so they could hide or reorder what the user thinks they're approving.
fn is_invisible(c: char) -> bool {
    matches!(c,
        '\u{00AD}' | '\u{034F}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{17B4}' | '\u{17B5}'
        | '\u{180B}'..='\u{180F}' | '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}'
        | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}' | '\u{FEFF}'
        | '\u{FFA0}' | '\u{FFF0}'..='\u{FFF8}' | '\u{E0000}'..='\u{E0FFF}')
}

/// Inbox text is untrusted (Muse wrote it): escape control and invisible characters so
/// printing it can't drive the user's terminal or disguise what they're approving.
fn terminal_safe(s: &str) -> String {
    s.chars()
        .map(|c| {
            if c.is_control() || is_invisible(c) {
                c.escape_unicode().to_string()
            } else {
                c.to_string()
            }
        })
        .collect()
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
    /// Static bearer token (`CORTEX_GATEWAY_TOKEN`); optional when OAuth is on.
    token: Option<String>,
    oauth: Option<oauth::OAuthConfig>,
    origins: Vec<String>,
    /// Bounds the blocking work actually in flight. The permit moves into the blocking
    /// task, so it is held for as long as the work runs, even if the client goes away.
    workers: Arc<tokio::sync::Semaphore>,
    /// Blocking state work for the OAuth endpoints (separate from `workers`).
    oauth_workers: Arc<tokio::sync::Semaphore>,
}

fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
}

fn rpc_error_response(status: StatusCode, code: i64, msg: &str) -> Response {
    let body = serde_json::to_string(&JsonRpcResponse::error(Value::Null, code, msg.into())).unwrap_or_default();
    (status, [("content-type", "application/json")], body).into_response()
}

/// Read a request body under an ABSOLUTE deadline (a per-frame timer would let a client
/// trickle one byte every few seconds and hold a slot indefinitely).
async fn read_body(headers: &HeaderMap, body: axum::body::Body, max: usize) -> Result<Bytes, Response> {
    let declared = headers
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    if declared.is_some_and(|n| n > max) {
        return Err(StatusCode::PAYLOAD_TOO_LARGE.into_response());
    }
    let read = axum::body::to_bytes(body, max);
    match tokio::time::timeout(std::time::Duration::from_secs(REQUEST_TIMEOUT_SECS), read).await {
        Ok(Ok(b)) => Ok(b),
        Ok(Err(_)) => Err(StatusCode::PAYLOAD_TOO_LARGE.into_response()),
        Err(_) => Err(StatusCode::REQUEST_TIMEOUT.into_response()),
    }
}

fn unauthorized(st: &HttpState, presented: bool) -> Response {
    let challenge = match &st.oauth {
        Some(o) => o.challenge(presented),
        None => "Bearer".to_string(),
    };
    (StatusCode::UNAUTHORIZED, [("www-authenticate", challenge)]).into_response()
}

/// `static` for the configured token, the grant id for a live OAuth access token.
async fn authenticate(st: &Arc<HttpState>, headers: &HeaderMap) -> Result<String, Response> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split_once(' '))
        .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
        .map(|(_, t)| t.trim().to_string());
    let Some(token) = token.filter(|t| !t.is_empty()) else {
        return Err(unauthorized(st, false));
    };
    if st.token.as_deref().is_some_and(|s| ct_eq(token.as_bytes(), s.as_bytes())) {
        return Ok("static".into());
    }
    // Only token-shaped strings reach the disk, and only within the bounded worker pool.
    if let Some(o) = st.oauth.as_ref().filter(|_| oauth::is_token_shaped(&token)) {
        let Ok(permit) = st.workers.clone().try_acquire_owned() else {
            return Err(rpc_error_response(StatusCode::SERVICE_UNAVAILABLE, -32000, "Server busy"));
        };
        let (paths, resource) = (st.gw.paths.clone(), o.resource.clone());
        let grant = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            oauth::grant_for_access_token(&paths, &resource, &token)
        })
        .await
        .ok()
        .flatten();
        if let Some(g) = grant {
            return Ok(g);
        }
    }
    Err(unauthorized(st, true))
}

async fn handle_post(State(st): State<Arc<HttpState>>, headers: HeaderMap, body: axum::body::Body) -> Response {
    let caller = match authenticate(&st, &headers).await {
        Ok(c) => c,
        Err(r) => return r,
    };
    if let Some(origin) = headers.get("origin") {
        let ok = origin.to_str().is_ok_and(|o| st.origins.iter().any(|a| a == o));
        if !ok {
            return StatusCode::FORBIDDEN.into_response();
        }
    }
    // The body is read only after auth.
    let body = match read_body(&headers, body, MAX_BODY_BYTES).await {
        Ok(b) => b,
        Err(r) => return r,
    };
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
    // Kill switch covers every method. `tools/call` checks it itself, after charging the
    // request budget (so a refusal is never a free oracle) and auditing.
    if req.method != "tools/call" && st.gw.paths.is_disabled() {
        return rpc_error_response(StatusCode::FORBIDDEN, -32000, "Memory access for Muse is turned off by the user");
    }
    let Ok(permit) = st.workers.clone().try_acquire_owned() else {
        return rpc_error_response(StatusCode::SERVICE_UNAVAILABLE, -32000, "Server busy");
    };
    let st2 = st.clone();
    let resp = tokio::task::spawn_blocking(move || {
        let _permit = permit;
        st2.gw.handle_as(&caller, &req)
    })
    .await;
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
    // Body reads have an absolute deadline (`read_body`). Deliberately no whole-request
    // timeout: that would drop the response while the blocking disclosure keeps running
    // (charging budget, auditing "ok"); that work is short and bounded by `workers`.
    // Header-phase slowloris is absorbed by the tunnel in front of this loopback listener.
    let mut r = Router::new()
        .route(
            "/mcp",
            post(handle_post).get(|| async { StatusCode::METHOD_NOT_ALLOWED }),
        )
        .layer(tower::limit::GlobalConcurrencyLimitLayer::new(MAX_CONCURRENT_REQUESTS));
    if state.oauth.is_some() {
        let oauth = Router::new()
            .route("/.well-known/oauth-protected-resource", get(oauth::protected_resource))
            .route("/.well-known/oauth-protected-resource/mcp", get(oauth::protected_resource))
            .route("/.well-known/oauth-authorization-server", get(oauth::authorization_server))
            .route("/register", post(oauth::register))
            .route("/authorize", get(oauth::authorize))
            .route("/authorize/wait", get(oauth::authorize_wait))
            .route("/authorize/approve", post(oauth::authorize_approve))
            .route("/token", post(oauth::token))
            .layer(tower::limit::GlobalConcurrencyLimitLayer::new(MAX_OAUTH_CONCURRENT));
        r = r.merge(oauth);
    }
    r.fallback(|| async { StatusCode::NOT_FOUND })
        .layer(DefaultBodyLimit::max(MAX_BODY_BYTES))
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

pub fn allow(cortex: &Cortex, text: &str) -> Result<Uuid, String> {
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
                println!("{}\t{}", m.id, terminal_safe(&content_to_string(&m.content).replace(['\n', '\t'], " ")));
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
            println!("{}", terminal_safe(&results_json(&items)));
        }
        GatewayAction::Audit => match read_private(&paths.audit) {
            Ok(s) => print!("{s}"),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => die(&e.to_string()),
        },
        GatewayAction::Off => {
            std::fs::write(&paths.disabled, b"off\n").unwrap_or_else(|e| die(&e.to_string()));
            // Sign-ins in flight die with the switch; signed-in clients are suspended, not revoked.
            if let Err(e) = oauth::cancel_in_flight(&paths) {
                eprintln!("warning: could not cancel OAuth sign-ins in progress: {e}");
            }
            println!("Muse access is OFF");
        }
        GatewayAction::Connect { code } => match oauth::connect(&paths, &code, true) {
            Ok(true) => println!("Approved. The sign-in page continues by itself."),
            Ok(false) => {
                println!("Refused.");
            }
            Err(e) => die(&e),
        },
        GatewayAction::Clients => {
            for line in oauth::list_grants(&paths).unwrap_or_else(|e| die(&e)) {
                println!("{line}");
            }
        }
        GatewayAction::Disconnect { id, all } => {
            let n = match (id.as_deref(), all) {
                (None, true) => oauth::disconnect(&paths, None),
                (Some(id), false) => match oauth::disconnect(&paths, Some(id)) {
                    Ok(0) => die("no signed-in client with that id (see `gateway clients`)"),
                    other => other,
                },
                _ => die("give either an ID or --all"),
            }
            .unwrap_or_else(|e| die(&e));
            println!("disconnected {n}");
        }
        GatewayAction::On => {
            match std::fs::remove_file(&paths.disabled) {
                Ok(()) => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => die(&e.to_string()),
            }
            println!("Muse access is ON");
        }
        GatewayAction::Inbox => {
            let items = with_inbox_lock(&paths, || read_inbox(&paths.inbox)).unwrap_or_else(|e| die(&e));
            for i in items {
                println!("{}\t{}\t{}", i.id, i.ts, terminal_safe(&i.text));
            }
        }
        GatewayAction::Approve { id } => {
            let cortex = open(db_path);
            let export_id = with_inbox_lock(&paths, || {
                let mut items = read_inbox(&paths.inbox)?;
                let pos = items.iter().position(|i| i.id == id).ok_or("no inbox item with that id")?;
                let text = items[pos].text.clone();
                // Memory writes first: if we crash before the inbox rewrite, the item is
                // still pending and re-approving is idempotent (dedup).
                cortex
                    .ingest_with_options(&text, "muse", None, None, None, None, Some(PrivacyLevel::Private))
                    .map_err(|e| e.to_string())?;
                let export_id = allow(&cortex, &text)?;
                items.remove(pos);
                write_inbox(&paths.inbox, &items)?;
                Ok(export_id)
            })
            .unwrap_or_else(|e| die(&e));
            println!("{export_id}");
        }
        GatewayAction::Reject { id, all } => {
            let removed = with_inbox_lock(&paths, || {
                let mut items = read_inbox(&paths.inbox)?;
                let before = items.len();
                match (id.as_deref(), all) {
                    (None, true) => items.clear(),
                    (Some(id), false) => {
                        items.retain(|i| i.id != id);
                        if items.len() == before {
                            return Err("no inbox item with that id".to_string());
                        }
                    }
                    _ => return Err("give either an ID or --all".to_string()),
                }
                write_inbox(&paths.inbox, &items)?;
                Ok(before - items.len())
            })
            .unwrap_or_else(|e| die(&e));
            println!("rejected {removed}");
        }
        GatewayAction::Serve {
            host,
            port,
            daily_requests,
            daily_disclosures,
            allow_origin,
            enable_remember,
            daily_remembers,
            oauth,
            public_url,
            oauth_redirect,
        } => {
            let token = std::env::var("CORTEX_GATEWAY_TOKEN").ok().filter(|t| !t.is_empty());
            if let Some(t) = &token {
                let distinct = t.chars().collect::<BTreeSet<_>>().len();
                if t.len() < MIN_TOKEN_LEN || distinct < MIN_TOKEN_DISTINCT_CHARS {
                    die("CORTEX_GATEWAY_TOKEN must be a random string of at least 32 characters (try `gateway token`)");
                }
            } else if !oauth {
                die("set CORTEX_GATEWAY_TOKEN (try `gateway token`) or use --oauth --public-url <https URL>");
            }
            let oauth_cfg = match (oauth, public_url) {
                (true, Some(url)) => Some(oauth::OAuthConfig::new(&url, &oauth_redirect).unwrap_or_else(|e| die(&e))),
                _ => None,
            };
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
                gw: Gateway {
                    cortex: Arc::new(cortex),
                    paths,
                    daily_requests,
                    daily_disclosures,
                    remember_enabled: enable_remember,
                    daily_remembers,
                    lock: Mutex::new(()),
                },
                token,
                oauth: oauth_cfg,
                origins: allow_origin,
                workers: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_REQUESTS)),
                oauth_workers: Arc::new(tokio::sync::Semaphore::new(MAX_OAUTH_WORKERS)),
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
                if let Some(o) = &state.oauth {
                    eprintln!("OAuth on: add the connector in Muse with {}", o.resource);
                }
                axum::serve(listener, router(state)).await.unwrap_or_else(|e| die(&e.to_string()));
            });
            drop(lock);
        }
    }
}

// ── Cortex Cloud tenant API ──────────────────────────────────────────────────
//
// The hosted service (`cortex-cloud`) runs one gateway per tenant directory, reusing
// everything above unchanged. See docs/design/muse-cloud.md.

const SEALED_PREFIX: &str = "enc1:";
/// Longest text one shared memory may have (the cloud refuses longer ones).
pub const MAX_EXPORT_TEXT_CHARS: usize = 2000;
/// all-MiniLM-L6-v2, the model devices and the cloud share.
pub const EMBED_DIM: usize = 384;

/// AES-256-GCM, random 96-bit nonce: `enc1:` + base64(nonce ‖ ciphertext).
fn seal(key: &[u8; 32], text: &str) -> Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::Engine as _;
    use rand::RngCore;
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let mut nonce = [0u8; 12];
    rand::thread_rng().fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(aes_gcm::Nonce::from_slice(&nonce), text.as_bytes())
        .map_err(|_| "encryption failed".to_string())?;
    let mut blob = nonce.to_vec();
    blob.extend_from_slice(&ct);
    Ok(format!("{SEALED_PREFIX}{}", base64::engine::general_purpose::STANDARD.encode(blob)))
}

fn unseal(key: &[u8; 32], stored: &str) -> Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use base64::Engine as _;
    let b64 = stored.strip_prefix(SEALED_PREFIX).ok_or("inbox item is not sealed")?;
    let blob = base64::engine::general_purpose::STANDARD.decode(b64).map_err(|_| "inbox item is corrupt")?;
    if blob.len() < 12 {
        return Err("inbox item is corrupt".into());
    }
    let cipher = aes_gcm::Aes256Gcm::new_from_slice(key).map_err(|e| e.to_string())?;
    let pt = cipher
        .decrypt(aes_gcm::Nonce::from_slice(&blob[..12]), &blob[12..])
        .map_err(|_| "inbox item failed authentication".to_string())?;
    String::from_utf8(pt).map_err(|_| "inbox item is corrupt".into())
}

/// Per-tenant gateway settings (the self-hosted `serve` flags).
pub struct TenantConfig {
    pub daily_requests: u32,
    pub daily_disclosures: u32,
    pub enable_remember: bool,
    pub daily_remembers: u32,
}

impl Default for TenantConfig {
    fn default() -> Self {
        Self { daily_requests: 100, daily_disclosures: 30, enable_remember: true, daily_remembers: 20 }
    }
}

/// The complete gateway (MCP + OAuth) for one tenant, mounted at the tenant's root: the
/// caller strips `/t/<rid>` before dispatching.
pub fn tenant_router(cortex: Arc<Cortex>, paths: Paths, oauth: oauth::OAuthConfig, cfg: &TenantConfig) -> Router {
    let state = Arc::new(HttpState {
        gw: Gateway {
            cortex,
            paths,
            daily_requests: cfg.daily_requests,
            daily_disclosures: cfg.daily_disclosures,
            remember_enabled: cfg.enable_remember,
            daily_remembers: cfg.daily_remembers,
            lock: Mutex::new(()),
        },
        token: None,
        oauth: Some(oauth),
        origins: Vec::new(),
        workers: Arc::new(tokio::sync::Semaphore::new(MAX_CONCURRENT_REQUESTS)),
        oauth_workers: Arc::new(tokio::sync::Semaphore::new(MAX_OAUTH_WORKERS)),
    });
    router(state)
}

/// How many memories Muse can currently see.
pub fn export_count(cortex: &Cortex) -> usize {
    export_rows(cortex).map(|r| r.len()).unwrap_or(0)
}

/// One item of a device push: the text and (optionally) its embedding from the device.
pub struct ExportItem {
    pub text: String,
    pub embedding: Option<Vec<f32>>,
}

/// Make the export equal `items` (a device push). Rows whose text is no longer shared are
/// deleted first, so an item the user stopped sharing is never served again even if the
/// push fails midway (the device pushes again). Unchanged rows keep their ids, so the
/// daily distinct-disclosure budget doesn't count them again.
pub fn replace_export(cortex: &Cortex, items: &[ExportItem]) -> Result<usize, String> {
    validate_export(items)?;
    apply_export(cortex, items)
}

/// Everything `replace_export` would refuse, checked without touching storage.
pub fn validate_export(items: &[ExportItem]) -> Result<(), String> {
    if items.len() > MAX_EXPORT {
        return Err(format!("at most {MAX_EXPORT} items"));
    }
    if let Some(bad) = items.iter().find(|i| i.text.trim().is_empty() || i.text.chars().count() > MAX_EXPORT_TEXT_CHARS) {
        return Err(format!("each item must be 1..={MAX_EXPORT_TEXT_CHARS} characters (got {})", bad.text.chars().count()));
    }
    if items.iter().any(|i| i.embedding.as_ref().is_some_and(|e| e.len() != EMBED_DIM || e.iter().any(|x| !x.is_finite()))) {
        return Err("invalid embedding".into());
    }
    Ok(())
}

fn apply_export(cortex: &Cortex, items: &[ExportItem]) -> Result<usize, String> {
    let wanted: BTreeSet<&str> = items.iter().map(|i| i.text.as_str()).collect();
    let mut kept = BTreeSet::new();
    for m in export_rows(cortex)? {
        let text = content_to_string(&m.content);
        if wanted.contains(text.as_str()) && kept.insert(text) {
            continue;
        }
        cortex.delete_memory(m.id).map_err(|e| e.to_string())?;
    }
    let mut seen = BTreeSet::new();
    for item in items {
        if !seen.insert(item.text.as_str()) || kept.contains(&item.text) {
            continue;
        }
        let source = MemSource {
            channel: "muse-export".into(),
            identity_id: None,
            chat_id: None,
            thread_id: None,
            message_id: None,
        };
        let mut builder = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text(item.text.clone()), source)
            .privacy(PrivacyLevel::Private)
            .namespace(EXPORT_NS);
        if let Some(e) = item.embedding.clone().or_else(|| cortex.embed_query(&item.text)) {
            builder = builder.embedding(e);
        }
        let mut mem = builder.build();
        mem.content_hash = mem.content_hash.map(|h| format!("{EXPORT_NS}:{h}"));
        cortex.storage().store_memory(&mem).map_err(|e| e.to_string())?;
    }
    Ok(seen.len())
}

/// Pending `remember` items, decrypted.
pub fn inbox_items(paths: &Paths) -> Result<Vec<InboxItem>, String> {
    let items = with_inbox_lock(paths, || read_inbox(&paths.inbox))?;
    items
        .into_iter()
        .map(|mut i| {
            if let Some(key) = &paths.inbox_key {
                i.text = unseal(key, &i.text)?;
            }
            Ok(i)
        })
        .collect()
}

/// Drop inbox items the device has taken (kept or discarded). Unknown ids are ignored.
pub fn inbox_ack(paths: &Paths, ids: &[String]) -> Result<usize, String> {
    with_inbox_lock(paths, || {
        let mut items = read_inbox(&paths.inbox)?;
        let before = items.len();
        items.retain(|i| !ids.contains(&i.id));
        if items.len() != before {
            write_inbox(&paths.inbox, &items)?;
        }
        Ok(before - items.len())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sealed_inbox_text_round_trips_and_detects_tampering() {
        let key = [3u8; 32];
        let sealed = seal(&key, "likes tea").unwrap();
        assert!(sealed.starts_with(SEALED_PREFIX) && !sealed.contains("tea"));
        assert_eq!(unseal(&key, &sealed).unwrap(), "likes tea");
        assert!(unseal(&[4u8; 32], &sealed).is_err(), "wrong key");
        let mut bad = sealed.clone();
        bad.pop();
        bad.push(if sealed.ends_with('A') { 'B' } else { 'A' });
        assert!(unseal(&key, &bad).is_err(), "tampered");
        assert!(unseal(&key, "plain").is_err(), "unsealed text refused");
    }

    #[test]
    fn replace_export_drops_unshared_items_and_validates() {
        let c = Cortex::in_memory().unwrap();
        let item = |t: &str| ExportItem { text: t.into(), embedding: Some(vec![0.05; EMBED_DIM]) };
        assert_eq!(replace_export(&c, &[item("a"), item("b"), item("a")]).unwrap(), 2);
        let id_b = export_rows(&c).unwrap().into_iter().find(|m| content_to_string(&m.content) == "b").unwrap().id;
        assert_eq!(replace_export(&c, &[item("b")]).unwrap(), 1);
        assert_eq!(export_rows(&c).unwrap()[0].id, id_b, "unchanged item keeps its id (disclosure budget)");
        let texts: Vec<String> = export_rows(&c).unwrap().iter().map(|m| content_to_string(&m.content)).collect();
        assert_eq!(texts, vec!["b".to_string()]);
        assert!(replace_export(&c, &[item(" ")]).is_err());
        assert!(replace_export(&c, &[ExportItem { text: "x".into(), embedding: Some(vec![f32::NAN; EMBED_DIM]) }]).is_err());
        assert!(replace_export(&c, &[ExportItem { text: "x".into(), embedding: Some(vec![0.1; 5]) }]).is_err(), "wrong dimension");
        assert!(replace_export(&c, &[ExportItem { text: "x".repeat(MAX_EXPORT_TEXT_CHARS + 1), embedding: None }]).is_err());
        assert!(replace_export(&c, &(0..=MAX_EXPORT).map(|i| item(&i.to_string())).collect::<Vec<_>>()).is_err());
        assert_eq!(export_rows(&c).unwrap().len(), 1, "a refused push changes nothing");
    }

    #[test]
    fn inbox_items_decrypt_and_ack_removes() {
        let dir = std::env::temp_dir().join(format!("gw-sealed-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::for_dir(&dir, Some([5u8; 32]));
        let item = InboxItem { id: "i1".into(), ts: "t".into(), text: seal(&[5u8; 32], "vegetarian").unwrap(), source: "muse".into() };
        append_inbox(&paths.inbox, &item).unwrap();
        assert!(!std::fs::read_to_string(&paths.inbox).unwrap().contains("vegetarian"));
        assert_eq!(inbox_items(&paths).unwrap()[0].text, "vegetarian");
        assert_eq!(inbox_ack(&paths, &["nope".into()]).unwrap(), 0);
        assert_eq!(inbox_ack(&paths, &["i1".into()]).unwrap(), 1);
        assert!(inbox_items(&paths).unwrap().is_empty());
    }

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
        assert!(tool_result(&out).to_string().len() <= MAX_RESPONSE_BYTES);
        assert!(!out.is_empty());
    }

    #[test]
    fn rank_caps_serialized_response_with_escaping_heavy_text() {
        // Quotes are escaped twice on the wire (results JSON inside a JSON string).
        let rows: Vec<_> = (0..5)
            .map(|i| mem(&format!("zebra{i} {}", "\"".repeat(480)), Some(EXPORT_NS), PrivacyLevel::Private))
            .collect();
        let out = rank(&rows, "zebra0 zebra1 zebra2 zebra3 zebra4", None, 5);
        assert!(!out.is_empty());
        assert!(tool_result(&out).to_string().len() <= MAX_RESPONSE_BYTES);
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
        let b = Budget { day: "2026-01-01".into(), requests: 7, disclosed: ["a".to_string()].into(), ..Budget::default() };
        save_budget(&p, &b).unwrap();
        assert_eq!(load_budget(&p, "2026-01-01").unwrap(), b);
        assert_eq!(load_budget(&p, "2026-01-02").unwrap().requests, 0);
        std::fs::write(&p, "{not json").unwrap();
        assert!(load_budget(&p, "2026-01-01").is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn over_budget_refusals_are_audited_once_per_day() {
        let dir = std::env::temp_dir().join(format!("gw-audit-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::for_db(dir.join("db.sqlite").to_str().unwrap());
        let audit = paths.audit.clone();
        let gw = Gateway {
            cortex: Arc::new(Cortex::in_memory().unwrap()),
            paths,
            daily_requests: 1,
            daily_disclosures: 5,
            remember_enabled: false,
            daily_remembers: 0,
            lock: Mutex::new(()),
        };
        for _ in 0..20 {
            gw.call_recall(&json!({"query": "zebra"}));
        }
        let lines = std::fs::read_to_string(&audit).unwrap();
        assert_eq!(lines.lines().count(), 2, "one ok + one denial: {lines}");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stopword_only_overlap_discloses_nothing() {
        let rows = [mem("my coffee is black", Some(EXPORT_NS), PrivacyLevel::Private)];
        assert!(rank(&rows, "what is my name", None, 5).is_empty());
        assert_eq!(rank(&rows, "what is my coffee", None, 5).len(), 1);
    }

    #[test]
    fn tokens_split_cjk_into_bigrams() {
        let t = tokens("我喜欢吃寿司 and Sushi");
        assert!(t.contains("寿司") && t.contains("喜欢") && t.contains("sushi") && t.contains("and"));
        assert!(tokens("猫").contains("猫"));
        let out = rank(&[mem("我喜欢吃寿司", Some(EXPORT_NS), PrivacyLevel::Private)], "寿司", None, 5);
        assert_eq!(out.len(), 1);
    }

    #[test]
    fn constant_time_eq() {
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"abcd"));
    }

    #[test]
    fn terminal_safe_escapes_controls_and_bidi() {
        let out = terminal_safe("ok\x1b[31mred\r\n\t\u{202E}e\u{200B}v\u{061C}i\u{FEFF}l");
        assert!(!out.chars().any(|c| c.is_control() || is_invisible(c)), "{out}");
        assert!(out.starts_with("ok") && out.contains("red") && out.contains("e\\u{200b}v"), "{out}");
    }

    #[test]
    fn inbox_roundtrip_and_fails_closed_on_corruption() {
        let dir = std::env::temp_dir().join(format!("gw-inbox-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let p = dir.join("inbox.jsonl");
        assert!(read_inbox(&p).unwrap().is_empty());
        let item = InboxItem { id: "1".into(), ts: "t".into(), text: "x".into(), source: "muse".into() };
        append_inbox(&p, &item).unwrap();
        assert_eq!(read_inbox(&p).unwrap(), vec![item]);
        write_inbox(&p, &[]).unwrap();
        assert!(read_inbox(&p).unwrap().is_empty());
        std::fs::write(&p, "{broken\n").unwrap();
        assert!(read_inbox(&p).is_err());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn full_inbox_is_indistinguishable_including_the_daily_cap() {
        let dir = std::env::temp_dir().join(format!("gw-full-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::for_db(dir.join("db.sqlite").to_str().unwrap());
        let filler = InboxItem { id: "x".into(), ts: "t".into(), text: "x".into(), source: "muse".into() };
        write_inbox(&paths.inbox, &vec![filler; MAX_INBOX]).unwrap();
        let gw = Gateway {
            cortex: Arc::new(Cortex::in_memory().unwrap()),
            paths,
            daily_requests: 100,
            daily_disclosures: 5,
            remember_enabled: true,
            daily_remembers: 2,
            lock: Mutex::new(()),
        };
        let call = || gw.call_remember(&json!({"text": "hello"}));
        assert_eq!(call()["content"][0]["text"], REMEMBER_ACK);
        assert_eq!(call()["content"][0]["text"], REMEMBER_ACK);
        // Same as with an empty inbox: the third call hits the daily cap.
        assert_eq!(call()["isError"], true);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn remember_fails_closed_when_accounting_cannot_be_persisted() {
        let dir = std::env::temp_dir().join(format!("gw-acct-{}", Uuid::new_v4()));
        std::fs::create_dir_all(&dir).unwrap();
        let paths = Paths::for_db(dir.join("db.sqlite").to_str().unwrap());
        // Audit path is a directory → every audit write fails.
        std::fs::create_dir_all(&paths.audit).unwrap();
        let inbox = paths.inbox.clone();
        let gw = Gateway {
            cortex: Arc::new(Cortex::in_memory().unwrap()),
            paths,
            daily_requests: 100,
            daily_disclosures: 5,
            remember_enabled: true,
            daily_remembers: 5,
            lock: Mutex::new(()),
        };
        let r = gw.call_remember(&json!({"text": "unaudited"}));
        assert_eq!(r["isError"], true, "{r}");
        assert!(read_inbox(&inbox).unwrap().is_empty(), "nothing stored without an audit record");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
