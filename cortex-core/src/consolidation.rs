use chrono::Duration;
use std::collections::HashMap;
use uuid::Uuid;

use crate::episode::{DecayConfig, EpisodeStore};
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

/// Configurable parameters for consolidation cycles.
#[derive(Debug, Clone)]
pub struct ConsolidationConfig {
    /// Minimum number of occurrences before an episodic memory is promoted to semantic.
    pub min_occurrences: usize,
    /// Salience threshold below which episodic memories are swept (soft-deleted).
    pub sweep_threshold: f32,
    /// Time window (in days) for behavioral pattern extraction.
    pub pattern_window_days: i64,
    /// Multiplier applied to salience when promoting episodic to semantic.
    pub salience_boost: f32,
}

impl Default for ConsolidationConfig {
    fn default() -> Self {
        Self {
            min_occurrences: 3,
            sweep_threshold: 0.05,
            pattern_window_days: 30,
            salience_boost: 1.5,
        }
    }
}

/// Background consolidation engine — like sleep for the brain.
pub struct ConsolidationEngine<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
    decay_config: Option<&'a DecayConfig>,
    config: ConsolidationConfig,
}

impl<'a> ConsolidationEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self { storage, index, decay_config: None, config: ConsolidationConfig::default() }
    }

    /// Set custom consolidation configuration.
    pub fn with_config(mut self, config: ConsolidationConfig) -> Self {
        self.config = config;
        self
    }

    /// Set custom decay configuration (forwarded to EpisodeStore).
    pub fn with_decay_config(mut self, config: Option<&'a DecayConfig>) -> Self {
        self.decay_config = config;
        self
    }

    /// Run a full consolidation cycle:
    /// 1. Apply temporal decay to all episodic memories
    /// 2. Promote repeated episodes to semantic facts
    /// 3. Sweep dead memories below threshold
    /// 4. Extract behavioral patterns
    pub fn run_consolidation_cycle(&self) -> Result<ConsolidationReport, CortexError> {
        let mut report = ConsolidationReport::default();

        // 1. Apply temporal decay first (updates salience scores)
        let mut episodes = EpisodeStore::new(self.storage, self.index);
        if let Some(cfg) = self.decay_config {
            episodes = episodes.with_decay_config(cfg);
        }
        report.decayed_updated = episodes.decay_tick()?;

        // 2. Find repeated episodic facts for promotion
        let promoted = self.find_promotion_candidates(self.config.min_occurrences)?;
        for ids in &promoted {
            self.promote_to_semantic(ids.clone())?;
        }
        report.promoted_to_semantic = promoted.len();

        // 3. Sweep dead memories (after decay, so freshly-decayed ones get swept)
        report.decayed_swept = self.sweep_decayed(self.config.sweep_threshold)?;

        // 4. Pattern extraction
        let patterns = self.extract_patterns(Duration::days(self.config.pattern_window_days))?;
        report.patterns_detected = patterns.len();

        // 5. Count episodes scanned
        report.episodes_scanned = self
            .storage
            .count_by_tier(MemoryTier::Episodic)?;

        Ok(report)
    }

    /// Find episodic memories that repeat (same content observed 3+ times).
    /// Groups by content text similarity (exact match for now).
    /// Processes in pages to avoid loading all episodic memories at once.
    fn find_promotion_candidates(
        &self,
        min_occurrences: usize,
    ) -> Result<Vec<Vec<Uuid>>, CortexError> {
        let mut fact_groups: HashMap<String, Vec<Uuid>> = HashMap::new();
        let page_size = 1000;
        let mut offset = 0;

        loop {
            let episodes = self.storage.list_by_tier_paged(MemoryTier::Episodic, offset, page_size)?;
            if episodes.is_empty() {
                break;
            }
            let page_len = episodes.len();

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

            if page_len < page_size {
                break;
            }
            offset += page_size;
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
        let salience = Salience::new((first.salience.base_score * self.config.salience_boost).min(1.0));
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
