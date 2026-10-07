//! MCP tools that connect the user's memory to Muse through Cortex Cloud — the agent does
//! everything; the user only confirms what to share and pastes one link into Muse.
//! Design: `docs/design/muse-cloud.md`.
//!
//! ```text
//! muse_connect(ids) ─ copy ids into the local export ─ register device (first time)
//!                   ─ push the whole export ─ open a 30-min window ─▶ link for Muse
//! muse_share / muse_unshare ─ edit local export ─ push
//! muse_inbox ─ pull what Muse asked to remember ─ keep (Private memory + export) / discard
//! muse_disconnect ─ delete everything in the cloud
//! ```
//!
//! The local `muse-export` namespace stays the source of truth; the cloud only ever holds
//! a copy of it. The device key lives in the OS keychain (macOS), else in a 0600 file next
//! to the database. Sharing is two-step: the first call returns exactly what would be
//! shared plus a confirmation code; only a second call with that code shares it.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use cortex_core::types::PrivacyLevel;
use cortex_core::Cortex;

use super::cloud::{Device, DEFAULT_CLOUD_URL, GONE};
use super::{allow, export_rows, private_options, read_private, MAX_EXPORT_ITEMS, MAX_EXPORT_TEXT_CHARS};
use crate::tools::content_to_string;

// Per database (several databases may share one directory): `<stem>.muse-cloud.json`,
// `<stem>.muse-device.key`, `<stem>.muse.lock`.

const CONFIRM_TTL_SECS: i64 = 15 * 60;

#[derive(Serialize, Deserialize, Default)]
struct State {
    rid: Option<String>,
    base_url: Option<String>,
    /// A local change hasn't reached the cloud yet (retried on the next muse_* call).
    #[serde(default)]
    dirty: bool,
    /// Pending confirmation: code, hash of exactly what will be shared, expiry.
    #[serde(default)]
    confirm: Option<(String, String, i64)>,
    /// Last export version sent: versions only ever increase, whatever the clock does.
    #[serde(default)]
    last_version: i64,
    /// Hash of the export as last pushed successfully. Any difference from the current
    /// local export (whatever changed it: CLI revoke, memory_delete, …) triggers a push.
    #[serde(default)]
    pushed_hash: Option<String>,
}

/// `<db dir>/<db file name>.<suffix>`: every Muse file belongs to exactly one database.
fn file_for(cortex: &Cortex, suffix: &str) -> Result<PathBuf, String> {
    let db = cortex.db_path().ok_or("Muse needs an on-disk Cortex database")?;
    let name = db.file_name().ok_or("Muse needs an on-disk Cortex database")?.to_string_lossy().to_string();
    Ok(db.with_file_name(format!("{name}.{suffix}")))
}

/// Every muse_* call runs under this exclusive OS lock: two MCP processes on the same
/// database never interleave a local change and its push (or clear each other's retry
/// marker).
fn with_lock<T>(cortex: &Cortex, f: impl FnOnce() -> Result<T, String>) -> Result<T, String> {
    let lock = private_options()
        .create(true)
        .truncate(false)
        .write(true)
        .open(file_for(cortex, "muse.lock")?)
        .map_err(|e| e.to_string())?;
    lock.lock().map_err(|e| e.to_string())?;
    let out = f();
    let _ = lock.unlock();
    out
}

fn write_private(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    let mut f = private_options().write(true).create_new(true).open(&tmp).map_err(|e| e.to_string())?;
    f.write_all(data).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())?;
    // The retry marker lives here: it must survive power loss.
    super::sync_parent(path).map_err(|e| e.to_string())
}

fn load_state(cortex: &Cortex) -> Result<State, String> {
    match read_private(&file_for(cortex, "muse-cloud.json")?) {
        Ok(s) => serde_json::from_str(&s).map_err(|_| "Muse connection state is corrupt".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.to_string()),
    }
}

fn save_state(cortex: &Cortex, st: &State) -> Result<(), String> {
    write_private(&file_for(cortex, "muse-cloud.json")?, &serde_json::to_vec(st).map_err(|e| e.to_string())?)
}

fn parse_key(hex: &str) -> Result<[u8; 32], String> {
    let hex = hex.trim();
    if hex.len() != 64 {
        return Err("Muse device key is corrupt".into());
    }
    (0..64)
        .step_by(2)
        .map(|i| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
        .collect::<Option<Vec<u8>>>()
        .and_then(|v| v.try_into().ok())
        .ok_or_else(|| "Muse device key is corrupt".into())
}

/// Keychain account for this database's device key.
fn keychain_account(cortex: &Cortex) -> Result<String, String> {
    use sha2::{Digest, Sha256};
    let db = cortex.db_path().ok_or("Muse needs an on-disk Cortex database")?;
    let h: String = Sha256::digest(db.to_string_lossy().as_bytes()).iter().take(8).map(|b| format!("{b:02x}")).collect();
    Ok(format!("muse-device-{h}"))
}

/// The device, creating its key on first use (keychain first, 0600 file otherwise).
fn device(cortex: &Cortex) -> Result<Device, String> {
    use cortex_core::sync::secret::{load_secret, store_secret};
    let st = load_state(cortex)?;
    let key_path = file_for(cortex, "muse-device.key")?;
    let account = keychain_account(cortex)?;
    let secret: [u8; 32] = if let Some(hex) = load_secret(&account) {
        parse_key(&hex)?
    } else {
        match read_private(&key_path) {
            Ok(hex) => parse_key(&hex)?,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                if st.rid.is_some() {
                    // Never mint a new identity for an existing connection: the old key
                    // is the only one the cloud accepts (and could be overwritten).
                    return Err("This computer's Muse device key is unavailable (keychain locked or file missing). Unlock the keychain and retry; if the key is lost, delete the Muse connection state and run muse_connect again.".into());
                }
                let s = Device::generate_secret();
                let hex: String = s.iter().map(|b| format!("{b:02x}")).collect();
                if !store_secret(&account, &hex) {
                    write_private(&key_path, hex.as_bytes())?;
                }
                s
            }
            Err(e) => return Err(e.to_string()),
        }
    };
    let base_url = std::env::var("CORTEX_CLOUD_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .or(st.base_url)
        .unwrap_or_else(|| DEFAULT_CLOUD_URL.to_string());
    Ok(Device::new(secret, st.rid, base_url))
}

/// Push the whole shared list. On failure the state is marked dirty and every later
/// muse_* call retries first, so an unshare can't silently stay visible in the cloud.
fn push(cortex: &Cortex, dev: &Device) -> Result<u64, String> {
    // The local list is the source of truth: it must survive power loss before the cloud
    // is told and the retry marker cleared (covers share, connect, unshare, inbox keep).
    cortex.flush_durable().map_err(|e| e.to_string())?;
    // Strictly increasing per database (pushes run under the per-database lock), so a
    // slower, older push can never win and a clock set back can't wedge syncing.
    let mut st = load_state(cortex)?;
    let version = chrono::Utc::now().timestamp_millis().max(st.last_version + 1);
    st.last_version = version;
    save_state(cortex, &st)?;
    let items: Vec<(String, Option<Vec<f32>>)> = export_rows(cortex)?
        .into_iter()
        .map(|m| (content_to_string(&m.content), m.embedding.map(|e| e.as_ref().clone())))
        .collect();
    let snapshot = plan_hash(&{
        let mut t: Vec<String> = items.iter().map(|(t, _)| t.clone()).collect();
        t.sort();
        t
    });
    let out = dev.push_export(version, &items);
    st.dirty = out.is_err();
    if out.is_ok() {
        st.pushed_hash = Some(snapshot);
    }
    save_state(cortex, &st)?;
    out.map_err(|e| format!("{e}. Muse may still see the previous list until this is retried (automatically, on the next Muse action)."))
}

/// Before any change to the shared list of a connected database: record that the cloud
/// is behind, so a crash or a failure midway is pushed on the next muse_* call.
fn mark_stale(cortex: &Cortex, dev: &Device) -> Result<(), String> {
    if dev.rid.is_some() {
        let mut st = load_state(cortex)?;
        st.dirty = true;
        save_state(cortex, &st)?;
    }
    Ok(())
}

/// Would adding `texts` exceed the export cap? Counts only items not already shared
/// (sharing the same thing twice is a no-op).
fn check_capacity(cortex: &Cortex, texts: &[&String]) -> Result<(), String> {
    use std::collections::BTreeSet;
    let mut all: BTreeSet<String> = export_rows(cortex)?.iter().map(|m| content_to_string(&m.content)).collect();
    let current = all.len();
    all.extend(texts.iter().map(|t| (*t).clone()));
    if all.len() > MAX_EXPORT_ITEMS {
        return Err(format!("At most {MAX_EXPORT_ITEMS} memories can be shared; {current} already are."));
    }
    Ok(())
}

/// Retry a push that failed earlier.
fn settle(cortex: &Cortex, dev: &Device) -> Result<(), String> {
    if dev.rid.is_none() {
        return Ok(());
    }
    let st = load_state(cortex)?;
    if st.dirty || st.pushed_hash != Some(current_hash(cortex)?) {
        push(cortex, dev)?;
    }
    Ok(())
}

fn current_hash(cortex: &Cortex) -> Result<String, String> {
    let mut texts: Vec<String> = export_rows(cortex)?.iter().map(|m| content_to_string(&m.content)).collect();
    texts.sort();
    Ok(plan_hash(&texts))
}

/// Whether this database is connected to Cortex Cloud.
pub fn is_connected(cortex: &Cortex) -> bool {
    cortex.db_path().is_some() && load_state(cortex).is_ok_and(|s| s.rid.is_some())
}

/// Bring the cloud copy in line with the local export if it changed outside the muse_*
/// tools (gateway allow/revoke, memory_delete). No-op when not connected. Called right
/// after those changes; every muse_* call also checks.
pub fn reconcile(cortex: &Cortex) -> Result<(), String> {
    if cortex.db_path().is_none() || load_state(cortex)?.rid.is_none() {
        return Ok(());
    }
    with_lock(cortex, || {
        let dev = device(cortex)?;
        settle(cortex, &dev)
    })
}

/// Exactly what a share call would add: (memory id or text) → text.
fn planned(cortex: &Cortex, args: &Value) -> Result<Vec<String>, String> {
    let mut texts = Vec::new();
    for id in ids_arg(args, "memory_ids") {
        let id = Uuid::parse_str(&id).map_err(|_| format!("not a memory id: {id}"))?;
        let mem = cortex
            .storage()
            .get_memory(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no memory with id {id}"))?;
        let text = content_to_string(&mem.content);
        if text.trim().is_empty() {
            return Err(format!("memory {id} is empty and can't be shared"));
        }
        texts.push(text);
    }
    texts.extend(ids_arg(args, "texts").into_iter().filter(|t| !t.trim().is_empty()));
    // Check everything the cloud would refuse BEFORE touching the local list, so one bad
    // item can never wedge every later sync.
    if let Some(t) = texts.iter().find(|t| t.chars().count() > MAX_EXPORT_TEXT_CHARS) {
        return Err(format!(
            "A memory is too long to share ({} characters, at most {MAX_EXPORT_TEXT_CHARS}). Share a shorter summary as text instead.",
            t.chars().count()
        ));
    }
    check_capacity(cortex, &texts.iter().collect::<Vec<_>>())?;
    Ok(texts)
}

/// Identifies exactly this list (a JSON array is unambiguous, whatever the texts contain).
fn plan_hash(texts: &[String]) -> String {
    use sha2::{Digest, Sha256};
    let encoded = serde_json::to_vec(texts).unwrap_or_default();
    Sha256::digest(&encoded).iter().map(|b| format!("{b:02x}")).collect()
}

/// Two-step sharing. Without a valid `confirmation`, returns the preview and a fresh code
/// (nothing is shared). With it, and for exactly the same items, lets the caller proceed.
fn confirmed(cortex: &Cortex, args: &Value, texts: &[String]) -> Result<Option<Value>, String> {
    let mut st = load_state(cortex)?;
    let hash = plan_hash(texts);
    let now = chrono::Utc::now().timestamp();
    let given = args.get("confirmation").and_then(Value::as_str);
    if let (Some(code), Some((want, h, exp))) = (given, &st.confirm) {
        if code == want && *h == hash && *exp > now {
            st.confirm = None;
            save_state(cortex, &st)?;
            return Ok(None);
        }
    }
    let code: String = Uuid::new_v4().simple().to_string()[..8].to_string();
    st.confirm = Some((code.clone(), hash, now + CONFIRM_TTL_SECS));
    save_state(cortex, &st)?;
    Ok(Some(json!({
        "needs_confirmation": true,
        "will_share_with_muse": texts,
        "confirmation": code,
        "next_step": "Show the user exactly this list and ask whether Muse may see it. Only if they say yes, call this tool again with the same arguments plus this confirmation. Nothing has been shared yet.",
    })))
}

fn ids_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Copy confirmed texts into the local export. Returns how many were added.
fn add_to_export(cortex: &Cortex, texts: &[String]) -> Result<usize, String> {
    for t in texts {
        allow(cortex, t)?;
    }
    Ok(texts.len())
}

fn shared_list(cortex: &Cortex) -> Result<Vec<Value>, String> {
    Ok(export_rows(cortex)?
        .into_iter()
        .map(|m| json!({ "id": m.id.to_string(), "text": content_to_string(&m.content) }))
        .collect())
}

pub fn schemas() -> Vec<Value> {
    let ids = json!({ "type": "array", "items": { "type": "string" } });
    vec![
        json!({
            "name": "muse_connect",
            "description": "Connect the user's memory to Meta Muse (works on their phone, even with this computer off). Muse will only see the memories the user explicitly agrees to share; everything else stays on their devices. Steps: (1) find candidate memories with memory_search (preferences, facts the user wants Muse to know), (2) call this with the chosen memory_ids (and/or short texts): it returns exactly what would be shared plus a confirmation code and shares nothing yet, (3) show the user that list and, only if they agree, call again with the same arguments plus the confirmation. Returns a link: tell the user to paste it into Muse and tap Allow on the page that opens. Connecting again disconnects any earlier Muse connection.",
            "inputSchema": { "type": "object", "properties": { "memory_ids": ids, "texts": ids, "confirmation": { "type": "string" } } }
        }),
        json!({
            "name": "muse_share",
            "description": "Let Muse see more memories (by memory id, or new short texts). Two steps: the first call returns exactly what would be shared and a confirmation code; show it to the user and, only if they agree, call again with the same arguments plus the confirmation.",
            "inputSchema": { "type": "object", "properties": { "memory_ids": ids, "texts": ids, "confirmation": { "type": "string" } } }
        }),
        json!({
            "name": "muse_unshare",
            "description": "Stop sharing memories with Muse. Takes ids from muse_status's `shared` list. Takes effect on Muse's next request.",
            "inputSchema": { "type": "object", "properties": { "shared_ids": ids }, "required": ["shared_ids"] }
        }),
        json!({
            "name": "muse_status",
            "description": "What Muse can see, whether Muse is connected and when it last read, and how many things Muse asked to remember (see muse_inbox).",
            "inputSchema": { "type": "object", "properties": {} }
        }),
        json!({
            "name": "muse_inbox",
            "description": "Things Muse asked to remember. Without arguments, lists them: ask the user about each. Then call with keep (saved as the user's private memory and shared with Muse) and/or discard ids.",
            "inputSchema": { "type": "object", "properties": { "keep": ids, "discard": ids } }
        }),
        json!({
            "name": "muse_disconnect",
            "description": "Disconnect Muse and delete everything stored in Cortex Cloud. The user's memories on this computer are not touched.",
            "inputSchema": { "type": "object", "properties": {} }
        }),
    ]
}

pub fn call(cortex: &Cortex, name: &str, args: &Value) -> Option<Result<String, String>> {
    let op: fn(&Cortex, &Value) -> Result<Value, String> = match name {
        "muse_connect" => connect,
        "muse_share" => share,
        "muse_unshare" => unshare,
        "muse_status" => |c, _| status(c),
        "muse_inbox" => inbox,
        "muse_disconnect" => |c, _| disconnect(c),
        _ => return None,
    };
    let out = with_lock(cortex, || op(cortex, args));
    Some(out.map(|v| v.to_string()))
}

fn register(cortex: &Cortex, dev: &mut Device) -> Result<(), String> {
    dev.register()?;
    let mut st = load_state(cortex)?;
    st.rid = dev.rid.clone();
    st.base_url = Some(dev.base_url.clone());
    save_state(cortex, &st)
}

fn connect(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let texts = planned(cortex, args)?;
    if !texts.is_empty() {
        if let Some(preview) = confirmed(cortex, args, &texts)? {
            return Ok(preview);
        }
    }
    // Items shared before the cloud limits existed would make every push fail: name them.
    let too_long: Vec<String> = export_rows(cortex)?
        .iter()
        .filter(|m| content_to_string(&m.content).chars().count() > MAX_EXPORT_TEXT_CHARS)
        .map(|m| m.id.to_string())
        .collect();
    if !too_long.is_empty() {
        return Err(format!(
            "These shared memories are longer than {MAX_EXPORT_TEXT_CHARS} characters; unshare them (muse_unshare) or share shorter versions first: {}",
            too_long.join(", ")
        ));
    }
    if texts.is_empty() && export_rows(cortex)?.is_empty() {
        return Err("Nothing is shared yet. Find memories with memory_search, ask the user which ones Muse may see, then call muse_connect with their memory_ids.".into());
    }
    let mut dev = device(cortex)?;
    if dev.rid.is_none() {
        register(cortex, &mut dev)?;
    }
    // 1. Revoke every earlier connection and open the window — BEFORE the new items enter
    //    the local list. If this fails nothing changed, so no later retry can publish the
    //    new items to a connection that should have been replaced.
    let (link, expires) = match dev.enroll() {
        Err(e) if e.starts_with(GONE) => {
            // The service lost this connection (expired or deleted): start a fresh one.
            dev.rid = None;
            register(cortex, &mut dev)?;
            dev.enroll()?
        }
        other => other?,
    };
    // 2. Only now add and publish.
    if !texts.is_empty() {
        mark_stale(cortex, &dev)?;
    }
    let added = add_to_export(cortex, &texts)?;
    push(cortex, &dev)?;
    let shared = export_rows(cortex)?.len();
    Ok(json!({
        "link": link,
        "shared": shared,
        "newly_shared": added,
        "expires_in_minutes": expires / 60,
        "tell_the_user": format!(
            "Open Muse on your phone and send it this: \"Add a custom connector (MCP) named Cortex Privacy Memory with this URL: {link}\". \
             When a Cortex page opens, tap Allow. The link works once, for the next {} minutes. \
             Muse will see only the {shared} memories you shared.",
            expires / 60
        ),
    }))
}

fn share(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let texts = planned(cortex, args)?;
    if texts.is_empty() {
        return Err("Give memory_ids or texts to share.".into());
    }
    if let Some(preview) = confirmed(cortex, args, &texts)? {
        return Ok(preview);
    }
    let dev = device(cortex)?;
    settle(cortex, &dev)?;
    mark_stale(cortex, &dev)?;
    let added = add_to_export(cortex, &texts)?;
    let pushed = if dev.rid.is_some() { Some(push(cortex, &dev)?) } else { None };
    Ok(json!({ "added": added, "shared": export_rows(cortex)?.len(), "synced_to_cloud": pushed.is_some() }))
}

fn unshare(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let ids = ids_arg(args, "shared_ids");
    if ids.is_empty() {
        return Err("Give shared_ids (from muse_status).".into());
    }
    // Parse everything first: a malformed id changes nothing.
    let parsed: Vec<Uuid> = ids
        .iter()
        .map(|id| Uuid::parse_str(id).map_err(|_| format!("not a shared id: {id}")))
        .collect::<Result<_, _>>()?;
    let export: Vec<Uuid> = export_rows(cortex)?.into_iter().map(|m| m.id).collect();
    // Ids already gone locally are fine (idempotent: e.g. retrying after a failed push).
    let targets: Vec<Uuid> = parsed.into_iter().filter(|id| export.contains(id)).collect();
    let dev = device(cortex)?;
    // Record "cloud is behind" BEFORE deleting, so a crash or a failed push is retried
    // with the new list, whatever happens next.
    mark_stale(cortex, &dev)?;
    for id in &targets {
        cortex.delete_memory(*id).map_err(|e| e.to_string())?;
    }
    if dev.rid.is_some() {
        push(cortex, &dev)?;
    }
    Ok(json!({ "removed": targets.len(), "shared": export_rows(cortex)?.len() }))
}

fn status(cortex: &Cortex) -> Result<Value, String> {
    let shared = shared_list(cortex)?;
    let dev = device(cortex)?;
    if dev.rid.is_none() {
        return Ok(json!({ "connected": false, "shared": shared }));
    }
    // A pending sync must not hide the shared list the user may need to fix it.
    let sync_error = settle(cortex, &dev).err();
    let cloud = match dev.status() {
        Err(e) if e.starts_with(GONE) => {
            return Ok(json!({ "connected": false, "shared": shared, "note": "The cloud connection expired; muse_connect starts a new one." }))
        }
        // Offline: still return the shared list (its ids are what muse_unshare needs).
        Err(e) => {
            return Ok(json!({ "connected": Value::Null, "shared": shared, "cloud_error": e, "sync_error": sync_error }))
        }
        Ok(v) => v,
    };
    let inbox = dev.inbox().map(|i| i.len()).unwrap_or(0);
    Ok(json!({
        "connected": cloud.get("connected").and_then(Value::as_u64).unwrap_or(0) > 0,
        "muse_connected_at": cloud.get("connected_at").and_then(Value::as_i64)
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.to_rfc3339()),
        "muse_last_read": cloud.get("last_used").and_then(Value::as_i64)
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.to_rfc3339()),
        "check_with_user": "If the user didn't connect Muse at muse_connected_at, run muse_connect again: it cancels every earlier connection.",
        "shared": shared,
        "waiting_in_inbox": inbox,
        "sync_error": sync_error,
    }))
}

fn inbox(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let dev = device(cortex)?;
    if dev.rid.is_none() {
        return Err("Muse isn't connected (muse_connect first).".into());
    }
    settle(cortex, &dev)?;
    let items = dev.inbox()?;
    let keep = ids_arg(args, "keep");
    let discard = ids_arg(args, "discard");
    if keep.is_empty() && discard.is_empty() {
        let list: Vec<Value> = items.iter().map(|(id, text)| json!({ "id": id, "text": text })).collect();
        return Ok(json!({ "items": list }));
    }
    // Check the whole batch against the cloud limits before changing anything, and mark the
    // cloud stale first, so a failure midway is still pushed later.
    let keeping: Vec<&String> = items.iter().filter(|(id, _)| keep.contains(id)).map(|(_, t)| t).collect();
    if let Some(t) = keeping.iter().find(|t| t.chars().count() > MAX_EXPORT_TEXT_CHARS) {
        return Err(format!("An item is too long to keep shared ({} characters).", t.chars().count()));
    }
    check_capacity(cortex, &keeping)?;
    if !keeping.is_empty() {
        mark_stale(cortex, &dev)?;
    }
    let mut done = Vec::new();
    for (id, text) in &items {
        if keep.contains(id) {
            cortex
                .ingest_with_options(text, "muse", None, None, None, None, Some(PrivacyLevel::Private))
                .map_err(|e| e.to_string())?;
            allow(cortex, text)?;
            done.push(id.clone());
        } else if discard.contains(id) {
            done.push(id.clone());
        }
    }
    if done.iter().any(|id| keep.contains(id)) {
        // push() makes the kept memories durable before the cloud deletes its inbox copy.
        push(cortex, &dev)?;
    }
    let removed = dev.inbox_ack(&done)?;
    Ok(json!({ "kept": keep.iter().filter(|k| done.contains(k)).count(), "removed_from_inbox": removed }))
}

fn disconnect(cortex: &Cortex) -> Result<Value, String> {
    let dev = device(cortex)?;
    if dev.rid.is_some() {
        match dev.delete() {
            Err(e) if !e.starts_with(GONE) => return Err(e),
            _ => {} // deleted, or already gone
        }
    }
    save_state(cortex, &State::default())?;
    Ok(json!({
        "disconnected": true,
        "note": "Everything in Cortex Cloud was deleted and Muse can no longer read anything. The shared list is kept on this computer; connect again any time."
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plan_hash_distinguishes_lists_whatever_the_texts_contain() {
        assert_ne!(plan_hash(&["first\u{0}second".into()]), plan_hash(&["first".into(), "second".into()]));
        assert_ne!(plan_hash(&["a,b".into()]), plan_hash(&["a".into(), "b".into()]));
        assert_eq!(plan_hash(&["x".into()]), plan_hash(&["x".into()]));
    }
}
