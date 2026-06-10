use cortex_core::retrieval::{RetrievalEngine, RetrievalQuery, RetrievalWeights};
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

#[test]
fn test_retrieve_by_embedding() {
    let (storage, index) = setup();

    let emb1 = vec![1.0, 0.0, 0.0];
    let emb2 = vec![0.0, 1.0, 0.0];
    let emb3 = vec![0.0, 0.0, 1.0];

    for (text, emb) in [("cats", emb1), ("dogs", emb2), ("birds", emb3)] {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(text.to_string()),
            MemSource::new("cli"),
        )
        .embedding(emb.clone())
        .build();
        storage.store_memory(&mem).unwrap();
        index.insert(mem.id, emb);
    }

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("similar to cats", 2)
        .with_embedding(vec![0.9, 0.1, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);
    // Cats should rank highest (most similar embedding)
    assert!(matches!(&results[0].memory.content, MemContent::Text(t) if t == "cats"));
}

#[test]
fn test_channel_boost() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    for (ch, text) in [("telegram", "from telegram"), ("email", "from email")] {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(text.to_string()),
            MemSource::new(ch),
        )
        .embedding(emb.clone())
        .build();
        storage.store_memory(&mem).unwrap();
        index.insert(mem.id, emb.clone());
    }

    let engine = RetrievalEngine::new(&storage, &index)
        .with_weights(RetrievalWeights {
            similarity: 0.1,
            temporal: 0.1,
            salience: 0.1,
            social: 0.0,
            channel: 0.7, // heavy channel weight
            fts: 0.0,
            frecency: 0.0,
        });

    let query = RetrievalQuery::new("test", 2)
        .with_embedding(vec![1.0, 0.0, 0.0])
        .with_channel("telegram");

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].score_breakdown.channel > results[1].score_breakdown.channel);
}

#[test]
fn test_person_boost() {
    let (storage, index) = setup();

    let person_id = Uuid::new_v4();
    let emb = vec![1.0, 0.0, 0.0];

    // Memory from the person
    let mut source = MemSource::new("telegram");
    source.identity_id = Some(person_id);
    let mem1 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("from alice".to_string()),
        source,
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&mem1).unwrap();
    index.insert(mem1.id, emb.clone());

    // Memory from someone else
    let mem2 = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("from bob".to_string()),
        MemSource::new("telegram"),
    )
    .embedding(emb.clone())
    .build();
    storage.store_memory(&mem2).unwrap();
    index.insert(mem2.id, emb.clone());

    let engine = RetrievalEngine::new(&storage, &index)
        .with_weights(RetrievalWeights {
            similarity: 0.1,
            temporal: 0.1,
            salience: 0.1,
            social: 0.6, // heavy social weight
            channel: 0.1,
            fts: 0.0,
            frecency: 0.0,
        });

    let query = RetrievalQuery::new("test", 2)
        .with_embedding(vec![1.0, 0.0, 0.0])
        .with_person(person_id);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);
    assert!(results[0].score_breakdown.social > 0.0);
}

#[test]
fn test_fallback_without_embedding() {
    let (storage, index) = setup();

    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("hello".to_string()),
        MemSource::new("cli"),
    )
    .build();
    storage.store_memory(&mem).unwrap();

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("anything", 5);

    let results = engine.retrieve(&query).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn test_score_breakdown_populated() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".to_string()),
        MemSource::new("cli"),
    )
    .embedding(emb.clone())
    .salience(Salience::new(0.8))
    .build();
    storage.store_memory(&mem).unwrap();
    index.insert(mem.id, emb);

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("test", 1)
        .with_embedding(vec![1.0, 0.0, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 1);
    let bd = &results[0].score_breakdown;
    assert!(bd.similarity > 0.0);
    assert!(bd.temporal > 0.0);
    assert!(bd.salience > 0.0);
}

#[test]
fn test_retrieval_respects_limit() {
    let (storage, index) = setup();

    for i in 0..10 {
        let emb = vec![i as f32 / 10.0, 1.0 - i as f32 / 10.0, 0.0];
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("memory {}", i)),
            MemSource::new("cli"),
        )
        .embedding(emb.clone())
        .build();
        storage.store_memory(&mem).unwrap();
        index.insert(mem.id, emb);
    }

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("test", 3)
        .with_embedding(vec![0.5, 0.5, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 3);
}

#[test]
fn test_results_sorted_by_score() {
    let (storage, index) = setup();

    let emb = vec![1.0, 0.0, 0.0];
    for (sal, text) in [(0.1, "low"), (0.9, "high"), (0.5, "mid")] {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(text.to_string()),
            MemSource::new("cli"),
        )
        .embedding(emb.clone())
        .salience(Salience::new(sal))
        .build();
        storage.store_memory(&mem).unwrap();
        index.insert(mem.id, emb.clone());
    }

    let engine = RetrievalEngine::new(&storage, &index);
    let query = RetrievalQuery::new("test", 3)
        .with_embedding(vec![1.0, 0.0, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    for i in 1..results.len() {
        assert!(results[i - 1].score >= results[i].score);
    }
}

#[test]
fn test_frecency_boosts_frequently_recalled_memory() {
    let (storage, index) = setup();
    let emb = vec![1.0, 0.0, 0.0];

    // Two memories with identical content and embedding — only recall history differs.
    let mut ids = Vec::new();
    for text in ["alpha notes hot", "alpha notes cold"] {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(text.to_string()),
            MemSource::new("cli"),
        )
        .embedding(emb.clone())
        .build();
        storage.store_memory(&mem).unwrap();
        index.insert(mem.id, emb.clone());
        ids.push((text, mem.id));
    }

    // "hot": recalled often and just now. "cold": never recalled, last touched 90 days ago.
    let now = chrono::Utc::now();
    let mut hot = storage.get_memory(ids[0].1).unwrap().unwrap();
    hot.temporal.access_count = 30;
    hot.temporal.last_accessed = now;
    storage.update_memory(&hot).unwrap();

    let mut cold = storage.get_memory(ids[1].1).unwrap().unwrap();
    cold.temporal.access_count = 0;
    cold.temporal.last_accessed = now - chrono::Duration::days(90);
    storage.update_memory(&cold).unwrap();

    // Isolate the frecency signal so the ranking change is unambiguous.
    let engine = RetrievalEngine::new(&storage, &index).with_weights(RetrievalWeights {
        similarity: 0.0,
        temporal: 0.0,
        salience: 0.0,
        social: 0.0,
        channel: 0.0,
        fts: 0.0,
        frecency: 1.0,
    });
    let query = RetrievalQuery::new("alpha notes", 2).with_embedding(vec![1.0, 0.0, 0.0]);

    let results = engine.retrieve(&query).unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].memory.id, hot.id,
        "frequently and recently recalled memory should rank first under frecency"
    );
    assert!(results[0].score_breakdown.frecency > results[1].score_breakdown.frecency);
}
