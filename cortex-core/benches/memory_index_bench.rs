//! Benchmark for MemoryIndex search performance (brute-force vs HNSW)
use cortex_core::storage::memory_index::MemoryIndex;
use uuid::Uuid;
use std::time::Instant;

fn main() {
    let sizes = [100, 1_000, 10_000, 50_000];
    let dim = 384; // typical embedding dimension

    for &n in &sizes {
        let index = MemoryIndex::new();

        // Insert n random vectors
        let insert_start = Instant::now();
        for i in 0..n {
            let id = Uuid::new_v4();
            let embedding: Vec<f32> = (0..dim)
                .map(|j| (((i * 7 + j * 13 + 37) % 100) as f32) / 100.0)
                .collect();
            index.insert(id, embedding);
        }
        let insert_elapsed = insert_start.elapsed();

        // Force HNSW build (for large collections)
        let build_start = Instant::now();
        index.build_index();
        let build_elapsed = build_start.elapsed();

        // Search 100 times (HNSW already built)
        let query: Vec<f32> = (0..dim)
            .map(|i| (((i * 3 + 7) % 100) as f32) / 100.0)
            .collect();
        let iterations = 100;
        let search_start = Instant::now();
        for _ in 0..iterations {
            let _ = index.search(&query, 10);
        }
        let search_elapsed = search_start.elapsed();

        let avg_search_us = search_elapsed.as_micros() as f64 / iterations as f64;
        let mode = if n >= 1000 { "HNSW" } else { "brute" };
        println!(
            "n={:>6} | insert: {:>8.1}ms | build: {:>8.1}ms | search(top-10): {:>8.1}µs avg [{mode}]",
            n,
            insert_elapsed.as_secs_f64() * 1000.0,
            build_elapsed.as_secs_f64() * 1000.0,
            avg_search_us,
        );
    }
}
