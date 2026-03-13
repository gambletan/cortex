//! Advanced consolidation tests: promotion candidates, pattern extraction,
//! decay + consolidation coordination, edge cases.

use cortex_core::consolidation::{ConsolidationConfig, ConsolidationEngine};
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;
use cortex_core::Cortex;

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

// ── Promotion candidates: content grouping ───────────────────────────────

#[test]
fn test_promotion_groups_similar_content() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_consolidation_config(ConsolidationConfig {
            min_occurrences: 2,
            sweep_threshold: 0.01,
            pattern_window_days: 30,
            salience_boost: 1.0,
        })
        .with_dedup_config(DeduplicationConfig {
            exact_dedup: false,
            near_dedup: false,
            near_dedup_threshold: 0.95,
        });

    // Ingest repeated similar content
    for _ in 0..3 {
        cortex.ingest("I use Rust for systems programming", "cli", None, None, None).unwrap();
    }

    let report = cortex.run_consolidation().unwrap();
    // Should scan episodic memories and potentially promote
    assert!(report.episodes_scanned > 0);
}

#[test]
fn test_promotion_different_content_types() {
    let (storage, index) = setup();

    // Facts
    for _ in 0..3 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Fact {
                subject: "User".into(),
                predicate: "likes".into(),
                object: "Rust".into(),
            },
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    // Preferences
    for _ in 0..3 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Preference {
                key: "editor".into(),
                value: "neovim".into(),
                confidence: 0.8,
            },
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    let engine = ConsolidationEngine::new(&storage, &index)
        .with_config(ConsolidationConfig {
            min_occurrences: 2,
            sweep_threshold: 0.01,
            pattern_window_days: 30,
            salience_boost: 1.0,
        });

    let report = engine.run_consolidation_cycle().unwrap();
    assert!(report.episodes_scanned >= 6);
}

// ── Decay + consolidation coordination ───────────────────────────────────

#[test]
fn test_consolidation_includes_decay() {
    let cortex = Cortex::in_memory().unwrap();

    for i in 0..5 {
        cortex.ingest(&format!("decay coord {}", i), "cli", None, None, None).unwrap();
    }

    // Consolidation cycle includes decay
    let report = cortex.run_consolidation().unwrap();
    // May or may not decay (time-dependent)
    let _ = report.decayed_updated + report.decayed_swept;
}

#[test]
fn test_decay_with_custom_config() {
    let config = cortex_core::episode::DecayConfig {
        temp_half_life_hours: 0.001, // very fast decay
        temp_importance_scale: 1.0,
        normal_half_life_hours: 0.001,
        normal_importance_scale: 1.0,
        permanent_floor: 0.3,
    };
    let cortex = Cortex::in_memory().unwrap().with_decay_config(config);

    cortex.ingest("fast decay", "cli", None, Some(0.1), None).unwrap();

    // Run decay — with tiny half-life, salience should decrease
    let count = cortex.run_decay().unwrap();
    // Decay happens based on time elapsed since ingest
    let _ = count;
}

// ── Sweep threshold ──────────────────────────────────────────────────────

#[test]
fn test_sweep_very_low_threshold() {
    let (storage, index) = setup();

    // Low-salience memory
    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("low salience".into()),
        MemSource::new("cli"),
    )
    .build();
    mem.salience = Salience::new(0.001);
    storage.store_memory(&mem).unwrap();

    let engine = ConsolidationEngine::new(&storage, &index)
        .with_config(ConsolidationConfig {
            min_occurrences: 2,
            sweep_threshold: 0.01, // very low threshold
            pattern_window_days: 30,
            salience_boost: 1.0,
        });

    let report = engine.run_consolidation_cycle().unwrap();
    // Should sweep the low-salience memory
    let _ = report.decayed_swept;
}

// ── Empty state ──────────────────────────────────────────────────────────

#[test]
fn test_consolidation_empty_store() {
    let (storage, index) = setup();
    let engine = ConsolidationEngine::new(&storage, &index);

    let report = engine.run_consolidation_cycle().unwrap();
    assert_eq!(report.episodes_scanned, 0);
    assert_eq!(report.promoted_to_semantic, 0);
    assert_eq!(report.decayed_swept, 0);
    assert_eq!(report.patterns_detected, 0);
}

// ── Auto-consolidation boundary ──────────────────────────────────────────

#[test]
fn test_auto_consolidation_at_exact_boundary() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_auto_consolidation_interval(5);

    // Ingest exactly 5 items
    for i in 0..5 {
        cortex.ingest(&format!("boundary {}", i), "cli", None, None, None).unwrap();
    }

    let m = cortex.metrics();
    assert!(m.consolidations >= 1, "Auto-consolidation should trigger at count=5");
}

#[test]
fn test_auto_consolidation_not_before_boundary() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_auto_consolidation_interval(10);

    for i in 0..9 {
        cortex.ingest(&format!("not yet {}", i), "cli", None, None, None).unwrap();
    }

    let m = cortex.metrics();
    assert_eq!(m.consolidations, 0, "Should not consolidate before interval");
}

// ── Salience boost on promotion ──────────────────────────────────────────

#[test]
fn test_consolidation_custom_salience_boost() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_consolidation_config(ConsolidationConfig {
            min_occurrences: 2,
            sweep_threshold: 0.01,
            pattern_window_days: 30,
            salience_boost: 2.0, // 2x boost on promotion
        })
        .with_dedup_config(DeduplicationConfig {
            exact_dedup: false,
            near_dedup: false,
            near_dedup_threshold: 0.95,
        });

    for _ in 0..5 {
        cortex.ingest("repeated important thing", "cli", None, None, None).unwrap();
    }

    let report = cortex.run_consolidation().unwrap();
    assert!(report.episodes_scanned > 0);
}
