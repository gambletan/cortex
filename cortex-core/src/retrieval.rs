use chrono::{DateTime, Utc};
use uuid::Uuid;

use std::collections::HashMap;

use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Multi-signal retrieval query.
pub struct RetrievalQuery {
    pub text: String,
    pub embedding: Option<Vec<f32>>,
    pub channel: Option<String>,
    pub person_id: Option<Uuid>,
    pub time_context: DateTime<Utc>,
    pub limit: usize,
}

impl RetrievalQuery {
    pub fn new(text: impl Into<String>, limit: usize) -> Self {
        Self {
            text: text.into(),
            embedding: None,
            channel: None,
            person_id: None,
            time_context: Utc::now(),
            limit,
        }
    }

    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    pub fn with_person(mut self, person_id: Uuid) -> Self {
        self.person_id = Some(person_id);
        self
    }
}

/// Retrieval result with score breakdown.
pub struct RetrievalResult {
    pub memory: MemObject,
    pub score: f32,
    pub score_breakdown: ScoreBreakdown,
}

/// Breakdown of how the final score was computed.
#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    pub similarity: f32,
    pub temporal: f32,
    pub salience: f32,
    pub social: f32,
    pub channel: f32,
}

/// Configurable weights for multi-signal retrieval.
#[derive(Debug, Clone)]
pub struct RetrievalWeights {
    pub similarity: f32,
    pub temporal: f32,
    pub salience: f32,
    pub social: f32,
    pub channel: f32,
}

impl Default for RetrievalWeights {
    fn default() -> Self {
        Self {
            similarity: 0.35,
            temporal: 0.20,
            salience: 0.25,
            social: 0.10,
            channel: 0.10,
        }
    }
}

/// Multi-signal retrieval engine.
pub struct RetrievalEngine<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
    weights: RetrievalWeights,
}

impl<'a> RetrievalEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self {
            storage,
            index,
            weights: RetrievalWeights::default(),
        }
    }

    pub fn with_weights(mut self, weights: RetrievalWeights) -> Self {
        self.weights = weights;
        self
    }

    /// Main retrieval: multi-signal ranked search with multi-hop expansion.
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalResult>, CortexError> {
        // Detect if query has temporal intent (adjusts scoring strategy)
        let temporal_intent = detect_temporal_intent(&query.text);

        // Gather candidates from vector similarity (if embedding provided)
        let mut candidate_ids: Vec<(Uuid, f32)> = Vec::new();

        if let Some(ref embedding) = query.embedding {
            candidate_ids = self.index.search(embedding, query.limit * 5);
        }

        // If no embedding, fall back to recent memories
        if candidate_ids.is_empty() {
            let recent = self
                .storage
                .list_by_tier(MemoryTier::Episodic, query.limit * 3)?;
            let semantic = self
                .storage
                .list_by_tier(MemoryTier::Semantic, query.limit * 3)?;

            for mem in recent.iter().chain(semantic.iter()) {
                candidate_ids.push((mem.id, 0.5)); // neutral similarity
            }
        }

        // For temporal queries, also pull in time-ordered candidates
        if temporal_intent != TemporalIntent::None {
            let time_ordered = self.get_temporal_candidates(&temporal_intent, query.limit * 3)?;
            for mem in time_ordered {
                if !candidate_ids.iter().any(|(id, _)| *id == mem.id) {
                    candidate_ids.push((mem.id, 0.3)); // lower base similarity
                }
            }
        }

        // Multi-hop expansion: extract entities from top candidates and pull related facts
        let hop_ids = self.multi_hop_expand(&candidate_ids, query.limit)?;
        for (id, score) in hop_ids {
            if !candidate_ids.iter().any(|(cid, _)| *cid == id) {
                candidate_ids.push((id, score));
            }
        }

        // Batch fetch all candidates in a single query
        let all_ids: Vec<Uuid> = candidate_ids.iter().map(|(id, _)| *id).collect();
        let memories = self.storage.get_memories_batch(&all_ids)?;
        let mem_map: HashMap<Uuid, MemObject> = memories.into_iter().map(|m| (m.id, m)).collect();

        // Adapt weights for temporal queries
        let weights = match temporal_intent {
            TemporalIntent::None => self.weights.clone(),
            _ => RetrievalWeights {
                similarity: self.weights.similarity * 0.6,
                temporal: self.weights.temporal * 2.5,
                salience: self.weights.salience * 0.8,
                social: self.weights.social,
                channel: self.weights.channel,
            },
        };

        // Score each candidate
        let mut results = Vec::new();
        for (id, sim_score) in &candidate_ids {
            if let Some(mem) = mem_map.get(id) {
                let breakdown = self.compute_scores(mem, *sim_score, query, &temporal_intent);
                let final_score = weights.similarity * breakdown.similarity
                    + weights.temporal * breakdown.temporal
                    + weights.salience * breakdown.salience
                    + weights.social * breakdown.social
                    + weights.channel * breakdown.channel;

                results.push(RetrievalResult {
                    memory: mem.clone(),
                    score: final_score,
                    score_breakdown: breakdown,
                });
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(query.limit);

        Ok(results)
    }

    /// Multi-hop expansion: look at top initial candidates, extract entities
    /// (subjects/objects from facts), and pull in related semantic memories.
    fn multi_hop_expand(
        &self,
        candidates: &[(Uuid, f32)],
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, CortexError> {
        // Only expand from top-5 candidates to keep it fast
        let top_n = candidates.len().min(5);
        let top_ids: Vec<Uuid> = candidates[..top_n].iter().map(|(id, _)| *id).collect();
        let top_mems = self.storage.get_memories_batch(&top_ids)?;

        // Extract entity names from facts in top candidates
        let mut entities: Vec<String> = Vec::new();
        for mem in &top_mems {
            match &mem.content {
                MemContent::Fact { subject, object, .. } => {
                    if subject != "User" && !entities.contains(subject) {
                        entities.push(subject.clone());
                    }
                    if !entities.contains(object) {
                        entities.push(object.clone());
                    }
                }
                MemContent::Relationship { person_a, person_b, .. } => {
                    // person_a/person_b are UUIDs, skip for now
                    let _ = (person_a, person_b);
                }
                _ => {}
            }
        }

        // Query semantic store for facts mentioning these entities
        let mut expanded: Vec<(Uuid, f32)> = Vec::new();
        let semantic_mems = self.storage.list_by_tier(MemoryTier::Semantic, limit * 3)?;

        for mem in &semantic_mems {
            if let MemContent::Fact { subject, object, .. } = &mem.content {
                for entity in &entities {
                    let entity_lower = entity.to_lowercase();
                    if subject.to_lowercase().contains(&entity_lower)
                        || object.to_lowercase().contains(&entity_lower)
                    {
                        expanded.push((mem.id, 0.25)); // lower score for hop-2 results
                        break;
                    }
                }
            }
        }

        Ok(expanded)
    }

    /// Get time-ordered candidates for temporal queries.
    fn get_temporal_candidates(
        &self,
        intent: &TemporalIntent,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        match intent {
            TemporalIntent::Recent => {
                self.storage
                    .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit)
            }
            TemporalIntent::First | TemporalIntent::Earliest => {
                // Get all and reverse (oldest first)
                let mut mems = self
                    .storage
                    .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit)?;
                mems.reverse();
                Ok(mems)
            }
            TemporalIntent::Sequence => {
                // For sequence queries, get more candidates to capture the full timeline
                self.storage
                    .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit * 2)
            }
            TemporalIntent::None => Ok(Vec::new()),
        }
    }

    fn compute_scores(
        &self,
        mem: &MemObject,
        similarity: f32,
        query: &RetrievalQuery,
        temporal_intent: &TemporalIntent,
    ) -> ScoreBreakdown {
        // Temporal scoring — enhanced with intent awareness
        let hours_ago = (query.time_context - mem.temporal.ingestion_time)
            .num_hours()
            .max(0) as f32;

        let temporal = match temporal_intent {
            TemporalIntent::Recent => {
                // Strongly prefer recent memories
                (-hours_ago / 48.0).exp() // 2-day half-life
            }
            TemporalIntent::First | TemporalIntent::Earliest => {
                // Prefer older memories
                let max_hours = 24.0 * 365.0; // normalize to ~1 year
                (hours_ago / max_hours).min(1.0)
            }
            TemporalIntent::Sequence => {
                // Flat temporal score — don't bias by time, let similarity decide
                // But boost memories that have event_time set (they're more temporally grounded)
                if mem.temporal.event_time.is_some() {
                    0.7
                } else {
                    0.4
                }
            }
            TemporalIntent::None => {
                // Default: exponential decay with 1-week half-life
                (-hours_ago / 168.0).exp()
            }
        };

        // Salience: use effective score directly
        let salience = mem.salience.effective_score;

        // Social: boost if memory is about the queried person
        let social = if let Some(person_id) = query.person_id {
            match &mem.source.identity_id {
                Some(id) if *id == person_id => 1.0,
                _ => match &mem.content {
                    MemContent::Relationship { person_a, person_b, .. }
                        if *person_a == person_id || *person_b == person_id =>
                    {
                        1.0
                    }
                    _ => 0.0,
                },
            }
        } else {
            0.0
        };

        // Channel: boost if memory is from the same channel
        let channel = if let Some(ref ch) = query.channel {
            if mem.source.channel == *ch {
                1.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        ScoreBreakdown {
            similarity,
            temporal,
            salience,
            social,
            channel,
        }
    }
}

// ── Temporal intent detection ────────────────────────────────────────────────

/// Detected temporal intent in a query.
#[derive(Debug, Clone, PartialEq)]
pub enum TemporalIntent {
    None,
    Recent,
    First,
    Earliest,
    Sequence,
}

/// Detect temporal intent from query text.
/// Supports English and Chinese temporal keywords.
fn detect_temporal_intent(query: &str) -> TemporalIntent {
    let lower = query.to_lowercase();

    // Recent intent
    let recent_en = [
        "last time", "recently", "latest", "most recent", "just now",
        "yesterday", "today", "this week", "last week", "this month",
        "last month", "the other day",
    ];
    let recent_zh = [
        "最近", "上次", "昨天", "今天", "刚才", "这周", "上周",
        "这个月", "上个月", "前几天", "最新",
    ];

    for pat in &recent_en {
        if lower.contains(pat) {
            return TemporalIntent::Recent;
        }
    }
    for pat in &recent_zh {
        if query.contains(pat) {
            return TemporalIntent::Recent;
        }
    }

    // First/earliest intent
    let first_en = [
        "first time", "the first", "originally", "initially", "at first",
        "when did", "earliest", "began", "started",
    ];
    let first_zh = [
        "第一次", "最初", "一开始", "起初", "最早", "什么时候开始",
    ];

    for pat in &first_en {
        if lower.contains(pat) {
            return TemporalIntent::First;
        }
    }
    for pat in &first_zh {
        if query.contains(pat) {
            return TemporalIntent::First;
        }
    }

    // Sequence intent (asking about order of events)
    let seq_en = [
        "before", "after", "then", "followed by", "in order",
        "timeline", "sequence", "chronolog", "when was", "what happened",
        "how many times", "how often",
    ];
    let seq_zh = [
        "之前", "之后", "然后", "顺序", "时间线", "多少次",
        "什么时候", "先后",
    ];

    for pat in &seq_en {
        if lower.contains(pat) {
            return TemporalIntent::Sequence;
        }
    }
    for pat in &seq_zh {
        if query.contains(pat) {
            return TemporalIntent::Sequence;
        }
    }

    TemporalIntent::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporal_recent() {
        assert_eq!(
            detect_temporal_intent("What did I do last time?"),
            TemporalIntent::Recent
        );
        assert_eq!(
            detect_temporal_intent("最近聊了什么"),
            TemporalIntent::Recent
        );
    }

    #[test]
    fn test_temporal_first() {
        assert_eq!(
            detect_temporal_intent("When did I first mention this?"),
            TemporalIntent::First
        );
        assert_eq!(
            detect_temporal_intent("第一次讨论是什么时候"),
            TemporalIntent::First
        );
    }

    #[test]
    fn test_temporal_sequence() {
        assert_eq!(
            detect_temporal_intent("What happened after the meeting?"),
            TemporalIntent::Sequence
        );
        assert_eq!(
            detect_temporal_intent("会议之后发生了什么"),
            TemporalIntent::Sequence
        );
    }

    #[test]
    fn test_no_temporal() {
        assert_eq!(
            detect_temporal_intent("Tell me about Alice"),
            TemporalIntent::None
        );
    }
}
