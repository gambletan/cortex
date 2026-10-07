//! MCP tool definitions and execution for Cortex memory engine.

use cortex_core::types::PrivacyLevel;
use cortex_core::Cortex;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

// ── Safety guardrails ───────────────────────────────────────────────────────
const MAX_INGEST_TEXT_BYTES: usize = 100_000;   // 100KB per memory
const MAX_BATCH_SIZE: usize = 100;              // memory_ingest_batch
const MAX_SEARCH_LIMIT: usize = 100;            // memory_search
const MAX_CONTEXT_TOKENS: usize = 8_000;        // memory_context
const MAX_TAG_SCAN_PER_TIER: usize = 10_000;    // tag_list_taxonomy

/// Return the list of available tools (MCP tool schema format).
/// Includes built-in tools and any plugin-registered tools.
pub fn list_tools_with_plugins(cortex: &Arc<Cortex>) -> Value {
    let mut tools = list_tools_builtin();
    #[cfg(feature = "gateway")]
    if let Some(list) = tools.as_array_mut() {
        list.extend(crate::gateway::muse_tools::schemas());
    }

    // Append plugin tools
    for pt in cortex.plugin_manager().list_tools() {
        tools.as_array_mut().unwrap().push(json!({
            "name": pt.name,
            "description": pt.description,
            "inputSchema": pt.input_schema,
        }));
    }

    tools
}

/// Return the list of built-in tools (without plugins).
fn list_tools_builtin() -> Value {
    json!([
        {
            "name": "memory_ingest",
            "description": "Store a new memory. Use this to remember something the user said, did, or prefers. Memories are automatically timestamped and linked to the person/channel.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The content to remember"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Source channel (e.g. 'telegram', 'slack', 'claude')"
                    },
                    "user_id": {
                        "type": "string",
                        "description": "User identifier in the channel (optional)"
                    },
                    "salience": {
                        "type": "number",
                        "description": "Importance hint 0.0-1.0 (optional, default auto)"
                    },
                    "embedding": {
                        "type": "array",
                        "items": { "type": "number" },
                        "description": "Pre-computed embedding vector (optional)"
                    },
                    "namespace": {
                        "type": "string",
                        "description": "Namespace for isolation (optional, e.g. 'user_123')"
                    },
                    "privacy": {
                        "type": "string",
                        "enum": ["private", "shared", "public"],
                        "description": "Privacy level (optional, default 'private'). 'private' never leaves this device; 'shared'/'public' opt the memory into encrypted cloud sync and remote-LLM context."
                    },
                    "scope": {
                        "type": "string",
                        "description": "Sharing scope when privacy='shared' (optional, default 'all')"
                    }
                },
                "required": ["text", "channel"]
            }
        },
        {
            "name": "memory_set_privacy",
            "description": "Change the privacy level of an existing memory. Promoting to 'shared'/'public' opts it into encrypted cloud sync; demoting to 'private' retracts it from other devices (local copy is kept).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Memory UUID"
                    },
                    "privacy": {
                        "type": "string",
                        "enum": ["private", "shared", "public"],
                        "description": "New privacy level"
                    },
                    "scope": {
                        "type": "string",
                        "description": "Sharing scope when privacy='shared' (optional, default 'all')"
                    }
                },
                "required": ["id", "privacy"]
            }
        },
        {
            "name": "memory_search",
            "description": "Search memories by text query. Returns the most relevant memories ranked by similarity, recency, salience, and social context. Use this when you need to recall something about the user or a topic.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "What to search for"
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max results (default 10)"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Filter by channel (optional)"
                    },
                    "person_id": {
                        "type": "string",
                        "description": "Filter by person UUID (optional)"
                    },
                    "embedding": {
                        "type": "array",
                        "items": { "type": "number" },
                        "description": "Query embedding for semantic search (optional)"
                    },
                    "namespace": {
                        "type": "string",
                        "description": "Filter by namespace for isolation (optional)"
                    }
                },
                "required": ["query"]
            }
        },
        {
            "name": "memory_context",
            "description": "Generate a comprehensive context summary from all memory tiers. Returns a structured text block ready for LLM system prompts, including user preferences, recent episodes, beliefs, and relationships.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "max_tokens": {
                        "type": "integer",
                        "description": "Max context length in tokens (default 2000)"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Filter by channel (optional)"
                    },
                    "person_id": {
                        "type": "string",
                        "description": "Filter by person UUID (optional)"
                    },
                    "namespace": {
                        "type": "string",
                        "description": "Filter by namespace for isolation (optional)"
                    },
                    "min_confidence": {
                        "type": "number",
                        "description": "Exclude facts/preferences below this confidence (0.0-1.0, default 0.3) — keeps low-confidence or superseded facts out of the injected context"
                    }
                }
            }
        },
        {
            "name": "belief_observe",
            "description": "Update a belief based on new evidence. Beliefs are probabilistic — supporting evidence increases confidence, contradicting evidence decreases it. Use this to track things you learn about the user over time.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Belief identifier (e.g. 'user_prefers_dark_mode', 'user_is_developer')"
                    },
                    "supports": {
                        "type": "boolean",
                        "description": "true = supporting evidence, false = contradicting"
                    },
                    "strength": {
                        "type": "number",
                        "description": "Evidence strength 0.0-1.0 (default 0.5)"
                    }
                },
                "required": ["key", "supports"]
            }
        },
        {
            "name": "belief_list",
            "description": "List current beliefs above a confidence threshold. Returns beliefs the system has formed about the user.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "threshold": {
                        "type": "number",
                        "description": "Minimum confidence 0.0-1.0 (default 0.6)"
                    }
                }
            }
        },
        {
            "name": "person_resolve",
            "description": "Resolve or create a person identity. Links a channel-specific user ID to a cross-channel person profile. Use this to track who you're talking to.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "name": {
                        "type": "string",
                        "description": "Display name"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Channel (e.g. 'telegram', 'slack')"
                    },
                    "channel_user_id": {
                        "type": "string",
                        "description": "User ID in that channel"
                    }
                },
                "required": ["name", "channel", "channel_user_id"]
            }
        },
        {
            "name": "fact_add",
            "description": "Store a semantic fact as a subject-predicate-object triple. Use for structured knowledge like 'User works_at Google' or 'User speaks Chinese'.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "Subject of the fact"
                    },
                    "predicate": {
                        "type": "string",
                        "description": "Relationship (e.g. 'works_at', 'lives_in', 'speaks')"
                    },
                    "object": {
                        "type": "string",
                        "description": "Object of the fact"
                    },
                    "confidence": {
                        "type": "number",
                        "description": "Confidence 0.0-1.0 (default 0.8)"
                    },
                    "channel": {
                        "type": "string",
                        "description": "Source channel (default 'manual')"
                    }
                },
                "required": ["subject", "predicate", "object"]
            }
        },
        {
            "name": "preference_set",
            "description": "Store a user preference. Use for things like language preference, communication style, favorite tools, etc.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Preference key (e.g. 'language', 'code_style', 'timezone')"
                    },
                    "value": {
                        "type": "string",
                        "description": "Preference value"
                    },
                    "confidence": {
                        "type": "number",
                        "description": "Confidence 0.0-1.0 (default 0.9)"
                    }
                },
                "required": ["key", "value"]
            }
        },
        {
            "name": "memory_consolidate",
            "description": "Run a consolidation cycle: apply temporal decay, promote repeated episodes to semantic facts, sweep dead memories, and extract patterns. Automatically runs every 100 ingests, but can be triggered manually.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "memory_infer",
            "description": "Run proactive inference on text without storing it. Returns extracted facts, preferences, and temporal classification (temporary/permanent/unknown). Useful for previewing what would be auto-extracted on ingest.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "The text to analyze"
                    }
                },
                "required": ["text"]
            }
        },
        {
            "name": "contradiction_check",
            "description": "Check if a potential fact contradicts existing knowledge. Returns conflicting facts if any. Useful before adding facts to see if they would supersede existing information.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "subject": {
                        "type": "string",
                        "description": "Subject of the fact"
                    },
                    "predicate": {
                        "type": "string",
                        "description": "Relationship"
                    },
                    "object": {
                        "type": "string",
                        "description": "Object of the fact"
                    }
                },
                "required": ["subject", "predicate", "object"]
            }
        },
        {
            "name": "memory_compress",
            "description": "Compress old conversation sessions into summaries. Reduces storage while preserving key information. Sessions older than max_age_days with at least min_messages entries get compressed.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "min_messages": {
                        "type": "integer",
                        "description": "Minimum messages per session to compress (default 5)"
                    },
                    "max_age_days": {
                        "type": "integer",
                        "description": "Only compress sessions older than this many days (default 7)"
                    }
                }
            }
        },
        {
            "name": "relationship_extract",
            "description": "Extract interpersonal relationships from text. Detects relationships like 'works_with', 'reports_to', 'friend_of', etc. Supports English and Chinese.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "text": {
                        "type": "string",
                        "description": "Text to analyze for relationships"
                    }
                },
                "required": ["text"]
            }
        },
        {
            "name": "memory_stats",
            "description": "Get memory statistics: counts per tier (episodic, semantic, procedural), people, beliefs, and vector index size.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "fact_query",
            "description": "Query semantic facts by entity name. Returns all facts where the entity appears as subject or object. More efficient than memory_search for structured knowledge lookups.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "entity": {
                        "type": "string",
                        "description": "Entity to search for (e.g. 'Alice', 'Python', 'Shanghai')"
                    }
                },
                "required": ["entity"]
            }
        },
        {
            "name": "preference_query",
            "description": "Query user preferences by key pattern. Returns matching preferences.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "key": {
                        "type": "string",
                        "description": "Key pattern to search (e.g. 'language', 'timezone'). Empty string returns all."
                    }
                },
                "required": ["key"]
            }
        },
        {
            "name": "person_list",
            "description": "List all known people in the memory graph. Returns names, channels, and interaction counts.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "memory_decay",
            "description": "Run temporal decay on episodic memories. Reduces salience of old, unaccessed memories. Lighter than full consolidation.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "memory_archive",
            "description": "Archive a memory to cold storage. Removes it from the active index. Use for old/low-value memories you want to keep but not actively search.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "Memory UUID to archive"
                    }
                },
                "required": ["id"]
            }
        },
        {
            "name": "memory_ingest_batch",
            "description": "Ingest multiple memories in a single transaction. More efficient than multiple memory_ingest calls. Supports deduplication.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "items": {
                        "type": "array",
                        "description": "Array of memory items to ingest",
                        "items": {
                            "type": "object",
                            "properties": {
                                "text": { "type": "string", "description": "Content to remember" },
                                "channel": { "type": "string", "description": "Source channel" },
                                "user_id": { "type": "string", "description": "User ID (optional)" },
                                "salience": { "type": "number", "description": "Importance 0-1 (optional)" },
                                "namespace": { "type": "string", "description": "Namespace for isolation (optional)" },
                                "privacy": { "type": "string", "enum": ["private", "shared", "public"], "description": "Privacy level (optional, default 'private'). Use 'shared'/'public' to sync." }
                            },
                            "required": ["text", "channel"]
                        }
                    }
                },
                "required": ["items"]
            }
        },
        {
            "name": "tag_list_taxonomy",
            "description": "List all tags currently in use across memories, with counts.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "memory_delete",
            "description": "Permanently delete a memory by ID.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "UUID of the memory to delete"
                    }
                },
                "required": ["id"]
            }
        },
        {
            "name": "memory_restore",
            "description": "Restore an archived memory back to an active tier.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "id": {
                        "type": "string",
                        "description": "UUID of the archived memory"
                    },
                    "tier": {
                        "type": "string",
                        "description": "Target tier: 'episodic', 'semantic', or 'procedural' (default: 'episodic')"
                    }
                },
                "required": ["id"]
            }
        },
        {
            "name": "namespace_list",
            "description": "List all namespaces with memory counts. Useful for multi-user/multi-context isolation overview.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "person_merge",
            "description": "Merge two person identities into one. Moves all identities, notes, and tags from source to target, then deletes the source person.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "target_id": {
                        "type": "string",
                        "description": "UUID of the person to keep (primary)"
                    },
                    "source_id": {
                        "type": "string",
                        "description": "UUID of the person to merge into target (will be deleted)"
                    }
                },
                "required": ["target_id", "source_id"]
            }
        },
        {
            "name": "sync_enable",
            "description": "Enable cross-device cloud sync. Auto-detects cloud provider (iCloud/Google Drive/OneDrive/Dropbox), generates device ID, and starts syncing. Encrypts with AES-256-GCM. The passphrase never passes through the assistant: it comes from CORTEX_SYNC_PASSPHRASE, or a generated one is stored in the OS keychain.",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "provider": {
                        "type": "string",
                        "description": "Cloud provider: 'icloud', 'gdrive', 'onedrive', 'dropbox'. If omitted, auto-detects the first available."
                    },
                    "device_name": {
                        "type": "string",
                        "description": "Human-readable device name (default: hostname)"
                    }
                }
            }
        },
        {
            "name": "sync_pull",
            "description": "Pull and apply remote changes from other devices. Call periodically or after enabling sync to fetch new memories from other devices.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "sync_status",
            "description": "Show cloud sync status: enabled/disabled, provider, connected devices, pending operations.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        },
        {
            "name": "sync_providers",
            "description": "Detect available cloud storage providers (iCloud Drive, Google Drive, OneDrive, Dropbox) on this machine.",
            "inputSchema": {
                "type": "object",
                "properties": {}
            }
        }
    ])
}

/// Execute a tool call and return the result as a string.
fn scrub(r: Result<String, String>) -> Result<String, String> {
    r.map(|s| scrub_json_text(&s)).map_err(|e| redact_local(&e))
}

/// Results are serialized JSON: redact each string value (keys too) and re-serialize, so
/// redaction can never break JSON escaping. Non-JSON text is redacted as plain text.
fn scrub_json_text(s: &str) -> String {
    fn walk(v: &mut Value) {
        match v {
            Value::String(t) => *t = redact_local(t),
            Value::Array(a) => a.iter_mut().for_each(walk),
            Value::Object(o) => {
                let entries: Vec<(String, Value)> = std::mem::take(o)
                    .into_iter()
                    .map(|(k, mut v)| {
                        walk(&mut v);
                        (redact_local(&k), v)
                    })
                    .collect();
                o.extend(entries);
            }
            _ => {}
        }
    }
    match serde_json::from_str::<Value>(s) {
        Ok(mut v) => {
            walk(&mut v);
            v.to_string()
        }
        Err(_) => redact_local(s),
    }
}

/// `redact_emails` plus the user's home directory (it carries the OS login name),
/// in both raw and JSON-escaped form.
pub(crate) fn redact_local(input: &str) -> String {
    let mut out = redact_emails(input);
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = home.to_string_lossy().trim_end_matches(['/', '\\']).to_string();
        if home.len() > 1 {
            let escaped = home.replace('\\', "\\\\");
            out = out.replace(&escaped, "~");
            out = out.replace(&home, "~");
        }
    }
    out
}

pub fn call_tool(cortex: &Arc<Cortex>, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "memory_ingest" => tool_memory_ingest(cortex, args),
        "memory_set_privacy" => tool_memory_set_privacy(cortex, args),
        "memory_search" => tool_memory_search(cortex, args),
        "memory_context" => tool_memory_context(cortex, args),
        "memory_consolidate" => tool_memory_consolidate(cortex),
        "memory_infer" => tool_memory_infer(cortex, args),
        "contradiction_check" => tool_contradiction_check(cortex, args),
        "belief_observe" => tool_belief_observe(cortex, args),
        "belief_list" => tool_belief_list(cortex, args),
        "person_resolve" => tool_person_resolve(cortex, args),
        "fact_add" => tool_fact_add(cortex, args),
        "preference_set" => tool_preference_set(cortex, args),
        "memory_compress" => tool_memory_compress(cortex, args),
        "relationship_extract" => tool_relationship_extract(cortex, args),
        "memory_stats" => tool_memory_stats(cortex),
        "fact_query" => tool_fact_query(cortex, args),
        "preference_query" => tool_preference_query(cortex, args),
        "person_list" => tool_person_list(cortex),
        "memory_decay" => tool_memory_decay(cortex),
        "memory_archive" => tool_memory_archive(cortex, args),
        "memory_ingest_batch" => tool_memory_ingest_batch(cortex, args),
        "tag_list_taxonomy" => tool_tag_list_taxonomy(cortex),
        "memory_delete" => {
            let out = tool_memory_delete(cortex, args);
            // Deleting a shared memory must also stop Muse seeing it in Cortex Cloud.
            #[cfg(feature = "gateway")]
            if out.is_ok() {
                if let Err(e) = crate::gateway::muse_tools::reconcile(cortex) {
                    tracing::warn!(error = %e, "Cortex Cloud not updated yet; retried on the next Muse action");
                }
            }
            out
        }
        "memory_restore" => tool_memory_restore(cortex, args),
        "namespace_list" => tool_namespace_list(cortex),
        "person_merge" => tool_person_merge(cortex, args),
        // Sync tools touch local paths and account identifiers: every result *and* error
        // string is scrubbed before it reaches the model.
        "sync_enable" => scrub(tool_sync_enable(cortex, args)),
        "sync_pull" => scrub(tool_sync_pull(cortex)),
        "sync_status" => scrub(tool_sync_status(cortex)),
        "sync_providers" => scrub(tool_sync_providers()),
        #[cfg(feature = "gateway")]
        n if n.starts_with("muse_") => crate::gateway::muse_tools::call(cortex, n, args)
            .unwrap_or_else(|| Err(format!("Unknown tool: {n}"))),
        _ => {
            // Fallback to plugin-registered tools
            let ctx = cortex.plugin_context();
            match cortex.plugin_manager().call_tool(name, args, &ctx) {
                Some(result) => result,
                None => Err(format!("Unknown tool: {name}")),
            }
        }
    }
}

// ── Tool implementations ────────────────────────────────────────────────────

fn get_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key).and_then(|v| v.as_str())
}

fn get_f32(args: &Value, key: &str, default: f32) -> f32 {
    args.get(key)
        .and_then(|v| v.as_f64())
        .map(|v| v as f32)
        .unwrap_or(default)
}

fn get_usize(args: &Value, key: &str, default: usize) -> usize {
    args.get(key)
        .and_then(|v| v.as_u64())
        .map(|v| v as usize)
        .unwrap_or(default)
}

fn get_embedding(args: &Value) -> Option<Vec<f32>> {
    args.get("embedding")
        .and_then(|v| v.as_array())
        .map(|arr| arr.iter().filter_map(|v| v.as_f64().map(|f| f as f32)).collect())
}

/// Parse the optional `privacy` argument: "private" (default), "shared", "public".
/// `scope` refines Shared (default "all"). Unknown values are an error — privacy
/// must never be silently coerced.
fn parse_privacy(args: &Value) -> Result<Option<PrivacyLevel>, String> {
    let Some(p) = get_str(args, "privacy") else {
        return Ok(None);
    };
    match p.to_lowercase().as_str() {
        "private" => Ok(Some(PrivacyLevel::Private)),
        "shared" => Ok(Some(PrivacyLevel::Shared {
            scope: get_str(args, "scope").unwrap_or("all").to_string(),
        })),
        "public" => Ok(Some(PrivacyLevel::Public)),
        other => Err(format!(
            "invalid privacy '{other}': use 'private', 'shared', or 'public'"
        )),
    }
}

fn privacy_label(p: &PrivacyLevel) -> &'static str {
    match p {
        PrivacyLevel::Private => "private",
        PrivacyLevel::Shared { .. } => "shared",
        PrivacyLevel::Public => "public",
    }
}

fn tool_memory_ingest(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let text = get_str(args, "text").ok_or("missing 'text'")?;
    if text.len() > MAX_INGEST_TEXT_BYTES {
        return Err(format!("text too large: {} bytes (max {})", text.len(), MAX_INGEST_TEXT_BYTES));
    }
    let channel = get_str(args, "channel").ok_or("missing 'channel'")?;
    let user_id = get_str(args, "user_id");
    let salience = args.get("salience").and_then(|v| v.as_f64()).map(|v| v as f32);
    let embedding = get_embedding(args);
    let namespace = get_str(args, "namespace");
    let privacy = parse_privacy(args)?;

    let mem = cortex
        .ingest_with_options(text, channel, user_id, salience, embedding, namespace, privacy)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "id": mem.id.to_string(),
        "tier": mem.tier.as_str(),
        "privacy": privacy_label(&mem.privacy),
        "created_at": mem.temporal.ingestion_time.to_rfc3339(),
        "status": "stored"
    })
    .to_string())
}

fn tool_memory_set_privacy(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let id_str = get_str(args, "id").ok_or("missing 'id'")?;
    let id = uuid::Uuid::parse_str(id_str).map_err(|e| format!("invalid id: {e}"))?;
    let privacy = parse_privacy(args)?.ok_or("missing 'privacy'")?;

    let mem = cortex.set_memory_privacy(id, privacy).map_err(|e| e.to_string())?;
    Ok(json!({
        "id": mem.id.to_string(),
        "privacy": privacy_label(&mem.privacy),
        "syncable": mem.privacy.is_syncable(),
        "status": "updated"
    })
    .to_string())
}

pub(crate) fn content_to_string(content: &cortex_core::types::MemContent) -> String {
    match content {
        cortex_core::types::MemContent::Text(t) => t.clone(),
        cortex_core::types::MemContent::Fact { subject, predicate, object } => {
            format!("{} {} {}", subject, predicate, object)
        }
        cortex_core::types::MemContent::Preference { key, value, .. } => {
            format!("{} = {}", key, value)
        }
        other => format!("{:?}", other),
    }
}

fn tool_memory_consolidate(cortex: &Arc<Cortex>) -> Result<String, String> {
    let report = cortex
        .run_consolidation()
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "episodes_scanned": report.episodes_scanned,
        "decayed_updated": report.decayed_updated,
        "decayed_swept": report.decayed_swept,
        "promoted_to_semantic": report.promoted_to_semantic,
        "patterns_detected": report.patterns_detected,
    })
    .to_string())
}

fn tool_memory_search(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let query = get_str(args, "query").ok_or("missing 'query'")?;
    let limit = get_usize(args, "limit", 10).min(MAX_SEARCH_LIMIT);
    let channel = get_str(args, "channel");
    let person_id = get_str(args, "person_id")
        .and_then(|s| Uuid::parse_str(s).ok());
    let embedding = get_embedding(args);
    let namespace = get_str(args, "namespace");

    let results = cortex
        .retrieve_with_namespace(query, limit, channel, person_id, embedding, namespace)
        .map_err(|e| e.to_string())?;

    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r.memory.id.to_string(),
                "text": content_to_string(&r.memory.content),
                "score": format!("{:.4}", r.score),
                "tier": r.memory.tier.as_str(),
                "created_at": r.memory.temporal.ingestion_time.to_rfc3339(),
                "channel": r.memory.source.channel,
            })
        })
        .collect();

    Ok(json!({
        "results": items,
        "total": items.len()
    })
    .to_string())
}

fn tool_memory_context(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let max_tokens = get_usize(args, "max_tokens", 2000).min(MAX_CONTEXT_TOKENS);
    let channel = get_str(args, "channel");
    let person_id = get_str(args, "person_id")
        .and_then(|s| Uuid::parse_str(s).ok());
    let namespace = get_str(args, "namespace");
    // Optional floor (0.0–1.0): exclude low-confidence / superseded facts from context.
    let min_confidence = args
        .get("min_confidence")
        .and_then(|v| v.as_f64())
        .map(|v| (v as f32).clamp(0.0, 1.0));

    let context = cortex
        .get_context_filtered(max_tokens, channel, person_id, namespace, min_confidence)
        .map_err(|e| e.to_string())?;

    Ok(context)
}

fn tool_belief_observe(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let key = get_str(args, "key").ok_or("missing 'key'")?;
    let supports = args
        .get("supports")
        .and_then(|v| v.as_bool())
        .ok_or("missing 'supports'")?;
    let strength = get_f32(args, "strength", 0.5);

    let belief = cortex
        .observe_belief(key, supports, strength)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "key": belief.key,
        "probability": format!("{:.4}", belief.probability),
        "observations": belief.observations,
        "status": if belief.probability > 0.7 { "confident" }
                  else if belief.probability > 0.3 { "uncertain" }
                  else { "unlikely" }
    })
    .to_string())
}

fn tool_belief_list(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let threshold = get_f32(args, "threshold", 0.6);

    let beliefs = cortex
        .get_beliefs(threshold)
        .map_err(|e| e.to_string())?;

    let items: Vec<Value> = beliefs
        .iter()
        .map(|b| {
            json!({
                "key": b.key,
                "probability": format!("{:.4}", b.probability),
                "observations": b.observations,
            })
        })
        .collect();

    Ok(json!({
        "beliefs": items,
        "total": items.len(),
        "threshold": threshold
    })
    .to_string())
}

fn tool_person_resolve(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let name = get_str(args, "name").ok_or("missing 'name'")?;
    let channel = get_str(args, "channel").ok_or("missing 'channel'")?;
    let channel_user_id = get_str(args, "channel_user_id").ok_or("missing 'channel_user_id'")?;

    let person = cortex
        .add_person(name, channel, channel_user_id)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "id": person.id.to_string(),
        "name": person.display_name,
        "identities": person.identities.iter().map(|i| {
            json!({ "channel": i.channel, "user_id": i.channel_user_id })
        }).collect::<Vec<_>>(),
    })
    .to_string())
}

fn tool_fact_add(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let subject = get_str(args, "subject").ok_or("missing 'subject'")?;
    let predicate = get_str(args, "predicate").ok_or("missing 'predicate'")?;
    let object = get_str(args, "object").ok_or("missing 'object'")?;
    let confidence = get_f32(args, "confidence", 0.8);
    let channel = get_str(args, "channel").unwrap_or("manual");

    let mem = cortex
        .add_fact(subject, predicate, object, confidence, channel, None)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "id": mem.id.to_string(),
        "triple": format!("{} {} {}", subject, predicate, object),
        "confidence": confidence,
        "status": "stored"
    })
    .to_string())
}

fn tool_preference_set(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let key = get_str(args, "key").ok_or("missing 'key'")?;
    let value = get_str(args, "value").ok_or("missing 'value'")?;
    let confidence = get_f32(args, "confidence", 0.9);

    let mem = cortex
        .add_preference(key, value, confidence)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "id": mem.id.to_string(),
        "preference": format!("{} = {}", key, value),
        "confidence": confidence,
        "status": "stored"
    })
    .to_string())
}

fn tool_memory_infer(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let text = get_str(args, "text").ok_or("missing 'text'")?;
    let knowledge = cortex.infer(text);

    let facts: Vec<Value> = knowledge.facts.iter().map(|f| {
        json!({
            "subject": f.subject,
            "predicate": f.predicate,
            "object": f.object,
            "confidence": f.confidence,
        })
    }).collect();

    let prefs: Vec<Value> = knowledge.preferences.iter().map(|p| {
        json!({
            "key": p.key,
            "value": p.value,
            "confidence": p.confidence,
        })
    }).collect();

    let temporal = match knowledge.temporal_hint {
        cortex_core::inference::TemporalHint::Temporary => "temporary",
        cortex_core::inference::TemporalHint::Permanent => "permanent",
        cortex_core::inference::TemporalHint::Unknown => "unknown",
    };

    Ok(json!({
        "facts": facts,
        "preferences": prefs,
        "temporal_hint": temporal,
        "total_extracted": facts.len() + prefs.len(),
    }).to_string())
}

fn tool_contradiction_check(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let subject = get_str(args, "subject").ok_or("missing 'subject'")?;
    let predicate = get_str(args, "predicate").ok_or("missing 'predicate'")?;
    let object = get_str(args, "object").ok_or("missing 'object'")?;

    let contradictions = cortex
        .check_contradictions(subject, predicate, object)
        .map_err(|e| e.to_string())?;

    let items: Vec<Value> = contradictions.iter().map(|(mem, score)| {
        let existing = match &mem.content {
            cortex_core::types::MemContent::Fact { subject, predicate, object } => {
                format!("{} {} {}", subject, predicate, object)
            }
            _ => format!("{:?}", mem.content),
        };
        json!({
            "id": mem.id.to_string(),
            "existing_fact": existing,
            "conflict_score": score,
        })
    }).collect();

    Ok(json!({
        "proposed": format!("{} {} {}", subject, predicate, object),
        "contradictions": items,
        "has_conflict": !items.is_empty(),
    }).to_string())
}

fn tool_memory_compress(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let min_messages = get_usize(args, "min_messages", 5);
    let max_age_days = args
        .get("max_age_days")
        .and_then(|v| v.as_i64())
        .unwrap_or(7);
    let min_messages = min_messages.max(1); // prevent compressing everything
    let max_age_days = max_age_days.max(1); // prevent compressing fresh memories

    let report = cortex
        .run_compression(min_messages, max_age_days)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "sessions_compressed": report.sessions_compressed,
        "episodes_consumed": report.episodes_consumed,
        "summaries_created": report.summaries_created,
    }).to_string())
}

fn tool_relationship_extract(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let text = get_str(args, "text").ok_or("missing 'text'")?;
    let rels = cortex.extract_relationships(text);

    let items: Vec<serde_json::Value> = rels.iter().map(|r| {
        json!({
            "person_a": r.person_a,
            "person_b": r.person_b,
            "relation": r.relation,
            "confidence": r.confidence,
        })
    }).collect();

    Ok(json!({
        "relationships": items,
        "total": items.len(),
    }).to_string())
}

fn tool_memory_stats(cortex: &Arc<Cortex>) -> Result<String, String> {
    let stats = cortex.stats().map_err(|e| e.to_string())?;
    let metrics = cortex.metrics();
    let (semantic_active, embedding_dim) = cortex.embedding_status();

    // Health score (inspired by openclaw-auto-dream)
    // Five dimensions, each 0.0–1.0:
    let total_memories = stats.episodic + stats.semantic + stats.procedural + stats.archived;
    let freshness: f32 = if stats.episodic > 0 { 0.8 } else { 0.0 };
    let coverage: f32 =
        ((stats.semantic as f32) / (total_memories.max(1) as f32)).min(1.0);
    let coherence: f32 = if stats.beliefs > 0 { 0.7 } else { 0.3 };
    let efficiency: f32 = if total_memories > 0 {
        1.0 - ((stats.archived as f32) / (total_memories.max(1) as f32))
    } else {
        1.0
    };
    let reachability: f32 = if stats.people > 0 { 0.8 } else { 0.4 };

    let health_score: f32 = (freshness * 0.25
        + coverage * 0.25
        + coherence * 0.20
        + efficiency * 0.15
        + reachability * 0.15)
        * 100.0;

    Ok(json!({
        "episodic": stats.episodic,
        "semantic": stats.semantic,
        "procedural": stats.procedural,
        "archived": stats.archived,
        "people": stats.people,
        "beliefs": stats.beliefs,
        "index_size": stats.index_size,
        "total": stats.total,
        "embeddings": {
            // Lets a client/agent detect degraded recall instead of hitting it silently.
            "semantic_search_active": semantic_active,
            "embedding_dim": embedding_dim,
            "embedded_memories": stats.index_size,
            "recall_mode": if semantic_active { "semantic+keyword" } else { "keyword-only (no active embeddings)" },
        },
        "metrics": {
            "ingests": metrics.ingests,
            "batch_ingests": metrics.batch_ingests,
            "retrievals": metrics.retrievals,
            "dedup_hits": metrics.dedup_hits,
            "consolidations": metrics.consolidations,
            "decay_runs": metrics.decay_runs,
            "archives": metrics.archives,
        },
        "health": {
            "score": (health_score * 100.0).round() / 100.0,
            "freshness": freshness,
            "coverage": (coverage * 1000.0).round() / 1000.0,
            "coherence": coherence,
            "efficiency": (efficiency * 1000.0).round() / 1000.0,
            "reachability": reachability,
        }
    }).to_string())
}

fn tool_fact_query(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let entity = get_str(args, "entity").ok_or("missing 'entity'")?;
    let facts = cortex.query_facts(entity).map_err(|e| e.to_string())?;

    let items: Vec<Value> = facts.iter().map(|m| {
        if let cortex_core::types::MemContent::Fact { subject, predicate, object } = &m.content {
            json!({
                "id": m.id.to_string(),
                "subject": subject,
                "predicate": predicate,
                "object": object,
                "confidence": format!("{:.2}", m.salience.base_score),
            })
        } else {
            json!({})
        }
    }).collect();

    Ok(json!({
        "entity": entity,
        "facts": items,
        "total": items.len(),
    }).to_string())
}

fn tool_preference_query(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let key = get_str(args, "key").ok_or("missing 'key'")?;
    let prefs = cortex.query_preferences(key).map_err(|e| e.to_string())?;

    let items: Vec<Value> = prefs.iter().map(|m| {
        if let cortex_core::types::MemContent::Preference { key, value, confidence } = &m.content {
            json!({
                "id": m.id.to_string(),
                "key": key,
                "value": value,
                "confidence": confidence,
            })
        } else {
            json!({})
        }
    }).collect();

    Ok(json!({
        "preferences": items,
        "total": items.len(),
    }).to_string())
}

fn tool_person_list(cortex: &Arc<Cortex>) -> Result<String, String> {
    let people = cortex.list_people().map_err(|e| e.to_string())?;

    let items: Vec<Value> = people.iter().map(|p| {
        json!({
            "id": p.id.to_string(),
            "name": p.display_name,
            "relationship": p.relationship_to_user,
            "interactions": p.interaction_count,
            "last_seen": p.last_seen.to_rfc3339(),
            "channels": p.identities.iter().map(|i| {
                json!({ "channel": i.channel, "user_id": i.channel_user_id })
            }).collect::<Vec<_>>(),
        })
    }).collect();

    Ok(json!({
        "people": items,
        "total": items.len(),
    }).to_string())
}

fn tool_memory_decay(cortex: &Arc<Cortex>) -> Result<String, String> {
    let updated = cortex.run_decay().map_err(|e| e.to_string())?;
    Ok(json!({
        "memories_updated": updated,
        "status": "decay_complete",
    }).to_string())
}

fn tool_memory_archive(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let id_str = get_str(args, "id").ok_or("missing 'id'")?;
    let id = Uuid::parse_str(id_str).map_err(|e| format!("Invalid UUID: {}", e))?;

    cortex.archive_memory(id).map_err(|e| e.to_string())?;

    Ok(json!({
        "id": id_str,
        "status": "archived",
    }).to_string())
}

fn tool_memory_ingest_batch(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let items_arr = args.get("items")
        .and_then(|v| v.as_array())
        .ok_or("missing 'items' array")?;
    if items_arr.len() > MAX_BATCH_SIZE {
        return Err(format!("batch too large: {} items (max {})", items_arr.len(), MAX_BATCH_SIZE));
    }

    // Validate each item's text size (same guard as memory_ingest)
    for (i, item) in items_arr.iter().enumerate() {
        let text_len = item.get("text").and_then(|v| v.as_str()).map(|s| s.len()).unwrap_or(0);
        if text_len > MAX_INGEST_TEXT_BYTES {
            return Err(format!("item[{}] text too large: {} bytes (max {})", i, text_len, MAX_INGEST_TEXT_BYTES));
        }
    }

    let items: Vec<cortex_core::types::BatchIngestItem> = items_arr.iter().map(|item| {
        cortex_core::types::BatchIngestItem {
            text: item.get("text").and_then(|v| v.as_str()).unwrap_or("").to_string(),
            channel: item.get("channel").and_then(|v| v.as_str()).unwrap_or("unknown").to_string(),
            user_id: item.get("user_id").and_then(|v| v.as_str()).map(String::from),
            salience_hint: item.get("salience").and_then(|v| v.as_f64()).map(|v| v as f32),
            embedding: None,
            namespace: item.get("namespace").and_then(|v| v.as_str()).map(String::from),
            privacy: parse_privacy(item).ok().flatten(),
        }
    }).collect();

    let report = cortex.ingest_batch(items).map_err(|e| e.to_string())?;

    Ok(json!({
        "total_submitted": report.total_submitted,
        "stored": report.stored,
        "deduplicated": report.deduplicated,
        "skipped_by_plugin": report.skipped_by_plugin,
        "status": "batch_complete"
    }).to_string())
}

fn tool_tag_list_taxonomy(cortex: &Arc<Cortex>) -> Result<String, String> {
    use std::collections::HashMap;

    let mut tag_counts: HashMap<String, usize> = HashMap::new();
    let tiers = [
        cortex_core::types::MemoryTier::Episodic,
        cortex_core::types::MemoryTier::Semantic,
        cortex_core::types::MemoryTier::Procedural,
    ];

    let mut scanned_total: usize = 0;
    let mut truncated = false;
    for tier in &tiers {
        if let Ok(mems) = cortex.storage().list_by_tier(*tier, MAX_TAG_SCAN_PER_TIER) {
            if mems.len() >= MAX_TAG_SCAN_PER_TIER {
                truncated = true;
            }
            scanned_total += mems.len();
            for mem in mems {
                for tag in &mem.tags {
                    *tag_counts.entry(tag.clone()).or_insert(0) += 1;
                }
            }
        }
    }

    let mut sorted: Vec<_> = tag_counts.into_iter().collect();
    sorted.sort_by_key(|e| std::cmp::Reverse(e.1));

    let items: Vec<Value> = sorted.iter().map(|(tag, count)| {
        json!({ "tag": tag, "count": count })
    }).collect();

    Ok(json!({
        "tags": items,
        "total_unique": items.len(),
        "scanned": scanned_total,
        "truncated": truncated,
    }).to_string())
}

fn tool_memory_delete(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let id_str = get_str(args, "id").ok_or("missing 'id'")?;
    let id = Uuid::parse_str(id_str).map_err(|e| format!("Invalid UUID: {}", e))?;

    cortex.delete_memory(id).map_err(|e| e.to_string())?;

    Ok(json!({
        "id": id_str,
        "status": "deleted",
    }).to_string())
}

fn tool_memory_restore(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let id_str = get_str(args, "id").ok_or("missing 'id'")?;
    let id = Uuid::parse_str(id_str).map_err(|e| format!("Invalid UUID: {}", e))?;
    let tier_str = get_str(args, "tier").unwrap_or("episodic");
    let tier = cortex_core::types::MemoryTier::parse(tier_str)
        .ok_or_else(|| format!("Invalid tier: {}", tier_str))?;

    cortex.restore_memory(id, tier).map_err(|e| e.to_string())?;

    Ok(json!({
        "id": id_str,
        "restored_to": tier_str,
        "status": "restored",
    }).to_string())
}

fn tool_namespace_list(cortex: &Arc<Cortex>) -> Result<String, String> {
    let namespaces = cortex.list_namespaces().map_err(|e| e.to_string())?;

    let items: Vec<Value> = namespaces.iter().map(|(ns, count)| {
        json!({ "namespace": ns, "count": count })
    }).collect();

    Ok(json!({
        "namespaces": items,
        "total": items.len(),
    }).to_string())
}

fn tool_person_merge(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let target_str = get_str(args, "target_id").ok_or("missing 'target_id'")?;
    let source_str = get_str(args, "source_id").ok_or("missing 'source_id'")?;
    let target_id = Uuid::parse_str(target_str).map_err(|e| format!("Invalid target UUID: {}", e))?;
    let source_id = Uuid::parse_str(source_str).map_err(|e| format!("Invalid source UUID: {}", e))?;
    if target_id == source_id {
        return Err("cannot merge a person with themselves".to_string());
    }

    let merged = cortex.merge_people(target_id, source_id).map_err(|e| e.to_string())?;

    Ok(json!({
        "merged_id": merged.id.to_string(),
        "display_name": merged.display_name,
        "identities": merged.identities.len(),
        "status": "merged",
    }).to_string())
}

// ── Sync tools ───────────────────────────────────────────────────────────────

fn tool_sync_enable(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    use cortex_core::sync::{SyncConfig, provider::{detect_all_providers, CloudProvider}};

    // Resolve provider
    let providers = detect_all_providers();
    let requested = get_str(args, "provider");

    let detected = if let Some(name) = requested {
        let target = match name.to_lowercase().as_str() {
            "icloud" => CloudProvider::ICloud,
            "gdrive" | "googledrive" | "google_drive" => CloudProvider::GoogleDrive,
            "onedrive" => CloudProvider::OneDrive,
            "dropbox" => CloudProvider::Dropbox,
            _ => return Err(format!("Unknown provider: {name}. Use: icloud, gdrive, onedrive, dropbox")),
        };
        providers.into_iter()
            .find(|p| std::mem::discriminant(&p.provider) == std::mem::discriminant(&target))
            .ok_or_else(|| format!("{} not found on this machine", target.as_str()))?
    } else {
        providers.into_iter().next()
            .ok_or("No cloud providers detected. Install iCloud Drive, Google Drive, OneDrive, or Dropbox.")?
    };

    // Device name: arg > hostname > "unknown"
    let device_name = get_str(args, "device_name")
        .map(String::from)
        .unwrap_or_else(|| {
            hostname::get()
                .ok()
                .and_then(|h| h.into_string().ok())
                .unwrap_or_else(|| "unknown-device".into())
        });

    // Device ID: random UUID
    let device_id = Uuid::new_v4().to_string();

    // Passphrase: arg, or generate one — but the master key must NEVER enter the model's
    // context (it decrypts and can forge everything in the sync folder). A generated key is
    // only acceptable if it can be kept in the OS keychain; otherwise the human must set
    // it up in a terminal, where it is shown to them directly.
    if get_str(args, "passphrase").is_some() {
        return Err("Don't pass the sync passphrase through the assistant — it would end up in \
            the conversation. Set CORTEX_SYNC_PASSPHRASE for this server, or run \
            `cortex-mcp-server sync enable` in a terminal."
            .into());
    }
    let passphrase = match std::env::var("CORTEX_SYNC_PASSPHRASE").ok().filter(|p| !p.is_empty()) {
        Some(p) => p,
        None => {
            use std::fmt::Write;
            let bytes: [u8; 24] = std::array::from_fn(|_| rand::random::<u8>());
            let mut s = String::with_capacity(48);
            for b in &bytes {
                let _ = write!(s, "{:02x}", b);
            }
            if !cortex_core::sync::secret::store_passphrase(&device_id, &s) {
                return Err("Cannot store a generated passphrase securely on this machine (no OS \
                    keychain). Run `cortex-mcp-server sync enable` in a terminal — it shows the \
                    passphrase to you directly — or set CORTEX_SYNC_PASSPHRASE."
                    .into());
            }
            s
        }
    };

    let config = SyncConfig::new(detected.sync_dir.clone(), device_id.clone(), device_name.clone())
        .with_encryption(&passphrase);

    cortex.enable_sync(config).map_err(|e| e.to_string())?;

    // Do an initial pull
    let pulled = cortex.sync_pull().unwrap_or(0);

    // Keep pulling automatically (poll + fs-watcher) for the life of this server.
    if let Err(e) = cortex.start_background_sync() {
        tracing::warn!(error = %e, "background sync did not start — pulls are manual");
    }

    Ok(json!({
        "status": "enabled",
        "provider": detected.provider.as_str(),
        "sync_dir": redact_emails(&detected.sync_dir.display().to_string()),
        "device_id": device_id,
        "encryption": true,
        "remote_changes_applied": pulled,
        "message": "Sync enabled. The passphrase is in this machine's OS keychain (service \
            'cortex-sync') and is deliberately not shown here; you need it on other devices."
    }).to_string())
}

fn tool_sync_pull(cortex: &Arc<Cortex>) -> Result<String, String> {
    let applied = cortex.sync_pull().map_err(|e| e.to_string())?;
    Ok(json!({
        "applied": applied,
        "status": if applied > 0 { "changes_applied" } else { "up_to_date" },
    }).to_string())
}

fn tool_sync_status(cortex: &Arc<Cortex>) -> Result<String, String> {
    match cortex.sync_status() {
        Some(status) => {
            Ok(json!({
                "enabled": true,
                "device_id": status.device_id,
                "provider": status.provider,
                "sync_dir": redact_emails(&status.sync_dir),
                "remote_devices": status.remote_devices,
            }).to_string())
        }
        None => {
            let providers = cortex_core::sync::provider::detect_all_providers();
            if providers.is_empty() {
                Ok(json!({
                    "enabled": false,
                    "status": "no_providers_detected",
                    "message": "No cloud storage providers found. Install iCloud Drive, Google Drive, OneDrive, or Dropbox.",
                }).to_string())
            } else {
                let detected: Vec<Value> = providers.iter().map(|p| {
                    json!({
                        "provider": p.provider.as_str(),
                        "sync_dir": redact_emails(&p.sync_dir.display().to_string()),
                    })
                }).collect();
                Ok(json!({
                    "enabled": false,
                    "status": "ready",
                    "message": "Cloud providers detected. Use sync_enable to start syncing.",
                    "available_providers": detected,
                }).to_string())
            }
        }
    }
}

fn tool_sync_providers() -> Result<String, String> {
    let providers = cortex_core::sync::provider::detect_all_providers();
    let items: Vec<Value> = providers.iter().map(|p| {
        json!({
            "provider": p.provider.as_str(),
            "sync_dir": redact_emails(&p.sync_dir.display().to_string()),
            "exists": p.sync_dir.exists(),
        })
    }).collect();

    Ok(json!({
        "providers": items,
        "total": items.len(),
    }).to_string())
}

/// Redact email addresses embedded in a path (or any string) before it is returned to the
/// LLM/agent in an MCP tool response.
///
/// PRIVACY: cloud-provider sync directories embed the user's account email in the path — e.g.
/// `.../CloudStorage/GoogleDrive-alice@gmail.com/My Drive/cortex-sync`. Emitting that raw
/// (as `sync_dir`) leaks the user's real email into the model's context on every
/// `sync_status` / `sync_providers` / `sync_enable` call. Cortex's mission is 100% local,
/// zero-telemetry privacy, so account PII must never ride along in a tool response.
///
/// Two passes:
/// 1. A known provider folder (`GoogleDrive-<account>`) has everything after the prefix up to
///    the next path separator replaced wholesale — no character whitelist can leak part of it.
/// 2. Any other email: the local-part is RFC 5322 `atext` plus `.` and non-ASCII (minus `/`
///    and `\`, which are path separators); the domain is `[A-Za-z0-9.-]` with at least one dot.
pub(crate) fn redact_emails(input: &str) -> String {
    redact_bare_emails(&redact_provider_accounts(input))
}

/// Cloud-storage folder names that embed an account (`GoogleDrive-<email>`) or an
/// organisation (`OneDrive-<Tenant>`, `Dropbox-<Team>`, `Box-<Org>`).
const PROVIDER_PREFIXES: &[&str] = &["GoogleDrive-", "OneDrive-", "Dropbox-", "Box-"];

fn redact_provider_accounts(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    let mut rest = input;
    // Only at the start of a path segment, so e.g. "Inbox-…" or "XBox-…" don't match.
    let at_segment_start = |s: &str, pos: usize| pos == 0 || s[..pos].ends_with(['/', '\\']);
    while let Some((pos, prefix)) = PROVIDER_PREFIXES
        .iter()
        .filter_map(|p| {
            rest.match_indices(p).find(|(pos, _)| at_segment_start(rest, *pos)).map(|(pos, _)| (pos, *p))
        })
        .min_by_key(|(pos, _)| *pos)
    {
        let account_start = pos + prefix.len();
        let account_len = rest[account_start..]
            .find(['/', '\\'])
            .unwrap_or(rest.len() - account_start);
        let account = &rest[account_start..account_start + account_len];
        out.push_str(&rest[..account_start]);
        if account.contains('@') {
            out.push_str("[redacted-email]");
        } else if prefix != "GoogleDrive-" && !account.is_empty() {
            out.push_str("[redacted-account]");
        } else {
            out.push_str(account);
        }
        rest = &rest[account_start + account_len..];
    }
    out.push_str(rest);
    out
}

fn redact_bare_emails(input: &str) -> String {
    let bytes = input.as_bytes();
    let is_local = |c: u8| {
        c.is_ascii_alphanumeric()
            || c >= 0x80
            || matches!(
                c,
                b'.' | b'!' | b'#' | b'$' | b'%' | b'&' | b'\'' | b'*' | b'+' | b'=' | b'?'
                    | b'^' | b'_' | b'`' | b'{' | b'|' | b'}' | b'~' | b'-'
            )
    };
    // Non-ASCII bytes cover IDN domains (u@例え.jp); a run of them always ends at an ASCII
    // byte or the end of input, so slicing stays on char boundaries.
    let is_domain = |c: u8| c.is_ascii_alphanumeric() || c >= 0x80 || matches!(c, b'.' | b'-');

    let mut out = String::with_capacity(input.len());
    let mut i = 0;
    // Track how far we've already copied into `out`.
    let mut copied = 0;
    while i < bytes.len() {
        if bytes[i] == b'@' {
            // Expand left over the local-part. A quoted local-part (`"a b"@x.com`) may
            // contain any character, so take everything back to the opening quote.
            let mut start = i;
            if start > copied && bytes[start - 1] == b'"' {
                if let Some(open) = input[copied..start - 1].rfind('"') {
                    start = copied + open;
                }
            } else {
                while start > copied && is_local(bytes[start - 1]) {
                    start -= 1;
                }
            }
            // Expand right over the domain.
            let mut end = i + 1;
            while end < bytes.len() && is_domain(bytes[end]) {
                end += 1;
            }
            // Sentence punctuation ("mail a@x.com.") is not part of the domain.
            while end > i + 1 && bytes[end - 1] == b'.' {
                end -= 1;
            }
            // Only treat it as an email if there is a non-empty local-part and a domain that
            // contains a dot (so a bare `foo@bar` or `@` alone is left untouched).
            let has_local = start < i;
            let domain = &input[i + 1..end];
            let looks_like_email = has_local && domain.contains('.') && !domain.starts_with('.');
            if looks_like_email {
                out.push_str(&input[copied..start]);
                out.push_str("[redacted-email]");
                copied = end;
                i = end;
                continue;
            }
        }
        i += 1;
    }
    out.push_str(&input[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redacts_google_drive_account_email_in_sync_path() {
        let path = "/Users/alice/Library/CloudStorage/GoogleDrive-ethanchang32@gmail.com/My Drive/cortex-sync";
        let redacted = redact_emails(path);
        assert!(
            !redacted.contains("ethanchang32@gmail.com"),
            "account email must not survive redaction: {redacted}"
        );
        assert!(
            !redacted.contains("ethanchang32"),
            "email local-part must not survive redaction: {redacted}"
        );
        assert!(
            !redacted.contains("gmail.com"),
            "email domain must not survive redaction: {redacted}"
        );
        assert!(redacted.contains("[redacted-email]"), "should mark the redaction: {redacted}");
        // The provider prefix and the rest of the path are preserved for usefulness.
        assert!(redacted.contains("GoogleDrive-"), "provider hint preserved: {redacted}");
        assert!(redacted.contains("cortex-sync"), "trailing path preserved: {redacted}");
    }

    #[test]
    fn leaves_non_email_paths_untouched() {
        let path = "/Users/alice/Library/Mobile Documents/iCloud~Drive/cortex-sync";
        assert_eq!(redact_emails(path), path);
        // A bare `@` or `user@host` with no dotted domain is not an email address.
        assert_eq!(redact_emails("build @ host"), "build @ host");
        assert_eq!(redact_emails("a@localhost"), "a@localhost");
    }

    #[test]
    fn redacts_multiple_emails() {
        let s = "from a@x.com to b@y.org";
        let r = redact_emails(s);
        assert_eq!(r, "from [redacted-email] to [redacted-email]");
    }

    #[test]
    fn redacts_hyphenated_local_part_in_sync_path() {
        let path = "/Users/a/Library/CloudStorage/GoogleDrive-alice-smith@example.com/My Drive";
        assert_eq!(
            redact_emails(path),
            "/Users/a/Library/CloudStorage/GoogleDrive-[redacted-email]/My Drive"
        );
    }

    #[test]
    fn redacts_trailing_hyphen_local_part() {
        let path = "/CloudStorage/GoogleDrive-alice-@example.com/x";
        let r = redact_emails(path);
        assert!(!r.contains("alice"), "local part must not survive: {r}");
        assert!(!r.contains("example.com"), "domain must not survive: {r}");
    }

    #[test]
    fn redacts_bare_hyphenated_email() {
        assert_eq!(redact_emails("x mary-jane@ex.org y"), "x [redacted-email] y");
    }

    #[test]
    fn redacts_apostrophe_and_unusual_local_parts_in_provider_folder() {
        for acct in ["alice.o'connor@example.com", "a!b#c@example.com", "jos\u{e9}@example.com"] {
            let path = format!("/CloudStorage/GoogleDrive-{acct}/My Drive/cortex-sync");
            assert_eq!(
                redact_emails(&path),
                "/CloudStorage/GoogleDrive-[redacted-email]/My Drive/cortex-sync"
            );
        }
    }

    #[test]
    fn redacts_apostrophe_bare_email() {
        assert_eq!(redact_emails("to o'brien@ex.com now"), "to [redacted-email] now");
    }

    #[test]
    fn keeps_provider_folder_without_email() {
        let p = "/CloudStorage/GoogleDrive-Shared/x";
        assert_eq!(redact_emails(p), p);
    }

    #[test]
    fn redacts_trailing_dot_and_idn_emails() {
        assert_eq!(redact_emails("mail a@x.com."), "mail [redacted-email].");
        assert_eq!(redact_emails("to u@例え.jp now"), "to [redacted-email] now");
        assert_eq!(redact_emails("u@mail.例え.jp"), "[redacted-email]");
    }

    #[test]
    fn redacts_org_named_provider_folders_at_segment_start_only() {
        assert_eq!(
            redact_emails("/CloudStorage/OneDrive-AcmeCorp/x"),
            "/CloudStorage/OneDrive-[redacted-account]/x"
        );
        assert_eq!(redact_emails("/d/Dropbox-Team Blue/x"), "/d/Dropbox-[redacted-account]/x");
        assert_eq!(redact_emails("/mail/Inbox-2024/x"), "/mail/Inbox-2024/x");
    }

    #[test]
    fn redact_local_hides_home_directory() {
        let home = std::env::var("HOME").unwrap_or_default();
        if home.len() > 1 {
            let s = format!("{home}/Library/CloudStorage/x");
            assert_eq!(redact_local(&s), "~/Library/CloudStorage/x");
        }
    }

    #[test]
    fn redacts_quoted_local_part() {
        assert_eq!(redact_emails(r#"x "a b"@x.com y"#), "x [redacted-email] y");
    }

    #[test]
    fn scrub_keeps_json_valid_with_quoted_emails() {
        let input = json!({"device_id": "\"a b\"@x.com", "remote_devices": ["GoogleDrive-u@x.com"]}).to_string();
        let out = scrub(Ok(input)).unwrap();
        let v: Value = serde_json::from_str(&out).expect("still valid JSON");
        assert_eq!(v["device_id"], "[redacted-email]");
        assert_eq!(v["remote_devices"][0], "GoogleDrive-[redacted-email]");
    }
}
