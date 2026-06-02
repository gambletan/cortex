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

// ── Input bounds (parity with the MCP tool layer) ────────────────────────────
// Clamp client-supplied sizes so a malformed/hostile request can't exhaust memory,
// and floor the compression window so a negative value can't invert its meaning.
const MAX_SEARCH_LIMIT: usize = 100;
const MAX_CONTEXT_TOKENS: usize = 8_000;

fn search_limit(req: Option<usize>) -> usize {
    req.unwrap_or(10).min(MAX_SEARCH_LIMIT)
}
fn context_tokens(req: Option<usize>) -> usize {
    req.unwrap_or(2000).min(MAX_CONTEXT_TOKENS)
}
fn compress_min_messages(req: Option<usize>) -> usize {
    req.unwrap_or(5).max(1)
}
fn compress_max_age_days(req: Option<i64>) -> i64 {
    // max(1): a negative window would set the cutoff in the future and compress
    // fresh memories instead of old ones.
    req.unwrap_or(7).max(1)
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

// ── Stats ────────────────────────────────────────────────────────────────────

pub async fn stats(State(state): State<Arc<AppState>>) -> AppResult {
    let s = state.cortex.stats().map_err(cortex_err)?;

    Ok(Json(json!({
        "total": s.total,
        "episodic": s.episodic,
        "semantic": s.semantic,
        "procedural": s.procedural,
        "archived": s.archived,
        "people": s.people,
        "beliefs": s.beliefs,
        "index_size": s.index_size,
    })))
}

// ── Recent Memories ──────────────────────────────────────────────────────────

pub async fn recent_memories(State(state): State<Arc<AppState>>) -> AppResult {
    use cortex_core::types::MemoryTier;
    // Fetch recent memories from all active tiers with enough headroom
    let mut mems = Vec::new();
    for tier in &[MemoryTier::Episodic, MemoryTier::Semantic, MemoryTier::Procedural] {
        let tier_mems = state.cortex.storage()
            .list_by_tier_ordered_by_ingestion(*tier, 20)
            .map_err(cortex_err)?;
        mems.extend(tier_mems);
    }
    // Sort by ingestion time descending, take top 20
    mems.sort_by_key(|m| std::cmp::Reverse(m.temporal.ingestion_time));
    mems.truncate(20);

    let results: Vec<serde_json::Value> = mems.iter().map(|m| {
        let text = match &m.content {
            cortex_core::types::MemContent::Text(t) => t.clone(),
            cortex_core::types::MemContent::Fact { subject, predicate, object } => format!("{} {} {}", subject, predicate, object),
            cortex_core::types::MemContent::Preference { key, value, .. } => format!("{} = {}", key, value),
            other => format!("{:?}", other),
        };
        json!({
            "id": m.id.to_string(),
            "text": text,
            "tier": m.tier.as_str(),
            "channel": m.source.channel,
            "created_at": m.temporal.ingestion_time.to_rfc3339(),
            "score": m.salience.effective_score,
        })
    }).collect();

    Ok(Json(json!({ "results": results })))
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
            search_limit(req.limit),
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
            context_tokens(q.max_tokens),
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

pub async fn list_people(State(state): State<Arc<AppState>>) -> AppResult {
    let people = state.cortex.list_people().map_err(cortex_err)?;

    let items: Vec<Value> = people
        .iter()
        .map(|p| {
            json!({
                "id": p.id.to_string(),
                "name": p.display_name,
                "identities": p.identities.iter().map(|i| {
                    json!({ "channel": i.channel, "user_id": i.channel_user_id })
                }).collect::<Vec<_>>(),
            })
        })
        .collect();

    Ok(Json(json!({ "people": items, "total": items.len() })))
}

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
    let data = cortex_core::export::export_all(state.cortex.storage()).map_err(cortex_err)?;
    Ok(Json(json!({
        "version": data.version,
        "exported_at": data.exported_at,
        "memories": data.memories,
        "people": data.people,
        "beliefs": data.beliefs,
        "patterns": data.patterns,
        "stats": {
            "memories": data.memories.len(),
            "people": data.people.len(),
            "beliefs": data.beliefs.len(),
            "patterns": data.patterns.len(),
        }
    })))
}

pub async fn import_all(
    State(state): State<Arc<AppState>>,
    Json(data): Json<cortex_core::export::ImportData>,
) -> AppResult {
    let report = cortex_core::export::import_all(
        state.cortex.storage(),
        state.cortex.index(),
        data,
    ).map_err(cortex_err)?;

    Ok(Json(json!({
        "status": "imported",
        "counts": {
            "memories": report.memories,
            "people": report.people,
            "beliefs": report.beliefs,
        },
    })))
}

// ── Compression ──────────────────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct CompressRequest {
    pub min_messages: Option<usize>,
    pub max_age_days: Option<i64>,
}

pub async fn compress(
    State(state): State<Arc<AppState>>,
    Json(req): Json<CompressRequest>,
) -> AppResult {
    let report = state
        .cortex
        .run_compression(
            compress_min_messages(req.min_messages),
            compress_max_age_days(req.max_age_days),
        )
        .map_err(cortex_err)?;

    Ok(Json(json!({
        "sessions_compressed": report.sessions_compressed,
        "episodes_consumed": report.episodes_consumed,
        "summaries_created": report.summaries_created,
    })))
}

// ── Quick Note (mobile capture) ─────────────────────────────────────────────

#[derive(Deserialize)]
pub struct QuickNoteRequest {
    pub text: String,
    #[serde(default = "default_quick_note_channel")]
    pub channel: String,
    /// Optional scope — honor per-user/namespace isolation like /v1/memories does.
    pub user_id: Option<String>,
}

fn default_quick_note_channel() -> String {
    "quick-note".to_string()
}

pub async fn quick_note(
    State(state): State<Arc<AppState>>,
    Json(req): Json<QuickNoteRequest>,
) -> AppResult {
    if req.text.trim().is_empty() {
        return Err((
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "text is required"})),
        ));
    }
    let mem = state
        .cortex
        .ingest(&req.text, &req.channel, req.user_id.as_deref(), None, None)
        .map_err(cortex_err)?;
    Ok(Json(json!({
        "id": mem.id.to_string(),
        "status": "remembered",
        "tier": mem.tier.as_str(),
    })))
}

// ── Relationship extraction ──────────────────────────────────────────────────

#[derive(Deserialize)]
pub struct RelationshipRequest {
    pub text: String,
}

pub async fn extract_relationships(
    State(state): State<Arc<AppState>>,
    Json(req): Json<RelationshipRequest>,
) -> AppResult {
    let rels = state.cortex.extract_relationships(&req.text);

    let items: Vec<Value> = rels
        .iter()
        .map(|r| {
            json!({
                "person_a": r.person_a,
                "person_b": r.person_b,
                "relation": r.relation,
                "confidence": r.confidence,
            })
        })
        .collect();

    Ok(Json(json!({
        "relationships": items,
        "total": items.len(),
    })))
}

#[cfg(test)]
mod input_bounds_tests {
    use super::*;

    #[test]
    fn search_limit_defaults_and_clamps() {
        assert_eq!(search_limit(None), 10);
        assert_eq!(search_limit(Some(25)), 25);
        assert_eq!(search_limit(Some(usize::MAX)), MAX_SEARCH_LIMIT);
    }

    #[test]
    fn context_tokens_defaults_and_clamps() {
        assert_eq!(context_tokens(None), 2000);
        assert_eq!(context_tokens(Some(500)), 500);
        assert_eq!(context_tokens(Some(usize::MAX)), MAX_CONTEXT_TOKENS);
    }

    #[test]
    fn compress_args_floor_at_one() {
        assert_eq!(compress_max_age_days(None), 7);
        assert_eq!(compress_max_age_days(Some(30)), 30);
        // A negative window must not invert into the future.
        assert_eq!(compress_max_age_days(Some(-365)), 1);
        assert_eq!(compress_min_messages(Some(0)), 1);
    }
}
