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

    /// Main retrieval: multi-signal ranked search.
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalResult>, CortexError> {
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

        // Batch fetch all candidates in a single query
        let all_ids: Vec<Uuid> = candidate_ids.iter().map(|(id, _)| *id).collect();
        let memories = self.storage.get_memories_batch(&all_ids)?;
        let mem_map: HashMap<Uuid, MemObject> = memories.into_iter().map(|m| (m.id, m)).collect();

        // Score each candidate
        let mut results = Vec::new();
        for (id, sim_score) in &candidate_ids {
            if let Some(mem) = mem_map.get(id) {
                let breakdown = self.compute_scores(mem, *sim_score, query);
                let final_score = self.weights.similarity * breakdown.similarity
                    + self.weights.temporal * breakdown.temporal
                    + self.weights.salience * breakdown.salience
                    + self.weights.social * breakdown.social
                    + self.weights.channel * breakdown.channel;

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

    fn compute_scores(
        &self,
        mem: &MemObject,
        similarity: f32,
        query: &RetrievalQuery,
    ) -> ScoreBreakdown {
        // Temporal: more recent = higher score, exponential decay
        let hours_ago = (query.time_context - mem.temporal.ingestion_time)
            .num_hours()
            .max(0) as f32;
        let temporal = (-hours_ago / 168.0).exp(); // half-life of ~1 week

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
