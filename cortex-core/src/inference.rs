//! Proactive inference — automatically extract structured knowledge from raw text.
//!
//! When text is ingested, the inference engine extracts:
//! - Facts (subject-predicate-object triples)
//! - Preferences (key-value pairs with confidence)
//! - Temporal hints (temporary vs permanent)
//!
//! This runs locally using pattern matching and heuristics — no LLM calls.
//! Designed to be fast enough to run on every ingest (<1ms).

use crate::types::*;

/// Extracted knowledge from a text input.
#[derive(Debug, Clone, Default)]
pub struct InferredKnowledge {
    pub facts: Vec<InferredFact>,
    pub preferences: Vec<InferredPreference>,
    pub temporal_hint: TemporalHint,
}

#[derive(Debug, Clone)]
pub struct InferredFact {
    pub subject: String,
    pub predicate: String,
    pub object: String,
    pub confidence: f32,
}

#[derive(Debug, Clone)]
pub struct InferredPreference {
    pub key: String,
    pub value: String,
    pub confidence: f32,
}

/// Hint about whether a memory is temporary or permanent.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum TemporalHint {
    /// No strong signal either way.
    Unknown,
    /// Likely temporary (e.g., "I'm working on X right now").
    Temporary,
    /// Likely permanent (e.g., "I live in Shanghai").
    Permanent,
}

impl Default for TemporalHint {
    fn default() -> Self {
        Self::Unknown
    }
}

/// Extract structured knowledge from raw text.
/// Designed to be fast (<1ms) — runs on every ingest.
pub fn extract(text: &str) -> InferredKnowledge {
    let mut knowledge = InferredKnowledge::default();
    let lower = text.to_lowercase();

    // Extract preferences
    extract_preferences(text, &lower, &mut knowledge);

    // Extract facts
    extract_facts(text, &lower, &mut knowledge);

    // Determine temporal hint
    knowledge.temporal_hint = classify_temporal(&lower);

    knowledge
}

// ── Preference extraction ────────────────────────────────────────────────────

/// Patterns: "I prefer X", "I like X", "I use X", "my favorite X is Y"
fn extract_preferences(text: &str, lower: &str, knowledge: &mut InferredKnowledge) {
    // "I prefer X over Y" / "I prefer X"
    if let Some(rest) = strip_prefix_any(lower, &["i prefer ", "i always use ", "i usually use "]) {
        let rest_orig = &text[text.len() - rest.len()..]; // preserve original case
        if let Some((a, _b)) = rest.split_once(" over ") {
            knowledge.preferences.push(InferredPreference {
                key: "preference".into(),
                value: clean_value(a),
                confidence: 0.85,
            });
        } else {
            let value = clean_value(first_clause(rest_orig));
            if !value.is_empty() && value.len() < 100 {
                knowledge.preferences.push(InferredPreference {
                    key: "preference".into(),
                    value,
                    confidence: 0.80,
                });
            }
        }
    }

    // "I like X" / "I love X"
    if let Some(rest) = strip_prefix_any(lower, &["i like ", "i love ", "i enjoy "]) {
        let value = clean_value(first_clause(rest));
        if !value.is_empty() && value.len() < 100 {
            knowledge.preferences.push(InferredPreference {
                key: "likes".into(),
                value,
                confidence: 0.75,
            });
        }
    }

    // "my favorite X is Y"
    if let Some(rest) = strip_prefix_any(lower, &["my favorite ", "my preferred "]) {
        if let Some((key_part, value_part)) = rest.split_once(" is ") {
            let key = clean_value(key_part);
            let value = clean_value(first_clause(value_part));
            if !key.is_empty() && !value.is_empty() {
                knowledge.preferences.push(InferredPreference {
                    key,
                    value,
                    confidence: 0.90,
                });
            }
        }
    }

    // Tech stack patterns: "I use X for Y"
    if let Some(rest) = strip_prefix_any(lower, &["i use "]) {
        if let Some((tool, purpose)) = rest.split_once(" for ") {
            let tool = clean_value(tool);
            let purpose = clean_value(first_clause(purpose));
            if !tool.is_empty() && tool.len() < 50 {
                knowledge.preferences.push(InferredPreference {
                    key: format!("tool_for_{}", purpose),
                    value: tool,
                    confidence: 0.80,
                });
            }
        }
    }

    // Language patterns
    for (pattern, lang_key) in &[
        ("i speak ", "language"),
        ("i code in ", "programming_language"),
        ("i program in ", "programming_language"),
        ("i write ", "programming_language"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let value = clean_value(first_clause(rest));
            if !value.is_empty() && value.len() < 50 {
                knowledge.preferences.push(InferredPreference {
                    key: lang_key.to_string(),
                    value,
                    confidence: 0.85,
                });
            }
        }
    }
}

// ── Fact extraction ──────────────────────────────────────────────────────────

/// Patterns: "I live in X", "I work at X", "I am a X", "my name is X"
fn extract_facts(text: &str, lower: &str, knowledge: &mut InferredKnowledge) {
    // Location: "I live in X", "I'm based in X", "I'm from X"
    for (pattern, predicate) in &[
        ("i live in ", "lives_in"),
        ("i'm based in ", "based_in"),
        ("i am based in ", "based_in"),
        ("i'm from ", "from"),
        ("i am from ", "from"),
        ("i moved to ", "lives_in"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let object = clean_value(first_clause(rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: predicate.to_string(),
                    object: capitalize_first(&object),
                    confidence: 0.85,
                });
            }
        }
    }

    // Work: "I work at X", "I work for X", "I'm a X at Y"
    for (pattern, predicate) in &[
        ("i work at ", "works_at"),
        ("i work for ", "works_at"),
        ("i work on ", "works_on"),
    ] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let object = clean_value(first_clause(rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: predicate.to_string(),
                    object: capitalize_first(&object),
                    confidence: 0.85,
                });
            }
        }
    }

    // Identity: "I am a X", "I'm a X"
    for pattern in &["i am a ", "i'm a ", "i am an ", "i'm an "] {
        if let Some(rest) = strip_prefix_any(lower, &[pattern]) {
            let object = clean_value(first_clause(rest));
            if !object.is_empty() && object.len() < 80 {
                knowledge.facts.push(InferredFact {
                    subject: "User".into(),
                    predicate: "is_a".into(),
                    object,
                    confidence: 0.80,
                });
            }
        }
    }

    // Name: "my name is X", "I'm X" (only if short, likely a name)
    if let Some(rest) = strip_prefix_any(lower, &["my name is ", "call me "]) {
        let name = clean_value(first_clause(rest));
        if !name.is_empty() && name.len() < 40 && name.split_whitespace().count() <= 3 {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "name".into(),
                object: capitalize_first(&name),
                confidence: 0.90,
            });
        }
    }

    // Age/experience: "I have X years of experience"
    if let Some(rest) = strip_prefix_any(lower, &["i have "]) {
        if rest.contains("years of experience") || rest.contains("years experience") {
            let years_part = first_clause(rest);
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "experience".into(),
                object: clean_value(years_part),
                confidence: 0.80,
            });
        }
    }

    // Timezone: "my timezone is X", "I'm in X timezone"
    if let Some(rest) = strip_prefix_any(lower, &["my timezone is ", "my time zone is "]) {
        let tz = clean_value(first_clause(rest));
        if !tz.is_empty() {
            knowledge.facts.push(InferredFact {
                subject: "User".into(),
                predicate: "timezone".into(),
                object: tz,
                confidence: 0.90,
            });
        }
    }
}

// ── Temporal classification ──────────────────────────────────────────────────

fn classify_temporal(lower: &str) -> TemporalHint {
    // Temporary signals
    let temporary_signals = [
        "right now", "currently", "at the moment", "today",
        "this week", "this morning", "this afternoon", "this evening",
        "working on", "debugging", "trying to", "about to",
        "just finished", "just started", "in the middle of",
        "temporarily", "for now",
    ];

    // Permanent signals
    let permanent_signals = [
        "always", "i am a", "i'm a", "i live", "i work at",
        "my name is", "i speak", "i prefer", "my favorite",
        "i was born", "i have been", "for years", "since",
        "i never", "i hate", "i love",
    ];

    let temp_score: f32 = temporary_signals.iter()
        .filter(|s| lower.contains(**s))
        .count() as f32;
    let perm_score: f32 = permanent_signals.iter()
        .filter(|s| lower.contains(**s))
        .count() as f32;

    if temp_score > perm_score && temp_score >= 1.0 {
        TemporalHint::Temporary
    } else if perm_score > temp_score && perm_score >= 1.0 {
        TemporalHint::Permanent
    } else {
        TemporalHint::Unknown
    }
}

// ── Helpers ──────────────────────────────────────────────────────────────────

/// Try multiple prefixes, return the rest after the first match.
fn strip_prefix_any<'a>(text: &'a str, prefixes: &[&str]) -> Option<&'a str> {
    for prefix in prefixes {
        if text.starts_with(prefix) {
            return Some(&text[prefix.len()..]);
        }
    }
    None
}

/// Get text up to the first sentence/clause boundary.
fn first_clause(text: &str) -> &str {
    // Check punctuation boundaries
    let punct_end = text.find(&['.', ',', '!', '?', '\n', ';'][..])
        .unwrap_or(text.len());
    // Also check " and " as a clause boundary
    let and_end = text.find(" and ")
        .unwrap_or(text.len());
    let end = punct_end.min(and_end);
    &text[..end]
}

/// Clean whitespace and trailing punctuation from a value.
fn clean_value(s: &str) -> String {
    s.trim()
        .trim_end_matches(&['.', ',', '!', '?', ';'][..])
        .trim()
        .to_string()
}

fn capitalize_first(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() {
        return String::new();
    }
    let mut chars = s.chars();
    let first = chars.next().unwrap().to_uppercase().to_string();
    first + chars.as_str()
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_preference_basic() {
        let k = extract("I prefer Rust over Python");
        assert_eq!(k.preferences.len(), 1);
        assert_eq!(k.preferences[0].value, "rust");
        assert!(k.preferences[0].confidence > 0.8);
    }

    #[test]
    fn test_extract_favorite() {
        let k = extract("My favorite editor is neovim");
        assert_eq!(k.preferences.len(), 1);
        assert_eq!(k.preferences[0].key, "editor");
        assert_eq!(k.preferences[0].value, "neovim");
    }

    #[test]
    fn test_extract_fact_location() {
        let k = extract("I live in Shanghai");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "lives_in");
        assert_eq!(k.facts[0].object, "Shanghai");
    }

    #[test]
    fn test_extract_fact_work() {
        let k = extract("I work at Google");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "works_at");
        assert_eq!(k.facts[0].object, "Google");
    }

    #[test]
    fn test_extract_fact_identity() {
        let k = extract("I'm a software engineer");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "is_a");
        assert_eq!(k.facts[0].object, "software engineer");
    }

    #[test]
    fn test_extract_name() {
        let k = extract("My name is Alvin");
        assert_eq!(k.facts.len(), 1);
        assert_eq!(k.facts[0].predicate, "name");
        assert_eq!(k.facts[0].object, "Alvin");
    }

    #[test]
    fn test_temporal_temporary() {
        let k = extract("I'm currently working on a Rust project");
        assert_eq!(k.temporal_hint, TemporalHint::Temporary);
    }

    #[test]
    fn test_temporal_permanent() {
        let k = extract("I live in Shanghai and I'm a developer");
        assert_eq!(k.temporal_hint, TemporalHint::Permanent);
    }

    #[test]
    fn test_no_extraction_from_noise() {
        let k = extract("The weather is nice today");
        assert!(k.facts.is_empty());
        assert!(k.preferences.is_empty());
    }

    #[test]
    fn test_programming_language() {
        let k = extract("I code in Rust and Python");
        assert!(!k.preferences.is_empty());
        assert_eq!(k.preferences[0].key, "programming_language");
    }

    #[test]
    fn test_use_tool_for() {
        let k = extract("I use neovim for editing");
        assert!(!k.preferences.is_empty());
        assert!(k.preferences[0].key.contains("tool_for"));
    }
}
