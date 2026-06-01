//! Cortex WASM — in-memory memory engine for browser demos.
//! No SQLite, no filesystem — pure in-memory with the same API.

use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;
use wasm_bindgen::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Memory {
    id: String,
    text: String,
    tier: String,
    channel: String,
    created_at: String,
    salience: f32,
    tags: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Fact {
    subject: String,
    predicate: String,
    object: String,
    confidence: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Belief {
    key: String,
    probability: f32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SearchResult {
    id: String,
    text: String,
    score: f32,
    tier: String,
    channel: String,
    created_at: String,
}

/// In-memory Cortex engine for browser use.
#[wasm_bindgen]
pub struct CortexWasm {
    memories: Vec<Memory>,
    facts: Vec<Fact>,
    beliefs: HashMap<String, f32>,
}

#[wasm_bindgen]
impl CortexWasm {
    /// Create a new in-memory Cortex instance.
    #[wasm_bindgen(constructor)]
    pub fn new() -> Self {
        Self {
            memories: Vec::new(),
            facts: Vec::new(),
            beliefs: HashMap::new(),
        }
    }

    /// Ingest a memory. Returns the memory ID.
    pub fn ingest(&mut self, text: &str, channel: &str) -> String {
        let id = Uuid::new_v4().to_string();
        let now = chrono::Utc::now().to_rfc3339();

        // Auto-extract facts (simple pattern matching)
        self.extract_facts(text);

        let mem = Memory {
            id: id.clone(),
            text: text.to_string(),
            tier: "episodic".to_string(),
            channel: channel.to_string(),
            created_at: now,
            salience: 0.5,
            tags: Vec::new(),
        };
        self.memories.push(mem);
        id
    }

    /// Search memories by query. Returns JSON array of results.
    pub fn search(&self, query: &str, limit: usize) -> String {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<SearchResult> = self.memories.iter().enumerate().map(|(i, m)| {
            let text_lower = m.text.to_lowercase();
            let mut score: f32 = 0.0;

            // Word overlap scoring
            for word in &query_words {
                if text_lower.contains(word) {
                    score += 1.0 / query_words.len() as f32;
                }
            }

            // Exact substring bonus
            if text_lower.contains(&query_lower) {
                score += 0.5;
            }

            // Recency boost (newer = higher)
            let recency = i as f32 / self.memories.len().max(1) as f32;
            score += recency * 0.2;

            SearchResult {
                id: m.id.clone(),
                text: m.text.clone(),
                score,
                tier: m.tier.clone(),
                channel: m.channel.clone(),
                created_at: m.created_at.clone(),
            }
        }).collect();

        scored.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        scored.truncate(limit);

        // Filter out zero-score results
        scored.retain(|r| r.score > 0.0);

        serde_json::to_string(&scored).unwrap_or_else(|_| "[]".to_string())
    }

    /// Add a fact (subject-predicate-object).
    pub fn add_fact(&mut self, subject: &str, predicate: &str, object: &str, confidence: f32) {
        self.facts.push(Fact {
            subject: subject.to_string(),
            predicate: predicate.to_string(),
            object: object.to_string(),
            confidence,
        });
    }

    /// Query facts by entity. Returns JSON array.
    pub fn query_facts(&self, entity: &str) -> String {
        let entity_lower = entity.to_lowercase();
        let results: Vec<&Fact> = self.facts.iter().filter(|f| {
            f.subject.to_lowercase().contains(&entity_lower)
                || f.object.to_lowercase().contains(&entity_lower)
        }).collect();
        serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string())
    }

    /// Observe evidence for a belief. Returns new probability.
    pub fn observe_belief(&mut self, key: &str, supports: bool, strength: f32) -> f32 {
        let prior = *self.beliefs.get(key).unwrap_or(&0.5);
        let likelihood_h = if supports { 0.5 + strength * 0.5 } else { 0.5 - strength * 0.5 };
        let likelihood_not_h = 1.0 - likelihood_h;
        let numerator = likelihood_h * prior;
        let denominator = numerator + likelihood_not_h * (1.0 - prior);
        let posterior = if denominator > 0.0 {
            (numerator / denominator).clamp(0.001, 0.999)
        } else {
            prior
        };
        self.beliefs.insert(key.to_string(), posterior);
        posterior
    }

    /// Get all beliefs as JSON.
    pub fn get_beliefs(&self) -> String {
        let results: Vec<Belief> = self.beliefs.iter().map(|(k, v)| Belief {
            key: k.clone(),
            probability: *v,
        }).collect();
        serde_json::to_string(&results).unwrap_or_else(|_| "[]".to_string())
    }

    /// Get stats as JSON.
    pub fn stats(&self) -> String {
        serde_json::to_string(&serde_json::json!({
            "total": self.memories.len(),
            "episodic": self.memories.iter().filter(|m| m.tier == "episodic").count(),
            "semantic": self.facts.len(),
            "beliefs": self.beliefs.len(),
        })).unwrap_or_else(|_| "{}".to_string())
    }

    /// Get memory count.
    pub fn count(&self) -> usize {
        self.memories.len()
    }

    /// Fact extraction from text. Handles multi-clause ("I live in X and work at Y")
    /// without breaking values that contain "and" ("Research and Development").
    /// Recurses for 3+ clauses. Accepts "I" prefix in second clause.
    fn extract_facts(&mut self, text: &str) {
        self.extract_facts_inner(text, 0);
    }

    fn extract_facts_inner(&mut self, text: &str, depth: u8) {
        // Guard against unbounded recursion (crafted input with many " and work at " repetitions)
        if depth >= 10 {
            self.extract_single(text.trim());
            return;
        }

        let verb_prefixes = [
            "work at ", "work for ", "i work at ", "i work for ",
            "i'm a ", "i am a ", "i'm an ", "i am an ",
            "live in ", "i live in ", "i'm based in ", "i am based in ",
            "based in ",
        ];

        // Search for " and " case-insensitively by scanning the original text.
        // We avoid lowercasing the whole string and using its byte offsets, because
        // to_lowercase() can change byte lengths (e.g. Turkish İ → i̇).
        let bytes = text.as_bytes();
        let mut search_from = 0;
        while search_from + 5 <= bytes.len() {
            let rest = &text[search_from..];
            // Find next " and " (case-insensitive) directly in the ORIGINAL bytes.
            // " and " is pure ASCII, so an ASCII-case-insensitive byte-window match can
            // never start inside a multi-byte char — the offset is always a valid char
            // boundary. Going via to_lowercase() byte offsets is wrong: lowercasing can
            // change byte length (e.g. İ → i̇), shifting the split point and dropping a clause.
            let rest_bytes = rest.as_bytes();
            let needle = b" and ";
            let rel_pos = match (0..=rest_bytes.len() - needle.len())
                .find(|&i| rest_bytes[i..i + needle.len()].eq_ignore_ascii_case(needle))
            {
                Some(p) => p,
                None => break,
            };
            let pos = search_from + rel_pos;
            // Verify the next 5 bytes in the original are " and " (case-insensitive)
            if pos + 5 > text.len() || !text.is_char_boundary(pos) || !text.is_char_boundary(pos + 5) {
                break;
            }
            let after = text[pos + 5..].trim_start().to_lowercase();
            if verb_prefixes.iter().any(|p| after.starts_with(p)) {
                let first = text[..pos].trim();
                let second = text[pos + 5..].trim();
                self.extract_single(first);
                // Prepend "I " to bare verb clauses so extract_single can match them
                let second_lower = second.to_lowercase();
                let bare_verbs = ["work at ", "work for ", "live in "];
                let normalized = if bare_verbs.iter().any(|v| second_lower.starts_with(v)) {
                    format!("I {}", second)
                } else if second_lower.starts_with("based in ") {
                    format!("I'm {}", second)
                } else {
                    second.to_string()
                };
                self.extract_facts_inner(&normalized, depth + 1);
                return;
            }
            search_from = pos + 5;
        }

        self.extract_single(text.trim());
    }

    fn extract_single(&mut self, text: &str) {
        let lower = text.to_lowercase();

        // "X lives in Y" — require "I" prefix to avoid false positives ("Live in the moment")
        for pattern in &["i live in ", "i'm based in ", "i am based in "] {
            if let Some(rest) = lower.strip_prefix(pattern) {
                let obj = rest.split(&[',', '.', '!', '?'][..]).next().unwrap_or("").trim();
                if !obj.is_empty() {
                    self.add_fact("User", "lives_in", &capitalize(obj), 0.85);
                }
            }
        }

        // "X works at Y"
        for pattern in &["i work at ", "i work for ", "work at ", "work for "] {
            if let Some(rest) = lower.strip_prefix(pattern) {
                let obj = rest.split(&[',', '.', '!', '?'][..]).next().unwrap_or("").trim();
                if !obj.is_empty() {
                    self.add_fact("User", "works_at", &capitalize(obj), 0.85);
                }
            }
        }

        // "I'm a X" / "I am a X"
        for pattern in &["i'm a ", "i am a ", "i'm an ", "i am an "] {
            if let Some(rest) = lower.strip_prefix(pattern) {
                let obj = rest.split(&[',', '.', '!', '?'][..]).next().unwrap_or("").trim();
                if !obj.is_empty() {
                    self.add_fact("User", "is_a", &capitalize(obj), 0.80);
                }
            }
        }
    }
}

fn capitalize(s: &str) -> String {
    let s = s.trim();
    if s.is_empty() { return String::new(); }
    let mut chars = s.chars();
    chars.next().unwrap().to_uppercase().to_string() + chars.as_str()
}

#[cfg(test)]
mod tests {
    use super::*;

    // Regression: a code point whose lowercase form changes byte length before the
    // " and " separator (İ U+0130 → i̇, 2→3 bytes) must not shift the clause split.
    // The buggy version derived the split offset from text.to_lowercase() and reused
    // it on the original string, dropping the second clause ("work at Google").
    #[test]
    fn split_survives_unicode_titlecase_before_separator() {
        let mut c = CortexWasm::new();
        c.extract_facts_inner("İ live in Berlin and work at Google", 0);
        assert!(
            c.facts.iter().any(|f| f.predicate == "works_at" && f.object == "Google"),
            "second clause after a unicode-titlecase char should still extract; got {:?}",
            c.facts
        );
    }

    // Happy path stays intact: plain ASCII splits both clauses.
    #[test]
    fn split_plain_ascii_clauses() {
        let mut c = CortexWasm::new();
        c.extract_facts_inner("I live in Berlin and work at Google", 0);
        assert!(c.facts.iter().any(|f| f.predicate == "lives_in" && f.object == "Berlin"));
        assert!(c.facts.iter().any(|f| f.predicate == "works_at" && f.object == "Google"));
    }
}
