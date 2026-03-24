//! Benchmark: core operations (ingest, search, context, beliefs).

use cortex_core::Cortex;
use std::time::Instant;

fn main() {
    let cortex = Cortex::in_memory().unwrap();

    // Seed data
    for i in 0..100 {
        cortex.ingest(&format!("Memory {} about various topics", i), "bench", None, None, None).unwrap();
    }

    // Ingest benchmark
    let n = 1000;
    let start = Instant::now();
    for i in 0..n {
        cortex.ingest(&format!("Bench ingest {}", i), "bench", None, None, None).unwrap();
    }
    let elapsed = start.elapsed();
    println!("ingest:  {:>8.1}µs/op  ({} ops in {:?})", elapsed.as_micros() as f64 / n as f64, n, elapsed);

    // Search benchmark
    let n = 500;
    let start = Instant::now();
    for _ in 0..n {
        cortex.retrieve("benchmark topic", 10, None, None, None).unwrap();
    }
    let elapsed = start.elapsed();
    println!("search:  {:>8.1}µs/op  ({} ops in {:?})", elapsed.as_micros() as f64 / n as f64, n, elapsed);

    // Context benchmark
    let n = 500;
    let start = Instant::now();
    for _ in 0..n {
        cortex.get_context(2000, None, None).unwrap();
    }
    let elapsed = start.elapsed();
    println!("context: {:>8.1}µs/op  ({} ops in {:?})", elapsed.as_micros() as f64 / n as f64, n, elapsed);

    // Belief benchmark
    let n = 5000;
    let start = Instant::now();
    for i in 0..n {
        cortex.observe_belief(&format!("belief_{}", i % 50), i % 2 == 0, 0.7).unwrap();
    }
    let elapsed = start.elapsed();
    println!("belief:  {:>8.1}µs/op  ({} ops in {:?})", elapsed.as_micros() as f64 / n as f64, n, elapsed);
}
