use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

/// A memory object — the atomic unit of Cortex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemObject {
    pub id: Uuid,
    pub tier: MemoryTier,
    pub content: MemContent,
    pub embedding: Option<Vec<f32>>,
    pub temporal: TemporalInfo,
    pub source: MemSource,
    pub salience: Salience,
    pub privacy: PrivacyLevel,
    pub links: Vec<MemLink>,
    pub tags: Vec<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryTier {
    Working,
    Episodic,
    Semantic,
    Procedural,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "episodic" => Some(Self::Episodic),
            "semantic" => Some(Self::Semantic),
            "procedural" => Some(Self::Procedural),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TemporalInfo {
    pub event_time: Option<DateTime<Utc>>,
    pub ingestion_time: DateTime<Utc>,
    pub last_accessed: DateTime<Utc>,
    pub access_count: u32,
    pub relevance_schedule: Option<String>,
}

impl TemporalInfo {
    pub fn now() -> Self {
        let now = Utc::now();
        Self {
            event_time: None,
            ingestion_time: now,
            last_accessed: now,
            access_count: 0,
            relevance_schedule: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Salience {
    pub base_score: f32,
    pub emotional_weight: f32,
    pub access_boost: f32,
    pub decay_factor: f32,
    pub effective_score: f32,
}

impl Salience {
    pub fn new(base_score: f32) -> Self {
        Self {
            base_score,
            emotional_weight: 1.0,
            access_boost: 1.0,
            decay_factor: 1.0,
            effective_score: base_score,
        }
    }

    pub fn recompute(&mut self) {
        self.effective_score =
            self.base_score * self.emotional_weight * self.access_boost * self.decay_factor;
    }
}

impl Default for Salience {
    fn default() -> Self {
        Self::new(0.5)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MemContent {
    Text(String),
    Fact {
        subject: String,
        predicate: String,
        object: String,
    },
    Preference {
        key: String,
        value: String,
        confidence: f32,
    },
    Relationship {
        person_a: Uuid,
        person_b: Uuid,
        relation: String,
    },
    Pattern {
        trigger: String,
        actions: Vec<String>,
        frequency: u32,
    },
    Event {
        title: String,
        start: DateTime<Utc>,
        end: Option<DateTime<Utc>>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemSource {
    pub channel: String,
    pub identity_id: Option<Uuid>,
    pub chat_id: Option<String>,
    pub thread_id: Option<String>,
    pub message_id: Option<String>,
}

impl MemSource {
    pub fn new(channel: impl Into<String>) -> Self {
        Self {
            channel: channel.into(),
            identity_id: None,
            chat_id: None,
            thread_id: None,
            message_id: None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum PrivacyLevel {
    Private,
    Shared { scope: String },
    Public,
}

impl Default for PrivacyLevel {
    fn default() -> Self {
        Self::Private
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemLink {
    pub target_id: Uuid,
    pub relation: LinkRelation,
    pub strength: f32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum LinkRelation {
    RelatedTo,
    Supports,
    Contradicts,
    Supersedes,
    PartOf,
    CausedBy,
    LeadsTo,
}

impl LinkRelation {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RelatedTo => "related_to",
            Self::Supports => "supports",
            Self::Contradicts => "contradicts",
            Self::Supersedes => "supersedes",
            Self::PartOf => "part_of",
            Self::CausedBy => "caused_by",
            Self::LeadsTo => "leads_to",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "related_to" => Some(Self::RelatedTo),
            "supports" => Some(Self::Supports),
            "contradicts" => Some(Self::Contradicts),
            "supersedes" => Some(Self::Supersedes),
            "part_of" => Some(Self::PartOf),
            "caused_by" => Some(Self::CausedBy),
            "leads_to" => Some(Self::LeadsTo),
            _ => None,
        }
    }
}

/// Builder for constructing MemObject instances.
pub struct MemObjectBuilder {
    tier: MemoryTier,
    content: MemContent,
    source: MemSource,
    embedding: Option<Vec<f32>>,
    salience: Salience,
    privacy: PrivacyLevel,
    tags: Vec<String>,
    metadata: HashMap<String, serde_json::Value>,
    event_time: Option<DateTime<Utc>>,
}

impl MemObjectBuilder {
    pub fn new(tier: MemoryTier, content: MemContent, source: MemSource) -> Self {
        Self {
            tier,
            content,
            source,
            embedding: None,
            salience: Salience::default(),
            privacy: PrivacyLevel::default(),
            tags: Vec::new(),
            metadata: HashMap::new(),
            event_time: None,
        }
    }

    pub fn embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(embedding);
        self
    }

    pub fn salience(mut self, salience: Salience) -> Self {
        self.salience = salience;
        self
    }

    pub fn privacy(mut self, privacy: PrivacyLevel) -> Self {
        self.privacy = privacy;
        self
    }

    pub fn tags(mut self, tags: Vec<String>) -> Self {
        self.tags = tags;
        self
    }

    pub fn event_time(mut self, time: DateTime<Utc>) -> Self {
        self.event_time = Some(time);
        self
    }

    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn build(self) -> MemObject {
        let now = Utc::now();
        MemObject {
            id: Uuid::new_v4(),
            tier: self.tier,
            content: self.content,
            embedding: self.embedding,
            temporal: TemporalInfo {
                event_time: self.event_time,
                ingestion_time: now,
                last_accessed: now,
                access_count: 0,
                relevance_schedule: None,
            },
            source: self.source,
            salience: self.salience,
            privacy: self.privacy,
            links: Vec::new(),
            tags: self.tags,
            metadata: self.metadata,
        }
    }
}
