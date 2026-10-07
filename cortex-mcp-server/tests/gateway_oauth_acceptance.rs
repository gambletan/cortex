//! Black-box acceptance tests for the Muse gateway's OAuth 2.1 authorization server
//! (`cortex-mcp-server gateway serve --oauth`, v2.5).
//!
//! Written by a context-isolated agent from `docs/design/muse-oauth.md` (plus
//! `muse-gateway.md`, `muse-remember.md`, `docs/muse.md`) only — never from the
//! implementation or its unit tests. Drives the real binary: CLI subcommands
//! (`gateway connect/clients/disconnect/off/on/allow`) + raw HTTP/1.1 against `gateway serve`.
//!
//! The tests run on loopback over plain http; the issuer is `--public-url https://gw.example.test`
//! (we never resolve it — redirects are read from `Location`, never followed).
//!
//! No extra dev-dependencies: SHA-256 / base64 for PKCE are implemented at the bottom of this
//! file (validated against `openssl dgst -sha256` and the FIPS 180-2 "abc" vector).
//!
//! Not covered (would need real waiting): code expiry after 60 s, pending TTL 10 min,
//! access-token 1 h expiry, rate-limit windows.
#![cfg(feature = "gateway")]

use serde_json::{json, Value};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::path::PathBuf;
use std::process::{Child, Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Barrier};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_cortex-mcp-server");
const ISSUER: &str = "https://gw.example.test";
const RESOURCE: &str = "https://gw.example.test/mcp";
const PRM_URL: &str = "https://gw.example.test/.well-known/oauth-protected-resource/mcp";
const MUSE_CB: &str = "https://agent.meta.ai/api/hatch/oauth/callback";
const STATIC_TOKEN: &str = "acceptance-oauth-static-token-0123456789abcdefghijklmnop";
const OAUTH_ARGS: [&str; 3] = ["--oauth", "--public-url", ISSUER];

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
            "cortex-gw-oauth-{}-{}-{}-{}",
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
    fn oauth_file(&self) -> PathBuf {
        self.dir.join("gateway-oauth.json")
    }
    fn oauth_lock(&self) -> PathBuf {
        self.dir.join("gateway-oauth.lock")
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

    /// Run and require success; returns stdout (lossy).
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
        String::from_utf8_lossy(&out.stdout).to_string()
    }

    /// Run with `input` written to stdin (then EOF). Bounded: a CLI that blocks
    /// (e.g. reading /dev/tty instead of stdin) fails the test instead of hanging it.
    fn run_stdin(&self, args: &[&str], input: &str) -> Output {
        let mut child = self
            .cmd()
            .args(args)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn cortex-mcp-server");
        {
            let mut si = child.stdin.take().unwrap();
            let _ = si.write_all(input.as_bytes());
        } // dropped → EOF
        let mut so = child.stdout.take().unwrap();
        let mut se = child.stderr.take().unwrap();
        let to = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = so.read_to_end(&mut v);
            v
        });
        let te = std::thread::spawn(move || {
            let mut v = Vec::new();
            let _ = se.read_to_end(&mut v);
            v
        });
        let status = match wait_exit(&mut child, Duration::from_secs(60)) {
            Some(s) => s,
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("{args:?} did not exit within 60 s (blocked on input?)");
            }
        };
        Output { status, stdout: to.join().unwrap(), stderr: te.join().unwrap() }
    }

    /// `gateway allow <text>` (export a memory so recall_memory has something to find).
    fn allow(&self, text: &str) {
        self.ok(&["gateway", "allow", text]);
    }

    fn clients(&self) -> String {
        self.ok(&["gateway", "clients"])
    }

    /// `gateway serve` with arbitrary flags; returns once it reports its port.
    fn serve(&self, extra: &[&str], static_token: Option<&str>) -> Server {
        let mut args = vec!["gateway", "serve", "--port", "0"];
        args.extend_from_slice(extra);
        let mut c = self.cmd();
        c.args(&args).stdout(Stdio::null()).stderr(Stdio::piped());
        if let Some(t) = static_token {
            c.env("CORTEX_GATEWAY_TOKEN", t);
        }
        let mut child = c.spawn().expect("spawn gateway serve");
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
                Err(_) => panic!("gateway serve {:?} never reported its port; stderr:\n{}", extra, seen),
            }
        }
    }

    /// OAuth-only gateway (no static token configured).
    fn serve_oauth(&self) -> Server {
        self.serve(&OAUTH_ARGS, None)
    }

    /// `serve` with these flags must refuse to start (exit non-zero, never listen).
    fn serve_must_fail(&self, extra: &[&str]) {
        let mut args = vec!["gateway", "serve", "--port", "0"];
        args.extend_from_slice(extra);
        let mut child = self
            .cmd()
            .args(&args)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn gateway serve");
        match wait_exit(&mut child, Duration::from_secs(30)) {
            Some(st) => assert!(!st.success(), "serve {extra:?} must exit non-zero, got {st:?}"),
            None => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("serve {extra:?} must refuse to start, but it kept running");
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

fn combined(out: &Output) -> String {
    format!("{}\n{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr))
}

fn rand_str() -> String {
    let n = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let c = COUNTER.fetch_add(1, Ordering::SeqCst);
    b64url(&sha256(format!("{n}-{c}-{}-oauth-acc", std::process::id()).as_bytes()))
}

// ───────────────────────────── HTTP helpers ─────────────────────────────

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(k, _)| k.eq_ignore_ascii_case(name))
            .map(|(_, v)| v.as_str())
    }
    fn location(&self) -> Option<&str> {
        self.header("location")
    }
    fn is_redirect(&self) -> bool {
        matches!(self.status, 302 | 303)
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("expected JSON body ({e}); status={} body={}", self.status, self.body))
    }
    /// OAuth `error` field of a JSON error response ("" if absent / not JSON).
    fn oauth_error(&self) -> String {
        serde_json::from_str::<Value>(&self.body)
            .ok()
            .and_then(|v| v.get("error").and_then(Value::as_str).map(str::to_string))
            .unwrap_or_default()
    }
    fn dump(&self) -> String {
        format!("status={} headers={:?} body={}", self.status, self.headers, self.body)
    }
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
    let mut lines = head.split("\r\n");
    let status_line = lines.next().unwrap_or("");
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse().ok())
        .unwrap_or_else(|| panic!("bad status line: {status_line}"));
    let headers: Vec<(String, String)> = lines
        .filter_map(|l| l.split_once(':'))
        .map(|(k, v)| (k.trim().to_ascii_lowercase(), v.trim().to_string()))
        .collect();
    let chunked = headers
        .iter()
        .any(|(k, v)| k == "transfer-encoding" && v.to_ascii_lowercase().contains("chunked"));
    let body = if chunked { dechunk(rest) } else { rest.to_string() };
    Resp { status, headers, body }
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

fn get(port: u16, path: &str) -> Resp {
    http(port, "GET", path, &[], b"")
}

fn pct(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{:02X}", b));
        }
    }
    out
}

fn pct_decode(s: &str) -> String {
    fn hex(c: u8) -> Option<u8> {
        (c as char).to_digit(16).map(|d| d as u8)
    }
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'+' {
            out.push(b' ');
            i += 1;
            continue;
        }
        if b[i] == b'%' && i + 2 < b.len() {
            if let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2])) {
                out.push(h * 16 + l);
                i += 3;
                continue;
            }
        }
        out.push(b[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

fn form(pairs: &[(&str, &str)]) -> String {
    pairs
        .iter()
        .map(|(k, v)| format!("{}={}", pct(k), pct(v)))
        .collect::<Vec<_>>()
        .join("&")
}

/// Split a redirect `Location` into (base URI, decoded query params).
fn split_location(loc: &str) -> (String, HashMap<String, String>) {
    let loc = loc.split('#').next().unwrap_or(loc);
    let (base, q) = loc.split_once('?').unwrap_or((loc, ""));
    let mut m = HashMap::new();
    for kv in q.split('&').filter(|s| !s.is_empty()) {
        let (k, v) = kv.split_once('=').unwrap_or((kv, ""));
        m.insert(pct_decode(k), pct_decode(v));
    }
    (base.to_string(), m)
}

fn redirect_params(r: &Resp) -> (String, HashMap<String, String>) {
    assert!(r.is_redirect(), "expected a redirect: {}", r.dump());
    split_location(r.location().unwrap_or_else(|| panic!("redirect without Location: {}", r.dump())))
}

fn assert_no_store(r: &Resp, what: &str) {
    let cc = r.header("cache-control").unwrap_or("").to_ascii_lowercase();
    assert!(cc.contains("no-store"), "{what}: Cache-Control must contain no-store: {}", r.dump());
}

// ───────────────────────────── OAuth helpers ─────────────────────────────

fn register(port: u16, meta: &Value) -> Resp {
    http(
        port,
        "POST",
        "/register",
        &[("Content-Type", "application/json"), ("Accept", "application/json")],
        meta.to_string().as_bytes(),
    )
}

fn is_2xx(s: u16) -> bool {
    (200..300).contains(&s)
}

/// DCR a public (PKCE-only) client for the Muse callback; returns client_id.
fn register_public(port: u16, name: &str) -> String {
    register_public_for(port, name, MUSE_CB)
}

fn register_public_for(port: u16, name: &str, redirect: &str) -> String {
    let r = register(
        port,
        &json!({
            "client_name": name,
            "redirect_uris": [redirect],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]
        }),
    );
    assert!(is_2xx(r.status), "register public client failed: {}", r.dump());
    r.json()["client_id"].as_str().expect("client_id string").to_string()
}

struct Pkce {
    verifier: String,
    challenge: String,
}

fn pkce() -> Pkce {
    let verifier = rand_str(); // 43 chars of base64url — valid RFC 7636 verifier
    let challenge = b64url(&sha256(verifier.as_bytes()));
    Pkce { verifier, challenge }
}

fn std_params<'a>(client_id: &'a str, redirect: &'a str, challenge: &'a str, state: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("response_type", "code"),
        ("client_id", client_id),
        ("redirect_uri", redirect),
        ("code_challenge", challenge),
        ("code_challenge_method", "S256"),
        ("state", state),
        ("scope", "memory"),
        ("resource", RESOURCE),
    ]
}

/// Replace (Some) or remove (None) a parameter.
fn set_param<'a>(v: &mut Vec<(&'a str, &'a str)>, key: &'a str, val: Option<&'a str>) {
    v.retain(|(k, _)| *k != key);
    if let Some(x) = val {
        v.push((key, x));
    }
}

fn authorize(port: u16, params: &[(&str, &str)]) -> Resp {
    get(port, &format!("/authorize?{}", form(params)))
}

/// A started authorization: the display code shown in the page and the `req` capability.
struct Pending {
    display: String,
    req: String,
    page: Resp,
}

fn extract_display_code(html: &str) -> Option<String> {
    let b = html.as_bytes();
    let is_c = |c: u8| c.is_ascii_uppercase() || c.is_ascii_digit();
    let mut cands: Vec<(usize, String)> = Vec::new();
    if b.len() >= 9 {
        for i in 0..=b.len() - 9 {
            if b[i + 4] == b'-'
                && b[i..i + 4].iter().all(|c| is_c(*c))
                && b[i + 5..i + 9].iter().all(|c| is_c(*c))
                && (i == 0 || !b[i - 1].is_ascii_alphanumeric())
                && (i + 9 == b.len() || !b[i + 9].is_ascii_alphanumeric())
            {
                cands.push((i, html[i..i + 9].to_string()));
            }
        }
    }
    // Prefer the one in the printed command (`… gateway connect XXXX-XXXX`).
    cands
        .iter()
        .find(|(i, _)| html[..*i].trim_end().ends_with("connect"))
        .or_else(|| cands.first())
        .map(|(_, c)| c.clone())
}

fn extract_req(html: &str) -> Option<String> {
    let marker = "/authorize/wait?req=";
    let idx = html.find(marker)? + marker.len();
    let req: String = html[idx..]
        .chars()
        .take_while(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '~' | '%'))
        .collect();
    if req.is_empty() {
        None
    } else {
        Some(req)
    }
}

fn begin(port: u16, params: &[(&str, &str)]) -> Pending {
    let page = authorize(port, params);
    assert_eq!(page.status, 200, "authorize should show the code page: {}", page.dump());
    let display = extract_display_code(&page.body)
        .unwrap_or_else(|| panic!("no XXXX-XXXX display code in authorize page: {}", page.body));
    let req = extract_req(&page.body)
        .unwrap_or_else(|| panic!("no /authorize/wait?req= refresh URL in page: {}", page.body));
    Pending { display, req, page }
}

fn connect(env: &Env, code: &str, answer: &str) -> Output {
    env.run_stdin(&["gateway", "connect", code], answer)
}

fn poll(port: u16, req: &str) -> Resp {
    get(port, &format!("/authorize/wait?req={req}"))
}

/// A wait-page response that delivers no code: either the 400 page or an
/// `error=access_denied` redirect.
fn assert_no_code(r: &Resp, what: &str) {
    if r.is_redirect() {
        let (_, q) = redirect_params(r);
        assert!(!q.contains_key("code"), "{what}: must not deliver a code: {}", r.dump());
        assert_eq!(q.get("error").map(String::as_str), Some("access_denied"), "{what}: {}", r.dump());
    } else {
        assert_eq!(r.status, 400, "{what}: expected 400 (nothing pending): {}", r.dump());
    }
}

/// authorize → `connect` with y → poll → code (asserting state + iss on the redirect).
fn obtain_code_with(env: &Env, port: u16, client_id: &str, redirect: &str, scope: Option<&str>) -> (String, Pkce) {
    let p = pkce();
    let state = rand_str();
    let mut params = std_params(client_id, redirect, &p.challenge, &state);
    set_param(&mut params, "scope", scope);
    let pend = begin(port, &params);
    let out = connect(env, &pend.display, "y\n");
    assert!(out.status.success(), "connect y failed: {}", combined(&out));
    let r = poll(port, &pend.req);
    assert_eq!(r.status, 302, "approved poll must 302: {}", r.dump());
    let (base, q) = redirect_params(&r);
    assert_eq!(base, redirect, "code goes to the registered redirect");
    assert_eq!(q.get("state"), Some(&state), "state echoed: {}", r.dump());
    assert_eq!(q.get("iss").map(String::as_str), Some(ISSUER), "iss param: {}", r.dump());
    let code = q.get("code").cloned().unwrap_or_else(|| panic!("no code in redirect: {}", r.dump()));
    (code, p)
}

fn obtain_code(env: &Env, port: u16, client_id: &str) -> (String, Pkce) {
    obtain_code_with(env, port, client_id, MUSE_CB, Some("memory"))
}

fn token(port: u16, pairs: &[(&str, &str)], extra_headers: &[(&str, &str)]) -> Resp {
    let mut h = vec![("Content-Type", "application/x-www-form-urlencoded"), ("Accept", "application/json")];
    h.extend_from_slice(extra_headers);
    http(port, "POST", "/token", &h, form(pairs).as_bytes())
}

fn token_code(port: u16, client_id: &str, code: &str, verifier: &str) -> Resp {
    token(
        port,
        &[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", MUSE_CB),
            ("client_id", client_id),
            ("code_verifier", verifier),
            ("resource", RESOURCE),
        ],
        &[],
    )
}

fn token_refresh(port: u16, client_id: &str, refresh: &str) -> Resp {
    token(
        port,
        &[("grant_type", "refresh_token"), ("refresh_token", refresh), ("client_id", client_id), ("resource", RESOURCE)],
        &[],
    )
}

struct Tokens {
    access: String,
    refresh: String,
    body: Value,
}

fn tokens_ok(r: &Resp) -> Tokens {
    assert_eq!(r.status, 200, "token request should succeed: {}", r.dump());
    assert_no_store(r, "/token");
    let body = r.json();
    let access = body["access_token"].as_str().expect("access_token").to_string();
    let refresh = body["refresh_token"].as_str().expect("refresh_token").to_string();
    assert!(
        body["token_type"].as_str().map(|t| t.eq_ignore_ascii_case("bearer")) == Some(true),
        "token_type Bearer: {body}"
    );
    assert_eq!(body["scope"], "memory", "grant scope is exactly memory: {body}");
    Tokens { access, refresh, body }
}

/// Full grant for a public client: code + PKCE exchange.
fn grant(env: &Env, port: u16, client_id: &str) -> Tokens {
    let (code, p) = obtain_code(env, port, client_id);
    tokens_ok(&token_code(port, client_id, &code, &p.verifier))
}

fn mcp(port: u16, bearer: Option<&str>, method: &str, params: Value) -> Resp {
    let body = json!({"jsonrpc": "2.0", "id": 1, "method": method, "params": params}).to_string();
    let auth = bearer.map(|b| format!("Bearer {b}"));
    let mut h: Vec<(&str, &str)> =
        vec![("Content-Type", "application/json"), ("Accept", "application/json, text/event-stream")];
    if let Some(a) = auth.as_deref() {
        h.push(("Authorization", a));
    }
    http(port, "POST", "/mcp", &h, body.as_bytes())
}

fn tools_list(port: u16, bearer: &str) -> Resp {
    mcp(port, Some(bearer), "tools/list", json!({}))
}

fn recall(port: u16, bearer: &str, query: &str) -> Resp {
    mcp(
        port,
        Some(bearer),
        "tools/call",
        json!({"name": "recall_memory", "arguments": {"query": query, "limit": 5}}),
    )
}

/// True iff the MCP call succeeded (HTTP 200, JSON-RPC result, not a tool error).
fn mcp_ok(r: &Resp) -> bool {
    if r.status != 200 {
        return false;
    }
    match serde_json::from_str::<Value>(&r.body) {
        Ok(v) => {
            v.get("error").is_none()
                && v.get("result").is_some()
                && v["result"].get("isError").and_then(Value::as_bool) != Some(true)
        }
        Err(_) => false,
    }
}

fn recall_texts(r: &Resp) -> Vec<String> {
    assert!(mcp_ok(r), "recall should succeed: {}", r.dump());
    let v = r.json();
    let text = v["result"]["content"][0]["text"].as_str().expect("content text").to_string();
    let inner: Value = serde_json::from_str(&text).expect("content text is JSON");
    inner["results"]
        .as_array()
        .expect("results array")
        .iter()
        .map(|x| x["text"].as_str().unwrap_or_default().to_string())
        .collect()
}

/// Find the `gateway clients` line for `name` and revoke that grant by id.
/// The id column's exact position is not specified, so try each token of the line
/// that is not part of the client name; accept the first whose `disconnect` succeeds
/// AND makes the grant disappear from `gateway clients`.
fn disconnect_by_name(env: &Env, name: &str) {
    let listing = env.clients();
    let line = listing
        .lines()
        .find(|l| l.contains(name))
        .unwrap_or_else(|| panic!("`gateway clients` has no line for {name}: {listing}"))
        .to_string();
    for tok in line.split(|c: char| c.is_whitespace() || matches!(c, ',' | '|' | ';')) {
        let t = tok.trim_matches(|c: char| !(c.is_ascii_alphanumeric() || c == '-' || c == '_'));
        if t.len() < 4 || name.contains(t) {
            continue;
        }
        let o = env.run(&["gateway", "disconnect", t]);
        if o.status.success() && !env.clients().contains(name) {
            return;
        }
    }
    panic!("could not disconnect the grant listed as: {line}");
}

// ──────────────────────────────── tests ────────────────────────────────

#[test]
fn pkce_helper_is_correct() {
    // Sanity of this file's own SHA-256/base64url (checked against openssl).
    let hex: String = sha256(b"abc").iter().map(|b| format!("{b:02x}")).collect();
    assert_eq!(hex, "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad");
    assert_eq!(
        b64url(&sha256(b"dBjftJeZ4CVP-mJ92K1qnAzkcz8wiDIozr9pFhgM6wE")),
        "rwARpSXDtZ5wluxlqfIG9oSW6Qiyx0NHE5vHuw0wagE"
    );
    assert_eq!(b64(b"foob", false), "Zm9vYg==");
    assert_eq!(pkce().verifier.len(), 43);
}

// 1. Discovery
#[test]
fn discovery_metadata_matches_spec() {
    let env = Env::new("discovery");
    let srv = env.serve_oauth();
    let port = srv.port;

    let a = get(port, "/.well-known/oauth-protected-resource/mcp");
    let b = get(port, "/.well-known/oauth-protected-resource");
    assert_eq!(a.status, 200, "{}", a.dump());
    assert_eq!(b.status, 200, "{}", b.dump());
    let (ja, jb) = (a.json(), b.json());
    assert_eq!(ja, jb, "both protected-resource metadata paths must be identical");
    assert_eq!(ja["resource"], RESOURCE);
    assert_eq!(ja["authorization_servers"], json!([ISSUER]));
    assert_eq!(ja["bearer_methods_supported"], json!(["header"]));
    assert_eq!(ja["scopes_supported"], json!(["memory"]));

    let m = get(port, "/.well-known/oauth-authorization-server");
    assert_eq!(m.status, 200, "{}", m.dump());
    let m = m.json();
    assert_eq!(m["issuer"], ISSUER);
    assert_eq!(m["authorization_endpoint"], format!("{ISSUER}/authorize"));
    assert_eq!(m["token_endpoint"], format!("{ISSUER}/token"));
    assert_eq!(m["registration_endpoint"], format!("{ISSUER}/register"));
    assert_eq!(m["scopes_supported"], json!(["memory"]));
    assert_eq!(m["response_types_supported"], json!(["code"]));
    let mut grants: Vec<String> =
        m["grant_types_supported"].as_array().expect("grant_types").iter().map(|v| v.as_str().unwrap().to_string()).collect();
    grants.sort();
    assert_eq!(grants, vec!["authorization_code", "refresh_token"]);
    assert_eq!(m["code_challenge_methods_supported"], json!(["S256"]), "S256 only, never plain");
    let mut methods: Vec<String> = m["token_endpoint_auth_methods_supported"]
        .as_array()
        .expect("auth methods")
        .iter()
        .map(|v| v.as_str().unwrap().to_string())
        .collect();
    methods.sort();
    assert_eq!(methods, vec!["client_secret_basic", "client_secret_post", "none"]);
    assert_eq!(m["authorization_response_iss_parameter_supported"], json!(true));

    let oidc = get(port, "/.well-known/openid-configuration");
    assert_eq!(oidc.status, 404, "no openid-configuration: {}", oidc.dump());
}

// 2. 401 challenge
#[test]
fn mcp_401_challenge_points_at_resource_metadata() {
    let env = Env::new("challenge");
    let srv = env.serve_oauth();

    let r = mcp(srv.port, None, "tools/list", json!({}));
    assert_eq!(r.status, 401, "{}", r.dump());
    let wa = r.header("www-authenticate").unwrap_or_else(|| panic!("no WWW-Authenticate: {}", r.dump()));
    assert!(wa.trim_start().to_ascii_lowercase().starts_with("bearer"), "{wa}");
    assert!(wa.contains(&format!("resource_metadata=\"{PRM_URL}\"")), "resource_metadata → /mcp-suffixed PRM: {wa}");
    assert!(wa.contains("scope=\"memory\""), "scope=\"memory\": {wa}");

    let r = mcp(srv.port, Some("not-a-real-token-0123456789abcdef0123456789"), "tools/list", json!({}));
    assert_eq!(r.status, 401, "{}", r.dump());
    let wa = r.header("www-authenticate").unwrap_or("");
    assert!(wa.contains("error=\"invalid_token\""), "presented token → invalid_token: {wa}");
    assert!(wa.contains(&format!("resource_metadata=\"{PRM_URL}\"")), "{wa}");
}

// 3. Full happy path + refresh rotation + replay revocation
#[test]
fn full_muse_flow_then_refresh_replay_revokes_grant() {
    let env = Env::new("full");
    env.allow("zebrafalcon oauth exported fact");
    let srv = env.serve_oauth();
    let port = srv.port;

    // Discover (as Muse would).
    let prm = get(port, "/.well-known/oauth-protected-resource/mcp").json();
    assert_eq!(prm["authorization_servers"][0], ISSUER);

    // DCR, public client.
    let r = register(
        port,
        &json!({
            "client_name": "Muse",
            "redirect_uris": [MUSE_CB],
            "token_endpoint_auth_method": "none",
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"]
        }),
    );
    assert!(is_2xx(r.status), "{}", r.dump());
    assert_no_store(&r, "/register");
    let reg = r.json();
    assert_eq!(reg["token_endpoint_auth_method"], "none", "{reg}");
    assert!(reg.get("client_secret").is_none_or(Value::is_null), "no secret for auth method none: {reg}");
    assert_eq!(reg["redirect_uris"], json!([MUSE_CB]));
    let client_id = reg["client_id"].as_str().expect("client_id").to_string();

    // Authorize.
    let p = pkce();
    let state = rand_str();
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, &state));
    assert!(pend.page.body.contains(&pend.display));
    assert!(
        pend.page.header("content-type").unwrap_or("").to_ascii_lowercase().contains("text/html"),
        "{}",
        pend.page.dump()
    );

    // Waiting: same page, no redirect, no code.
    let w = poll(port, &pend.req);
    assert_eq!(w.status, 200, "waiting poll shows the page: {}", w.dump());
    assert!(w.location().is_none());

    // Local approval.
    let out = connect(&env, &pend.display, "y\n");
    let text = combined(&out);
    assert!(out.status.success(), "connect y: {text}");
    assert!(text.contains(MUSE_CB), "connect shows the full redirect URI: {text}");
    assert!(text.contains(RESOURCE), "connect shows the resource: {text}");
    assert!(text.to_ascii_lowercase().contains("unverified"), "client name labelled unverified: {text}");

    // Code delivery.
    let r = poll(port, &pend.req);
    assert_eq!(r.status, 302, "{}", r.dump());
    let (base, q) = redirect_params(&r);
    assert_eq!(base, MUSE_CB);
    assert_eq!(q.get("state"), Some(&state));
    assert_eq!(q.get("iss").map(String::as_str), Some(ISSUER));
    let code = q.get("code").cloned().expect("code");

    // Token exchange with PKCE.
    let t1 = tokens_ok(&token_code(port, &client_id, &code, &p.verifier));
    if let Some(exp) = t1.body.get("expires_in").and_then(Value::as_i64) {
        assert!(exp > 0 && exp <= 3600, "access lifetime ≤ 1 h: {}", t1.body);
    }

    // MCP with the access token.
    let tl = tools_list(port, &t1.access);
    assert!(mcp_ok(&tl), "tools/list with OAuth token: {}", tl.dump());
    assert!(tl.body.contains("recall_memory"), "{}", tl.body);
    let texts = recall_texts(&recall(port, &t1.access, "zebrafalcon"));
    assert!(texts.iter().any(|t| t.contains("zebrafalcon oauth exported fact")), "{texts:?}");

    // Refresh rotates both tokens.
    let t2 = tokens_ok(&token_refresh(port, &client_id, &t1.refresh));
    assert_ne!(t2.access, t1.access);
    assert_ne!(t2.refresh, t1.refresh);
    assert!(mcp_ok(&tools_list(port, &t2.access)), "rotated access works");
    assert_eq!(tools_list(port, &t1.access).status, 401, "old access token dies on rotation");

    // Replay of the used refresh token → invalid_grant and the whole grant is revoked.
    let replay = token_refresh(port, &client_id, &t1.refresh);
    assert_eq!(replay.status, 400, "{}", replay.dump());
    assert_eq!(replay.oauth_error(), "invalid_grant", "{}", replay.dump());
    assert_eq!(tools_list(port, &t2.access).status, 401, "replay revokes the grant's live access token");
    let r2 = token_refresh(port, &client_id, &t2.refresh);
    assert_eq!(r2.status, 400, "{}", r2.dump());
    assert_eq!(r2.oauth_error(), "invalid_grant", "replay revokes the grant's live refresh token");
}

// 4. Default auth method = client_secret_basic, issued secret, enforced
#[test]
fn register_default_auth_method_is_client_secret_basic_and_enforced() {
    let env = Env::new("basic");
    let srv = env.serve_oauth();
    let port = srv.port;

    let r = register(port, &json!({"client_name": "Muse", "redirect_uris": [MUSE_CB]}));
    assert!(is_2xx(r.status), "{}", r.dump());
    let reg = r.json();
    assert_eq!(reg["token_endpoint_auth_method"], "client_secret_basic", "RFC 7591 default: {reg}");
    let client_id = reg["client_id"].as_str().expect("client_id").to_string();
    let secret = reg["client_secret"].as_str().expect("a secret is issued").to_string();
    assert!(!secret.is_empty());

    let basic = format!("Basic {}", b64(format!("{}:{}", pct(&client_id), pct(&secret)).as_bytes(), false));
    let bad_basic = format!("Basic {}", b64(format!("{}:{}", pct(&client_id), "wrong-secret").as_bytes(), false));

    // Failed client authentication attempts (on code #1).
    let (code1, p1) = obtain_code(&env, port, &client_id);
    let base = [
        ("grant_type", "authorization_code"),
        ("code", code1.as_str()),
        ("redirect_uri", MUSE_CB),
        ("code_verifier", p1.verifier.as_str()),
    ];
    let mut no_secret = base.to_vec();
    no_secret.push(("client_id", client_id.as_str()));
    let r = token(port, &no_secret, &[]);
    assert_eq!(r.status, 401, "no secret → 401: {}", r.dump());
    assert_eq!(r.oauth_error(), "invalid_client", "{}", r.dump());

    let r = token(port, &base, &[("Authorization", bad_basic.as_str())]);
    assert_eq!(r.status, 401, "wrong secret → 401: {}", r.dump());
    assert_eq!(r.oauth_error(), "invalid_client", "{}", r.dump());

    // No downgrade: a basic client may not authenticate with client_secret_post.
    let mut post = no_secret.clone();
    post.push(("client_secret", secret.as_str()));
    let r = token(port, &post, &[]);
    assert_eq!(r.status, 401, "registered method is enforced (post ≠ basic): {}", r.dump());
    assert_eq!(r.oauth_error(), "invalid_client", "{}", r.dump());

    // Correct HTTP Basic on a fresh code works.
    let (code2, p2) = obtain_code(&env, port, &client_id);
    let r = token(
        port,
        &[
            ("grant_type", "authorization_code"),
            ("code", code2.as_str()),
            ("redirect_uri", MUSE_CB),
            ("code_verifier", p2.verifier.as_str()),
        ],
        &[("Authorization", basic.as_str())],
    );
    let t = tokens_ok(&r);
    assert!(mcp_ok(&tools_list(port, &t.access)));

    // Refresh also requires the client secret.
    let r = token_refresh(port, &client_id, &t.refresh);
    assert_eq!(r.status, 401, "refresh without secret → 401: {}", r.dump());
    let r = token(
        port,
        &[("grant_type", "refresh_token"), ("refresh_token", t.refresh.as_str())],
        &[("Authorization", basic.as_str())],
    );
    tokens_ok(&r);
}

// 5a. Registration validation
#[test]
fn register_rejects_bad_metadata() {
    let env = Env::new("regbad");
    let srv = env.serve_oauth();
    let port = srv.port;
    let cases = [
        ("redirect not allowlisted", json!({"redirect_uris": ["https://evil.example/cb"], "token_endpoint_auth_method": "none"})),
        ("redirect missing", json!({"client_name": "x", "token_endpoint_auth_method": "none"})),
        ("redirect with fragment", json!({"redirect_uris": [format!("{MUSE_CB}#frag")], "token_endpoint_auth_method": "none"})),
        ("too many redirects", json!({"redirect_uris": [MUSE_CB, MUSE_CB, MUSE_CB, MUSE_CB, MUSE_CB, MUSE_CB], "token_endpoint_auth_method": "none"})),
        ("bad grant type", json!({"redirect_uris": [MUSE_CB], "grant_types": ["client_credentials"], "token_endpoint_auth_method": "none"})),
        ("bad response type", json!({"redirect_uris": [MUSE_CB], "response_types": ["token"], "token_endpoint_auth_method": "none"})),
        ("bad auth method", json!({"redirect_uris": [MUSE_CB], "token_endpoint_auth_method": "private_key_jwt"})),
    ];
    for (what, meta) in cases.iter() {
        let r = register(port, meta);
        assert_eq!(r.status, 400, "{what}: must be rejected with 400: {}", r.dump());
        assert!(!r.oauth_error().is_empty(), "{what}: OAuth error body: {}", r.dump());
        assert!(r.json().get("client_id").is_none(), "{what}: no client issued");
    }
    // Unsupported grant / response types are invalid_client_metadata.
    let r = register(port, &json!({"redirect_uris": [MUSE_CB], "grant_types": ["implicit"], "token_endpoint_auth_method": "none"}));
    assert_eq!(r.oauth_error(), "invalid_client_metadata", "{}", r.dump());
    // Oversized body (> 8 KiB) never registers.
    let big = json!({"redirect_uris": [MUSE_CB], "token_endpoint_auth_method": "none", "client_name": "x", "pad": "a".repeat(9000)});
    let r = register(port, &big);
    assert!(!is_2xx(r.status), "> 8 KiB registration must be refused: {}", r.dump());
}

// 5b. Never redirect to an unvalidated client/URI
#[test]
fn authorize_never_redirects_for_unknown_client_or_unregistered_uri() {
    let env = Env::new("noredirect");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let p = pkce();

    let unknown = std_params("no-such-client-id", MUSE_CB, &p.challenge, "s1");
    let r = authorize(port, &unknown);
    assert_eq!(r.status, 400, "unknown client → HTML 400: {}", r.dump());
    assert!(r.location().is_none(), "never redirect for an unknown client: {}", r.dump());

    let evil = std_params(&client_id, "https://evil.example/cb", &p.challenge, "s2");
    let r = authorize(port, &evil);
    assert_eq!(r.status, 400, "unregistered redirect → HTML 400: {}", r.dump());
    assert!(r.location().is_none(), "never redirect to an unregistered URI: {}", r.dump());

    // Unknown client AND missing PKCE: client validation comes first → still no redirect.
    let mut both = std_params("no-such-client-id", MUSE_CB, &p.challenge, "s3");
    set_param(&mut both, "code_challenge", None);
    let r = authorize(port, &both);
    assert_eq!(r.status, 400, "{}", r.dump());
    assert!(r.location().is_none(), "{}", r.dump());

    // Missing redirect_uri entirely → no redirect either.
    let mut none = std_params(&client_id, MUSE_CB, &p.challenge, "s4");
    set_param(&mut none, "redirect_uri", None);
    let r = authorize(port, &none);
    assert!(r.location().is_none(), "no redirect without a validated redirect_uri: {}", r.dump());
}

/// `/authorize` protocol error after client+redirect validation: error redirect to the
/// registered URI carrying `error`, the original `state`, and `iss`, and never a code.
fn check_authz_error(port: u16, params: &[(&str, &str)], expected_err: Option<&str>, what: &str) {
    let r = authorize(port, params);
    let (base, q) = redirect_params(&r);
    assert_eq!(base, MUSE_CB, "{what}");
    assert!(!q.contains_key("code"), "{what}: no code: {}", r.dump());
    let err = q.get("error").cloned().unwrap_or_default();
    assert!(!err.is_empty(), "{what}: error param: {}", r.dump());
    if let Some(e) = expected_err {
        assert_eq!(err, e, "{what}: {}", r.dump());
    }
    assert_eq!(q.get("state").map(String::as_str), Some("st-err"), "{what}: state echoed");
    assert_eq!(q.get("iss").map(String::as_str), Some(ISSUER), "{what}: iss");
}

// 5c. Protocol errors after validation are error redirects with state + iss
#[test]
fn authorize_protocol_errors_redirect_with_state_and_iss() {
    let env = Env::new("authzerr");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let p = pkce();


    let mut v = std_params(&client_id, MUSE_CB, &p.challenge, "st-err");
    set_param(&mut v, "code_challenge", None);
    set_param(&mut v, "code_challenge_method", None);
    check_authz_error(port, &v, None, "missing code_challenge");

    let mut v = std_params(&client_id, MUSE_CB, &p.verifier, "st-err");
    set_param(&mut v, "code_challenge_method", Some("plain"));
    check_authz_error(port, &v, None, "plain PKCE");

    let mut v = std_params(&client_id, MUSE_CB, &p.challenge, "st-err");
    set_param(&mut v, "code_challenge_method", None);
    check_authz_error(port, &v, None, "missing code_challenge_method");

    let mut v = std_params(&client_id, MUSE_CB, &p.challenge, "st-err");
    set_param(&mut v, "response_type", Some("token"));
    check_authz_error(port, &v, None, "response_type=token");

    let mut v = std_params(&client_id, MUSE_CB, &p.challenge, "st-err");
    set_param(&mut v, "resource", Some("https://other.example/mcp"));
    check_authz_error(port, &v, Some("invalid_target"), "resource mismatch");

    // `resource` is optional: omitting it is a valid request.
    let mut v = std_params(&client_id, MUSE_CB, &p.challenge, "st-ok");
    set_param(&mut v, "resource", None);
    let r = authorize(port, &v);
    assert_eq!(r.status, 200, "resource is optional: {}", r.dump());
}

// 5d. Token endpoint: PKCE, single use, binding
#[test]
fn token_rejects_wrong_verifier_reuse_and_cross_client() {
    let env = Env::new("tokenneg");
    let srv = env.serve_oauth();
    let port = srv.port;
    let a = register_public(port, "ClientA");
    let b = register_public(port, "ClientB");

    // Wrong verifier → invalid_grant, and the code is burned (deleted on any attempt).
    let (code, p) = obtain_code(&env, port, &a);
    let wrong = pkce();
    let r = token_code(port, &a, &code, &wrong.verifier);
    assert_eq!(r.status, 400, "{}", r.dump());
    assert_eq!(r.oauth_error(), "invalid_grant", "{}", r.dump());
    let r = token_code(port, &a, &code, &p.verifier);
    assert_eq!(r.oauth_error(), "invalid_grant", "code is single-use even after a failed attempt: {}", r.dump());

    // Reuse after success → invalid_grant.
    let (code, p) = obtain_code(&env, port, &a);
    tokens_ok(&token_code(port, &a, &code, &p.verifier));
    let r = token_code(port, &a, &code, &p.verifier);
    assert_eq!(r.status, 400, "{}", r.dump());
    assert_eq!(r.oauth_error(), "invalid_grant", "code reuse: {}", r.dump());

    // Code issued to A, redeemed by B → refused, and A can no longer redeem it.
    let (code, p) = obtain_code(&env, port, &a);
    let r = token_code(port, &b, &code, &p.verifier);
    assert_eq!(r.status, 400, "{}", r.dump());
    assert_eq!(r.oauth_error(), "invalid_grant", "{}", r.dump());
    let r = token_code(port, &a, &code, &p.verifier);
    assert!(!is_2xx(r.status), "code burned by the cross-client attempt: {}", r.dump());

    // Resource mismatch at the token endpoint → refused.
    let (code, p) = obtain_code(&env, port, &a);
    let r = token(
        port,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", MUSE_CB),
            ("client_id", a.as_str()),
            ("code_verifier", p.verifier.as_str()),
            ("resource", "https://other.example/mcp"),
        ],
        &[],
    );
    assert_eq!(r.status, 400, "{}", r.dump());
    let e = r.oauth_error();
    assert!(e == "invalid_target" || e == "invalid_grant", "resource mismatch at /token: {}", r.dump());

    // Refresh token of A presented by B → refused.
    let t = grant(&env, port, &a);
    let r = token_refresh(port, &b, &t.refresh);
    assert!(!is_2xx(r.status), "refresh bound to its client: {}", r.dump());
}

// 5e. Denial and unknown codes
#[test]
fn connect_no_denies_and_unknown_code_fails() {
    let env = Env::new("deny");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let p = pkce();

    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st-deny"));
    let _ = connect(&env, &pend.display, "n\n");
    let r = poll(port, &pend.req);
    assert_eq!(r.status, 302, "denied → error redirect: {}", r.dump());
    let (base, q) = redirect_params(&r);
    assert_eq!(base, MUSE_CB);
    assert_eq!(q.get("error").map(String::as_str), Some("access_denied"), "{}", r.dump());
    assert!(!q.contains_key("code"));
    assert_eq!(q.get("state").map(String::as_str), Some("st-deny"));
    assert_eq!(q.get("iss").map(String::as_str), Some(ISSUER));
    assert_eq!(poll(port, &pend.req).status, 400, "denied request is deleted");

    // Anything other than y/yes is a denial (empty answer = default N).
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st-empty"));
    let _ = connect(&env, &pend.display, "\n");
    let r = poll(port, &pend.req);
    if r.is_redirect() {
        let (_, q) = redirect_params(&r);
        assert!(!q.contains_key("code"), "empty answer must not approve: {}", r.dump());
    }
    let _ = connect(&env, &pend.display, "sure\n");
    let r = poll(port, &pend.req);
    if r.is_redirect() {
        let (_, q) = redirect_params(&r);
        assert!(!q.contains_key("code"), "'sure' must not approve: {}", r.dump());
    }

    // Unknown display code → non-zero exit.
    let out = connect(&env, "QQQQ-QQQQ", "y\n");
    assert!(!out.status.success(), "unknown code must exit non-zero: {}", combined(&out));

    // Unknown req on the wait page → 400.
    assert_eq!(poll(port, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA").status, 400);
}

// 5f. One code per approval
#[test]
fn wait_delivers_code_only_once() {
    let env = Env::new("once");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let p = pkce();
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st"));
    assert!(connect(&env, &pend.display, "y\n").status.success());
    let first = poll(port, &pend.req);
    assert_eq!(first.status, 302, "{}", first.dump());
    assert!(redirect_params(&first).1.contains_key("code"));
    let second = poll(port, &pend.req);
    assert_eq!(second.status, 400, "second poll after delivery finds nothing: {}", second.dump());
    assert!(second.location().is_none());
    // The already-delivered request cannot be approved again either.
    assert!(!connect(&env, &pend.display, "y\n").status.success(), "consumed request can't be re-approved");
}

// 6a. Kill switch suspends (does not revoke) and blocks every OAuth path
#[test]
fn kill_switch_suspends_grants_and_blocks_oauth_endpoints() {
    let env = Env::new("killswitch");
    env.allow("zebrafalcon killswitch fact");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let t = grant(&env, port, &client_id);
    assert!(mcp_ok(&tools_list(port, &t.access)));

    env.ok(&["gateway", "off"]);

    assert!(!mcp_ok(&tools_list(port, &t.access)), "off blocks /mcp (every method)");
    assert!(!mcp_ok(&recall(port, &t.access, "zebrafalcon")), "off blocks recall");

    let p = pkce();
    let r = authorize(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st-off"));
    let (base, q) = redirect_params(&r);
    assert_eq!(base, MUSE_CB);
    assert_eq!(q.get("error").map(String::as_str), Some("access_denied"), "{}", r.dump());
    assert_eq!(q.get("state").map(String::as_str), Some("st-off"));
    assert!(extract_req(&r.body).is_none(), "no pending request while off");

    let r = token_refresh(port, &client_id, &t.refresh);
    // Spec revised after adversarial review: transient while OFF (grant suspended, not revoked).
    assert_eq!(r.status, 503, "{}", r.dump());
    assert_eq!(r.oauth_error(), "temporarily_unavailable", "{}", r.dump());

    env.ok(&["gateway", "on"]);

    // Suspended, not revoked: the same tokens work again.
    assert!(mcp_ok(&tools_list(port, &t.access)), "existing access token usable after on");
    let texts = recall_texts(&recall(port, &t.access, "zebrafalcon"));
    assert!(texts.iter().any(|x| x.contains("zebrafalcon killswitch fact")), "{texts:?}");
    let t2 = tokens_ok(&token_refresh(port, &client_id, &t.refresh));
    assert!(mcp_ok(&tools_list(port, &t2.access)));
}

// 6b. Off during a pending approval / undelivered / unredeemed code invalidates it
#[test]
fn kill_switch_during_approval_invalidates_pending_and_codes() {
    let env = Env::new("killpending");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");

    // A: waiting for approval.
    let pa_pkce = pkce();
    let pa = begin(port, &std_params(&client_id, MUSE_CB, &pa_pkce.challenge, "st-a"));
    // B: approved, code not yet delivered.
    let pb_pkce = pkce();
    let pb = begin(port, &std_params(&client_id, MUSE_CB, &pb_pkce.challenge, "st-b"));
    assert!(connect(&env, &pb.display, "y\n").status.success());
    // C: code delivered, not yet redeemed.
    let (code_c, pc) = obtain_code(&env, port, &client_id);

    env.ok(&["gateway", "off"]);

    let out = connect(&env, &pa.display, "y\n");
    assert!(!out.status.success(), "connect refused while off: {}", combined(&out));
    assert_no_code(&poll(port, &pa.req), "pending A while off");
    assert_no_code(&poll(port, &pb.req), "approved B while off");
    let r = token_code(port, &client_id, &code_c, &pc.verifier);
    // Spec revised after adversarial review: transient while OFF (grant suspended, not revoked).
    assert_eq!(r.status, 503, "{}", r.dump());
    assert_eq!(r.oauth_error(), "temporarily_unavailable", "{}", r.dump());

    env.ok(&["gateway", "on"]);

    assert!(!connect(&env, &pa.display, "y\n").status.success(), "off deleted pending A");
    assert_no_code(&poll(port, &pa.req), "pending A after on");
    assert_no_code(&poll(port, &pb.req), "approved B after on");
    let r = token_code(port, &client_id, &code_c, &pc.verifier);
    assert!(!is_2xx(r.status), "off deleted unredeemed code C: {}", r.dump());

    // A fresh flow works again after on.
    let t = grant(&env, port, &client_id);
    assert!(mcp_ok(&tools_list(port, &t.access)));
}

// 7. disconnect <id> and --all
#[test]
fn disconnect_id_and_all_revoke_on_next_request() {
    let env = Env::new("disconnect");
    let srv = env.serve_oauth();
    let port = srv.port;
    let alpha = register_public(port, "AccAlphaClient");
    let beta = register_public(port, "AccBetaClient");
    let ta = grant(&env, port, &alpha);
    let tb = grant(&env, port, &beta);
    assert!(mcp_ok(&tools_list(port, &ta.access)));
    assert!(mcp_ok(&tools_list(port, &tb.access)));

    let listing = env.clients();
    assert!(listing.contains("AccAlphaClient"), "{listing}");
    assert!(listing.contains("AccBetaClient"), "{listing}");

    disconnect_by_name(&env, "AccAlphaClient");
    let r = tools_list(port, &ta.access);
    assert_eq!(r.status, 401, "disconnected grant → 401 on next request: {}", r.dump());
    assert!(!token_refresh(port, &alpha, &ta.refresh).status.eq(&200), "revoked refresh token");
    assert!(mcp_ok(&tools_list(port, &tb.access)), "other grant unaffected");
    let listing = env.clients();
    assert!(!listing.contains("AccAlphaClient"), "{listing}");
    assert!(listing.contains("AccBetaClient"), "{listing}");

    env.ok(&["gateway", "disconnect", "--all"]);
    assert_eq!(tools_list(port, &tb.access).status, 401, "disconnect --all revokes everything");
    let r = token_refresh(port, &beta, &tb.refresh);
    assert!(!is_2xx(r.status), "{}", r.dump());
    let listing = env.clients();
    assert!(!listing.contains("AccBetaClient"), "{listing}");
}

// 8. Scope narrowing
#[test]
fn unknown_scopes_are_narrowed_to_memory() {
    let env = Env::new("scope");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");

    let (code, p) = obtain_code_with(&env, port, &client_id, MUSE_CB, Some("memory admin offline_access"));
    let t = tokens_ok(&token_code(port, &client_id, &code, &p.verifier));
    assert_eq!(t.body["scope"], "memory");

    let (code, p) = obtain_code_with(&env, port, &client_id, MUSE_CB, Some("admin"));
    let t = tokens_ok(&token_code(port, &client_id, &code, &p.verifier));
    assert_eq!(t.body["scope"], "memory");

    let (code, p) = obtain_code_with(&env, port, &client_id, MUSE_CB, None);
    let t = tokens_ok(&token_code(port, &client_id, &code, &p.verifier));
    assert_eq!(t.body["scope"], "memory");
    let t2 = tokens_ok(&token_refresh(port, &client_id, &t.refresh));
    assert_eq!(t2.body["scope"], "memory");
}

fn assert_html_hardened(r: &Resp, what: &str) {
    assert_eq!(r.header("x-frame-options").map(|v| v.to_ascii_uppercase()), Some("DENY".into()), "{what}: {}", r.dump());
    let csp = r.header("content-security-policy").unwrap_or_else(|| panic!("{what}: CSP missing: {}", r.dump()));
    assert!(csp.contains("frame-ancestors 'none'"), "{what}: {csp}");
    assert!(csp.contains("default-src 'none'"), "{what}: {csp}");
    assert_no_store(r, what);
    assert_eq!(
        r.header("referrer-policy").map(|v| v.to_ascii_lowercase()),
        Some("no-referrer".into()),
        "{what}: {}",
        r.dump()
    );
}

// 9a. HTML hardening + escaping
#[test]
fn authorize_pages_are_hardened_and_escape_client_strings() {
    let env = Env::new("html");
    let srv = env.serve_oauth();
    let port = srv.port;

    let r = register(
        port,
        &json!({
            "client_name": "<script>alert('pwn')</script><img src=x onerror=alert(1)>",
            "redirect_uris": [MUSE_CB],
            "token_endpoint_auth_method": "none"
        }),
    );
    assert!(is_2xx(r.status), "{}", r.dump());
    let client_id = r.json()["client_id"].as_str().unwrap().to_string();

    let p = pkce();
    let state = "\"><script>alert('state')</script>";
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, state));
    assert_html_hardened(&pend.page, "/authorize page");
    for raw in ["<script>alert('pwn')", "<img src=x", "<script>alert('state')"] {
        assert!(!pend.page.body.contains(raw), "unescaped client string {raw:?} in page: {}", pend.page.body);
    }

    let w = poll(port, &pend.req);
    assert_eq!(w.status, 200);
    assert_html_hardened(&w, "/authorize/wait (waiting)");
    assert!(!w.body.contains("<script>alert("), "{}", w.body);

    let bad = std_params("<script>alert('cid')</script>", MUSE_CB, &p.challenge, "s");
    let r = authorize(port, &bad);
    assert_eq!(r.status, 400);
    assert_html_hardened(&r, "/authorize 400 page");
    assert!(!r.body.contains("<script>alert('cid')"), "{}", r.body);

    let r = poll(port, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA");
    assert_eq!(r.status, 400);
    assert_html_hardened(&r, "/authorize/wait 400 page");
}

// 9b. Terminal escaping in connect / clients
#[test]
fn connect_and_clients_escape_terminal_control_chars() {
    let env = Env::new("termesc");
    let srv = env.serve_oauth();
    let port = srv.port;
    let evil_name = "Muse\u{1b}]0;pwned\u{7}\u{1b}[2J\u{1b}[31mRED\u{202e}evil\r\nApprove? [y/N] y";
    let r = register(
        port,
        &json!({"client_name": evil_name, "redirect_uris": [MUSE_CB], "token_endpoint_auth_method": "none"}),
    );
    if !is_2xx(r.status) {
        // Rejecting control characters at registration is an acceptable (stricter) defense.
        assert_eq!(r.status, 400, "{}", r.dump());
        return;
    }
    let client_id = r.json()["client_id"].as_str().unwrap().to_string();
    let p = pkce();
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st"));
    let out = connect(&env, &pend.display, "y\n");
    assert!(out.status.success(), "{}", combined(&out));
    for (stream, bytes) in [("stdout", &out.stdout), ("stderr", &out.stderr)] {
        let s = String::from_utf8_lossy(bytes);
        assert!(!s.contains('\u{1b}'), "connect {stream} contains raw ESC: {s:?}");
        assert!(!s.contains('\u{7}'), "connect {stream} contains raw BEL: {s:?}");
        assert!(!s.contains('\u{202e}'), "connect {stream} contains raw bidi override: {s:?}");
        assert!(!s.contains('\r'), "connect {stream} contains raw CR: {s:?}");
    }

    let r = poll(port, &pend.req);
    let code = redirect_params(&r).1.get("code").cloned().expect("code");
    tokens_ok(&token_code(port, &client_id, &code, &p.verifier));
    let out = env.run(&["gateway", "clients"]);
    assert!(out.status.success());
    let s = String::from_utf8_lossy(&out.stdout);
    assert!(!s.contains('\u{1b}') && !s.contains('\u{202e}') && !s.contains('\u{7}'), "clients escapes names: {s:?}");
}

// 10a. Static token alongside OAuth
#[test]
fn static_token_coexists_with_oauth_and_survives_disconnect_all() {
    let env = Env::new("static");
    let srv = env.serve(&OAUTH_ARGS, Some(STATIC_TOKEN));
    let port = srv.port;
    assert!(mcp_ok(&tools_list(port, STATIC_TOKEN)), "static token still accepted with --oauth");

    let client_id = register_public(port, "Muse");
    let t = grant(&env, port, &client_id);
    assert!(mcp_ok(&tools_list(port, &t.access)), "OAuth token accepted too");

    env.ok(&["gateway", "disconnect", "--all"]);
    assert_eq!(tools_list(port, &t.access).status, 401);
    assert!(mcp_ok(&tools_list(port, STATIC_TOKEN)), "disconnect --all does not revoke the static token");
}

// 10b. --oauth without a static token starts and only accepts OAuth tokens
#[test]
fn oauth_serve_starts_without_static_token() {
    let env = Env::new("nostatic");
    let srv = env.serve_oauth();
    let port = srv.port;
    assert_eq!(tools_list(port, STATIC_TOKEN).status, 401, "no static token configured");
    assert_eq!(tools_list(port, "").status, 401);
    let client_id = register_public(port, "Muse");
    let t = grant(&env, port, &client_id);
    assert!(mcp_ok(&tools_list(port, &t.access)));
}

// 10c. Startup validation of OAuth flags
#[test]
fn serve_rejects_invalid_oauth_configuration() {
    let env = Env::new("badcfg");
    env.serve_must_fail(&["--oauth", "--public-url", "http://gw.example.test"]);
    env.serve_must_fail(&["--oauth", "--public-url", "https://gw.example.test/sub"]);
    env.serve_must_fail(&["--oauth", "--public-url", "https://gw.example.test?x=1"]);
    env.serve_must_fail(&["--oauth", "--public-url", "https://gw.example.test#f"]);
    env.serve_must_fail(&["--oauth", "--public-url", ISSUER, "--oauth-redirect", "http://evil.example/cb"]);
    // Without --oauth (and without --enable of OAuth), a missing static token still refuses to start.
    env.serve_must_fail(&[]);
}

// 11. Concurrent polls → exactly one code
#[test]
fn concurrent_polls_yield_exactly_one_code() {
    let env = Env::new("concpoll");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let p = pkce();
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st"));
    assert!(connect(&env, &pend.display, "y\n").status.success());

    const N: usize = 8;
    let barrier = Arc::new(Barrier::new(N));
    let handles: Vec<_> = (0..N)
        .map(|_| {
            let b = barrier.clone();
            let req = pend.req.clone();
            std::thread::spawn(move || {
                b.wait();
                let r = poll(port, &req);
                let code = if r.is_redirect() {
                    r.location().map(split_location).and_then(|(_, q)| q.get("code").cloned())
                } else {
                    None
                };
                (r.status, code)
            })
        })
        .collect();
    let results: Vec<(u16, Option<String>)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let codes: Vec<&String> = results.iter().filter_map(|(_, c)| c.as_ref()).collect();
    assert_eq!(codes.len(), 1, "exactly one poll gets a code: {results:?}");
    for (st, c) in &results {
        if c.is_none() {
            assert_eq!(*st, 400, "losers see nothing pending: {results:?}");
        }
    }
    // And the single code works.
    tokens_ok(&token_code(port, &client_id, codes[0], &p.verifier));
}

// 12. Storage: 0600, hash-only
#[cfg(unix)]
#[test]
fn oauth_state_file_is_private_and_hash_only() {
    use std::os::unix::fs::PermissionsExt;
    let env = Env::new("storage");
    let srv = env.serve_oauth();
    let port = srv.port;

    // A confidential client so a secret exists too.
    let r = register(port, &json!({"client_name": "Muse", "redirect_uris": [MUSE_CB]}));
    assert!(is_2xx(r.status), "{}", r.dump());
    let reg = r.json();
    let client_id = reg["client_id"].as_str().unwrap().to_string();
    let secret = reg["client_secret"].as_str().unwrap().to_string();
    let basic = format!("Basic {}", b64(format!("{}:{}", pct(&client_id), pct(&secret)).as_bytes(), false));

    let read = || std::fs::read_to_string(env.oauth_file()).expect("gateway-oauth.json exists in the DB dir");
    assert!(!read().contains(&secret), "client secret stored in plaintext");

    let p = pkce();
    let pend = begin(port, &std_params(&client_id, MUSE_CB, &p.challenge, "st"));
    assert!(!read().contains(&pend.req), "req capability stored in plaintext");
    assert!(connect(&env, &pend.display, "y\n").status.success());
    let r = poll(port, &pend.req);
    let code = redirect_params(&r).1.get("code").cloned().expect("code");
    assert!(!read().contains(&code), "authorization code stored in plaintext");

    let t = tokens_ok(&token(
        port,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", MUSE_CB),
            ("code_verifier", p.verifier.as_str()),
        ],
        &[("Authorization", basic.as_str())],
    ));
    let s = read();
    assert!(!s.contains(&t.access), "access token stored in plaintext");
    assert!(!s.contains(&t.refresh), "refresh token stored in plaintext");
    assert!(!s.contains(&secret));

    let mode = std::fs::metadata(env.oauth_file()).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600, "gateway-oauth.json must be 0600, got {mode:o}");
    if let Ok(m) = std::fs::metadata(env.oauth_lock()) {
        assert_eq!(m.permissions().mode() & 0o077, 0, "lock file must not be group/world accessible");
    }
}

// Spec "concurrent refresh": racing refreshes revoke the grant.
#[test]
fn concurrent_refresh_revokes_grant() {
    let env = Env::new("concrefresh");
    let srv = env.serve_oauth();
    let port = srv.port;
    let client_id = register_public(port, "Muse");
    let t = grant(&env, port, &client_id);

    let barrier = Arc::new(Barrier::new(2));
    let handles: Vec<_> = (0..2)
        .map(|_| {
            let b = barrier.clone();
            let cid = client_id.clone();
            let rt = t.refresh.clone();
            std::thread::spawn(move || {
                b.wait();
                let r = token_refresh(port, &cid, &rt);
                let access = if r.status == 200 {
                    r.json()["access_token"].as_str().map(str::to_string)
                } else {
                    None
                };
                (r.status, r.oauth_error(), access)
            })
        })
        .collect();
    let results: Vec<(u16, String, Option<String>)> = handles.into_iter().map(|h| h.join().unwrap()).collect();
    let winners: Vec<&String> = results.iter().filter_map(|(_, _, a)| a.as_ref()).collect();
    assert!(winners.len() <= 1, "at most one refresh succeeds: {results:?}");
    assert!(
        results.iter().any(|(s, e, _)| *s == 400 && e == "invalid_grant"),
        "the loser is invalid_grant: {results:?}"
    );
    for a in winners {
        assert_eq!(tools_list(port, a).status, 401, "refresh reuse revoked the winner's tokens too");
    }
    assert_eq!(tools_list(port, &t.access).status, 401);
}

// Spec "restart between approval and poll".
#[test]
fn restart_between_approval_and_poll_still_delivers_code() {
    let env = Env::new("restart");
    let srv = env.serve_oauth();
    let client_id = register_public(srv.port, "Muse");
    let p = pkce();
    let pend = begin(srv.port, &std_params(&client_id, MUSE_CB, &p.challenge, "st-restart"));
    assert!(connect(&env, &pend.display, "y\n").status.success());
    drop(srv);

    let srv = env.serve_oauth();
    let r = poll(srv.port, &pend.req);
    assert_eq!(r.status, 302, "approval persisted across restart: {}", r.dump());
    let (_, q) = redirect_params(&r);
    assert_eq!(q.get("state").map(String::as_str), Some("st-restart"));
    let code = q.get("code").cloned().expect("code");
    let t = tokens_ok(&token_code(srv.port, &client_id, &code, &p.verifier));
    assert!(mcp_ok(&tools_list(srv.port, &t.access)));

    // Grants survive a restart too.
    drop(srv);
    let srv = env.serve_oauth();
    assert!(mcp_ok(&tools_list(srv.port, &t.access)), "grant persisted across restart");
}

// --oauth-redirect: extra loopback callback, exact match only.
#[test]
fn extra_loopback_redirect_is_allowlisted_by_exact_match() {
    let env = Env::new("extraredirect");
    let cb = "http://127.0.0.1:9/cb";
    let mut args: Vec<&str> = OAUTH_ARGS.to_vec();
    args.extend_from_slice(&["--oauth-redirect", cb]);
    let srv = env.serve(&args, None);
    let port = srv.port;

    let r = register(port, &json!({"redirect_uris": ["http://127.0.0.1:9/cb/other"], "token_endpoint_auth_method": "none"}));
    assert_eq!(r.status, 400, "prefix of an allowlisted URI is not a match: {}", r.dump());
    let r = register(port, &json!({"redirect_uris": ["http://127.0.0.1:9/cb?x=1"], "token_endpoint_auth_method": "none"}));
    assert_eq!(r.status, 400, "added query is not an exact match: {}", r.dump());

    let client_id = register_public_for(port, "LoopbackClient", cb);
    let (code, p) = obtain_code_with(&env, port, &client_id, cb, Some("memory"));
    let r = token(
        port,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", cb),
            ("client_id", client_id.as_str()),
            ("code_verifier", p.verifier.as_str()),
        ],
        &[],
    );
    let t = tokens_ok(&r);
    assert!(mcp_ok(&tools_list(port, &t.access)));

    // Token request with a different redirect_uri than the authorization used → refused.
    let (code, p) = obtain_code_with(&env, port, &client_id, cb, Some("memory"));
    let r = token(
        port,
        &[
            ("grant_type", "authorization_code"),
            ("code", code.as_str()),
            ("redirect_uri", MUSE_CB),
            ("client_id", client_id.as_str()),
            ("code_verifier", p.verifier.as_str()),
        ],
        &[],
    );
    assert!(!is_2xx(r.status), "redirect_uri must equal the authorization's: {}", r.dump());
}

// ───────────────────── SHA-256 / base64 (no extra dev-deps) ─────────────────────

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

fn sha256(data: &[u8]) -> [u8; 32] {
    let mut h: [u32; 8] = [
        0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
    ];
    let mut msg = data.to_vec();
    let bit_len = (data.len() as u64).wrapping_mul(8);
    msg.push(0x80);
    while msg.len() % 64 != 56 {
        msg.push(0);
    }
    msg.extend_from_slice(&bit_len.to_be_bytes());
    for chunk in msg.chunks(64) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes([chunk[4 * i], chunk[4 * i + 1], chunk[4 * i + 2], chunk[4 * i + 3]]);
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }
        let mut v = h;
        for i in 0..64 {
            let (a, b, c, d, e, f, g, hh) = (v[0], v[1], v[2], v[3], v[4], v[5], v[6], v[7]);
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ (!e & g);
            let t1 = hh.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let t2 = s0.wrapping_add(maj);
            v = [t1.wrapping_add(t2), a, b, c, d.wrapping_add(t1), e, f, g];
        }
        for i in 0..8 {
            h[i] = h[i].wrapping_add(v[i]);
        }
    }
    let mut out = [0u8; 32];
    for i in 0..8 {
        out[4 * i..4 * i + 4].copy_from_slice(&h[i].to_be_bytes());
    }
    out
}

fn b64(data: &[u8], url: bool) -> String {
    let alpha: &[u8; 64] = if url {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_"
    } else {
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/"
    };
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        let chars = [(n >> 18) & 63, (n >> 12) & 63, (n >> 6) & 63, n & 63];
        for (i, c) in chars.iter().enumerate() {
            if i <= chunk.len() {
                out.push(alpha[*c as usize] as char);
            } else if !url {
                out.push('=');
            }
        }
    }
    out
}

/// base64url without padding (PKCE, RFC 7636 §4.2).
fn b64url(data: &[u8]) -> String {
    b64(data, true)
}

// ── Regressions from the adversarial review (written by the implementer) ─────

#[test]
fn review_disconnect_all_drops_approved_signins_and_unredeemed_codes() {
    let env = Env::new("rv-disc-all");
    let srv = env.serve_oauth();
    let port = srv.port;
    let cid = register_public(port, "RvClient");
    // Approved but not yet polled.
    let p = pkce();
    let pend = begin(port, &std_params(&cid, MUSE_CB, &p.challenge, "s1"));
    assert!(connect(&env, &pend.display, "y\n").status.success());
    // Code delivered but not redeemed.
    let (code, pc) = obtain_code(&env, port, &cid);
    env.ok(&["gateway", "disconnect", "--all"]);
    assert_no_code(&poll(port, &pend.req), "approved sign-in after disconnect --all");
    let r = token_code(port, &cid, &code, &pc.verifier);
    assert_eq!(r.oauth_error(), "invalid_grant", "{}", r.dump());
}

#[test]
fn review_replayed_code_revokes_the_grant_it_produced() {
    let env = Env::new("rv-code-replay");
    let srv = env.serve_oauth();
    let port = srv.port;
    let cid = register_public(port, "RvClient");
    let (code, p) = obtain_code(&env, port, &cid);
    let t = tokens_ok(&token_code(port, &cid, &code, &p.verifier));
    assert!(mcp_ok(&tools_list(port, &t.access)));
    let r = token_code(port, &cid, &code, &p.verifier);
    assert_eq!(r.oauth_error(), "invalid_grant", "{}", r.dump());
    assert_eq!(tools_list(port, &t.access).status, 401, "grant from a replayed code is revoked");
}

#[test]
fn review_kill_switch_does_not_look_like_revocation() {
    let env = Env::new("rv-off-403");
    let srv = env.serve_oauth();
    let port = srv.port;
    let cid = register_public(port, "RvClient");
    let t = grant(&env, port, &cid);
    env.ok(&["gateway", "off"]);
    let r = tools_list(port, &t.access);
    assert_eq!(r.status, 403, "valid token while OFF is 403, not 401: {}", r.dump());
    assert!(r.header("www-authenticate").is_none(), "{}", r.dump());
    env.ok(&["gateway", "on"]);
    assert!(mcp_ok(&tools_list(port, &t.access)));
}

#[test]
fn review_client_name_with_invisible_chars_is_refused() {
    let env = Env::new("rv-name");
    let srv = env.serve_oauth();
    for name in ["Mu\u{202E}esu", "Muse\u{200B}", "Muse\u{1b}[31m"] {
        let r = register(srv.port, &serde_json::json!({ "redirect_uris": [MUSE_CB], "client_name": name, "token_endpoint_auth_method": "none" }));
        assert_eq!(r.oauth_error(), "invalid_client_metadata", "{name:?}: {}", r.dump());
    }
}

#[test]
fn review_overlong_state_is_refused_without_redirect() {
    let env = Env::new("rv-state");
    let srv = env.serve_oauth();
    let cid = register_public(srv.port, "RvClient");
    let p = pkce();
    let long = "s".repeat(1025);
    let r = authorize(srv.port, &std_params(&cid, MUSE_CB, &p.challenge, &long));
    assert_eq!(r.status, 400, "{}", r.dump());
    assert!(r.location().is_none());
}

#[test]
fn review_junk_token_requests_dont_lock_out_a_registered_client() {
    let env = Env::new("rv-token-rate");
    let srv = env.serve_oauth();
    let port = srv.port;
    let cid = register_public(port, "RvClient");
    let t = grant(&env, port, &cid);
    for _ in 0..80 {
        let r = token_refresh(port, "cortex-AAAAAAAAAAAAAAAAAAAAAA", "junk");
        assert_eq!(r.status, 401, "{}", r.dump());
    }
    let r = token_refresh(port, &cid, &t.refresh);
    assert_eq!(r.status, 200, "Muse can still refresh after a junk flood: {}", r.dump());
}

#[test]
fn review_omitted_grant_types_still_allow_refresh() {
    let env = Env::new("rv-grant-types");
    let srv = env.serve_oauth();
    let port = srv.port;
    let r = register(port, &serde_json::json!({ "redirect_uris": [MUSE_CB], "token_endpoint_auth_method": "none" }));
    let meta = r.json();
    let gts: Vec<&str> = meta["grant_types"].as_array().unwrap().iter().filter_map(|v| v.as_str()).collect();
    assert!(gts.contains(&"refresh_token"), "{}", r.dump());
    let cid = meta["client_id"].as_str().unwrap().to_string();
    let t = grant(&env, port, &cid);
    assert_eq!(token_refresh(port, &cid, &t.refresh).status, 200);

    // Registered for codes only → refresh refused.
    let r = register(port, &serde_json::json!({ "redirect_uris": [MUSE_CB], "token_endpoint_auth_method": "none", "grant_types": ["authorization_code"] }));
    let cid2 = r.json()["client_id"].as_str().unwrap().to_string();
    let t2 = grant(&env, port, &cid2);
    assert_eq!(token_refresh(port, &cid2, &t2.refresh).oauth_error(), "unauthorized_client");
}

#[test]
fn review_trickled_oauth_bodies_dont_starve_mcp() {
    use std::io::Write as _;
    let env = Env::new("rv-lane");
    let tok = "q7Lx9vR2mK4pT8wZ3nB6cF1hJ5sD0gY2uE7aW4iO9eV";
    let srv = env.serve(&["--oauth", "--public-url", ISSUER], Some(tok));
    let port = srv.port;
    // Hold every OAuth slot (and more) with bodies that never finish.
    let mut held = Vec::new();
    for _ in 0..24 {
        if let Ok(mut s) = std::net::TcpStream::connect(("127.0.0.1", port)) {
            let _ = s.write_all(
                b"POST /token HTTP/1.1\r\nHost: x\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: 4000\r\n\r\ngrant_type=",
            );
            held.push(s);
        }
    }
    std::thread::sleep(Duration::from_millis(300));
    let r = tools_list(port, tok);
    assert!(mcp_ok(&r), "static-token MCP must still be served: {}", r.dump());
    drop(held);
}

#[test]
fn review_rate_limited_wait_page_keeps_polling() {
    let env = Env::new("rv-wait-rate");
    let srv = env.serve_oauth();
    let port = srv.port;
    let unknown = "A".repeat(43);
    let mut limited = None;
    for _ in 0..400 {
        let r = poll(port, &unknown);
        if r.status == 429 {
            limited = Some(r);
            break;
        }
    }
    let r = limited.expect("wait endpoint is rate limited");
    assert!(
        r.body.contains("http-equiv=\"refresh\"") && r.body.contains(&format!("req={unknown}")),
        "a rate-limited poll must keep refreshing with the same req: {}",
        r.dump()
    );
}
