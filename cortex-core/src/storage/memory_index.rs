use instant_distance::{Builder, HnswMap, Search};
use parking_lot::RwLock;
use std::collections::HashMap;
use uuid::Uuid;

/// Pre-normalized point for HNSW — avoids redundant norm computation during build.
#[derive(Clone)]
struct Point {
    embedding: Vec<f32>,
    norm: f32,
}

impl Point {
    fn new(embedding: Vec<f32>) -> Self {
        let norm = embedding.iter().map(|x| x * x).sum::<f32>().sqrt();
        Self { embedding, norm }
    }
}

impl instant_distance::Point for Point {
    fn distance(&self, other: &Self) -> f32 {
        // Cosine distance with precomputed norms (no redundant sqrt)
        let denom = self.norm * other.norm;
        if denom == 0.0 {
            return 1.0;
        }
        let dot: f32 = self.embedding.iter().zip(other.embedding.iter()).map(|(a, b)| a * b).sum();
        1.0 - (dot / denom)
    }
}

/// Hybrid vector index: brute-force for small/mutating collections,
/// HNSW for large stable collections. New insertions go into a pending buffer
/// that is searched via brute-force and merged with HNSW results, avoiding
/// costly full rebuilds on every search.
pub struct MemoryIndex {
    inner: RwLock<IndexInner>,
}

/// Configuration for HNSW index thresholds and build parameters.
#[derive(Clone, Debug)]
pub struct IndexConfig {
    /// Minimum collection size before using HNSW (below this, brute-force is used).
    pub min_size: usize,
    /// Rebuild HNSW when pending buffer exceeds this fraction of stable size.
    pub pending_ratio: f32,
    /// Rebuild HNSW when removals exceed this fraction of stable size.
    pub removal_ratio: f32,
    /// HNSW ef_construction parameter (higher = better recall, slower build).
    pub ef_construction: usize,
}

impl Default for IndexConfig {
    fn default() -> Self {
        Self {
            min_size: HNSW_MIN_SIZE,
            pending_ratio: HNSW_PENDING_RATIO,
            removal_ratio: HNSW_REMOVAL_RATIO,
            ef_construction: 40,
        }
    }
}

struct IndexInner {
    /// Stable entries included in the current HNSW build (source of truth together with pending)
    stable: HashMap<Uuid, NormalizedEntry>,
    /// New entries not yet in HNSW — searched via brute-force and merged
    pending: HashMap<Uuid, NormalizedEntry>,
    /// HNSW index for fast ANN search on stable entries
    hnsw: Option<HnswMap<Point, Uuid>>,
    /// Total removals since last HNSW build (stale entries in HNSW)
    removals_since_build: usize,
    /// Size of stable set at last HNSW build
    size_at_build: usize,
    /// Index configuration
    config: IndexConfig,
}

struct NormalizedEntry {
    embedding: Vec<f32>,
    norm: f32,
}

/// Use HNSW when collection is large enough to benefit.
const HNSW_MIN_SIZE: usize = 1000;
/// Rebuild HNSW when pending buffer exceeds 20% of stable size.
const HNSW_PENDING_RATIO: f32 = 0.20;
/// Rebuild HNSW when removals exceed 10% of stable size.
const HNSW_REMOVAL_RATIO: f32 = 0.10;

impl MemoryIndex {
    pub fn new() -> Self {
        Self::with_config(IndexConfig::default())
    }

    pub fn with_config(config: IndexConfig) -> Self {
        Self {
            inner: RwLock::new(IndexInner {
                stable: HashMap::new(),
                pending: HashMap::new(),
                hnsw: None,
                removals_since_build: 0,
                size_at_build: 0,
                config,
            }),
        }
    }

    /// Insert a new embedding. Goes into the pending buffer (cheap, no rebuild).
    pub fn insert(&self, id: Uuid, embedding: Vec<f32>) {
        let norm = l2_norm(&embedding);
        let mut inner = self.inner.write();
        // Remove from stable if updating an existing entry
        inner.stable.remove(&id);
        inner.pending.insert(id, NormalizedEntry { embedding, norm });
    }

    /// Remove an embedding.
    pub fn remove(&self, id: &Uuid) {
        let mut inner = self.inner.write();
        if inner.pending.remove(id).is_none() {
            // Was in stable — mark as removal so HNSW knows it's stale
            if inner.stable.remove(id).is_some() {
                inner.removals_since_build += 1;
            }
        }
    }

    pub fn len(&self) -> usize {
        let inner = self.inner.read();
        inner.stable.len() + inner.pending.len()
    }

    pub fn is_empty(&self) -> bool {
        let inner = self.inner.read();
        inner.stable.is_empty() && inner.pending.is_empty()
    }

    /// Search for the top-k most similar vectors.
    /// Merges HNSW results (stable) with brute-force results (pending).
    pub fn search(&self, query: &[f32], limit: usize) -> Vec<(Uuid, f32)> {
        let mut inner = self.inner.write();
        let total = inner.stable.len() + inner.pending.len();

        if total == 0 {
            return Vec::new();
        }

        // Small collection: brute-force everything
        if total < inner.config.min_size {
            return brute_force_search_both(&inner.stable, &inner.pending, query, limit);
        }

        // Check if HNSW needs a full rebuild:
        // 1. No HNSW yet
        // 2. Too many removals (stale entries in HNSW)
        let needs_rebuild = inner.hnsw.is_none()
            || (inner.size_at_build > 0
                && inner.removals_since_build as f32 > inner.size_at_build as f32 * inner.config.removal_ratio);

        // Check if pending buffer should be flushed into stable + rebuild
        let pending_overflow = inner.hnsw.is_some()
            && inner.size_at_build > 0
            && inner.pending.len() as f32 > inner.size_at_build as f32 * inner.config.pending_ratio;

        if needs_rebuild || pending_overflow {
            flush_and_rebuild(&mut inner);
        }

        // Merge: HNSW search on stable + brute-force on pending
        let mut results: Vec<(Uuid, f32)> = Vec::new();

        // HNSW search on stable entries
        if let Some(ref hnsw) = inner.hnsw {
            let query_point = Point::new(query.to_vec());
            let mut search = Search::default();
            let hnsw_results = hnsw.search(&query_point, &mut search);

            for item in hnsw_results.take(limit * 2) {
                let id = *item.value;
                // Skip entries that were removed after the HNSW build
                if inner.stable.contains_key(&id) {
                    let similarity = 1.0 - item.distance;
                    results.push((id, similarity));
                }
            }
        }

        // Brute-force on pending buffer
        if !inner.pending.is_empty() {
            let pending_results = brute_force_search_map(&inner.pending, query, limit);
            results.extend(pending_results);
        }

        // Merge, deduplicate, sort, truncate
        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.dedup_by_key(|r| r.0);
        results.truncate(limit);
        results
    }

    /// Force HNSW rebuild (call after bulk operations).
    pub fn build_index(&self) {
        let mut inner = self.inner.write();
        let total = inner.stable.len() + inner.pending.len();
        if total >= inner.config.min_size {
            flush_and_rebuild(&mut inner);
        }
    }

    /// Rebuild the index from an iterator of (id, embedding) pairs.
    pub fn rebuild(&self, entries: impl Iterator<Item = (Uuid, Vec<f32>)>) {
        let mut inner = self.inner.write();
        inner.stable.clear();
        inner.pending.clear();
        for (id, embedding) in entries {
            let norm = l2_norm(&embedding);
            inner.pending.insert(id, NormalizedEntry { embedding, norm });
        }
        inner.hnsw = None;
        inner.removals_since_build = 0;
        inner.size_at_build = 0;
    }
}

impl Default for MemoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

/// Flush pending entries into stable, then rebuild HNSW from all stable entries.
fn flush_and_rebuild(inner: &mut IndexInner) {
    // Move all pending into stable
    for (id, entry) in inner.pending.drain() {
        inner.stable.insert(id, entry);
    }

    let points: Vec<Point> = inner.stable.values().map(|e| Point::new(e.embedding.clone())).collect();
    let ids: Vec<Uuid> = inner.stable.keys().copied().collect();
    let hnsw = Builder::default().ef_construction(inner.config.ef_construction).build(points, ids);
    inner.hnsw = Some(hnsw);
    inner.removals_since_build = 0;
    inner.size_at_build = inner.stable.len();
}

/// Brute-force search across both stable and pending maps.
fn brute_force_search_both(
    stable: &HashMap<Uuid, NormalizedEntry>,
    pending: &HashMap<Uuid, NormalizedEntry>,
    query: &[f32],
    limit: usize,
) -> Vec<(Uuid, f32)> {
    let query_norm = l2_norm(query);
    if query_norm == 0.0 {
        return Vec::new();
    }

    let mut results: Vec<(Uuid, f32)> = stable
        .iter()
        .chain(pending.iter())
        .filter_map(|(id, entry)| {
            if entry.norm == 0.0 {
                return None;
            }
            let dot = dot_product(query, &entry.embedding);
            let sim = dot / (query_norm * entry.norm);
            if sim.is_finite() { Some((*id, sim)) } else { None }
        })
        .collect();

    if results.len() > limit {
        results.select_nth_unstable_by(limit, |a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
    }
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Brute-force search on a single map.
fn brute_force_search_map(
    entries: &HashMap<Uuid, NormalizedEntry>,
    query: &[f32],
    limit: usize,
) -> Vec<(Uuid, f32)> {
    let query_norm = l2_norm(query);
    if query_norm == 0.0 {
        return Vec::new();
    }

    let mut results: Vec<(Uuid, f32)> = entries
        .iter()
        .filter_map(|(id, entry)| {
            if entry.norm == 0.0 {
                return None;
            }
            let dot = dot_product(query, &entry.embedding);
            let sim = dot / (query_norm * entry.norm);
            if sim.is_finite() { Some((*id, sim)) } else { None }
        })
        .collect();

    if results.len() > limit {
        results.select_nth_unstable_by(limit, |a, b| {
            b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
        });
        results.truncate(limit);
    }
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results
}

/// Dot product — 4-lane accumulator for better SIMD autovectorization by LLVM.
#[inline]
fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    let len = a.len().min(b.len());
    let chunks = len / 4;
    let mut acc = [0.0f32; 4];

    for i in 0..chunks {
        let base = i * 4;
        acc[0] += a[base] * b[base];
        acc[1] += a[base + 1] * b[base + 1];
        acc[2] += a[base + 2] * b[base + 2];
        acc[3] += a[base + 3] * b[base + 3];
    }

    let mut sum = acc[0] + acc[1] + acc[2] + acc[3];
    for i in (chunks * 4)..len {
        sum += a[i] * b[i];
    }
    sum
}

/// L2 norm — precomputed once per insert.
#[inline]
/// L2 norm — uses dot_product for consistent SIMD optimization.
#[inline]
fn l2_norm(v: &[f32]) -> f32 {
    dot_product(v, v).sqrt()
}

/// Cosine similarity (standalone, for external use).
pub fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }
    let dot = dot_product(a, b);
    let denom = l2_norm(a) * l2_norm(b);
    if denom == 0.0 { 0.0 } else { dot / denom }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_identical() {
        let v = vec![1.0, 0.0, 0.0];
        let sim = cosine_similarity(&v, &v);
        assert!((sim - 1.0).abs() < 1e-6);
    }

    #[test]
    fn test_cosine_orthogonal() {
        let a = vec![1.0, 0.0];
        let b = vec![0.0, 1.0];
        let sim = cosine_similarity(&a, &b);
        assert!(sim.abs() < 1e-6);
    }

    #[test]
    fn test_search_brute_force() {
        let index = MemoryIndex::new();
        let id1 = Uuid::new_v4();
        let id2 = Uuid::new_v4();
        let id3 = Uuid::new_v4();

        index.insert(id1, vec![1.0, 0.0, 0.0]);
        index.insert(id2, vec![0.9, 0.1, 0.0]);
        index.insert(id3, vec![0.0, 0.0, 1.0]);

        let results = index.search(&[1.0, 0.0, 0.0], 2);
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].0, id1);
    }

    #[test]
    fn test_search_hnsw_large() {
        let index = MemoryIndex::new();
        let mut target_id = Uuid::new_v4();

        // Insert above HNSW threshold
        for i in 0..1500 {
            let id = Uuid::new_v4();
            if i == 0 {
                target_id = id;
                index.insert(id, vec![1.0, 0.0, 0.0]);
            } else {
                let x = (i as f32 * 0.1).sin();
                let y = (i as f32 * 0.1).cos();
                let z = (i as f32 * 0.2).sin();
                index.insert(id, vec![x, y, z]);
            }
        }

        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(!results.is_empty());
        assert!(results.iter().any(|(id, _)| *id == target_id));
    }

    #[test]
    fn test_search_pending_found() {
        let index = MemoryIndex::new();

        // Build a stable HNSW first
        for i in 0..1200 {
            let id = Uuid::new_v4();
            let x = (i as f32 * 0.1).sin();
            let y = (i as f32 * 0.1).cos();
            let z = (i as f32 * 0.2).sin();
            index.insert(id, vec![x, y, z]);
        }
        index.build_index();

        // Now add a pending entry that should be the best match
        let target_id = Uuid::new_v4();
        index.insert(target_id, vec![1.0, 0.0, 0.0]);

        let results = index.search(&[1.0, 0.0, 0.0], 3);
        assert!(results.iter().any(|(id, _)| *id == target_id),
            "Pending entry should be found in search results");
    }

    #[test]
    fn test_remove() {
        let index = MemoryIndex::new();
        let id = Uuid::new_v4();
        index.insert(id, vec![1.0, 0.0]);
        index.remove(&id);
        assert!(index.is_empty());
    }

    #[test]
    fn test_remove_from_stable() {
        let index = MemoryIndex::new();

        // Build HNSW with entries
        for i in 0..1200 {
            let id = Uuid::new_v4();
            index.insert(id, vec![(i as f32).sin(), (i as f32).cos(), 0.0]);
        }
        let target = Uuid::new_v4();
        index.insert(target, vec![1.0, 0.0, 0.0]);
        index.build_index();

        // Remove target from stable
        index.remove(&target);

        // Should not appear in results
        let results = index.search(&[1.0, 0.0, 0.0], 5);
        assert!(!results.iter().any(|(id, _)| *id == target),
            "Removed stable entry should not appear in results");
    }
}
