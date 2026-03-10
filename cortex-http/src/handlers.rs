//! HTTP request handlers for Cortex REST API.

use std::sync::Arc;

use axum::{
    extract::{Json, Query, State},
    http::StatusCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;

use crate::AppState;
use cortex_core::types::*;

// ── Shared helpers ───────────────────────────────────────────────────────────

type AppResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn err(status: StatusCode, msg: impl Into<String>) -> (StatusCode, Json<Value>) {
    (status, Json(json!({ "error": msg.into() })))
}

fn cortex_err(e: cortex_core::CortexError) -> (StatusCode, Json<Value>) {
    err(StatusCode::INTERNAL_SERVER_ERROR, e.to_string())
}

fn content_to_string(content: &MemContent) -> String {
    match content {
        MemContent::Text(t) => t.clone(),
        MemContent::Fact { subject, predicate, object } => {
            format!("{} {} {}", subject, predicate, object)
        }
        MemContent::Preference { key, value, .. } => format!("{} = {}", key, value),
        other => format!("{:?}", other),
    }
}

// ── Health ───────────────────────────────────────────────────────────────────

pub async fn health() -> Json<Value> {
    Json(json!({
        "status": "ok",
        "engine": "cortex",
        "version": env!("CARGO_PKG_VERSION"),
    }))
}

// ── Ingest ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct IngestRequest {
    pub text: String,
    pub channel: String,
    pub user_id: Option<String>,
    pub salience: Option<f32>,
}

pub async fn ingest(
    State(state): State<Arc<AppState>>,
    Json(req): Json<IngestRequest>,
) -> AppResult {
    let mem = state
        .cortex
        .ingest(
            &req.text,
            &req.channel,
            req.user_id.as_deref(),
            req.salience,
            None,
        )
        .map_err(cortex_err)?;

    Ok(Json(json!({
        "id": mem.id.to_string(),
        "tier": mem.tier.as_str(),
        "created_at": mem.temporal.ingestion_time.to_rfc3339(),
    })))
}

// ── Search ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SearchRequest {
    pub query: String,
    pub limit: Option<usize>,
    pub channel: Option<String>,
    pub person_id: Option<String>,
}

pub async fn search(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SearchRequest>,
) -> AppResult {
    let person_id = req
        .person_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());

    let results = state
        .cortex
        .retrieve(
            &req.query,
            req.limit.unwrap_or(10),
            req.channel.as_deref(),
            person_id,
            None,
        )
        .map_err(cortex_err)?;

    let items: Vec<Value> = results
        .iter()
        .map(|r| {
            json!({
                "id": r.memory.id.to_string(),
                "text": content_to_string(&r.memory.content),
                "score": r.score,
                "tier": r.memory.tier.as_str(),
                "created_at": r.memory.temporal.ingestion_time.to_rfc3339(),
                "channel": r.memory.source.channel,
            })
        })
        .collect();

    Ok(Json(json!({
        "results": items,
        "total": items.len(),
    })))
}

// ── Context ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ContextQuery {
    pub max_tokens: Option<usize>,
    pub channel: Option<String>,
    pub person_id: Option<String>,
}

pub async fn context(
    State(state): State<Arc<AppState>>,
    Query(q): Query<ContextQuery>,
) -> AppResult {
    let person_id = q
        .person_id
        .as_deref()
        .and_then(|s| Uuid::parse_str(s).ok());

    let ctx = state
        .cortex
        .get_context(
            q.max_tokens.unwrap_or(2000),
            q.channel.as_deref(),
            person_id,
        )
        .map_err(cortex_err)?;

    Ok(Json(json!({ "context": ctx })))
}

// ── Consolidate ──────────────────────────────────────────────────────────────

pub async fn consolidate(State(state): State<Arc<AppState>>) -> AppResult {
    let report = state.cortex.run_consolidation().map_err(cortex_err)?;

    Ok(Json(json!({
        "episodes_scanned": report.episodes_scanned,
        "decayed_updated": report.decayed_updated,
        "decayed_swept": report.decayed_swept,
        "promoted_to_semantic": report.promoted_to_semantic,
        "patterns_detected": report.patterns_detected,
    })))
}

// ── Infer ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct InferRequest {
    pub text: String,
}

pub async fn infer(
    State(state): State<Arc<AppState>>,
    Json(req): Json<InferRequest>,
) -> AppResult {
    let knowledge = state.cortex.infer(&req.text);

    let facts: Vec<Value> = knowledge
        .facts
        .iter()
        .map(|f| {
            json!({
                "subject": f.subject,
                "predicate": f.predicate,
                "object": f.object,
                "confidence": f.confidence,
            })
        })
        .collect();

    let prefs: Vec<Value> = knowledge
        .preferences
        .iter()
        .map(|p| json!({ "key": p.key, "value": p.value, "confidence": p.confidence }))
        .collect();

    let temporal = match knowledge.temporal_hint {
        cortex_core::inference::TemporalHint::Temporary => "temporary",
        cortex_core::inference::TemporalHint::Permanent => "permanent",
        cortex_core::inference::TemporalHint::Unknown => "unknown",
    };

    Ok(Json(json!({
        "facts": facts,
        "preferences": prefs,
        "temporal_hint": temporal,
    })))
}

// ── Facts ────────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct AddFactRequest {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: Option<f32>,
    pub channel: Option<String>,
}

pub async fn add_fact(
    State(state): State<Arc<AppState>>,
    Json(req): Json<AddFactRequest>,
) -> AppResult {
    let mem = state
        .cortex
        .add_fact(
            &req.subject,
            &req.predicate,
            &req.object,
            req.confidence.unwrap_or(0.8),
            req.channel.as_deref().unwrap_or("http"),
            None,
        )
        .map_err(cortex_err)?;

    Ok(Json(json!({
        "id": mem.id.to_string(),
        "triple": format!("{} {} {}", req.subject, req.predicate, req.object),
        "status": "stored",
    })))
}

#[derive(Deserialize)]
pub struct ContradictionRequest {
    pub subject: String,
    pub predicate: String,
    pub object: String,
}

pub async fn check_contradictions(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ContradictionRequest>,
) -> AppResult {
    let contradictions = state
        .cortex
        .check_contradictions(&req.subject, &req.predicate, &req.object)
        .map_err(cortex_err)?;

    let items: Vec<Value> = contradictions
        .iter()
        .map(|(mem, score)| {
            let existing = match &mem.content {
                MemContent::Fact { subject, predicate, object } => {
                    format!("{} {} {}", subject, predicate, object)
                }
                _ => format!("{:?}", mem.content),
            };
            json!({
                "id": mem.id.to_string(),
                "existing_fact": existing,
                "conflict_score": score,
            })
        })
        .collect();

    Ok(Json(json!({
        "proposed": format!("{} {} {}", req.subject, req.predicate, req.object),
        "contradictions": items,
        "has_conflict": !items.is_empty(),
    })))
}

// ── Preferences ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct SetPreferenceRequest {
    pub key: String,
    pub value: String,
    pub confidence: Option<f32>,
}

pub async fn set_preference(
    State(state): State<Arc<AppState>>,
    Json(req): Json<SetPreferenceRequest>,
) -> AppResult {
    let mem = state
        .cortex
        .add_preference(&req.key, &req.value, req.confidence.unwrap_or(0.9))
        .map_err(cortex_err)?;

    Ok(Json(json!({
        "id": mem.id.to_string(),
        "preference": format!("{} = {}", req.key, req.value),
        "status": "stored",
    })))
}

// ── Beliefs ──────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct BeliefsQuery {
    pub threshold: Option<f32>,
}

pub async fn list_beliefs(
    State(state): State<Arc<AppState>>,
    Query(q): Query<BeliefsQuery>,
) -> AppResult {
    let beliefs = state
        .cortex
        .get_beliefs(q.threshold.unwrap_or(0.6))
        .map_err(cortex_err)?;

    let items: Vec<Value> = beliefs
        .iter()
        .map(|b| {
            json!({
                "key": b.key,
                "probability": b.probability,
                "observations": b.observations.len(),
            })
        })
        .collect();

    Ok(Json(json!({ "beliefs": items, "total": items.len() })))
}

#[derive(Deserialize)]
pub struct ObserveBelief {
    pub key: String,
    pub supports: bool,
    pub strength: Option<f32>,
}

pub async fn observe_belief(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ObserveBelief>,
) -> AppResult {
    let belief = state
        .cortex
        .observe_belief(&req.key, req.supports, req.strength.unwrap_or(0.5))
        .map_err(cortex_err)?;

    Ok(Json(json!({
        "key": belief.key,
        "probability": belief.probability,
        "observations": belief.observations.len(),
    })))
}

// ── People ───────────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct ResolvePersonRequest {
    pub name: String,
    pub channel: String,
    pub channel_user_id: String,
}

pub async fn resolve_person(
    State(state): State<Arc<AppState>>,
    Json(req): Json<ResolvePersonRequest>,
) -> AppResult {
    let person = state
        .cortex
        .add_person(&req.name, &req.channel, &req.channel_user_id)
        .map_err(cortex_err)?;

    Ok(Json(json!({
        "id": person.id.to_string(),
        "name": person.display_name,
        "identities": person.identities.iter().map(|i| {
            json!({ "channel": i.channel, "user_id": i.channel_user_id })
        }).collect::<Vec<_>>(),
    })))
}

// ── Export/Import ────────────────────────────────────────────────────────────

pub async fn export_all(State(state): State<Arc<AppState>>) -> AppResult {
    let storage = state.cortex.storage();

    let tiers = [MemoryTier::Episodic, MemoryTier::Semantic, MemoryTier::Procedural];
    let mut memories = Vec::new();
    for tier in &tiers {
        let mems = storage.list_by_tier(*tier, 100_000).map_err(cortex_err)?;
        memories.extend(mems);
    }

    let people = storage.list_people().map_err(cortex_err)?;
    let beliefs = storage.list_beliefs_above(0.0).map_err(cortex_err)?;
    let patterns = storage.list_patterns(0).map_err(cortex_err)?;

    Ok(Json(json!({
        "version": "cortex-export-v1",
        "exported_at": chrono::Utc::now().to_rfc3339(),
        "memories": memories,
        "people": people,
        "beliefs": beliefs,
        "patterns": patterns,
        "stats": {
            "memories": memories.len(),
            "people": people.len(),
            "beliefs": beliefs.len(),
            "patterns": patterns.len(),
        }
    })))
}

#[derive(Deserialize)]
pub struct ImportData {
    #[allow(dead_code)]
    pub version: Option<String>,
    pub memories: Option<Vec<MemObject>>,
    pub people: Option<Vec<cortex_core::people::Person>>,
    pub beliefs: Option<Vec<cortex_core::belief::Belief>>,
}

pub async fn import_all(
    State(state): State<Arc<AppState>>,
    Json(data): Json<ImportData>,
) -> AppResult {
    let storage = state.cortex.storage();
    let mut imported = json!({ "memories": 0, "people": 0, "beliefs": 0 });

    if let Some(memories) = data.memories {
        let count = memories.len();
        for mem in &memories {
            storage.store_memory(mem).map_err(cortex_err)?;
            // Re-index embeddings
            if let Some(ref emb) = mem.embedding {
                state.cortex.index().insert(mem.id, emb.clone());
            }
        }
        imported["memories"] = json!(count);
    }

    if let Some(people) = data.people {
        let count = people.len();
        for person in &people {
            storage.store_person(person).map_err(cortex_err)?;
        }
        imported["people"] = json!(count);
    }

    if let Some(beliefs) = data.beliefs {
        let count = beliefs.len();
        for belief in &beliefs {
            storage.store_belief(belief).map_err(cortex_err)?;
        }
        imported["beliefs"] = json!(count);
    }

    Ok(Json(json!({
        "status": "imported",
        "counts": imported,
    })))
}
