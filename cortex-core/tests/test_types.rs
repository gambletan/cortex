//! Tests for types, builders, enums, and utility functions.

use cortex_core::types::*;
use chrono::Utc;

// ── MemoryTier ───────────────────────────────────────────────────────────

#[test]
fn test_memory_tier_parse_all() {
    assert_eq!(MemoryTier::parse("working"), Some(MemoryTier::Working));
    assert_eq!(MemoryTier::parse("episodic"), Some(MemoryTier::Episodic));
    assert_eq!(MemoryTier::parse("semantic"), Some(MemoryTier::Semantic));
    assert_eq!(MemoryTier::parse("procedural"), Some(MemoryTier::Procedural));
    assert_eq!(MemoryTier::parse("archived"), Some(MemoryTier::Archived));
    assert_eq!(MemoryTier::parse("invalid"), None);
    assert_eq!(MemoryTier::parse(""), None);
}

#[test]
fn test_memory_tier_as_str_roundtrip() {
    for tier in &[
        MemoryTier::Working,
        MemoryTier::Episodic,
        MemoryTier::Semantic,
        MemoryTier::Procedural,
        MemoryTier::Archived,
    ] {
        assert_eq!(MemoryTier::parse(tier.as_str()), Some(*tier));
    }
}

// ── MemoryDurability ─────────────────────────────────────────────────────

#[test]
fn test_durability_parse() {
    assert!(matches!(MemoryDurability::parse("normal"), MemoryDurability::Normal));
    assert!(matches!(MemoryDurability::parse("temporary"), MemoryDurability::Temporary));
    assert!(matches!(MemoryDurability::parse("permanent"), MemoryDurability::Permanent));
    assert!(matches!(MemoryDurability::parse("unknown"), MemoryDurability::Normal));
    assert!(matches!(MemoryDurability::parse(""), MemoryDurability::Normal));
}

#[test]
fn test_durability_as_str_roundtrip() {
    for d in &[
        MemoryDurability::Normal,
        MemoryDurability::Temporary,
        MemoryDurability::Permanent,
    ] {
        assert_eq!(MemoryDurability::parse(d.as_str()).as_str(), d.as_str());
    }
}

// ── LinkRelation ─────────────────────────────────────────────────────────

#[test]
fn test_link_relation_parse_all() {
    assert_eq!(LinkRelation::parse("related_to"), Some(LinkRelation::RelatedTo));
    assert_eq!(LinkRelation::parse("supports"), Some(LinkRelation::Supports));
    assert_eq!(LinkRelation::parse("contradicts"), Some(LinkRelation::Contradicts));
    assert_eq!(LinkRelation::parse("supersedes"), Some(LinkRelation::Supersedes));
    assert_eq!(LinkRelation::parse("part_of"), Some(LinkRelation::PartOf));
    assert_eq!(LinkRelation::parse("caused_by"), Some(LinkRelation::CausedBy));
    assert_eq!(LinkRelation::parse("leads_to"), Some(LinkRelation::LeadsTo));
    assert_eq!(LinkRelation::parse("invalid"), None);
}

#[test]
fn test_link_relation_roundtrip() {
    for rel in &[
        LinkRelation::RelatedTo,
        LinkRelation::Supports,
        LinkRelation::Contradicts,
        LinkRelation::Supersedes,
        LinkRelation::PartOf,
        LinkRelation::CausedBy,
        LinkRelation::LeadsTo,
    ] {
        assert_eq!(LinkRelation::parse(rel.as_str()), Some(*rel));
    }
}

// ── Salience ─────────────────────────────────────────────────────────────

#[test]
fn test_salience_new() {
    let s = Salience::new(0.7);
    assert!((s.base_score - 0.7).abs() < 0.01);
    assert!((s.emotional_weight - 1.0).abs() < 0.01);
    assert!((s.access_boost - 1.0).abs() < 0.01);
    assert!((s.decay_factor - 1.0).abs() < 0.01);
    assert!((s.effective_score - 0.7).abs() < 0.01);
}

#[test]
fn test_salience_recompute() {
    let mut s = Salience::new(0.5);
    s.emotional_weight = 2.0;
    s.access_boost = 1.5;
    s.decay_factor = 0.8;
    s.recompute();
    // 0.5 * 2.0 * 1.5 * 0.8 = 1.2
    assert!((s.effective_score - 1.2).abs() < 0.01);
}

#[test]
fn test_salience_default() {
    let s = Salience::default();
    assert!((s.base_score - 0.5).abs() < 0.01);
}

// ── TemporalInfo ─────────────────────────────────────────────────────────

#[test]
fn test_temporal_info_now() {
    let t = TemporalInfo::now();
    assert!(t.event_time.is_none());
    assert_eq!(t.access_count, 0);
    assert!(matches!(t.durability, MemoryDurability::Normal));
    // Should be close to now
    let diff = Utc::now() - t.ingestion_time;
    assert!(diff.num_seconds() < 2);
}

// ── MemSource ────────────────────────────────────────────────────────────

#[test]
fn test_mem_source_new() {
    let s = MemSource::new("telegram");
    assert_eq!(s.channel, "telegram");
    assert!(s.identity_id.is_none());
    assert!(s.chat_id.is_none());
    assert!(s.thread_id.is_none());
    assert!(s.message_id.is_none());
}

// ── MemObjectBuilder ─────────────────────────────────────────────────────

#[test]
fn test_builder_default() {
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("test"),
    )
    .build();

    assert_eq!(mem.tier, MemoryTier::Episodic);
    assert!(matches!(mem.privacy, PrivacyLevel::Private));
    assert!(mem.tags.is_empty());
    assert!(mem.metadata.is_empty());
    assert!(mem.links.is_empty());
    assert!(mem.content_hash.is_some());
    assert!(mem.namespace.is_none());
    assert!(mem.embedding.is_none());
}

#[test]
fn test_builder_all_fields() {
    let emb = vec![0.1, 0.2, 0.3];
    let now = Utc::now();

    let mem = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "Alice".into(),
            predicate: "likes".into(),
            object: "Rust".into(),
        },
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .salience(Salience::new(0.9))
    .privacy(PrivacyLevel::Public)
    .tags(vec!["tech".into(), "fact".into()])
    .event_time(now)
    .durability(MemoryDurability::Permanent)
    .namespace("team-x")
    .meta("source".to_string(), serde_json::json!("manual"))
    .build();

    assert_eq!(mem.tier, MemoryTier::Semantic);
    assert_eq!(mem.embedding.unwrap().len(), 3);
    assert!((mem.salience.base_score - 0.9).abs() < 0.01);
    assert!(matches!(mem.privacy, PrivacyLevel::Public));
    assert_eq!(mem.tags, vec!["tech", "fact"]);
    assert_eq!(mem.temporal.event_time, Some(now));
    assert!(matches!(mem.temporal.durability, MemoryDurability::Permanent));
    assert_eq!(mem.namespace.as_deref(), Some("team-x"));
    assert!(mem.metadata.contains_key("source"));
    assert!(mem.content_hash.is_some());
}

#[test]
fn test_builder_privacy_shared() {
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("shared".into()),
        MemSource::new("test"),
    )
    .privacy(PrivacyLevel::Shared {
        scope: "team-alpha".into(),
    })
    .build();

    match &mem.privacy {
        PrivacyLevel::Shared { scope } => assert_eq!(scope, "team-alpha"),
        _ => panic!("expected Shared privacy"),
    }
}

// ── compute_content_hash ─────────────────────────────────────────────────

#[test]
fn test_content_hash_text() {
    let h = compute_content_hash(&MemContent::Text("hello".into()));
    assert_eq!(h.len(), 64);
}

#[test]
fn test_content_hash_fact() {
    let h = compute_content_hash(&MemContent::Fact {
        subject: "A".into(),
        predicate: "B".into(),
        object: "C".into(),
    });
    assert_eq!(h.len(), 64);
}

#[test]
fn test_content_hash_preference() {
    let h = compute_content_hash(&MemContent::Preference {
        key: "lang".into(),
        value: "rust".into(),
        confidence: 0.9,
    });
    assert_eq!(h.len(), 64);
}

#[test]
fn test_content_hash_relationship() {
    let h = compute_content_hash(&MemContent::Relationship {
        person_a: uuid::Uuid::new_v4(),
        person_b: uuid::Uuid::new_v4(),
        relation: "friend".into(),
    });
    assert_eq!(h.len(), 64);
}

#[test]
fn test_content_hash_pattern() {
    let h = compute_content_hash(&MemContent::Pattern {
        trigger: "morning".into(),
        actions: vec!["coffee".into()],
        frequency: 5,
    });
    assert_eq!(h.len(), 64);
}

#[test]
fn test_content_hash_event() {
    let h = compute_content_hash(&MemContent::Event {
        title: "meeting".into(),
        start: Utc::now(),
        end: None,
    });
    assert_eq!(h.len(), 64);
}

#[test]
fn test_content_hash_deterministic() {
    let c = MemContent::Text("same content".into());
    assert_eq!(compute_content_hash(&c), compute_content_hash(&c));
}

#[test]
fn test_content_hash_different_content() {
    let h1 = compute_content_hash(&MemContent::Text("a".into()));
    let h2 = compute_content_hash(&MemContent::Text("b".into()));
    assert_ne!(h1, h2);
}

// ── DeduplicationConfig ──────────────────────────────────────────────────

#[test]
fn test_dedup_config_default() {
    let cfg = DeduplicationConfig::default();
    assert!(cfg.exact_dedup);
    assert!(!cfg.near_dedup);
    assert!((cfg.near_dedup_threshold - 0.95).abs() < 0.01);
}

// ── CortexEvent ──────────────────────────────────────────────────────────

#[test]
fn test_cortex_event_variants() {
    let id = uuid::Uuid::new_v4();

    // Just verify all variants can be constructed (compile-time check)
    let _e1 = CortexEvent::MemoryCreated { id, tier: MemoryTier::Episodic };
    let _e2 = CortexEvent::MemoryUpdated { id };
    let _e3 = CortexEvent::MemoryDeleted { id };
    let _e4 = CortexEvent::MemoryArchived { id };
    let _e5 = CortexEvent::ConsolidationCompleted { report: "test".into() };
    let _e6 = CortexEvent::DecayCompleted { count: 5 };
}

// ── BatchIngestReport ────────────────────────────────────────────────────

#[test]
fn test_batch_ingest_report_default() {
    let r = BatchIngestReport::default();
    assert_eq!(r.total_submitted, 0);
    assert_eq!(r.stored, 0);
    assert_eq!(r.deduplicated, 0);
    assert_eq!(r.skipped_by_plugin, 0);
}

// ── Serialization round-trips ────────────────────────────────────────────

#[test]
fn test_mem_object_serde_roundtrip() {
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("serde test".into()),
        MemSource::new("test"),
    )
    .namespace("ns1")
    .tags(vec!["tag1".into()])
    .build();

    let json = serde_json::to_string(&mem).unwrap();
    let deserialized: MemObject = serde_json::from_str(&json).unwrap();

    assert_eq!(deserialized.id, mem.id);
    assert_eq!(deserialized.tier, mem.tier);
    assert_eq!(deserialized.content_hash, mem.content_hash);
    assert_eq!(deserialized.namespace, mem.namespace);
    assert_eq!(deserialized.tags, mem.tags);
}

#[test]
fn test_mem_object_serde_backwards_compat() {
    // JSON without content_hash and namespace (old format) should deserialize
    let json = r#"{
        "id": "00000000-0000-0000-0000-000000000001",
        "tier": "Episodic",
        "content": {"Text": "old format"},
        "embedding": null,
        "temporal": {
            "event_time": null,
            "ingestion_time": "2024-01-01T00:00:00Z",
            "last_accessed": "2024-01-01T00:00:00Z",
            "access_count": 0,
            "relevance_schedule": null,
            "durability": "Normal"
        },
        "source": {"channel": "test", "identity_id": null, "chat_id": null, "thread_id": null, "message_id": null},
        "salience": {"base_score": 0.5, "emotional_weight": 1.0, "access_boost": 1.0, "decay_factor": 1.0, "effective_score": 0.5},
        "privacy": "Private",
        "links": [],
        "tags": [],
        "metadata": {}
    }"#;

    let mem: MemObject = serde_json::from_str(json).unwrap();
    assert!(mem.content_hash.is_none());
    assert!(mem.namespace.is_none());
}
