//! Conversation compression — summarize long episode sequences into compact memories.
//!
//! When episodic memory accumulates too many entries from the same channel/session,
//! the compression engine merges them into a single summary memory, preserving
//! key facts while reducing storage.

use chrono::{DateTime, Duration, Utc};
use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Result of a compression operation.
#[derive(Debug, Default)]
pub struct CompressionReport {
    pub sessions_compressed: usize,
    pub episodes_consumed: usize,
    pub summaries_created: usize,
}

/// External summarizer function type.
/// Takes a list of texts and the original count, returns a summary string.
/// Use this to plug in an LLM-based summarizer instead of extractive summarization.
pub type SummarizerFn = Box<dyn Fn(&[String], usize) -> Result<String, CortexError> + Send + Sync>;

/// Compression engine — reduces episodic bloat while preserving knowledge.
pub struct CompressionEngine<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
    /// Optional external summarizer (e.g., LLM-powered).
    /// When set, replaces the built-in extractive summarizer.
    summarizer: Option<&'a SummarizerFn>,
    /// Maximum character length for extractive summaries.
    summary_max_chars: usize,
}

impl<'a> CompressionEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self { storage, index, summarizer: None, summary_max_chars: 500 }
    }

    /// Use an external summarizer (e.g., LLM-powered) for compression.
    pub fn with_summarizer(mut self, summarizer: &'a SummarizerFn) -> Self {
        self.summarizer = Some(summarizer);
        self
    }

    /// Set the maximum character length for extractive summaries.
    pub fn with_summary_max_chars(mut self, n: usize) -> Self {
        self.summary_max_chars = n;
        self
    }

    /// Find conversation sessions that are candidates for compression.
    /// A session is a sequence of episodic memories from the same channel
    /// within a time window, with at least `min_messages` entries.
    pub fn find_compressible_sessions(
        &self,
        min_messages: usize,
        max_age: Duration,
    ) -> Result<Vec<ConversationSession>, CortexError> {
        let cutoff = Utc::now() - max_age;

        // Group by (channel, date) to find sessions — paginated loading.
        // Track min/max timestamps inline to avoid re-iteration.
        struct SessionAccum {
            memories: Vec<MemObject>,
            min_time: DateTime<Utc>,
            max_time: DateTime<Utc>,
        }
        let mut sessions: std::collections::HashMap<String, SessionAccum> =
            std::collections::HashMap::new();
        let page_size = 1000;
        let mut offset = 0;

        loop {
            let episodes = self.storage.list_by_tier_paged(MemoryTier::Episodic, offset, page_size)?;
            if episodes.is_empty() {
                break;
            }
            let page_len = episodes.len();

            for mem in episodes {
                if mem.temporal.ingestion_time > cutoff {
                    continue; // too recent, don't compress yet
                }
                let date = mem.temporal.ingestion_time.format("%Y-%m-%d").to_string();
                let key = format!("{}|{}", mem.source.channel, date);
                let t = mem.temporal.ingestion_time;
                let accum = sessions.entry(key).or_insert_with(|| SessionAccum {
                    memories: Vec::new(),
                    min_time: t,
                    max_time: t,
                });
                if t < accum.min_time { accum.min_time = t; }
                if t > accum.max_time { accum.max_time = t; }
                accum.memories.push(mem);
            }

            if page_len < page_size {
                break;
            }
            offset += page_size;
        }

        let mut result = Vec::new();
        for (key, accum) in sessions {
            if accum.memories.len() < min_messages {
                continue;
            }
            let channel = key.split('|').next().unwrap_or("").to_string();

            result.push(ConversationSession {
                channel,
                start: accum.min_time,
                end: accum.max_time,
                memories: accum.memories,
            });
        }

        // Sort by start time (oldest first)
        result.sort_by_key(|s| s.start);
        Ok(result)
    }

    /// Compress a session into a summary memory.
    /// Extracts key content from all messages, creates a compressed summary,
    /// and replaces the originals.
    pub fn compress_session(
        &self,
        session: &ConversationSession,
    ) -> Result<MemObject, CortexError> {
        if session.memories.is_empty() {
            return Err(CortexError::InvalidInput("Empty session".into()));
        }

        // Build summary text from all messages
        let mut texts = Vec::new();
        let mut max_salience: f32 = 0.0;
        let mut identity_id = None;
        let mut tags: Vec<String> = Vec::new();

        for mem in &session.memories {
            let text = match &mem.content {
                MemContent::Text(t) => t.clone(),
                MemContent::Fact { subject, predicate, object } => {
                    format!("{} {} {}", subject, predicate, object)
                }
                MemContent::Preference { key, value, .. } => format!("{}: {}", key, value),
                _ => continue,
            };
            texts.push(text);

            if mem.salience.effective_score > max_salience {
                max_salience = mem.salience.effective_score;
            }
            if identity_id.is_none() {
                identity_id = mem.source.identity_id;
            }
            for tag in &mem.tags {
                if !tags.contains(tag) {
                    tags.push(tag.clone());
                }
            }
        }

        // Create compressed summary (use external summarizer if available)
        let summary = if let Some(summarizer) = self.summarizer {
            (summarizer)(&texts, session.memories.len())?
        } else {
            self.build_summary(&texts, session.memories.len())
        };

        let mut source = MemSource::new(&session.channel);
        source.identity_id = identity_id;

        let summary_mem = MemObjectBuilder::new(
            MemoryTier::Episodic,
            MemContent::Text(summary),
            source,
        )
        .salience(Salience::new(max_salience.max(0.3)))
        .tags(tags)
        .event_time(session.start)
        .durability(MemoryDurability::Normal)
        .meta(
            "compressed_from".to_string(),
            serde_json::json!(session.memories.len()),
        )
        .meta(
            "session_start".to_string(),
            serde_json::json!(session.start.to_rfc3339()),
        )
        .meta(
            "session_end".to_string(),
            serde_json::json!(session.end.to_rfc3339()),
        )
        .build();

        // Store summary
        self.storage.store_memory(&summary_mem)?;

        // Delete originals in a single transaction
        let ids: Vec<uuid::Uuid> = session.memories.iter().map(|m| m.id).collect();
        self.storage.delete_memories_batch(&ids)?;
        for id in &ids {
            self.index.remove(id);
        }

        Ok(summary_mem)
    }

    /// Run compression on all eligible sessions.
    pub fn run_compression(
        &self,
        min_messages: usize,
        max_age: Duration,
    ) -> Result<CompressionReport, CortexError> {
        let sessions = self.find_compressible_sessions(min_messages, max_age)?;
        let mut report = CompressionReport::default();

        for session in &sessions {
            let episode_count = session.memories.len();
            match self.compress_session(session) {
                Ok(_) => {
                    report.sessions_compressed += 1;
                    report.episodes_consumed += episode_count;
                    report.summaries_created += 1;
                }
                Err(e) => {
                    tracing::warn!(
                        channel = %session.channel,
                        error = %e,
                        "Failed to compress session"
                    );
                }
            }
        }

        Ok(report)
    }

    /// Build a compressed summary from a list of texts.
    /// Uses extractive summarization: keep the most informative sentences,
    /// deduplicate, categorize by content type, and concatenate.
    fn build_summary(&self, texts: &[String], original_count: usize) -> String {
        // Deduplicate exact and near-exact matches
        let mut unique_texts: Vec<&String> = Vec::new();
        for text in texts {
            let normalized = text.trim().to_lowercase();
            if normalized.is_empty() {
                continue;
            }
            let is_dup = unique_texts.iter().any(|existing| {
                let existing_norm = existing.trim().to_lowercase();
                existing_norm == normalized
                    || (existing_norm.len() > 10
                        && normalized.len() > 10
                        && (existing_norm.contains(&normalized) || normalized.contains(&existing_norm)))
            });
            if !is_dup {
                unique_texts.push(text);
            }
        }

        // Categorize texts into groups: facts, preferences, general
        let mut groups: std::collections::HashMap<ContentGroup, Vec<&String>> =
            std::collections::HashMap::new();
        for t in &unique_texts {
            groups.entry(classify_content(t)).or_default().push(t);
        }

        // Build document frequency map for TF-IDF-like scoring.
        // Document frequency = number of texts a word appears in.
        let num_docs = unique_texts.len().max(1) as f32;
        let mut doc_freq: std::collections::HashMap<String, usize> =
            std::collections::HashMap::new();
        for t in &unique_texts {
            let words_in_doc: std::collections::HashSet<String> = t
                .split_whitespace()
                .map(|w| w.to_lowercase())
                .collect();
            for w in words_in_doc {
                *doc_freq.entry(w).or_insert(0) += 1;
            }
        }

        // Score each text using TF-IDF-like metric:
        //   score = sum of (tf * idf) for each unique word
        //   tf = count of word in this text / total words in text
        //   idf = ln(num_docs / doc_freq(word))
        let mut scored: Vec<(&String, f32)> = unique_texts
            .iter()
            .map(|t| {
                let word_list: Vec<String> = t
                    .split_whitespace()
                    .map(|w| w.to_lowercase())
                    .collect();
                let total_words = word_list.len().max(1) as f32;

                // Term frequency counts
                let mut tf_counts: std::collections::HashMap<&str, usize> =
                    std::collections::HashMap::new();
                for w in &word_list {
                    *tf_counts.entry(w.as_str()).or_insert(0) += 1;
                }

                let score: f32 = tf_counts
                    .iter()
                    .map(|(word, &count)| {
                        let tf = count as f32 / total_words;
                        let df = doc_freq
                            .get(*word)
                            .copied()
                            .unwrap_or(1) as f32;
                        let idf = (num_docs / df).ln().max(0.0);
                        tf * idf
                    })
                    .sum();

                (*t, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Build index-based scored list for stable identity tracking.
        let scored_indices: Vec<(usize, f32)> = scored
            .iter()
            .enumerate()
            .map(|(i, (_, s))| (i, *s))
            .collect();

        // Classify each scored entry by content group.
        let entry_groups: Vec<ContentGroup> = scored.iter().map(|(t, _)| classify_content(t)).collect();

        // Ensure at least one entry from each non-empty content group is represented.
        let mut selected: Vec<&str> = Vec::new();
        let mut total_len: usize = 0;
        let mut selected_indices: std::collections::HashSet<usize> =
            std::collections::HashSet::new();
        let max_chars = self.summary_max_chars;

        // First pass: pick the highest-scored entry from each group.
        for group in &[ContentGroup::Fact, ContentGroup::Preference, ContentGroup::General] {
            let best = scored_indices
                .iter()
                .filter(|(i, _)| entry_groups[*i] == *group)
                .max_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));

            if let Some(&(idx, _)) = best {
                let text = scored[idx].0;
                if total_len + text.len() <= max_chars || selected.is_empty() {
                    selected.push(text.as_str());
                    total_len += text.len();
                    selected_indices.insert(idx);
                }
            }
        }

        // Second pass: fill remaining budget from the global ranked list.
        for (idx, _) in &scored_indices {
            if total_len >= max_chars {
                break;
            }
            if selected_indices.contains(idx) {
                continue;
            }
            let text = scored[*idx].0;
            if total_len + text.len() > max_chars && !selected.is_empty() {
                break;
            }
            selected.push(text.as_str());
            total_len += text.len();
            selected_indices.insert(*idx);
        }

        let header = format!("[Compressed: {} messages]", original_count);
        if selected.is_empty() {
            header
        } else {
            format!("{} {}", header, selected.join(" | "))
        }
    }
}

/// Content group for categorization during compression.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum ContentGroup {
    Fact,
    Preference,
    General,
}

/// Classify text into a content group for compression summarization.
fn classify_content(text: &str) -> ContentGroup {
    let trimmed = text.trim();
    // Facts: contain "=" or structured "subject predicate object" patterns
    if trimmed.contains('=') {
        return ContentGroup::Fact;
    }
    // Preferences: "key: value" or "key:value" format (single colon, not URLs)
    if let Some(colon_pos) = trimmed.find(':') {
        let before = &trimmed[..colon_pos];
        // Exclude URLs (http:, https:, ftp:) and timestamps
        if !before.ends_with("http")
            && !before.ends_with("https")
            && !before.ends_with("ftp")
            && !before.chars().all(|c| c.is_ascii_digit())
            && before.split_whitespace().count() <= 4
        {
            return ContentGroup::Preference;
        }
    }
    ContentGroup::General
}

/// A detected conversation session suitable for compression.
pub struct ConversationSession {
    pub channel: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub memories: Vec<MemObject>,
}
