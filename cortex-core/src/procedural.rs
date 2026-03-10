use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// A detected behavioral pattern.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pattern {
    pub id: Uuid,
    pub trigger: String,
    pub actions: Vec<String>,
    pub frequency: u32,
    pub last_seen: DateTime<Utc>,
}

/// Procedural memory store — learned workflows and behavioral patterns.
pub struct ProceduralStore<'a> {
    storage: &'a dyn StorageBackend,
}

impl<'a> ProceduralStore<'a> {
    pub fn new(storage: &'a dyn StorageBackend) -> Self {
        Self { storage }
    }

    /// Record an observed action for a given trigger.
    /// If the trigger already exists, increments frequency.
    pub fn record_action(
        &self,
        trigger: &str,
        action: &str,
        _context: &str,
    ) -> Result<Pattern, CortexError> {
        if let Some(mut pattern) = self.storage.get_pattern(trigger)? {
            if !pattern.actions.contains(&action.to_string()) {
                pattern.actions.push(action.to_string());
            }
            pattern.frequency += 1;
            pattern.last_seen = Utc::now();
            self.storage.update_pattern(&pattern)?;
            Ok(pattern)
        } else {
            let pattern = Pattern {
                id: Uuid::new_v4(),
                trigger: trigger.to_string(),
                actions: vec![action.to_string()],
                frequency: 1,
                last_seen: Utc::now(),
            };
            self.storage.store_pattern(&pattern)?;
            Ok(pattern)
        }
    }

    /// Find repeated action sequences above a minimum frequency.
    pub fn detect_patterns(&self, min_frequency: u32) -> Result<Vec<Pattern>, CortexError> {
        self.storage.list_patterns(min_frequency)
    }

    /// Lookup a pattern by its trigger string.
    pub fn get_pattern(&self, trigger: &str) -> Result<Option<Pattern>, CortexError> {
        self.storage.get_pattern(trigger)
    }

    /// Promote a detected pattern to a procedural MemObject.
    pub fn promote_pattern(&self, pattern: &Pattern) -> Result<MemObject, CortexError> {
        let mem = MemObjectBuilder::new(
            MemoryTier::Procedural,
            MemContent::Pattern {
                trigger: pattern.trigger.clone(),
                actions: pattern.actions.clone(),
                frequency: pattern.frequency,
            },
            MemSource::new("system"),
        )
        .salience(Salience::new(0.7))
        .build();

        self.storage.store_memory(&mem)?;
        Ok(mem)
    }
}
