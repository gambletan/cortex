//! Black-box acceptance tests for the Muse gateway `remember` tool (capture inbox).
//!
//! Written by a context-isolated agent from `docs/design/muse-remember.md`,
//! `docs/design/muse-gateway.md` and `docs/muse.md` only — never from the
//! implementation. Drives the real binary: CLI subcommands + raw HTTP/1.1 against
//! `gateway serve --enable-remember`.
#![cfg(feature = "gateway")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_cortex-mcp-server");
const TOKEN: &str = "acceptance-test-token-0123456789abcdef0123456789abcdef";
const SAVED: &str = "Saved for the user's review.";

// ───────────────────────── environment / CLI helpers ─────────────────────────

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct Env {
    dir: PathBuf,
}

impl Env {
    fn new(name: &str) -> Env {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "cortex-gw-rem-{}-{}-{}-{}",
            name,
            std::process::id(),
            nanos,
            n
        ));
        std::fs::create_dir_all(&dir).unwrap();
        Env { dir }
    }

    fn db(&self) -> PathBuf {
        self.dir.join("memory.db")
    }
    fn audit_file(&self) -> PathBuf {
        self.dir.join("gateway-audit.jsonl")
    }

    fn cmd(&self) -> Command {
        let mut c = Command::new(BIN);
        c.env("CORTEX_NO_EMBEDDINGS", "1")
            .env("CORTEX_DB_PATH", self.db())
            .env("HOME", &self.dir)
            .env_remove("CORTEX_GATEWAY_TOKEN")
            .stdin(Stdio::null());
        c
    }

    fn run(&self, args: &[&str]) -> Output {
        self.cmd().args(args).output().expect("spawn cortex-mcp-server")
    }

    /// Run and require success; returns stdout (untrimmed, lossy).
    fn ok_raw(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {:?} failed: status={:?}\nstdout={}\nstderr={}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    fn ok(&self, args: &[&str]) -> String {
        self.ok_raw(args).trim().to_string()
    }

    /// `gateway inbox` → [(id, ts, escaped text)].
    fn inbox(&self) -> Vec<(String, String, String)> {
        self.ok_raw(&["gateway", "inbox"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let mut parts = l.splitn(3, '\t');
                let id = parts.next().unwrap().to_string();
                let ts = parts.next().unwrap_or_else(|| panic!("inbox line lacks ts: {l:?}")).to_string();
                let text = parts.next().unwrap_or_else(|| panic!("inbox line lacks text: {l:?}")).to_string();
                assert!(!id.is_empty() && !ts.is_empty(), "inbox line has empty id/ts: {l:?}");
                (id, ts, text)
            })
            .collect()
    }

    fn inbox_id_for(&self, needle: &str) -> String {
        let items = self.inbox();
        items
            .iter()
            .find(|(_, _, t)| t.contains(needle))
            .map(|(id, _, _)| id.clone())
            .unwrap_or_else(|| panic!("no inbox item containing {needle:?}: {items:?}"))
    }

    /// `gateway approve <id>` → printed export memory id.
    fn approve(&self, id: &str) -> String {
        let out = self.ok(&["gateway", "approve", id]);
        let printed = out.lines().last().unwrap_or("").trim().to_string();
        assert!(is_uuid(&printed), "approve should print the export memory UUID, got {out:?}");
        printed
    }

    fn list_text(&self) -> String {
        self.ok_raw(&["gateway", "list"])
    }

    fn preview_text(&self, query: &str) -> String {
        self.ok_raw(&["gateway", "preview", query, "--limit", "5"])
    }

    /// `cortex-mcp-server search <q>` → stdout (+stderr) of the normal search CLI.
    fn search_text(&self, query: &str) -> String {
        let out = self.run(&["search", query]);
        assert!(
            out.status.success(),
            "search failed: stdout={} stderr={}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        // The CLI echoes the query on a miss ("No results found for: <q>"); that line is
        // not a result, so drop it before callers check for the keyword.
        String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter(|l| !l.starts_with("No results found for:"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn serve(&self, extra: &[&str]) -> Server {
        let mut args = vec!["gateway", "serve", "--port", "0"];
        args.extend_from_slice(extra);
        let mut child = self
            .cmd()
            .args(&args)
            .env("CORTEX_GATEWAY_TOKEN", TOKEN)
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn gateway serve");
        let stderr = child.stderr.take().unwrap();
        let (tx, rx) = mpsc::channel::<String>();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines() {
                match line {
                    Ok(l) => {
                        let _ = tx.send(l);
                    }
                    Err(_) => break,
                }
            }
        });
        let mut server = Server { child, port: 0 };
        let deadline = Instant::now() + Duration::from_secs(60);
        let mut seen = String::new();
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match rx.recv_timeout(left) {
                Ok(line) => {
                    if let Some(port) = parse_listen_port(&line) {
                        server.port = port;
                        return server;
                    }
                    seen.push_str(&line);
                    seen.push('\n');
                }
                Err(_) => panic!("gateway serve never reported its port; stderr:\n{}", seen),
            }
        }
    }
}

impl Drop for Env {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

struct Server {
    child: Child,
    port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn parse_listen_port(line: &str) -> Option<u16> {
    let idx = line.find("listening on http://")?;
    let rest = &line[idx + "listening on http://".len()..];
    let hostport = &rest[..rest.find("/mcp")?];
    hostport.rsplit(':').next()?.parse().ok()
}

fn is_uuid(s: &str) -> bool {
    let b = s.as_bytes();
    b.len() == 36
        && b.iter().enumerate().all(|(i, c)| match i {
            8 | 13 | 18 | 23 => *c == b'-',
            _ => c.is_ascii_hexdigit(),
        })
}

// ───────────────────────────── HTTP helpers ─────────────────────────────

struct Resp {
    status: u16,
    body: String,
}

fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect gateway");
    s.set_read_timeout(Some(Duration::from_secs(30))).unwrap();
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nConnection: close\r\nContent-Length: {}\r\n",
        body.len()
    );
    for (k, v) in headers {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let _ = s.write_all(req.as_bytes());
    let _ = s.write_all(body);
    let _ = s.flush();
    let mut raw = Vec::new();
    let _ = s.read_to_end(&mut raw);
    let text = String::from_utf8_lossy(&raw).to_string();
    let (head, rest) = text
        .split_once("\r\n\r\n")
        .unwrap_or_else(|| panic!("malformed HTTP response: {:?}", text));
    let status: u16 = head
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("bad status line: {head}"));
    let chunked = head.lines().any(|l| {
        l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked")
    });
    let body = if chunked { dechunk(rest) } else { rest.to_string() };
    Resp { status, body }
}

fn dechunk(mut s: &str) -> String {
    let mut out = String::new();
    while let Some((size_line, rest)) = s.split_once("\r\n") {
        let size = usize::from_str_radix(size_line.split(';').next().unwrap().trim(), 16).unwrap_or(0);
        if size == 0 || rest.len() < size {
            break;
        }
        out.push_str(&rest[..size]);
        s = rest[size..].strip_prefix("\r\n").unwrap_or(&rest[size..]);
    }
    out
}

fn post_authed(port: u16, body: &str) -> Resp {
    let auth = format!("Bearer {TOKEN}");
    http(
        port,
        "POST",
        "/mcp",
        &[
            ("Content-Type", "application/json"),
            ("Accept", "application/json, text/event-stream"),
            ("Authorization", auth.as_str()),
        ],
        body.as_bytes(),
    )
}

/// Authenticated JSON-RPC request; returns the full response envelope.
fn rpc(port: u16, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let r = post_authed(port, &body);
    assert_eq!(r.status, 200, "{method}: unexpected HTTP status, body={}", r.body);
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("{method}: non-JSON body ({e}): {}", r.body))
}

/// tools/call <name> → (raw body, full envelope).
fn call_tool(port: u16, name: &str, args: Value) -> (String, Value) {
    let body = json!({
        "jsonrpc": "2.0", "id": 9, "method": "tools/call",
        "params": {"name": name, "arguments": args}
    })
    .to_string();
    let r = post_authed(port, &body);
    assert_eq!(r.status, 200, "tools/call {name} HTTP status, body={}", r.body);
    let v: Value = serde_json::from_str(&r.body).expect("tools/call JSON");
    (r.body, v)
}

/// tools/call remember → (raw body, `result` object). Panics on a JSON-RPC error.
fn remember_raw(port: u16, args: Value) -> (String, Value) {
    let (body, v) = call_tool(port, "remember", args);
    let result = v
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("remember has no result: {body}"));
    (body, result)
}

fn is_error(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) == Some(true)
}

fn content_text(result: &Value) -> String {
    let content = result["content"].as_array().expect("result.content array");
    assert_eq!(content.len(), 1, "exactly one content item: {result}");
    assert_eq!(content[0]["type"], "text");
    content[0]["text"].as_str().expect("content text").to_string()
}

/// Successful remember: exact confirmation text, no echo.
fn remember_ok(port: u16, text: &str) {
    let (body, result) = remember_raw(port, json!({"text": text}));
    assert!(!is_error(&result), "remember unexpectedly errored: {body}");
    assert_eq!(content_text(&result), SAVED, "{body}");
}

fn recall_texts_and_body(port: u16, query: &str) -> (Vec<String>, String) {
    let (body, v) = call_tool(port, "recall_memory", json!({"query": query, "limit": 5}));
    let result = v.get("result").cloned().unwrap_or_else(|| panic!("recall has no result: {body}"));
    assert!(!is_error(&result), "recall errored: {body}");
    let inner: Value = serde_json::from_str(&content_text(&result)).expect("recall content is JSON");
    let texts = inner["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|r| r["text"].as_str().unwrap().to_string())
        .collect();
    (texts, body)
}

fn tool_names(port: u16) -> Vec<String> {
    let list = rpc(port, "tools/list", json!({}));
    list["result"]["tools"]
        .as_array()
        .expect("tools array")
        .iter()
        .map(|t| t["name"].as_str().unwrap().to_string())
        .collect()
}

fn audit_lines(env: &Env) -> Vec<Value> {
    std::fs::read_to_string(env.audit_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("audit line not JSON ({e}): {l}")))
        .collect()
}

// ─────────────────────────────── opt-in flag ───────────────────────────────

#[test]
fn without_flag_remember_is_unlisted_and_unknown() {
    let env = Env::new("noflag");
    let srv = env.serve(&[]);
    assert_eq!(tool_names(srv.port), vec!["recall_memory"], "only recall_memory without the flag");

    let (body, v) = call_tool(srv.port, "remember", json!({"text": "noflagotter must not be stored"}));
    assert!(v.get("error").is_some(), "remember without the flag must be a JSON-RPC error: {body}");
    assert!(v.get("result").is_none(), "no result alongside the error: {body}");
    assert!(!body.contains(SAVED), "{body}");
    drop(srv);
    assert!(env.inbox().is_empty(), "nothing stored without the flag");
}

#[test]
fn with_flag_tools_list_has_remember_with_schema() {
    let env = Env::new("schema");
    let srv = env.serve(&["--enable-remember"]);
    let list = rpc(srv.port, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools array");
    let mut names: Vec<&str> = tools.iter().map(|t| t["name"].as_str().unwrap()).collect();
    names.sort();
    assert_eq!(names, vec!["recall_memory", "remember"], "{list}");

    let t = tools.iter().find(|t| t["name"] == "remember").unwrap();
    let schema = &t["inputSchema"];
    assert_eq!(schema["type"], "object", "{t}");
    let text = &schema["properties"]["text"];
    assert_eq!(text["type"], "string", "{t}");
    assert_eq!(text["minLength"], 1, "{t}");
    assert_eq!(text["maxLength"], 1000, "{t}");
    let req = schema["required"].as_array().expect("required");
    assert!(req.contains(&json!("text")), "{t}");
}

// ───────────────────────────── remember result ─────────────────────────────

#[test]
fn remember_success_has_no_id_and_no_echo() {
    let env = Env::new("noecho");
    let srv = env.serve(&["--enable-remember"]);
    let text = "echoparrot prefers window seats on night trains";
    let (body, result) = remember_raw(srv.port, json!({"text": text}));
    assert!(!is_error(&result), "{body}");
    assert_eq!(content_text(&result), SAVED);
    assert!(!body.contains("echoparrot"), "response echoed the text: {body}");

    // The returned body must not carry the inbox id either.
    let items = env.inbox();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].2.contains("echoparrot"), "{items:?}");
    assert!(!body.contains(&items[0].0), "response leaked the inbox id: {body}");
}

#[test]
fn remember_bad_args_are_tool_errors_and_store_nothing() {
    let env = Env::new("badargs");
    let srv = env.serve(&["--enable-remember", "--daily-requests", "100"]);
    let too_long = format!("badargyak {}", "y".repeat(1000)); // 1010 chars
    for args in [
        json!({}),
        json!({"text": ""}),
        json!({"text": 42}),
        json!({"text": null}),
        json!({"text": ["badargyak"]}),
        json!({"text": too_long}),
    ] {
        let (body, result) = remember_raw(srv.port, args.clone());
        assert!(is_error(&result), "args {} should be isError: {body}", &args.to_string()[..args.to_string().len().min(80)]);
        assert!(!body.contains(SAVED), "{body}");
    }
    assert!(env.inbox().is_empty(), "invalid remembers must store nothing: {:?}", env.inbox());

    // Boundary: exactly 1000 ASCII chars is accepted.
    let exact = format!("boundarymole{}", "z".repeat(1000 - "boundarymole".len()));
    assert_eq!(exact.chars().count(), 1000);
    remember_ok(srv.port, &exact);
    assert_eq!(env.inbox().len(), 1);
}

// ─────────────────────────────── quarantine ───────────────────────────────

#[test]
fn remembered_items_are_quarantined_until_approved() {
    let env = Env::new("quarantine");
    let srv = env.serve(&["--enable-remember"]);
    let text = "quarantowl the user keeps bees on the roof";
    remember_ok(srv.port, text);

    assert_eq!(env.inbox().len(), 1, "item is pending in the inbox");

    let (results, body) = recall_texts_and_body(srv.port, "quarantowl");
    assert!(results.is_empty(), "pending item returned by recall: {results:?}");
    assert!(!body.contains("quarantowl"), "{body}");

    assert!(!env.search_text("quarantowl").contains("quarantowl"), "pending item visible to ordinary search");
    assert!(!env.list_text().contains("quarantowl"), "pending item in gateway list");
    assert!(!env.preview_text("quarantowl").contains("quarantowl"), "pending item in gateway preview");
}

// ─────────────────────────────── inbox display ───────────────────────────────

#[test]
fn inbox_escapes_control_characters() {
    let env = Env::new("escape");
    let srv = env.serve(&["--enable-remember"]);
    let evil = "escowl \x1b[31mRED\x1b[0m \x1b]0;pwned\x07 cr\rlf\nnext\ttabbed \x08bs \x7fdel";
    remember_ok(srv.port, evil);
    remember_ok(srv.port, "plainwren second item");

    let raw = env.ok_raw(&["gateway", "inbox"]);
    for (c, name) in [('\x1b', "ESC"), ('\r', "CR"), ('\x07', "BEL"), ('\x08', "BS"), ('\x7f', "DEL")] {
        assert!(!raw.contains(c), "raw {name} in inbox output: {raw:?}");
    }
    let lines: Vec<&str> = raw.lines().filter(|l| !l.trim().is_empty()).collect();
    assert_eq!(lines.len(), 2, "one line per pending item (embedded \\n escaped): {raw:?}");
    for l in &lines {
        assert_eq!(l.matches('\t').count(), 2, "exactly 2 tab separators (embedded \\t escaped): {l:?}");
    }
    let evil_line = lines.iter().find(|l| l.contains("escowl")).expect("escowl line");
    assert!(evil_line.contains("RED") && evil_line.contains("next") && evil_line.contains("tabbed"), "{evil_line:?}");
    assert!(lines.iter().any(|l| l.ends_with("plainwren second item")), "plain text shown as-is: {lines:?}");
}

// ─────────────────────────────── approve ───────────────────────────────

#[test]
fn approve_makes_item_recallable_and_searchable() {
    let env = Env::new("approve");
    let text = "approvelynx the user is allergic to shellfish";
    {
        let srv = env.serve(&["--enable-remember"]);
        remember_ok(srv.port, text);
    }
    let id = env.inbox_id_for("approvelynx");
    let export_id = env.approve(&id);

    assert!(env.inbox().is_empty(), "approved item left the inbox");
    assert!(
        env.list_text().lines().any(|l| l.starts_with(&export_id) && l.contains("approvelynx")),
        "export copy listed under the printed id: {}",
        env.list_text()
    );
    assert!(env.search_text("approvelynx").contains("approvelynx"), "approved item is a normal memory via search");

    let srv = env.serve(&["--enable-remember"]);
    let (results, _) = recall_texts_and_body(srv.port, "approvelynx");
    assert_eq!(results, vec![text.to_string()], "recall returns the approved item");
}

#[test]
fn approve_unknown_id_fails_and_changes_nothing() {
    let env = Env::new("approveunknown");
    {
        let srv = env.serve(&["--enable-remember"]);
        remember_ok(srv.port, "staysputgnu pending forever");
    }
    let before = env.inbox();
    for bogus in ["00000000-0000-4000-8000-000000000000", "does-not-exist", ""] {
        let out = env.run(&["gateway", "approve", bogus]);
        assert!(!out.status.success(), "approve {bogus:?} must fail");
    }
    assert_eq!(env.inbox(), before, "inbox unchanged");
    assert!(!env.list_text().contains("staysputgnu"));
    assert!(!env.search_text("staysputgnu").contains("staysputgnu"));
}

// ─────────────────────────────── reject ───────────────────────────────

#[test]
fn reject_removes_item_and_it_never_becomes_recallable() {
    let env = Env::new("reject");
    let srv = env.serve(&["--enable-remember"]);
    remember_ok(srv.port, "rejectbadger bogus injected fact");
    remember_ok(srv.port, "keepheron legit fact");

    let rid = env.inbox_id_for("rejectbadger");
    env.ok(&["gateway", "reject", &rid]);
    let items = env.inbox();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].2.contains("keepheron"), "{items:?}");

    // Rejected id can no longer be approved.
    assert!(!env.run(&["gateway", "approve", &rid]).status.success(), "approve of a rejected id must fail");
    let (results, body) = recall_texts_and_body(srv.port, "rejectbadger");
    assert!(results.is_empty() && !body.contains("rejectbadger"), "{body}");
    assert!(!env.search_text("rejectbadger").contains("rejectbadger"));
    assert!(!env.list_text().contains("rejectbadger"));
}

#[test]
fn reject_all_empties_inbox() {
    let env = Env::new("rejectall");
    let srv = env.serve(&["--enable-remember"]);
    for i in 0..3 {
        remember_ok(srv.port, &format!("rejectallmink item {i}"));
    }
    assert_eq!(env.inbox().len(), 3);
    env.ok(&["gateway", "reject", "--all"]);
    assert!(env.inbox().is_empty(), "reject --all empties the inbox");
    let (results, _) = recall_texts_and_body(srv.port, "rejectallmink");
    assert!(results.is_empty());
}

#[test]
fn reject_unknown_id_fails_and_changes_nothing() {
    let env = Env::new("rejectunknown");
    {
        let srv = env.serve(&["--enable-remember"]);
        remember_ok(srv.port, "rejectstaytapir pending");
    }
    let before = env.inbox();
    let out = env.run(&["gateway", "reject", "00000000-0000-4000-8000-000000000000"]);
    assert!(!out.status.success(), "reject of unknown id must fail");
    assert_eq!(env.inbox(), before);
}

// ─────────────────────────────── caps / budget ───────────────────────────────

#[test]
fn daily_remember_cap_enforced() {
    let env = Env::new("remcap");
    let srv = env.serve(&["--enable-remember", "--daily-remembers", "2", "--daily-requests", "100"]);
    remember_ok(srv.port, "capfinch one");
    remember_ok(srv.port, "capfinch two");
    let (body, result) = remember_raw(srv.port, json!({"text": "capfinch three overflow"}));
    assert!(is_error(&result), "3rd remember over --daily-remembers 2 must error: {body}");
    let items = env.inbox();
    assert_eq!(items.len(), 2, "{items:?}");
    assert!(items.iter().all(|(_, _, t)| !t.contains("overflow")), "{items:?}");
}

#[test]
fn daily_remember_cap_default_is_20() {
    let env = Env::new("remcapdefault");
    let srv = env.serve(&["--enable-remember", "--daily-requests", "100"]);
    for i in 0..20 {
        remember_ok(srv.port, &format!("defaultcapvole {i}"));
    }
    let (body, result) = remember_raw(srv.port, json!({"text": "defaultcapvole overflow"}));
    assert!(is_error(&result), "21st remember over default cap must error: {body}");
    assert_eq!(env.inbox().len(), 20);
}

#[test]
fn remember_consumes_daily_request_budget() {
    let env = Env::new("reqbudget");
    let srv = env.serve(&["--enable-remember", "--daily-requests", "2", "--daily-remembers", "50"]);
    remember_ok(srv.port, "budgetkoi one");
    remember_ok(srv.port, "budgetkoi two");
    let (body, result) = remember_raw(srv.port, json!({"text": "budgetkoi three overflow"}));
    assert!(is_error(&result), "remember over the request budget must error: {body}");
    assert_eq!(env.inbox().len(), 2);

    // recall shares the same exhausted budget.
    let (body, v) = call_tool(srv.port, "recall_memory", json!({"query": "budgetkoi"}));
    assert!(is_error(&v["result"]), "recall after remembers exhausted the budget must error: {body}");
}

// ─────────────────────────────── kill switch ───────────────────────────────

#[test]
fn kill_switch_blocks_remember() {
    let env = Env::new("kill");
    let srv = env.serve(&["--enable-remember"]);
    env.ok(&["gateway", "off"]);
    let (body, result) = remember_raw(srv.port, json!({"text": "killswitchlemur must not land"}));
    assert!(is_error(&result), "remember with kill switch on must error: {body}");
    assert!(!body.contains(SAVED));
    env.ok(&["gateway", "on"]);
    assert!(env.inbox().is_empty(), "nothing stored while off: {:?}", env.inbox());
    remember_ok(srv.port, "killswitchlemur after on");
    assert_eq!(env.inbox().len(), 1);
}

// ─────────────────────────────── audit ───────────────────────────────

#[test]
fn audit_logs_remember_without_text() {
    let env = Env::new("audit");
    let srv = env.serve(&["--enable-remember"]);
    remember_ok(srv.port, "auditsecretmarker the user's locker code is 4417");
    let (_, result) = remember_raw(srv.port, json!({"text": ""}));
    assert!(is_error(&result));

    let lines = audit_lines(&env);
    let rem: Vec<&Value> = lines.iter().filter(|l| l["tool"] == "remember").collect();
    assert_eq!(rem.len(), 2, "one audit line per remember call: {lines:?}");
    for l in &rem {
        assert!(l.get("ts").is_some() && l.get("outcome").is_some(), "{l}");
    }
    let raw = std::fs::read_to_string(env.audit_file()).unwrap();
    let printed = env.ok(&["gateway", "audit"]);
    for hay in [&raw, &printed] {
        for secret in ["auditsecretmarker", "locker", "4417"] {
            assert!(!hay.contains(secret), "audit leaked {secret:?}: {hay}");
        }
    }
}

// ──────────────────────── live server + CLI concurrency ────────────────────────

#[test]
fn approve_and_reject_while_server_running() {
    let env = Env::new("live");
    let srv = env.serve(&["--enable-remember"]);
    remember_ok(srv.port, "liveotter approve me");
    remember_ok(srv.port, "liveweasel reject me");

    let a = env.inbox_id_for("liveotter");
    let r = env.inbox_id_for("liveweasel");
    env.approve(&a);
    env.ok(&["gateway", "reject", &r]);
    assert!(env.inbox().is_empty());

    // Server sees the approval without restart.
    let (results, _) = recall_texts_and_body(srv.port, "liveotter");
    assert_eq!(results, vec!["liveotter approve me".to_string()]);
    let (results, _) = recall_texts_and_body(srv.port, "liveweasel");
    assert!(results.is_empty());

    // A remember after approve/reject still works and lands in the shared inbox.
    remember_ok(srv.port, "livebison after approve");
    let items = env.inbox();
    assert_eq!(items.len(), 1, "{items:?}");
    assert!(items[0].2.contains("livebison"));
}

#[test]
fn concurrent_remembers_and_cli_rejects_do_not_lose_or_corrupt() {
    let env = Env::new("concurrent");
    let srv = env.serve(&["--enable-remember", "--daily-remembers", "100", "--daily-requests", "200"]);
    let port = srv.port;
    let writer = std::thread::spawn(move || {
        for i in 0..15 {
            remember_ok(port, &format!("concgecko item {i}"));
        }
    });
    // Meanwhile, the CLI repeatedly rewrites the inbox by rejecting what it sees.
    let mut rejected = 0usize;
    let deadline = Instant::now() + Duration::from_secs(60);
    while !writer.is_finished() && Instant::now() < deadline {
        if let Some((id, _, _)) = env.inbox().into_iter().next() {
            env.ok(&["gateway", "reject", &id]);
            rejected += 1;
        }
    }
    writer.join().expect("writer thread");
    let remaining = env.inbox(); // also proves the file still parses (no corrupt lines)
    assert_eq!(remaining.len() + rejected, 15, "no remember lost or duplicated: rejected={rejected} remaining={remaining:?}");
    remember_ok(port, "concgecko final");
}
