//! Tests for procedural memory — behavioral pattern detection and promotion.

use cortex_core::procedural::ProceduralStore;
use cortex_core::storage::sqlite::SqliteStorage;
use cortex_core::storage::traits::StorageBackend;
use cortex_core::types::*;

fn setup() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

#[test]
fn test_record_action_creates_new_pattern() {
    let storage = setup();
    let store = ProceduralStore::new(&storage);

    let pattern = store.record_action("morning", "check_email", "daily routine").unwrap();
    assert_eq!(pattern.trigger, "morning");
    assert_eq!(pattern.actions, vec!["check_email"]);
    assert_eq!(pattern.frequency, 1);
}

#[test]
fn test_record_action_increments_frequency() {
    let storage = setup();
    let store = ProceduralStore::new(&storage);

    store.record_action("morning", "check_email", "").unwrap();
    store.record_action("morning", "check_email", "").unwrap();
    let pattern = store.record_action("morning", "check_email", "").unwrap();

    assert_eq!(pattern.frequency, 3);
    // Same action should not be duplicated
    assert_eq!(pattern.actions.len(), 1);
}

#[test]
fn test_record_action_adds_new_actions() {
    let storage = setup();
    let store = ProceduralStore::new(&storage);

    store.record_action("morning", "check_email", "").unwrap();
    let pattern = store.record_action("morning", "read_news", "").unwrap();

    assert_eq!(pattern.frequency, 2);
    assert_eq!(pattern.actions.len(), 2);
    assert!(pattern.actions.contains(&"check_email".to_string()));
    assert!(pattern.actions.contains(&"read_news".to_string()));
}

#[test]
fn test_detect_patterns_min_frequency() {
    let storage = setup();
    let store = ProceduralStore::new(&storage);

    // Pattern with frequency 3
    store.record_action("morning", "email", "").unwrap();
    store.record_action("morning", "email", "").unwrap();
    store.record_action("morning", "email", "").unwrap();

    // Pattern with frequency 1
    store.record_action("evening", "read", "").unwrap();

    // Min frequency 3 should only return "morning"
    let patterns = store.detect_patterns(3).unwrap();
    assert_eq!(patterns.len(), 1);
    assert_eq!(patterns[0].trigger, "morning");

    // Min frequency 1 should return both
    let all = store.detect_patterns(1).unwrap();
    assert_eq!(all.len(), 2);
}

#[test]
fn test_get_pattern() {
    let storage = setup();
    let store = ProceduralStore::new(&storage);

    assert!(store.get_pattern("nonexistent").unwrap().is_none());

    store.record_action("deploy", "run_tests", "").unwrap();
    let pattern = store.get_pattern("deploy").unwrap();
    assert!(pattern.is_some());
    assert_eq!(pattern.unwrap().trigger, "deploy");
}

#[test]
fn test_promote_pattern() {
    let storage = setup();
    let store = ProceduralStore::new(&storage);

    let pattern = store.record_action("standup", "open_jira", "").unwrap();
    let mem = store.promote_pattern(&pattern).unwrap();

    assert_eq!(mem.tier, MemoryTier::Procedural);
    if let MemContent::Pattern { trigger, actions, frequency } = &mem.content {
        assert_eq!(trigger, "standup");
        assert_eq!(actions, &vec!["open_jira".to_string()]);
        assert_eq!(*frequency, 1);
    } else {
        panic!("Expected Pattern content");
    }

    // Verify it was stored
    let stored = storage.get_memory(mem.id).unwrap();
    assert!(stored.is_some());
}
