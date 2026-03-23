//! Merge logic — apply remote SyncOps to the local database.
//!
//! Uses Last-Writer-Wins (LWW) per entity based on HLC ordering.
//! Tombstones prevent deleted entities from being recreated by stale ops.

use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::sync::oplog::{SyncOp, SyncPayload};
use crate::sync::state;
use crate::CortexError;
use rusqlite::Connection;

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
pub fn apply_op(
    op: &SyncOp,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
    sync_conn: &Connection,
) -> Result<MergeResult, CortexError> {
    match &op.payload {
        SyncPayload::MemoryUpsert { memory } => {
            let entity_type = "memory";
            let entity_id = memory.id;

            // Check tombstone
            if let Some(tomb_hlc) = state::is_tombstoned(sync_conn, entity_type, entity_id)? {
                if tomb_hlc >= op.hlc {
                    return Ok(MergeResult::Tombstoned);
                }
                // Op is newer than tombstone — allow recreation
            }

            // LWW check
            if let Some(local_hlc) = state::get_entity_hlc(sync_conn, entity_type, entity_id)? {
                if local_hlc >= op.hlc {
                    return Ok(MergeResult::Skipped);
                }
            }

            // Apply: upsert memory
            let exists = storage.get_memory(entity_id)?.is_some();

            // Content-hash dedup: only for new memories (not updates to existing ones)
            if !exists {
                if let Some(ref hash) = memory.content_hash {
                    if let Ok(Some(_existing)) = storage.find_by_content_hash(hash) {
                        return Ok(MergeResult::Deduplicated);
                    }
                }
            }

            if exists {
                storage.update_memory(memory)?;
            } else {
                storage.store_memory(memory)?;
            }

            // Update vector index
            if let Some(ref emb) = memory.embedding {
                index.insert_arc(entity_id, emb);
            }

            // Record HLC
            state::set_entity_hlc(sync_conn, entity_type, entity_id, &op.hlc)?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::MemoryDelete { id } => {
            let entity_type = "memory";

            // LWW: only delete if this op is newer
            if let Some(local_hlc) = state::get_entity_hlc(sync_conn, entity_type, *id)? {
                if local_hlc >= op.hlc {
                    return Ok(MergeResult::Skipped);
                }
            }

            // Apply delete
            storage.delete_memory(*id)?;
            index.remove(id);

            // Record tombstone and HLC
            state::set_tombstone(sync_conn, entity_type, *id, &op.hlc)?;
            state::set_entity_hlc(sync_conn, entity_type, *id, &op.hlc)?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::PersonUpsert { person } => {
            let entity_type = "person";

            if let Some(local_hlc) = state::get_entity_hlc(sync_conn, entity_type, person.id)? {
                if local_hlc >= op.hlc {
                    // Merge interaction_count: take max
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

            state::set_entity_hlc(sync_conn, entity_type, person.id, &op.hlc)?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::PersonDelete { id } => {
            let entity_type = "person";
            if let Some(local_hlc) = state::get_entity_hlc(sync_conn, entity_type, *id)? {
                if local_hlc >= op.hlc {
                    return Ok(MergeResult::Skipped);
                }
            }
            storage.delete_person(*id)?;
            state::set_tombstone(sync_conn, entity_type, *id, &op.hlc)?;
            state::set_entity_hlc(sync_conn, entity_type, *id, &op.hlc)?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::BeliefUpsert { belief } => {
            let entity_type = "belief";

            // Beliefs use CRDT merge: union observations, recompute probability
            if let Ok(Some(local_belief)) = storage.get_belief(&belief.key) {
                // Merge observation lists (add-only set by timestamp)
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

                // Recompute probability from merged observations
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

            state::set_entity_hlc(sync_conn, entity_type, belief.id, &op.hlc)?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::PatternUpsert { pattern } => {
            let entity_type = "pattern";

            if let Ok(Some(mut local_pattern)) = storage.get_pattern(&pattern.trigger) {
                // Merge: union actions, max frequency
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

            state::set_entity_hlc(sync_conn, entity_type, pattern.id, &op.hlc)?;
            Ok(MergeResult::Applied)
        }

        SyncPayload::LinkUpsert {
            source_id,
            target_id,
            relation,
            strength,
        } => {
            // Add-wins: store link, strength = max
            let link_relation = crate::types::LinkRelation::parse(relation)
                .unwrap_or(crate::types::LinkRelation::RelatedTo);
            storage.store_link(*source_id, *target_id, link_relation, *strength)?;
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
        let strength = match &obs.evidence {
            crate::belief::Evidence::Supports(s) => *s,
            crate::belief::Evidence::Contradicts(s) => *s,
        };

        let (likelihood_h, likelihood_not_h) = match &obs.evidence {
            crate::belief::Evidence::Supports(_) => {
                let lh = 0.5 + strength * 0.5;
                (lh, 1.0 - lh)
            }
            crate::belief::Evidence::Contradicts(_) => {
                let lh = 0.5 - strength * 0.5;
                (lh, 1.0 - lh)
            }
        };

        let numerator = likelihood_h * prob;
        let denominator = numerator + likelihood_not_h * (1.0 - prob);
        if denominator > 0.0 {
            prob = (numerator / denominator).clamp(0.001, 0.999);
        }
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
}
