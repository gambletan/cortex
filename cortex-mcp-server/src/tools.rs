//! MCP tool definitions and execution for Cortex memory engine.

use cortex_core::Cortex;
use serde_json::{json, Value};
use std::sync::Arc;
use uuid::Uuid;

/// Return the list of available tools (MCP tool schema format).
pub fn list_tools() -> Value {
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
                    }
                },
                "required": ["text", "channel"]
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
        }
    ])
}

/// Execute a tool call and return the result as a string.
pub fn call_tool(cortex: &Arc<Cortex>, name: &str, args: &Value) -> Result<String, String> {
    match name {
        "memory_ingest" => tool_memory_ingest(cortex, args),
        "memory_search" => tool_memory_search(cortex, args),
        "memory_context" => tool_memory_context(cortex, args),
        "memory_consolidate" => tool_memory_consolidate(cortex),
        "belief_observe" => tool_belief_observe(cortex, args),
        "belief_list" => tool_belief_list(cortex, args),
        "person_resolve" => tool_person_resolve(cortex, args),
        "fact_add" => tool_fact_add(cortex, args),
        "preference_set" => tool_preference_set(cortex, args),
        _ => Err(format!("Unknown tool: {name}")),
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

fn tool_memory_ingest(cortex: &Arc<Cortex>, args: &Value) -> Result<String, String> {
    let text = get_str(args, "text").ok_or("missing 'text'")?;
    let channel = get_str(args, "channel").ok_or("missing 'channel'")?;
    let user_id = get_str(args, "user_id");
    let salience = args.get("salience").and_then(|v| v.as_f64()).map(|v| v as f32);
    let embedding = get_embedding(args);

    let mem = cortex
        .ingest(text, channel, user_id, salience, embedding)
        .map_err(|e| e.to_string())?;

    Ok(json!({
        "id": mem.id.to_string(),
        "tier": mem.tier.as_str(),
        "created_at": mem.temporal.ingestion_time.to_rfc3339(),
        "status": "stored"
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
    let limit = get_usize(args, "limit", 10);
    let channel = get_str(args, "channel");
    let person_id = get_str(args, "person_id")
        .and_then(|s| Uuid::parse_str(s).ok());
    let embedding = get_embedding(args);

    let results = cortex
        .retrieve(query, limit, channel, person_id, embedding)
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
    let max_tokens = get_usize(args, "max_tokens", 2000);
    let channel = get_str(args, "channel");
    let person_id = get_str(args, "person_id")
        .and_then(|s| Uuid::parse_str(s).ok());

    let context = cortex
        .get_context(max_tokens, channel, person_id)
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
