use cortex_core::people::PeopleGraph;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;

fn setup() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

#[test]
fn test_resolve_identity_creates_new() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let person = graph
        .resolve_identity("telegram", "user123", Some("Alice"), Some("alice_t"))
        .unwrap();

    assert_eq!(person.display_name, "Alice");
    assert_eq!(person.identities.len(), 1);
    assert_eq!(person.identities[0].channel, "telegram");
    assert_eq!(person.identities[0].channel_user_id, "user123");
}

#[test]
fn test_resolve_identity_finds_existing() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let p1 = graph
        .resolve_identity("telegram", "user123", Some("Alice"), None)
        .unwrap();
    let p2 = graph
        .resolve_identity("telegram", "user123", None, None)
        .unwrap();

    assert_eq!(p1.id, p2.id);
}

#[test]
fn test_merge_identities() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let p1 = graph
        .resolve_identity("telegram", "user123", Some("Alice"), None)
        .unwrap();
    let p2 = graph
        .resolve_identity("email", "alice@example.com", Some("Alice E"), None)
        .unwrap();

    let merged = graph.merge_identities(p1.id, p2.id).unwrap();
    assert_eq!(merged.identities.len(), 2);
    assert_eq!(merged.display_name, "Alice"); // keeps primary name

    // p2 should be deleted
    assert!(storage.get_person(p2.id).unwrap().is_none());
}

#[test]
fn test_merge_preserves_notes_and_tags() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let mut p1 = graph.resolve_identity("tg", "u1", Some("A"), None).unwrap();
    p1.notes = vec!["note1".to_string()];
    p1.tags = vec!["friend".to_string()];
    storage.update_person(&p1).unwrap();

    let mut p2 = graph.resolve_identity("em", "u2", Some("B"), None).unwrap();
    p2.notes = vec!["note2".to_string()];
    p2.tags = vec!["colleague".to_string()];
    storage.update_person(&p2).unwrap();

    let merged = graph.merge_identities(p1.id, p2.id).unwrap();
    assert_eq!(merged.notes.len(), 2);
    assert_eq!(merged.tags.len(), 2);
}

#[test]
fn test_get_context() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let person = graph
        .resolve_identity("telegram", "bob42", Some("Bob"), None)
        .unwrap();

    // Add a memory associated with this person
    let mut source = MemSource::new("telegram");
    source.identity_id = Some(person.id);
    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("Bob asked about Rust".to_string()),
        source,
    )
    .build();
    storage.store_memory(&mem).unwrap();

    let ctx = graph.get_context(person.id).unwrap();
    assert_eq!(ctx.person.display_name, "Bob");
    // Note: list_memories_by_source_identity searches by identity_id in source JSON
}

#[test]
fn test_record_interaction() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let person = graph
        .resolve_identity("tg", "u1", Some("Test"), None)
        .unwrap();
    assert_eq!(person.interaction_count, 0);

    graph.record_interaction(person.id).unwrap();
    graph.record_interaction(person.id).unwrap();

    let updated = storage.get_person(person.id).unwrap().unwrap();
    assert_eq!(updated.interaction_count, 2);
}

#[test]
fn test_relationship_graph_empty() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let edges = graph.get_relationship_graph().unwrap();
    assert!(edges.is_empty());
}

#[test]
fn test_resolve_identity_uses_user_id_as_name_fallback() {
    let storage = setup();
    let graph = PeopleGraph::new(&storage);

    let person = graph
        .resolve_identity("slack", "U12345", None, None)
        .unwrap();

    assert_eq!(person.display_name, "U12345");
}
