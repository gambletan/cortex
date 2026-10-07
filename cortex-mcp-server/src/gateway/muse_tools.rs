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
//! a copy of it. The device key is a 0600 file next to the database (the same protection
//! as the memories themselves).

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use uuid::Uuid;

use cortex_core::types::PrivacyLevel;
use cortex_core::Cortex;

use super::cloud::{Device, DEFAULT_CLOUD_URL};
use super::{allow, export_rows, private_options, read_private};
use crate::tools::content_to_string;

const STATE_FILE: &str = "muse-cloud.json";
const KEY_FILE: &str = "muse-device.key";

#[derive(Serialize, Deserialize, Default)]
struct State {
    rid: Option<String>,
    base_url: Option<String>,
}

fn dir(cortex: &Cortex) -> Result<PathBuf, String> {
    let db = cortex.db_path().ok_or("Muse needs an on-disk Cortex database")?;
    Ok(db.parent().map(PathBuf::from).unwrap_or_else(|| PathBuf::from(".")))
}

fn write_private(path: &std::path::Path, data: &[u8]) -> Result<(), String> {
    use std::io::Write as _;
    let tmp = path.with_extension(format!("{}.tmp", Uuid::new_v4().simple()));
    let mut f = private_options().write(true).create_new(true).open(&tmp).map_err(|e| e.to_string())?;
    f.write_all(data).and_then(|_| f.sync_all()).map_err(|e| e.to_string())?;
    std::fs::rename(&tmp, path).map_err(|e| e.to_string())
}

fn load_state(cortex: &Cortex) -> Result<State, String> {
    match read_private(&dir(cortex)?.join(STATE_FILE)) {
        Ok(s) => serde_json::from_str(&s).map_err(|_| "Muse connection state is corrupt".to_string()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(State::default()),
        Err(e) => Err(e.to_string()),
    }
}

fn save_state(cortex: &Cortex, st: &State) -> Result<(), String> {
    write_private(&dir(cortex)?.join(STATE_FILE), &serde_json::to_vec(st).map_err(|e| e.to_string())?)
}

/// The device, creating its key on first use.
fn device(cortex: &Cortex) -> Result<Device, String> {
    let st = load_state(cortex)?;
    let key_path = dir(cortex)?.join(KEY_FILE);
    let secret: [u8; 32] = match read_private(&key_path) {
        Ok(hex) => (0..64)
            .step_by(2)
            .map(|i| hex.get(i..i + 2).and_then(|h| u8::from_str_radix(h, 16).ok()))
            .collect::<Option<Vec<u8>>>()
            .and_then(|v| v.try_into().ok())
            .ok_or("Muse device key is corrupt")?,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            let s = Device::generate_secret();
            write_private(&key_path, s.iter().map(|b| format!("{b:02x}")).collect::<String>().as_bytes())?;
            s
        }
        Err(e) => return Err(e.to_string()),
    };
    let base_url = std::env::var("CORTEX_CLOUD_URL")
        .ok()
        .filter(|u| !u.is_empty())
        .or(st.base_url)
        .unwrap_or_else(|| DEFAULT_CLOUD_URL.to_string());
    Ok(Device::new(secret, st.rid, base_url))
}

fn push(cortex: &Cortex, dev: &Device) -> Result<u64, String> {
    let items: Vec<(String, Option<Vec<f32>>)> = export_rows(cortex)?
        .into_iter()
        .map(|m| (content_to_string(&m.content), m.embedding.map(|e| e.as_ref().clone())))
        .collect();
    dev.push_export(&items)
}

fn ids_arg(args: &Value, key: &str) -> Vec<String> {
    args.get(key)
        .and_then(Value::as_array)
        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_string).collect())
        .unwrap_or_default()
}

/// Copy memories (by id) or new texts into the local export. Returns how many were added.
fn add_to_export(cortex: &Cortex, args: &Value) -> Result<usize, String> {
    let mut n = 0;
    for id in ids_arg(args, "memory_ids") {
        let id = Uuid::parse_str(&id).map_err(|_| format!("not a memory id: {id}"))?;
        let mem = cortex
            .storage()
            .get_memory(id)
            .map_err(|e| e.to_string())?
            .ok_or_else(|| format!("no memory with id {id}"))?;
        allow(cortex, &content_to_string(&mem.content))?;
        n += 1;
    }
    for text in ids_arg(args, "texts") {
        if !text.trim().is_empty() {
            allow(cortex, &text)?;
            n += 1;
        }
    }
    Ok(n)
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
            "description": "Connect the user's memory to Meta Muse (works on their phone, even with this computer off). Muse will only see the memories the user explicitly agrees to share; everything else stays on their devices. Steps: (1) find candidate memories with memory_search (preferences, facts the user wants Muse to know), (2) ASK the user which to share, (3) call this with their memory_ids (and/or short texts). Returns a link: tell the user to paste it into Muse and tap Allow on the page that opens. Connecting again disconnects any earlier Muse connection.",
            "inputSchema": { "type": "object", "properties": { "memory_ids": ids, "texts": ids } }
        }),
        json!({
            "name": "muse_share",
            "description": "Let Muse see more memories (by memory id, or new short texts). Only call after the user agreed.",
            "inputSchema": { "type": "object", "properties": { "memory_ids": ids, "texts": ids } }
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
    let out = match name {
        "muse_connect" => connect(cortex, args),
        "muse_share" => share(cortex, args),
        "muse_unshare" => unshare(cortex, args),
        "muse_status" => status(cortex),
        "muse_inbox" => inbox(cortex, args),
        "muse_disconnect" => disconnect(cortex),
        _ => return None,
    };
    Some(out.map(|v| v.to_string()))
}

fn connect(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let added = add_to_export(cortex, args)?;
    let shared = export_rows(cortex)?.len();
    if shared == 0 {
        return Err("Nothing is shared yet. Find memories with memory_search, ask the user which ones Muse may see, then call muse_connect with their memory_ids.".into());
    }
    let mut dev = device(cortex)?;
    if dev.rid.is_none() {
        dev.register()?;
        save_state(cortex, &State { rid: dev.rid.clone(), base_url: Some(dev.base_url.clone()) })?;
    }
    push(cortex, &dev)?;
    let (link, expires) = dev.enroll()?;
    Ok(json!({
        "link": link,
        "shared": shared,
        "newly_shared": added,
        "expires_in_minutes": expires / 60,
        "tell_the_user": format!(
            "Open Muse on your phone and send it this: \"Add a custom connector (MCP) with this URL: {link}\". \
             When a Cortex page opens, tap Allow. The link works once, for the next {} minutes. \
             Muse will see only the {shared} memories you shared.",
            expires / 60
        ),
    }))
}

fn share(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let added = add_to_export(cortex, args)?;
    let dev = device(cortex)?;
    let pushed = if dev.rid.is_some() { Some(push(cortex, &dev)?) } else { None };
    Ok(json!({ "added": added, "shared": export_rows(cortex)?.len(), "synced_to_cloud": pushed.is_some() }))
}

fn unshare(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let ids = ids_arg(args, "shared_ids");
    let export: Vec<Uuid> = export_rows(cortex)?.into_iter().map(|m| m.id).collect();
    let mut removed = 0;
    for id in &ids {
        let id = Uuid::parse_str(id).map_err(|_| format!("not a shared id: {id}"))?;
        if !export.contains(&id) {
            return Err(format!("{id} is not in the shared list (see muse_status)"));
        }
        cortex.delete_memory(id).map_err(|e| e.to_string())?;
        removed += 1;
    }
    let dev = device(cortex)?;
    if dev.rid.is_some() {
        push(cortex, &dev)?;
    }
    Ok(json!({ "removed": removed, "shared": export_rows(cortex)?.len() }))
}

fn status(cortex: &Cortex) -> Result<Value, String> {
    let shared = shared_list(cortex)?;
    let dev = device(cortex)?;
    if dev.rid.is_none() {
        return Ok(json!({ "connected": false, "shared": shared }));
    }
    let cloud = dev.status()?;
    let inbox = dev.inbox().map(|i| i.len()).unwrap_or(0);
    Ok(json!({
        "connected": cloud.get("connected").and_then(Value::as_u64).unwrap_or(0) > 0,
        "muse_last_read": cloud.get("last_used").and_then(Value::as_i64)
            .and_then(|t| chrono::DateTime::from_timestamp(t, 0)).map(|d| d.to_rfc3339()),
        "shared": shared,
        "waiting_in_inbox": inbox,
    }))
}

fn inbox(cortex: &Cortex, args: &Value) -> Result<Value, String> {
    let dev = device(cortex)?;
    if dev.rid.is_none() {
        return Err("Muse isn't connected (muse_connect first).".into());
    }
    let items = dev.inbox()?;
    let keep = ids_arg(args, "keep");
    let discard = ids_arg(args, "discard");
    if keep.is_empty() && discard.is_empty() {
        let list: Vec<Value> = items.iter().map(|(id, text)| json!({ "id": id, "text": text })).collect();
        return Ok(json!({ "items": list }));
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
        push(cortex, &dev)?;
    }
    let removed = dev.inbox_ack(&done)?;
    Ok(json!({ "kept": keep.iter().filter(|k| done.contains(k)).count(), "removed_from_inbox": removed }))
}

fn disconnect(cortex: &Cortex) -> Result<Value, String> {
    let dev = device(cortex)?;
    if dev.rid.is_some() {
        dev.delete()?;
    }
    save_state(cortex, &State::default())?;
    Ok(json!({
        "disconnected": true,
        "note": "Everything in Cortex Cloud was deleted and Muse can no longer read anything. The shared list is kept on this computer; connect again any time."
    }))
}
