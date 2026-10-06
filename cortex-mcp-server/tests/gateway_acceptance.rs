//! Black-box acceptance tests for `cortex-mcp-server gateway` (Muse gateway).
//!
//! Written by a context-isolated agent from `docs/design/muse-gateway.md` and the
//! gateway contract only — never from the implementation. Drives the real binary:
//! CLI subcommands + raw HTTP/1.1 against `gateway serve`.
#![cfg(feature = "gateway")]

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_cortex-mcp-server");
const TOKEN: &str = "acceptance-test-token-0123456789abcdef0123456789abcdef";
const EXPORT_NS: &str = "muse-export";

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
            "cortex-gw-acc-{}-{}-{}-{}",
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
    fn kill_switch(&self) -> PathBuf {
        self.dir.join("gateway.disabled")
    }
    fn state_file(&self) -> PathBuf {
        self.dir.join("gateway-state.json")
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

    /// Run and require success; returns trimmed stdout.
    fn ok(&self, args: &[&str]) -> String {
        let out = self.run(args);
        assert!(
            out.status.success(),
            "command {:?} failed: status={:?}\nstdout={}\nstderr={}",
            args,
            out.status,
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        String::from_utf8_lossy(&out.stdout).trim().to_string()
    }

    /// `gateway allow <text>` → new export memory UUID.
    fn allow(&self, text: &str) -> String {
        let id = self.ok(&["gateway", "allow", text]);
        assert!(is_uuid(&id), "allow should print a UUID, got {:?}", id);
        id
    }

    /// Normal (non-export) memory via the existing ingest CLI. Returns its UUID,
    /// parsed from the command's output.
    fn ingest(&self, text: &str, privacy: &str) -> String {
        let out = self.run(&["ingest", text, "--privacy", privacy]);
        assert!(
            out.status.success(),
            "ingest failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let all = format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        find_uuid(&all).unwrap_or_else(|| panic!("ingest output has no UUID: {}", all))
    }

    fn list(&self) -> Vec<(String, String)> {
        self.ok(&["gateway", "list"])
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| {
                let (id, text) = l.split_once('\t').expect("list line is <uuid>\\t<text>");
                (id.to_string(), text.to_string())
            })
            .collect()
    }

    fn preview(&self, query: &str) -> Value {
        let s = self.ok(&["gateway", "preview", query, "--limit", "5"]);
        serde_json::from_str(&s).unwrap_or_else(|e| panic!("preview not JSON ({e}): {s}"))
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
            // Keep draining stderr for the life of the process so it never blocks.
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

fn find_uuid(s: &str) -> Option<String> {
    let chars: Vec<char> = s.chars().collect();
    (0..chars.len().saturating_sub(35))
        .map(|i| chars[i..i + 36].iter().collect::<String>())
        .find(|c| is_uuid(c))
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
    // The server may answer (e.g. 413) before consuming the whole body; ignore write errors.
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
    let chunked = head
        .lines()
        .any(|l| l.to_ascii_lowercase().starts_with("transfer-encoding:") && l.to_ascii_lowercase().contains("chunked"));
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

const JSON_HDRS: [(&str, &str); 2] = [
    ("Content-Type", "application/json"),
    ("Accept", "application/json, text/event-stream"),
];

fn post_raw(port: u16, extra: &[(&str, &str)], body: &str) -> Resp {
    let mut h: Vec<(&str, &str)> = JSON_HDRS.to_vec();
    h.extend_from_slice(extra);
    http(port, "POST", "/mcp", &h, body.as_bytes())
}

fn post_authed(port: u16, body: &str) -> Resp {
    let auth = format!("Bearer {TOKEN}");
    post_raw(port, &[("Authorization", auth.as_str())], body)
}

/// Authenticated JSON-RPC request; returns the full response envelope.
fn rpc(port: u16, method: &str, params: Value) -> Value {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let r = post_authed(port, &body);
    assert_eq!(r.status, 200, "{method}: unexpected HTTP status, body={}", r.body);
    serde_json::from_str(&r.body).unwrap_or_else(|e| panic!("{method}: non-JSON body ({e}): {}", r.body))
}

/// tools/call recall_memory → (raw response body, `result` object).
fn recall_raw(port: u16, args: Value) -> (String, Value) {
    let body = json!({
        "jsonrpc": "2.0", "id": 7, "method": "tools/call",
        "params": {"name": "recall_memory", "arguments": args}
    })
    .to_string();
    let r = post_authed(port, &body);
    assert_eq!(r.status, 200, "tools/call HTTP status, body={}", r.body);
    let v: Value = serde_json::from_str(&r.body).expect("tools/call JSON");
    let result = v
        .get("result")
        .cloned()
        .unwrap_or_else(|| panic!("tools/call has no result: {}", r.body));
    (r.body, result)
}

fn is_error(result: &Value) -> bool {
    result.get("isError").and_then(Value::as_bool) == Some(true)
}

/// Successful recall → the list of result objects.
fn recall(port: u16, query: &str, limit: u64) -> Vec<Value> {
    let (body, result) = recall_raw(port, json!({"query": query, "limit": limit}));
    assert!(!is_error(&result), "recall unexpectedly errored: {body}");
    parse_results(&result)
}

fn parse_results(result: &Value) -> Vec<Value> {
    let content = result["content"].as_array().expect("result.content array");
    assert_eq!(content.len(), 1, "exactly one content item: {result}");
    assert_eq!(content[0]["type"], "text");
    let inner: Value = serde_json::from_str(content[0]["text"].as_str().expect("content text"))
        .expect("content text is JSON");
    inner["results"].as_array().expect("results array").clone()
}

fn texts(results: &[Value]) -> Vec<String> {
    results
        .iter()
        .map(|r| r["text"].as_str().expect("text field").to_string())
        .collect()
}

/// Every disclosed result object carries exactly {text, created_at}.
fn assert_result_shape(results: &[Value]) {
    for r in results {
        let obj = r.as_object().expect("result is object");
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort();
        assert_eq!(keys, vec!["created_at", "text"], "result leaks extra fields: {r}");
        assert!(!r["created_at"].is_null(), "created_at present: {r}");
    }
}

fn audit_lines(env: &Env) -> Vec<Value> {
    std::fs::read_to_string(env.audit_file())
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(|l| serde_json::from_str(l).unwrap_or_else(|e| panic!("audit line not JSON ({e}): {l}")))
        .collect()
}

fn wait_exit(child: &mut Child, within: Duration) -> Option<std::process::ExitStatus> {
    let deadline = Instant::now() + within;
    while Instant::now() < deadline {
        if let Some(st) = child.try_wait().unwrap() {
            return Some(st);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    None
}

fn exists(p: &Path) -> bool {
    p.exists()
}

// ──────────────────────────────── CLI tests ────────────────────────────────

#[test]
fn cli_token_is_64_lowercase_hex_and_fresh() {
    let env = Env::new("token");
    let a = env.ok(&["gateway", "token"]);
    let b = env.ok(&["gateway", "token"]);
    for t in [&a, &b] {
        assert_eq!(t.lines().count(), 1, "one line: {t:?}");
        assert_eq!(t.len(), 64, "64 chars: {t:?}");
        assert!(t.chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')), "lowercase hex: {t:?}");
    }
    assert_ne!(a, b, "each call should mint a fresh random token");
}

#[test]
fn cli_allow_list_revoke_roundtrip() {
    let env = Env::new("roundtrip");
    let text = "zebrafalcon roundtrip exported fact";
    let id = env.allow(text);
    assert!(env.list().contains(&(id.clone(), text.to_string())), "list shows the export");

    env.ok(&["gateway", "revoke", &id]);
    assert!(env.list().iter().all(|(i, _)| i != &id), "revoked id gone from list");
    assert!(env.preview("zebrafalcon")["results"].as_array().unwrap().is_empty());
}

#[test]
fn cli_allow_from_copies_and_keeps_original() {
    let env = Env::new("allowfrom");
    let text = "zebrafalcon original private note";
    let orig = env.ingest(text, "private");

    let copy = env.ok(&["gateway", "allow", "--from", &orig]);
    assert!(is_uuid(&copy), "allow --from prints a UUID: {copy:?}");
    assert_ne!(copy, orig, "copy must be a new memory, not a move");

    let list = env.list();
    assert!(list.contains(&(copy.clone(), text.to_string())), "copy listed with same text: {list:?}");
    assert!(list.iter().all(|(i, _)| i != &orig), "original must not be in export ns");
    assert_eq!(texts(env.preview("zebrafalcon")["results"].as_array().unwrap()), vec![text]);

    // Original still exists: after revoking the copy, it can be copied again.
    env.ok(&["gateway", "revoke", &copy]);
    let again = env.ok(&["gateway", "allow", "--from", &orig]);
    assert!(is_uuid(&again), "original survived allow --from + revoke of the copy");
}

#[test]
fn cli_revoke_refuses_non_export_id() {
    let env = Env::new("revokerefuse");
    let normal = env.ingest("zebrafalcon normal memory must survive", "shared");

    let out = env.run(&["gateway", "revoke", &normal]);
    assert!(!out.status.success(), "revoke of a non-export memory must fail");

    // Still exists → allow --from still works.
    let copy = env.ok(&["gateway", "allow", "--from", &normal]);
    assert!(is_uuid(&copy), "normal memory must not have been deleted");

    let bogus = "00000000-0000-4000-8000-000000000000";
    assert!(!env.run(&["gateway", "revoke", bogus]).status.success(), "unknown id → nonzero");
}

#[test]
fn cli_off_on_toggles_kill_switch_file() {
    let env = Env::new("offon");
    env.ok(&["gateway", "off"]);
    assert!(exists(&env.kill_switch()), "off creates gateway.disabled");
    env.ok(&["gateway", "on"]);
    assert!(!exists(&env.kill_switch()), "on removes gateway.disabled");
}

#[test]
fn serve_refuses_missing_or_short_token() {
    let env = Env::new("shorttoken");
    for token in [None, Some("short"), Some("0123456789abcdef0123456789abcde")] {
        // last one is 31 chars
        let mut c = env.cmd();
        c.args(["gateway", "serve", "--port", "0"])
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        if let Some(t) = token {
            c.env("CORTEX_GATEWAY_TOKEN", t);
        }
        let mut child = c.spawn().unwrap();
        let st = wait_exit(&mut child, Duration::from_secs(20));
        if st.is_none() {
            let _ = child.kill();
            let _ = child.wait();
        }
        let st = st.unwrap_or_else(|| panic!("serve kept running with token {token:?}"));
        assert!(!st.success(), "serve must exit nonzero with token {token:?}");
    }
}

// ─────────────────────────────── HTTP tests ───────────────────────────────

#[test]
fn http_privacy_gate_returns_only_export_namespace() {
    let env = Env::new("gate");
    let private = "zebrafalcon private diary entry";
    let shared = "zebrafalcon shared sync note";
    let public = "zebrafalcon public bio line";
    env.ingest(private, "private");
    env.ingest(shared, "shared");
    env.ingest(public, "public");
    let exported = "zebrafalcon exported for muse";
    env.allow(exported);

    let srv = env.serve(&[]);
    let (body, result) = recall_raw(srv.port, json!({"query": "zebrafalcon", "limit": 5}));
    assert!(!is_error(&result), "{body}");
    let results = parse_results(&result);
    assert_result_shape(&results);
    assert_eq!(texts(&results), vec![exported]);
    for leak in [private, shared, public, EXPORT_NS] {
        assert!(!body.contains(leak), "response leaked {leak:?}: {body}");
    }

    // preview shows exactly the same.
    let p = env.preview("zebrafalcon");
    let presults = p["results"].as_array().unwrap();
    assert_result_shape(presults);
    assert_eq!(texts(presults), vec![exported]);
}

#[test]
fn http_revoke_from_other_process_takes_effect_next_call() {
    let env = Env::new("revokelive");
    let keep = "zebrafalcon keep this one";
    let gone = "zebrafalcon revoke this one";
    env.allow(keep);
    let gone_id = env.allow(gone);

    let srv = env.serve(&[]);
    let before = texts(&recall(srv.port, "zebrafalcon", 5));
    assert!(before.contains(&gone.to_string()) && before.contains(&keep.to_string()), "{before:?}");

    env.ok(&["gateway", "revoke", &gone_id]);
    let (body, result) = recall_raw(srv.port, json!({"query": "zebrafalcon", "limit": 5}));
    assert!(!is_error(&result), "{body}");
    assert_eq!(texts(&parse_results(&result)), vec![keep]);
    assert!(!body.contains(gone), "revoked memory still served: {body}");
}

#[test]
fn http_kill_switch_checked_per_request() {
    let env = Env::new("killswitch");
    let text = "zebrafalcon killswitch payload";
    env.allow(text);
    let srv = env.serve(&[]);
    assert_eq!(texts(&recall(srv.port, "zebrafalcon", 3)), vec![text]);

    env.ok(&["gateway", "off"]);
    let (body, result) = recall_raw(srv.port, json!({"query": "zebrafalcon", "limit": 3}));
    assert!(is_error(&result), "kill switch must fail closed: {body}");
    assert!(!body.contains(text), "kill switch leaked text: {body}");

    env.ok(&["gateway", "on"]);
    assert_eq!(texts(&recall(srv.port, "zebrafalcon", 3)), vec![text], "works again without restart");
}

#[test]
fn http_daily_request_budget_enforced_and_persisted() {
    let env = Env::new("reqbudget");
    let text = "zebrafalcon request budget payload";
    env.allow(text);
    {
        let srv = env.serve(&["--daily-requests", "2"]);
        recall(srv.port, "zebrafalcon", 1);
        recall(srv.port, "zebrafalcon", 1);
        let (body, result) = recall_raw(srv.port, json!({"query": "zebrafalcon", "limit": 1}));
        assert!(is_error(&result), "3rd call over request budget must error: {body}");
        assert!(!body.contains(text), "over-budget call leaked: {body}");
    }
    assert!(exists(&env.state_file()), "budget persisted to gateway-state.json");
    // Restart: budget must not reset.
    let srv = env.serve(&["--daily-requests", "2"]);
    let (body, result) = recall_raw(srv.port, json!({"query": "zebrafalcon", "limit": 1}));
    assert!(is_error(&result), "restart must not reset request budget: {body}");
    assert!(!body.contains(text));
}

#[test]
fn http_daily_disclosure_budget_counts_distinct_and_persists() {
    let env = Env::new("discbudget");
    let a = "zebrafalcon alpha disclosure";
    let b = "quokkaneedle bravo disclosure";
    let c = "lynxharbor charlie disclosure";
    env.allow(a);
    env.allow(b);
    env.allow(c);
    let args = ["--daily-disclosures", "2", "--daily-requests", "100"];
    {
        let srv = env.serve(&args);
        assert_eq!(texts(&recall(srv.port, "zebrafalcon", 1)), vec![a]);
        assert_eq!(texts(&recall(srv.port, "zebrafalcon", 1)), vec![a], "re-disclosure is free");
        assert_eq!(texts(&recall(srv.port, "quokkaneedle", 1)), vec![b]);
        assert_eq!(texts(&recall(srv.port, "zebrafalcon", 1)), vec![a], "still free at cap");
        // Over the disclosure budget, unseen matches are withheld silently (an error
        // would itself reveal "a new memory matches" — a free search oracle).
        let (body, result) = recall_raw(srv.port, json!({"query": "lynxharbor", "limit": 1}));
        assert!(!is_error(&result) && parse_results(&result).is_empty(), "3rd distinct memory exceeds disclosure budget: {body}");
        assert!(!body.contains(c), "over-budget disclosure leaked: {body}");
    }
    let srv = env.serve(&args);
    let (body, result) = recall_raw(srv.port, json!({"query": "lynxharbor", "limit": 1}));
    assert!(!is_error(&result) && parse_results(&result).is_empty(), "restart must not reset disclosure budget: {body}");
    assert!(!body.contains(c));
    assert_eq!(
        texts(&recall(srv.port, "quokkaneedle", 1)),
        vec![b],
        "already-disclosed memory still free after restart"
    );
}

#[test]
fn http_auth_required() {
    let env = Env::new("auth");
    let text = "zebrafalcon auth payload";
    env.allow(text);
    let srv = env.serve(&[]);
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"recall_memory","arguments":{"query":"zebrafalcon"}}})
    .to_string();
    let wrong = format!("Bearer {}x", TOKEN);
    for hdrs in [
        vec![],
        vec![("Authorization", wrong.as_str())],
        vec![("Authorization", "Bearer ")],
        vec![("Authorization", TOKEN)],
    ] {
        let r = post_raw(srv.port, &hdrs, &body);
        assert_eq!(r.status, 401, "headers {hdrs:?} → body {}", r.body);
        assert!(!r.body.contains(text));
    }
    assert_eq!(post_authed(srv.port, &body).status, 200);
}

#[test]
fn http_origin_allowlist() {
    let env = Env::new("origin");
    let srv = env.serve(&["--allow-origin", "https://muse.example"]);
    let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string();
    let auth = format!("Bearer {TOKEN}");
    let evil = post_raw(srv.port, &[("Authorization", &auth), ("Origin", "https://evil.example")], &ping);
    assert_eq!(evil.status, 403, "{}", evil.body);
    let good = post_raw(srv.port, &[("Authorization", &auth), ("Origin", "https://muse.example")], &ping);
    assert_eq!(good.status, 200, "{}", good.body);
    assert_eq!(post_authed(srv.port, &ping).status, 200, "no Origin header is fine");
}

#[test]
fn http_method_path_size_and_batch_rules() {
    let env = Env::new("httprules");
    let srv = env.serve(&[]);
    let auth = format!("Bearer {TOKEN}");

    let get = http(srv.port, "GET", "/mcp", &[("Authorization", &auth)], b"");
    assert_eq!(get.status, 405, "GET /mcp");

    let ping = json!({"jsonrpc":"2.0","id":1,"method":"ping"}).to_string();
    let mut h: Vec<(&str, &str)> = JSON_HDRS.to_vec();
    h.push(("Authorization", &auth));
    for path in ["/", "/admin", "/mcp/extra", "/api/memories"] {
        let r = http(srv.port, "POST", path, &h, ping.as_bytes());
        assert_eq!(r.status, 404, "POST {path}");
    }

    // > 64 KiB body (valid-looking JSON).
    let big = format!(
        "{{\"jsonrpc\":\"2.0\",\"id\":1,\"method\":\"ping\",\"params\":{{\"pad\":\"{}\"}}}}",
        "a".repeat(70 * 1024)
    );
    let r = post_authed(srv.port, &big);
    assert_eq!(r.status, 413, "oversized body");

    // Batch array.
    let batch = json!([{"jsonrpc":"2.0","id":1,"method":"ping"},{"jsonrpc":"2.0","id":2,"method":"ping"}]).to_string();
    let r = post_authed(srv.port, &batch);
    assert_eq!(r.status, 400, "batch → 400, body={}", r.body);
    let v: Value = serde_json::from_str(&r.body).expect("batch error is JSON");
    assert_eq!(v["error"]["code"], -32600, "{}", r.body);
}

#[test]
fn http_initialize_ping_and_tools_list_shape() {
    let env = Env::new("shape");
    let srv = env.serve(&[]);

    let init = rpc(
        srv.port,
        "initialize",
        json!({"protocolVersion":"2025-03-26","capabilities":{},"clientInfo":{"name":"acc","version":"0"}}),
    );
    let r = &init["result"];
    assert!(r["protocolVersion"].is_string(), "{init}");
    assert!(r["capabilities"]["tools"].is_object(), "{init}");
    assert!(r["serverInfo"].is_object(), "{init}");

    let note = json!({"jsonrpc":"2.0","method":"notifications/initialized"}).to_string();
    let n = post_authed(srv.port, &note);
    assert_eq!(n.status, 202);
    assert!(n.body.trim().is_empty(), "202 has empty body: {:?}", n.body);

    let ping = rpc(srv.port, "ping", json!({}));
    assert_eq!(ping["result"], json!({}));

    let list = rpc(srv.port, "tools/list", json!({}));
    let tools = list["result"]["tools"].as_array().expect("tools array");
    assert_eq!(tools.len(), 1, "exactly one tool: {list}");
    let t = &tools[0];
    assert_eq!(t["name"], "recall_memory");
    let props = &t["inputSchema"]["properties"];
    assert_eq!(props["query"]["type"], "string");
    assert_eq!(props["query"]["maxLength"], 500);
    assert_eq!(props["limit"]["type"], "integer");
    assert_eq!(props["limit"]["minimum"], 1);
    assert_eq!(props["limit"]["maximum"], 5);
    let req = t["inputSchema"]["required"].as_array().expect("required");
    assert!(req.contains(&json!("query")));
}

#[test]
fn http_redacts_emails_and_caps_snippet_length() {
    let env = Env::new("redact");
    env.allow("emailowl reach me at alice.smith@example.com any time");
    let long = format!("longheron {}", "x".repeat(2000));
    env.allow(&long);
    let srv = env.serve(&[]);

    let (body, result) = recall_raw(srv.port, json!({"query": "emailowl", "limit": 1}));
    assert!(!body.contains("alice.smith@example.com"), "email leaked: {body}");
    let t = texts(&parse_results(&result));
    assert_eq!(t.len(), 1);
    assert!(t[0].contains("[redacted-email]"), "{t:?}");
    assert!(t[0].contains("emailowl"), "{t:?}");

    let t = texts(&recall(srv.port, "longheron", 1));
    assert_eq!(t.len(), 1);
    assert!(t[0].chars().count() <= 500, "snippet {} chars > 500", t[0].chars().count());

    // preview applies the same redaction and cap.
    let p = env.preview("emailowl").to_string();
    assert!(!p.contains("alice.smith@example.com") && p.contains("[redacted-email]"), "{p}");
    for r in env.preview("longheron")["results"].as_array().unwrap() {
        assert!(r["text"].as_str().unwrap().chars().count() <= 500);
    }
}

#[test]
fn http_bad_args_are_tool_errors() {
    let env = Env::new("badargs");
    let text = "zebrafalcon badargs payload";
    env.allow(text);
    let srv = env.serve(&[]);
    for args in [json!({}), json!({"query": 42}), json!({"limit": 3})] {
        let (body, result) = recall_raw(srv.port, args.clone());
        assert!(is_error(&result), "args {args} should be isError: {body}");
        assert!(!body.contains(text));
    }
}

#[test]
fn preview_does_not_charge_budget_or_audit() {
    let env = Env::new("preview");
    let text = "zebrafalcon preview payload";
    env.allow(text);
    for _ in 0..3 {
        assert_eq!(texts(env.preview("zebrafalcon")["results"].as_array().unwrap()), vec![text]);
    }
    assert!(audit_lines(&env).is_empty(), "preview must not write audit");
    assert!(env.ok(&["gateway", "audit"]).is_empty(), "audit command shows nothing yet");

    // A single-request, single-disclosure budget is still fully available.
    let srv = env.serve(&["--daily-requests", "1", "--daily-disclosures", "1"]);
    assert_eq!(texts(&recall(srv.port, "zebrafalcon", 1)), vec![text]);
    assert_eq!(audit_lines(&env).len(), 1, "exactly the one real call is audited");
}

#[test]
fn audit_records_metadata_without_query_or_snippet() {
    let env = Env::new("audit");
    let text = "zebrafalcon snippetpayloadmarker confidential";
    env.allow(text);
    let srv = env.serve(&[]);
    let results = recall(srv.port, "zebrafalcon secretquerytoken", 3);
    assert_eq!(texts(&results), vec![text]);

    let lines = audit_lines(&env);
    assert_eq!(lines.len(), 1, "one audit line per tools/call: {lines:?}");
    let l = &lines[0];
    for f in ["ts", "method", "tool", "n_results", "memory_ids", "bytes", "outcome"] {
        assert!(l.get(f).is_some(), "audit line missing {f}: {l}");
    }
    assert_eq!(l["tool"], "recall_memory");
    assert_eq!(l["n_results"], 1);
    assert_eq!(l["memory_ids"].as_array().map(Vec::len), Some(1));

    let raw_file = std::fs::read_to_string(env.audit_file()).unwrap();
    let printed = env.ok(&["gateway", "audit"]);
    assert!(!printed.is_empty(), "gateway audit prints the log");
    for hay in [&raw_file, &printed] {
        for secret in ["secretquerytoken", "snippetpayloadmarker", "zebrafalcon", "confidential"] {
            assert!(!hay.contains(secret), "audit leaked {secret:?}: {hay}");
        }
    }
}

/// Slow-body defense: the body has an absolute read deadline (~10 s), and an
/// unauthenticated request is refused before any body byte is read.
#[test]
fn trickled_body_is_cut_off_and_unauthenticated_never_waits() {
    use std::io::{Read, Write};
    use std::net::TcpStream;
    use std::time::{Duration, Instant};
    let env = Env::new("slowbody");
    let srv = env.serve(&[]);

    // Unauthenticated: 401 immediately even though the body never arrives.
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n").unwrap();
    let mut buf = [0u8; 64];
    let n = s.read(&mut buf).unwrap();
    assert!(String::from_utf8_lossy(&buf[..n]).contains("401"));

    // Authenticated trickle: one byte per second never completes; cut off by the deadline.
    let mut s = TcpStream::connect(("127.0.0.1", srv.port)).unwrap();
    s.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
    write!(s, "POST /mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {TOKEN}\r\nContent-Type: application/json\r\nContent-Length: 100\r\n\r\n").unwrap();
    let start = Instant::now();
    let mut response = String::new();
    while start.elapsed() < Duration::from_secs(20) {
        let _ = s.write_all(b" ");
        match s.read(&mut buf) {
            Ok(n) if n > 0 => {
                response.push_str(&String::from_utf8_lossy(&buf[..n]));
                break;
            }
            _ => std::thread::sleep(Duration::from_millis(800)),
        }
    }
    assert!(response.contains("408"), "expected 408, got {response:?} after {:?}", start.elapsed());
    assert!(start.elapsed() < Duration::from_secs(15));
}
