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

        let mut scored: Vec<SearchResult> = self.memories.iter().map(|m| {
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
            let idx = self.memories.iter().position(|x| x.id == m.id).unwrap_or(0);
            let recency = idx as f32 / self.memories.len().max(1) as f32;
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
    fn extract_facts(&mut self, text: &str) {
        let lower = text.to_lowercase();

        // Known second-clause verb prefixes (after " and ")
        let verb_prefixes = ["work at ", "work for ", "i'm a ", "i am a ", "live in "];

        // Try splitting on " and " only if the second part starts with a known verb
        if let Some(pos) = lower.find(" and ") {
            let after = lower[pos + 5..].trim_start();
            let is_verb_clause = verb_prefixes.iter().any(|p| after.starts_with(p));
            if is_verb_clause {
                let first = &text[..pos];
                let second = text[pos + 5..].trim();
                self.extract_single(first);
                self.extract_single(second);
                return;
            }
        }

        // No split — extract from full text
        self.extract_single(text);
    }

    fn extract_single(&mut self, text: &str) {
        let lower = text.to_lowercase();

        // "X lives in Y"
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
                    self.add_fact("User", "is_a", obj, 0.80);
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
