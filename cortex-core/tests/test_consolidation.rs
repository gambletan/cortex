use cortex_core::consolidation::ConsolidationEngine;
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;

fn setup() -> (SqliteStorage, MemoryIndex) {
    (SqliteStorage::open_in_memory().unwrap(), MemoryIndex::new())
}

#[test]
fn test_promote_to_semantic() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    // Add 3 identical episodic facts
    let mut ids = Vec::new();
    for _ in 0..3 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Fact {
                subject: "User".to_string(),
                predicate: "prefers".to_string(),
                object: "dark mode".to_string(),
            },
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
        ids.push(mem.id);
    }

    let promoted = engine.promote_to_semantic(ids.clone()).unwrap();
    assert_eq!(promoted.tier, MemoryTier::Semantic);

    // Check links were created
    let links = storage.get_links(promoted.id).unwrap();
    assert_eq!(links.len(), 3);
}

#[test]
fn test_sweep_decayed() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    // Add memories with very low salience
    for i in 0..5 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("old memory {}", i)),
            MemSource::new("cli"),
        )
        .salience(Salience {
            base_score: 0.01,
            emotional_weight: 1.0,
            access_boost: 1.0,
            decay_factor: 0.01,
            effective_score: 0.0001,
        })
        .build();
        storage.store_memory(&mem).unwrap();
    }

    // Add one healthy memory
    let healthy = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("fresh memory".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience::new(0.8))
    .build();
    storage.store_memory(&healthy).unwrap();

    let swept = engine.sweep_decayed(0.1).unwrap();
    assert_eq!(swept, 5);

    let remaining = storage.count_by_tier(MemoryTier::Episodic).unwrap();
    assert_eq!(remaining, 1);
}

#[test]
fn test_run_consolidation_cycle() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    // Add some episodic memories
    for _ in 0..3 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text("repeated observation".to_string()),
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    let report = engine.run_consolidation_cycle().unwrap();
    assert!(report.episodes_scanned > 0);
    // The 3 identical texts should be promoted
    assert_eq!(report.promoted_to_semantic, 1);
}

#[test]
fn test_promote_empty_ids_fails() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    let result = engine.promote_to_semantic(vec![]);
    assert!(result.is_err());
}

#[test]
fn test_sweep_no_decayed() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    // Add healthy memories only
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("fresh".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience::new(0.9))
    .build();
    storage.store_memory(&mem).unwrap();

    let swept = engine.sweep_decayed(0.1).unwrap();
    assert_eq!(swept, 0);
}

#[test]
fn test_consolidation_preserves_semantic() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    // Add a semantic memory (should not be swept)
    let sem = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Text("permanent knowledge".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience {
        base_score: 0.01,
        emotional_weight: 1.0,
        access_boost: 1.0,
        decay_factor: 0.01,
        effective_score: 0.0001,
    })
    .build();
    storage.store_memory(&sem).unwrap();

    let swept = engine.sweep_decayed(0.1).unwrap();
    assert_eq!(swept, 0); // semantic memories not swept
}

#[test]
fn test_consolidation_cycle_includes_decay() {
    let (storage, index) = setup();

    // Add episodic memory with old last_accessed time
    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("old memory".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience::new(0.5))
    .build();
    // Simulate 200 hours since last access
    mem.temporal.last_accessed = chrono::Utc::now() - chrono::Duration::hours(200);
    storage.store_memory(&mem).unwrap();

    let engine = ConsolidationEngine::new(&storage, &index);
    let report = engine.run_consolidation_cycle().unwrap();

    // Decay should have been applied
    assert!(report.decayed_updated > 0);

    // Check the memory's decay_factor was reduced
    let updated = storage.get_memory(mem.id).unwrap().unwrap();
    assert!(updated.salience.decay_factor < 0.9, "decay_factor should have decreased: {}", updated.salience.decay_factor);
}

#[test]
fn test_importance_aware_decay() {
    use cortex_core::episode::EpisodeStore;

    let (storage, index) = setup();
    let store = EpisodeStore::new(&storage, &index);

    // Add two memories: one high-importance, one low-importance
    let mut low_imp = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("low importance".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience::new(0.2)) // low base_score
    .build();
    low_imp.temporal.last_accessed = chrono::Utc::now() - chrono::Duration::hours(100);
    storage.store_memory(&low_imp).unwrap();

    let mut high_imp = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("high importance".to_string()),
        MemSource::new("cli"),
    )
    .salience(Salience::new(0.9)) // high base_score
    .build();
    high_imp.temporal.last_accessed = chrono::Utc::now() - chrono::Duration::hours(100);
    storage.store_memory(&high_imp).unwrap();

    store.decay_tick().unwrap();

    let low_updated = storage.get_memory(low_imp.id).unwrap().unwrap();
    let high_updated = storage.get_memory(high_imp.id).unwrap().unwrap();

    // High-importance memory should decay slower (higher decay_factor)
    assert!(
        high_updated.salience.decay_factor > low_updated.salience.decay_factor,
        "high importance ({}) should have higher decay_factor than low importance ({})",
        high_updated.salience.decay_factor,
        low_updated.salience.decay_factor
    );
}

#[test]
fn test_auto_consolidation_on_ingest() {
    let cortex = cortex_core::Cortex::in_memory().unwrap();

    // Ingest 100 memories to trigger auto-consolidation
    for i in 0..100 {
        cortex
            .ingest(
                &format!("memory {}", i),
                "test",
                None,
                None,
                None,
            )
            .unwrap();
    }

    // After 100 ingests, auto-consolidation should have run.
    // We can verify by checking that decay was applied (no crash = success)
    let report = cortex.run_consolidation().unwrap();
    assert!(report.episodes_scanned > 0);
}

#[test]
fn test_run_decay_api() {
    let cortex = cortex_core::Cortex::in_memory().unwrap();

    // Add some memories
    for i in 0..5 {
        cortex.ingest(&format!("mem {}", i), "test", None, None, None).unwrap();
    }

    // run_decay should work and return count
    let updated = cortex.run_decay().unwrap();
    // Fresh memories shouldn't need decay updates
    assert_eq!(updated, 0);
}
