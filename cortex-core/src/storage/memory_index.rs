use parking_lot::RwLock;
use std::collections::HashMap;
use uuid::Uuid;

/// Simple in-memory vector index using brute-force cosine similarity.
/// Can be swapped for HNSW later.
pub struct MemoryIndex {
    vectors: RwLock<HashMap<Uuid, Vec<f32>>>,
}

impl MemoryIndex {
    pub fn new() -> Self {
        Self {
            vectors: RwLock::new(HashMap::new()),
        }
    }

    pub fn insert(&self, id: Uuid, embedding: Vec<f32>) {
        self.vectors.write().insert(id, embedding);
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
    /// Returns (id, similarity_score) pairs sorted descending by score.
    pub fn search(&self, query: &[f32], limit: usize) -> Vec<(Uuid, f32)> {
        let vectors = self.vectors.read();
        let mut results: Vec<(Uuid, f32)> = vectors
            .iter()
            .filter_map(|(id, vec)| {
                let sim = cosine_similarity(query, vec);
                if sim.is_finite() {
                    Some((*id, sim))
                } else {
                    None
                }
            })
            .collect();

        results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(limit);
        results
    }

    /// Rebuild the index from an iterator of (id, embedding) pairs.
    pub fn rebuild(&self, entries: impl Iterator<Item = (Uuid, Vec<f32>)>) {
        let mut vectors = self.vectors.write();
        vectors.clear();
        for (id, embedding) in entries {
            vectors.insert(id, embedding);
        }
    }
}

impl Default for MemoryIndex {
    fn default() -> Self {
        Self::new()
    }
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f32 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let mut dot = 0.0f32;
    let mut norm_a = 0.0f32;
    let mut norm_b = 0.0f32;

    for (x, y) in a.iter().zip(b.iter()) {
        dot += x * y;
        norm_a += x * x;
        norm_b += y * y;
    }

    let denom = norm_a.sqrt() * norm_b.sqrt();
    if denom == 0.0 {
        0.0
    } else {
        dot / denom
    }
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
