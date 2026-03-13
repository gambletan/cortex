use crate::types::{LinkRelation, MemContent, MemObject, MemoryTier};
use chrono::{DateTime, Utc};
use uuid::Uuid;

/// Pluggable storage backend trait.
pub trait StorageBackend: Send + Sync {
    fn init(&self) -> Result<(), crate::CortexError>;

    // Memory CRUD
    fn store_memory(&self, mem: &MemObject) -> Result<(), crate::CortexError>;
    fn get_memory(&self, id: Uuid) -> Result<Option<MemObject>, crate::CortexError>;
    fn update_memory(&self, mem: &MemObject) -> Result<(), crate::CortexError>;
    fn delete_memory(&self, id: Uuid) -> Result<(), crate::CortexError>;

    // Queries
    fn list_by_tier(
        &self,
        tier: MemoryTier,
        limit: usize,
    ) -> Result<Vec<MemObject>, crate::CortexError>;
    fn list_by_tier_ordered_by_ingestion(
        &self,
        tier: MemoryTier,
        limit: usize,
    ) -> Result<Vec<MemObject>, crate::CortexError>;
    fn list_by_channel(
        &self,
        channel: &str,
        limit: usize,
    ) -> Result<Vec<MemObject>, crate::CortexError>;
    fn list_by_salience_below(
        &self,
        tier: MemoryTier,
        threshold: f32,
    ) -> Result<Vec<MemObject>, crate::CortexError>;
    fn list_in_time_range(
        &self,
        tier: MemoryTier,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<MemObject>, crate::CortexError>;

    // Links
    fn store_link(
        &self,
        source_id: Uuid,
        target_id: Uuid,
        relation: LinkRelation,
        strength: f32,
    ) -> Result<(), crate::CortexError>;
    fn get_links(&self, source_id: Uuid)
        -> Result<Vec<(Uuid, LinkRelation, f32)>, crate::CortexError>;

    // People
    fn store_person(&self, person: &crate::people::Person) -> Result<(), crate::CortexError>;
    fn get_person(&self, id: Uuid) -> Result<Option<crate::people::Person>, crate::CortexError>;
    fn find_person_by_channel_identity(
        &self,
        channel: &str,
        channel_user_id: &str,
    ) -> Result<Option<crate::people::Person>, crate::CortexError>;
    fn update_person(&self, person: &crate::people::Person) -> Result<(), crate::CortexError>;
    fn delete_person(&self, id: Uuid) -> Result<(), crate::CortexError>;
    fn list_people(&self) -> Result<Vec<crate::people::Person>, crate::CortexError>;

    // Beliefs
    fn store_belief(&self, belief: &crate::belief::Belief) -> Result<(), crate::CortexError>;
    fn get_belief(&self, key: &str) -> Result<Option<crate::belief::Belief>, crate::CortexError>;
    fn update_belief(&self, belief: &crate::belief::Belief) -> Result<(), crate::CortexError>;
    fn list_beliefs_above(
        &self,
        threshold: f32,
    ) -> Result<Vec<crate::belief::Belief>, crate::CortexError>;

    // Patterns
    fn store_pattern(&self, pattern: &crate::procedural::Pattern)
        -> Result<(), crate::CortexError>;
    fn get_pattern(
        &self,
        trigger: &str,
    ) -> Result<Option<crate::procedural::Pattern>, crate::CortexError>;
    fn update_pattern(
        &self,
        pattern: &crate::procedural::Pattern,
    ) -> Result<(), crate::CortexError>;
    fn list_patterns(
        &self,
        min_frequency: u32,
    ) -> Result<Vec<crate::procedural::Pattern>, crate::CortexError>;

    // Batch fetch
    fn get_memories_batch(&self, ids: &[Uuid]) -> Result<Vec<MemObject>, crate::CortexError> {
        // Default implementation: N individual queries (override for batch SQL)
        let mut results = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(mem) = self.get_memory(*id)? {
                results.push(mem);
            }
        }
        Ok(results)
    }

    // Entity-indexed queries (optimized for semantic lookups)
    /// Query semantic facts where subject or object contains the entity string.
    /// Uses SQL-level filtering instead of loading all semantic memories.
    fn query_facts_by_entity(&self, entity: &str) -> Result<Vec<MemObject>, crate::CortexError> {
        // Default: fall back to full scan (overridden by SQLite for indexed query)
        let all = self.list_by_tier(MemoryTier::Semantic, 10_000)?;
        let entity_lower = entity.to_lowercase();
        Ok(all.into_iter().filter(|m| {
            match &m.content {
                MemContent::Fact { subject, object, .. } => {
                    subject.to_lowercase().contains(&entity_lower)
                        || object.to_lowercase().contains(&entity_lower)
                }
                _ => false,
            }
        }).collect())
    }

    /// Query semantic preferences where key contains the pattern string.
    fn query_preferences_by_key(&self, key_pattern: &str) -> Result<Vec<MemObject>, crate::CortexError> {
        let all = self.list_by_tier(MemoryTier::Semantic, 10_000)?;
        let pattern_lower = key_pattern.to_lowercase();
        Ok(all.into_iter().filter(|m| {
            match &m.content {
                MemContent::Preference { key, .. } => {
                    key.to_lowercase().contains(&pattern_lower)
                }
                _ => false,
            }
        }).collect())
    }

    // Batch store (transactional)
    fn store_memories_batch(&self, mems: &[MemObject]) -> Result<usize, crate::CortexError> {
        // Default: N individual inserts (override for transactional batch)
        for mem in mems {
            self.store_memory(mem)?;
        }
        Ok(mems.len())
    }

    // Deduplication
    fn find_by_content_hash(&self, hash: &str) -> Result<Option<MemObject>, crate::CortexError>;

    // Archival
    fn archive_memory(&self, id: Uuid) -> Result<(), crate::CortexError> {
        if let Some(mut mem) = self.get_memory(id)? {
            mem.tier = crate::types::MemoryTier::Archived;
            self.update_memory(&mem)?;
        }
        Ok(())
    }

    fn restore_memory(&self, id: Uuid, target_tier: MemoryTier) -> Result<(), crate::CortexError> {
        if let Some(mut mem) = self.get_memory(id)? {
            mem.tier = target_tier;
            self.update_memory(&mem)?;
        }
        Ok(())
    }

    // Namespace-filtered queries
    fn list_by_tier_and_namespace(
        &self,
        tier: MemoryTier,
        namespace: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemObject>, crate::CortexError> {
        // Default: filter in memory (override for SQL-level filtering)
        let all = self.list_by_tier(tier, limit)?;
        Ok(match namespace {
            Some(ns) => all.into_iter().filter(|m| m.namespace.as_deref() == Some(ns)).collect(),
            None => all,
        })
    }

    // Bulk operations
    fn count_by_tier(&self, tier: MemoryTier) -> Result<usize, crate::CortexError>;
    fn list_memories_by_source_identity(
        &self,
        identity_id: Uuid,
    ) -> Result<Vec<MemObject>, crate::CortexError>;
}
