//! Tests for MemoryIndex — hybrid HNSW + brute-force vector search.

use cortex_core::storage::memory_index::{cosine_similarity, IndexConfig, MemoryIndex};
use uuid::Uuid;

fn rand_embedding(dim: usize) -> Vec<f32> {
    (0..dim).map(|i| (i as f32 * 0.1).sin()).collect()
}

fn similar_embedding(base: &[f32], noise: f32) -> Vec<f32> {
    base.iter().map(|v| v + noise).collect()
}

// ── Basic operations ─────────────────────────────────────────────────────

#[test]
fn test_new_is_empty() {
    let idx = MemoryIndex::new();
    assert!(idx.is_empty());
    assert_eq!(idx.len(), 0);
}

#[test]
fn test_with_config() {
    let config = IndexConfig {
        min_size: 500,
        pending_ratio: 0.1,
        removal_ratio: 0.05,
        ef_construction: 40,
    };
    let idx = MemoryIndex::with_config(config);
    assert!(idx.is_empty());
}

#[test]
fn test_insert_and_len() {
    let idx = MemoryIndex::new();
    idx.insert(Uuid::new_v4(), rand_embedding(128));
    assert_eq!(idx.len(), 1);
    assert!(!idx.is_empty());

    idx.insert(Uuid::new_v4(), rand_embedding(128));
    assert_eq!(idx.len(), 2);
}

#[test]
fn test_remove() {
    let idx = MemoryIndex::new();
    let id = Uuid::new_v4();
    idx.insert(id, rand_embedding(128));
    assert_eq!(idx.len(), 1);

    idx.remove(&id);
    assert_eq!(idx.len(), 0);
    assert!(idx.is_empty());
}

#[test]
fn test_remove_nonexistent() {
    let idx = MemoryIndex::new();
    idx.insert(Uuid::new_v4(), rand_embedding(128));
    idx.remove(&Uuid::new_v4()); // should not panic
    assert_eq!(idx.len(), 1);
}

// ── Search ───────────────────────────────────────────────────────────────

#[test]
fn test_search_basic() {
    let idx = MemoryIndex::new();
    let emb = rand_embedding(128);
    let id = Uuid::new_v4();
    idx.insert(id, emb.clone());
    // Insert a very different embedding
    idx.insert(Uuid::new_v4(), similar_embedding(&emb, 10.0));

    let results = idx.search(&emb, 5);
    assert!(!results.is_empty());
    // The exact match should be first (highest similarity)
    assert_eq!(results[0].0, id);
    assert!(results[0].1 > 0.99);
}

#[test]
fn test_search_respects_limit() {
    let idx = MemoryIndex::new();
    for _ in 0..20 {
        idx.insert(Uuid::new_v4(), rand_embedding(128));
    }

    let results = idx.search(&rand_embedding(128), 5);
    assert!(results.len() <= 5);
}

#[test]
fn test_search_empty_index() {
    let idx = MemoryIndex::new();
    let results = idx.search(&rand_embedding(128), 5);
    assert!(results.is_empty());
}

#[test]
fn test_search_orders_by_similarity() {
    let idx = MemoryIndex::new();
    let base = rand_embedding(128);

    let id_close = Uuid::new_v4();
    idx.insert(id_close, similar_embedding(&base, 0.01));

    let id_far = Uuid::new_v4();
    idx.insert(id_far, similar_embedding(&base, 5.0));

    let results = idx.search(&base, 10);
    assert!(results.len() >= 2);
    // Close should rank higher
    let pos_close = results.iter().position(|(id, _)| *id == id_close).unwrap();
    let pos_far = results.iter().position(|(id, _)| *id == id_far).unwrap();
    assert!(pos_close < pos_far);
}

// ── Build / Rebuild ──────────────────────────────────────────────────────

#[test]
fn test_build_index() {
    let idx = MemoryIndex::new();
    for _ in 0..10 {
        idx.insert(Uuid::new_v4(), rand_embedding(64));
    }
    // Build should not panic
    idx.build_index();
    assert_eq!(idx.len(), 10);

    // Search should still work after build
    let results = idx.search(&rand_embedding(64), 5);
    assert!(!results.is_empty());
}

#[test]
fn test_rebuild() {
    let idx = MemoryIndex::new();
    let mut entries = Vec::new();
    for _ in 0..5 {
        let id = Uuid::new_v4();
        let emb = rand_embedding(64);
        idx.insert(id, emb.clone());
        entries.push((id, emb));
    }
    idx.build_index();
    idx.rebuild(entries.into_iter());
    assert_eq!(idx.len(), 5);
}

// ── cosine_similarity ────────────────────────────────────────────────────

#[test]
fn test_cosine_similarity_identical() {
    let v = vec![1.0, 2.0, 3.0];
    let sim = cosine_similarity(&v, &v);
    assert!((sim - 1.0).abs() < 1e-5);
}

#[test]
fn test_cosine_similarity_orthogonal() {
    let a = vec![1.0, 0.0];
    let b = vec![0.0, 1.0];
    let sim = cosine_similarity(&a, &b);
    assert!(sim.abs() < 1e-5);
}

#[test]
fn test_cosine_similarity_opposite() {
    let a = vec![1.0, 0.0];
    let b = vec![-1.0, 0.0];
    let sim = cosine_similarity(&a, &b);
    assert!((sim - (-1.0)).abs() < 1e-5);
}

#[test]
fn test_cosine_similarity_different_magnitudes() {
    let a = vec![1.0, 0.0];
    let b = vec![100.0, 0.0];
    let sim = cosine_similarity(&a, &b);
    assert!((sim - 1.0).abs() < 1e-5);
}

#[test]
fn test_cosine_similarity_empty() {
    let sim = cosine_similarity(&[], &[]);
    assert!(sim.is_nan() || sim.abs() < 1e-5);
}

#[test]
fn test_cosine_similarity_mismatched_len() {
    // Should not panic, but result may be partial
    let a = vec![1.0, 2.0, 3.0];
    let b = vec![1.0, 2.0];
    let _ = cosine_similarity(&a, &b);
}
