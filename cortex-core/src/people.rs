use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

use crate::storage::traits::StorageBackend;
use crate::types::MemObject;
use crate::CortexError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Person {
    pub id: Uuid,
    pub identities: Vec<ChannelIdentity>,
    pub display_name: String,
    pub relationship_to_user: String,
    pub first_seen: DateTime<Utc>,
    pub last_seen: DateTime<Utc>,
    pub interaction_count: u32,
    pub communication_style: HashMap<String, String>,
    pub tags: Vec<String>,
    pub notes: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChannelIdentity {
    pub channel: String,
    pub channel_user_id: String,
    pub username: Option<String>,
    pub display_name: Option<String>,
}

/// Context about a person, aggregated from all memory sources.
pub struct PersonContext {
    pub person: Person,
    pub related_memories: Vec<MemObject>,
}

/// People graph — cross-channel identity resolution and relationship tracking.
pub struct PeopleGraph<'a> {
    storage: &'a dyn StorageBackend,
}

impl<'a> PeopleGraph<'a> {
    pub fn new(storage: &'a dyn StorageBackend) -> Self {
        Self { storage }
    }

    /// Find or create a person by channel identity.
    pub fn resolve_identity(
        &self,
        channel: &str,
        channel_user_id: &str,
        display_name: Option<&str>,
        username: Option<&str>,
    ) -> Result<Person, CortexError> {
        // Try to find existing person
        if let Some(person) = self
            .storage
            .find_person_by_channel_identity(channel, channel_user_id)?
        {
            return Ok(person);
        }

        // Create new person
        let now = Utc::now();
        let name = display_name
            .or(username)
            .unwrap_or(channel_user_id)
            .to_string();
        let person = Person {
            id: Uuid::new_v4(),
            identities: vec![ChannelIdentity {
                channel: channel.to_string(),
                channel_user_id: channel_user_id.to_string(),
                username: username.map(String::from),
                display_name: display_name.map(String::from),
            }],
            display_name: name,
            relationship_to_user: String::new(),
            first_seen: now,
            last_seen: now,
            interaction_count: 0,
            communication_style: HashMap::new(),
            tags: Vec::new(),
            notes: Vec::new(),
        };
        self.storage.store_person(&person)?;
        Ok(person)
    }

    /// Merge two person records (cross-channel identity resolution).
    /// Keeps person_a as the primary, merges identities/notes from person_b.
    pub fn merge_identities(
        &self,
        person_a_id: Uuid,
        person_b_id: Uuid,
    ) -> Result<Person, CortexError> {
        let mut a = self
            .storage
            .get_person(person_a_id)?
            .ok_or_else(|| CortexError::NotFound(format!("Person {}", person_a_id)))?;
        let b = self
            .storage
            .get_person(person_b_id)?
            .ok_or_else(|| CortexError::NotFound(format!("Person {}", person_b_id)))?;

        // Merge identities
        for identity in b.identities {
            if !a
                .identities
                .iter()
                .any(|i| i.channel == identity.channel && i.channel_user_id == identity.channel_user_id)
            {
                a.identities.push(identity);
            }
        }

        // Merge notes
        for note in b.notes {
            if !a.notes.contains(&note) {
                a.notes.push(note);
            }
        }

        // Merge tags
        for tag in b.tags {
            if !a.tags.contains(&tag) {
                a.tags.push(tag);
            }
        }

        // Merge communication styles
        for (k, v) in b.communication_style {
            a.communication_style.entry(k).or_insert(v);
        }

        // Take earliest first_seen, latest last_seen
        if b.first_seen < a.first_seen {
            a.first_seen = b.first_seen;
        }
        if b.last_seen > a.last_seen {
            a.last_seen = b.last_seen;
        }
        a.interaction_count += b.interaction_count;

        // Delete person_b first so its channel_identities are cascade-deleted,
        // then update person_a with the merged identities.
        self.storage.delete_person(person_b_id)?;
        self.storage.update_person(&a)?;
        Ok(a)
    }

    /// Get all memories related to a person.
    pub fn get_context(&self, person_id: Uuid) -> Result<PersonContext, CortexError> {
        let person = self
            .storage
            .get_person(person_id)?
            .ok_or_else(|| CortexError::NotFound(format!("Person {}", person_id)))?;

        let related_memories = self
            .storage
            .list_memories_by_source_identity(person_id)?;

        Ok(PersonContext {
            person,
            related_memories,
        })
    }

    /// Get the full social graph as (person_a, person_b, relation) triples.
    /// Extracts relationship memories from the store.
    pub fn get_relationship_graph(&self) -> Result<Vec<(Person, Person, String)>, CortexError> {
        let people = self.storage.list_people()?;
        // Build HashMap for O(1) lookups instead of O(N) linear search per relationship
        let people_map: HashMap<Uuid, &Person> = people.iter().map(|p| (p.id, p)).collect();
        let mut graph = Vec::new();

        // Find Relationship content in semantic memories
        let semantic_mems = self
            .storage
            .list_by_tier(crate::types::MemoryTier::Semantic, 1000)?;

        for mem in &semantic_mems {
            if let crate::types::MemContent::Relationship {
                person_a,
                person_b,
                ref relation,
            } = mem.content
            {
                if let (Some(pa), Some(pb)) = (
                    people_map.get(&person_a),
                    people_map.get(&person_b),
                ) {
                    graph.push(((*pa).clone(), (*pb).clone(), relation.clone()));
                }
            }
        }
        Ok(graph)
    }

    /// Update interaction count and last_seen for a person.
    pub fn record_interaction(&self, person_id: Uuid) -> Result<(), CortexError> {
        if let Some(mut person) = self.storage.get_person(person_id)? {
            person.interaction_count += 1;
            person.last_seen = Utc::now();
            self.storage.update_person(&person)?;
        }
        Ok(())
    }
}
