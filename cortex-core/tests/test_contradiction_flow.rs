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
fn different_people_employment_do_not_falsely_contradict() {
    let cortex = Cortex::in_memory().unwrap();
    cortex.ingest("Sarah works at Stripe", "test", None, None, None).unwrap();
    cortex.ingest("Bob works at Google", "test", None, None, None).unwrap();
    // Different subjects → no contradiction; each keeps their employer.
    assert!(employer(&cortex, "Sarah").iter().any(|o| o == "Stripe"));
    assert!(employer(&cortex, "Bob").iter().any(|o| o == "Google"));
}

#[test]
fn speculative_relationships_stay_out_of_injected_context() {
    // "X works with Y" is a single-sentence guess; on tech prose it's noise. It may be
    // stored/queryable, but must NOT be injected into the LLM's Known Facts context.
    let cortex = Cortex::in_memory().unwrap();
    cortex
        .ingest("React works with TypeScript", "test", None, None, None)
        .unwrap();
    let ctx = cortex.get_context(2000, None, None).unwrap();
    assert!(
        !ctx.contains("colleague_of"),
        "speculative relationship leaked into injected context:\n{ctx}"
    );
}

#[test]
fn tech_prose_does_not_create_false_facts() {
    // Regression guard for the cortex-user-skeptic HIGH finding: ordinary tech prose
    // must not pollute the fact store with entity-relation false positives.
    let cortex = Cortex::in_memory().unwrap();
    for prose in [
        "Python runs on Linux",
        "The Helios project runs on the Aurora database",
        "Docker is part of our stack",
    ] {
        cortex.ingest(prose, "test", None, None, None).unwrap();
    }
    for entity in ["Python", "Helios", "Aurora", "Docker"] {
        let facts = cortex.query_facts(entity).unwrap();
        let rel_facts: Vec<_> = facts
            .iter()
            .filter(|m| matches!(&m.content, MemContent::Fact { .. }))
            .collect();
        assert!(
            rel_facts.is_empty(),
            "{entity}: tech prose created false facts: {:?}",
            rel_facts.iter().map(|m| &m.content).collect::<Vec<_>>()
        );
    }
}
