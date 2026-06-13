//! Iteration 20: the documented "job change is auto-detected as a contradiction" flow
//! must actually work from natural-language ingest — not only when a caller hand-builds
//! facts with identical predicates. Regression guard for the cortex-user-skeptic finding
//! that contradiction detection was brittle exact-predicate matching that real phrasing
//! never triggered.

use cortex_core::types::MemContent;
use cortex_core::Cortex;

/// Helper: the object of the highest-confidence `works_at` fact for `subject`.
fn employer(cortex: &Cortex, subject: &str) -> Vec<String> {
    cortex
        .query_facts(subject)
        .unwrap()
        .into_iter()
        .filter_map(|m| match m.content {
            MemContent::Fact { ref predicate, ref object, .. } if predicate == "works_at" => Some(object.clone()),
            _ => None,
        })
        .collect()
}

#[test]
fn employment_is_extracted_from_natural_language() {
    let cortex = Cortex::in_memory().unwrap();
    cortex
        .ingest("Sarah works at Stripe", "test", None, None, None)
        .unwrap();
    let e = employer(&cortex, "Sarah");
    assert!(
        e.iter().any(|o| o == "Stripe"),
        "expected Sarah works_at Stripe, got {e:?}"
    );
}

#[test]
fn job_change_supersedes_old_employer() {
    let cortex = Cortex::in_memory().unwrap();
    // Day 1: Sarah at Stripe.
    cortex
        .ingest("Sarah works at Stripe", "test", None, None, None)
        .unwrap();
    // Later: Sarah moves employer (unambiguous phrasing).
    cortex
        .ingest("Sarah now works at Anthropic", "test", None, None, None)
        .unwrap();

    // Contract (matches the README's "old fact confidence decayed", not deleted):
    // the new employer is present, extracted exactly once, and the old employer is
    // SUPERSEDED — kept as history but decayed below the new fact so retrieval/context
    // surfaces the current employer.
    use cortex_core::types::MemContent;
    let sarah_facts: Vec<(String, f32)> = cortex
        .query_facts("Sarah")
        .unwrap()
        .into_iter()
        .filter_map(|m| match &m.content {
            MemContent::Fact { predicate, object, .. } if predicate == "works_at" => {
                Some((object.clone(), m.salience.effective_score))
            }
            _ => None,
        })
        .collect();

    let anthropic: Vec<f32> = sarah_facts.iter().filter(|(o, _)| o == "Anthropic").map(|(_, s)| *s).collect();
    let stripe: Vec<f32> = sarah_facts.iter().filter(|(o, _)| o == "Stripe").map(|(_, s)| *s).collect();

    assert_eq!(anthropic.len(), 1, "new employer should be extracted exactly once: {sarah_facts:?}");
    assert_eq!(stripe.len(), 1, "old employer kept as history (superseded, not deleted): {sarah_facts:?}");
    assert!(
        stripe[0] < anthropic[0],
        "superseded employer must be decayed below the current one: stripe={} anthropic={}",
        stripe[0], anthropic[0]
    );
}

#[test]
fn distinct_predicates_do_not_falsely_contradict() {
    let cortex = Cortex::in_memory().unwrap();
    // "works at" (employment) and "is located in" (location) are different predicates
    // about the same subject — not a contradiction.
    cortex
        .ingest("Helios runs on Aurora", "test", None, None, None)
        .unwrap();
    cortex
        .ingest("Helios is part of Northwind", "test", None, None, None)
        .unwrap();
    let facts = cortex.query_facts("Helios").unwrap();
    // Both relations coexist (runs_on + part_of); neither was wrongly superseded.
    let preds: Vec<String> = facts
        .into_iter()
        .filter_map(|m| match m.content {
            MemContent::Fact { ref predicate, .. } => Some(predicate.clone()),
            _ => None,
        })
        .collect();
    assert!(preds.iter().any(|p| p == "runs_on"), "runs_on lost: {preds:?}");
    assert!(preds.iter().any(|p| p == "part_of"), "part_of lost: {preds:?}");
}
