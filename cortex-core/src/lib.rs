pub mod belief;
pub mod compression;
pub mod consolidation;
pub mod context;
pub mod embedder;
pub mod episode;
pub mod events;
pub mod export;
pub mod inference;
pub mod people;
pub mod plugin;
pub mod plugin_manager;
pub mod plugins;
pub mod procedural;
pub mod relationship;
pub mod retrieval;
pub mod semantic;
pub mod storage;
pub mod sync;
pub mod types;
pub mod working;

use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use uuid::Uuid;

use crate::belief::BeliefEngine;
use crate::compression::{CompressionEngine, CompressionReport};
use crate::consolidation::{ConsolidationConfig, ConsolidationEngine, ConsolidationReport};
use crate::context::{ContextConfig, generate_context};
use crate::episode::EpisodeStore;
use crate::events::{EventBus, EventHandler};
use crate::people::PeopleGraph;
use crate::plugin::{Plugin, PluginAction, PluginContext};
use crate::plugin_manager::PluginManager;
use crate::retrieval::{RetrievalEngine, RetrievalQuery, RetrievalResult};
use crate::semantic::SemanticStore;
use crate::storage::memory_index::{IndexConfig, MemoryIndex};
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
    pub archived: usize,
    pub people: usize,
    pub beliefs: usize,
    pub index_size: usize,
    pub total: usize,
}

/// Observable metrics for Cortex operations.
pub struct CortexMetrics {
    pub ingests: AtomicU64,
    pub batch_ingests: AtomicU64,
    pub retrievals: AtomicU64,
    pub dedup_hits: AtomicU64,
    pub consolidations: AtomicU64,
    pub decay_runs: AtomicU64,
    pub compressions: AtomicU64,
    pub archives: AtomicU64,
    pub plugin_errors: AtomicU64,
}

impl Default for CortexMetrics {
    fn default() -> Self {
        Self {
            ingests: AtomicU64::new(0),
            batch_ingests: AtomicU64::new(0),
            retrievals: AtomicU64::new(0),
            dedup_hits: AtomicU64::new(0),
            consolidations: AtomicU64::new(0),
            decay_runs: AtomicU64::new(0),
            compressions: AtomicU64::new(0),
            archives: AtomicU64::new(0),
            plugin_errors: AtomicU64::new(0),
        }
    }
}

impl CortexMetrics {
    pub fn snapshot(&self) -> MetricsSnapshot {
        MetricsSnapshot {
            ingests: self.ingests.load(Ordering::Relaxed),
            batch_ingests: self.batch_ingests.load(Ordering::Relaxed),
            retrievals: self.retrievals.load(Ordering::Relaxed),
            dedup_hits: self.dedup_hits.load(Ordering::Relaxed),
            consolidations: self.consolidations.load(Ordering::Relaxed),
            decay_runs: self.decay_runs.load(Ordering::Relaxed),
            compressions: self.compressions.load(Ordering::Relaxed),
            archives: self.archives.load(Ordering::Relaxed),
            plugin_errors: self.plugin_errors.load(Ordering::Relaxed),
        }
    }
}

/// Point-in-time snapshot of metrics (all plain u64, no atomics).
#[derive(Debug, Clone)]
pub struct MetricsSnapshot {
    pub ingests: u64,
    pub batch_ingests: u64,
    pub retrievals: u64,
    pub dedup_hits: u64,
    pub consolidations: u64,
    pub decay_runs: u64,
    pub compressions: u64,
    pub archives: u64,
    pub plugin_errors: u64,
}

/// Default interval: auto-run consolidation every N ingests.
const DEFAULT_AUTO_CONSOLIDATION_INTERVAL: u64 = 100;

/// Main Cortex instance — the entry point for all memory operations.
pub struct Cortex {
    storage: SqliteStorage,
    index: MemoryIndex,
    working: WorkingMemory,
    ingest_counter: AtomicU64,
    /// Optional custom decay config (opt-in, default disabled).
    decay_config: Option<episode::DecayConfig>,
    /// Optional LLM inference callback (opt-in, default disabled).
    inference_fn: Option<inference::InferenceFn>,
    /// Plugin manager for lifecycle hooks and custom tools.
    plugins: PluginManager,
    /// Consolidation configuration (tunable parameters).
    consolidation_config: ConsolidationConfig,
    /// How often to auto-run consolidation (every N ingests).
    auto_consolidation_interval: u64,
    /// Observable operation metrics.
    metrics: CortexMetrics,
    /// Event bus for change feed.
    event_bus: EventBus,
    /// Deduplication configuration.
    dedup_config: DeduplicationConfig,
    /// LRU cache for retrieval results (parking_lot::Mutex for faster lock/unlock).
    /// Cache entries are tagged with write_generation; stale entries are evicted on read.
    retrieval_cache: parking_lot::Mutex<lru::LruCache<u64, (u64, Vec<RetrievalResult>)>>,
    /// Monotonic counter incremented on content-changing writes (ingest, delete, promote).
    /// Decay/salience-only updates don't bump this — cached rankings stay approximately valid.
    write_generation: AtomicU64,
    /// Whether the vector index has been loaded from storage.
    index_loaded: AtomicBool,
    /// Cloud sync engine (opt-in, default disabled).
    sync_engine: parking_lot::Mutex<Option<crate::sync::SyncEngine>>,
    /// Handle for background sync (polling + filesystem watcher).
    background_sync_handle: parking_lot::Mutex<Option<crate::sync::BackgroundSyncHandle>>,
    #[cfg(feature = "embeddings")]
    embedder: parking_lot::Mutex<Option<Option<crate::embedder::Embedder>>>,
}

impl Cortex {
    /// Open or create a Cortex database at the given path.
    pub fn open(db_path: &str) -> Result<Self, CortexError> {
        Self::open_with_config(db_path, IndexConfig::default())
    }

    /// Open or create an encrypted Cortex database.
    /// Requires the `encrypted-db` feature (SQLCipher).
    pub fn open_encrypted(db_path: &str, passphrase: &str) -> Result<Self, CortexError> {
        let storage = SqliteStorage::open_with_key(db_path, Some(passphrase))?;
        let index = MemoryIndex::with_config(IndexConfig::default());
        Ok(Self::build(storage, index))
    }

    /// Open or create a Cortex database with custom index configuration.
    /// Vector index is lazily loaded on first retrieve/get_context call for fast startup.
    pub fn open_with_config(db_path: &str, index_config: IndexConfig) -> Result<Self, CortexError> {
        let storage = SqliteStorage::open(db_path)?;
        let index = MemoryIndex::with_config(index_config);
        Ok(Self::build(storage, index))
    }

    fn build(storage: SqliteStorage, index: MemoryIndex) -> Self {
        Self {
            storage,
            index,
            working: WorkingMemory::default(),
            ingest_counter: AtomicU64::new(0),
            decay_config: None,
            inference_fn: None,
            plugins: PluginManager::new(),
            consolidation_config: ConsolidationConfig::default(),
            auto_consolidation_interval: DEFAULT_AUTO_CONSOLIDATION_INTERVAL,
            metrics: CortexMetrics::default(),
            event_bus: EventBus::new(),
            dedup_config: DeduplicationConfig::default(),
            retrieval_cache: parking_lot::Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(64).unwrap(),
            )),
            write_generation: AtomicU64::new(0),
            index_loaded: AtomicBool::new(false),
            sync_engine: parking_lot::Mutex::new(None),
            background_sync_handle: parking_lot::Mutex::new(None),
            #[cfg(feature = "embeddings")]
            embedder: parking_lot::Mutex::new(None), // lazy init on first use
        }
    }

    /// Create an in-memory Cortex (useful for testing).
    pub fn in_memory() -> Result<Self, CortexError> {
        let storage = SqliteStorage::open_in_memory()?;
        let mut cortex = Self::build(storage, MemoryIndex::new());
        cortex.index_loaded = AtomicBool::new(true); // in-memory starts empty
        Ok(cortex)
    }

    /// Enable cloud sync. Creates sync folder structure and subscribes to events.
    pub fn enable_sync(&self, config: crate::sync::SyncConfig) -> Result<(), CortexError> {
        let engine = crate::sync::SyncEngine::new(config, &self.storage)?;
        *self.sync_engine.lock() = Some(engine);
        Ok(())
    }

    /// Pull remote sync changes from other devices.
    /// Returns the number of operations applied.
    pub fn sync_pull(&self) -> Result<usize, CortexError> {
        let mut guard = self.sync_engine.lock();
        let engine = guard.as_mut()
            .ok_or_else(|| CortexError::InvalidInput("Sync not enabled".into()))?;
        engine.pull_remote(&self.storage, &self.index)
    }

    /// Get sync status. Returns None if sync is not enabled.
    pub fn sync_status(&self) -> Option<crate::sync::SyncStatus> {
        let guard = self.sync_engine.lock();
        guard.as_ref().and_then(|e| e.status().ok())
    }

    /// Record a sync operation for a memory event (called internally after mutations).
    fn sync_record_event(&self, event: &CortexEvent) {
        let mut guard = self.sync_engine.lock();
        if let Some(ref mut engine) = *guard {
            if let Err(e) = engine.record_memory_event(event, &self.storage) {
                tracing::warn!(error = %e, "Failed to record sync event");
            }
        }
    }

    /// Start background sync: periodic polling + filesystem watcher.
    /// Sync must be enabled first via `enable_sync()`.
    ///
    /// Uses a channel-based design: the watcher and timer send "pull requested"
    /// signals, the background thread calls `sync_pull()` through a callback
    /// that captures only Arc-wrapped data (no raw pointers, no UB on move).
    pub fn start_background_sync(self: &std::sync::Arc<Self>) -> Result<(), CortexError> {
        let guard = self.sync_engine.lock();
        let engine_ref = guard.as_ref()
            .ok_or_else(|| CortexError::InvalidInput("Sync not enabled. Call enable_sync() first.".into()))?;
        let config = engine_ref.config().clone();
        drop(guard);

        // Stop any existing background sync
        let mut bg_guard = self.background_sync_handle.lock();
        if let Some(handle) = bg_guard.take() {
            handle.stop();
        }

        let stop_flag = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let stop_clone = stop_flag.clone();

        // Channel for watcher notifications
        let (notify_tx, notify_rx) = std::sync::mpsc::channel::<()>();
        let notify_tx_clone = notify_tx.clone();

        // Start filesystem watcher
        let watcher_handle = crate::sync::watcher::start_watcher(
            crate::sync::watcher::WatcherConfig {
                watch_dir: config.devices_dir(),
                device_id: config.device_id.clone(),
                debounce: std::time::Duration::from_secs(2),
            },
            Box::new(move || {
                let _ = notify_tx_clone.send(());
            }),
        )?;

        // Use Weak<Cortex> — does NOT prevent Drop. Thread auto-exits when Cortex is dropped.
        let cortex_weak = std::sync::Arc::downgrade(self);
        let poll_interval = config.poll_interval();

        let poll_thread = std::thread::Builder::new()
            .name("cortex-bg-sync".into())
            .spawn(move || {
                let mut last_poll = std::time::Instant::now();
                loop {
                    if stop_clone.load(std::sync::atomic::Ordering::Acquire) {
                        break;
                    }

                    let remaining = poll_interval.saturating_sub(last_poll.elapsed());
                    let timeout = remaining.min(std::time::Duration::from_millis(200));

                    let should_pull = match notify_rx.recv_timeout(timeout) {
                        Ok(()) => true,
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                            last_poll.elapsed() >= poll_interval
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
                    };

                    while notify_rx.try_recv().is_ok() {}

                    if should_pull {
                        last_poll = std::time::Instant::now();
                        // Upgrade Weak → Arc. If Cortex was dropped, exit thread.
                        match cortex_weak.upgrade() {
                            Some(cortex) => {
                                match cortex.sync_pull() {
                                    Ok(0) => {}
                                    Ok(n) => tracing::info!(applied = n, "Background sync: pulled changes"),
                                    Err(e) => tracing::warn!(error = %e, "Background sync: pull failed"),
                                }
                            }
                            None => break, // Cortex dropped, exit
                        }
                    }
                }
                tracing::debug!("Background sync thread exiting");
            })
            .map_err(|e| CortexError::Storage(format!("Failed to spawn bg sync thread: {}", e)))?;

        *bg_guard = Some(crate::sync::BackgroundSyncHandle::new(
            stop_flag,
            poll_thread,
            watcher_handle,
        ));

        Ok(())
    }

    /// Stop background sync if running.
    pub fn stop_background_sync(&self) {
        let mut guard = self.background_sync_handle.lock();
        if let Some(handle) = guard.take() {
            handle.stop();
        }
    }

    /// Enable custom decay configuration (opt-in).
    /// When set, overrides the hardcoded decay half-lives.
    pub fn with_decay_config(mut self, config: episode::DecayConfig) -> Self {
        self.decay_config = Some(config);
        self
    }

    /// Enable LLM-assisted inference (opt-in).
    /// When set, LLM results are merged with pattern matching during ingest.
    pub fn with_inference_fn(mut self, f: inference::InferenceFn) -> Self {
        self.inference_fn = Some(f);
        self
    }

    /// Set custom consolidation configuration.
    pub fn with_consolidation_config(mut self, config: ConsolidationConfig) -> Self {
        self.consolidation_config = config;
        self
    }

    /// Set how often auto-consolidation runs (every N ingests).
    pub fn with_auto_consolidation_interval(mut self, interval: u64) -> Self {
        self.auto_consolidation_interval = interval;
        self
    }

    /// Register a plugin (compile-time, trait-object based).
    pub fn with_plugin(mut self, plugin: Box<dyn Plugin>) -> Self {
        self.plugins.register(plugin);
        self
    }

    /// Register a plugin with configuration.
    pub fn with_plugin_config(mut self, plugin: Box<dyn Plugin>, config: serde_json::Value) -> Self {
        self.plugins.register_with_config(plugin, config);
        self
    }

    /// Set deduplication configuration.
    pub fn with_dedup_config(mut self, config: DeduplicationConfig) -> Self {
        self.dedup_config = config;
        self
    }

    /// Subscribe to the event bus (change feed).
    pub fn on_event(&self, handler: EventHandler) {
        self.event_bus.subscribe(handler);
    }

    /// Get a snapshot of operation metrics.
    pub fn metrics(&self) -> MetricsSnapshot {
        self.metrics.snapshot()
    }

    /// Lazily load vector index from storage on first retrieval call.
    /// Uses compare_exchange to ensure only one thread loads the index.
    fn ensure_index_loaded(&self) -> Result<(), CortexError> {
        // Fast path: already loaded
        if self.index_loaded.load(Ordering::Acquire) {
            return Ok(());
        }
        // Try to claim the loading responsibility (only one thread wins)
        if self.index_loaded.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
            // Another thread is loading or has loaded — we're done
            return Ok(());
        }
        let all_tiers = [MemoryTier::Episodic, MemoryTier::Semantic, MemoryTier::Procedural];
        for tier in &all_tiers {
            let mems = self.storage.list_by_tier(*tier, 100_000)?;
            for mem in mems {
                if let Some(ref emb) = mem.embedding {
                    self.index.insert_arc(mem.id, emb);
                }
            }
        }
        tracing::info!(index_size = self.index.len(), "Lazy-loaded vector index");
        Ok(())
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
        self.ingest_with_namespace(text, channel, user_id, salience_hint, embedding, None)
    }

    /// Ingest a new memory with explicit namespace isolation.
    pub fn ingest_with_namespace(
        &self,
        text: &str,
        channel: &str,
        user_id: Option<&str>,
        salience_hint: Option<f32>,
        embedding: Option<Vec<f32>>,
        namespace: Option<&str>,
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

        // ── Proactive inference (with optional LLM) ────────────────────
        let inferred = inference::extract_with_llm(text, self.inference_fn.as_ref());

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
        if let Some(ns) = namespace {
            builder = builder.namespace(ns.to_string());
        }

        let mut mem = builder.build();

        // ── Plugin: on_pre_ingest ─────────────────────────────────────────
        let plugin_ctx = PluginContext {
            storage: &self.storage,
            index: &self.index,
        };
        let pre_actions = self.plugins.on_pre_ingest(&mem, text, &plugin_ctx);
        let skip = PluginManager::apply_actions(&pre_actions, &mut mem);
        if skip {
            return Ok(mem);
        }

        // Apply plugin-requested side effects (facts, preferences, beliefs)
        let deferred_actions: Vec<_> = pre_actions
            .iter()
            .filter(|a| matches!(a, PluginAction::AddFact { .. } | PluginAction::AddPreference { .. } | PluginAction::ObserveBelief { .. }))
            .cloned()
            .collect();

        // ── Deduplication check ─────────────────────────────────────────
        if self.dedup_config.exact_dedup {
            if let Some(ref hash) = mem.content_hash {
                if self.storage.find_by_content_hash(hash)?.is_some() {
                    self.metrics.dedup_hits.fetch_add(1, Ordering::Relaxed);
                    tracing::debug!(hash = %hash, "Exact duplicate skipped");
                    return Ok(mem);
                }
            }
        }

        self.storage.store_memory(&mem)?;
        if let Some(emb) = embedding {
            self.index.insert(mem.id, emb);
        }
        let result = mem.clone();

        // Emit event + sync
        let event = CortexEvent::MemoryCreated {
            id: result.id,
            tier: result.tier,
        };
        self.event_bus.emit(&event);
        self.sync_record_event(&event);
        self.metrics.ingests.fetch_add(1, Ordering::Relaxed);

        // ── Plugin: on_post_ingest ────────────────────────────────────────
        self.plugins.on_post_ingest(&result, &plugin_ctx);

        // ── Auto-extract facts ───────────────────────────────────────────
        let semantic = SemanticStore::new(&self.storage, &self.index);
        if !inferred.facts.is_empty() {
            // Batch-load all existing facts for relevant subjects in a single query
            let subjects: Vec<String> = inferred.facts.iter()
                .map(|f| f.subject.clone())
                .collect::<std::collections::HashSet<_>>()
                .into_iter()
                .collect();
            let existing_facts = self.storage.query_facts_by_entities(&subjects)?;

            for fact in &inferred.facts {
                // Check contradictions against preloaded facts (in-memory, no SQL)
                let contradictions: Vec<(&MemObject, f32)> = existing_facts.iter()
                    .filter_map(|m| {
                        if let MemContent::Fact { subject: ref s, predicate: ref p, object: ref o } = m.content {
                            if s.to_lowercase() == fact.subject.to_lowercase()
                                && p.to_lowercase() == fact.predicate.to_lowercase()
                                && o.to_lowercase() != fact.object.to_lowercase()
                            {
                                return Some((m, m.salience.effective_score));
                            }
                        }
                        None
                    })
                    .collect();

                if !contradictions.is_empty() {
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
                    let _ = semantic.add_fact(
                        &fact.subject, &fact.predicate, &fact.object,
                        fact.confidence,
                        MemSource::new(channel),
                        None,
                    );
                }
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

        // ── Plugin: apply deferred actions (facts, preferences, beliefs) ──
        self.apply_plugin_side_effects(&deferred_actions, "pre_ingest");

        // Auto-consolidation: run every N ingests
        let count = self.ingest_counter.fetch_add(1, Ordering::Relaxed) + 1;
        if self.auto_consolidation_interval > 0
            && count.is_multiple_of(self.auto_consolidation_interval)
        {
            tracing::info!("Auto-consolidation triggered at ingest #{}", count);
            if let Err(e) = self.run_consolidation() {
                tracing::warn!("Auto-consolidation failed: {}", e);
            }
        }

        self.bump_write_generation();
        Ok(result)
    }

    /// Batch ingest multiple memories in a single transaction.
    /// Supports deduplication and batched index rebuild.
    #[tracing::instrument(skip(self, items), fields(count = items.len()))]
    pub fn ingest_batch(
        &self,
        items: Vec<BatchIngestItem>,
    ) -> Result<BatchIngestReport, CortexError> {
        let mut report = BatchIngestReport {
            total_submitted: items.len(),
            ..Default::default()
        };

        let mut mems_to_store: Vec<MemObject> = Vec::with_capacity(items.len());
        let mut embeddings_to_index: Vec<(Uuid, Vec<f32>)> = Vec::new();
        let mut batch_inferred: Vec<(String, inference::InferredKnowledge)> = Vec::new();

        let plugin_ctx = PluginContext {
            storage: &self.storage,
            index: &self.index,
        };

        // Create PeopleGraph once for the entire batch
        let people = PeopleGraph::new(&self.storage);

        for item in &items {
            let mut source = MemSource::new(&item.channel);
            let embedding = self.auto_embed(&item.text, item.embedding.clone());

            if let Some(ref uid) = item.user_id {
                if let Ok(person) = people.resolve_identity(&item.channel, uid, None, None) {
                    source.identity_id = Some(person.id);
                    let _ = people.record_interaction(person.id);
                }
            }

            let inferred = inference::extract_with_llm(&item.text, self.inference_fn.as_ref());
            let durability = match inferred.temporal_hint {
                inference::TemporalHint::Temporary => MemoryDurability::Temporary,
                inference::TemporalHint::Permanent => MemoryDurability::Permanent,
                inference::TemporalHint::Unknown => MemoryDurability::Normal,
            };

            let mut builder = MemObjectBuilder::new(
                MemoryTier::Episodic,
                MemContent::Text(item.text.clone()),
                source,
            )
            .salience(Salience::new(item.salience_hint.unwrap_or(0.5)))
            .durability(durability);

            if let Some(ref ns) = item.namespace {
                builder = builder.namespace(ns.clone());
            }
            if let Some(ref emb) = embedding {
                builder = builder.embedding(emb.clone());
            }

            let mut mem = builder.build();

            // Dedup check
            if self.dedup_config.exact_dedup {
                if let Some(ref hash) = mem.content_hash {
                    if self.storage.find_by_content_hash(hash)?.is_some() {
                        report.deduplicated += 1;
                        self.metrics.dedup_hits.fetch_add(1, Ordering::Relaxed);
                        continue;
                    }
                    // Also check within current batch
                    if mems_to_store.iter().any(|m| m.content_hash.as_ref() == Some(hash)) {
                        report.deduplicated += 1;
                        continue;
                    }
                }
            }

            // Plugin pre-ingest
            let pre_actions = self.plugins.on_pre_ingest(&mem, &item.text, &plugin_ctx);
            let skip = PluginManager::apply_actions(&pre_actions, &mut mem);
            if skip {
                report.skipped_by_plugin += 1;
                continue;
            }

            if let Some(emb) = embedding {
                embeddings_to_index.push((mem.id, emb));
            }
            // Collect inferred knowledge for post-batch fact/preference extraction
            if !inferred.facts.is_empty() || !inferred.preferences.is_empty() {
                batch_inferred.push((item.channel.clone(), inferred));
            }
            mems_to_store.push(mem);
        }

        // Batch store in a single transaction
        let stored = self.storage.store_memories_batch(&mems_to_store)?;
        report.stored = stored;

        // Batch index insert
        for (id, emb) in embeddings_to_index {
            self.index.insert(id, emb);
        }

        // Emit events + sync
        for mem in &mems_to_store {
            let event = CortexEvent::MemoryCreated {
                id: mem.id,
                tier: mem.tier,
            };
            self.event_bus.emit(&event);
            self.sync_record_event(&event);
        }

        // Auto-extract facts and preferences from batch (like single ingest)
        if !batch_inferred.is_empty() {
            let semantic = SemanticStore::new(&self.storage, &self.index);
            for (channel, inferred) in &batch_inferred {
                for fact in &inferred.facts {
                    let _ = semantic.add_fact(
                        &fact.subject, &fact.predicate, &fact.object,
                        fact.confidence,
                        MemSource::new(channel.as_str()),
                        None,
                    );
                }
                for pref in &inferred.preferences {
                    let _ = semantic.add_preference(&pref.key, &pref.value, pref.confidence);
                }
            }
        }

        self.metrics.batch_ingests.fetch_add(1, Ordering::Relaxed);
        self.metrics.ingests.fetch_add(stored as u64, Ordering::Relaxed);

        // Auto-consolidation check
        let count = self.ingest_counter.fetch_add(stored as u64, Ordering::Relaxed) + stored as u64;
        if self.auto_consolidation_interval > 0
            && count / self.auto_consolidation_interval > (count - stored as u64) / self.auto_consolidation_interval
        {
            tracing::info!("Auto-consolidation triggered after batch ingest at count {}", count);
            if let Err(e) = self.run_consolidation() {
                tracing::warn!("Auto-consolidation failed: {}", e);
            }
        }

        self.bump_write_generation();
        Ok(report)
    }

    /// Multi-signal retrieval with optional namespace isolation.
    #[tracing::instrument(skip(self, embedding))]
    pub fn retrieve(
        &self,
        query: &str,
        limit: usize,
        channel: Option<&str>,
        person_id: Option<Uuid>,
        embedding: Option<Vec<f32>>,
    ) -> Result<Vec<RetrievalResult>, CortexError> {
        self.retrieve_with_namespace(query, limit, channel, person_id, embedding, None)
    }

    /// Multi-signal retrieval with explicit namespace filtering.
    #[tracing::instrument(skip(self, embedding))]
    pub fn retrieve_with_namespace(
        &self,
        query: &str,
        limit: usize,
        channel: Option<&str>,
        person_id: Option<Uuid>,
        embedding: Option<Vec<f32>>,
        namespace: Option<&str>,
    ) -> Result<Vec<RetrievalResult>, CortexError> {
        self.ensure_index_loaded()?;

        // ── Cache lookup ──────────────────────────────────────────────────
        let cache_key = {
            let mut h = DefaultHasher::new();
            query.hash(&mut h);
            limit.hash(&mut h);
            channel.hash(&mut h);
            person_id.hash(&mut h);
            namespace.hash(&mut h);
            h.finish()
        };

        let current_gen = self.write_generation.load(Ordering::Acquire);
        {
            let mut cache = self.retrieval_cache.lock();
            if let Some((gen, cached)) = cache.get(&cache_key) {
                if *gen == current_gen {
                    self.metrics.retrievals.fetch_add(1, Ordering::Relaxed);
                    return Ok(cached.clone());
                }
                // Stale entry — generation mismatch, treat as miss
            }
        }

        let embedding = self.auto_embed(query, embedding);

        let mut q = RetrievalQuery::new(query, limit);
        if let Some(ch) = channel {
            q = q.with_channel(ch);
        }
        if let Some(pid) = person_id {
            q = q.with_person(pid);
        }
        if let Some(ns) = namespace {
            q = q.with_namespace(ns);
        }
        if let Some(emb) = embedding {
            q = q.with_embedding(emb);
        }

        let engine = RetrievalEngine::new(&self.storage, &self.index);
        let mut results = engine.retrieve(&q)?;

        // ── Plugin: on_pre_retrieve ───────────────────────────────────────
        let plugin_ctx = PluginContext {
            storage: &self.storage,
            index: &self.index,
        };
        self.plugins.on_pre_retrieve(query, &mut results, &plugin_ctx);

        // ── Cache store (tagged with current generation) ─────────────────
        {
            let mut cache = self.retrieval_cache.lock();
            cache.put(cache_key, (current_gen, results.clone()));
        }

        self.metrics.retrievals.fetch_add(1, Ordering::Relaxed);
        Ok(results)
    }

    /// Generate LLM-ready context from memory state.
    pub fn get_context(
        &self,
        max_tokens: usize,
        channel: Option<&str>,
        person_id: Option<Uuid>,
    ) -> Result<String, CortexError> {
        self.get_context_with_namespace(max_tokens, channel, person_id, None)
    }

    /// Generate LLM-ready context with namespace isolation.
    pub fn get_context_with_namespace(
        &self,
        max_tokens: usize,
        channel: Option<&str>,
        person_id: Option<Uuid>,
        namespace: Option<&str>,
    ) -> Result<String, CortexError> {
        self.ensure_index_loaded()?;
        let mut config = ContextConfig::new(max_tokens);
        if let Some(ch) = channel {
            config = config.with_channel(ch);
        }
        if let Some(pid) = person_id {
            config = config.with_person(pid);
        }
        if let Some(ns) = namespace {
            config = config.with_namespace(ns);
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
    #[tracing::instrument(skip(self))]
    pub fn run_consolidation(&self) -> Result<ConsolidationReport, CortexError> {
        let engine = ConsolidationEngine::new(&self.storage, &self.index)
            .with_config(self.consolidation_config.clone())
            .with_decay_config(self.decay_config.as_ref());
        let report = engine.run_consolidation_cycle()?;

        // ── Plugin: on_consolidation ──────────────────────────────────────
        let plugin_ctx = PluginContext {
            storage: &self.storage,
            index: &self.index,
        };
        let actions = self.plugins.on_consolidation(&report, &plugin_ctx);
        self.apply_plugin_side_effects(&actions, "consolidation");

        self.bump_write_generation();
        self.metrics.consolidations.fetch_add(1, Ordering::Relaxed);
        self.event_bus.emit(&CortexEvent::ConsolidationCompleted {
            report: format!("{:?}", report),
        });
        Ok(report)
    }

    /// Run only temporal decay on episodic memories.
    #[tracing::instrument(skip(self))]
    pub fn run_decay(&self) -> Result<usize, CortexError> {
        let mut episodes = EpisodeStore::new(&self.storage, &self.index);
        if let Some(ref cfg) = self.decay_config {
            episodes = episodes.with_decay_config(cfg);
        }
        let count = episodes.decay_tick()?;

        // ── Plugin: on_decay ──────────────────────────────────────────────
        let plugin_ctx = PluginContext {
            storage: &self.storage,
            index: &self.index,
        };
        self.plugins.on_decay(count, &plugin_ctx);

        // Decay only adjusts salience scores — cached retrieval rankings stay approximately
        // valid, so we skip bumping write_generation here. Saves cache thrashing during
        // auto-consolidation cycles that run decay on every 100 ingests.
        self.metrics.decay_runs.fetch_add(1, Ordering::Relaxed);
        self.event_bus.emit(&CortexEvent::DecayCompleted { count });
        Ok(count)
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
    /// Uses LLM callback if configured, otherwise pattern matching only.
    pub fn infer(&self, text: &str) -> inference::InferredKnowledge {
        inference::extract_with_llm(text, self.inference_fn.as_ref())
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
        let archived = self.storage.count_by_tier(MemoryTier::Archived)?;
        let people = self.storage.list_people()?.len();
        let beliefs = self.storage.list_beliefs_above(0.0)?.len();
        let index_size = self.index.len();
        Ok(MemoryStats {
            episodic,
            semantic,
            procedural,
            archived,
            people,
            beliefs,
            index_size,
            total: episodic + semantic + procedural,
        })
    }

    /// Archive a memory (move to cold storage tier).
    pub fn archive_memory(&self, id: Uuid) -> Result<(), CortexError> {
        self.storage.archive_memory(id)?;
        self.index.remove(&id);
        self.bump_write_generation();
        self.metrics.archives.fetch_add(1, Ordering::Relaxed);
        let event = CortexEvent::MemoryArchived { id };
        self.event_bus.emit(&event);
        self.sync_record_event(&event);
        Ok(())
    }

    /// Restore an archived memory to its original tier.
    pub fn restore_memory(&self, id: Uuid, target_tier: MemoryTier) -> Result<(), CortexError> {
        self.storage.restore_memory(id, target_tier)?;
        // Re-index if it has an embedding
        if let Some(mem) = self.storage.get_memory(id)? {
            if let Some(ref emb) = mem.embedding {
                self.index.insert_arc(id, emb);
            }
        }
        self.bump_write_generation();
        Ok(())
    }

    /// Archive old, low-salience memories automatically.
    /// Moves episodic memories older than `max_age_days` with salience below `threshold`.
    pub fn auto_archive(
        &self,
        max_age_days: i64,
        salience_threshold: f32,
    ) -> Result<usize, CortexError> {
        let cutoff = chrono::Utc::now() - chrono::Duration::days(max_age_days);
        let candidates = self.storage.list_by_salience_below(MemoryTier::Episodic, salience_threshold)?;
        let mut count = 0;
        for mem in candidates {
            if mem.temporal.ingestion_time < cutoff
                && mem.temporal.durability != MemoryDurability::Permanent
            {
                self.archive_memory(mem.id)?;
                count += 1;
            }
        }
        tracing::info!(count, "Auto-archived old low-salience memories");
        Ok(count)
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

    /// Delete a memory permanently.
    pub fn delete_memory(&self, id: Uuid) -> Result<(), CortexError> {
        self.storage.delete_memory(id)?;
        self.index.remove(&id);
        self.bump_write_generation();
        let event = CortexEvent::MemoryDeleted { id };
        self.event_bus.emit(&event);
        self.sync_record_event(&event);
        Ok(())
    }

    /// List all distinct namespaces with memory counts.
    pub fn list_namespaces(&self) -> Result<Vec<(String, usize)>, CortexError> {
        self.storage.list_namespaces()
    }

    /// Merge two person identities — moves all identities/notes/tags from
    /// `source_id` to `target_id`, then deletes the source person.
    pub fn merge_people(&self, target_id: Uuid, source_id: Uuid) -> Result<crate::people::Person, CortexError> {
        let people = PeopleGraph::new(&self.storage);
        let merged = people.merge_identities(target_id, source_id)?;
        self.bump_write_generation();
        Ok(merged)
    }

    /// Get the underlying storage as a trait object (for advanced use).
    pub fn storage(&self) -> &dyn StorageBackend {
        &self.storage
    }

    /// Get the underlying SQLite storage (for sync and advanced use).
    pub fn sqlite_storage(&self) -> &SqliteStorage {
        &self.storage
    }

    /// Get the memory index (for advanced use).
    pub fn index(&self) -> &MemoryIndex {
        &self.index
    }

    /// Get the plugin manager (for tool dispatch from MCP server).
    pub fn plugin_manager(&self) -> &PluginManager {
        &self.plugins
    }

    /// Create a PluginContext for the current state.
    pub fn plugin_context(&self) -> PluginContext<'_> {
        PluginContext {
            storage: &self.storage,
            index: &self.index,
        }
    }

    /// Bump the write generation, lazily invalidating all cached retrieval results.
    /// Use for content-changing writes (ingest, delete, promote, compress).
    /// Does NOT lock the cache — stale entries are evicted on next read.
    fn bump_write_generation(&self) {
        self.write_generation.fetch_add(1, Ordering::Release);
    }

    /// Apply side-effect actions (facts, preferences, beliefs) from plugins.
    fn apply_plugin_side_effects(&self, actions: &[PluginAction], _hook: &str) {
        let semantic = SemanticStore::new(&self.storage, &self.index);
        for action in actions {
            match action {
                PluginAction::AddFact { subject, predicate, object, confidence } => {
                    let _ = semantic.add_fact(
                        subject, predicate, object, *confidence,
                        MemSource::new("plugin"), None,
                    );
                }
                PluginAction::AddPreference { key, value, confidence } => {
                    let _ = semantic.add_preference(key, value, *confidence);
                }
                PluginAction::ObserveBelief { key, supports, strength } => {
                    let belief_engine = BeliefEngine::new(&self.storage);
                    let evidence = if *supports {
                        crate::belief::Evidence::Supports(*strength)
                    } else {
                        crate::belief::Evidence::Contradicts(*strength)
                    };
                    let _ = belief_engine.observe(key, evidence);
                }
                _ => {}
            }
        }
    }
}

impl Drop for Cortex {
    fn drop(&mut self) {
        // CRITICAL: Stop background sync thread BEFORE fields it references are dropped.
        // The background thread holds raw pointers to storage/index/sync_engine.
        self.stop_background_sync();
    }
}
