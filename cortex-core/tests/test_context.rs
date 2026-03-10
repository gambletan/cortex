use cortex_core::context::{generate_context, ContextConfig};
use cortex_core::storage::memory_index::MemoryIndex;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;
use cortex_core::people::PeopleGraph;

fn setup() -> (SqliteStorage, MemoryIndex) {
    let storage = SqliteStorage::open_in_memory().unwrap();
    let index = MemoryIndex::new();
    (storage, index)
}

#[test]
fn test_empty_context() {
    let (storage, index) = setup();
    let config = ContextConfig::default();

    let ctx = generate_context(&config, &storage, &index).unwrap();
    assert!(ctx.contains("[Cortex Memory Context]"));
}

#[test]
fn test_context_with_preferences() {
    let (storage, index) = setup();

    let mem = MemObjectBuilder::new(
        MemoryTier::Semantic,
        MemContent::Preference {
            key: "editor".to_string(),
            value: "neovim".to_string(),
            confidence: 0.9,
        },
        MemSource::new("system"),
    )
    .build();
    storage.store_memory(&mem).unwrap();

    let config = ContextConfig::new(2000);
    let ctx = generate_context(&config, &storage, &index).unwrap();
    assert!(ctx.contains("editor"));
    assert!(ctx.contains("neovim"));
    assert!(ctx.contains("User Profile"));
}

#[test]
fn test_context_with_recent_episodes() {
    let (storage, index) = setup();

    for i in 0..3 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(format!("conversation snippet {}", i)),
            MemSource::new("telegram"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    let config = ContextConfig::new(2000).with_recent_episodes(5);
    let ctx = generate_context(&config, &storage, &index).unwrap();
    assert!(ctx.contains("Recent Context"));
    assert!(ctx.contains("conversation snippet"));
}

#[test]
fn test_context_with_person() {
    let (storage, index) = setup();

    let graph = PeopleGraph::new(&storage);
    let person = graph
        .resolve_identity("telegram", "alice42", Some("Alice"), None)
        .unwrap();

    let config = ContextConfig::new(2000).with_person(person.id);
    let ctx = generate_context(&config, &storage, &index).unwrap();
    assert!(ctx.contains("Alice"));
    assert!(ctx.contains("Conversation Partner"));
}

#[test]
fn test_context_truncation() {
    let (storage, index) = setup();

    // Add lots of preferences to exceed budget
    for i in 0..100 {
        let mem = MemObjectBuilder::new(
            MemoryTier::Semantic,
            MemContent::Preference {
                key: format!("setting_{}", i),
                value: format!("value_{}_with_extra_padding_to_make_this_longer", i),
                confidence: 0.8,
            },
            MemSource::new("system"),
        )
        .build();
        storage.store_memory(&mem).unwrap();
    }

    // Very small token budget
    let config = ContextConfig::new(50);
    let ctx = generate_context(&config, &storage, &index).unwrap();
    // 50 tokens * 4 chars = 200 chars max
    assert!(ctx.len() < 500); // some margin for truncation mechanics
}
