//! Tests for the EventBus change feed.

use cortex_core::events::EventBus;
use cortex_core::types::{CortexEvent, MemoryTier};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use uuid::Uuid;

#[test]
fn test_event_bus_new() {
    let bus = EventBus::new();
    assert_eq!(bus.subscriber_count(), 0);
}

#[test]
fn test_event_bus_default() {
    let bus = EventBus::default();
    assert_eq!(bus.subscriber_count(), 0);
}

#[test]
fn test_subscribe_increases_count() {
    let bus = EventBus::new();
    bus.subscribe(Box::new(|_| {}));
    assert_eq!(bus.subscriber_count(), 1);

    bus.subscribe(Box::new(|_| {}));
    assert_eq!(bus.subscriber_count(), 2);
}

#[test]
fn test_emit_calls_handlers() {
    let bus = EventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();

    bus.subscribe(Box::new(move |_| {
        c.fetch_add(1, Ordering::Relaxed);
    }));

    let event = CortexEvent::MemoryCreated {
        id: Uuid::new_v4(),
        tier: MemoryTier::Episodic,
    };

    bus.emit(&event);
    assert_eq!(counter.load(Ordering::Relaxed), 1);

    bus.emit(&event);
    assert_eq!(counter.load(Ordering::Relaxed), 2);
}

#[test]
fn test_emit_calls_all_handlers() {
    let bus = EventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));

    for _ in 0..5 {
        let c = counter.clone();
        bus.subscribe(Box::new(move |_| {
            c.fetch_add(1, Ordering::Relaxed);
        }));
    }

    bus.emit(&CortexEvent::DecayCompleted { count: 10 });
    assert_eq!(counter.load(Ordering::Relaxed), 5);
}

#[test]
fn test_emit_with_no_subscribers() {
    let bus = EventBus::new();
    // Should not panic
    bus.emit(&CortexEvent::MemoryDeleted {
        id: Uuid::new_v4(),
    });
}

#[test]
fn test_event_data_received() {
    let bus = EventBus::new();
    let received_id = Arc::new(std::sync::Mutex::new(None));
    let r = received_id.clone();

    bus.subscribe(Box::new(move |event| {
        if let CortexEvent::MemoryArchived { id } = event {
            *r.lock().unwrap() = Some(*id);
        }
    }));

    let target_id = Uuid::new_v4();
    bus.emit(&CortexEvent::MemoryArchived { id: target_id });

    assert_eq!(*received_id.lock().unwrap(), Some(target_id));
}

#[test]
fn test_all_event_variants_emittable() {
    let bus = EventBus::new();
    let counter = Arc::new(AtomicUsize::new(0));
    let c = counter.clone();

    bus.subscribe(Box::new(move |_| {
        c.fetch_add(1, Ordering::Relaxed);
    }));

    let id = Uuid::new_v4();
    bus.emit(&CortexEvent::MemoryCreated { id, tier: MemoryTier::Episodic });
    bus.emit(&CortexEvent::MemoryUpdated { id });
    bus.emit(&CortexEvent::MemoryDeleted { id });
    bus.emit(&CortexEvent::MemoryArchived { id });
    bus.emit(&CortexEvent::ConsolidationCompleted { report: "test".into() });
    bus.emit(&CortexEvent::DecayCompleted { count: 0 });

    assert_eq!(counter.load(Ordering::Relaxed), 6);
}
