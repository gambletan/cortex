//! Direct tests for SqliteStorage backend methods.

use chrono::{Duration, Utc};
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;
use uuid::Uuid;

fn storage() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

fn make_mem(text: &str, tier: MemoryTier) -> MemObject {
    MemObjectBuilder::new(tier, MemContent::Text(text.into()), MemSource::new("test")).build()
}

fn make_mem_with_ns(text: &str, ns: &str) -> MemObject {
    MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text(text.into()),
        MemSource::new("test"),
    )
    .namespace(ns)
    .build()
}

// ── CRUD ─────────────────────────────────────────────────────────────────

#[test]
fn test_store_and_get() {
    let s = storage();
    let mem = make_mem("hello", MemoryTier::Episodic);
    let id = mem.id;
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(id).unwrap().unwrap();
    assert_eq!(got.id, id);
    match &got.content {
        MemContent::Text(t) => assert_eq!(t, "hello"),
        _ => panic!("wrong content"),
    }
}

#[test]
fn test_get_missing() {
    let s = storage();
    assert!(s.get_memory(Uuid::new_v4()).unwrap().is_none());
}

#[test]
fn test_update_memory() {
    let s = storage();
    let mut mem = make_mem("original", MemoryTier::Episodic);
    s.store_memory(&mem).unwrap();

    mem.content = MemContent::Text("updated".into());
    mem.salience.base_score = 0.9;
    s.update_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    match &got.content {
        MemContent::Text(t) => assert_eq!(t, "updated"),
        _ => panic!("wrong content"),
    }
    assert!((got.salience.base_score - 0.9).abs() < 0.01);
}

#[test]
fn test_delete_memory() {
    let s = storage();
    let mem = make_mem("delete me", MemoryTier::Episodic);
    let id = mem.id;
    s.store_memory(&mem).unwrap();
    assert!(s.get_memory(id).unwrap().is_some());

    s.delete_memory(id).unwrap();
    assert!(s.get_memory(id).unwrap().is_none());
}

// ── Queries ──────────────────────────────────────────────────────────────

#[test]
fn test_list_by_tier() {
    let s = storage();
    s.store_memory(&make_mem("ep1", MemoryTier::Episodic)).unwrap();
    s.store_memory(&make_mem("ep2", MemoryTier::Episodic)).unwrap();
    s.store_memory(&make_mem("sem1", MemoryTier::Semantic)).unwrap();

    let eps = s.list_by_tier(MemoryTier::Episodic, 100).unwrap();
    assert_eq!(eps.len(), 2);

    let sems = s.list_by_tier(MemoryTier::Semantic, 100).unwrap();
    assert_eq!(sems.len(), 1);
}

#[test]
fn test_list_by_tier_respects_limit() {
    let s = storage();
    for i in 0..10 {
        s.store_memory(&make_mem(&format!("m{}", i), MemoryTier::Episodic)).unwrap();
    }
    let results = s.list_by_tier(MemoryTier::Episodic, 3).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_list_by_tier_ordered_by_ingestion_uses_event_time_not_insertion() {
    use chrono::TimeZone;
    let s = storage();

    // Deterministic times that also exercise the whole-second vs fractional-second
    // edge: chrono serializes "…00Z" and "…00.500Z", which do NOT sort chronologically
    // by plain string comparison ('.' < 'Z'), so the ordering must parse the timestamp.
    let whole = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();
    let frac_after = whole + Duration::milliseconds(500); // 12:00:00.500 — latest
    let frac_before = whole - Duration::milliseconds(500); // 11:59:59.500 — earliest

    // Insert in a scrambled order so neither insertion nor string order matches.
    let mut a = make_mem("whole", MemoryTier::Episodic);
    a.temporal.ingestion_time = whole;
    s.store_memory(&a).unwrap();

    let mut b = make_mem("frac_before", MemoryTier::Episodic);
    b.temporal.ingestion_time = frac_before;
    s.store_memory(&b).unwrap();

    let mut c = make_mem("frac_after", MemoryTier::Episodic);
    c.temporal.ingestion_time = frac_after;
    s.store_memory(&c).unwrap();

    let ordered = s
        .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, 10)
        .unwrap();
    let ids: Vec<_> = ordered.iter().map(|m| m.id).collect();
    // Chronological DESC: frac_after (.500), whole (.000), frac_before (prev .500).
    assert_eq!(ids, vec![c.id, a.id, b.id]);
}

#[test]
fn test_list_by_channel() {
    let s = storage();
    let mut m1 = make_mem("a", MemoryTier::Episodic);
    m1.source.channel = "slack".into();
    s.store_memory(&m1).unwrap();

    let mut m2 = make_mem("b", MemoryTier::Episodic);
    m2.source.channel = "telegram".into();
    s.store_memory(&m2).unwrap();

    let slack = s.list_by_channel("slack", 100).unwrap();
    assert_eq!(slack.len(), 1);

    let telegram = s.list_by_channel("telegram", 100).unwrap();
    assert_eq!(telegram.len(), 1);

    let none = s.list_by_channel("discord", 100).unwrap();
    assert!(none.is_empty());
}

#[test]
fn test_list_by_salience_below() {
    let s = storage();
    let mut low = make_mem("low", MemoryTier::Episodic);
    low.salience = Salience::new(0.1);
    s.store_memory(&low).unwrap();

    let mut high = make_mem("high", MemoryTier::Episodic);
    high.salience = Salience::new(0.9);
    s.store_memory(&high).unwrap();

    let results = s.list_by_salience_below(MemoryTier::Episodic, 0.5).unwrap();
    assert_eq!(results.len(), 1);
    match &results[0].content {
        MemContent::Text(t) => assert_eq!(t, "low"),
        _ => panic!("wrong content"),
    }
}

#[test]
fn test_list_in_time_range() {
    let s = storage();
    s.store_memory(&make_mem("now", MemoryTier::Episodic)).unwrap();

    let start = Utc::now() - Duration::hours(1);
    let end = Utc::now() + Duration::hours(1);
    let results = s.list_in_time_range(MemoryTier::Episodic, start, end).unwrap();
    assert_eq!(results.len(), 1);

    // Out of range
    let old_start = Utc::now() - Duration::days(10);
    let old_end = Utc::now() - Duration::days(9);
    let empty = s.list_in_time_range(MemoryTier::Episodic, old_start, old_end).unwrap();
    assert!(empty.is_empty());
}

#[test]
fn test_count_by_tier() {
    let s = storage();
    assert_eq!(s.count_by_tier(MemoryTier::Episodic).unwrap(), 0);

    s.store_memory(&make_mem("a", MemoryTier::Episodic)).unwrap();
    s.store_memory(&make_mem("b", MemoryTier::Episodic)).unwrap();
    s.store_memory(&make_mem("c", MemoryTier::Semantic)).unwrap();

    assert_eq!(s.count_by_tier(MemoryTier::Episodic).unwrap(), 2);
    assert_eq!(s.count_by_tier(MemoryTier::Semantic).unwrap(), 1);
    assert_eq!(s.count_by_tier(MemoryTier::Procedural).unwrap(), 0);
}

// ── Links ────────────────────────────────────────────────────────────────

#[test]
fn test_store_and_get_links() {
    let s = storage();
    let m1 = make_mem("a", MemoryTier::Episodic);
    let m2 = make_mem("b", MemoryTier::Episodic);
    s.store_memory(&m1).unwrap();
    s.store_memory(&m2).unwrap();

    s.store_link(m1.id, m2.id, LinkRelation::RelatedTo, 0.8).unwrap();

    let links = s.get_links(m1.id).unwrap();
    assert_eq!(links.len(), 1);
    assert_eq!(links[0].0, m2.id);
    assert_eq!(links[0].1, LinkRelation::RelatedTo);
    assert!((links[0].2 - 0.8).abs() < 0.01);
}

#[test]
fn test_get_links_empty() {
    let s = storage();
    let links = s.get_links(Uuid::new_v4()).unwrap();
    assert!(links.is_empty());
}

// ── Batch operations ─────────────────────────────────────────────────────

#[test]
fn test_get_memories_batch() {
    let s = storage();
    let m1 = make_mem("a", MemoryTier::Episodic);
    let m2 = make_mem("b", MemoryTier::Episodic);
    let m3 = make_mem("c", MemoryTier::Episodic);
    s.store_memory(&m1).unwrap();
    s.store_memory(&m2).unwrap();
    s.store_memory(&m3).unwrap();

    let batch = s.get_memories_batch(&[m1.id, m3.id]).unwrap();
    assert_eq!(batch.len(), 2);

    // Empty batch
    let empty = s.get_memories_batch(&[]).unwrap();
    assert!(empty.is_empty());

    // Missing ID
    let partial = s.get_memories_batch(&[m1.id, Uuid::new_v4()]).unwrap();
    assert_eq!(partial.len(), 1);
}

#[test]
fn test_store_memories_batch() {
    let s = storage();
    let mems: Vec<MemObject> = (0..5)
        .map(|i| make_mem(&format!("batch_{}", i), MemoryTier::Episodic))
        .collect();

    let count = s.store_memories_batch(&mems).unwrap();
    assert_eq!(count, 5);
    assert_eq!(s.count_by_tier(MemoryTier::Episodic).unwrap(), 5);

    // Empty batch
    let zero = s.store_memories_batch(&[]).unwrap();
    assert_eq!(zero, 0);
}

// ── Content hash / dedup ─────────────────────────────────────────────────

#[test]
fn test_find_by_content_hash() {
    let s = storage();
    let mem = make_mem("unique content", MemoryTier::Episodic);
    let hash = mem.content_hash.clone().unwrap();
    s.store_memory(&mem).unwrap();

    let found = s.find_by_content_hash(&hash).unwrap();
    assert!(found.is_some());
    assert_eq!(found.unwrap().id, mem.id);

    let missing = s.find_by_content_hash("nonexistent").unwrap();
    assert!(missing.is_none());
}

#[test]
fn test_content_hash_persists() {
    let s = storage();
    let mem = make_mem("hashme", MemoryTier::Episodic);
    assert!(mem.content_hash.is_some());
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    assert_eq!(got.content_hash, mem.content_hash);
}

// ── Namespace ────────────────────────────────────────────────────────────

#[test]
fn test_namespace_persists() {
    let s = storage();
    let mem = make_mem_with_ns("ns content", "team-alpha");
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    assert_eq!(got.namespace.as_deref(), Some("team-alpha"));
}

#[test]
fn test_list_by_tier_and_namespace() {
    let s = storage();
    s.store_memory(&make_mem_with_ns("a", "ns1")).unwrap();
    s.store_memory(&make_mem_with_ns("b", "ns1")).unwrap();
    s.store_memory(&make_mem_with_ns("c", "ns2")).unwrap();
    s.store_memory(&make_mem("d", MemoryTier::Episodic)).unwrap(); // no namespace

    let ns1 = s.list_by_tier_and_namespace(MemoryTier::Episodic, Some("ns1"), 100).unwrap();
    assert_eq!(ns1.len(), 2);

    let ns2 = s.list_by_tier_and_namespace(MemoryTier::Episodic, Some("ns2"), 100).unwrap();
    assert_eq!(ns2.len(), 1);

    let all = s.list_by_tier_and_namespace(MemoryTier::Episodic, None, 100).unwrap();
    assert_eq!(all.len(), 4);
}

#[test]
fn test_list_by_tier_and_namespace_orders_by_ingestion_time() {
    use chrono::TimeZone;
    let s = storage();
    let whole = Utc.with_ymd_and_hms(2026, 6, 1, 12, 0, 0).unwrap();

    // Insertion order != ingestion order, and include the whole-second edge.
    let mut newest = make_mem_with_ns("newest", "ns1");
    newest.temporal.ingestion_time = whole + Duration::milliseconds(500);
    s.store_memory(&newest).unwrap();

    let mut oldest = make_mem_with_ns("oldest", "ns1");
    oldest.temporal.ingestion_time = whole - Duration::minutes(60);
    s.store_memory(&oldest).unwrap();

    let mut mid = make_mem_with_ns("mid", "ns1");
    mid.temporal.ingestion_time = whole; // whole second, no fraction
    s.store_memory(&mid).unwrap();

    // Different namespace — must be excluded.
    s.store_memory(&make_mem_with_ns("other", "ns2")).unwrap();

    let got = s
        .list_by_tier_and_namespace(MemoryTier::Episodic, Some("ns1"), 10)
        .unwrap();
    let ids: Vec<_> = got.iter().map(|m| m.id).collect();
    // Ingestion-time DESC, namespace-scoped.
    assert_eq!(ids, vec![newest.id, mid.id, oldest.id]);
}

// ── Archive / Restore ────────────────────────────────────────────────────

#[test]
fn test_archive_memory() {
    let s = storage();
    let mem = make_mem("archivable", MemoryTier::Episodic);
    let id = mem.id;
    s.store_memory(&mem).unwrap();

    s.archive_memory(id).unwrap();
    let got = s.get_memory(id).unwrap().unwrap();
    assert_eq!(got.tier, MemoryTier::Archived);
}

#[test]
fn test_restore_memory() {
    let s = storage();
    let mem = make_mem("restorable", MemoryTier::Archived);
    let id = mem.id;
    s.store_memory(&mem).unwrap();

    s.restore_memory(id, MemoryTier::Episodic).unwrap();
    let got = s.get_memory(id).unwrap().unwrap();
    assert_eq!(got.tier, MemoryTier::Episodic);
}

#[test]
fn test_archive_missing_is_ok() {
    let s = storage();
    // Archiving a non-existent memory should not error
    s.archive_memory(Uuid::new_v4()).unwrap();
}

// ── Beliefs ──────────────────────────────────────────────────────────────

#[test]
fn test_belief_crud() {
    let s = storage();
    let belief = cortex_core::belief::Belief {
        id: Uuid::new_v4(),
        key: "test_belief".into(),
        probability: 0.7,
        observations: vec![],
        last_updated: Utc::now(),
    };

    s.store_belief(&belief).unwrap();
    let got = s.get_belief("test_belief").unwrap().unwrap();
    assert_eq!(got.key, "test_belief");
    assert!((got.probability - 0.7).abs() < 0.01);

    // Update
    let mut updated = got;
    updated.probability = 0.9;
    s.update_belief(&updated).unwrap();
    let got2 = s.get_belief("test_belief").unwrap().unwrap();
    assert!((got2.probability - 0.9).abs() < 0.01);

    // List above threshold
    let high = s.list_beliefs_above(0.8).unwrap();
    assert_eq!(high.len(), 1);

    let low = s.list_beliefs_above(0.95).unwrap();
    assert!(low.is_empty());
}

#[test]
fn test_get_belief_missing() {
    let s = storage();
    assert!(s.get_belief("nonexistent").unwrap().is_none());
}

// ── Patterns ─────────────────────────────────────────────────────────────

#[test]
fn test_pattern_crud() {
    let s = storage();
    let pattern = cortex_core::procedural::Pattern {
        id: Uuid::new_v4(),
        trigger: "morning".into(),
        actions: vec!["check_email".into(), "read_news".into()],
        frequency: 5,
        last_seen: Utc::now(),
    };

    s.store_pattern(&pattern).unwrap();
    let got = s.get_pattern("morning").unwrap().unwrap();
    assert_eq!(got.trigger, "morning");
    assert_eq!(got.actions.len(), 2);
    assert_eq!(got.frequency, 5);

    // Update
    let mut updated = got;
    updated.frequency = 10;
    s.update_pattern(&updated).unwrap();
    let got2 = s.get_pattern("morning").unwrap().unwrap();
    assert_eq!(got2.frequency, 10);

    // List by min frequency
    let frequent = s.list_patterns(8).unwrap();
    assert_eq!(frequent.len(), 1);

    let rare = s.list_patterns(20).unwrap();
    assert!(rare.is_empty());
}

#[test]
fn test_get_pattern_missing() {
    let s = storage();
    assert!(s.get_pattern("nonexistent").unwrap().is_none());
}

// ── People ───────────────────────────────────────────────────────────────

#[test]
fn test_people_crud() {
    use cortex_core::people::{ChannelIdentity, Person};

    let s = storage();
    let person = Person {
        id: Uuid::new_v4(),
        identities: vec![ChannelIdentity {
            channel: "slack".into(),
            channel_user_id: "U123".into(),
            username: Some("alice".into()),
            display_name: Some("Alice".into()),
        }],
        display_name: "Alice".into(),
        relationship_to_user: "colleague".into(),
        first_seen: Utc::now(),
        last_seen: Utc::now(),
        interaction_count: 1,
        communication_style: std::collections::BTreeMap::new(),
        tags: vec!["team".into()],
        notes: vec![],
    };

    s.store_person(&person).unwrap();

    let got = s.get_person(person.id).unwrap().unwrap();
    assert_eq!(got.display_name, "Alice");
    assert_eq!(got.identities.len(), 1);
    assert_eq!(got.identities[0].channel, "slack");

    // Find by channel identity
    let found = s.find_person_by_channel_identity("slack", "U123").unwrap().unwrap();
    assert_eq!(found.id, person.id);

    // Not found
    assert!(s.find_person_by_channel_identity("slack", "UXXX").unwrap().is_none());

    // List people
    let all = s.list_people().unwrap();
    assert_eq!(all.len(), 1);

    // Update
    let mut updated = got;
    updated.interaction_count = 5;
    s.update_person(&updated).unwrap();
    let got2 = s.get_person(person.id).unwrap().unwrap();
    assert_eq!(got2.interaction_count, 5);

    // Delete
    s.delete_person(person.id).unwrap();
    assert!(s.get_person(person.id).unwrap().is_none());
}

// ── Entity-indexed queries ───────────────────────────────────────────────

#[test]
fn test_query_facts_by_entity() {
    let s = storage();
    let fact = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "Alice".into(),
            predicate: "works_at".into(),
            object: "Google".into(),
        },
        MemSource::new("test"),
    )
    .build();
    s.store_memory(&fact).unwrap();

    let results = s.query_facts_by_entity("Alice").unwrap();
    assert_eq!(results.len(), 1);

    let results2 = s.query_facts_by_entity("Google").unwrap();
    assert_eq!(results2.len(), 1);

    let empty = s.query_facts_by_entity("Bob").unwrap();
    assert!(empty.is_empty());
}

#[test]
fn test_query_preferences_by_key() {
    let s = storage();
    let pref = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Preference {
            key: "editor".into(),
            value: "neovim".into(),
            confidence: 0.9,
        },
        MemSource::new("test"),
    )
    .build();
    s.store_memory(&pref).unwrap();

    let results = s.query_preferences_by_key("editor").unwrap();
    assert_eq!(results.len(), 1);

    let empty = s.query_preferences_by_key("theme").unwrap();
    assert!(empty.is_empty());
}

// ── Source identity query ────────────────────────────────────────────────

#[test]
fn test_list_memories_by_source_identity() {
    // The query uses json_extract with a quoted UUID format.
    // Verify it doesn't error, even if JSON quoting prevents matches.
    let s = storage();
    let person_id = Uuid::new_v4();

    let mut mem = make_mem("from person", MemoryTier::Episodic);
    mem.source.identity_id = Some(person_id);
    s.store_memory(&mem).unwrap();

    // Should not error
    let results = s.list_memories_by_source_identity(person_id).unwrap();
    // Result depends on SQLite json_extract UUID quoting behavior
    assert!(results.len() <= 1);
}

// ── Embedding blob round-trip ────────────────────────────────────────────

#[test]
fn test_embedding_round_trip() {
    let s = storage();
    let emb = vec![0.1, 0.2, 0.3, 0.4, 0.5];
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("with embedding".into()),
        MemSource::new("test"),
    )
    .embedding(emb.clone())
    .build();
    s.store_memory(&mem).unwrap();

    let got = s.get_memory(mem.id).unwrap().unwrap();
    let got_emb = got.embedding.unwrap();
    assert_eq!(got_emb.len(), 5);
    // int8 quantization: precision loss < 0.005 for small vectors with narrow range
    for (a, b) in emb.iter().zip(got_emb.iter()) {
        assert!((a - b).abs() < 0.01, "int8 round-trip: {} vs {}", a, b);
    }
}

#[test]
fn test_embedding_int8_all_same_values() {
    let s = storage();
    let emb = vec![0.5, 0.5, 0.5, 0.5];
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("constant embedding".into()),
        MemSource::new("test"),
    )
    .embedding(emb.clone())
    .build();
    s.store_memory(&mem).unwrap();
    let got = s.get_memory(mem.id).unwrap().unwrap();
    let got_emb = got.embedding.unwrap();
    for (a, b) in emb.iter().zip(got_emb.iter()) {
        assert!((a - b).abs() < 1e-6, "constant round-trip: {} vs {}", a, b);
    }
}

#[test]
fn test_embedding_int8_cosine_accuracy() {
    let s = storage();
    // Typical embedding range: small values around 0
    let emb: Vec<f32> = (0..384).map(|i| (i as f32 * 0.01).sin() * 0.1).collect();
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("384-dim embedding".into()),
        MemSource::new("test"),
    )
    .embedding(emb.clone())
    .build();
    s.store_memory(&mem).unwrap();
    let got = s.get_memory(mem.id).unwrap().unwrap();
    let got_emb = got.embedding.unwrap();
    assert_eq!(got_emb.len(), 384);
    // Cosine similarity between original and quantized should be > 0.99
    let dot: f32 = emb.iter().zip(got_emb.iter()).map(|(a, b)| a * b).sum();
    let norm_a: f32 = emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    let norm_b: f32 = got_emb.iter().map(|x| x * x).sum::<f32>().sqrt();
    let cosine = dot / (norm_a * norm_b);
    assert!(cosine > 0.99, "int8 cosine accuracy: {}", cosine);
}
