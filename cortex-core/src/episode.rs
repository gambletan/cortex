use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Episode store — raw memory stream with temporal decay.
pub struct EpisodeStore<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
}

impl<'a> EpisodeStore<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self { storage, index }
    }

    /// Store a new episodic memory.
    pub fn add(
        &self,
        content: MemContent,
        source: MemSource,
        salience_hint: Option<f32>,
        embedding: Option<Vec<f32>>,
    ) -> Result<MemObject, CortexError> {
        let salience = Salience::new(salience_hint.unwrap_or(0.5));

        let mut builder = MemObjectBuilder::new(MemoryTier::Episodic, content, source)
            .salience(salience);

        if let Some(emb) = embedding.clone() {
            builder = builder.embedding(emb);
        }

        let mem = builder.build();
        self.storage.store_memory(&mem)?;

        if let Some(emb) = embedding {
            self.index.insert(mem.id, emb);
        }

        Ok(mem)
    }

    /// Retrieve episodic memories by embedding similarity within a time range.
    pub fn query(
        &self,
        query_embedding: &[f32],
        limit: usize,
        time_range: Option<(DateTime<Utc>, DateTime<Utc>)>,
    ) -> Result<Vec<MemObject>, CortexError> {
        let candidates = self.index.search(query_embedding, limit * 3);

        let mut results = Vec::new();
        for (id, _score) in candidates {
            if let Some(mem) = self.storage.get_memory(id)? {
                if mem.tier != MemoryTier::Episodic {
                    continue;
                }
                if let Some((start, end)) = &time_range {
                    if mem.temporal.ingestion_time < *start || mem.temporal.ingestion_time > *end {
                        continue;
                    }
                }
                results.push(mem);
                if results.len() >= limit {
                    break;
                }
            }
        }
        Ok(results)
    }

    /// Get the most recent episodic memories, optionally filtered by channel.
    pub fn get_recent(
        &self,
        limit: usize,
        channel_filter: Option<&str>,
    ) -> Result<Vec<MemObject>, CortexError> {
        if let Some(channel) = channel_filter {
            let mems = self.storage.list_by_channel(channel, limit * 2)?;
            Ok(mems
                .into_iter()
                .filter(|m| m.tier == MemoryTier::Episodic)
                .take(limit)
                .collect())
        } else {
            self.storage
                .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit)
        }
    }

    /// Apply temporal decay to all episodic memories.
    /// Reduces salience based on time since last access.
    pub fn decay_tick(&self) -> Result<usize, CortexError> {
        let mems = self
            .storage
            .list_by_tier(MemoryTier::Episodic, 10_000)?;
        let now = Utc::now();
        let mut updated = 0;

        for mut mem in mems {
            let hours_since_access = (now - mem.temporal.last_accessed)
                .num_hours()
                .max(0) as f32;

            // Exponential decay: halves every 72 hours of non-access
            let decay = (-hours_since_access / 72.0_f32.ln()).exp();
            let new_decay = decay.clamp(0.01, 1.0);

            if (mem.salience.decay_factor - new_decay).abs() > 0.001 {
                mem.salience.decay_factor = new_decay;
                mem.salience.recompute();
                self.storage.update_memory(&mem)?;
                updated += 1;
            }
        }
        Ok(updated)
    }

    /// Get memories below a salience threshold (candidates for deletion or promotion).
    pub fn get_decayed(&self, threshold: f32) -> Result<Vec<MemObject>, CortexError> {
        self.storage
            .list_by_salience_below(MemoryTier::Episodic, threshold)
    }

    /// Mark a memory as accessed (boosts salience).
    pub fn touch(&self, id: Uuid) -> Result<(), CortexError> {
        if let Some(mut mem) = self.storage.get_memory(id)? {
            mem.temporal.last_accessed = Utc::now();
            mem.temporal.access_count += 1;
            // Logarithmic boost: diminishing returns on repeated access
            mem.salience.access_boost = 1.0 + (mem.temporal.access_count as f32).ln() * 0.1;
            mem.salience.recompute();
            self.storage.update_memory(&mem)?;
        }
        Ok(())
    }

    /// Get episodes from a specific time window (e.g., last 24 hours).
    pub fn get_window(&self, duration: Duration) -> Result<Vec<MemObject>, CortexError> {
        let now = Utc::now();
        let start = now - duration;
        self.storage
            .list_in_time_range(MemoryTier::Episodic, start, now)
    }
}
