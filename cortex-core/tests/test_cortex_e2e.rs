//! End-to-end tests for the Cortex top-level API.

use cortex_core::Cortex;

#[test]
fn test_ingest_and_retrieve() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .ingest("I live in Shanghai", "telegram", None, None, None)
        .unwrap();
    cortex
        .ingest("I prefer dark mode", "telegram", None, None, None)
        .unwrap();

    let results = cortex.retrieve("where do I live", 5, None, None, None).unwrap();
    assert!(!results.is_empty());
}

#[test]
fn test_ingest_auto_extracts_facts() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .ingest("I work at Google", "cli", None, None, None)
        .unwrap();

    // The inference engine should have auto-extracted a fact
    let storage = cortex.storage();
    let semantic = storage
        .list_by_tier(cortex_core::types::MemoryTier::Semantic, 100)
        .unwrap();

    // Should have at least one semantic fact about "Google"
    let has_google_fact = semantic.iter().any(|m| match &m.content {
        cortex_core::types::MemContent::Fact { object, .. } => {
            object.to_lowercase().contains("google")
        }
        _ => false,
    });
    assert!(has_google_fact, "Should auto-extract work fact");
}

#[test]
fn test_ingest_auto_extracts_preferences() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .ingest("I prefer using Vim for editing", "cli", None, None, None)
        .unwrap();

    let storage = cortex.storage();
    let semantic = storage
        .list_by_tier(cortex_core::types::MemoryTier::Semantic, 100)
        .unwrap();

    let has_pref = semantic.iter().any(|m| matches!(&m.content, cortex_core::types::MemContent::Preference { .. }));
    assert!(has_pref, "Should auto-extract preference");
}

#[test]
fn test_ingest_auto_extracts_relationships() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .ingest("Alice works with Bob on the project", "cli", None, None, None)
        .unwrap();

    let storage = cortex.storage();
    let semantic = storage
        .list_by_tier(cortex_core::types::MemoryTier::Semantic, 100)
        .unwrap();

    let has_rel = semantic.iter().any(|m| match &m.content {
        cortex_core::types::MemContent::Fact { predicate, .. } => {
            predicate == "colleague_of"
        }
        _ => false,
    });
    assert!(has_rel, "Should auto-extract colleague relationship");
}

#[test]
fn test_ingest_chinese_inference() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .ingest("我住在上海", "telegram", None, None, None)
        .unwrap();

    let storage = cortex.storage();
    let semantic = storage
        .list_by_tier(cortex_core::types::MemoryTier::Semantic, 100)
        .unwrap();

    let has_shanghai = semantic.iter().any(|m| match &m.content {
        cortex_core::types::MemContent::Fact { object, .. } => {
            object.contains("上海")
        }
        _ => false,
    });
    assert!(has_shanghai, "Should auto-extract Chinese location fact");
}

#[test]
fn test_contradiction_detection() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .add_fact("User", "lives_in", "Shanghai", 0.9, "cli", None)
        .unwrap();

    let contradictions = cortex
        .check_contradictions("User", "lives_in", "Beijing")
        .unwrap();
    assert!(!contradictions.is_empty(), "Should detect contradiction");

    // Same fact should NOT be a contradiction
    let no_contradiction = cortex
        .check_contradictions("User", "lives_in", "Shanghai")
        .unwrap();
    assert!(no_contradiction.is_empty(), "Same value should not contradict");
}

#[test]
fn test_belief_lifecycle() {
    let cortex = Cortex::in_memory().unwrap();

    let b1 = cortex.observe_belief("likes_coffee", true, 0.8).unwrap();
    assert!(b1.probability > 0.5);

    let b2 = cortex.observe_belief("likes_coffee", true, 0.9).unwrap();
    assert!(b2.probability > b1.probability);

    // Contradicting evidence
    let b3 = cortex.observe_belief("likes_coffee", false, 0.7).unwrap();
    assert!(b3.probability < b2.probability);

    let beliefs = cortex.get_beliefs(0.5).unwrap();
    assert!(!beliefs.is_empty(), "Should have beliefs above threshold");
}

#[test]
fn test_person_resolution() {
    let cortex = Cortex::in_memory().unwrap();

    let p1 = cortex.add_person("Alice", "telegram", "alice_123").unwrap();
    let p2 = cortex.add_person("Alice", "telegram", "alice_123").unwrap();

    // Same identity should resolve to same person
    assert_eq!(p1.id, p2.id);
}

#[test]
fn test_consolidation() {
    let cortex = Cortex::in_memory().unwrap();

    for i in 0..5 {
        cortex
            .ingest(&format!("memory {}", i), "cli", None, None, None)
            .unwrap();
    }

    let report = cortex.run_consolidation().unwrap();
    assert!(report.episodes_scanned > 0);
}

#[test]
fn test_extract_relationships_api() {
    let cortex = Cortex::in_memory().unwrap();

    let rels = cortex.extract_relationships("Alice reports to Bob");
    assert!(!rels.is_empty());
    assert_eq!(rels[0].relation, "reports_to");

    let zh_rels = cortex.extract_relationships("Alice和Bob是同事");
    assert!(!zh_rels.is_empty());
    assert_eq!(zh_rels[0].relation, "colleague_of");
}

#[test]
fn test_infer_api() {
    let cortex = Cortex::in_memory().unwrap();

    let knowledge = cortex.infer("I live in Shanghai and I prefer using Rust");
    assert!(!knowledge.facts.is_empty());
    // Preference extraction depends on specific patterns like "I prefer using X"
    assert_eq!(
        knowledge.temporal_hint,
        cortex_core::inference::TemporalHint::Permanent
    );
}

#[test]
fn test_context_generation() {
    let cortex = Cortex::in_memory().unwrap();

    cortex
        .ingest("Meeting with Alice about Q3 roadmap", "slack", None, Some(0.8), None)
        .unwrap();
    cortex
        .add_preference("timezone", "Asia/Shanghai", 0.9)
        .unwrap();

    let ctx = cortex.get_context(2000, None, None).unwrap();
    assert!(!ctx.is_empty());
}

#[test]
fn test_temporal_durability_on_ingest() {
    let cortex = Cortex::in_memory().unwrap();

    // Permanent memory
    let mem = cortex
        .ingest("I live in Shanghai", "cli", None, None, None)
        .unwrap();
    assert_eq!(
        mem.temporal.durability,
        cortex_core::types::MemoryDurability::Permanent
    );

    // Temporary memory
    let mem = cortex
        .ingest("I'm currently debugging a test", "cli", None, None, None)
        .unwrap();
    assert_eq!(
        mem.temporal.durability,
        cortex_core::types::MemoryDurability::Temporary
    );
}
