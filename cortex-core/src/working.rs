use std::collections::VecDeque;
use uuid::Uuid;

use crate::types::*;

/// Working memory — current session context. Ephemeral, not persisted.
pub struct WorkingMemory {
    items: VecDeque<MemObject>,
    capacity: usize,
}

impl WorkingMemory {
    pub fn new(capacity: usize) -> Self {
        Self {
            items: VecDeque::with_capacity(capacity),
            capacity,
        }
    }

    /// Push a new item into working memory. Evicts oldest if at capacity.
    pub fn push(&mut self, mem: MemObject) {
        if self.items.len() >= self.capacity {
            self.items.pop_front();
        }
        self.items.push_back(mem);
    }

    /// Get all items currently in working memory.
    pub fn items(&self) -> &VecDeque<MemObject> {
        &self.items
    }

    /// Get the most recent N items.
    pub fn recent(&self, n: usize) -> Vec<&MemObject> {
        self.items.iter().rev().take(n).collect()
    }

    /// Find an item by ID.
    pub fn get(&self, id: Uuid) -> Option<&MemObject> {
        self.items.iter().find(|m| m.id == id)
    }

    /// Clear all working memory.
    pub fn clear(&mut self) {
        self.items.clear();
    }

    /// Current size of working memory.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Create a working memory MemObject from text.
    pub fn create_working_mem(text: &str, source: MemSource) -> MemObject {
        MemObjectBuilder::new(
            MemoryTier::Working,
            MemContent::Text(text.to_string()),
            source,
        )
        .build()
    }
}

impl Default for WorkingMemory {
    fn default() -> Self {
        Self::new(100)
    }
}
