//! Benchmark: vector index operations.

use cortex_core::storage::memory_index::MemoryIndex;
use std::time::Instant;
use uuid::Uuid;

fn main() {
    let index = MemoryIndex::new();

    // Insert embeddings
    let n = 5000;
    let start = Instant::now();
    for i in 0..n {
        let id = Uuid::new_v4();
        let emb: Vec<f32> = (0..384).map(|j| ((i * 7 + j) as f32 * 0.01).sin()).collect();
        index.insert(id, emb);
    }
    let elapsed = start.elapsed();
    println!("insert:  {:>8.1}µs/op  ({} ops in {:?})", elapsed.as_micros() as f64 / n as f64, n, elapsed);

    // Search
    let query: Vec<f32> = (0..384).map(|j| (j as f32 * 0.01).cos()).collect();
    let n = 1000;
    let start = Instant::now();
    for _ in 0..n {
        index.search(&query, 10);
    }
    let elapsed = start.elapsed();
    println!("search:  {:>8.1}µs/op  ({} ops in {:?})", elapsed.as_micros() as f64 / n as f64, n, elapsed);

    println!("index size: {}", index.len());
}
