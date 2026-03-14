use uuid::Uuid;

use crate::belief::BeliefEngine;
use crate::people::PeopleGraph;
use crate::procedural::ProceduralStore;
use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;

/// Configuration for context generation.
pub struct ContextConfig {
    pub max_tokens: usize,
    pub include_people: bool,
    pub include_preferences: bool,
    pub include_recent_episodes: usize,
    pub include_patterns: bool,
    pub channel: Option<String>,
    pub person_id: Option<Uuid>,
}

impl Default for ContextConfig {
    fn default() -> Self {
        Self {
            max_tokens: 2000,
            include_people: true,
            include_preferences: true,
            include_recent_episodes: 10,
            include_patterns: true,
            channel: None,
            person_id: None,
        }
    }
}

impl ContextConfig {
    pub fn new(max_tokens: usize) -> Self {
        Self {
            max_tokens,
            ..Default::default()
        }
    }

    pub fn with_channel(mut self, channel: impl Into<String>) -> Self {
        self.channel = Some(channel.into());
        self
    }

    pub fn with_person(mut self, person_id: Uuid) -> Self {
        self.person_id = Some(person_id);
        self
    }

    pub fn with_recent_episodes(mut self, count: usize) -> Self {
        self.include_recent_episodes = count;
        self
    }
}

/// A prioritized context section with its items sorted by salience.
struct ContextSection {
    header: String,
    /// (salience_score, formatted_line)
    items: Vec<(f32, String)>,
    /// Budget weight — higher means more chars allocated proportionally.
    weight: f32,
    /// If true, preserve insertion order instead of sorting by salience.
    preserve_order: bool,
}

impl ContextSection {
    fn new(header: impl Into<String>, weight: f32) -> Self {
        Self {
            header: header.into(),
            items: Vec::new(),
            weight,
            preserve_order: false,
        }
    }

    fn ordered(header: impl Into<String>, weight: f32) -> Self {
        Self {
            header: header.into(),
            items: Vec::new(),
            weight,
            preserve_order: true,
        }
    }

    fn push(&mut self, salience: f32, line: String) {
        self.items.push((salience, line));
    }

    /// Sort items by salience descending (unless preserve_order is set),
    /// deduplicate near-identical lines, then render up to `char_budget` chars.
    fn render(&mut self, char_budget: usize) -> Option<String> {
        if self.items.is_empty() {
            return None;
        }
        // Sort by salience descending — most important first (unless time-ordered)
        if !self.preserve_order {
            self.items.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
        }

        // Deduplicate: skip items whose normalized text closely matches an already-included item.
        // This prevents "User lives_in Shanghai" appearing twice from different sources.
        let mut out = self.header.clone();
        let mut used = out.len();
        let mut seen: Vec<String> = Vec::new();

        for (_, line) in &self.items {
            if used + line.len() > char_budget {
                break;
            }
            let normalized = normalize_for_dedup(line);
            if seen.iter().any(|s| is_near_duplicate(s, &normalized)) {
                continue;
            }
            seen.push(normalized);
            out.push_str(line);
            used += line.len();
        }
        if out.len() > self.header.len() {
            Some(out)
        } else {
            None
        }
    }
}

/// Generate LLM-ready context from Cortex memory state.
/// Sections are budget-allocated by priority weight; items within each section
/// are sorted by salience descending so the most important info always appears.
pub fn generate_context(
    config: &ContextConfig,
    storage: &dyn StorageBackend,
    _index: &MemoryIndex,
) -> Result<String, CortexError> {
    let chars_budget = config.max_tokens * 4; // ~1 token ≈ 4 chars
    let header = "[Cortex Memory Context]";
    let header_len = header.len();
    let available = chars_budget.saturating_sub(header_len);

    let mut sections: Vec<ContextSection> = Vec::new();

    // ── User preferences + Semantic facts (single query) ──────────────
    if config.include_preferences {
        // Load all semantic memories once instead of two separate queries
        let semantic_mems = storage.list_by_tier(MemoryTier::Semantic, 10_000)?;

        let mut pref_sec = ContextSection::new("\n## User Profile\n", 25.0);
        let mut fact_sec = ContextSection::new("\n## Known Facts\n", 25.0);

        for mem in &semantic_mems {
            match &mem.content {
                MemContent::Preference { key, value, confidence } => {
                    let line = format!("- {} = {} (confidence: {:.0}%)\n", key, value, confidence * 100.0);
                    pref_sec.push(mem.salience.base_score, line);
                }
                MemContent::Fact { subject, predicate, object } => {
                    let line = format!("- {} {} {} (confidence: {:.0}%)\n",
                        subject, predicate, object, mem.salience.base_score * 100.0);
                    fact_sec.push(mem.salience.base_score, line);
                }
                _ => {}
            }
        }

        if !pref_sec.items.is_empty() {
            sections.push(pref_sec);
        }
        if !fact_sec.items.is_empty() {
            sections.push(fact_sec);
        }
    }

    // ── Conversation partner (weight 10%) ───────────────────────────────
    if let Some(person_id) = config.person_id {
        if config.include_people {
            let people = PeopleGraph::new(storage);
            if let Ok(ctx) = people.get_context(person_id) {
                let mut sec = ContextSection::new("\n## Current Conversation Partner\n", 10.0);
                sec.push(1.0, format!("- Name: {}\n", ctx.person.display_name));
                if !ctx.person.relationship_to_user.is_empty() {
                    sec.push(0.9, format!("- Relationship: {}\n", ctx.person.relationship_to_user));
                }
                sec.push(0.5, format!("- Interactions: {}\n", ctx.person.interaction_count));
                if !ctx.person.tags.is_empty() {
                    sec.push(0.4, format!("- Tags: {}\n", ctx.person.tags.join(", ")));
                }
                for mem in ctx.related_memories.iter().take(3) {
                    let summary = summarize_content(&mem.content);
                    sec.push(mem.salience.base_score, format!("  - {}\n", summary));
                }
                sections.push(sec);
            }
        }
    }

    // ── Recent episodes (weight 20%) ────────────────────────────────────
    if config.include_recent_episodes > 0 {
        let recent = if let Some(ref ch) = config.channel {
            storage.list_by_channel(ch, config.include_recent_episodes)?
        } else {
            storage.list_by_tier_ordered_by_ingestion(
                MemoryTier::Episodic,
                config.include_recent_episodes,
            )?
        };

        if !recent.is_empty() {
            let mut sec = ContextSection::ordered("\n## Recent Context\n", 20.0);
            for mem in &recent {
                let summary = summarize_content(&mem.content);
                let time = mem.temporal.event_time.unwrap_or(mem.temporal.ingestion_time);
                let line = format!("- [{}] {}\n", time.format("%Y-%m-%d %H:%M"), summary);
                sec.push(mem.salience.base_score, line);
            }
            sections.push(sec);
        }
    }

    // ── Active patterns (weight 10%) ────────────────────────────────────
    if config.include_patterns {
        let proc_store = ProceduralStore::new(storage);
        let patterns = proc_store.detect_patterns(3)?;
        if !patterns.is_empty() {
            let mut sec = ContextSection::new("\n## Active Patterns\n", 10.0);
            for p in patterns.iter().take(5) {
                let line = format!("- When \"{}\": {} (seen {}x)\n",
                    p.trigger, p.actions.join(", "), p.frequency);
                sec.push(p.frequency as f32 / 100.0, line);
            }
            sections.push(sec);
        }
    }

    // ── Beliefs (weight 10%) ────────────────────────────────────────────
    let belief_engine = BeliefEngine::new(storage);
    let beliefs = belief_engine.get_confident_beliefs(0.8)?;
    if !beliefs.is_empty() {
        let mut sec = ContextSection::new("\n## Beliefs\n", 10.0);
        for b in &beliefs {
            let direction = if b.probability > 0.5 { "likely" } else { "unlikely" };
            let line = format!("- {} ({}, {:.0}%)\n", b.key, direction, b.probability * 100.0);
            // Use deviation from 0.5 as salience — stronger beliefs rank higher
            let strength = (b.probability - 0.5).abs() * 2.0;
            sec.push(strength, line);
        }
        sections.push(sec);
    }

    // ── Budget allocation & rendering ───────────────────────────────────
    // Two-pass: first pass allocates proportionally, second pass redistributes
    // unused budget to sections that could use more.
    let total_weight: f32 = sections.iter().map(|s| s.weight).sum();
    let mut result = String::from(header);

    if total_weight > 0.0 {
        // Pass 1: render each section with its proportional budget
        let mut rendered: Vec<Option<String>> = Vec::new();
        let mut leftover: usize = 0;

        for sec in &mut sections {
            let sec_budget = ((sec.weight / total_weight) * available as f32) as usize;
            let r = sec.render(sec_budget);
            let used = r.as_ref().map(|s| s.len()).unwrap_or(0);
            leftover += sec_budget.saturating_sub(used);
            rendered.push(r);
        }

        // Pass 2: if there's leftover, re-render sections that were budget-constrained
        if leftover > 100 {
            let bonus_per = leftover / sections.len().max(1);
            for (i, sec) in sections.iter_mut().enumerate() {
                let original_budget = ((sec.weight / total_weight) * available as f32) as usize;
                let expanded = sec.render(original_budget + bonus_per);
                if let Some(ref exp) = expanded {
                    if exp.len() > rendered[i].as_ref().map(|s| s.len()).unwrap_or(0) {
                        rendered[i] = expanded;
                    }
                }
            }
        }

        for r in rendered.into_iter().flatten() {
            result.push_str(&r);
        }
    }

    // Safety truncation (shouldn't normally trigger)
    if result.len() > chars_budget {
        result.truncate(chars_budget);
        if let Some(pos) = result.rfind('\n') {
            result.truncate(pos + 1);
        }
        result.push_str("...[truncated]\n");
    }

    Ok(result)
}

fn summarize_content(content: &MemContent) -> String {
    match content {
        MemContent::Text(t) => {
            if t.len() > 300 {
                format!("{}...", &t[..300])
            } else {
                t.clone()
            }
        }
        MemContent::Fact {
            subject,
            predicate,
            object,
        } => format!("{} {} {}", subject, predicate, object),
        MemContent::Preference {
            key,
            value,
            confidence,
        } => format!("{} = {} ({:.0}%)", key, value, confidence * 100.0),
        MemContent::Relationship {
            relation, ..
        } => format!("Relationship: {}", relation),
        MemContent::Pattern {
            trigger,
            actions,
            frequency,
        } => format!("Pattern: {} -> {} ({}x)", trigger, actions.join(", "), frequency),
        MemContent::Event { title, start, .. } => {
            format!("Event: {} at {}", title, start.format("%Y-%m-%d %H:%M"))
        }
    }
}

/// Normalize a line for deduplication: lowercase, strip punctuation/numbers/whitespace.
fn normalize_for_dedup(s: &str) -> String {
    s.to_lowercase()
        .chars()
        .filter(|c| c.is_alphanumeric() || *c == ' ')
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// Two normalized strings are near-duplicates if they share >80% of their words.
/// Only triggers when both strings have at least 3 words to avoid short-string false positives.
fn is_near_duplicate(a: &str, b: &str) -> bool {
    if a == b {
        return true;
    }
    let a_words: std::collections::HashSet<&str> = a.split_whitespace().collect();
    let b_words: std::collections::HashSet<&str> = b.split_whitespace().collect();
    if a_words.len() < 3 || b_words.len() < 3 {
        return false;
    }
    let overlap = a_words.intersection(&b_words).count();
    let min_len = a_words.len().min(b_words.len());
    overlap as f32 / min_len as f32 > 0.8
}
