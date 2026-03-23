use uuid::Uuid;

use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Semantic store — consolidated knowledge graph of permanent facts and preferences.
pub struct SemanticStore<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
}

impl<'a> SemanticStore<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self { storage, index }
    }

    /// Build a fact MemObject without storing it (for batch inserts).
    pub fn build_fact_mem(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        confidence: f32,
        source: MemSource,
        embedding: Option<Vec<f32>>,
    ) -> MemObject {
        let content = MemContent::Fact {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
        };
        let mut builder = MemObjectBuilder::new(MemoryTier::Semantic, content, source)
            .salience(Salience::new(confidence));
        if let Some(emb) = embedding {
            builder = builder.embedding(emb);
        }
        builder.build()
    }

    /// Build a preference MemObject without storing it (for batch inserts).
    pub fn build_preference_mem(&self, key: &str, value: &str, confidence: f32) -> MemObject {
        let content = MemContent::Preference {
            key: key.to_string(),
            value: value.to_string(),
            confidence,
        };
        MemObjectBuilder::new(MemoryTier::Semantic, content, MemSource::new("system"))
            .salience(Salience::new(confidence))
            .build()
    }

    /// Add a semantic fact (subject-predicate-object triple).
    pub fn add_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        confidence: f32,
        source: MemSource,
        embedding: Option<Vec<f32>>,
    ) -> Result<MemObject, CortexError> {
        let content = MemContent::Fact {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
        };

        let mut builder = MemObjectBuilder::new(MemoryTier::Semantic, content, source)
            .salience(Salience::new(confidence));

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

    /// Add a user preference.
    pub fn add_preference(
        &self,
        key: &str,
        value: &str,
        confidence: f32,
    ) -> Result<MemObject, CortexError> {
        let content = MemContent::Preference {
            key: key.to_string(),
            value: value.to_string(),
            confidence,
        };

        let mem = MemObjectBuilder::new(
            MemoryTier::Semantic,
            content,
            MemSource::new("system"),
        )
        .salience(Salience::new(confidence))
        .build();

        self.storage.store_memory(&mem)?;
        Ok(mem)
    }

    /// Query facts about a subject or object entity.
    /// Uses SQL-level filtering when available (no full table scan).
    pub fn query_facts(&self, entity: &str) -> Result<Vec<MemObject>, CortexError> {
        if entity.is_empty() {
            // Empty entity = return all facts (for context generation)
            let all = self.storage.list_by_tier(MemoryTier::Semantic, 10_000)?;
            return Ok(all.into_iter().filter(|m| matches!(&m.content, MemContent::Fact { .. })).collect());
        }
        self.storage.query_facts_by_entity(entity)
    }

    /// Query preferences matching a key pattern (substring match).
    /// Uses SQL-level filtering when available.
    pub fn query_preferences(&self, key_pattern: &str) -> Result<Vec<MemObject>, CortexError> {
        if key_pattern.is_empty() {
            // Empty pattern = return all preferences
            let all = self.storage.list_by_tier(MemoryTier::Semantic, 10_000)?;
            return Ok(all.into_iter().filter(|m| matches!(&m.content, MemContent::Preference { .. })).collect());
        }
        self.storage.query_preferences_by_key(key_pattern)
    }

    /// Update the confidence of a semantic memory.
    pub fn update_confidence(&self, id: Uuid, new_confidence: f32) -> Result<(), CortexError> {
        if let Some(mut mem) = self.storage.get_memory(id)? {
            if let MemContent::Preference {
                ref mut confidence, ..
            } = &mut mem.content {
                *confidence = new_confidence;
            }
            mem.salience.base_score = new_confidence;
            mem.salience.recompute();
            self.storage.update_memory(&mem)?;
        }
        Ok(())
    }

    /// Find facts that contradict a given fact.
    /// Returns (conflicting_memory, conflict_score) pairs.
    pub fn find_contradictions(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<Vec<(MemObject, f32)>, CortexError> {
        let facts = self.query_facts(subject)?;
        let mut contradictions = Vec::new();

        for fact in facts {
            if let MemContent::Fact {
                subject: ref s,
                predicate: ref p,
                object: ref o,
            } = fact.content
            {
                // Same subject+predicate but different object = potential contradiction
                if s.to_lowercase() == subject.to_lowercase()
                    && p.to_lowercase() == predicate.to_lowercase()
                    && o.to_lowercase() != object.to_lowercase()
                {
                    let conflict_score = fact.salience.effective_score;
                    contradictions.push((fact, conflict_score));
                }
            }
        }
        Ok(contradictions)
    }

    /// Merge two facts by creating a Supersedes link from new to old.
    pub fn merge_facts(&self, old_id: Uuid, new_id: Uuid) -> Result<(), CortexError> {
        self.storage.store_link(
            new_id,
            old_id,
            LinkRelation::Supersedes,
            1.0,
        )?;

        // Lower the salience of the old fact
        if let Some(mut old_mem) = self.storage.get_memory(old_id)? {
            old_mem.salience.base_score *= 0.3;
            old_mem.salience.recompute();
            self.storage.update_memory(&old_mem)?;
        }

        Ok(())
    }

    /// Semantic search using vector similarity.
    pub fn search_similar(
        &self,
        query_embedding: &[f32],
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        let candidates = self.index.search(query_embedding, limit * 2);

        // Batch fetch all candidate memories in a single query
        let all_ids: Vec<Uuid> = candidates.iter().map(|(id, _)| *id).collect();
        let memories = self.storage.get_memories_batch(&all_ids)?;
        let mem_map: std::collections::HashMap<Uuid, MemObject> =
            memories.into_iter().map(|m| (m.id, m)).collect();

        let mut results = Vec::new();
        for (id, _score) in &candidates {
            if let Some(mem) = mem_map.get(id) {
                if mem.tier == MemoryTier::Semantic {
                    results.push(mem.clone());
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(results)
    }
}
