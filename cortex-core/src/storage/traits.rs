use crate::types::{LinkRelation, MemObject, MemoryTier};
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

    // Bulk operations
    fn count_by_tier(&self, tier: MemoryTier) -> Result<usize, crate::CortexError>;
    fn list_memories_by_source_identity(
        &self,
        identity_id: Uuid,
    ) -> Result<Vec<MemObject>, crate::CortexError>;
}
