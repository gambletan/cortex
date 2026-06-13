use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Sha256, Digest};
use std::collections::HashMap;
use std::sync::Arc;
use uuid::Uuid;

/// Whether a memory is temporary (will decay fast) or permanent (should persist).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum MemoryDurability {
    /// Default — decay follows normal importance-aware curve.
    #[default]
    Normal,
    /// Temporary — aggressive decay (e.g., "debugging X right now").
    Temporary,
    /// Permanent — never decay below a floor (e.g., "I live in Shanghai").
    Permanent,
}

impl MemoryDurability {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Temporary => "temporary",
            Self::Permanent => "permanent",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "temporary" => Self::Temporary,
            "permanent" => Self::Permanent,
            _ => Self::Normal,
        }
    }
}

/// A memory object — the atomic unit of Cortex.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MemObject {
    pub id: Uuid,
    pub tier: MemoryTier,
    pub content: MemContent,
    /// Embedding vector wrapped in Arc for cheap cloning (768-dim = 3KiB).
    #[serde(serialize_with = "serialize_arc_embedding", deserialize_with = "deserialize_arc_embedding")]
    pub embedding: Option<Arc<Vec<f32>>>,
    pub temporal: TemporalInfo,
    pub source: MemSource,
    pub salience: Salience,
    pub privacy: PrivacyLevel,
    pub links: Vec<MemLink>,
    pub tags: Vec<String>,
    #[serde(serialize_with = "ordered_map")]
    pub metadata: HashMap<String, serde_json::Value>,
    /// SHA-256 hash of content for deduplication.
    #[serde(default)]
    pub content_hash: Option<String>,
    /// Namespace for access control isolation.
    #[serde(default)]
    pub namespace: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryTier {
    Working,
    Episodic,
    Semantic,
    Procedural,
    /// Cold storage for old/low-salience memories.
    Archived,
}

impl MemoryTier {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Episodic => "episodic",
            Self::Semantic => "semantic",
            Self::Procedural => "procedural",
            Self::Archived => "archived",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "working" => Some(Self::Working),
            "episodic" => Some(Self::Episodic),
            "semantic" => Some(Self::Semantic),
            "procedural" => Some(Self::Procedural),
            "archived" => Some(Self::Archived),
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
    /// Whether this memory is temporary or permanent.
    #[serde(default)]
    pub durability: MemoryDurability,
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
            durability: MemoryDurability::default(),
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

impl MemContent {
    /// Securely clear all sensitive text fields in this content.
    /// Call when dropping memories from caches or working memory.
    pub fn zeroize_content(&mut self) {
        use zeroize::Zeroize;
        match self {
            MemContent::Text(s) => s.zeroize(),
            MemContent::Fact { subject, predicate, object } => {
                subject.zeroize();
                predicate.zeroize();
                object.zeroize();
            }
            MemContent::Preference { key, value, .. } => {
                key.zeroize();
                value.zeroize();
            }
            MemContent::Relationship { relation, .. } => {
                relation.zeroize();
            }
            MemContent::Pattern { trigger, actions, .. } => {
                trigger.zeroize();
                for a in actions {
                    a.zeroize();
                }
            }
            MemContent::Event { title, .. } => {
                title.zeroize();
            }
        }
    }
}

impl Drop for MemContent {
    /// Wipe sensitive text from RAM when memory content is dropped, fulfilling the
    /// SECURITY.md "zeroized on drop" guarantee. Placed on `MemContent` (not `MemObject`)
    /// so it does not restrict moving non-sensitive fields (e.g. the `Arc` embedding) out
    /// of a `MemObject`, while still wiping content whenever a memory is dropped.
    fn drop(&mut self) {
        self.zeroize_content();
    }
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

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub enum PrivacyLevel {
    #[default]
    Private,
    Shared { scope: String },
    Public,
}

impl PrivacyLevel {
    /// Returns true if this privacy level allows syncing via oplog.
    /// Private memories (the default) never leave the local database.
    pub fn is_syncable(&self) -> bool {
        !matches!(self, PrivacyLevel::Private)
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

    pub fn parse(s: &str) -> Option<Self> {
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
    embedding: Option<Arc<Vec<f32>>>,
    salience: Salience,
    privacy: PrivacyLevel,
    tags: Vec<String>,
    metadata: HashMap<String, serde_json::Value>,
    event_time: Option<DateTime<Utc>>,
    durability: MemoryDurability,
    namespace: Option<String>,
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
            durability: MemoryDurability::default(),
            namespace: None,
        }
    }

    pub fn embedding(mut self, embedding: Vec<f32>) -> Self {
        self.embedding = Some(Arc::new(embedding));
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

    pub fn durability(mut self, durability: MemoryDurability) -> Self {
        self.durability = durability;
        self
    }

    pub fn meta(mut self, key: impl Into<String>, value: serde_json::Value) -> Self {
        self.metadata.insert(key.into(), value);
        self
    }

    pub fn namespace(mut self, ns: impl Into<String>) -> Self {
        self.namespace = Some(ns.into());
        self
    }

    pub fn build(self) -> MemObject {
        let now = Utc::now();
        let content_hash = compute_content_hash(&self.content);
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
                durability: self.durability,
            },
            source: self.source,
            salience: self.salience,
            privacy: self.privacy,
            links: Vec::new(),
            tags: self.tags,
            metadata: self.metadata,
            content_hash: Some(content_hash),
            namespace: self.namespace,
        }
    }
}

/// Serde helper: serialize a `HashMap` with keys in sorted order.
///
/// `HashMap` iteration order is randomized per process, so its serde_json output is
/// non-deterministic. Anything that hashes or signs a serialized object containing one
/// (e.g. the sync oplog's per-operation HMAC) would then compute a different digest on
/// every device, silently rejecting legitimate data as tampered. Emitting keys in sorted
/// order makes serialization canonical. Empty maps serialize identically to the default,
/// so this is backward-compatible with already-persisted data.
pub(crate) fn ordered_map<S, K, V>(map: &HashMap<K, V>, serializer: S) -> Result<S::Ok, S::Error>
where
    S: serde::Serializer,
    K: serde::Serialize + Ord,
    V: serde::Serialize,
{
    use serde::Serialize;
    let ordered: std::collections::BTreeMap<_, _> = map.iter().collect();
    ordered.serialize(serializer)
}

/// Serde helper: serialize Arc<Vec<f32>> as Option<Vec<f32>>.
fn serialize_arc_embedding<S: serde::Serializer>(
    val: &Option<Arc<Vec<f32>>>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match val {
        Some(arc) => serializer.serialize_some(arc.as_ref()),
        None => serializer.serialize_none(),
    }
}

/// Serde helper: deserialize Option<Vec<f32>> into Option<Arc<Vec<f32>>>.
fn deserialize_arc_embedding<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<Arc<Vec<f32>>>, D::Error> {
    let opt: Option<Vec<f32>> = Option::deserialize(deserializer)?;
    Ok(opt.map(Arc::new))
}

/// Compute SHA-256 hash of memory content for deduplication.
pub fn compute_content_hash(content: &MemContent) -> String {
    let text = match content {
        MemContent::Text(t) => t.clone(),
        MemContent::Fact { subject, predicate, object } => {
            format!("fact:{}:{}:{}", subject, predicate, object)
        }
        MemContent::Preference { key, value, .. } => format!("pref:{}:{}", key, value),
        MemContent::Relationship { person_a, person_b, relation } => {
            format!("rel:{}:{}:{}", person_a, person_b, relation)
        }
        MemContent::Pattern { trigger, actions, .. } => {
            format!("pattern:{}:{}", trigger, actions.join(","))
        }
        MemContent::Event { title, start, .. } => format!("event:{}:{}", title, start),
    };
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Configuration for batch ingest deduplication.
#[derive(Debug, Clone)]
pub struct DeduplicationConfig {
    /// Enable exact content hash dedup.
    pub exact_dedup: bool,
    /// Enable near-duplicate detection via vector similarity.
    pub near_dedup: bool,
    /// Cosine similarity threshold for near-dedup (0.0-1.0).
    pub near_dedup_threshold: f32,
}

impl Default for DeduplicationConfig {
    fn default() -> Self {
        Self {
            exact_dedup: true,
            near_dedup: false,
            near_dedup_threshold: 0.95,
        }
    }
}

/// Item for batch ingest.
pub struct BatchIngestItem {
    pub text: String,
    pub channel: String,
    pub user_id: Option<String>,
    pub salience_hint: Option<f32>,
    pub embedding: Option<Vec<f32>>,
    pub namespace: Option<String>,
}

/// Result of a batch ingest operation.
#[derive(Debug, Default)]
pub struct BatchIngestReport {
    pub total_submitted: usize,
    pub stored: usize,
    pub deduplicated: usize,
    pub skipped_by_plugin: usize,
}

/// Memory event for the change feed.
#[derive(Debug, Clone)]
pub enum CortexEvent {
    MemoryCreated { id: Uuid, tier: MemoryTier },
    MemoryUpdated { id: Uuid },
    MemoryDeleted { id: Uuid },
    MemoryArchived { id: Uuid },
    ConsolidationCompleted { report: String },
    DecayCompleted { count: usize },
}

#[cfg(test)]
mod zeroize_drop_tests {
    use super::*;

    /// Vuln fix wiring guarantee: `MemContent` MUST have an explicit `Drop` impl so that
    /// content is zeroized automatically. The `T: Drop` bound is satisfied ONLY by types
    /// with an explicit `impl Drop` — so if someone deletes `impl Drop for MemContent`,
    /// this stops compiling. This is the discriminator the runtime test below cannot be.
    #[test]
    fn memcontent_has_explicit_drop_impl_for_zeroize() {
        #[allow(drop_bounds)]
        fn requires_explicit_drop<T: Drop>() {}
        requires_explicit_drop::<MemContent>();
    }

    /// The wipe that `Drop` runs must clear the actual heap bytes (not merely reset len).
    /// Observed soundly: `ManuallyDrop` keeps the `String` allocation alive, and
    /// `String::zeroize` zeroes the buffer in place (keeping the allocation), so reading
    /// the live buffer afterward is sound — no freed-memory read.
    #[test]
    fn zeroize_content_clears_heap_bytes() {
        use std::mem::ManuallyDrop;

        let canary = "ULTRASECRET_CANARY_7f3a";
        let mut content = ManuallyDrop::new(MemContent::Text(canary.to_string()));

        let (ptr, len) = match &*content {
            MemContent::Text(s) => (s.as_ptr(), s.len()),
            _ => unreachable!(),
        };

        // Precondition: the live buffer holds the canary.
        // SAFETY: `content` is alive (ManuallyDrop); the buffer is valid and initialized.
        let before = unsafe { std::slice::from_raw_parts(ptr, len) };
        assert_eq!(before, canary.as_bytes(), "precondition: canary present");

        // Exercise the exact wipe that `impl Drop for MemContent` runs.
        content.zeroize_content();

        // Buffer is still allocated (ManuallyDrop) but its bytes must now be zero.
        // SAFETY: buffer not freed; sound read of live memory.
        let after = unsafe { std::slice::from_raw_parts(ptr, len) };
        assert!(
            after.iter().all(|&b| b == 0),
            "content heap bytes must be zeroized, found: {:?}",
            after
        );

        // Cleanup: actually drop (frees the buffer).
        unsafe { ManuallyDrop::drop(&mut content) };
    }
}
