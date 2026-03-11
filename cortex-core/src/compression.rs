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

/// Compression engine — reduces episodic bloat while preserving knowledge.
pub struct CompressionEngine<'a> {
    storage: &'a dyn StorageBackend,
    index: &'a MemoryIndex,
}

impl<'a> CompressionEngine<'a> {
    pub fn new(storage: &'a dyn StorageBackend, index: &'a MemoryIndex) -> Self {
        Self { storage, index }
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
        let episodes = self
            .storage
            .list_by_tier_ordered_by_ingestion(MemoryTier::Episodic, 10_000)?;

        // Group by (channel, date) to find sessions
        let mut sessions: std::collections::HashMap<String, Vec<MemObject>> =
            std::collections::HashMap::new();

        for mem in episodes {
            if mem.temporal.ingestion_time > cutoff {
                continue; // too recent, don't compress yet
            }
            let date = mem.temporal.ingestion_time.format("%Y-%m-%d").to_string();
            let key = format!("{}|{}", mem.source.channel, date);
            sessions.entry(key).or_default().push(mem);
        }

        let mut result = Vec::new();
        for (key, memories) in sessions {
            if memories.len() < min_messages {
                continue;
            }
            let parts: Vec<&str> = key.splitn(2, '|').collect();
            let channel = parts[0].to_string();
            let start = memories
                .iter()
                .map(|m| m.temporal.ingestion_time)
                .min()
                .unwrap();
            let end = memories
                .iter()
                .map(|m| m.temporal.ingestion_time)
                .max()
                .unwrap();

            result.push(ConversationSession {
                channel,
                start,
                end,
                memories,
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

        // Create compressed summary
        let summary = self.build_summary(&texts, session.memories.len());

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

        // Delete originals
        for mem in &session.memories {
            self.storage.delete_memory(mem.id)?;
            self.index.remove(&mem.id);
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
    /// deduplicate, and concatenate with timestamps.
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

        // Score by information density (longer + more unique words = more informative)
        let mut scored: Vec<(&String, f32)> = unique_texts
            .iter()
            .map(|t| {
                let words: std::collections::HashSet<&str> = t.split_whitespace().collect();
                let score = words.len() as f32 * (1.0 + (t.len() as f32 / 100.0).ln().max(0.0));
                (*t, score)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        // Keep top sentences up to ~500 chars
        let mut summary_parts = Vec::new();
        let mut total_len = 0;
        for (text, _) in &scored {
            if total_len + text.len() > 500 && !summary_parts.is_empty() {
                break;
            }
            summary_parts.push(text.as_str());
            total_len += text.len();
        }

        let header = format!("[Compressed: {} messages]", original_count);
        if summary_parts.is_empty() {
            header
        } else {
            format!("{} {}", header, summary_parts.join(" | "))
        }
    }
}

/// A detected conversation session suitable for compression.
pub struct ConversationSession {
    pub channel: String,
    pub start: DateTime<Utc>,
    pub end: DateTime<Utc>,
    pub memories: Vec<MemObject>,
}
