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
    pub fn query_facts(&self, entity: &str) -> Result<Vec<MemObject>, CortexError> {
        let all = self
            .storage
            .list_by_tier(MemoryTier::Semantic, 10_000)?;

        Ok(all
            .into_iter()
            .filter(|m| match &m.content {
                MemContent::Fact {
                    subject, object, ..
                } => {
                    subject.to_lowercase().contains(&entity.to_lowercase())
                        || object.to_lowercase().contains(&entity.to_lowercase())
                }
                _ => false,
            })
            .collect())
    }

    /// Query preferences matching a key pattern (substring match).
    pub fn query_preferences(&self, key_pattern: &str) -> Result<Vec<MemObject>, CortexError> {
        let all = self
            .storage
            .list_by_tier(MemoryTier::Semantic, 10_000)?;

        Ok(all
            .into_iter()
            .filter(|m| match &m.content {
                MemContent::Preference { key, .. } => {
                    key.to_lowercase().contains(&key_pattern.to_lowercase())
                }
                _ => false,
            })
            .collect())
    }

    /// Update the confidence of a semantic memory.
    pub fn update_confidence(&self, id: Uuid, new_confidence: f32) -> Result<(), CortexError> {
        if let Some(mut mem) = self.storage.get_memory(id)? {
            match &mut mem.content {
                MemContent::Preference {
                    ref mut confidence, ..
                } => {
                    *confidence = new_confidence;
                }
                _ => {}
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
        let mut results = Vec::new();
        for (id, _score) in candidates {
            if let Some(mem) = self.storage.get_memory(id)? {
                if mem.tier == MemoryTier::Semantic {
                    results.push(mem);
                    if results.len() >= limit {
                        break;
                    }
                }
            }
        }
        Ok(results)
    }
}
