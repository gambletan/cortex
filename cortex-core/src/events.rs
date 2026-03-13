//! Event bus — change feed for memory mutations.
//!
//! Subscribers receive `CortexEvent` notifications for memory lifecycle events.
//! Useful for audit logging, cache invalidation, or downstream triggers.

use std::sync::{Arc, Mutex};
use crate::types::CortexEvent;

/// Callback type for event subscribers.
pub type EventHandler = Box<dyn Fn(&CortexEvent) + Send + Sync>;

/// Simple synchronous event bus for memory mutation events.
pub struct EventBus {
    handlers: Arc<Mutex<Vec<EventHandler>>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    pub fn new() -> Self {
        Self {
            handlers: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Subscribe a handler to receive all events.
    pub fn subscribe(&self, handler: EventHandler) {
        if let Ok(mut handlers) = self.handlers.lock() {
            handlers.push(handler);
        }
    }

    /// Emit an event to all subscribers.
    pub fn emit(&self, event: &CortexEvent) {
        if let Ok(handlers) = self.handlers.lock() {
            for handler in handlers.iter() {
                handler(event);
            }
        }
    }

    /// Number of subscribers.
    pub fn subscriber_count(&self) -> usize {
        self.handlers.lock().map(|h| h.len()).unwrap_or(0)
    }
}
