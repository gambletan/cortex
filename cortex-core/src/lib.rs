pub mod belief;
pub mod compression;
pub mod consolidation;
pub mod context;
pub mod embedder;
pub mod episode;
pub mod inference;
pub mod people;
pub mod procedural;
pub mod relationship;
pub mod retrieval;
pub mod semantic;
pub mod storage;
pub mod types;
pub mod working;

use std::sync::atomic::{AtomicU64, Ordering};
use uuid::Uuid;

use crate::belief::BeliefEngine;
use crate::compression::{CompressionEngine, CompressionReport};
use crate::consolidation::{ConsolidationEngine, ConsolidationReport};
use crate::context::{ContextConfig, generate_context};
use crate::episode::EpisodeStore;
use crate::people::PeopleGraph;
use crate::retrieval::{RetrievalEngine, RetrievalQuery, RetrievalResult};
use crate::semantic::SemanticStore;
use crate::storage::memory_index::MemoryIndex;
use crate::storage::sqlite::SqliteStorage;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::working::WorkingMemory;

/// Top-level error type for Cortex.
#[derive(Debug, thiserror::Error)]
pub enum CortexError {
    #[error("Storage error: {0}")]
    Storage(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Invalid input: {0}")]
    InvalidInput(String),

    #[error("Serialization error: {0}")]
    Serialization(String),
}

/// Memory statistics (counts per tier).
#[derive(Debug, Clone)]
pub struct MemoryStats {
    pub episodic: usize,
    pub semantic: usize,
    pub procedural: usize,
    pub people: usize,
    pub beliefs: usize,
    pub index_size: usize,
    pub total: usize,
}

/// How often to auto-run consolidation (every N ingests).
const AUTO_CONSOLIDATION_INTERVAL: u64 = 100;

/// Main Cortex instance — the entry point for all memory operations.
pub struct Cortex {
    storage: SqliteStorage,
    index: MemoryIndex,
    working: WorkingMemory,
    ingest_counter: AtomicU64,
    #[cfg(feature = "embeddings")]
    embedder: parking_lot::Mutex<Option<Option<crate::embedder::Embedder>>>,
}

impl Cortex {
    /// Open or create a Cortex database at the given path.
    pub fn open(db_path: &str) -> Result<Self, CortexError> {
        let storage = SqliteStorage::open(db_path)?;
        let index = MemoryIndex::new();

        // Load existing embeddings into memory index
        let all_tiers = [MemoryTier::Episodic, MemoryTier::Semantic, MemoryTier::Procedural];
        for tier in &all_tiers {
            let mems = storage.list_by_tier(*tier, 100_000)?;
            for mem in mems {
                if let Some(ref emb) = mem.embedding {
                    index.insert(mem.id, emb.clone());
                }
            }
        }

        Ok(Self {
            storage,
            index,
            working: WorkingMemory::default(),
            ingest_counter: AtomicU64::new(0),
            #[cfg(feature = "embeddings")]
            embedder: parking_lot::Mutex::new(None), // lazy init on first use
        })
    }

    /// Create an in-memory Cortex (useful for testing).
    pub fn in_memory() -> Result<Self, CortexError> {
        let storage = SqliteStorage::open_in_memory()?;
        Ok(Self {
            storage,
            index: MemoryIndex::new(),
            working: WorkingMemory::default(),
            ingest_counter: AtomicU64::new(0),
            #[cfg(feature = "embeddings")]
            embedder: parking_lot::Mutex::new(None),
        })
    }

    /// Auto-generate embedding if embedder is available and none provided.
    /// Lazily initializes the embedding model on first call.
    #[allow(unused_variables)]
    fn auto_embed(&self, text: &str, embedding: Option<Vec<f32>>) -> Option<Vec<f32>> {
        if embedding.is_some() {
            return embedding;
        }
        #[cfg(feature = "embeddings")]
        {
            let mut guard = self.embedder.lock();
            // None = not yet initialized, Some(None) = init failed, Some(Some(e)) = ready
            if guard.is_none() {
                tracing::info!("Initializing local embedding model (first use)...");
                match crate::embedder::Embedder::new() {
                    Ok(e) => *guard = Some(Some(e)),
                    Err(e) => {
                        tracing::warn!("Local embedder unavailable: {}", e);
                        *guard = Some(None);
                    }
                }
            }
            if let Some(Some(ref embedder)) = *guard {
                match embedder.embed(text) {
                    Ok(emb) => return Some(emb),
                    Err(e) => tracing::warn!("Auto-embed failed: {}", e),
                }
            }
        }
        None
    }

    /// Ingest a new memory from any channel.
    /// Automatically runs proactive inference to extract facts, preferences,
    /// and temporal hints. Also checks for contradictions with existing knowledge.
    pub fn ingest(
        &self,
        text: &str,
        channel: &str,
        user_id: Option<&str>,
        salience_hint: Option<f32>,
        embedding: Option<Vec<f32>>,
    ) -> Result<MemObject, CortexError> {
        let mut source = MemSource::new(channel);
        let embedding = self.auto_embed(text, embedding);

        // Resolve person identity if user_id provided
        if let Some(uid) = user_id {
            let people = PeopleGraph::new(&self.storage);
            let person = people.resolve_identity(channel, uid, None, None)?;
            source.identity_id = Some(person.id);
            people.record_interaction(person.id)?;
        }

        // ── Proactive inference ──────────────────────────────────────────
        let inferred = inference::extract(text);

        // Set durability based on temporal hint
        let durability = match inferred.temporal_hint {
            inference::TemporalHint::Temporary => MemoryDurability::Temporary,
            inference::TemporalHint::Permanent => MemoryDurability::Permanent,
            inference::TemporalHint::Unknown => MemoryDurability::Normal,
        };

        let mut builder = crate::types::MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(text.to_string()),
            source,
        )
        .salience(crate::types::Salience::new(salience_hint.unwrap_or(0.5)))
        .durability(durability);

        if let Some(emb) = embedding.clone() {
            builder = builder.embedding(emb);
        }

        let mem = builder.build();
        self.storage.store_memory(&mem)?;
        if let Some(emb) = embedding {
            self.index.insert(mem.id, emb);
        }
        let result = mem;

        // ── Auto-extract facts ───────────────────────────────────────────
        let semantic = SemanticStore::new(&self.storage, &self.index);
        for fact in &inferred.facts {
            // Check for contradictions before storing
            let contradictions = semantic.find_contradictions(
                &fact.subject, &fact.predicate, &fact.object,
            )?;

            if !contradictions.is_empty() {
                // New fact contradicts existing — supersede the old ones
                let new_fact = semantic.add_fact(
                    &fact.subject, &fact.predicate, &fact.object,
                    fact.confidence,
                    MemSource::new(channel),
                    None,
                )?;
                for (old, _score) in &contradictions {
                    semantic.merge_facts(old.id, new_fact.id)?;
                    tracing::info!(
                        subject = %fact.subject,
                        predicate = %fact.predicate,
                        new = %fact.object,
                        "Contradiction resolved: superseded old fact"
                    );
                }
            } else {
                // No contradiction — store normally
                let _ = semantic.add_fact(
                    &fact.subject, &fact.predicate, &fact.object,
                    fact.confidence,
                    MemSource::new(channel),
                    None,
                );
            }
        }

        // ── Auto-extract preferences ─────────────────────────────────────
        for pref in &inferred.preferences {
            let _ = semantic.add_preference(&pref.key, &pref.value, pref.confidence);
        }

        // ── Auto-extract relationships (bidirectional) ─────────────────
        let relationships = relationship::extract_relationships(text);
        let bidirectional = relationship::with_inverses(&relationships);
        for rel in &bidirectional {
            let _ = semantic.add_fact(
                &rel.person_a,
                &rel.relation,
                &rel.person_b,
                rel.confidence,
                MemSource::new(channel),
                None,
            );
            tracing::info!(
                a = %rel.person_a,
                relation = %rel.relation,
                b = %rel.person_b,
                "Auto-extracted relationship"
            );
        }

        // Auto-consolidation: run every N ingests
        let count = self.ingest_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if count % AUTO_CONSOLIDATION_INTERVAL == 0 {
            tracing::info!("Auto-consolidation triggered at ingest #{}", count);
            if let Err(e) = self.run_consolidation() {
                tracing::warn!("Auto-consolidation failed: {}", e);
            }
        }

        Ok(result)
    }

    /// Multi-signal retrieval.
    pub fn retrieve(
        &self,
        query: &str,
        limit: usize,
        channel: Option<&str>,
        person_id: Option<Uuid>,
        embedding: Option<Vec<f32>>,
    ) -> Result<Vec<RetrievalResult>, CortexError> {
        let embedding = self.auto_embed(query, embedding);

        let mut q = RetrievalQuery::new(query, limit);
        if let Some(ch) = channel {
            q = q.with_channel(ch);
        }
        if let Some(pid) = person_id {
            q = q.with_person(pid);
        }
        if let Some(emb) = embedding {
            q = q.with_embedding(emb);
        }

        let engine = RetrievalEngine::new(&self.storage, &self.index);
        engine.retrieve(&q)
    }

    /// Generate LLM-ready context from memory state.
    pub fn get_context(
        &self,
        max_tokens: usize,
        channel: Option<&str>,
        person_id: Option<Uuid>,
    ) -> Result<String, CortexError> {
        let mut config = ContextConfig::new(max_tokens);
        if let Some(ch) = channel {
            config = config.with_channel(ch);
        }
        if let Some(pid) = person_id {
            config = config.with_person(pid);
        }
        generate_context(&config, &self.storage, &self.index)
    }

    /// Add or resolve a person by channel identity.
    pub fn add_person(
        &self,
        name: &str,
        channel: &str,
        channel_user_id: &str,
    ) -> Result<crate::people::Person, CortexError> {
        let people = PeopleGraph::new(&self.storage);
        people.resolve_identity(channel, channel_user_id, Some(name), None)
    }

    /// Run a full consolidation cycle (decay + promote + sweep + patterns).
    pub fn run_consolidation(&self) -> Result<ConsolidationReport, CortexError> {
        let engine = ConsolidationEngine::new(&self.storage, &self.index);
        engine.run_consolidation_cycle()
    }

    /// Run only temporal decay on episodic memories.
    pub fn run_decay(&self) -> Result<usize, CortexError> {
        let episodes = EpisodeStore::new(&self.storage, &self.index);
        episodes.decay_tick()
    }

    /// Get beliefs above a confidence threshold.
    pub fn get_beliefs(
        &self,
        threshold: f32,
    ) -> Result<Vec<crate::belief::Belief>, CortexError> {
        let engine = BeliefEngine::new(&self.storage);
        engine.get_confident_beliefs(threshold)
    }

    /// Observe evidence for a belief.
    pub fn observe_belief(
        &self,
        key: &str,
        supports: bool,
        strength: f32,
    ) -> Result<crate::belief::Belief, CortexError> {
        let engine = BeliefEngine::new(&self.storage);
        let evidence = if supports {
            crate::belief::Evidence::Supports(strength)
        } else {
            crate::belief::Evidence::Contradicts(strength)
        };
        engine.observe(key, evidence)
    }

    /// Add a semantic fact.
    pub fn add_fact(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
        confidence: f32,
        channel: &str,
        embedding: Option<Vec<f32>>,
    ) -> Result<MemObject, CortexError> {
        let fact_text = format!("{} {} {}", subject, predicate, object);
        let embedding = self.auto_embed(&fact_text, embedding);

        let semantic = SemanticStore::new(&self.storage, &self.index);
        semantic.add_fact(
            subject,
            predicate,
            object,
            confidence,
            MemSource::new(channel),
            embedding,
        )
    }

    /// Add a user preference.
    pub fn add_preference(
        &self,
        key: &str,
        value: &str,
        confidence: f32,
    ) -> Result<MemObject, CortexError> {
        let semantic = SemanticStore::new(&self.storage, &self.index);
        semantic.add_preference(key, value, confidence)
    }

    /// Check for contradictions with a potential new fact.
    /// Returns existing facts that conflict (same subject+predicate, different object).
    pub fn check_contradictions(
        &self,
        subject: &str,
        predicate: &str,
        object: &str,
    ) -> Result<Vec<(MemObject, f32)>, CortexError> {
        let semantic = SemanticStore::new(&self.storage, &self.index);
        semantic.find_contradictions(subject, predicate, object)
    }

    /// Run proactive inference on text without ingesting.
    /// Returns extracted facts, preferences, and temporal classification.
    pub fn infer(&self, text: &str) -> inference::InferredKnowledge {
        inference::extract(text)
    }

    /// Extract relationships from text without ingesting.
    pub fn extract_relationships(&self, text: &str) -> Vec<relationship::InferredRelationship> {
        relationship::extract_relationships(text)
    }

    /// Run conversation compression on old sessions.
    /// Compresses sessions older than `max_age_days` with at least `min_messages` entries.
    pub fn run_compression(
        &self,
        min_messages: usize,
        max_age_days: i64,
    ) -> Result<CompressionReport, CortexError> {
        let engine = CompressionEngine::new(&self.storage, &self.index);
        engine.run_compression(min_messages, chrono::Duration::days(max_age_days))
    }

    /// Run compression with an external summarizer (e.g., LLM-powered).
    pub fn run_compression_with_summarizer(
        &self,
        min_messages: usize,
        max_age_days: i64,
        summarizer: &compression::SummarizerFn,
    ) -> Result<CompressionReport, CortexError> {
        let engine = CompressionEngine::new(&self.storage, &self.index)
            .with_summarizer(summarizer);
        engine.run_compression(min_messages, chrono::Duration::days(max_age_days))
    }

    /// Get memory statistics (counts per tier).
    pub fn stats(&self) -> Result<MemoryStats, CortexError> {
        let episodic = self.storage.count_by_tier(MemoryTier::Episodic)?;
        let semantic = self.storage.count_by_tier(MemoryTier::Semantic)?;
        let procedural = self.storage.count_by_tier(MemoryTier::Procedural)?;
        let people = self.storage.list_people()?.len();
        let beliefs = self.storage.list_beliefs_above(0.0)?.len();
        let index_size = self.index.len();
        Ok(MemoryStats {
            episodic,
            semantic,
            procedural,
            people,
            beliefs,
            index_size,
            total: episodic + semantic + procedural,
        })
    }

    /// Query facts by entity (SQL-indexed, no full scan).
    pub fn query_facts(&self, entity: &str) -> Result<Vec<MemObject>, CortexError> {
        let semantic = SemanticStore::new(&self.storage, &self.index);
        semantic.query_facts(entity)
    }

    /// Query preferences by key pattern.
    pub fn query_preferences(&self, key_pattern: &str) -> Result<Vec<MemObject>, CortexError> {
        let semantic = SemanticStore::new(&self.storage, &self.index);
        semantic.query_preferences(key_pattern)
    }

    /// List all known people.
    pub fn list_people(&self) -> Result<Vec<crate::people::Person>, CortexError> {
        self.storage.list_people()
    }

    /// Access to working memory.
    pub fn working_memory(&mut self) -> &mut WorkingMemory {
        &mut self.working
    }

    /// Get the underlying storage (for advanced use).
    pub fn storage(&self) -> &dyn StorageBackend {
        &self.storage
    }

    /// Get the memory index (for advanced use).
    pub fn index(&self) -> &MemoryIndex {
        &self.index
    }
}
