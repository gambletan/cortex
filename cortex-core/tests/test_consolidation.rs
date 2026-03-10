use cortex_core::consolidation::ConsolidationEngine;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;

fn setup() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

#[test]
fn test_promote_to_semantic() {
    let storage = setup();
    let engine = ConsolidationEngine::new(&storage);

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
    let storage = setup();
    let engine = ConsolidationEngine::new(&storage);

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
    let storage = setup();
    let engine = ConsolidationEngine::new(&storage);

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
    let storage = setup();
    let engine = ConsolidationEngine::new(&storage);

    let result = engine.promote_to_semantic(vec![]);
    assert!(result.is_err());
}

#[test]
fn test_sweep_no_decayed() {
    let storage = setup();
    let engine = ConsolidationEngine::new(&storage);

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
    let storage = setup();
    let engine = ConsolidationEngine::new(&storage);

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
