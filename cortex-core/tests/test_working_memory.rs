//! Tests for WorkingMemory — the ephemeral scratch-pad tier.

use cortex_core::working::WorkingMemory;
use cortex_core::types::*;

fn make_item(text: &str) -> MemObject {
    MemObjectBuilder::new(
        MemoryTier::Working,
        MemContent::Text(text.to_string()),
        MemSource::new("test"),
    )
    .build()
}

#[test]
fn test_new_is_empty() {
    let wm = WorkingMemory::new(10);
    assert!(wm.is_empty());
    assert_eq!(wm.len(), 0);
}

#[test]
fn test_default_is_empty() {
    let wm = WorkingMemory::default();
    assert!(wm.is_empty());
    assert_eq!(wm.len(), 0);
}

#[test]
fn test_push_and_len() {
    let mut wm = WorkingMemory::new(10);
    wm.push(make_item("hello"));
    assert_eq!(wm.len(), 1);
    assert!(!wm.is_empty());

    wm.push(make_item("world"));
    assert_eq!(wm.len(), 2);
}

#[test]
fn test_items_returns_all() {
    let mut wm = WorkingMemory::new(10);
    wm.push(make_item("a"));
    wm.push(make_item("b"));
    wm.push(make_item("c"));

    let items = wm.items();
    assert_eq!(items.len(), 3);
}

#[test]
fn test_recent_returns_latest() {
    let mut wm = WorkingMemory::new(10);
    for i in 0..5 {
        wm.push(make_item(&format!("item {}", i)));
    }

    let recent = wm.recent(3);
    assert_eq!(recent.len(), 3);
}

#[test]
fn test_recent_clamps_to_available() {
    let mut wm = WorkingMemory::new(10);
    wm.push(make_item("only one"));

    let recent = wm.recent(5);
    assert_eq!(recent.len(), 1);
}

#[test]
fn test_get_by_id() {
    let mut wm = WorkingMemory::new(10);
    let item = make_item("findme");
    let id = item.id;
    wm.push(item);
    wm.push(make_item("other"));

    let found = wm.get(id);
    assert!(found.is_some());
    match &found.unwrap().content {
        MemContent::Text(t) => assert_eq!(t, "findme"),
        _ => panic!("wrong content type"),
    }
}

#[test]
fn test_get_missing_returns_none() {
    let wm = WorkingMemory::new(10);
    let missing = wm.get(uuid::Uuid::new_v4());
    assert!(missing.is_none());
}

#[test]
fn test_clear() {
    let mut wm = WorkingMemory::new(10);
    wm.push(make_item("a"));
    wm.push(make_item("b"));
    assert_eq!(wm.len(), 2);

    wm.clear();
    assert!(wm.is_empty());
    assert_eq!(wm.len(), 0);
}

#[test]
fn test_capacity_eviction() {
    let mut wm = WorkingMemory::new(3);
    wm.push(make_item("a"));
    wm.push(make_item("b"));
    wm.push(make_item("c"));
    assert_eq!(wm.len(), 3);

    // Pushing beyond capacity should evict oldest
    wm.push(make_item("d"));
    assert_eq!(wm.len(), 3);
    // The oldest "a" should be gone, newest "d" present
    let items = wm.items();
    let texts: Vec<String> = items
        .iter()
        .map(|m| match &m.content {
            MemContent::Text(t) => t.clone(),
            _ => String::new(),
        })
        .collect();
    assert!(!texts.contains(&"a".to_string()));
    assert!(texts.contains(&"d".to_string()));
}

#[test]
fn test_create_working_mem() {
    let mem = WorkingMemory::create_working_mem("test content", MemSource::new("cli"));
    assert_eq!(mem.tier, MemoryTier::Working);
    assert_eq!(mem.source.channel, "cli");
    match &mem.content {
        MemContent::Text(t) => assert_eq!(t, "test content"),
        _ => panic!("expected Text content"),
    }
}
