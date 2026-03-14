//! Shared export/import logic for Cortex data.

use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;
use serde::{Deserialize, Serialize};

/// Report from an import operation.
#[derive(Debug, Clone, Serialize)]
pub struct ImportReport {
    pub memories: usize,
    pub people: usize,
    pub beliefs: usize,
}

/// Full export payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportData {
    pub version: String,
    pub exported_at: String,
    pub memories: Vec<MemObject>,
    pub people: Vec<crate::people::Person>,
    pub beliefs: Vec<crate::belief::Belief>,
    pub patterns: Vec<crate::procedural::Pattern>,
}

/// Import payload (all fields optional for partial imports).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ImportData {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub memories: Option<Vec<MemObject>>,
    #[serde(default)]
    pub people: Option<Vec<crate::people::Person>>,
    #[serde(default)]
    pub beliefs: Option<Vec<crate::belief::Belief>>,
}

/// Export all data from storage.
pub fn export_all(storage: &dyn StorageBackend) -> Result<ExportData, CortexError> {
    let tiers = [
        MemoryTier::Episodic,
        MemoryTier::Semantic,
        MemoryTier::Procedural,
    ];
    let mut memories = Vec::new();
    for tier in &tiers {
        let mems = storage.list_by_tier(*tier, 100_000)?;
        memories.extend(mems);
    }

    let people = storage.list_people()?;
    let beliefs = storage.list_beliefs_above(0.0)?;
    let patterns = storage.list_patterns(0)?;

    Ok(ExportData {
        version: "cortex-export-v1".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        memories,
        people,
        beliefs,
        patterns,
    })
}

/// Import data into storage. Returns counts of imported items.
pub fn import_all(
    storage: &dyn StorageBackend,
    index: &crate::storage::memory_index::MemoryIndex,
    data: ImportData,
) -> Result<ImportReport, CortexError> {
    let mut report = ImportReport {
        memories: 0,
        people: 0,
        beliefs: 0,
    };

    if let Some(memories) = data.memories {
        report.memories = memories.len();
        for mem in &memories {
            storage.store_memory(mem)?;
            if let Some(ref emb) = mem.embedding {
                index.insert(mem.id, emb.clone());
            }
        }
    }

    if let Some(people) = data.people {
        report.people = people.len();
        for person in &people {
            storage.store_person(person)?;
        }
    }

    if let Some(beliefs) = data.beliefs {
        report.beliefs = beliefs.len();
        for belief in &beliefs {
            storage.store_belief(belief)?;
        }
    }

    Ok(report)
}
