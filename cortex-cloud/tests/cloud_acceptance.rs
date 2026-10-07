//! Black-box acceptance tests for Cortex Cloud for Muse (`cortex-cloud`, v2.6).
//!
//! Written by a context-isolated agent from `docs/design/muse-cloud.md` and
//! `docs/design/muse-oauth.md` only — never from the implementation or its unit tests.
//!
//! Drives the real binaries:
//! - `cortex-cloud` (this package) over raw HTTP/1.1 on loopback (base URL `https://cloud.test`
//!   is never resolved; redirects are read from `Location`, never followed);
//! - `cortex-mcp-server` (the local device) over stdio JSON-RPC (`muse_*` tools);
//! - the public device-API client `cortex_mcp_server::gateway::cloud::Device`, plus raw
//!   Ed25519-signed requests for the negative signature tests.
//!
//! `cortex-mcp-server` is located next to this package's binary (`target/<profile>/`), or via
//! `CORTEX_MCP_SERVER_BIN`; if missing it is built once with `cargo build -p cortex-mcp-server`.
//!
//! Not covered (would need real waiting): 30-min enrollment expiry, 90-day idle deletion,
//! rate-limit windows.

use base64::engine::general_purpose::{STANDARD, URL_SAFE_NO_PAD};
use base64::Engine as _;
use cortex_mcp_server::gateway::cloud::{canonical, Device, H_KEY, H_NONCE, H_SIG, H_TS};
use ed25519_dalek::{Signer, SigningKey};
use rand::RngCore;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, OnceLock};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const CLOUD_BIN: &str = env!("CARGO_BIN_EXE_cortex-cloud");
const BASE: &str = "https://cloud.test";
const MUSE_CB: &str = "https://agent.meta.ai/api/hatch/oauth/callback";

// ───────────────────────── temp dirs / binaries ─────────────────────────

static COUNTER: AtomicUsize = AtomicUsize::new(0);

struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> TempDir {
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("cortex-cloud-acc-{name}-{}-{nanos}-{n}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        TempDir(dir)
    }
    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn mcp_server_bin() -> PathBuf {
    static BIN: OnceLock<PathBuf> = OnceLock::new();
    BIN.get_or_init(|| {
        if let Ok(p) = std::env::var("CORTEX_MCP_SERVER_BIN") {
            return PathBuf::from(p);
        }
        let dir = Path::new(CLOUD_BIN).parent().unwrap().to_path_buf();
        let p = dir.join(format!("cortex-mcp-server{}", std::env::consts::EXE_SUFFIX));
        if !p.exists() {
            let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
            let st = Command::new(cargo)
                .args(["build", "-p", "cortex-mcp-server", "--bin", "cortex-mcp-server"])
                .status()
                .expect("run cargo build for cortex-mcp-server");
            assert!(st.success(), "building cortex-mcp-server failed");
        }
        assert!(p.exists(), "cortex-mcp-server binary not found at {}", p.display());
        // `cargo test -p cortex-cloud` doesn't rebuild that binary: refuse to test a stale one.
        let built = std::fs::metadata(&p).and_then(|m| m.modified()).unwrap();
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("../cortex-mcp-server/src");
        let newest = files_under(&src).iter().filter_map(|f| std::fs::metadata(f).and_then(|m| m.modified()).ok()).max();
        assert!(
            newest.is_none_or(|n| n <= built),
            "cortex-mcp-server binary is older than its sources: run `cargo build -p cortex-mcp-server` (or test the workspace)"
        );
        p
    })
    .clone()
}

fn free_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    l.local_addr().unwrap().port()
}

// ───────────────────────────── cloud server ─────────────────────────────

struct Cloud {
    child: Child,
    port: u16,
    data: PathBuf,
    master: PathBuf,
}

impl Cloud {
    /// Start `cortex-cloud` with its data dir + master key under `root`.
    fn start(root: &Path) -> Cloud {
        let data = root.join("data");
        let master = root.join("master.key");
        for _attempt in 0..5 {
            let port = free_port();
            let mut child = Command::new(CLOUD_BIN)
                .args(["--base-url", BASE, "--listen", &format!("127.0.0.1:{port}")])
                .arg("--data-dir")
                .arg(&data)
                .arg("--master-key")
                .arg(&master)
                .env("CORTEX_NO_EMBEDDINGS", "1")
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::piped())
                .spawn()
                .expect("spawn cortex-cloud");
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
            let deadline = Instant::now() + Duration::from_secs(60);
            let mut seen = String::new();
            let mut ok = false;
            loop {
                let left = deadline.saturating_duration_since(Instant::now());
                match rx.recv_timeout(left) {
                    Ok(line) => {
                        if line.contains("cortex-cloud listening on http://") {
                            ok = true;
                            break;
                        }
                        seen.push_str(&line);
                        seen.push('\n');
                    }
                    Err(_) => break,
                }
            }
            if ok {
                let c = Cloud { child, port, data: data.clone(), master: master.clone() };
                let h = get(port, "/healthz");
                assert_eq!(h.status, 200, "healthz: {}", h.dump());
                assert_eq!(h.body.trim(), "ok");
                return c;
            }
            let _ = child.kill();
            let _ = child.wait();
            eprintln!("cortex-cloud did not start on {port} (retrying): {seen}");
        }
        panic!("cortex-cloud never started");
    }

    fn url(&self) -> String {
        format!("http://127.0.0.1:{}", self.port)
    }

    fn stop(mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Cloud {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ───────────────────────────── HTTP helpers ─────────────────────────────

struct Resp {
    status: u16,
    headers: Vec<(String, String)>,
    body: String,
}

impl Resp {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str())
    }
    fn headers_all(&self, name: &str) -> Vec<&str> {
        self.headers.iter().filter(|(k, _)| k.eq_ignore_ascii_case(name)).map(|(_, v)| v.as_str()).collect()
    }
    fn location(&self) -> Option<&str> {
        self.header("location")
    }
    fn json(&self) -> Value {
        serde_json::from_str(&self.body)
            .unwrap_or_else(|e| panic!("expected JSON body ({e}); status={} body={}", self.status, self.body))
    }
    fn dump(&self) -> String {
        format!("status={} headers={:?} body={}", self.status, self.headers, self.body)
    }
}

fn http(port: u16, method: &str, path: &str, headers: &[(&str, &str)], body: &[u8]) -> Resp {
    let mut s = TcpStream::connect(("127.0.0.1", port)).expect("connect cortex-cloud");
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
    let (head, rest) = text.split_once("\r\n\r\n").unwrap_or_else(|| panic!("malformed HTTP response: {text:?}"));
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
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn pct_decode(s: &str) -> String {
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
            if let Ok(v) = u8::from_str_radix(&s[i + 1..i + 3], 16) {
                out.push(v);
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
    pairs.iter().map(|(k, v)| format!("{}={}", pct(k), pct(v))).collect::<Vec<_>>().join("&")
}

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

fn sha256(b: &[u8]) -> Vec<u8> {
    Sha256::digest(b).to_vec()
}

fn rand_b64url(n: usize) -> String {
    let mut v = vec![0u8; n];
    rand::thread_rng().fill_bytes(&mut v);
    URL_SAFE_NO_PAD.encode(v)
}

// ───────────────────────────── local device (stdio MCP) ─────────────────────────────

/// One local Cortex install: its own DB dir + HOME, talking to one cloud.
struct Dev {
    root: PathBuf,
    cloud_url: String,
    db_name: String,
}

struct ToolOut {
    is_error: bool,
    text: String,
}

impl ToolOut {
    fn json(&self) -> Value {
        assert!(!self.is_error, "tool returned an error: {}", self.text);
        serde_json::from_str(&self.text).unwrap_or_else(|e| panic!("tool text is not JSON ({e}): {}", self.text))
    }
}

impl Dev {
    fn new(root: &Path, cloud: &Cloud) -> Dev {
        let root = root.to_path_buf();
        std::fs::create_dir_all(root.join("dev")).unwrap();
        Dev { root, cloud_url: cloud.url(), db_name: "memory.db".into() }
    }

    /// Another database in the same directory.
    fn with_db(root: &Path, cloud: &Cloud, db_name: &str) -> Dev {
        let mut d = Dev::new(root, cloud);
        d.db_name = db_name.into();
        d
    }

    fn db_dir(&self) -> PathBuf {
        self.root.join("dev")
    }

    /// Run one `tools/call` in a fresh stdio session (state persists on disk).
    fn call(&self, tool: &str, args: Value) -> ToolOut {
        let mut child = Command::new(mcp_server_bin())
            .env("CORTEX_DB_PATH", self.db_dir().join(&self.db_name))
            .env("CORTEX_CLOUD_URL", &self.cloud_url)
            .env("CORTEX_NO_EMBEDDINGS", "1")
            // Never touch the developer's real login keychain.
            .env("CORTEX_NO_KEYCHAIN", "1")
            .env("HOME", &self.root)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .expect("spawn cortex-mcp-server");
        let mut stdin = child.stdin.take().unwrap();
        let lines = [
            json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2024-11-05","capabilities":{},"clientInfo":{"name":"acc","version":"1"}}}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","id":2,"method":"tools/call","params":{"name":tool,"arguments":args}}),
        ];
        for l in &lines {
            writeln!(stdin, "{l}").unwrap();
        }
        stdin.flush().unwrap();
        let stdout = child.stdout.take().unwrap();
        let (tx, rx) = mpsc::channel::<Value>();
        std::thread::spawn(move || {
            for line in BufReader::new(stdout).lines().map_while(Result::ok) {
                if let Ok(v) = serde_json::from_str::<Value>(&line) {
                    if v.get("id") == Some(&json!(2)) {
                        let _ = tx.send(v);
                        return;
                    }
                }
            }
        });
        let resp = rx.recv_timeout(Duration::from_secs(120));
        drop(stdin);
        let _ = child.kill();
        let _ = child.wait();
        let v = resp.unwrap_or_else(|_| panic!("no response from cortex-mcp-server for {tool}"));
        if let Some(e) = v.get("error") {
            return ToolOut { is_error: true, text: e.to_string() };
        }
        let r = &v["result"];
        let text = r["content"][0]["text"].as_str().unwrap_or_default().to_string();
        ToolOut { is_error: r["isError"].as_bool() == Some(true), text }
    }

    /// Sharing is two-step (spec revision after review): the first call only previews and
    /// returns a confirmation code; the second call with it shares.
    fn call_confirmed(&self, tool: &str, args: Value) -> ToolOut {
        let first = self.call(tool, args.clone());
        if first.is_error {
            return first;
        }
        let v = first.json();
        match v.get("confirmation").and_then(Value::as_str) {
            Some(code) if v["needs_confirmation"] == json!(true) => {
                let mut a = args;
                a["confirmation"] = json!(code);
                self.call(tool, a)
            }
            _ => first,
        }
    }

    fn connect_texts(&self, texts: &[&str]) -> Value {
        self.call_confirmed("muse_connect", json!({"texts": texts})).json()
    }

    fn status(&self) -> Value {
        self.call("muse_status", json!({})).json()
    }
}

/// `https://cloud.test/t/<rid>/mcp` → rid.
fn rid_of(link: &str) -> String {
    let rest = link.strip_prefix(&format!("{BASE}/t/")).unwrap_or_else(|| panic!("link not under {BASE}/t/: {link}"));
    let rid = rest.strip_suffix("/mcp").unwrap_or_else(|| panic!("link does not end in /mcp: {link}"));
    assert!(!rid.is_empty() && !rid.contains('/'), "bad rid in {link}");
    rid.to_string()
}

// ───────────────────────────── Muse simulator ─────────────────────────────

struct Muse {
    port: u16,
    rid: String,
}

struct Consent {
    status: u16,
    body: String,
    req: Option<String>,
    csrf: Option<String>,
    cookie: Option<String>,
    resp: Resp,
}

fn hidden_field(html: &str, name: &str) -> Option<String> {
    let marker = format!("name=\"{name}\"");
    let idx = html.find(&marker)?;
    // Look in the enclosing <input …> tag for value="…".
    let start = html[..idx].rfind('<')?;
    let end = idx + html[idx..].find('>')?;
    let tag = &html[start..end];
    let v = tag.find("value=\"")? + "value=\"".len();
    let val = &tag[v..];
    Some(val[..val.find('"')?].to_string())
}

impl Muse {
    fn new(cloud: &Cloud, rid: &str) -> Muse {
        Muse { port: cloud.port, rid: rid.to_string() }
    }
    fn p(&self, suffix: &str) -> String {
        format!("/t/{}{}", self.rid, suffix)
    }
    fn issuer(&self) -> String {
        format!("{BASE}/t/{}", self.rid)
    }
    fn resource(&self) -> String {
        format!("{BASE}/t/{}/mcp", self.rid)
    }

    fn register(&self) -> String {
        let r = http(
            self.port,
            "POST",
            &self.p("/register"),
            &[("Content-Type", "application/json")],
            json!({"client_name":"Muse","redirect_uris":[MUSE_CB],"token_endpoint_auth_method":"none",
                   "grant_types":["authorization_code","refresh_token"],"response_types":["code"]})
            .to_string()
            .as_bytes(),
        );
        assert!((200..300).contains(&r.status), "DCR failed: {}", r.dump());
        r.json()["client_id"].as_str().expect("client_id").to_string()
    }

    fn authorize(&self, client_id: &str, challenge: &str, state: &str) -> Consent {
        let resource = self.resource();
        let q = form(&[
            ("response_type", "code"),
            ("client_id", client_id),
            ("redirect_uri", MUSE_CB),
            ("code_challenge", challenge),
            ("code_challenge_method", "S256"),
            ("state", state),
            ("scope", "memory"),
            ("resource", &resource),
        ]);
        let r = get(self.port, &format!("{}?{q}", self.p("/authorize")));
        let cookie = r
            .headers_all("set-cookie")
            .into_iter()
            .find(|c| c.starts_with("cx_csrf="))
            .map(|c| c.split(';').next().unwrap().to_string());
        Consent {
            status: r.status,
            body: r.body.clone(),
            req: hidden_field(&r.body, "req"),
            csrf: hidden_field(&r.body, "csrf"),
            cookie,
            resp: r,
        }
    }

    fn approve(&self, req: &str, csrf: &str, cookie: Option<&str>, origin: Option<&str>) -> Resp {
        let mut h: Vec<(&str, &str)> = vec![("Content-Type", "application/x-www-form-urlencoded")];
        if let Some(c) = cookie {
            h.push(("Cookie", c));
        }
        if let Some(o) = origin {
            h.push(("Origin", o));
        }
        http(self.port, "POST", &self.p("/authorize/approve"), &h, form(&[("req", req), ("csrf", csrf)]).as_bytes())
    }

    /// Follow a same-origin redirect (`https://cloud.test/...` or a relative path) by GET.
    fn follow(&self, loc: &str) -> Resp {
        let path = loc.strip_prefix(BASE).unwrap_or(loc);
        assert!(path.starts_with('/'), "unexpected redirect target {loc}");
        get(self.port, path)
    }

    fn token(&self, pairs: &[(&str, &str)]) -> Resp {
        http(
            self.port,
            "POST",
            &self.p("/token"),
            &[("Content-Type", "application/x-www-form-urlencoded"), ("Accept", "application/json")],
            form(pairs).as_bytes(),
        )
    }

    /// Full Muse sign-in: DCR → authorize → Allow (cookie + csrf + Origin) → wait → token.
    /// Returns the access token.
    fn sign_in(&self) -> String {
        let client = self.register();
        let verifier = rand_b64url(32);
        let challenge = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
        let state = rand_b64url(12);
        let c = self.authorize(&client, &challenge, &state);
        assert_eq!(c.status, 200, "consent page: {}", c.resp.dump());
        let req = c.req.clone().unwrap_or_else(|| panic!("no hidden req on consent page: {}", c.body));
        let csrf = c.csrf.clone().unwrap_or_else(|| panic!("no hidden csrf on consent page: {}", c.body));
        let cookie = c.cookie.clone().unwrap_or_else(|| panic!("no cx_csrf cookie: {}", c.resp.dump()));
        let a = self.approve(&req, &csrf, Some(&cookie), Some(BASE));
        assert_eq!(a.status, 303, "Allow should 303 to the wait page: {}", a.dump());
        let loc = a.location().expect("Location on Allow").to_string();
        assert!(loc.contains("/authorize/wait?req="), "Allow redirects to wait: {loc}");
        let w = self.follow(&loc);
        assert_eq!(w.status, 302, "wait after Allow should 302 to Muse: {}", w.dump());
        let (base, q) = split_location(w.location().unwrap());
        assert_eq!(base, MUSE_CB, "code goes to Muse callback");
        assert_eq!(q.get("state"), Some(&state), "state echoed");
        assert_eq!(q.get("iss").cloned(), Some(self.issuer()), "iss = tenant issuer");
        let code = q.get("code").cloned().expect("code in callback");
        let resource = self.resource();
        let t = self.token(&[
            ("grant_type", "authorization_code"),
            ("code", &code),
            ("redirect_uri", MUSE_CB),
            ("client_id", &client),
            ("code_verifier", &verifier),
            ("resource", &resource),
        ]);
        assert_eq!(t.status, 200, "token exchange: {}", t.dump());
        t.json()["access_token"].as_str().expect("access_token").to_string()
    }

    fn mcp_at(&self, rid: &str, bearer: &str, method: &str, params: Value) -> Resp {
        let body = json!({"jsonrpc":"2.0","id":1,"method":method,"params":params}).to_string();
        let auth = format!("Bearer {bearer}");
        http(
            self.port,
            "POST",
            &format!("/t/{rid}/mcp"),
            &[
                ("Content-Type", "application/json"),
                ("Accept", "application/json, text/event-stream"),
                ("Authorization", &auth),
            ],
            body.as_bytes(),
        )
    }

    fn call(&self, bearer: &str, tool: &str, args: Value) -> Resp {
        self.mcp_at(&self.rid, bearer, "tools/call", json!({"name": tool, "arguments": args}))
    }

    fn recall(&self, bearer: &str, query: &str) -> Vec<String> {
        let r = self.call(bearer, "recall_memory", json!({"query": query, "limit": 5}));
        assert!(mcp_ok(&r), "recall should succeed: {}", r.dump());
        let v = r.json();
        let text = v["result"]["content"][0]["text"].as_str().expect("content text").to_string();
        let inner: Value = serde_json::from_str(&text).unwrap_or_else(|_| panic!("recall text not JSON: {text}"));
        inner["results"]
            .as_array()
            .unwrap_or_else(|| panic!("no results array: {inner}"))
            .iter()
            .map(|x| x["text"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

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

// ───────────────────────────── raw signed device API ─────────────────────────────

struct Signer2 {
    sk: SigningKey,
}

impl Signer2 {
    fn new(secret: [u8; 32]) -> Signer2 {
        Signer2 { sk: SigningKey::from_bytes(&secret) }
    }
    fn pub_b64(&self) -> String {
        STANDARD.encode(self.sk.verifying_key().to_bytes())
    }
    /// Build signed headers for (method, path, body) at `ts` with `nonce`.
    fn headers(&self, method: &str, path: &str, body: &[u8], ts: i64, nonce: &str) -> Vec<(String, String)> {
        let sig = self.sk.sign(&canonical(method, path, body, ts, nonce));
        vec![
            (H_KEY.to_string(), self.pub_b64()),
            (H_TS.to_string(), ts.to_string()),
            (H_NONCE.to_string(), nonce.to_string()),
            (H_SIG.to_string(), STANDARD.encode(sig.to_bytes())),
        ]
    }
}

fn now() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() as i64
}

fn nonce() -> String {
    rand_b64url(16)
}

/// Send `body` with headers signed over `signed_body` (normally the same).
#[allow(clippy::too_many_arguments)]
fn send_signed(
    port: u16,
    s: &Signer2,
    method: &str,
    path: &str,
    signed_body: &[u8],
    body: &[u8],
    ts: i64,
    nonce: &str,
) -> Resp {
    let hs = s.headers(method, path, signed_body, ts, nonce);
    let mut h: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    if !body.is_empty() {
        h.push(("Content-Type", "application/json"));
    }
    http(port, method, path, &h, body)
}

/// Register a tenant through the public client; returns (secret, rid, Device).
fn new_tenant(cloud: &Cloud) -> ([u8; 32], String, Device) {
    let secret = Device::generate_secret();
    let mut d = Device::new(secret, None, cloud.url());
    let rid = d.register().expect("register tenant");
    (secret, rid, d)
}

/// The tenant directory behind a public (Muse-facing) id. Spec revision: the device manages
/// its tenant by a stable management id; Muse's URL carries a public id that rotates on
/// every enrollment (`data/public/<pid>` → management id).
fn tenant_dir_of(data: &Path, pid: &str) -> PathBuf {
    let mid = std::fs::read_to_string(data.join("public").join(pid)).unwrap_or_default();
    data.join("tenants").join(mid.trim())
}

/// Export snapshots carry a strictly increasing version (spec revision after review).
fn next_version() -> i64 {
    use std::sync::atomic::{AtomicI64, Ordering};
    static V: AtomicI64 = AtomicI64::new(0);
    let base = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_millis() as i64;
    let _ = V.compare_exchange(0, base, Ordering::SeqCst, Ordering::SeqCst);
    V.fetch_add(1, Ordering::SeqCst) + 1
}

fn items(texts: &[&str]) -> Vec<(String, Option<Vec<f32>>)> {
    texts.iter().map(|t| (t.to_string(), None)).collect()
}

/// All regular files under `dir`, recursively.
fn files_under(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let mut stack = vec![dir.to_path_buf()];
    while let Some(d) = stack.pop() {
        if let Ok(rd) = std::fs::read_dir(&d) {
            for e in rd.flatten() {
                let p = e.path();
                if p.is_dir() {
                    stack.push(p);
                } else if p.is_file() {
                    out.push(p);
                }
            }
        }
    }
    out
}

fn contains_bytes(hay: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && hay.windows(needle.len()).any(|w| w == needle)
}

/// Panic if any file under `dir` contains `needle` in plaintext.
fn assert_not_on_disk(dir: &Path, needle: &str, what: &str) {
    for f in files_under(dir) {
        let bytes = std::fs::read(&f).unwrap_or_default();
        assert!(
            !contains_bytes(&bytes, needle.as_bytes()),
            "{what}: plaintext {needle:?} found on disk in {}",
            f.display()
        );
    }
}

// ──────────────────────────────── tests ────────────────────────────────

/// 1. Happy path: muse_connect → link → Muse discovery, DCR, consent, token → recall finds the share.
#[test]
fn happy_path_connect_and_recall() {
    let tmp = TempDir::new("happy");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);

    let out = dev.connect_texts(&["My dog is named Biscuit", "I drink oolong tea every morning"]);
    let link = out["link"].as_str().expect("link").to_string();
    assert_eq!(out["shared"], 2, "{out}");
    assert_eq!(out["expires_in_minutes"], 30, "{out}");
    assert!(out["tell_the_user"].as_str().unwrap_or_default().contains(&link), "tell_the_user carries the link: {out}");
    let rid = rid_of(&link);
    // 128-bit random id, URL-safe charset.
    assert!(rid.len() >= 22, "rid should be >= 128 bits of base64url: {rid}");
    assert!(rid.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'), "rid charset: {rid}");

    let muse = Muse::new(&cloud, &rid);

    // Unauthenticated /mcp points at the tenant's PRM.
    let r = http(cloud.port, "POST", &muse.p("/mcp"), &[("Content-Type", "application/json")], b"{}");
    assert_eq!(r.status, 401, "{}", r.dump());
    let wa = r.header("www-authenticate").unwrap_or_default();
    assert!(wa.contains(&format!("{BASE}/t/{rid}")), "WWW-Authenticate names the tenant's PRM: {wa}");

    // Discovery at inserted and appended forms.
    for path in [
        format!("/.well-known/oauth-authorization-server/t/{rid}"),
        format!("/t/{rid}/.well-known/oauth-authorization-server"),
    ] {
        let r = get(cloud.port, &path);
        assert_eq!(r.status, 200, "{path}: {}", r.dump());
        let j = r.json();
        assert_eq!(j["issuer"], muse.issuer(), "{path}: {j}");
        assert_eq!(j["authorization_endpoint"], format!("{}/authorize", muse.issuer()));
        assert_eq!(j["token_endpoint"], format!("{}/token", muse.issuer()));
        assert_eq!(j["registration_endpoint"], format!("{}/register", muse.issuer()));
    }
    for path in [
        format!("/.well-known/oauth-protected-resource/t/{rid}/mcp"),
        format!("/t/{rid}/.well-known/oauth-protected-resource/mcp"),
    ] {
        let r = get(cloud.port, &path);
        assert_eq!(r.status, 200, "{path}: {}", r.dump());
        let j = r.json();
        assert_eq!(j["resource"], muse.resource(), "{path}: {j}");
        assert_eq!(j["authorization_servers"], json!([muse.issuer()]), "{path}: {j}");
    }

    let token = muse.sign_in();
    let hits = muse.recall(&token, "dog name");
    assert!(hits.iter().any(|t| t.contains("Biscuit")), "recall finds the shared text: {hits:?}");

    // The device sees the connection.
    let st = dev.status();
    assert_eq!(st["connected"], true, "{st}");
    assert_eq!(st["shared"].as_array().map(|a| a.len()), Some(2), "{st}");
}

/// Spec revision after review (pairing code dropped: a per-window code is visible to a
/// racer too). Instead, a late click on a used link says it was used and how to recover.
#[test]
fn used_link_says_it_was_used() {
    let tmp = TempDir::new("usedlink");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let out = dev.connect_texts(&["Used link check memory"]);
    let rid = rid_of(out["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &rid);
    let _token = muse.sign_in();
    let client = muse.register();
    let c = muse.authorize(&client, &URL_SAFE_NO_PAD.encode(sha256(b"v")), "s");
    assert!(c.resp.location().is_none(), "no redirect: {}", c.resp.dump());
    assert!(c.body.contains("already used"), "page must say the link was used: {}", c.body);
    assert!(c.body.to_lowercase().contains("connect muse again"), "page must say how to recover: {}", c.body);
}

/// Sharing needs the user's confirmation: the first call shares nothing.
#[test]
fn sharing_requires_confirmation() {
    let tmp = TempDir::new("confirm");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let first = dev.call("muse_connect", json!({"texts": ["Confirmation check memory"]})).json();
    assert_eq!(first["needs_confirmation"], json!(true), "{first}");
    assert!(first.get("link").is_none(), "no link before confirmation: {first}");
    assert_eq!(dev.status()["shared"].as_array().map(Vec::len), Some(0), "nothing shared yet");
    // A wrong code, or the right code for different items, shares nothing.
    let bad = dev.call("muse_connect", json!({"texts": ["Confirmation check memory"], "confirmation": "nope"})).json();
    assert_eq!(bad["needs_confirmation"], json!(true), "{bad}");
    let code = bad["confirmation"].as_str().unwrap().to_string();
    let other = dev
        .call("muse_connect", json!({"texts": ["Something else"], "confirmation": code}))
        .json();
    assert_eq!(other["needs_confirmation"], json!(true), "code is bound to the exact list: {other}");
    let ok = dev.connect_texts(&["Confirmation check memory"]);
    assert!(ok["link"].is_string(), "{ok}");
}

/// 2. Enrollment window: one Allow closes it; a new muse_connect revokes the old grant and reopens.
#[test]
fn enrollment_window_single_use_and_reconnect_revokes() {
    let tmp = TempDir::new("window");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let out = dev.connect_texts(&["Window test: favourite colour is teal"]);
    let rid = rid_of(out["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &rid);
    let token1 = muse.sign_in();
    assert!(muse.recall(&token1, "favourite colour").iter().any(|t| t.contains("teal")));

    // Window closed: a second authorize shows no consent form and does not redirect.
    let client = muse.register();
    let c = muse.authorize(&client, &URL_SAFE_NO_PAD.encode(sha256(b"x")), "st");
    assert!(c.resp.location().is_none(), "no redirect outside a window: {}", c.resp.dump());
    assert!(c.req.is_none() && c.csrf.is_none(), "no consent form outside a window: {}", c.body);
    assert!(!c.body.contains("/authorize/approve"), "no approve form: {}", c.body);

    // New link → old token revoked, new window opens.
    let out2 = dev.connect_texts(&[]);
    let link2 = out2["link"].as_str().expect("link on reconnect").to_string();
    // Spec revision (Codex): every enrollment moves the tenant to a fresh URL, so an earlier
    // (possibly leaked) link can never be used in a later window.
    let rid2 = rid_of(&link2);
    assert_ne!(rid2, rid, "fresh URL on reconnect");
    let r = muse.call(&token1, "recall_memory", json!({"query":"colour"}));
    assert!(matches!(r.status, 401 | 404), "old Muse token must be revoked by a new enrollment: {}", r.dump());
    let muse2 = Muse::new(&cloud, &rid2);
    let r = muse2.call(&token1, "recall_memory", json!({"query":"colour"}));
    assert_eq!(r.status, 401, "old token useless on the new URL too: {}", r.dump());
    let old = get(cloud.port, &format!("/.well-known/oauth-authorization-server/t/{rid}"));
    assert_eq!(old.status, 404, "the old link is dead, it can't reach the new window: {}", old.dump());
    let token2 = muse2.sign_in();
    assert!(muse2.recall(&token2, "favourite colour").iter().any(|t| t.contains("teal")));
}

/// 3. Consent CSRF: missing cookie / mismatched csrf / foreign Origin are refused without
///    consuming the window; GETs never consume it.
#[test]
fn consent_csrf_and_get_never_consumes() {
    let tmp = TempDir::new("csrf");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let out = dev.connect_texts(&["CSRF test memory about kayaking"]);
    let rid = rid_of(out["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &rid);
    let client = muse.register();

    // Two GETs (link preview + real), plus a HEAD: none consume the window.
    let verifier = rand_b64url(32);
    let challenge = URL_SAFE_NO_PAD.encode(sha256(verifier.as_bytes()));
    let first = muse.authorize(&client, &challenge, "s1");
    assert_eq!(first.status, 200, "{}", first.resp.dump());
    let _ = http(cloud.port, "HEAD", &format!("/t/{rid}/mcp"), &[], b"");
    let c = muse.authorize(&client, &challenge, "s2");
    assert_eq!(c.status, 200, "second GET still shows consent: {}", c.resp.dump());
    let req = c.req.clone().expect("req");
    let csrf = c.csrf.clone().expect("csrf");
    let cookie = c.cookie.clone().expect("cookie");
    let set_cookie = c.resp.headers_all("set-cookie").join(" | ").to_ascii_lowercase();
    assert!(set_cookie.contains("httponly"), "csrf cookie HttpOnly: {set_cookie}");
    assert!(set_cookie.contains("secure"), "csrf cookie Secure: {set_cookie}");
    assert!(set_cookie.contains("samesite"), "csrf cookie SameSite: {set_cookie}");
    let csp = c.resp.header("content-security-policy").unwrap_or_default();
    assert!(csp.contains("frame-ancestors 'none'"), "consent page frame-ancestors none: {csp}");
    // Found in a real browser: form-action also governs the redirects after the POST, so
    // the client's callback origin must be allowed or sign-in never completes.
    assert!(csp.contains("form-action 'self' https://agent.meta.ai"), "consent CSP must allow the callback hop: {csp}");
    assert!(c.resp.header("cache-control").unwrap_or_default().contains("no-store"));
    // Spec revision (Codex): `no-referrer` makes browsers send `Origin: null` on the Allow
    // POST, so the consent page uses `same-origin` (nothing leaks to other sites).
    assert_eq!(c.resp.header("referrer-policy"), Some("same-origin"));

    let refused = |r: &Resp, what: &str| {
        assert!(
            !(r.status == 303 || r.status == 302) || !r.location().unwrap_or_default().contains("wait"),
            "{what}: must not approve: {}",
            r.dump()
        );
        assert!(r.status >= 400, "{what}: expected a 4xx refusal: {}", r.dump());
        // The pending request must not be approved: polling the wait page yields no code.
        let w = get(cloud.port, &format!("/t/{rid}/authorize/wait?req={}", pct(&req)));
        if let Some(loc) = w.location() {
            assert!(!loc.contains("code="), "{what}: wait page must not deliver a code: {}", w.dump());
        }
    };
    refused(&muse.approve(&req, &csrf, None, Some(BASE)), "no cookie");
    refused(&muse.approve(&req, "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA", Some(&cookie), Some(BASE)), "bad csrf");
    refused(&muse.approve(&req, &csrf, Some(&cookie), Some("https://evil.example")), "foreign Origin");
    refused(&muse.approve(&req, &csrf, Some(&cookie), Some("null")), "null Origin");
    // Cookie from another consent page with this page's csrf → mismatch.
    let other = muse.authorize(&client, &challenge, "s3");
    if let Some(oc) = other.cookie.as_deref() {
        if Some(oc) != Some(cookie.as_str()) {
            refused(&muse.approve(&req, &csrf, Some(oc), Some(BASE)), "cookie of another page");
        }
    }

    // The window survives all of the above: a proper Allow on a fresh consent page works.
    let token = muse.sign_in();
    assert!(muse.recall(&token, "kayaking").iter().any(|t| t.contains("kayaking")));
}

/// Spec ("CSRF token + Origin required"): an Allow POST without any Origin header is refused.
#[test]
fn consent_allow_without_origin_is_refused() {
    let tmp = TempDir::new("noorigin");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let rid = rid_of(dev.connect_texts(&["Origin check memory"])["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &rid);
    let client = muse.register();
    let c = muse.authorize(&client, &URL_SAFE_NO_PAD.encode(sha256(b"v")), "s");
    let r = muse.approve(c.req.as_deref().unwrap(), c.csrf.as_deref().unwrap(), c.cookie.as_deref(), None);
    assert!(r.status >= 400, "Allow without Origin must be refused: {}", r.dump());
}

/// 4. Cross-tenant: A's Muse token is useless on B; A's device key cannot call B's API.
#[test]
fn cross_tenant_isolation() {
    let tmp = TempDir::new("xtenant");
    let cloud = Cloud::start(tmp.path());
    let dev_a = Dev::new(&tmp.path().join("a"), &cloud);
    let dev_b = Dev::new(&tmp.path().join("b"), &cloud);
    let rid_a = rid_of(dev_a.connect_texts(&["Tenant A secret: alpha-pineapple"])["link"].as_str().unwrap());
    let rid_b = rid_of(dev_b.connect_texts(&["Tenant B secret: bravo-mango"])["link"].as_str().unwrap());
    assert_ne!(rid_a, rid_b);
    let muse_a = Muse::new(&cloud, &rid_a);
    let muse_b = Muse::new(&cloud, &rid_b);
    let ta = muse_a.sign_in();
    let tb = muse_b.sign_in();

    let r = muse_a.mcp_at(&rid_b, &ta, "tools/call", json!({"name":"recall_memory","arguments":{"query":"secret"}}));
    assert_eq!(r.status, 401, "A's token on B: {}", r.dump());
    assert!(!r.body.contains("bravo"), "no B data leaks");
    let r = muse_b.mcp_at(&rid_a, &tb, "tools/list", json!({}));
    assert_eq!(r.status, 401, "B's token on A: {}", r.dump());
    // Each sees only its own.
    let a_hits = muse_a.recall(&ta, "secret");
    assert!(a_hits.iter().all(|t| !t.contains("bravo")), "{a_hits:?}");
    let b_hits = muse_b.recall(&tb, "secret");
    assert!(b_hits.iter().all(|t| !t.contains("alpha")), "{b_hits:?}");

    // Device-API: A's key signing a request for B's tenant → 401.
    let (sa, rid_x, _da) = new_tenant(&cloud);
    let (_sb, rid_y, db) = new_tenant(&cloud);
    db.push_export(next_version(), &items(&["Y only"])).expect("push Y");
    let s = Signer2::new(sa);
    for (m, p) in [
        ("GET", format!("/api/tenants/{rid_y}/inbox")),
        ("POST", format!("/api/tenants/{rid_y}/enroll")),
        ("DELETE", format!("/api/tenants/{rid_y}")),
    ] {
        let r = send_signed(cloud.port, &s, m, &p, b"", b"", now(), &nonce());
        assert_eq!(r.status, 401, "A's key on B {m} {p}: {}", r.dump());
    }
    let body = json!({"items":[{"text":"injected by A"}]}).to_string();
    let p = format!("/api/tenants/{rid_y}/export");
    let r = send_signed(cloud.port, &s, "PUT", &p, body.as_bytes(), body.as_bytes(), now(), &nonce());
    assert_eq!(r.status, 401, "A's key PUT export on B: {}", r.dump());
    // B still intact and A's own tenant still usable.
    assert!(db.status().is_ok(), "B still alive after A's attempts");
    let _ = rid_x;
}

/// 5a. Device API signatures: replay, stale ts, tampered body, wrong key, unknown rid, key mismatch.
#[test]
fn device_api_signature_checks() {
    let tmp = TempDir::new("sig");
    let cloud = Cloud::start(tmp.path());
    let (secret, rid, dev) = new_tenant(&cloud);
    dev.push_export(next_version(), &items(&["sig test item"])).expect("push");
    let s = Signer2::new(secret);
    let inbox = format!("/api/tenants/{rid}/inbox");

    // Sanity: a correctly signed raw request works.
    let r = send_signed(cloud.port, &s, "GET", &inbox, b"", b"", now(), &nonce());
    assert_eq!(r.status, 200, "valid signed inbox GET: {}", r.dump());

    // Replay of the exact same request (same nonce) → 401.
    let (ts, n) = (now(), nonce());
    let r1 = send_signed(cloud.port, &s, "GET", &inbox, b"", b"", ts, &n);
    assert_eq!(r1.status, 200, "{}", r1.dump());
    let r2 = send_signed(cloud.port, &s, "GET", &inbox, b"", b"", ts, &n);
    assert_eq!(r2.status, 401, "replayed nonce: {}", r2.dump());

    // Stale / future timestamps (> 300 s skew) → 401.
    let r = send_signed(cloud.port, &s, "GET", &inbox, b"", b"", now() - 1000, &nonce());
    assert_eq!(r.status, 401, "stale ts: {}", r.dump());
    let r = send_signed(cloud.port, &s, "GET", &inbox, b"", b"", now() + 1000, &nonce());
    assert_eq!(r.status, 401, "future ts: {}", r.dump());

    // Tampered body: signature over one body, a different body sent → 401.
    let export = format!("/api/tenants/{rid}/export");
    let good = json!({"items":[{"text":"original"}]}).to_string();
    let evil = json!({"items":[{"text":"tampered"}]}).to_string();
    let r = send_signed(cloud.port, &s, "PUT", &export, good.as_bytes(), evil.as_bytes(), now(), &nonce());
    assert_eq!(r.status, 401, "tampered body: {}", r.dump());

    // Tampered path/query: signed for one path, sent to another.
    let hs = s.headers("GET", &format!("{inbox}?a=1"), b"", now(), &nonce());
    let h: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r = http(cloud.port, "GET", &format!("{inbox}?a=2"), &h, b"");
    assert_eq!(r.status, 401, "tampered query: {}", r.dump());

    // Bad signature bytes.
    let mut hs = s.headers("GET", &inbox, b"", now(), &nonce());
    for (k, v) in hs.iter_mut() {
        if k == H_SIG {
            *v = STANDARD.encode([7u8; 64]);
        }
    }
    let h: Vec<(&str, &str)> = hs.iter().map(|(k, v)| (k.as_str(), v.as_str())).collect();
    let r = http(cloud.port, "GET", &inbox, &h, b"");
    assert_eq!(r.status, 401, "garbage signature: {}", r.dump());

    // Missing headers.
    let r = get(cloud.port, &inbox);
    assert_eq!(r.status, 401, "unsigned request: {}", r.dump());

    // Unregistered key on a real tenant → 401.
    let stranger = Signer2::new(Device::generate_secret());
    let r = send_signed(cloud.port, &stranger, "GET", &inbox, b"", b"", now(), &nonce());
    assert_eq!(r.status, 401, "unknown key: {}", r.dump());

    // Unknown (well-formed) rid → 404.
    let unknown = rand_b64url(16);
    let r = send_signed(cloud.port, &s, "GET", &format!("/api/tenants/{unknown}/inbox"), b"", b"", now(), &nonce());
    assert_eq!(r.status, 404, "unknown rid: {}", r.dump());

    // Register with a body public_key that is not the signing key → 400.
    let other = Signer2::new(Device::generate_secret());
    let body = json!({"public_key": other.pub_b64()}).to_string();
    let r = send_signed(cloud.port, &s, "POST", "/api/tenants", body.as_bytes(), body.as_bytes(), now(), &nonce());
    assert_eq!(r.status, 400, "register with mismatched public_key: {}", r.dump());
    // Control: the same body shape with the right key is accepted (proves the 400 is about the key).
    let fresh = Signer2::new(Device::generate_secret());
    let body = json!({"public_key": fresh.pub_b64()}).to_string();
    let r = send_signed(cloud.port, &fresh, "POST", "/api/tenants", body.as_bytes(), body.as_bytes(), now(), &nonce());
    assert!((200..300).contains(&r.status), "register control (assumed body shape {{public_key}}): {}", r.dump());
}

/// 5b. Replay protection survives a restart on the same data dir.
#[test]
fn replay_rejected_after_restart() {
    let tmp = TempDir::new("restart");
    let cloud = Cloud::start(tmp.path());
    let (secret, rid, _dev) = new_tenant(&cloud);
    let s = Signer2::new(secret);
    let inbox = format!("/api/tenants/{rid}/inbox");
    let (ts, n) = (now(), nonce());
    let r = send_signed(cloud.port, &s, "GET", &inbox, b"", b"", ts, &n);
    assert_eq!(r.status, 200, "{}", r.dump());
    let (data, master) = (cloud.data.clone(), cloud.master.clone());
    cloud.stop();
    assert!(data.exists() && master.exists());
    let cloud2 = Cloud::start(tmp.path());
    let r = send_signed(cloud2.port, &s, "GET", &inbox, b"", b"", ts, &n);
    assert_eq!(r.status, 401, "replay after restart: {}", r.dump());
    // A fresh nonce still works after restart (tenant + key persisted).
    let r = send_signed(cloud2.port, &s, "GET", &inbox, b"", b"", now(), &nonce());
    assert_eq!(r.status, 200, "fresh request after restart: {}", r.dump());
}

/// 6. Unshare: muse_unshare removes an item; Muse recall stops returning it.
#[test]
fn unshare_removes_from_muse() {
    let tmp = TempDir::new("unshare");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let rid = rid_of(
        dev.connect_texts(&["Unshare me: my locker code is 9182", "Keep me: I like jazz piano"])["link"]
            .as_str()
            .unwrap(),
    );
    let muse = Muse::new(&cloud, &rid);
    let token = muse.sign_in();
    assert!(muse.recall(&token, "locker code").iter().any(|t| t.contains("locker")));

    let st = dev.status();
    let id = st["shared"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["text"].as_str().unwrap_or_default().contains("locker"))
        .and_then(|x| x["id"].as_str())
        .expect("shared id for locker")
        .to_string();
    let out = dev.call("muse_unshare", json!({"shared_ids":[id]}));
    assert!(!out.is_error, "muse_unshare: {}", out.text);

    let st = dev.status();
    let texts: Vec<String> =
        st["shared"].as_array().unwrap().iter().map(|x| x["text"].as_str().unwrap_or_default().to_string()).collect();
    assert!(!texts.iter().any(|t| t.contains("locker")), "status no longer lists it: {st}");
    assert!(texts.iter().any(|t| t.contains("jazz")), "others stay shared: {st}");

    let hits = muse.recall(&token, "locker code");
    assert!(!hits.iter().any(|t| t.contains("locker")), "Muse must no longer see it: {hits:?}");
    assert!(muse.recall(&token, "jazz piano").iter().any(|t| t.contains("jazz")));

    // muse_share adds a new item that Muse then sees.
    let out = dev.call_confirmed("muse_share", json!({"texts":["Newly shared: I run on Tuesdays"]}));
    assert!(!out.is_error, "muse_share: {}", out.text);
    assert!(muse.recall(&token, "run Tuesdays").iter().any(|t| t.contains("Tuesdays")));
}

/// 7. remember → inbox (encrypted at rest, not searchable) → keep → recall returns it;
///    shared export texts never on disk in plaintext.
#[test]
fn remember_inbox_keep_and_encrypted_at_rest() {
    let tmp = TempDir::new("remember");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let shared = "Shared plaintext probe QUOKKA-7731 lives here";
    let rid = rid_of(dev.connect_texts(&[shared])["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &rid);
    let token = muse.sign_in();

    let proposal = "Muse proposal PANGOLIN-4402 user prefers window seats";
    let r = muse.call(&token, "remember", json!({"text": proposal}));
    assert!(mcp_ok(&r), "remember: {}", r.dump());
    let r2 = muse.call(&token, "remember", json!({"text": "Muse proposal WOMBAT-1188 discard me"}));
    assert!(mcp_ok(&r2), "remember 2: {}", r2.dump());

    // Not searchable before approval.
    let hits = muse.recall(&token, "PANGOLIN window seats");
    assert!(!hits.iter().any(|t| t.contains("PANGOLIN")), "inbox must not be searchable: {hits:?}");

    // Nothing in plaintext on the cloud's disk.
    assert_not_on_disk(&cloud.data, "PANGOLIN-4402", "inbox text");
    assert_not_on_disk(&cloud.data, "QUOKKA-7731", "shared export text");
    assert_not_on_disk(&cloud.data, "WOMBAT-1188", "inbox text 2");

    let st = dev.status();
    assert_eq!(st["waiting_in_inbox"], 2, "{st}");
    let inbox = dev.call("muse_inbox", json!({})).json();
    let list = inbox["items"].as_array().unwrap_or_else(|| panic!("items: {inbox}"));
    let keep = list
        .iter()
        .find(|x| x["text"].as_str().unwrap_or_default().contains("PANGOLIN"))
        .and_then(|x| x["id"].as_str())
        .unwrap_or_else(|| panic!("proposal in inbox: {inbox}"))
        .to_string();
    let discard = list
        .iter()
        .find(|x| x["text"].as_str().unwrap_or_default().contains("WOMBAT"))
        .and_then(|x| x["id"].as_str())
        .unwrap_or_else(|| panic!("proposal 2 in inbox: {inbox}"))
        .to_string();
    let res = dev.call("muse_inbox", json!({"keep":[keep], "discard":[discard]})).json();
    assert_eq!(res["kept"], 1, "{res}");

    // Cloud inbox emptied (acked).
    let after = dev.call("muse_inbox", json!({})).json();
    assert_eq!(after["items"].as_array().map(|a| a.len()), Some(0), "inbox emptied: {after}");

    // Kept → in export → Muse recall finds it; discarded one never appears.
    let hits = muse.recall(&token, "PANGOLIN window seats");
    assert!(hits.iter().any(|t| t.contains("PANGOLIN")), "kept proposal recalled: {hits:?}");
    let hits = muse.recall(&token, "WOMBAT discard");
    assert!(!hits.iter().any(|t| t.contains("WOMBAT")), "discarded never recalled: {hits:?}");

    // Still no plaintext at rest after the re-push.
    assert_not_on_disk(&cloud.data, "PANGOLIN-4402", "kept text in export");
    assert_not_on_disk(&cloud.data, "QUOKKA-7731", "shared export text");
}

/// 8. muse_disconnect: tokens dead, tenant dir gone, status shows disconnected.
#[test]
fn disconnect_wipes_tenant() {
    let tmp = TempDir::new("disconnect");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let rid = rid_of(dev.connect_texts(&["Disconnect test memory"])["link"].as_str().unwrap());
    let muse = Muse::new(&cloud, &rid);
    let token = muse.sign_in();
    assert!(mcp_ok(&muse.call(&token, "recall_memory", json!({"query":"disconnect"}))));
    let tenant_dir = tenant_dir_of(&cloud.data, &rid);
    let had_dir = tenant_dir.exists();

    let out = dev.call("muse_disconnect", json!({})).json();
    assert_eq!(out["disconnected"], true, "{out}");

    let r = muse.call(&token, "recall_memory", json!({"query":"disconnect"}));
    assert!(matches!(r.status, 401 | 404), "token after disconnect: {}", r.dump());
    assert!(!r.body.contains("Disconnect test"), "no data after disconnect");
    if had_dir {
        assert!(!tenant_dir.exists(), "tenant directory removed: {}", tenant_dir.display());
    }
    // Wherever it lived, the rid must not remain anywhere under the data dir as a directory.
    for f in files_under(&cloud.data) {
        assert!(!f.to_string_lossy().contains(&rid), "leftover tenant file {}", f.display());
    }
    let r = get(cloud.port, &format!("/.well-known/oauth-protected-resource/t/{rid}/mcp"));
    assert_eq!(r.status, 404, "rid gone from discovery: {}", r.dump());
    let st = dev.status();
    assert_eq!(st["connected"], false, "{st}");
}

/// 9. Push limits: > 1000 items refused (old export kept); empty text refused.
#[test]
fn push_limits() {
    let tmp = TempDir::new("limits");
    let cloud = Cloud::start(tmp.path());
    let (_s, _rid, dev) = new_tenant(&cloud);
    dev.push_export(next_version(), &items(&["baseline item one"])).expect("baseline push");

    let many: Vec<(String, Option<Vec<f32>>)> = (0..1001).map(|i| (format!("bulk item {i}"), None)).collect();
    assert!(dev.push_export(next_version(), &many).is_err(), "1001 items must be refused");
    let exactly: Vec<(String, Option<Vec<f32>>)> = (0..1000).map(|i| (format!("bulk item {i}"), None)).collect();
    assert!(dev.push_export(next_version(), &exactly).is_ok(), "1000 items is within the limit");

    assert!(dev.push_export(next_version(), &items(&[""])).is_err(), "empty text refused");
}

/// 10. Unknown / malformed tenant paths → 404.
#[test]
fn unknown_and_malformed_tenant_paths_404() {
    let tmp = TempDir::new("paths");
    let cloud = Cloud::start(tmp.path());
    let (_s, _mid, dev) = new_tenant(&cloud);
    // Muse-facing paths use the public id from the link.
    let rid = rid_of(&dev.enroll().expect("enroll").0);
    let unknown = rand_b64url(16);
    let paths = [
        format!("/t/{unknown}/mcp"),
        format!("/t/{unknown}/authorize?response_type=code"),
        format!("/.well-known/oauth-authorization-server/t/{unknown}"),
        format!("/.well-known/oauth-protected-resource/t/{unknown}/mcp"),
        format!("/t/{unknown}/.well-known/oauth-authorization-server"),
        "/t/../x/mcp".to_string(),
        "/t/%2e%2e/mcp".to_string(),
        "/t/..%2f..%2fetc/mcp".to_string(),
        "/t/abc/mcp".to_string(),
        "/t/short/.well-known/oauth-authorization-server".to_string(),
        format!("/t/{}!/mcp", &rid[..rid.len() - 1]),
        format!("/t/{}x/mcp", rid),
    ];
    for p in &paths {
        let r = get(cloud.port, p);
        assert_eq!(r.status, 404, "GET {p}: {}", r.dump());
        let r = http(cloud.port, "POST", p, &[("Content-Type", "application/json")], b"{}");
        assert!(matches!(r.status, 404 | 405), "POST {p}: {}", r.dump());
        assert_ne!(r.status, 401, "POST {p} must not look like a live tenant");
    }
    let r = http(
        cloud.port,
        "POST",
        &format!("/t/{unknown}/register"),
        &[("Content-Type", "application/json")],
        json!({"redirect_uris":[MUSE_CB]}).to_string().as_bytes(),
    );
    assert_eq!(r.status, 404, "DCR on unknown tenant: {}", r.dump());
    // Control: the real tenant resolves.
    let r = get(cloud.port, &format!("/.well-known/oauth-authorization-server/t/{rid}"));
    assert_eq!(r.status, 200, "{}", r.dump());
}

// ── Regressions from the adversarial review (written by the implementer) ─────

#[test]
fn review_stale_export_snapshot_cannot_resurrect_an_unshared_item() {
    let tmp = TempDir::new("rv-stale");
    let cloud = Cloud::start(tmp.path());
    let (_, rid, dev) = new_tenant(&cloud);
    let old = next_version();
    let new = next_version();
    dev.push_export(new, &items(&["kept"])).expect("newer snapshot");
    let err = dev.push_export(old, &items(&["kept", "unshared later"])).expect_err("older snapshot refused");
    assert!(err.contains("409"), "{err}");
    let status = dev.status().unwrap();
    assert_eq!(status["shared"], json!(1), "{status} (rid {rid})");
}

#[test]
fn review_wrong_key_is_refused_before_the_body_is_read() {
    let tmp = TempDir::new("rv-prebody");
    let cloud = Cloud::start(tmp.path());
    let (_, rid, _) = new_tenant(&cloud);
    let stranger = Signer2::new(Device::generate_secret());
    let path = format!("/api/tenants/{rid}/export");
    // Declares a huge body but sends none: must be refused on headers alone, not wait.
    let hs = stranger.headers("PUT", &path, b"", now(), &nonce());
    let mut req = format!("PUT {path} HTTP/1.1\r\nHost: x\r\nContent-Length: 11000000\r\nConnection: close\r\n");
    for (k, v) in &hs {
        req.push_str(&format!("{k}: {v}\r\n"));
    }
    req.push_str("\r\n");
    let started = std::time::Instant::now();
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", cloud.port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(10))).unwrap();
    sock.write_all(req.as_bytes()).unwrap();
    let mut buf = [0u8; 64];
    let n = std::io::Read::read(&mut sock, &mut buf).unwrap_or(0);
    let head = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(head.starts_with("HTTP/1.1 401"), "{head}");
    assert!(started.elapsed() < Duration::from_secs(5), "answered without reading a body");
}

#[test]
fn review_register_replay_is_refused() {
    let tmp = TempDir::new("rv-regreplay");
    let cloud = Cloud::start(tmp.path());
    let s = Signer2::new(Device::generate_secret());
    let body = json!({ "public_key": s.pub_b64() }).to_string();
    let (ts, n) = (now(), nonce());
    let first = send_signed(cloud.port, &s, "POST", "/api/tenants", body.as_bytes(), body.as_bytes(), ts, &n);
    assert_eq!(first.status, 201, "{}", first.dump());
    let again = send_signed(cloud.port, &s, "POST", "/api/tenants", body.as_bytes(), body.as_bytes(), ts, &n);
    assert_eq!(again.status, 401, "replayed registration: {}", again.dump());
}

#[test]
fn review_wrong_embedding_dimension_is_refused() {
    let tmp = TempDir::new("rv-dim");
    let cloud = Cloud::start(tmp.path());
    let (_, _, dev) = new_tenant(&cloud);
    let err = dev.push_export(next_version(), &[("x".to_string(), Some(vec![0.1; 1000]))]).expect_err("refused");
    assert!(err.contains("400"), "{err}");
}

#[test]
fn review_client_recovers_when_the_cloud_lost_its_tenant() {
    let tmp = TempDir::new("rv-gone");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let first = dev.connect_texts(&["Recovery check memory"]);
    let rid = rid_of(first["link"].as_str().unwrap());
    // The service forgets the tenant (idle sweep, operator wipe, …).
    std::fs::remove_dir_all(tenant_dir_of(&tmp.path().join("data"), &rid)).unwrap();
    assert_eq!(dev.status()["connected"], json!(false));
    let again = dev.call("muse_connect", json!({})).json();
    let rid2 = rid_of(again["link"].as_str().unwrap_or_else(|| panic!("reconnects: {again}")));
    assert_ne!(rid, rid2, "a fresh tenant");
    let out = dev.call("muse_disconnect", json!({}));
    assert!(!out.is_error, "{}", out.text);
    // Disconnect is idempotent even if the cloud already forgot us.
    let out = dev.call("muse_disconnect", json!({}));
    assert!(!out.is_error, "{}", out.text);
}

#[test]
fn review_unshare_while_the_cloud_is_down_is_not_lost() {
    let tmp = TempDir::new("rv-unshare-down");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let link = dev.connect_texts(&["Gamma secret", "Delta public"]);
    let rid = rid_of(link["link"].as_str().unwrap());
    let token = Muse::new(&cloud, &rid).sign_in();
    let shared = dev.status()["shared"].as_array().unwrap().clone();
    let id_of = |t: &str| shared.iter().find(|s| s["text"] == json!(t)).unwrap()["id"].as_str().unwrap().to_string();
    let (gamma, delta) = (id_of("Gamma secret"), id_of("Delta public"));
    cloud.stop();
    // Two unshares while the cloud is down: both must survive until the next sync.
    assert!(dev.call("muse_unshare", json!({ "shared_ids": [gamma] })).is_error);
    assert!(dev.call("muse_unshare", json!({ "shared_ids": [delta] })).is_error);
    let cloud2 = Cloud::start(tmp.path());
    let dev2 = Dev::new(tmp.path(), &cloud2);
    let _ = dev2.call("muse_status", json!({}));
    let muse = Muse::new(&cloud2, &rid);
    for q in ["Gamma secret", "Delta public"] {
        let hits = muse.recall(&token, q);
        assert!(hits.is_empty(), "{q} must be gone after the retry: {hits:?}");
    }
}

#[test]
fn review_unshare_with_a_bad_id_changes_nothing() {
    let tmp = TempDir::new("rv-unshare-bad");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["Keep A", "Keep B"]);
    let id = dev.status()["shared"][0]["id"].as_str().unwrap().to_string();
    let out = dev.call("muse_unshare", json!({ "shared_ids": [id, "not-an-id"] }));
    assert!(out.is_error, "{}", out.text);
    assert_eq!(dev.status()["shared"].as_array().map(Vec::len), Some(2), "nothing removed locally");
}

#[test]
fn review_too_long_memory_is_refused_before_it_wedges_sync() {
    let tmp = TempDir::new("rv-toolong");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["Short one"]);
    let long = "x".repeat(2001);
    let out = dev.call_confirmed("muse_share", json!({ "texts": [long] }));
    assert!(out.is_error, "{}", out.text);
    let st = dev.status();
    assert_eq!(st["shared"].as_array().map(Vec::len), Some(1), "{st}");
    assert!(st["sync_error"].is_null(), "sync still healthy: {st}");
}

#[test]
fn review_two_databases_in_one_directory_have_separate_connections() {
    let tmp = TempDir::new("rv-twodb");
    let cloud = Cloud::start(tmp.path());
    let a = Dev::with_db(tmp.path(), &cloud, "work.db");
    let b = Dev::with_db(tmp.path(), &cloud, "home.db");
    let ra = rid_of(a.connect_texts(&["Work memory"])["link"].as_str().unwrap());
    let a_dir = tenant_dir_of(&tmp.path().join("data"), &ra);
    let rb = rid_of(b.connect_texts(&["Home memory"])["link"].as_str().unwrap());
    assert_ne!(ra, rb, "each database gets its own tenant");
    let out = b.call("muse_disconnect", json!({}));
    assert!(!out.is_error, "{}", out.text);
    assert_eq!(a.status()["connected"], json!(false), "a is not signed in yet, but still has a tenant");
    assert!(a_dir.join("device.pub").is_file(), "b's disconnect left a's tenant alone");
}

#[test]
fn review_retrying_a_failed_unshare_reaches_the_cloud() {
    let tmp = TempDir::new("rv-unshare-retry");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let link = dev.connect_texts(&["Secret alpha fact", "Public beta fact"]);
    let rid = rid_of(link["link"].as_str().unwrap());
    let token = Muse::new(&cloud, &rid).sign_in();
    let id = dev.status()["shared"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["text"] == json!("Secret alpha fact"))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    cloud.stop();
    let out = dev.call("muse_unshare", json!({ "shared_ids": [id.clone()] }));
    assert!(out.is_error, "push must fail while the cloud is down: {}", out.text);
    let cloud2 = Cloud::start(tmp.path());
    let dev2 = Dev::new(tmp.path(), &cloud2);
    // Retrying the same call completes the pending sync first.
    let _ = dev2.call("muse_unshare", json!({ "shared_ids": [id] }));
    let hits = Muse::new(&cloud2, &rid).recall(&token, "Secret alpha fact");
    assert!(hits.iter().all(|t| !t.contains("alpha")), "unshared item must be gone from the cloud: {hits:?}");
}

#[test]
fn review_missing_device_key_is_reported_not_replaced() {
    let tmp = TempDir::new("rv-key");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["Key check memory"]);
    let key = dev.db_dir().join("memory.db.muse-device.key");
    assert!(key.is_file(), "fallback key file exists (keychain disabled in tests)");
    std::fs::remove_file(&key).unwrap();
    let out = dev.call("muse_status", json!({}));
    assert!(out.is_error && out.text.contains("device key is unavailable"), "{}", out.text);
    assert!(!key.exists(), "no new key minted for an existing connection");
}

#[test]
fn review_reconnect_works_after_the_master_key_is_replaced() {
    let tmp = TempDir::new("rv-masterkey");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let first = dev.connect_texts(&["Master key check"]);
    let rid = rid_of(first["link"].as_str().unwrap());
    cloud.stop();
    std::fs::remove_file(tmp.path().join("master.key")).unwrap();
    let cloud2 = Cloud::start(tmp.path());
    let dev2 = Dev::new(tmp.path(), &cloud2);
    let again = dev2.call("muse_connect", json!({}));
    assert!(!again.is_error, "reconnect must recover: {}", again.text);
    let rid2 = rid_of(again.json()["link"].as_str().unwrap());
    assert_ne!(rid, rid2, "unreadable tenant replaced by a fresh one");
    let token = Muse::new(&cloud2, &rid2).sign_in();
    assert_eq!(Muse::new(&cloud2, &rid2).recall(&token, "Master key check"), vec!["Master key check".to_string()]);
}

#[test]
fn review_clock_set_back_does_not_wedge_syncing() {
    let tmp = TempDir::new("rv-clock");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let link = dev.connect_texts(&["Clock one", "Clock two"]);
    let rid = rid_of(link["link"].as_str().unwrap());
    // As if the last push happened while the device clock ran an hour fast.
    let future = now() * 1000 + 3_600_000;
    std::fs::write(tenant_dir_of(&tmp.path().join("data"), &rid).join("export.version"), future.to_string()).unwrap();
    let state_path = dev.db_dir().join("memory.db.muse-cloud.json");
    let mut st: Value = serde_json::from_str(&std::fs::read_to_string(&state_path).unwrap()).unwrap();
    st["last_version"] = json!(future);
    std::fs::write(&state_path, st.to_string()).unwrap();
    let id = dev.status()["shared"][0]["id"].as_str().unwrap().to_string();
    let out = dev.call("muse_unshare", json!({ "shared_ids": [id] }));
    assert!(!out.is_error, "unshare must still sync: {}", out.text);
}

#[test]
fn review_reconnect_revokes_before_publishing_new_shares() {
    let tmp = TempDir::new("rv-reconnect-order");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let rid = rid_of(dev.connect_texts(&["Old shared"])["link"].as_str().unwrap());
    let intruder = Muse::new(&cloud, &rid).sign_in();
    // Reconnecting with a new item: the earlier connection must never see it.
    dev.connect_texts(&["Brand new secret"]);
    let r = Muse::new(&cloud, &rid).mcp_at(&rid, &intruder, "tools/list", json!({}));
    assert!(matches!(r.status, 401 | 404), "earlier connection revoked: {}", r.dump());
}

#[test]
fn review_resharing_counts_only_new_items_against_the_cap() {
    let tmp = TempDir::new("rv-cap");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["seed"]);
    let texts: Vec<String> = (0..600).map(|i| format!("cap item {i}")).collect();
    let first = dev.call_confirmed("muse_share", json!({ "texts": texts }));
    assert!(!first.is_error, "{}", first.text);
    let again = dev.call_confirmed("muse_share", json!({ "texts": texts }));
    assert!(!again.is_error, "re-sharing the same 600 is a no-op, not over the cap: {}", again.text);
    assert_eq!(dev.status()["shared"].as_array().map(Vec::len), Some(601));
}

#[test]
fn review_failed_reconnect_never_queues_new_shares_for_the_old_connection() {
    let tmp = TempDir::new("rv-reconnect-fail");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let rid = rid_of(dev.connect_texts(&["Old shared"])["link"].as_str().unwrap());
    let old_conn = Muse::new(&cloud, &rid).sign_in();
    cloud.stop();
    // Reconnect with a new item while the cloud is unreachable: must fail without
    // touching the shared list.
    let preview = dev.call("muse_connect", json!({ "texts": ["New secret"] })).json();
    let code = preview["confirmation"].as_str().unwrap().to_string();
    let out = dev.call("muse_connect", json!({ "texts": ["New secret"], "confirmation": code }));
    assert!(out.is_error, "{}", out.text);
    let cloud2 = Cloud::start(tmp.path());
    let dev2 = Dev::new(tmp.path(), &cloud2);
    let st = dev2.status();
    assert_eq!(st["shared"].as_array().map(Vec::len), Some(1), "new item not added: {st}");
    let hits = Muse::new(&cloud2, &rid).recall(&old_conn, "New secret");
    assert!(hits.iter().all(|h| !h.contains("New secret")), "{hits:?}");
}

#[test]
fn review_blank_memory_is_refused_before_it_wedges_sync() {
    let tmp = TempDir::new("rv-blank");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["Normal"]);
    let id = dev.call("memory_ingest", json!({ "text": "   ", "channel": "t" })).json()["id"].as_str().unwrap().to_string();
    let out = dev.call("muse_share", json!({ "memory_ids": [id] }));
    assert!(out.is_error && out.text.contains("empty"), "{}", out.text);
    let st = dev.status();
    assert_eq!(st["shared"].as_array().map(Vec::len), Some(1), "{st}");
    assert!(st["sync_error"].is_null(), "{st}");
}

#[test]
fn review_lost_enroll_response_leaves_the_tenant_manageable() {
    let tmp = TempDir::new("rv-enroll-lost");
    let cloud = Cloud::start(tmp.path());
    let (_s, mid, dev) = new_tenant(&cloud);
    dev.push_export(next_version(), &items(&["kept item"])).unwrap();
    let (first, _) = dev.enroll().unwrap(); // pretend this response never arrived
    let (second, _) = dev.enroll().expect("the device can always enroll again");
    assert_ne!(rid_of(&first), rid_of(&second));
    assert_eq!(get(cloud.port, &format!("/.well-known/oauth-authorization-server/t/{}", rid_of(&first))).status, 404);
    assert_eq!(dev.status().unwrap()["shared"], json!(1), "data still reachable by the stable id");
    dev.delete().expect("delete reaches the tenant");
    assert!(!tmp.path().join("data/tenants").join(&mid).exists(), "nothing orphaned");
    assert_eq!(std::fs::read_dir(tmp.path().join("data/public")).unwrap().count(), 0, "no stale public ids");
}

#[test]
fn review_in_flight_request_cannot_read_what_is_shared_after_revocation() {
    use std::io::Read as _;
    let tmp = TempDir::new("rv-inflight");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let pid = rid_of(dev.connect_texts(&["Old item"])["link"].as_str().unwrap());
    let old_token = Muse::new(&cloud, &pid).sign_in();
    // The old connection authenticates now but holds its body back.
    let body = json!({"jsonrpc":"2.0","id":1,"method":"tools/call",
        "params":{"name":"recall_memory","arguments":{"query":"Freshly shared secret"}}})
    .to_string();
    let head = format!(
        "POST /t/{pid}/mcp HTTP/1.1\r\nHost: x\r\nAuthorization: Bearer {old_token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let mut sock = std::net::TcpStream::connect(("127.0.0.1", cloud.port)).unwrap();
    sock.set_read_timeout(Some(Duration::from_secs(20))).unwrap();
    sock.write_all(head.as_bytes()).unwrap();
    std::thread::sleep(Duration::from_millis(400));
    // Meanwhile the user reconnects and shares something new.
    dev.connect_texts(&["Freshly shared secret"]);
    sock.write_all(body.as_bytes()).unwrap();
    let mut resp = String::new();
    let _ = sock.read_to_string(&mut resp);
    assert!(!resp.contains("Freshly shared secret"), "revoked in-flight request read new data: {resp}");
}

#[test]
fn review_status_still_lists_shares_when_the_cloud_is_down() {
    let tmp = TempDir::new("rv-status-offline");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["Offline listing"]);
    cloud.stop();
    let st = dev.status();
    assert_eq!(st["shared"].as_array().map(Vec::len), Some(1), "{st}");
    assert!(st["cloud_error"].is_string(), "{st}");
}

#[test]
fn review_deleting_a_shared_memory_elsewhere_reaches_the_cloud() {
    let tmp = TempDir::new("rv-elsewhere");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let pid = rid_of(dev.connect_texts(&["Zeta secret", "Eta public"])["link"].as_str().unwrap());
    let token = Muse::new(&cloud, &pid).sign_in();
    let id = dev.status()["shared"]
        .as_array()
        .unwrap()
        .iter()
        .find(|s| s["text"] == json!("Zeta secret"))
        .unwrap()["id"]
        .as_str()
        .unwrap()
        .to_string();
    // Deleted through the ordinary memory tool, not muse_unshare.
    let out = dev.call("memory_delete", json!({ "id": id }));
    assert!(!out.is_error, "{}", out.text);
    let hits = Muse::new(&cloud, &pid).recall(&token, "Zeta secret");
    assert!(hits.iter().all(|h| !h.contains("Zeta")), "cloud must follow: {hits:?}");
}

#[test]
fn review_delete_reports_when_the_cloud_could_not_be_updated() {
    let tmp = TempDir::new("rv-delete-pending");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    dev.connect_texts(&["Theta secret"]);
    let id = dev.status()["shared"][0]["id"].as_str().unwrap().to_string();
    cloud.stop();
    let out = dev.call("memory_delete", json!({ "id": id })).json();
    assert!(out["muse_cloud"].as_str().unwrap_or_default().contains("NOT updated"), "{out}");
}

#[test]
fn review_deleting_the_original_also_unshares_its_copy() {
    let tmp = TempDir::new("rv-source-delete");
    let cloud = Cloud::start(tmp.path());
    let dev = Dev::new(tmp.path(), &cloud);
    let id = dev.call("memory_ingest", json!({ "text": "Iota original fact", "channel": "t" })).json()["id"]
        .as_str()
        .unwrap()
        .to_string();
    let link = dev.call_confirmed("muse_connect", json!({ "memory_ids": [id.clone()] })).json();
    let pid = rid_of(link["link"].as_str().unwrap());
    let token = Muse::new(&cloud, &pid).sign_in();
    let out = dev.call("memory_delete", json!({ "id": id })).json();
    assert_eq!(out["also_unshared_from_muse"], json!(1), "{out}");
    assert_eq!(dev.status()["shared"].as_array().map(Vec::len), Some(0));
    let hits = Muse::new(&cloud, &pid).recall(&token, "Iota original fact");
    assert!(hits.is_empty(), "{hits:?}");
}
