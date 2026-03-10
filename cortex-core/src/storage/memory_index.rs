use parking_lot::RwLock;
use std::collections::HashMap;
use uuid::Uuid;

/// Normalized vector entry — precomputed norm for O(1) cosine similarity.
struct NormalizedEntry {
    embedding: Vec<f32>,
    norm: f32,
}

/// In-memory vector index with precomputed norms and partial sort.
/// Brute-force cosine similarity, optimized for < 100K vectors.
/// Can be swapped for HNSW (instant-distance) for larger datasets.
pub struct MemoryIndex {
    vectors: RwLock<HashMap<Uuid, NormalizedEntry>>,
}

impl MemoryIndex {
    pub fn new() -> Self {
        Self {
            vectors: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, id: Uuid, embedding: Vec<f32>) {
        let norm = l2_norm(&embedding);
        self.vectors.write().insert(id, NormalizedEntry { embedding, norm });
    }

    pub fn remove(&self, id: &Uuid) {
        self.vectors.write().remove(id);
    }

    pub fn len(&self) -> usize {
        self.vectors.read().len()
    }

    pub fn is_empty(&self) -> bool {
        self.vectors.read().is_empty()
    }

    /// Search for the top-k most similar vectors by cosine similarity.
    /// Uses precomputed norms to avoid redundant sqrt per query.
    /// Uses partial sort (select_nth) instead of full sort for large collections.
    pub fn search(&self, query: &[f32], limit: usize) -> Vec<(Uuid, f32)> {
        let query_norm = l2_norm(query);
        if query_norm == 0.0 {
            return Vec::new();
        }

        let vectors = self.vectors.read();
        let mut results: Vec<(Uuid, f32)> = vectors
            .iter()
            .filter_map(|(id, entry)| {
                if entry.norm == 0.0 {
                    return None;
                }
                let dot = dot_product(query, &entry.embedding);
                let sim = dot / (query_norm * entry.norm);
                if sim.is_finite() {
                    Some((*id, sim))
                } else {
                    None
                }
            })
            .collect();

        // Partial sort: only find top-k, O(n) instead of O(n log n)
        if results.len() > limit {
            results.select_nth_unstable_by(limit, |a, b| {
                b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal)
            });
            results.truncate(limit);
            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        } else {
            results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        }

        results
    }

    /// Rebuild the index from an iterator of (id, embedding) pairs.
    pub fn rebuild(&self, entries: impl Iterator<Item = (Uuid, Vec<f32>)>) {
        let mut vectors = self.vectors.write();
        vectors.clear();
        for (id, embedding) in entries {
            let norm = l2_norm(&embedding);
            vectors.insert(id, NormalizedEntry { embedding, norm });
        }
    }
}

impl Default for MemoryIndex {
    fn default() -> Self {
        Self::new()
    }
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
    fn test_search() {
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
}
