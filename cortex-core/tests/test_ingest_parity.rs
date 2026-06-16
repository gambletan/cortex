//! Iteration 21: single and batch ingest share ONE lifecycle (`prepare_ingest` →
//! near-dedup → store → `finalize_ingest`). These guard the parity that the unification
//! establishes — `ingest_batch` previously skipped near-dedup, fact-contradiction
//! resolution, and relationship extraction, a silent divergence from single ingest.
//! Each test below is written to FAIL against the pre-unification `ingest_batch`.

use cortex_core::types::*;
use cortex_core::Cortex;

fn item(text: &str) -> BatchIngestItem {
    BatchIngestItem {
        text: text.into(),
        channel: "test".into(),
        privacy: None,
        user_id: None,
        salience_hint: None,
        embedding: None,
        namespace: None,
    }
}

/// (employer, effective_score) for every `works_at` fact about `subject`.
fn works_at(cortex: &Cortex, subject: &str) -> Vec<(String, f32)> {
    cortex
        .query_facts(subject)
        .unwrap()
        .into_iter()
        .filter_map(|m| match &m.content {
            MemContent::Fact { predicate, object, .. } if predicate == "works_at" => {
                Some((object.clone(), m.salience.effective_score))
            }
            _ => None,
        })
        .collect()
}

#[test]
fn batch_resolves_contradictions_in_input_order() {
    // Two contradicting employment facts about the same subject, in ONE batch.
    // Pre-unification: batch added both at full confidence (no supersede).
    let cortex = Cortex::in_memory().unwrap();
    cortex
        .ingest_batch(vec![
            item("Sarah works at Stripe"),
            item("Sarah now works at Anthropic"),
        ])
        .unwrap();

    let facts = works_at(&cortex, "Sarah");
    let anthropic: Vec<f32> = facts.iter().filter(|(o, _)| o == "Anthropic").map(|(_, s)| *s).collect();
    let stripe: Vec<f32> = facts.iter().filter(|(o, _)| o == "Stripe").map(|(_, s)| *s).collect();
    assert_eq!(anthropic.len(), 1, "new employer extracted exactly once: {facts:?}");
    assert_eq!(stripe.len(), 1, "old employer kept as history (superseded, not deleted): {facts:?}");
    assert!(
        stripe[0] < anthropic[0],
        "old employer must be decayed below the current one: {facts:?}"
    );
}

#[test]
fn batch_extracts_relationships_like_single() {
    // Pre-unification: ingest_batch skipped relationship extraction entirely.
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest_batch(vec![item("Alice works with Bob")]).unwrap();
    let alice = cortex.query_facts("Alice").unwrap();
    assert!(
        alice.iter().any(|m| matches!(
            &m.content,
            MemContent::Fact { predicate, .. } if predicate.contains("colleague")
        )),
        "batch should extract the Alice/Bob relationship: {:?}",
        alice.iter().map(|m| &m.content).collect::<Vec<_>>()
    );
}

#[test]
fn batch_near_dedup_against_existing_memory() {
    // Pre-unification: batch had no near-dedup, so this near-copy would be stored.
    let cortex = Cortex::in_memory().unwrap().with_dedup_config(DeduplicationConfig {
        exact_dedup: true,
        near_dedup: true,
        near_dedup_threshold: 0.95,
    });
    cortex
        .ingest("I prefer dark mode in my editor", "test", None, None, Some(vec![1.0, 0.0, 0.0]))
        .unwrap();
    let before = cortex.stats().unwrap().episodic;

    let mut near = item("dark mode is my editor preference");
    near.embedding = Some(vec![1.0, 0.0, 0.0]); // cosine 1.0 ≥ 0.95
    let report = cortex.ingest_batch(vec![near]).unwrap();

    assert_eq!(report.stored, 0, "near-dup of an existing memory must not be stored");
    assert_eq!(report.deduplicated, 1, "near-dup must be counted: {report:?}");
    assert_eq!(cortex.stats().unwrap().episodic, before, "episodic count unchanged");
}

#[test]
fn batch_near_dedup_within_same_batch_keeps_first() {
    // Pre-unification: two near-identical items in one batch were both stored.
    let cortex = Cortex::in_memory().unwrap().with_dedup_config(DeduplicationConfig {
        exact_dedup: true,
        near_dedup: true,
        near_dedup_threshold: 0.95,
    });
    let mut a = item("dark mode preference");
    a.embedding = Some(vec![1.0, 0.0, 0.0]);
    let mut b = item("I like dark mode");
    b.embedding = Some(vec![1.0, 0.0, 0.0]); // near-dup of `a`, later in the batch
    let mut c = item("I live in Shanghai");
    c.embedding = Some(vec![0.0, 1.0, 0.0]); // orthogonal → distinct

    let report = cortex.ingest_batch(vec![a, b, c]).unwrap();
    assert_eq!(report.stored, 2, "near-dup sibling dropped, distinct kept: {report:?}");
    assert_eq!(report.deduplicated, 1, "intra-batch near-dup counted: {report:?}");
    assert_eq!(cortex.stats().unwrap().episodic, 2);
}

#[test]
fn single_item_batch_matches_single_ingest_facts() {
    // Codex parity test #1: a 1-element batch yields the same extracted facts as single.
    let single = Cortex::in_memory().unwrap();
    single.ingest("Sarah works at Stripe", "test", None, None, None).unwrap();

    let batch = Cortex::in_memory().unwrap();
    batch.ingest_batch(vec![item("Sarah works at Stripe")]).unwrap();

    assert_eq!(
        works_at(&single, "Sarah"),
        works_at(&batch, "Sarah"),
        "a 1-item batch must extract the same facts as single ingest"
    );
}
