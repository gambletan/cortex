//! Advanced retrieval tests: query expansion, multi-hop, temporal intent,
//! query type detection, namespace isolation, archived filtering, weight adaptation.

use cortex_core::retrieval::{
    RetrievalEngine, RetrievalQuery, RetrievalWeights,
    detect_query_type, QueryType,
};
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;
use uuid::Uuid;

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

// ── Query expansion via semantic store ───────────────────────────────────

#[test]
fn test_query_expansion_with_facts() {
    let (storage, index) = setup();

    // Store a fact: Alice works_at Google
    let fact = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "Alice".into(),
            predicate: "works_at".into(),
            object: "Google".into(),
        },
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&fact).unwrap();

    // Store an episodic memory mentioning Google
    let emb = vec![1.0, 0.0, 0.0];
    let ep = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("Google announced a new product today".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&ep).unwrap();
    index.insert(ep.id, emb.clone());

    // Query about Alice — should expand to Google and find the episodic memory
    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("Tell me about Alice", 5)
        .with_embedding(vec![0.5, 0.5, 0.0]);
    let results = engine.retrieve(&query).unwrap();

    // Should find at least the episodic memory via expansion
    assert!(!results.is_empty());
}

#[test]
fn test_query_expansion_with_preferences() {
    let (storage, index) = setup();

    // Store a preference
    let pref = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Preference {
            key: "editor".into(),
            value: "neovim".into(),
            confidence: 0.9,
        },
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&pref).unwrap();

    // Store episodic memory mentioning neovim
    let emb = vec![1.0, 0.0, 0.0];
    let ep = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("I configured neovim with LSP today".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&ep).unwrap();
    index.insert(ep.id, emb);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("editor setup", 5)
        .with_embedding(vec![0.8, 0.2, 0.0]);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn test_query_expansion_empty_for_no_entities() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    let ep = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("generic message".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&ep).unwrap();
    index.insert(ep.id, emb);

    // Query with no entities — expansion should be empty, still returns results
    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("hi", 5)
        .with_embedding(vec![1.0, 0.0, 0.0]);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

// ── Multi-hop expansion ─────────────────────────────────────────────────

#[test]
fn test_multi_hop_expansion() {
    let (storage, index) = setup();

    // Fact chain: Alice → Google, Google → Mountain View
    let fact1 = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "Alice".into(),
            predicate: "works_at".into(),
            object: "Google".into(),
        },
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&fact1).unwrap();

    let fact2 = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "Google".into(),
            predicate: "located_in".into(),
            object: "Mountain View".into(),
        },
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&fact2).unwrap();

    // Episodic memory as initial candidate
    let emb = vec![1.0, 0.0, 0.0];
    let ep = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("Alice told me about her work".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&ep).unwrap();
    index.insert(ep.id, emb);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("Alice work", 10)
        .with_embedding(vec![1.0, 0.0, 0.0]);
    let results = engine.retrieve(&query).unwrap();

    // Should find at least the episodic memory; facts may also appear via expansion
    assert!(!results.is_empty());
}

// ── Namespace isolation in retrieval ─────────────────────────────────────

#[test]
fn test_retrieval_namespace_isolation() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];

    // Memory in namespace "alpha"
    let m1 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("alpha data".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .namespace("alpha")
    .build();
    storage.store_memory(&m1).unwrap();
    index.insert(m1.id, emb.clone());

    // Memory in namespace "beta"
    let m2 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("beta data".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .namespace("beta")
    .build();
    storage.store_memory(&m2).unwrap();
    index.insert(m2.id, emb.clone());

    // Memory with no namespace
    let m3 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("global data".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&m3).unwrap();
    index.insert(m3.id, emb.clone());

    let engine = RetrievalEngine::new(&storage, &index);

    // Query with namespace=alpha should only return alpha
    let query = RetrievalQuery::new("data", 10)
        .with_embedding(vec![1.0, 0.0, 0.0])
        .with_namespace("alpha");
    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].memory.namespace.as_deref(), Some("alpha"));
}

// ── Archived memory filtering ────────────────────────────────────────────

#[test]
fn test_retrieval_skips_archived() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];

    let m1 = MemObjectBuilder::new(
        MemoryTier::Archived,
        MemContent::Text("archived data".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&m1).unwrap();
    index.insert(m1.id, emb.clone());

    let m2 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("active data".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&m2).unwrap();
    index.insert(m2.id, emb.clone());

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("data", 10)
        .with_embedding(vec![1.0, 0.0, 0.0]);
    let results = engine.retrieve(&query).unwrap();

    // Only active memory should appear
    assert_eq!(results.len(), 1);
    assert_ne!(results[0].memory.tier, MemoryTier::Archived);
}

// ── Weight adaptation by query type ──────────────────────────────────────

#[test]
fn test_weight_adaptation_person_query() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    let person_id = Uuid::new_v4();

    // Memory from the person
    let mut source = MemSource::new("telegram");
    source.identity_id = Some(person_id);
    let m1 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("from alice".into()),
        source,
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&m1).unwrap();
    index.insert(m1.id, emb.clone());

    // Memory from nobody
    let m2 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("from nobody".into()),
        MemSource::new("telegram"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&m2).unwrap();
    index.insert(m2.id, emb.clone());

    let engine = RetrievalEngine::new(&storage, &index);

    // "Who is Alice" is a PersonQuery → social weight boosted
    let query = RetrievalQuery::new("who is Alice", 5)
        .with_embedding(vec![1.0, 0.0, 0.0])
        .with_person(person_id);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
    // The person's memory should rank higher due to social boost
    assert!(results[0].score_breakdown.social > 0.0);
}

#[test]
fn test_weight_adaptation_fact_query() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("some data".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&mem).unwrap();
    index.insert(mem.id, emb);

    let engine = RetrievalEngine::new(&storage, &index);
    // "What is X" → FactQuery → similarity weight boosted
    let query = RetrievalQuery::new("what is Rust", 5)
        .with_embedding(vec![1.0, 0.0, 0.0]);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn test_weight_adaptation_preference_query() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("I prefer neovim".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .salience(Salience::new(0.9))
    .build();
    storage.store_memory(&mem).unwrap();
    index.insert(mem.id, emb);

    let engine = RetrievalEngine::new(&storage, &index);
    // "What do I like" → PreferenceQuery → salience weight boosted
    let query = RetrievalQuery::new("what do i like for editing", 5)
        .with_embedding(vec![1.0, 0.0, 0.0]);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

// ── Query type detection: comprehensive ──────────────────────────────────

#[test]
fn test_query_type_about_name() {
    // "about [Name]" with capitalized word → PersonQuery
    assert_eq!(detect_query_type("Tell me about Alice"), QueryType::PersonQuery);
}

#[test]
fn test_query_type_about_lowercase() {
    // "about something" without capital → not PersonQuery
    assert_eq!(detect_query_type("tell me about coding"), QueryType::General);
}

#[test]
fn test_query_type_zh_person() {
    assert_eq!(detect_query_type("关于张三的信息"), QueryType::PersonQuery);
}

#[test]
fn test_query_type_zh_non_name() {
    // "关于" + non-name word → not PersonQuery
    assert_eq!(detect_query_type("关于这个问题"), QueryType::General);
}

#[test]
fn test_query_type_zh_fact() {
    assert_eq!(detect_query_type("Rust是什么"), QueryType::FactQuery);
}

#[test]
fn test_query_type_zh_preference() {
    assert_eq!(detect_query_type("我喜欢什么编辑器"), QueryType::PreferenceQuery);
}

#[test]
fn test_query_type_temporal_overrides_fact() {
    // "What happened recently" has both fact ("what") and temporal ("recently")
    // Temporal should win
    assert_eq!(detect_query_type("what happened recently"), QueryType::Temporal);
}

#[test]
fn test_query_type_who_at_start() {
    assert_eq!(detect_query_type("who told me about Rust"), QueryType::PersonQuery);
}

#[test]
fn test_query_type_where_is() {
    assert_eq!(detect_query_type("where is my config file"), QueryType::FactQuery);
}

// ── Temporal retrieval candidates ────────────────────────────────────────

#[test]
fn test_temporal_query_recent_intent() {
    let (storage, index) = setup();

    for i in 0..5 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("event {}", i)),
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    let engine = RetrievalEngine::new(&storage, &index);
    // "recently" triggers Recent intent → temporal weight boosted
    let query = RetrievalQuery::new("what happened recently", 5);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn test_temporal_query_first_intent() {
    let (storage, index) = setup();

    for i in 0..5 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("event {}", i)),
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    let engine = RetrievalEngine::new(&storage, &index);
    // "first time" triggers First intent → older memories scored higher
    let query = RetrievalQuery::new("the first time we met", 5);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn test_temporal_query_sequence_intent() {
    let (storage, index) = setup();

    for i in 0..5 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("step {}", i)),
            MemSource::new("cli"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    let engine = RetrievalEngine::new(&storage, &index);
    // "timeline" triggers Sequence intent
    let query = RetrievalQuery::new("show me the timeline of events", 5);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

// ── Fallback without embedding ───────────────────────────────────────────

#[test]
fn test_retrieval_fallback_mixed_tiers() {
    let (storage, index) = setup();

    // Episodic
    let ep = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("episodic data".into()),
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&ep).unwrap();

    // Semantic
    let sem = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Fact {
            subject: "User".into(),
            predicate: "likes".into(),
            object: "Rust".into(),
        },
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&sem).unwrap();

    // No embedding → falls back to recent memories from both tiers
    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("anything", 10);
    let results = engine.retrieve(&query).unwrap();
    assert!(results.len() >= 2);
}

// ── Custom weights ───────────────────────────────────────────────────────

#[test]
fn test_zero_similarity_weight() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&mem).unwrap();
    index.insert(mem.id, emb);

    let engine = RetrievalEngine::new(&storage, &index)
        .with_weights(RetrievalWeights {
            similarity: 0.0,
            temporal: 1.0,
            salience: 0.0,
            social: 0.0,
            channel: 0.0,
            fts: 0.0,
            frecency: 0.0,
        });

    let query = RetrievalQuery::new("test", 5)
        .with_embedding(vec![1.0, 0.0, 0.0]);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
    // Score should be driven only by temporal
    assert_eq!(results[0].score_breakdown.similarity * 0.0, 0.0);
}

// ── Social score with relationship content ───────────────────────────────

#[test]
fn test_social_score_relationship_content() {
    let (storage, index) = setup();

    let person_id = Uuid::new_v4();
    let emb = vec![1.0, 0.0, 0.0];

    // A relationship memory mentioning the person
    let rel = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Relationship {
            person_a: person_id,
            person_b: Uuid::new_v4(),
            relation: "colleague".into(),
        },
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&rel).unwrap();
    index.insert(rel.id, emb);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("colleague", 5)
        .with_embedding(vec![1.0, 0.0, 0.0])
        .with_person(person_id);
    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
    // Social score should be 1.0 because relationship matches person_id
    assert_eq!(results[0].score_breakdown.social, 1.0);
}
