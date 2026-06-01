//! Merge logic — apply remote SyncOps to the local database.
//!
//! Uses Last-Writer-Wins (LWW) per entity based on HLC ordering.
//! Tombstones prevent deleted entities from being recreated by stale ops.

use crate::storage::memory_index::MemoryIndex;
use crate::storage::sqlite::SqliteStorage;
use crate::storage::traits::StorageBackend;
use crate::sync::oplog::{SyncOp, SyncPayload};
use crate::sync::state::{self, EntityType};
use crate::CortexError;

/// Result of merging a single operation.
#[derive(Debug)]
pub enum MergeResult {
    /// Applied the operation (created or updated).
    Applied,
    /// Skipped — local version is newer (LWW).
    Skipped,
    /// Skipped — entity was deleted locally with a newer HLC.
    Tombstoned,
    /// Skipped — duplicate content (content_hash match).
    Deduplicated,
}

/// Apply a single SyncOp to the local database.
/// Uses `with_write_conn` for sync state (HLC, tombstones) and StorageBackend for data.
pub fn apply_op(
    op: &SyncOp,
    storage: &SqliteStorage,
    index: &MemoryIndex,
) -> Result<MergeResult, CortexError> {
    match &op.payload {
        SyncPayload::MemoryUpsert { memory } => {
            let entity_type = EntityType::Memory;
            let entity_id = memory.id;

            // Check tombstone (sync state query)
            let tomb = storage.with_write_conn(|c| state::is_tombstoned(c, entity_type, entity_id))?;
            if let Some(tomb_hlc) = tomb {
                if tomb_hlc >= op.hlc {
                    return Ok(MergeResult::Tombstoned);
                }
            }

            // LWW check (sync state query)
            let local = storage.with_write_conn(|c| state::get_entity_hlc(c, entity_type, entity_id))?;
            if let Some(local_hlc) = local {
                if local_hlc >= op.hlc {
                    return Ok(MergeResult::Skipped);
                }
            }

            // Apply: upsert memory (storage operations)
            let exists = storage.get_memory(entity_id)?.is_some();

            if !exists {
                if let Some(ref hash) = memory.content_hash {
                    if let Ok(Some(_)) = storage.find_by_content_hash(hash) {
                        // Still record HLC so future ops with lower HLC are correctly skipped
                        storage.with_write_conn(|c| state::set_entity_hlc(c, entity_type, entity_id, &op.hlc))?;
                        return Ok(MergeResult::Deduplicated);
                    }
                }
            }

            if exists {
                storage.update_memory(memory)?;
            } else {
                storage.store_memory(memory)?;
            }

            if let Some(ref emb) = memory.embedding {
                index.insert_arc(entity_id, emb);
            }

            // Record HLC (sync state update)
            storage.with_write_conn(|c| state::set_entity_hlc(c, entity_type, entity_id, &op.hlc))?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::MemoryDelete { id } => {
            let entity_type = EntityType::Memory;

            let local = storage.with_write_conn(|c| state::get_entity_hlc(c, entity_type, *id))?;
            if let Some(local_hlc) = local {
                if local_hlc >= op.hlc {
                    return Ok(MergeResult::Skipped);
                }
            }

            storage.delete_memory(*id)?;
            index.remove(id);

            storage.with_write_conn(|c| {
                state::set_tombstone(c, entity_type, *id, &op.hlc)?;
                state::set_entity_hlc(c, entity_type, *id, &op.hlc)
            })?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::PersonUpsert { person } => {
            let entity_type = EntityType::Person;

            let local = storage.with_write_conn(|c| state::get_entity_hlc(c, entity_type, person.id))?;
            if let Some(local_hlc) = local {
                if local_hlc >= op.hlc {
                    if let Ok(Some(local_person)) = storage.get_person(person.id) {
                        if person.interaction_count > local_person.interaction_count {
                            let mut merged = local_person;
                            merged.interaction_count = person.interaction_count;
                            if person.last_seen > merged.last_seen {
                                merged.last_seen = person.last_seen;
                            }
                            storage.update_person(&merged)?;
                        }
                    }
                    return Ok(MergeResult::Skipped);
                }
            }

            if storage.get_person(person.id)?.is_some() {
                storage.update_person(person)?;
            } else {
                storage.store_person(person)?;
            }

            storage.with_write_conn(|c| state::set_entity_hlc(c, entity_type, person.id, &op.hlc))?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::PersonDelete { id } => {
            let entity_type = EntityType::Person;
            let local = storage.with_write_conn(|c| state::get_entity_hlc(c, entity_type, *id))?;
            if let Some(local_hlc) = local {
                if local_hlc >= op.hlc {
                    return Ok(MergeResult::Skipped);
                }
            }
            storage.delete_person(*id)?;
            storage.with_write_conn(|c| {
                state::set_tombstone(c, entity_type, *id, &op.hlc)?;
                state::set_entity_hlc(c, entity_type, *id, &op.hlc)
            })?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::BeliefUpsert { belief } => {
            let entity_type = EntityType::Belief;

            if let Ok(Some(local_belief)) = storage.get_belief(&belief.key) {
                let mut merged = local_belief.clone();
                let local_timestamps: std::collections::HashSet<_> = merged
                    .observations
                    .iter()
                    .map(|o| o.timestamp.timestamp_millis())
                    .collect();

                for obs in &belief.observations {
                    if !local_timestamps.contains(&obs.timestamp.timestamp_millis()) {
                        merged.observations.push(obs.clone());
                    }
                }

                if !merged.observations.is_empty() {
                    merged.probability = recompute_belief_probability(&merged.observations);
                }

                if merged.last_updated < belief.last_updated {
                    merged.last_updated = belief.last_updated;
                }

                storage.update_belief(&merged)?;
            } else {
                storage.store_belief(belief)?;
            }

            storage.with_write_conn(|c| state::set_entity_hlc(c, entity_type, belief.id, &op.hlc))?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::PatternUpsert { pattern } => {
            let entity_type = EntityType::Pattern;

            if let Ok(Some(mut local_pattern)) = storage.get_pattern(&pattern.trigger) {
                for action in &pattern.actions {
                    if !local_pattern.actions.contains(action) {
                        local_pattern.actions.push(action.clone());
                    }
                }
                local_pattern.frequency = local_pattern.frequency.max(pattern.frequency);
                if pattern.last_seen > local_pattern.last_seen {
                    local_pattern.last_seen = pattern.last_seen;
                }
                storage.update_pattern(&local_pattern)?;
            } else {
                storage.store_pattern(pattern)?;
            }

            storage.with_write_conn(|c| state::set_entity_hlc(c, entity_type, pattern.id, &op.hlc))?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::LinkUpsert {
            source_id,
            target_id,
            relation,
            strength,
        } => {
            storage.store_link(*source_id, *target_id, *relation, *strength)?;
            Ok(MergeResult::Applied)
        }
    }
}

/// Recompute belief probability by replaying all observations from prior=0.5.
fn recompute_belief_probability(observations: &[crate::belief::Observation]) -> f32 {
    let mut prob = 0.5_f32;
    let mut sorted = observations.to_vec();
    sorted.sort_by_key(|o| o.timestamp);
    for obs in &sorted {
        prob = crate::belief::bayesian_update(prob, &obs.evidence);
    }
    prob
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_recompute_neutral() {
        let prob = recompute_belief_probability(&[]);
        assert!((prob - 0.5).abs() < 0.001);
    }

    #[test]
    fn test_recompute_with_support() {
        let obs = vec![crate::belief::Observation {
            timestamp: chrono::Utc::now(),
            evidence: crate::belief::Evidence::Supports(0.8),
            prior: 0.5,
            posterior: 0.8, // not used in recompute
        }];
        let prob = recompute_belief_probability(&obs);
        assert!(prob > 0.5);
    }

    // ── apply_op merge semantics ────────────────────────────────────────────
    use crate::sync::hlc::HlcTimestamp;
    use uuid::Uuid;

    fn belief_op(key: &str, obs_ms: i64, wall_ms: u64, device: &str) -> SyncOp {
        use crate::belief::{Belief, Evidence, Observation};
        let ts = chrono::DateTime::from_timestamp_millis(obs_ms).unwrap();
        let belief = Belief {
            id: Uuid::new_v4(),
            key: key.to_string(),
            probability: 0.5,
            observations: vec![Observation {
                timestamp: ts,
                evidence: Evidence::Supports(0.8),
                prior: 0.5,
                posterior: 0.8,
            }],
            last_updated: ts,
        };
        SyncOp {
            op_id: Uuid::new_v4(),
            hlc: HlcTimestamp::new(wall_ms, 0, device),
            payload: SyncPayload::BeliefUpsert { belief },
        }
    }

    fn obs_timestamps(s: &SqliteStorage, key: &str) -> Vec<i64> {
        let mut ts: Vec<i64> = s
            .get_belief(key)
            .unwrap()
            .unwrap()
            .observations
            .iter()
            .map(|o| o.timestamp.timestamp_millis())
            .collect();
        ts.sort_unstable();
        ts
    }

    #[test]
    fn test_belief_upsert_is_idempotent() {
        let s = SqliteStorage::open_in_memory().unwrap();
        let idx = MemoryIndex::new();
        let op = belief_op("likes_rust", 1000, 100, "dev-a");
        apply_op(&op, &s, &idx).unwrap();
        apply_op(&op, &s, &idx).unwrap(); // replay
        assert_eq!(
            obs_timestamps(&s, "likes_rust"),
            vec![1000],
            "replaying the same op must not duplicate observations"
        );
    }

    #[test]
    fn test_belief_upsert_unions_observations_regardless_of_hlc() {
        let s = SqliteStorage::open_in_memory().unwrap();
        let idx = MemoryIndex::new();
        // Local op carries a HIGH hlc.
        apply_op(&belief_op("k", 2000, 500, "dev-a"), &s, &idx).unwrap();
        // Remote op carries a LOWER hlc but a distinct observation.
        apply_op(&belief_op("k", 1000, 100, "dev-b"), &s, &idx).unwrap();
        // Beliefs are an add-only CRDT — the lower-HLC observation must be merged,
        // not skipped. (Guards against "adding an LWW skip" to BeliefUpsert.)
        assert_eq!(obs_timestamps(&s, "k"), vec![1000, 2000]);
    }

    #[test]
    fn test_belief_upsert_is_commutative() {
        let ab = {
            let s = SqliteStorage::open_in_memory().unwrap();
            let idx = MemoryIndex::new();
            apply_op(&belief_op("k", 1000, 100, "a"), &s, &idx).unwrap();
            apply_op(&belief_op("k", 2000, 200, "b"), &s, &idx).unwrap();
            obs_timestamps(&s, "k")
        };
        let ba = {
            let s = SqliteStorage::open_in_memory().unwrap();
            let idx = MemoryIndex::new();
            apply_op(&belief_op("k", 2000, 200, "b"), &s, &idx).unwrap();
            apply_op(&belief_op("k", 1000, 100, "a"), &s, &idx).unwrap();
            obs_timestamps(&s, "k")
        };
        assert_eq!(ab, ba, "merged observation set must be order-independent");
    }

    #[test]
    fn test_memory_upsert_skips_older_hlc() {
        use crate::types::{MemContent, MemObjectBuilder, MemSource, MemoryTier};
        let s = SqliteStorage::open_in_memory().unwrap();
        let idx = MemoryIndex::new();
        let mut mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text("v1".into()),
            MemSource::new("t"),
        )
        .build();
        let id = mem.id;

        let op_new = SyncOp {
            op_id: Uuid::new_v4(),
            hlc: HlcTimestamp::new(200, 0, "a"),
            payload: SyncPayload::MemoryUpsert { memory: mem.clone() },
        };
        assert!(matches!(
            apply_op(&op_new, &s, &idx).unwrap(),
            MergeResult::Applied
        ));

        // An op with an older HLC must not overwrite, even with changed content.
        mem.content = MemContent::Text("v2".into());
        let op_old = SyncOp {
            op_id: Uuid::new_v4(),
            hlc: HlcTimestamp::new(100, 0, "b"),
            payload: SyncPayload::MemoryUpsert { memory: mem.clone() },
        };
        assert!(matches!(
            apply_op(&op_old, &s, &idx).unwrap(),
            MergeResult::Skipped
        ));

        match s.get_memory(id).unwrap().unwrap().content {
            MemContent::Text(t) => assert_eq!(t, "v1"),
            other => panic!("unexpected content: {other:?}"),
        }
    }
}
