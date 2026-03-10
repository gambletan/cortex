use uuid::Uuid;

use crate::belief::BeliefEngine;
use crate::people::PeopleGraph;
use crate::procedural::ProceduralStore;
use crate::semantic::SemanticStore;
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
            include_recent_episodes: 5,
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

/// Generate LLM-ready context from Cortex memory state.
pub fn generate_context(
    config: &ContextConfig,
    storage: &dyn StorageBackend,
    index: &MemoryIndex,
) -> Result<String, CortexError> {
    let mut sections: Vec<String> = Vec::new();
    let token_budget = config.max_tokens;

    // Rough estimate: 1 token ~= 4 characters
    let chars_budget = token_budget * 4;

    sections.push("[Cortex Memory Context]".to_string());

    // User preferences
    if config.include_preferences {
        let semantic = SemanticStore::new(storage, index);
        let prefs = semantic.query_preferences("")?;
        if !prefs.is_empty() {
            let mut pref_section = String::from("\n## User Profile\n");
            for mem in prefs.iter().take(10) {
                if let MemContent::Preference {
                    key,
                    value,
                    confidence,
                } = &mem.content
                {
                    let line = format!("- {} = {} (confidence: {:.0}%)\n", key, value, confidence * 100.0);
                    pref_section.push_str(&line);
                }
            }
            sections.push(pref_section);
        }
    }

    // Current conversation partner
    if let Some(person_id) = config.person_id {
        if config.include_people {
            let people = PeopleGraph::new(storage);
            if let Ok(ctx) = people.get_context(person_id) {
                let mut person_section = String::from("\n## Current Conversation Partner\n");
                person_section.push_str(&format!("- Name: {}\n", ctx.person.display_name));
                if !ctx.person.relationship_to_user.is_empty() {
                    person_section.push_str(&format!(
                        "- Relationship: {}\n",
                        ctx.person.relationship_to_user
                    ));
                }
                person_section.push_str(&format!(
                    "- Interactions: {}\n",
                    ctx.person.interaction_count
                ));
                if !ctx.person.tags.is_empty() {
                    person_section.push_str(&format!("- Tags: {}\n", ctx.person.tags.join(", ")));
                }
                if !ctx.related_memories.is_empty() {
                    person_section.push_str("- Recent shared context:\n");
                    for mem in ctx.related_memories.iter().take(3) {
                        let summary = summarize_content(&mem.content);
                        person_section.push_str(&format!("  - {}\n", summary));
                    }
                }
                sections.push(person_section);
            }
        }
    }

    // Recent episodes
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
            let mut episode_section = String::from("\n## Recent Context\n");
            for mem in &recent {
                let summary = summarize_content(&mem.content);
                let time = mem
                    .temporal
                    .event_time
                    .unwrap_or(mem.temporal.ingestion_time);
                episode_section.push_str(&format!(
                    "- [{}] {}\n",
                    time.format("%Y-%m-%d %H:%M"),
                    summary
                ));
            }
            sections.push(episode_section);
        }
    }

    // Active patterns
    if config.include_patterns {
        let proc_store = ProceduralStore::new(storage);
        let patterns = proc_store.detect_patterns(3)?;
        if !patterns.is_empty() {
            let mut pattern_section = String::from("\n## Active Patterns\n");
            for p in patterns.iter().take(5) {
                pattern_section.push_str(&format!(
                    "- When \"{}\": {} (seen {}x)\n",
                    p.trigger,
                    p.actions.join(", "),
                    p.frequency
                ));
            }
            sections.push(pattern_section);
        }
    }

    // Confident beliefs
    let belief_engine = BeliefEngine::new(storage);
    let beliefs = belief_engine.get_confident_beliefs(0.8)?;
    if !beliefs.is_empty() {
        let mut belief_section = String::from("\n## Beliefs\n");
        for b in beliefs.iter().take(5) {
            let direction = if b.probability > 0.5 { "likely" } else { "unlikely" };
            belief_section.push_str(&format!(
                "- {} ({}, {:.0}%)\n",
                b.key,
                direction,
                b.probability * 100.0
            ));
        }
        sections.push(belief_section);
    }

    // Join and truncate to budget
    let mut result = sections.join("");
    if result.len() > chars_budget {
        result.truncate(chars_budget);
        // Find the last newline to avoid cutting mid-line
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
            if t.len() > 100 {
                format!("{}...", &t[..100])
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
