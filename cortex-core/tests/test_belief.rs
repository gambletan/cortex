use cortex_core::belief::{BeliefEngine, Evidence};
use cortex_core::storage::sqlite::SqliteStorage;

fn setup() -> SqliteStorage {
    SqliteStorage::open_in_memory().unwrap()
}

#[test]
fn test_observe_creates_belief() {
    let storage = setup();
    let engine = BeliefEngine::new(&storage);

    let belief = engine
        .observe("user.prefers_concise", Evidence::Supports(0.8))
        .unwrap();

    assert_eq!(belief.key, "user.prefers_concise");
    assert!(belief.probability > 0.5); // should be above prior after supporting evidence
    assert_eq!(belief.observations.len(), 1);
}

#[test]
fn test_supporting_evidence_increases_probability() {
    let storage = setup();
    let engine = BeliefEngine::new(&storage);

    let b1 = engine
        .observe("likes_rust", Evidence::Supports(0.7))
        .unwrap();
    let b2 = engine
        .observe("likes_rust", Evidence::Supports(0.7))
        .unwrap();

    assert!(b2.probability > b1.probability);
    assert_eq!(b2.observations.len(), 2);
}

#[test]
fn test_contradicting_evidence_decreases_probability() {
    let storage = setup();
    let engine = BeliefEngine::new(&storage);

    // First build up the belief
    engine
        .observe("likes_java", Evidence::Supports(0.8))
        .unwrap();
    let before = engine
        .observe("likes_java", Evidence::Supports(0.8))
        .unwrap();

    // Then contradict it
    let after = engine
        .observe("likes_java", Evidence::Contradicts(0.9))
        .unwrap();

    assert!(after.probability < before.probability);
}

#[test]
fn test_get_belief() {
    let storage = setup();
    let engine = BeliefEngine::new(&storage);

    assert!(engine.get_belief("nonexistent").unwrap().is_none());

    engine.observe("exists", Evidence::Supports(0.5)).unwrap();
    let belief = engine.get_belief("exists").unwrap();
    assert!(belief.is_some());
}

#[test]
fn test_get_confident_beliefs() {
    let storage = setup();
    let engine = BeliefEngine::new(&storage);

    // Create a confident belief
    for _ in 0..5 {
        engine
            .observe("very_sure", Evidence::Supports(0.9))
            .unwrap();
    }

    // Create an uncertain belief
    engine
        .observe("not_sure", Evidence::Supports(0.1))
        .unwrap();

    let confident = engine.get_confident_beliefs(0.8).unwrap();
    assert!(confident.iter().any(|b| b.key == "very_sure"));
}

#[test]
fn test_probability_bounds() {
    let storage = setup();
    let engine = BeliefEngine::new(&storage);

    // Even extreme evidence shouldn't push probability to exactly 0 or 1
    for _ in 0..20 {
        engine
            .observe("extreme", Evidence::Supports(1.0))
            .unwrap();
    }

    let belief = engine.get_belief("extreme").unwrap().unwrap();
    assert!(belief.probability < 1.0);
    assert!(belief.probability > 0.0);
}
