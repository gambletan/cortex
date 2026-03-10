use chrono::Duration;
use std::collections::HashMap;
use uuid::Uuid;

use crate::episode::EpisodeStore;
use crate::procedural::{Pattern, ProceduralStore};
use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Report from a consolidation cycle.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    pub episodes_scanned: usize,
    pub promoted_to_semantic: usize,
    pub decayed_updated: usize,
    pub decayed_swept: usize,
    pub patterns_detected: usize,
    pub contradictions_found: usize,
    pub people_updated: usize,
}

/// Background consolidation engine — like sleep for the brain.
pub struct ConsolidationEngine<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
}

impl<'a> ConsolidationEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self { storage, index }
    }

    /// Run a full consolidation cycle:
    /// 1. Apply temporal decay to all episodic memories
    /// 2. Promote repeated episodes to semantic facts
    /// 3. Sweep dead memories below threshold
    /// 4. Extract behavioral patterns
    pub fn run_consolidation_cycle(&self) -> Result<ConsolidationReport, CortexError> {
        let mut report = ConsolidationReport::default();

        // 1. Apply temporal decay first (updates salience scores)
        let episodes = EpisodeStore::new(self.storage, self.index);
        report.decayed_updated = episodes.decay_tick()?;

        // 2. Find repeated episodic facts for promotion
        let promoted = self.find_promotion_candidates(3)?;
        for ids in &promoted {
            self.promote_to_semantic(ids.clone())?;
        }
        report.promoted_to_semantic = promoted.len();

        // 3. Sweep dead memories (after decay, so freshly-decayed ones get swept)
        report.decayed_swept = self.sweep_decayed(0.05)?;

        // 4. Pattern extraction
        let patterns = self.extract_patterns(Duration::days(30))?;
        report.patterns_detected = patterns.len();

        // 5. Count episodes scanned
        report.episodes_scanned = self
            .storage
            .count_by_tier(MemoryTier::Episodic)?;

        Ok(report)
    }

    /// Find episodic memories that repeat (same content observed 3+ times).
    /// Groups by content text similarity (exact match for now).
    fn find_promotion_candidates(
        &self,
        min_occurrences: usize,
    ) -> Result<Vec<Vec<Uuid>>, CortexError> {
        let episodes = self
            .storage
            .list_by_tier(MemoryTier::Episodic, 10_000)?;

        // Group facts by (subject, predicate)
        let mut fact_groups: HashMap<String, Vec<Uuid>> = HashMap::new();
        for mem in &episodes {
            let key = match &mem.content {
                MemContent::Fact {
                    subject,
                    predicate,
                    object,
                } => Some(format!("{}|{}|{}", subject.to_lowercase(), predicate.to_lowercase(), object.to_lowercase())),
                MemContent::Preference { key, value, .. } => {
                    Some(format!("pref|{}|{}", key.to_lowercase(), value.to_lowercase()))
                }
                MemContent::Text(t) => Some(format!("text|{}", t.to_lowercase())),
                _ => None,
            };
            if let Some(k) = key {
                fact_groups.entry(k).or_default().push(mem.id);
            }
        }

        Ok(fact_groups
            .into_values()
            .filter(|ids| ids.len() >= min_occurrences)
            .collect())
    }

    /// Promote a group of episodic memories to a single semantic memory.
    pub fn promote_to_semantic(&self, episode_ids: Vec<Uuid>) -> Result<MemObject, CortexError> {
        if episode_ids.is_empty() {
            return Err(CortexError::InvalidInput(
                "No episode IDs provided".to_string(),
            ));
        }

        // Use the first episode as the template
        let first = self
            .storage
            .get_memory(episode_ids[0])?
            .ok_or_else(|| CortexError::NotFound(format!("Memory {}", episode_ids[0])))?;

        // Create semantic memory with boosted salience
        let salience = Salience::new((first.salience.base_score * 1.5).min(1.0));
        let mem = MemObjectBuilder::new(MemoryTier::Semantic, first.content.clone(), first.source.clone())
            .salience(salience)
            .tags(first.tags.clone())
            .build();

        self.storage.store_memory(&mem)?;

        // Link the new semantic memory back to episodes
        for eid in &episode_ids {
            self.storage.store_link(
                mem.id,
                *eid,
                LinkRelation::PartOf,
                1.0,
            )?;
        }

        Ok(mem)
    }

    /// Extract behavioral patterns from recent action sequences.
    pub fn extract_patterns(&self, _window: Duration) -> Result<Vec<Pattern>, CortexError> {
        let procedural_store = ProceduralStore::new(self.storage);
        procedural_store.detect_patterns(3)
    }

    /// Soft-delete episodic memories below salience threshold.
    /// Returns count of deleted memories.
    pub fn sweep_decayed(&self, threshold: f32) -> Result<usize, CortexError> {
        let decayed = self
            .storage
            .list_by_salience_below(MemoryTier::Episodic, threshold)?;
        let count = decayed.len();
        for mem in decayed {
            self.storage.delete_memory(mem.id)?;
        }
        Ok(count)
    }
}
