use chrono::Duration;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use crate::episode::{DecayConfig, EpisodeStore};
use crate::procedural::{Pattern, ProceduralStore};
use crate::storage::memory_index::{cosine_similarity, MemoryIndex};
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Minimum cosine similarity for embedding-based promotion grouping.
const SEMANTIC_SIMILARITY_THRESHOLD: f32 = 0.85;

/// Report from a consolidation cycle.
#[derive(Debug, Default)]
pub struct ConsolidationReport {
    pub episodes_scanned: usize,
    pub promoted_to_semantic: usize,
    pub decayed_updated: usize,
    /// Count of memories archived (moved to cold storage, not deleted).
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

    /// Find episodic memories that repeat, using two strategies:
    /// 1. Exact content hash matching (fast, strict)
    /// 2. Embedding-based semantic similarity for ungrouped memories (cosine >= 0.85)
    ///
    /// Processes in pages to avoid loading all episodic memories at once.
    fn find_promotion_candidates(
        &self,
        min_occurrences: usize,
    ) -> Result<Vec<Vec<Uuid>>, CortexError> {
        let mut fact_groups: HashMap<String, Vec<Uuid>> = HashMap::new();
        // Collect ungrouped memories with embeddings for the semantic pass.
        // Store (id, embedding) — clone the Arc's inner vec only for memories
        // that didn't match by hash, keeping the hot path allocation-free.
        let mut ungrouped_with_embeddings: Vec<(Uuid, Vec<f32>)> = Vec::new();
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
                } else if let Some(ref emb) = mem.embedding {
                    // No hash key (e.g. Relationship, Pattern, Event) but has embedding
                    ungrouped_with_embeddings.push((mem.id, emb.as_ref().clone()));
                }
            }

            if page_len < page_size {
                break;
            }
            offset += page_size;
        }

        // Collect IDs that were already grouped by hash so we can skip them
        // in the semantic pass.
        let hash_grouped_ids: HashSet<Uuid> = fact_groups
            .values()
            .flat_map(|ids| ids.iter().copied())
            .collect();

        // Also add memories that had a hash key but whose group is too small
        // (they won't be promoted by hash, so give them a chance via embedding).
        // We need to re-scan fact_groups to find singleton/small groups with embeddings.
        // To avoid a second storage round-trip, collect their IDs now and fetch
        // embeddings only for those that need it.
        let mut extra_candidates: Vec<Uuid> = Vec::new();
        for ids in fact_groups.values() {
            if ids.len() < min_occurrences {
                extra_candidates.extend(ids);
            }
        }
        for id in &extra_candidates {
            // Skip if already in the ungrouped list
            if ungrouped_with_embeddings.iter().any(|(uid, _)| uid == id) {
                continue;
            }
            if let Some(mem) = self.storage.get_memory(*id)? {
                if let Some(ref emb) = mem.embedding {
                    ungrouped_with_embeddings.push((mem.id, emb.as_ref().clone()));
                }
            }
        }

        // --- Pass 1: collect hash-based groups that meet the threshold ---
        let mut results: Vec<Vec<Uuid>> = fact_groups
            .into_values()
            .filter(|ids| ids.len() >= min_occurrences)
            .collect();

        // --- Pass 2: greedy single-pass embedding clustering for ungrouped memories ---
        // Remove any memory that already belongs to a promoted hash group.
        ungrouped_with_embeddings.retain(|(id, _)| !hash_grouped_ids.contains(id));

        if ungrouped_with_embeddings.len() >= min_occurrences {
            let semantic_groups = self.cluster_by_embedding(&ungrouped_with_embeddings);
            for group in semantic_groups {
                if group.len() >= min_occurrences {
                    results.push(group);
                }
            }
        }

        Ok(results)
    }

    /// Greedy single-pass clustering of memories by embedding similarity.
    /// Each unassigned memory is compared to existing cluster centroids (the
    /// first member's embedding). If similar enough, it joins; otherwise it
    /// starts a new cluster.
    fn cluster_by_embedding(&self, items: &[(Uuid, Vec<f32>)]) -> Vec<Vec<Uuid>> {
        // Each cluster is represented by the embedding of its first member
        // (used as the centroid) and the list of member IDs.
        let mut clusters: Vec<(Vec<Uuid>, &[f32])> = Vec::new();

        for (id, embedding) in items {
            let mut assigned = false;
            for (members, centroid) in &mut clusters {
                if cosine_similarity(embedding, centroid) >= SEMANTIC_SIMILARITY_THRESHOLD {
                    members.push(*id);
                    assigned = true;
                    break;
                }
            }
            if !assigned {
                clusters.push((vec![*id], embedding.as_slice()));
            }
        }

        clusters.into_iter().map(|(ids, _)| ids).collect()
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
        let mut builder = MemObjectBuilder::new(MemoryTier::Semantic, first.content.clone(), first.source.clone())
            .salience(salience)
            .tags(first.tags.clone());
        if let Some(ref ns) = first.namespace {
            builder = builder.namespace(ns.clone());
        }
        let mem = builder.build();

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

    /// Archive episodic memories below salience threshold.
    /// Returns count of archived memories.
    pub fn sweep_decayed(&self, threshold: f32) -> Result<usize, CortexError> {
        let decayed = self
            .storage
            .list_by_salience_below(MemoryTier::Episodic, threshold)?;
        if decayed.is_empty() {
            return Ok(0);
        }
        let ids_to_sweep: Vec<uuid::Uuid> = decayed.iter().map(|m| m.id).collect();
        // Archive instead of delete - preserve memories in cold storage
        let mut archived_count = 0;
        for id in &ids_to_sweep {
            if self.storage.archive_memory(*id).is_ok() {
                archived_count += 1;
            }
        }
        Ok(archived_count)
    }
}
