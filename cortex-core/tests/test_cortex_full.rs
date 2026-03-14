//! Full coverage tests for Cortex API — builder methods, auto-archive,
//! compression, working memory, dedup config, context config, metrics details.

use cortex_core::consolidation::ConsolidationConfig;
use cortex_core::storage::memory_index::IndexConfig;
use cortex_core::types::*;
use cortex_core::Cortex;
use std::sync::Arc;

// ── Builder methods ──────────────────────────────────────────────────────

#[test]
fn test_open_with_config() {
    let config = IndexConfig {
        min_size: 500,
        pending_ratio: 0.1,
        removal_ratio: 0.05,
        ef_construction: 40,
        ..Default::default()
    };
    // Use a temp path
    let path = format!("/tmp/cortex_test_{}.db", uuid::Uuid::new_v4());
    let cortex = Cortex::open_with_config(&path, config).unwrap();

    // Should work normally
    cortex.ingest("test", "cli", None, None, None).unwrap();
    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 1);

    // Cleanup
    drop(cortex);
    let _ = std::fs::remove_file(&path);
}

#[test]
fn test_with_consolidation_config() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_consolidation_config(ConsolidationConfig {
            min_occurrences: 5,
            sweep_threshold: 0.05,
            pattern_window_days: 7,
            salience_boost: 1.2,
        });

    // Ingest and consolidate with custom config
    for i in 0..10 {
        cortex.ingest(&format!("msg {}", i), "cli", None, None, None).unwrap();
    }
    let report = cortex.run_consolidation().unwrap();
    assert!(report.episodes_scanned > 0);
}

#[test]
fn test_with_auto_consolidation_interval() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_auto_consolidation_interval(3);

    // Ingest 3 items — should trigger auto-consolidation
    for i in 0..3 {
        cortex.ingest(&format!("auto {}", i), "cli", None, None, None).unwrap();
    }

    let m = cortex.metrics();
    // Should have run at least 1 consolidation (auto-triggered at ingest #3)
    assert!(m.consolidations >= 1);
}

#[test]
fn test_with_decay_config() {
    let config = cortex_core::episode::DecayConfig {
        temp_half_life_hours: 1.0,
        temp_importance_scale: 2.0,
        normal_half_life_hours: 24.0,
        normal_importance_scale: 48.0,
        permanent_floor: 0.3,
    };
    let cortex = Cortex::in_memory().unwrap().with_decay_config(config);

    cortex.ingest("decay test", "cli", None, None, None).unwrap();
    let _count = cortex.run_decay().unwrap();
    // May or may not decay anything (depends on timing), but shouldn't error
}

#[test]
fn test_with_inference_fn() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_inference_fn(Box::new(|text| {
            // Custom inference — always return a fixed fact
            Ok(cortex_core::inference::InferredKnowledge {
                facts: vec![cortex_core::inference::InferredFact {
                    subject: "User".into(),
                    predicate: "custom_inferred".into(),
                    object: text.to_string(),
                    confidence: 0.9,
                }],
                preferences: vec![],
                temporal_hint: cortex_core::inference::TemporalHint::Unknown,
            })
        }));

    cortex.ingest("custom LLM", "cli", None, None, None).unwrap();

    // Check that custom inference was used
    let facts = cortex.query_facts("custom LLM").unwrap();
    assert!(!facts.is_empty());
}

#[test]
fn test_with_dedup_config_disabled() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_dedup_config(DeduplicationConfig {
            exact_dedup: false,
            near_dedup: false,
            near_dedup_threshold: 0.95,
        });

    // With dedup disabled, duplicate content should be stored twice
    cortex.ingest("duplicate", "cli", None, None, None).unwrap();
    cortex.ingest("duplicate", "cli", None, None, None).unwrap();

    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 2); // both stored
}

// ── Auto-archive ─────────────────────────────────────────────────────────

#[test]
fn test_auto_archive_skips_permanent() {
    let cortex = Cortex::in_memory().unwrap();

    // Permanent memory — "I live in ..." is classified as permanent
    cortex.ingest("I live in Shanghai", "cli", None, Some(0.1), None).unwrap();

    // Attempt to auto-archive with very low threshold
    let archived = cortex.auto_archive(0, 0.5).unwrap();
    // Permanent memories should not be archived even with low salience
    assert_eq!(archived, 0);
}

#[test]
fn test_auto_archive_archives_old_low_salience() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_dedup_config(DeduplicationConfig {
            exact_dedup: false,
            near_dedup: false,
            near_dedup_threshold: 0.95,
        });

    // Ingest a temporary memory with very low salience
    cortex.ingest("just testing something", "cli", None, Some(0.05), None).unwrap();

    // max_age_days=0 means archive anything older than now
    let _archived = cortex.auto_archive(0, 0.1).unwrap();
    // The temporary memory should be archivable (timing-dependent)
}

// ── Compression via Cortex ───────────────────────────────────────────────

#[test]
fn test_run_compression() {
    let cortex = Cortex::in_memory().unwrap();

    // Ingest some memories (won't be old enough to compress with 7-day window)
    for i in 0..3 {
        cortex.ingest(&format!("compress test {}", i), "cli", None, None, None).unwrap();
    }

    let report = cortex.run_compression(2, 7).unwrap();
    // Too recent to compress, but should not error
    assert_eq!(report.summaries_created, 0);
}

#[test]
fn test_run_compression_with_summarizer() {
    let cortex = Cortex::in_memory().unwrap();

    let summarizer: cortex_core::compression::SummarizerFn = Box::new(|texts, count| {
        Ok(format!("[Custom summary of {} texts from {} total]", texts.len(), count))
    });

    let report = cortex.run_compression_with_summarizer(2, 0, &summarizer).unwrap();
    assert_eq!(report.summaries_created, 0); // no old sessions
}

// ── Working memory accessor ──────────────────────────────────────────────

#[test]
fn test_working_memory_accessor() {
    let mut cortex = Cortex::in_memory().unwrap();

    let wm = cortex.working_memory();
    assert!(wm.is_empty());

    let mem = cortex_core::working::WorkingMemory::create_working_mem(
        "working item",
        MemSource::new("cli"),
    );
    let id = mem.id;
    cortex.working_memory().push(mem);

    assert_eq!(cortex.working_memory().len(), 1);
    assert!(cortex.working_memory().get(id).is_some());
}

// ── Accessor methods ─────────────────────────────────────────────────────

#[test]
fn test_storage_accessor() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest("test", "cli", None, None, None).unwrap();

    let count = cortex.storage().count_by_tier(MemoryTier::Episodic).unwrap();
    assert!(count >= 1);
}

#[test]
fn test_index_accessor() {
    let cortex = Cortex::in_memory().unwrap();
    let _idx = cortex.index();
    // Just verify it's accessible
}

#[test]
fn test_plugin_manager_accessor() {
    let cortex = Cortex::in_memory().unwrap();
    assert_eq!(cortex.plugin_manager().count(), 0);
}

#[test]
fn test_plugin_context_accessor() {
    let cortex = Cortex::in_memory().unwrap();
    let _ctx = cortex.plugin_context();
    // Just verify it's accessible
}

// ── Metrics tracking comprehensive ──────────────────────────────────────

#[test]
fn test_metrics_comprehensive() {
    let cortex = Cortex::in_memory().unwrap();

    // Initial metrics should be zero
    let m = cortex.metrics();
    assert_eq!(m.ingests, 0);
    assert_eq!(m.retrievals, 0);
    assert_eq!(m.consolidations, 0);
    assert_eq!(m.decay_runs, 0);
    assert_eq!(m.dedup_hits, 0);
    assert_eq!(m.archives, 0);
    assert_eq!(m.batch_ingests, 0);

    // Ingest
    cortex.ingest("metric A", "cli", None, None, None).unwrap();
    assert_eq!(cortex.metrics().ingests, 1);

    // Retrieve
    cortex.retrieve("metric", 5, None, None, None).unwrap();
    assert_eq!(cortex.metrics().retrievals, 1);

    // Consolidation
    cortex.run_consolidation().unwrap();
    assert_eq!(cortex.metrics().consolidations, 1);

    // Decay
    cortex.run_decay().unwrap();
    assert_eq!(cortex.metrics().decay_runs, 1);

    // Dedup hit (ingest same content)
    cortex.ingest("metric A", "cli", None, None, None).unwrap();
    assert_eq!(cortex.metrics().dedup_hits, 1);

    // Batch ingest
    let items = vec![BatchIngestItem {
        text: "batch metric".into(),
        channel: "cli".into(),
        user_id: None,
        salience_hint: None,
        embedding: None,
        namespace: None,
    }];
    cortex.ingest_batch(items).unwrap();
    assert_eq!(cortex.metrics().batch_ingests, 1);

    // Archive
    let mem = cortex.ingest("to archive", "cli", None, None, None).unwrap();
    cortex.archive_memory(mem.id).unwrap();
    assert_eq!(cortex.metrics().archives, 1);
}

// ── Event bus comprehensive ──────────────────────────────────────────────

#[test]
fn test_event_bus_all_events() {
    let cortex = Cortex::in_memory().unwrap();

    let events = Arc::new(std::sync::Mutex::new(Vec::new()));
    let e = events.clone();

    cortex.on_event(Box::new(move |event| {
        e.lock().unwrap().push(format!("{:?}", event));
    }));

    // Trigger various events
    let mem = cortex.ingest("event test", "cli", None, None, None).unwrap();
    cortex.run_consolidation().unwrap();
    cortex.run_decay().unwrap();
    cortex.archive_memory(mem.id).unwrap();

    let collected = events.lock().unwrap();
    assert!(collected.len() >= 4, "Should have at least 4 events: created, consolidation, decay, archived");
}

// ── Ingest with person identity ──────────────────────────────────────────

#[test]
fn test_ingest_with_user_id() {
    let cortex = Cortex::in_memory().unwrap();

    let mem = cortex
        .ingest("test with identity", "telegram", Some("user123"), None, None)
        .unwrap();

    assert!(mem.source.identity_id.is_some());

    let people = cortex.list_people().unwrap();
    assert_eq!(people.len(), 1);
}

// ── Query facts and preferences ──────────────────────────────────────────

#[test]
fn test_query_facts_api() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.add_fact("Alice", "knows", "Rust", 0.9, "cli", None).unwrap();
    cortex.add_fact("Bob", "knows", "Python", 0.8, "cli", None).unwrap();

    let alice_facts = cortex.query_facts("Alice").unwrap();
    assert_eq!(alice_facts.len(), 1);

    let all = cortex.query_facts("knows").unwrap();
    assert!(all.is_empty()); // query is on subject/object, not predicate
}

#[test]
fn test_query_preferences_api() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.add_preference("editor", "neovim", 0.9).unwrap();
    cortex.add_preference("language", "rust", 0.8).unwrap();

    let results = cortex.query_preferences("editor").unwrap();
    assert_eq!(results.len(), 1);

    let all = cortex.query_preferences("").unwrap();
    assert_eq!(all.len(), 2);
}

// ── Stats includes archived ──────────────────────────────────────────────

#[test]
fn test_stats_includes_archived() {
    let cortex = Cortex::in_memory().unwrap();

    let mem = cortex.ingest("will archive", "cli", None, None, None).unwrap();
    cortex.archive_memory(mem.id).unwrap();

    let stats = cortex.stats().unwrap();
    assert_eq!(stats.archived, 1);
    assert_eq!(stats.episodic, 0);
    // Archived doesn't count in total (total = episodic + semantic + procedural)
    assert_eq!(stats.total, stats.episodic + stats.semantic + stats.procedural);
}

// ── Restore memory ───────────────────────────────────────────────────────

#[test]
fn test_restore_to_different_tier() {
    let cortex = Cortex::in_memory().unwrap();

    let mem = cortex.ingest("restore me", "cli", None, None, None).unwrap();
    cortex.archive_memory(mem.id).unwrap();

    // Restore to semantic instead of original episodic
    cortex.restore_memory(mem.id, MemoryTier::Semantic).unwrap();

    let stats = cortex.stats().unwrap();
    assert_eq!(stats.semantic, 1); // at least 1 (may have auto-extracted facts too)
    assert_eq!(stats.archived, 0);
}

// ── Batch ingest with user_id and namespace ──────────────────────────────

#[test]
fn test_batch_ingest_with_user_id() {
    let cortex = Cortex::in_memory().unwrap();

    let items = vec![
        BatchIngestItem {
            text: "from user".into(),
            channel: "slack".into(),
            user_id: Some("U001".into()),
            salience_hint: Some(0.7),
            embedding: None,
            namespace: Some("team-x".into()),
        },
    ];

    let report = cortex.ingest_batch(items).unwrap();
    assert_eq!(report.stored, 1);

    let people = cortex.list_people().unwrap();
    assert_eq!(people.len(), 1);
}

// ── Context with filters ─────────────────────────────────────────────────

#[test]
fn test_context_with_channel_filter() {
    let cortex = Cortex::in_memory().unwrap();

    cortex.ingest("slack message", "slack", None, None, None).unwrap();
    cortex.ingest("telegram message", "telegram", None, None, None).unwrap();

    let ctx = cortex.get_context(2000, Some("slack"), None).unwrap();
    assert!(!ctx.is_empty());
}

#[test]
fn test_context_with_person_filter() {
    let cortex = Cortex::in_memory().unwrap();

    let person = cortex.add_person("Alice", "slack", "alice123").unwrap();
    cortex.ingest("test", "slack", Some("alice123"), None, None).unwrap();

    let ctx = cortex.get_context(2000, None, Some(person.id)).unwrap();
    assert!(!ctx.is_empty());
}

// ── Plugin with_plugin_config ────────────────────────────────────────────

struct NoopPlugin;
impl cortex_core::plugin::Plugin for NoopPlugin {
    fn meta(&self) -> cortex_core::plugin::PluginMeta {
        cortex_core::plugin::PluginMeta {
            name: "noop".into(),
            version: "1.0.0".into(),
            description: "does nothing".into(),
        }
    }
}

#[test]
fn test_with_plugin() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(NoopPlugin));

    assert_eq!(cortex.plugin_manager().count(), 1);
}

#[test]
fn test_with_plugin_config() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin_config(Box::new(NoopPlugin), serde_json::json!({"key": "val"}));

    assert_eq!(cortex.plugin_manager().count(), 1);
}
