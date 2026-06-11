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

/// Export all data from storage — the **local full-backup** path used by the
/// user-initiated `/export` endpoint and MCP backup. Includes every memory (including
/// Private) plus all derived entities, because the data goes to the user's own machine.
///
/// ⚠️ Do NOT use this on the sync path. Snapshots are cloud-bound; use [`export_for_sync`].
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

/// Export ONLY data that is safe to **sync to peers** (used by encrypted snapshots).
///
/// Two privacy guarantees, mirroring the oplog's sync boundary
/// (`SyncEngine::record_memory_event`):
/// 1. Only syncable (Public/Shared) memories are included — Private (the default) never
///    leaves the device, even inside an encrypted snapshot bound for another device.
/// 2. People, beliefs, and patterns are excluded entirely. They are derived aggregates
///    with no privacy provenance — a belief key, a person's notes, or a pattern trigger
///    can encode information distilled from Private memories — so we never let them cross
///    the sync boundary. Receiving devices re-derive them locally from synced memories.
pub fn export_for_sync(storage: &dyn StorageBackend) -> Result<ExportData, CortexError> {
    let tiers = [
        MemoryTier::Episodic,
        MemoryTier::Semantic,
        MemoryTier::Procedural,
    ];
    let mut memories = Vec::new();
    for tier in &tiers {
        let mems = storage.list_by_tier(*tier, 100_000)?;
        for mem in mems {
            if mem.privacy.is_syncable() {
                memories.push(mem);
            }
        }
    }

    Ok(ExportData {
        version: "cortex-export-v1".to_string(),
        exported_at: chrono::Utc::now().to_rfc3339(),
        memories,
        people: Vec::new(),
        beliefs: Vec::new(),
        patterns: Vec::new(),
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
                index.insert_arc(mem.id, emb);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Snapshots sync to peers, so `export_for_sync` must (1) drop Private memories and
    /// (2) carry no derived entities (people/beliefs/patterns). Both can encode
    /// information that must never cross the sync boundary.
    #[test]
    fn export_for_sync_drops_private_memories_and_all_entities() {
        let cortex = crate::Cortex::in_memory().unwrap();

        // A syncable (Public) memory — SHOULD be exported.
        let public = crate::types::MemObjectBuilder::new(
            crate::MemoryTier::Episodic,
            crate::MemContent::Text("public note".to_string()),
            crate::MemSource::new("test"),
        )
        .privacy(crate::PrivacyLevel::Public)
        .build();
        cortex.storage().store_memory(&public).unwrap();

        // A Private memory — must NOT be exported.
        let private = crate::types::MemObjectBuilder::new(
            crate::MemoryTier::Episodic,
            crate::MemContent::Text("PRIVATE_SECRET_NOTE".to_string()),
            crate::MemSource::new("test"),
        )
        .privacy(crate::PrivacyLevel::Private)
        .build();
        cortex.storage().store_memory(&private).unwrap();

        // Derived entities exist locally.
        cortex
            .add_fact("Alice", "works_at", "Stripe", 0.9, "test", None)
            .unwrap();
        cortex.observe_belief("user_has_secret", true, 0.8).unwrap();
        assert!(
            !cortex.storage().list_beliefs_above(0.0).unwrap().is_empty(),
            "precondition: a belief must exist locally"
        );

        let data = export_for_sync(cortex.storage()).unwrap();

        assert!(
            data.memories.iter().all(|m| m.privacy.is_syncable()),
            "no Private memory may be exported for sync"
        );
        assert!(
            !data.memories.is_empty(),
            "the Public memory must still be exported for bootstrap"
        );
        assert!(data.people.is_empty(), "people must NOT sync via snapshot");
        assert!(data.beliefs.is_empty(), "beliefs must NOT sync via snapshot");
        assert!(data.patterns.is_empty(), "patterns must NOT sync via snapshot");
    }
}
