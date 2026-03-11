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
/// HNSW for large stable collections. Automatically switches strategy.
pub struct MemoryIndex {
    inner: RwLock<IndexInner>,
}

struct IndexInner {
    /// Raw embeddings (always maintained, source of truth)
    entries: HashMap<Uuid, NormalizedEntry>,
    /// HNSW index for fast ANN search (rebuilt on demand)
    hnsw: Option<HnswMap<Point, Uuid>>,
    /// Number of mutations since last HNSW build
    mutations_since_build: usize,
    /// Size at last HNSW build
    size_at_build: usize,
}

struct NormalizedEntry {
    embedding: Vec<f32>,
    norm: f32,
}

/// Use HNSW when collection is large enough to benefit.
const HNSW_MIN_SIZE: usize = 1000;
/// Rebuild HNSW when mutations exceed 10% of collection size.
const HNSW_REBUILD_RATIO: f32 = 0.10;

impl MemoryIndex {
    pub fn new() -> Self {
        Self {
            inner: RwLock::new(IndexInner {
                entries: HashMap::new(),
                hnsw: None,
                mutations_since_build: 0,
                size_at_build: 0,
            }),
        }
    }

    pub fn insert(&self, id: Uuid, embedding: Vec<f32>) {
        let norm = l2_norm(&embedding);
        let mut inner = self.inner.write();
        inner.entries.insert(id, NormalizedEntry { embedding, norm });
        inner.mutations_since_build += 1;
    }

    pub fn remove(&self, id: &Uuid) {
        let mut inner = self.inner.write();
        if inner.entries.remove(id).is_some() {
            inner.mutations_since_build += 1;
        }
    }

    pub fn len(&self) -> usize {
        self.inner.read().entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.read().entries.is_empty()
    }

    /// Search for the top-k most similar vectors.
    /// Uses HNSW for large stable collections, brute-force otherwise.
    pub fn search(&self, query: &[f32], limit: usize) -> Vec<(Uuid, f32)> {
        let mut inner = self.inner.write();
        let n = inner.entries.len();

        if n == 0 {
            return Vec::new();
        }

        // Small collection or high mutation rate: brute-force
        if n < HNSW_MIN_SIZE {
            return brute_force_search(&inner.entries, query, limit);
        }

        // Check if HNSW needs rebuild
        let needs_rebuild = inner.hnsw.is_none()
            || inner.mutations_since_build as f32 > inner.size_at_build as f32 * HNSW_REBUILD_RATIO;

        if needs_rebuild {
            rebuild_hnsw(&mut inner);
        }

        // HNSW search
        if let Some(ref hnsw) = inner.hnsw {
            let query_point = Point::new(query.to_vec());
            let mut search = Search::default();
            let results = hnsw.search(&query_point, &mut search);

            results
                .take(limit)
                .map(|item| {
                    let id = *item.value;
                    let similarity = 1.0 - item.distance;
                    (id, similarity)
                })
                .collect()
        } else {
            brute_force_search(&inner.entries, query, limit)
        }
    }

    /// Force HNSW rebuild (call after bulk operations).
    pub fn build_index(&self) {
        let mut inner = self.inner.write();
        if inner.entries.len() >= HNSW_MIN_SIZE {
            rebuild_hnsw(&mut inner);
        }
    }

    /// Rebuild the index from an iterator of (id, embedding) pairs.
    pub fn rebuild(&self, entries: impl Iterator<Item = (Uuid, Vec<f32>)>) {
        let mut inner = self.inner.write();
        inner.entries.clear();
        for (id, embedding) in entries {
            let norm = l2_norm(&embedding);
            inner.entries.insert(id, NormalizedEntry { embedding, norm });
        }
        inner.mutations_since_build = inner.entries.len(); // force rebuild on next search
        inner.hnsw = None;
    }
}

impl Default for MemoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn rebuild_hnsw(inner: &mut IndexInner) {
    let points: Vec<Point> = inner.entries.values().map(|e| Point::new(e.embedding.clone())).collect();
    let ids: Vec<Uuid> = inner.entries.keys().copied().collect();
    let hnsw = Builder::default().ef_construction(40).build(points, ids);
    inner.hnsw = Some(hnsw);
    inner.mutations_since_build = 0;
    inner.size_at_build = inner.entries.len();
}

fn brute_force_search(
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

/// Dot product — auto-vectorized by LLVM for f32 slices.
#[inline]
fn dot_product(a: &[f32], b: &[f32]) -> f32 {
    a.iter().zip(b.iter()).map(|(x, y)| x * y).sum()
}

/// L2 norm — precomputed once per insert.
#[inline]
fn l2_norm(v: &[f32]) -> f32 {
    v.iter().map(|x| x * x).sum::<f32>().sqrt()
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
    fn test_remove() {
        let index = MemoryIndex::new();
        let id = Uuid::new_v4();
        index.insert(id, vec![1.0, 0.0]);
        index.remove(&id);
        assert!(index.is_empty());
    }
}
