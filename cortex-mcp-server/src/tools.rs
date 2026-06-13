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
                                "namespace": { "type": "string", "description": "Namespace for isolation (optional)" }
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
            "description": "Enable cross-device cloud sync. Auto-detects cloud provider (iCloud/Google Drive/OneDrive/Dropbox), generates device ID, and starts syncing. Optionally encrypts with AES-256-GCM. Returns the passphrase (user must save it for other devices).",
            "inputSchema": {
                "type": "object",
                "properties": {
                    "provider": {
                        "type": "string",
                        "description": "Cloud provider: 'icloud', 'gdrive', 'onedrive', 'dropbox'. If omitted, auto-detects the first available."
                    },
                    "passphrase": {
                        "type": "string",
                        "description": "Encryption passphrase for AES-256-GCM. If omitted, a random one is generated and returned."
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
        "memory_delete" => tool_memory_delete(cortex, args),
        "memory_restore" => tool_memory_restore(cortex, args),
        "namespace_list" => tool_namespace_list(cortex),
        "person_merge" => tool_person_merge(cortex, args),
        "sync_enable" => tool_sync_enable(cortex, args),
        "sync_pull" => tool_sync_pull(cortex),
        "sync_status" => tool_sync_status(cortex),
        "sync_providers" => tool_sync_providers(),
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

fn content_to_string(content: &cortex_core::types::MemContent) -> String {
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

    let context = cortex
        .get_context_with_namespace(max_tokens, channel, person_id, namespace)
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

    // Passphrase: arg or generate random 48-char hex
    let passphrase = get_str(args, "passphrase")
        .map(String::from)
        .unwrap_or_else(|| {
            use std::fmt::Write;
            let bytes: [u8; 24] = std::array::from_fn(|_| rand::random::<u8>());
            let mut s = String::with_capacity(48);
            for b in &bytes {
                let _ = write!(s, "{:02x}", b);
            }
            s
        });

    let config = SyncConfig::new(detected.sync_dir.clone(), device_id.clone(), device_name.clone())
        .with_encryption(&passphrase);

    cortex.enable_sync(config).map_err(|e| e.to_string())?;

    // Do an initial pull
    let pulled = cortex.sync_pull().unwrap_or(0);

    Ok(json!({
        "status": "enabled",
        "provider": detected.provider.as_str(),
        "sync_dir": detected.sync_dir.display().to_string(),
        "device_id": device_id,
        "device_name": device_name,
        "encryption": true,
        "passphrase": passphrase,
        "remote_changes_applied": pulled,
        "message": "Sync enabled! IMPORTANT: Save the passphrase — you need it on other devices."
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
                "device_name": status.device_name,
                "provider": status.provider,
                "sync_dir": status.sync_dir,
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
                        "sync_dir": p.sync_dir.display().to_string(),
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
            "sync_dir": p.sync_dir.display().to_string(),
            "exists": p.sync_dir.exists(),
        })
    }).collect();

    Ok(json!({
        "providers": items,
        "total": items.len(),
    }).to_string())
}
