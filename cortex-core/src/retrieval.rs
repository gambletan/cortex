use chrono::{DateTime, Utc};
use uuid::Uuid;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Multi-signal retrieval query.
pub struct RetrievalQuery {
    pub text: String,
    pub embedding: Option<Vec<f32>>,
    pub channel: Option<String>,
    pub person_id: Option<Uuid>,
    pub namespace: Option<String>,
    pub time_context: DateTime<Utc>,
    pub limit: usize,
}

impl RetrievalQuery {
    pub fn new(text: impl Into<String>, limit: usize) -> Self {
        Self {
            text: text.into(),
            embedding: None,
            channel: None,
            person_id: None,
            namespace: None,
            time_context: Utc::now(),
            limit,
        }
    }

    pub fn with_embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    pub fn with_person(mut self, person_id: Uuid) -> Self {
        self.person_id = Some(person_id);
        self
    }

    pub fn with_namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = Some(namespace.into());
        self
    }
}

/// Retrieval result with score breakdown.
#[derive(Clone)]
pub struct RetrievalResult {
    pub memory: Arc<MemObject>,
    pub score: f32,
    pub score_breakdown: ScoreBreakdown,
}

/// Breakdown of how the final score was computed.
#[derive(Debug, Clone)]
pub struct ScoreBreakdown {
    pub similarity: f32,
    pub temporal: f32,
    pub salience: f32,
    pub social: f32,
    pub channel: f32,
    pub fts: f32,
}

/// Configurable weights for multi-signal retrieval.
#[derive(Debug, Clone)]
pub struct RetrievalWeights {
    pub similarity: f32,
    pub temporal: f32,
    pub salience: f32,
    pub social: f32,
    pub channel: f32,
    pub fts: f32,
}

impl Default for RetrievalWeights {
    fn default() -> Self {
        Self {
            similarity: 0.30,
            temporal: 0.18,
            salience: 0.22,
            social: 0.08,
            channel: 0.07,
            fts: 0.15,
        }
    }
}

/// Multi-signal retrieval engine.
pub struct RetrievalEngine<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
    weights: RetrievalWeights,
}

impl<'a> RetrievalEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self {
            storage,
            index,
            weights: RetrievalWeights::default(),
        }
    }

    pub fn with_weights(mut self, weights: RetrievalWeights) -> Self {
        self.weights = weights;
        self
    }

    /// Main retrieval: multi-signal ranked search with query expansion and multi-hop.
    pub fn retrieve(&self, query: &RetrievalQuery) -> Result<Vec<RetrievalResult>, CortexError> {
        // Detect if query has temporal intent (adjusts scoring strategy)
        let temporal_intent = detect_temporal_intent(&query.text);

        // Query expansion: find related entities from semantic store
        let expanded_entities = self.expand_query(&query.text)?;

        // Gather candidates using HashMap for O(1) dedup instead of O(N) linear scan
        let mut candidate_scores: HashMap<Uuid, f32> = HashMap::new();

        if let Some(ref embedding) = query.embedding {
            for (id, score) in self.index.search(embedding, query.limit * 5) {
                candidate_scores.insert(id, score);
            }
        }

        // If no embedding, fall back to recent memories
        if candidate_scores.is_empty() {
            let recent = self
                .storage
                .list_by_tier(MemoryTier::Episodic, query.limit * 3)?;
            let semantic = self
                .storage
                .list_by_tier(MemoryTier::Semantic, query.limit * 3)?;

            for mem in recent.iter().chain(semantic.iter()) {
                candidate_scores.entry(mem.id).or_insert(0.5);
            }
        }

        // Query expansion: pull in memories matching expanded entities
        if !expanded_entities.is_empty() {
            let expansion_ids = self.get_expansion_candidates(&expanded_entities, query.limit)?;
            for (id, score) in expansion_ids {
                candidate_scores.entry(id).or_insert(score);
            }
        }

        // For temporal queries, also pull in time-ordered candidates
        if temporal_intent != TemporalIntent::None {
            let time_ordered = self.get_temporal_candidates(&temporal_intent, query.limit * 3)?;
            for mem in time_ordered {
                candidate_scores.entry(mem.id).or_insert(0.3);
            }
        }

        // Multi-hop expansion: extract entities from top candidates and pull related facts
        let candidate_ids_vec: Vec<(Uuid, f32)> = candidate_scores.iter().map(|(&id, &s)| (id, s)).collect();
        let hop_ids = self.multi_hop_expand(&candidate_ids_vec, query.limit)?;
        for (id, score) in hop_ids {
            candidate_scores.entry(id).or_insert(score);
        }

        // FTS5 full-text search: gather additional candidates via BM25 keyword matching
        let fts_results = self.storage.fts_search(&query.text, query.limit * 3)?;
        let mut fts_scores: HashMap<Uuid, f32> = HashMap::new();
        for (id, score) in &fts_results {
            fts_scores.insert(*id, *score as f32);
            candidate_scores.entry(*id).or_insert(0.2);
        }

        // Batch fetch all candidates in a single query
        let all_ids: Vec<Uuid> = candidate_scores.keys().copied().collect();
        let memories = self.storage.get_memories_batch(&all_ids)?;
        let mem_map: HashMap<Uuid, Arc<MemObject>> = memories.into_iter().map(|m| (m.id, Arc::new(m))).collect();

        // Adapt weights for temporal queries
        let weights = match temporal_intent {
            TemporalIntent::None => self.weights.clone(),
            _ => RetrievalWeights {
                similarity: self.weights.similarity * 0.6,
                temporal: self.weights.temporal * 2.5,
                salience: self.weights.salience * 0.8,
                social: self.weights.social,
                channel: self.weights.channel,
                fts: self.weights.fts,
            },
        };

        // Adapt weights for query content type (applied multiplicatively)
        let query_type = detect_query_type(&query.text);
        let weights = match query_type {
            QueryType::PersonQuery => RetrievalWeights {
                similarity: weights.similarity,
                temporal: weights.temporal * 0.5,
                salience: weights.salience,
                social: weights.social * 3.0,
                channel: weights.channel,
                fts: weights.fts,
            },
            QueryType::FactQuery => RetrievalWeights {
                similarity: weights.similarity * 1.3,
                temporal: weights.temporal * 0.7,
                salience: weights.salience,
                social: weights.social,
                channel: weights.channel,
                fts: weights.fts * 1.2,
            },
            QueryType::PreferenceQuery => RetrievalWeights {
                similarity: weights.similarity,
                temporal: weights.temporal,
                salience: weights.salience * 1.5,
                social: weights.social * 0.5,
                channel: weights.channel,
                fts: weights.fts,
            },
            QueryType::General | QueryType::Temporal => weights,
        };

        // Score each candidate
        let mut results = Vec::new();
        for (&id, &sim_score) in &candidate_scores {
            if let Some(mem) = mem_map.get(&id) {
                // Namespace isolation: skip memories that don't match requested namespace
                if let Some(ref ns) = query.namespace {
                    if mem.namespace.as_deref() != Some(ns.as_str()) {
                        continue;
                    }
                }

                // Skip archived memories from retrieval results
                if mem.tier == MemoryTier::Archived {
                    continue;
                }

                let fts_score = fts_scores.get(&id).copied().unwrap_or(0.0);
                let breakdown = self.compute_scores(mem, sim_score, query, &temporal_intent, fts_score);
                let final_score = weights.similarity * breakdown.similarity
                    + weights.temporal * breakdown.temporal
                    + weights.salience * breakdown.salience
                    + weights.social * breakdown.social
                    + weights.channel * breakdown.channel
                    + weights.fts * breakdown.fts;

                results.push(RetrievalResult {
                    memory: Arc::clone(mem),
                    score: final_score,
                    score_breakdown: breakdown,
                });
            }
        }

        // Sort by score descending
        results.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal));
        results.truncate(query.limit);

        Ok(results)
    }

    /// Query expansion: extract entities from the query text and look up related
    /// entities from the semantic store using indexed queries per entity.
    fn expand_query(&self, query_text: &str) -> Result<HashSet<String>, CortexError> {
        let mut expanded = HashSet::new();
        let query_lower = query_text.to_lowercase();

        // Extract entity-like tokens from the query (capitalized words, Chinese names)
        let query_entities = extract_query_entities(query_text);
        if query_entities.is_empty() {
            return Ok(expanded);
        }

        // Use indexed fact queries per entity instead of loading all semantic memories
        for entity in &query_entities {
            let facts = self.storage.query_facts_by_entity(entity)?;
            for mem in &facts {
                if let MemContent::Fact { subject, predicate, object } = &mem.content {
                    let entity_lower = entity.to_lowercase();
                    if subject.to_lowercase().contains(&entity_lower) {
                        expanded.insert(object.clone());
                        expanded.insert(predicate.clone());
                    } else if object.to_lowercase().contains(&entity_lower) {
                        expanded.insert(subject.clone());
                        expanded.insert(predicate.clone());
                    }
                }
            }
        }

        // Check preferences matching query terms
        let prefs = self.storage.query_preferences_by_key(&query_lower)?;
        for mem in &prefs {
            if let MemContent::Preference { key, value, .. } = &mem.content {
                expanded.insert(key.clone());
                expanded.insert(value.clone());
            }
        }

        // Don't expand with common/generic terms
        expanded.retain(|e| e.len() > 1 && e != "User");

        Ok(expanded)
    }

    /// Pull in episodic memories that mention any of the expanded entities.
    fn get_expansion_candidates(
        &self,
        entities: &HashSet<String>,
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, CortexError> {
        let terms: Vec<String> = entities.iter().cloned().collect();
        let matches = self.storage.search_episodic_by_terms(&terms, limit)?;
        Ok(matches.into_iter().map(|(id, _text)| (id, 0.30_f32)).collect())
    }

    /// Multi-hop expansion: look at top initial candidates, extract entities
    /// (subjects/objects from facts), and pull in related semantic memories.
    fn multi_hop_expand(
        &self,
        candidates: &[(Uuid, f32)],
        limit: usize,
    ) -> Result<Vec<(Uuid, f32)>, CortexError> {
        // Only expand from top-5 candidates to keep it fast
        let top_n = candidates.len().min(5);
        let top_ids: Vec<Uuid> = candidates[..top_n].iter().map(|(id, _)| *id).collect();
        let top_mems = self.storage.get_memories_batch(&top_ids)?;

        // Extract entity names from facts in top candidates
        let mut entities: Vec<String> = Vec::new();
        for mem in &top_mems {
            match &mem.content {
                MemContent::Fact { subject, object, .. } => {
                    if subject != "User" && !entities.contains(subject) {
                        entities.push(subject.clone());
                    }
                    if !entities.contains(object) {
                        entities.push(object.clone());
                    }
                }
                MemContent::Relationship { person_a, person_b, .. } => {
                    // person_a/person_b are UUIDs, skip for now
                    let _ = (person_a, person_b);
                }
                _ => {}
            }
        }

        // Query semantic store for facts mentioning these entities (single batched query)
        let mut expanded: Vec<(Uuid, f32)> = Vec::new();
        let mut seen = HashSet::new();

        if !entities.is_empty() {
            let facts = self.storage.query_facts_by_entities(&entities)?;
            for mem in facts {
                if seen.insert(mem.id) {
                    expanded.push((mem.id, 0.25)); // lower score for hop-2 results
                }
                if expanded.len() >= limit {
                    break;
                }
            }
        }

        Ok(expanded)
    }

    /// Get time-ordered candidates for temporal queries.
    fn get_temporal_candidates(
        &self,
        intent: &TemporalIntent,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        match intent {
            TemporalIntent::Recent => {
                self.storage
                    .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit)
            }
            TemporalIntent::First | TemporalIntent::Earliest => {
                // Get all and reverse (oldest first)
                let mut mems = self
                    .storage
                    .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit)?;
                mems.reverse();
                Ok(mems)
            }
            TemporalIntent::Sequence => {
                // For sequence queries, get more candidates to capture the full timeline
                self.storage
                    .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, limit * 2)
            }
            TemporalIntent::None => Ok(Vec::new()),
        }
    }

    fn compute_scores(
        &self,
        mem: &MemObject,
        similarity: f32,
        query: &RetrievalQuery,
        temporal_intent: &TemporalIntent,
        fts: f32,
    ) -> ScoreBreakdown {
        // Temporal scoring — enhanced with intent awareness
        let hours_ago = (query.time_context - mem.temporal.ingestion_time)
            .num_hours()
            .max(0) as f32;

        let temporal = match temporal_intent {
            TemporalIntent::Recent => {
                // Strongly prefer recent memories
                (-hours_ago / 48.0).exp() // 2-day half-life
            }
            TemporalIntent::First | TemporalIntent::Earliest => {
                // Prefer older memories
                let max_hours = 24.0 * 365.0; // normalize to ~1 year
                (hours_ago / max_hours).min(1.0)
            }
            TemporalIntent::Sequence => {
                // Flat temporal score — don't bias by time, let similarity decide
                // But boost memories that have event_time set (they're more temporally grounded)
                if mem.temporal.event_time.is_some() {
                    0.7
                } else {
                    0.4
                }
            }
            TemporalIntent::None => {
                // Default: exponential decay with 1-week half-life
                (-hours_ago / 168.0).exp()
            }
        };

        // Salience: use effective score directly
        let salience = mem.salience.effective_score;

        // Social: boost if memory is about the queried person
        // Constant-time comparison: evaluate both person_a and person_b without short-circuit
        // to prevent timing attacks revealing which field matched
        let social = if let Some(person_id) = query.person_id {
            match &mem.source.identity_id {
                Some(id) if *id == person_id => 1.0,
                _ => match &mem.content {
                    MemContent::Relationship { person_a, person_b, .. } => {
                        let matches_a = *person_a == person_id;
                        let matches_b = *person_b == person_id;
                        if matches_a | matches_b { 1.0 } else { 0.0 }
                    }
                    _ => 0.0,
                },
            }
        } else {
            0.0
        };

        // Channel: boost if memory is from the same channel
        let channel = if let Some(ref ch) = query.channel {
            if mem.source.channel == *ch {
                1.0
            } else {
                0.0
            }
        } else {
            0.0
        };

        ScoreBreakdown {
            similarity,
            temporal,
            salience,
            social,
            channel,
            fts,
        }
    }
}

// ── Query type detection ─────────────────────────────────────────────────────

/// Detected query content type for adaptive weight adjustment.
#[derive(Debug, Clone, PartialEq)]
pub enum QueryType {
    General,
    PersonQuery,
    FactQuery,
    PreferenceQuery,
    Temporal, // handled by TemporalIntent; kept for classification completeness
}

/// Detect query content type from query text.
/// Supports English and Chinese query patterns.
pub fn detect_query_type(query: &str) -> QueryType {
    let lower = query.to_lowercase();

    // Check temporal first — if TemporalIntent is active, classify as Temporal
    if detect_temporal_intent(query) != TemporalIntent::None {
        return QueryType::Temporal;
    }

    // Preference patterns (check before fact patterns since "what do I like" contains "what")
    let pref_en = ["what do i like", "my preference", "do i prefer", "my favorite", "my favourite"];
    let pref_zh = ["我喜欢什么", "我的偏好", "我偏好", "我喜欢的"];
    for pat in &pref_en {
        if lower.contains(pat) {
            return QueryType::PreferenceQuery;
        }
    }
    for pat in &pref_zh {
        if query.contains(pat) {
            return QueryType::PreferenceQuery;
        }
    }

    // Person patterns
    let person_en = ["who is", "who was", "who are", "about him", "about her", "about them"];
    for pat in &person_en {
        if lower.contains(pat) {
            return QueryType::PersonQuery;
        }
    }
    // "who" at start of query
    if lower.starts_with("who ") {
        return QueryType::PersonQuery;
    }
    // "about [Name]" — look for "about" followed by a capitalized word
    if let Some(pos) = lower.find("about ") {
        let after = &query[pos + 6..];
        if after.chars().next().is_some_and(|c| c.is_uppercase()) {
            return QueryType::PersonQuery;
        }
    }
    // Chinese person patterns
    let person_zh = ["谁是", "谁"];
    for pat in &person_zh {
        if query.contains(pat) {
            return QueryType::PersonQuery;
        }
    }
    if let Some(pos) = query.find("关于") {
        let after = &query[pos + "关于".len()..];
        // Extract the word after "关于" up to a boundary, treat as person query only if 2-4 chars (name-like)
        // and does not contain common non-name words
        let name: String = after
            .chars()
            .take_while(|c| !c.is_whitespace() && !"的了吗呢吧？！，。".contains(*c))
            .collect();
        let char_count = name.chars().count();
        let non_name_words = ["这个", "那个", "什么", "哪个", "一些", "所有", "我们", "他们", "项目", "问题", "情况", "事情"];
        let is_non_name = non_name_words.iter().any(|w| name.contains(w));
        if (2..=4).contains(&char_count) && !is_non_name {
            return QueryType::PersonQuery;
        }
    }

    // Fact patterns
    let fact_en = ["what is", "what are", "where does", "where is", "what does"];
    let fact_zh = ["是什么", "哪里", "在哪", "做什么"];
    for pat in &fact_en {
        if lower.contains(pat) {
            return QueryType::FactQuery;
        }
    }
    for pat in &fact_zh {
        if query.contains(pat) {
            return QueryType::FactQuery;
        }
    }

    QueryType::General
}

// ── Temporal intent detection ────────────────────────────────────────────────

/// Detected temporal intent in a query.
#[derive(Debug, Clone, PartialEq)]
pub enum TemporalIntent {
    None,
    Recent,
    First,
    Earliest,
    Sequence,
}

/// Extract entity-like tokens from query text for query expansion.
/// Finds capitalized words (English names) and Chinese name-like sequences.
fn extract_query_entities(text: &str) -> Vec<String> {
    let mut entities = Vec::new();

    // English: collect sequences of capitalized words (proper nouns)
    let words: Vec<&str> = text.split_whitespace().collect();
    let mut i = 0;
    while i < words.len() {
        let word = words[i].trim_matches(|c: char| !c.is_alphanumeric());
        if !word.is_empty() && word.chars().next().is_some_and(|c| c.is_uppercase()) {
            // Start of a capitalized sequence
            let mut name_parts = vec![word.to_string()];
            let mut j = i + 1;
            while j < words.len() {
                let next = words[j].trim_matches(|c: char| !c.is_alphanumeric());
                if !next.is_empty() && next.chars().next().is_some_and(|c| c.is_uppercase()) {
                    name_parts.push(next.to_string());
                    j += 1;
                } else {
                    break;
                }
            }
            let name = name_parts.join(" ");
            // Skip common English words that happen to start sentences
            let skip = [
                "I", "The", "A", "An", "What", "Where", "When", "Who", "How",
                "Tell", "Do", "Does", "Is", "Are", "Was", "Were", "Can", "Could",
                "Will", "Would", "Should", "My", "Your", "His", "Her", "Their",
            ];
            if !skip.contains(&name.as_str()) {
                entities.push(name);
            }
            i = j;
        } else {
            i += 1;
        }
    }

    // Chinese: extract 2-4 character sequences that look like names
    // (Between known markers like 关于/about, 的/possessive)
    let zh_markers = ["关于", "about "];
    for marker in &zh_markers {
        if let Some(pos) = text.find(marker) {
            let after = &text[pos + marker.len()..];
            let name: String = after
                .chars()
                .take_while(|c| !c.is_whitespace() && !"的了吗呢吧？！，。".contains(*c))
                .collect();
            if name.chars().count() >= 2 && name.chars().count() <= 6 {
                entities.push(name);
            }
        }
    }

    // Also extract standalone query words that are long enough to be meaningful
    let lower = text.to_lowercase();
    for word in lower.split_whitespace() {
        let clean: String = word.chars().filter(|c| c.is_alphanumeric()).collect();
        if clean.len() >= 4
            && !["what", "where", "when", "about", "know", "tell", "does", "have"]
                .contains(&clean.as_str())
            && !entities.iter().any(|e| e.to_lowercase() == clean)
        {
            entities.push(clean);
        }
    }

    entities
}

/// Detect temporal intent from query text.
/// Supports English and Chinese temporal keywords.
fn detect_temporal_intent(query: &str) -> TemporalIntent {
    let lower = query.to_lowercase();

    // Recent intent
    let recent_en = [
        "last time", "recently", "latest", "most recent", "just now",
        "yesterday", "today", "this week", "last week", "this month",
        "last month", "the other day",
    ];
    let recent_zh = [
        "最近", "上次", "昨天", "今天", "刚才", "这周", "上周",
        "这个月", "上个月", "前几天", "最新",
    ];

    for pat in &recent_en {
        if lower.contains(pat) {
            return TemporalIntent::Recent;
        }
    }
    for pat in &recent_zh {
        if query.contains(pat) {
            return TemporalIntent::Recent;
        }
    }

    // First/earliest intent
    let first_en = [
        "first time", "the first", "originally", "initially", "at first",
        "when did", "earliest", "began", "started",
    ];
    let first_zh = [
        "第一次", "最初", "一开始", "起初", "最早", "什么时候开始",
    ];

    for pat in &first_en {
        if lower.contains(pat) {
            return TemporalIntent::First;
        }
    }
    for pat in &first_zh {
        if query.contains(pat) {
            return TemporalIntent::First;
        }
    }

    // Sequence intent (asking about order of events)
    let seq_en = [
        "before", "after", "then", "followed by", "in order",
        "timeline", "sequence", "chronolog", "when was", "what happened",
        "how many times", "how often",
    ];
    let seq_zh = [
        "之前", "之后", "然后", "顺序", "时间线", "多少次",
        "什么时候", "先后",
    ];

    for pat in &seq_en {
        if lower.contains(pat) {
            return TemporalIntent::Sequence;
        }
    }
    for pat in &seq_zh {
        if query.contains(pat) {
            return TemporalIntent::Sequence;
        }
    }

    TemporalIntent::None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_temporal_recent() {
        assert_eq!(
            detect_temporal_intent("What did I do last time?"),
            TemporalIntent::Recent
        );
        assert_eq!(
            detect_temporal_intent("最近聊了什么"),
            TemporalIntent::Recent
        );
    }

    #[test]
    fn test_temporal_first() {
        assert_eq!(
            detect_temporal_intent("When did I first mention this?"),
            TemporalIntent::First
        );
        assert_eq!(
            detect_temporal_intent("第一次讨论是什么时候"),
            TemporalIntent::First
        );
    }

    #[test]
    fn test_temporal_sequence() {
        assert_eq!(
            detect_temporal_intent("What happened after the meeting?"),
            TemporalIntent::Sequence
        );
        assert_eq!(
            detect_temporal_intent("会议之后发生了什么"),
            TemporalIntent::Sequence
        );
    }

    #[test]
    fn test_no_temporal() {
        assert_eq!(
            detect_temporal_intent("Tell me about Alice"),
            TemporalIntent::None
        );
    }

    // ── QueryType detection tests ──────────────────────────────────────────

    #[test]
    fn test_query_type_person_who() {
        assert_eq!(detect_query_type("Who is Alice?"), QueryType::PersonQuery);
        assert_eq!(detect_query_type("who was that person?"), QueryType::PersonQuery);
    }

    #[test]
    fn test_query_type_person_about_name() {
        assert_eq!(detect_query_type("Tell me about Alice"), QueryType::PersonQuery);
        assert_eq!(detect_query_type("What do you know about Bob Smith?"), QueryType::PersonQuery);
    }

    #[test]
    fn test_query_type_person_chinese() {
        assert_eq!(detect_query_type("谁是张三"), QueryType::PersonQuery);
        assert_eq!(detect_query_type("关于张三的信息"), QueryType::PersonQuery);
        // Non-name phrases after "关于" should NOT be PersonQuery
        assert_ne!(detect_query_type("关于这个项目的进展"), QueryType::PersonQuery);
        assert_ne!(detect_query_type("关于问题的解答"), QueryType::PersonQuery);
    }

    #[test]
    fn test_query_type_fact() {
        assert_eq!(detect_query_type("What is Rust?"), QueryType::FactQuery);
        assert_eq!(detect_query_type("Where does Alice work?"), QueryType::FactQuery);
        assert_eq!(detect_query_type("What does Bob do?"), QueryType::FactQuery);
    }

    #[test]
    fn test_query_type_fact_chinese() {
        assert_eq!(detect_query_type("Rust是什么"), QueryType::FactQuery);
        assert_eq!(detect_query_type("他在哪里工作"), QueryType::FactQuery);
    }

    #[test]
    fn test_query_type_preference() {
        assert_eq!(detect_query_type("What do I like to eat?"), QueryType::PreferenceQuery);
        assert_eq!(detect_query_type("What is my preference for editors?"), QueryType::PreferenceQuery);
    }

    #[test]
    fn test_query_type_preference_chinese() {
        assert_eq!(detect_query_type("我喜欢什么颜色"), QueryType::PreferenceQuery);
        assert_eq!(detect_query_type("我的偏好是什么"), QueryType::PreferenceQuery);
    }

    #[test]
    fn test_query_type_temporal_defers() {
        // Temporal queries should be classified as Temporal, not other types
        assert_eq!(detect_query_type("What did I do last time?"), QueryType::Temporal);
        assert_eq!(detect_query_type("最近聊了什么"), QueryType::Temporal);
    }

    #[test]
    fn test_query_type_general() {
        assert_eq!(detect_query_type("hello there"), QueryType::General);
        assert_eq!(detect_query_type("summarize everything"), QueryType::General);
    }

    #[test]
    fn test_extract_query_entities_english() {
        let entities = extract_query_entities("What do I know about Alice?");
        assert!(entities.iter().any(|e| e == "Alice"), "Should extract Alice: {:?}", entities);
    }

    #[test]
    fn test_extract_query_entities_full_name() {
        let entities = extract_query_entities("Tell me about Alice Smith");
        assert!(entities.iter().any(|e| e == "Alice Smith"), "Should extract full name: {:?}", entities);
    }

    #[test]
    fn test_extract_query_entities_chinese() {
        let entities = extract_query_entities("关于张三的信息");
        assert!(entities.iter().any(|e| e == "张三"), "Should extract Chinese name: {:?}", entities);
    }

    #[test]
    fn test_extract_query_entities_skip_common() {
        let entities = extract_query_entities("What is the weather?");
        // "What" should be skipped
        assert!(!entities.iter().any(|e| e == "What"), "Should skip common words: {:?}", entities);
    }
}
