use crate::belief::Belief;
use crate::people::{ChannelIdentity, Person};
use crate::procedural::Pattern;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;
use chrono::{DateTime, Utc};
use parking_lot::Mutex;
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Arc;
use uuid::Uuid;

/// Number of read-only connections in the pool.
const READ_POOL_SIZE: usize = 4;

/// Number of entries in the memory object cache.
const MEM_CACHE_SIZE: usize = 2048;

/// Number of entries in the entity-facts query cache.
const ENTITY_CACHE_SIZE: usize = 256;

/// SQLite-backed storage for Cortex. Local-first, zero-config.
/// Uses WAL mode with separate read/write connections for concurrency.
/// Includes an LRU cache for deserialized MemObjects to avoid repeated
/// JSON parsing and f16 embedding deserialization on hot-path lookups.
pub struct SqliteStorage {
    write_conn: Mutex<Connection>,
    read_pool: Vec<Mutex<Connection>>,
    /// LRU cache for deserialized MemObjects — avoids repeated JSON + embedding blob parsing.
    mem_cache: Mutex<lru::LruCache<Uuid, Arc<MemObject>>>,
    /// LRU cache for entity→facts query results. Keyed by lowercased entity pattern.
    /// Invalidated on any semantic tier write (store/update/delete).
    entity_cache: Mutex<lru::LruCache<String, Vec<MemObject>>>,
}

impl SqliteStorage {
    fn apply_read_pragmas(conn: &Connection) -> Result<(), CortexError> {
        conn.execute_batch(
            "PRAGMA cache_size=-64000;
             PRAGMA mmap_size=268435456;
             PRAGMA temp_store=MEMORY;
             PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| CortexError::Storage(e.to_string()))
    }

    pub fn open(path: &str) -> Result<Self, CortexError> {
        Self::open_with_key(path, None)
    }

    /// Open a Cortex database with optional encryption passphrase.
    /// Requires the `encrypted-db` feature (SQLCipher). Without the feature,
    /// the passphrase is ignored and the DB is opened without encryption.
    pub fn open_with_key(path: &str, passphrase: Option<&str>) -> Result<Self, CortexError> {
        let conn = Connection::open(path).map_err(|e| CortexError::Storage(e.to_string()))?;

        // Apply encryption key if provided (requires bundled-sqlcipher feature)
        #[cfg(feature = "encrypted-db")]
        if let Some(key) = passphrase {
            conn.pragma_update(None, "key", key)
                .map_err(|e| CortexError::Storage(format!("Failed to set encryption key: {}", e)))?;
        }
        #[cfg(not(feature = "encrypted-db"))]
        if passphrase.is_some() {
            tracing::warn!("Database encryption requested but 'encrypted-db' feature not enabled. Opening without encryption.");
        }

        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             PRAGMA cache_size=-64000;
             PRAGMA mmap_size=268435456;
             PRAGMA temp_store=MEMORY;
             PRAGMA foreign_keys=ON;
             PRAGMA wal_autocheckpoint=1000;
             PRAGMA busy_timeout=5000;",
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;

        // Open read-only connections for concurrent reads
        let mut read_pool = Vec::with_capacity(READ_POOL_SIZE);
        for _ in 0..READ_POOL_SIZE {
            let reader = Connection::open_with_flags(
                path,
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY
                    | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX
                    | rusqlite::OpenFlags::SQLITE_OPEN_URI,
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
            #[cfg(feature = "encrypted-db")]
            if let Some(key) = passphrase {
                reader.pragma_update(None, "key", key)
                    .map_err(|e| CortexError::Storage(format!("Failed to set reader encryption key: {}", e)))?;
            }
            Self::apply_read_pragmas(&reader)?;
            read_pool.push(Mutex::new(reader));
        }

        let storage = Self {
            write_conn: Mutex::new(conn),
            read_pool,
            mem_cache: Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MEM_CACHE_SIZE).unwrap(),
            )),
            entity_cache: Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(ENTITY_CACHE_SIZE).unwrap(),
            )),
        };
        storage.init()?;
        Ok(storage)
    }

    pub fn open_in_memory() -> Result<Self, CortexError> {
        let conn =
            Connection::open_in_memory().map_err(|e| CortexError::Storage(e.to_string()))?;
        conn.execute_batch("PRAGMA foreign_keys=ON;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        // In-memory DBs can't share connections, so read_pool is empty
        let storage = Self {
            write_conn: Mutex::new(conn),
            read_pool: Vec::new(),
            mem_cache: Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(MEM_CACHE_SIZE).unwrap(),
            )),
            entity_cache: Mutex::new(lru::LruCache::new(
                std::num::NonZeroUsize::new(ENTITY_CACHE_SIZE).unwrap(),
            )),
        };
        storage.init()?;
        Ok(storage)
    }

    /// Get a read connection. Tries each reader first, falls back to write conn.
    fn read_conn(&self) -> Result<parking_lot::MutexGuard<'_, Connection>, CortexError> {
        for reader in &self.read_pool {
            if let Some(guard) = reader.try_lock() {
                return Ok(guard);
            }
        }
        // All readers busy or no readers (in-memory mode): use write conn
        if let Some(reader) = self.read_pool.first() {
            return Ok(reader.lock());
        }
        Ok(self.write_conn.lock())
    }

    /// Execute a closure with access to the write connection.
    /// Used by the sync module to run state operations on the same DB.
    pub fn with_write_conn<F, R>(&self, f: F) -> Result<R, CortexError>
    where
        F: FnOnce(&Connection) -> Result<R, CortexError>,
    {
        let conn = self.write_conn.lock();
        f(&conn)
    }

    /// Invalidate a single entry from the memory cache.
    fn cache_evict(&self, id: &Uuid) {
        self.mem_cache.lock().pop(id);
    }

    /// Invalidate all entries from the memory cache.
    #[allow(dead_code)]
    fn cache_clear(&self) {
        self.mem_cache.lock().clear();
    }

    /// Put a MemObject into the cache. Also invalidates entity cache for semantic writes.
    fn cache_put(&self, mem: &MemObject) {
        self.mem_cache.lock().put(mem.id, Arc::new(mem.clone()));
        // Invalidate entity cache when semantic facts/prefs change
        if mem.tier == MemoryTier::Semantic {
            self.entity_cache.lock().clear();
        }
    }

    fn parse_mem_row(row: &rusqlite::Row) -> Result<MemObject, rusqlite::Error> {
        let id_str: String = row.get(0)?;
        let tier_str: String = row.get(1)?;
        let content_json: String = row.get(2)?;
        let embedding_blob: Option<Vec<u8>> = row.get(3)?;
        let temporal_json: String = row.get(4)?;
        let source_json: String = row.get(5)?;
        let salience_json: String = row.get(6)?;
        let privacy_json: String = row.get(7)?;
        let tags_json: String = row.get(8)?;
        let metadata_json: String = row.get(9)?;
        let links_json: String = row.get(10)?;
        // Columns 11+ are optional (content_hash, namespace added by migration)
        let content_hash: Option<String> = row.get(11).ok().unwrap_or(None);
        let namespace: Option<String> = row.get(12).ok().unwrap_or(None);

        let id = Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4());
        let tier = MemoryTier::parse(&tier_str).unwrap_or(MemoryTier::Episodic);
        let content: MemContent = serde_json::from_str(&content_json).unwrap();
        let embedding = embedding_blob.map(|b| std::sync::Arc::new(bytes_to_f32_vec(&b)));
        let temporal: TemporalInfo = serde_json::from_str(&temporal_json).unwrap();
        let source: MemSource = serde_json::from_str(&source_json).unwrap();
        let salience: Salience = serde_json::from_str(&salience_json).unwrap();
        // Fast-path: skip serde for common default values (avoids ~40% of JSON parsing)
        let privacy: PrivacyLevel = if privacy_json == "\"Private\"" {
            PrivacyLevel::Private
        } else {
            serde_json::from_str(&privacy_json).unwrap()
        };
        let tags: Vec<String> = if tags_json == "[]" {
            Vec::new()
        } else {
            serde_json::from_str(&tags_json).unwrap()
        };
        let metadata: std::collections::HashMap<String, serde_json::Value> = if metadata_json == "{}" {
            std::collections::HashMap::new()
        } else {
            serde_json::from_str(&metadata_json).unwrap()
        };
        let links: Vec<MemLink> = if links_json == "[]" {
            Vec::new()
        } else {
            serde_json::from_str(&links_json).unwrap()
        };

        Ok(MemObject {
            id,
            tier,
            content,
            embedding,
            temporal,
            source,
            salience,
            privacy,
            links,
            tags,
            metadata,
            content_hash,
            namespace,
        })
    }
}

// ── Fast-path serde constants for writes ─────────────────────────────────────
// Skip serde_json::to_string() for common default values — matches the read-side
// fast-path in parse_mem_row. Saves ~40% of JSON serialization on typical ingests.
const DEFAULT_PRIVACY_JSON: &str = "\"Private\"";
const DEFAULT_TAGS_JSON: &str = "[]";
const DEFAULT_METADATA_JSON: &str = "{}";
const DEFAULT_LINKS_JSON: &str = "[]";

/// Serialize privacy, returning fast-path constant for common default.
#[inline]
fn serialize_privacy(privacy: &PrivacyLevel) -> String {
    if matches!(privacy, PrivacyLevel::Private) {
        DEFAULT_PRIVACY_JSON.to_string()
    } else {
        serde_json::to_string(privacy).unwrap()
    }
}

/// Serialize tags, returning fast-path constant for empty.
#[inline]
fn serialize_tags(tags: &[String]) -> String {
    if tags.is_empty() {
        DEFAULT_TAGS_JSON.to_string()
    } else {
        serde_json::to_string(tags).unwrap()
    }
}

/// Serialize metadata, returning fast-path constant for empty.
#[inline]
fn serialize_metadata(metadata: &std::collections::HashMap<String, serde_json::Value>) -> String {
    if metadata.is_empty() {
        DEFAULT_METADATA_JSON.to_string()
    } else {
        serde_json::to_string(metadata).unwrap()
    }
}

/// Serialize links, returning fast-path constant for empty.
#[inline]
fn serialize_links(links: &[MemLink]) -> String {
    if links.is_empty() {
        DEFAULT_LINKS_JSON.to_string()
    } else {
        serde_json::to_string(links).unwrap()
    }
}

/// Extract materialized column values from memory content.
/// Returns (fact_subject, fact_object, pref_key) — all lowercased for indexed LIKE queries.
fn extract_materialized_columns(content: &MemContent) -> (Option<String>, Option<String>, Option<String>) {
    match content {
        MemContent::Fact { subject, object, .. } => {
            (Some(subject.to_lowercase()), Some(object.to_lowercase()), None)
        }
        MemContent::Preference { key, .. } => {
            (None, None, Some(key.to_lowercase()))
        }
        _ => (None, None, None),
    }
}

/// Magic prefix for f16-encoded embedding blobs (distinguishes from legacy f32).
const F16_MAGIC: [u8; 2] = [0xF1, 0x6F];

/// Magic prefix for int8-quantized embedding blobs.
/// Format: [0xI8, 0xQT] + 4 bytes min_f32 + 4 bytes max_f32 + N bytes quantized.
const I8_MAGIC: [u8; 2] = [0x18, 0xC7];

/// Serialize embedding as int8 quantized (1 byte/float + 8 bytes header) for ~75% storage reduction.
/// Uses linear quantization: value = min + (byte / 255.0) * (max - min).
fn f32_vec_to_bytes(v: &[f32]) -> Vec<u8> {
    // Header: magic(2) + min(4) + max(4) = 10 bytes overhead
    let mut min_val = f32::INFINITY;
    let mut max_val = f32::NEG_INFINITY;
    for &f in v {
        if f < min_val { min_val = f; }
        if f > max_val { max_val = f; }
    }
    let range = max_val - min_val;

    let mut buf = Vec::with_capacity(10 + v.len());
    buf.extend_from_slice(&I8_MAGIC);
    buf.extend_from_slice(&min_val.to_le_bytes());
    buf.extend_from_slice(&max_val.to_le_bytes());

    if range == 0.0 {
        // All values identical — quantize to 128
        buf.resize(10 + v.len(), 128);
    } else {
        let scale = 255.0 / range;
        for &f in v {
            buf.push(((f - min_val) * scale + 0.5) as u8);
        }
    }
    buf
}

/// Deserialize embedding — auto-detects int8 (I8 magic) vs f16 (F1 magic) vs legacy f32.
fn bytes_to_f32_vec(b: &[u8]) -> Vec<f32> {
    use half::f16;

    // Check for int8 magic prefix
    if b.len() >= 10 && b[0] == I8_MAGIC[0] && b[1] == I8_MAGIC[1] {
        let min_val = f32::from_le_bytes([b[2], b[3], b[4], b[5]]);
        let max_val = f32::from_le_bytes([b[6], b[7], b[8], b[9]]);
        let range = max_val - min_val;
        let data = &b[10..];
        let mut result = Vec::with_capacity(data.len());
        if range == 0.0 {
            result.resize(data.len(), min_val);
        } else {
            let inv_scale = range / 255.0;
            for &byte in data {
                result.push(min_val + byte as f32 * inv_scale);
            }
        }
        return result;
    }

    // Check for f16 magic prefix
    if b.len() >= 2 && b[0] == F16_MAGIC[0] && b[1] == F16_MAGIC[1] {
        let data = &b[2..];
        let mut result = Vec::with_capacity(data.len() / 2);
        for chunk in data.chunks_exact(2) {
            result.push(f16::from_le_bytes([chunk[0], chunk[1]]).to_f32());
        }
        return result;
    }

    // Legacy f32 format (no prefix)
    let mut result = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        result.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    result
}

impl StorageBackend for SqliteStorage {
    fn init(&self) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS memories (
                id TEXT PRIMARY KEY,
                tier TEXT NOT NULL,
                content_json TEXT NOT NULL,
                embedding_blob BLOB,
                temporal_json TEXT NOT NULL,
                source_json TEXT NOT NULL,
                salience_json TEXT NOT NULL,
                privacy_json TEXT NOT NULL DEFAULT '\"Private\"',
                tags_json TEXT NOT NULL DEFAULT '[]',
                metadata_json TEXT NOT NULL DEFAULT '{}',
                links_json TEXT NOT NULL DEFAULT '[]',
                created_at TEXT NOT NULL,
                updated_at TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_memories_tier ON memories(tier);
            CREATE INDEX IF NOT EXISTS idx_memories_created ON memories(created_at);
            CREATE INDEX IF NOT EXISTS idx_memories_source_channel
                ON memories(json_extract(source_json, '$.channel'));
            -- Serves list_by_tier_ordered_by_ingestion: ORDER BY event time without a
            -- full computed sort. Expression must match the query's ORDER BY exactly.
            CREATE INDEX IF NOT EXISTS idx_memories_tier_ingestion
                ON memories(tier, julianday(json_extract(temporal_json, '$.ingestion_time')) DESC);
            -- salience_score materialized column indexed via migration below

            CREATE TABLE IF NOT EXISTS links (
                source_id TEXT NOT NULL,
                target_id TEXT NOT NULL,
                relation TEXT NOT NULL,
                strength REAL NOT NULL,
                PRIMARY KEY (source_id, target_id, relation)
            );

            CREATE TABLE IF NOT EXISTS people (
                id TEXT PRIMARY KEY,
                display_name TEXT NOT NULL,
                relationship TEXT NOT NULL DEFAULT '',
                first_seen TEXT NOT NULL,
                last_seen TEXT NOT NULL,
                interaction_count INTEGER NOT NULL DEFAULT 0,
                tags_json TEXT NOT NULL DEFAULT '[]',
                notes_json TEXT NOT NULL DEFAULT '[]',
                communication_style_json TEXT NOT NULL DEFAULT '{}'
            );

            CREATE TABLE IF NOT EXISTS channel_identities (
                person_id TEXT NOT NULL,
                channel TEXT NOT NULL,
                channel_user_id TEXT NOT NULL,
                username TEXT,
                display_name TEXT,
                PRIMARY KEY (channel, channel_user_id),
                FOREIGN KEY (person_id) REFERENCES people(id) ON DELETE CASCADE
            );

            CREATE TABLE IF NOT EXISTS beliefs (
                id TEXT PRIMARY KEY,
                key TEXT NOT NULL UNIQUE,
                probability REAL NOT NULL,
                observations_json TEXT NOT NULL DEFAULT '[]',
                last_updated TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_beliefs_key ON beliefs(key);

            CREATE TABLE IF NOT EXISTS patterns (
                id TEXT PRIMARY KEY,
                trigger TEXT NOT NULL,
                actions_json TEXT NOT NULL,
                frequency INTEGER NOT NULL DEFAULT 1,
                last_seen TEXT NOT NULL
            );

            CREATE INDEX IF NOT EXISTS idx_patterns_trigger ON patterns(trigger);
            ",
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;

        // Migration: create FTS5 virtual table for full-text search
        let has_fts: bool = conn
            .prepare("SELECT * FROM memories_fts LIMIT 0")
            .is_ok();
        if !has_fts {
            conn.execute_batch(
                "CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(
                     id UNINDEXED, content, tokenize='unicode61'
                 );",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;

            // Backfill FTS5 from existing memories
            conn.execute_batch(
                "INSERT INTO memories_fts(id, content)
                 SELECT id, content_json FROM memories
                 WHERE id NOT IN (SELECT id FROM memories_fts);",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        }

        // Migration: add FTS5 sync triggers (replaces manual INSERT/DELETE per write)
        conn.execute_batch(
            "CREATE TRIGGER IF NOT EXISTS memories_fts_ai AFTER INSERT ON memories BEGIN
                 INSERT INTO memories_fts(id, content) VALUES (NEW.id, NEW.content_json);
             END;
             CREATE TRIGGER IF NOT EXISTS memories_fts_ad AFTER DELETE ON memories BEGIN
                 DELETE FROM memories_fts WHERE id = OLD.id;
             END;
             CREATE TRIGGER IF NOT EXISTS memories_fts_au AFTER UPDATE OF content_json ON memories BEGIN
                 DELETE FROM memories_fts WHERE id = OLD.id;
                 INSERT INTO memories_fts(id, content) VALUES (NEW.id, NEW.content_json);
             END;",
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;

        // Migration: add content_hash and namespace columns if missing
        let has_content_hash: bool = conn
            .prepare("SELECT content_hash FROM memories LIMIT 0")
            .is_ok();
        if !has_content_hash {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN content_hash TEXT;
                 ALTER TABLE memories ADD COLUMN namespace TEXT;
                 CREATE INDEX IF NOT EXISTS idx_memories_content_hash ON memories(content_hash);
                 CREATE INDEX IF NOT EXISTS idx_memories_namespace ON memories(namespace);",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        }

        // Migration: add materialized salience_score column
        let has_salience_score: bool = conn
            .prepare("SELECT salience_score FROM memories LIMIT 0")
            .is_ok();
        if !has_salience_score {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN salience_score REAL;
                 CREATE INDEX IF NOT EXISTS idx_memories_tier_salience ON memories(tier, salience_score);
                 UPDATE memories SET salience_score = json_extract(salience_json, '$.effective_score')
                     WHERE salience_score IS NULL;",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        }

        // Migration: add materialized columns for fact subject/object and preference key
        // Replaces json_extract() in WHERE clauses with indexed column lookups.
        let has_fact_subject: bool = conn
            .prepare("SELECT fact_subject FROM memories LIMIT 0")
            .is_ok();
        if !has_fact_subject {
            conn.execute_batch(
                "ALTER TABLE memories ADD COLUMN fact_subject TEXT;
                 ALTER TABLE memories ADD COLUMN fact_object TEXT;
                 ALTER TABLE memories ADD COLUMN pref_key TEXT;
                 ALTER TABLE memories ADD COLUMN source_channel TEXT;
                 ALTER TABLE memories ADD COLUMN source_identity_id TEXT;
                 CREATE INDEX IF NOT EXISTS idx_memories_fact_subject ON memories(fact_subject) WHERE fact_subject IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_memories_fact_object ON memories(fact_object) WHERE fact_object IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_memories_pref_key ON memories(pref_key) WHERE pref_key IS NOT NULL;
                 CREATE INDEX IF NOT EXISTS idx_memories_source_channel_mat ON memories(source_channel);
                 CREATE INDEX IF NOT EXISTS idx_memories_source_identity ON memories(source_identity_id) WHERE source_identity_id IS NOT NULL;
                 UPDATE memories SET
                     fact_subject = LOWER(json_extract(content_json, '$.Fact.subject')),
                     fact_object = LOWER(json_extract(content_json, '$.Fact.object')),
                     pref_key = LOWER(json_extract(content_json, '$.Preference.key')),
                     source_channel = json_extract(source_json, '$.channel'),
                     source_identity_id = json_extract(source_json, '$.identity_id');",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        }

        // Migration: sync tables (for cloud sync feature)
        crate::sync::state::init_sync_tables(&conn)?;

        Ok(())
    }

    fn store_memory(&self, mem: &MemObject) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        let now = Utc::now().to_rfc3339();
        let embedding_blob = mem.embedding.as_ref().map(|e| f32_vec_to_bytes(e));
        let content_json = serde_json::to_string(&mem.content).unwrap();
        let id_str = mem.id.to_string();
        let (fact_subject, fact_object, pref_key) = extract_materialized_columns(&mem.content);
        let mut stmt = conn
            .prepare_cached(
                "INSERT INTO memories (id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace, salience_score, fact_subject, fact_object, pref_key, source_channel, source_identity_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        stmt.execute(params![
                id_str,
                mem.tier.as_str(),
                content_json,
                embedding_blob,
                serde_json::to_string(&mem.temporal).unwrap(),
                serde_json::to_string(&mem.source).unwrap(),
                serde_json::to_string(&mem.salience).unwrap(),
                serialize_privacy(&mem.privacy),
                serialize_tags(&mem.tags),
                serialize_metadata(&mem.metadata),
                serialize_links(&mem.links),
                mem.content_hash,
                mem.namespace,
                mem.salience.effective_score,
                fact_subject,
                fact_object,
                pref_key,
                mem.source.channel,
                mem.source.identity_id.map(|id| id.to_string()),
                now,
                now,
            ])
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        // FTS5 synced via trigger (memories_fts_ai)
        self.cache_put(mem);
        Ok(())
    }

    fn get_memory(&self, id: Uuid) -> Result<Option<MemObject>, CortexError> {
        // Check cache first
        {
            let mut cache = self.mem_cache.lock();
            if let Some(cached) = cache.get(&id) {
                return Ok(Some((**cached).clone()));
            }
        }
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE id = ?1",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let result = stmt
            .query_row(params![id.to_string()], Self::parse_mem_row)
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        if let Some(ref mem) = result {
            self.cache_put(mem);
        }
        Ok(result)
    }

    fn update_memory(&self, mem: &MemObject) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        let now = Utc::now().to_rfc3339();
        let embedding_blob = mem.embedding.as_ref().map(|e| f32_vec_to_bytes(e));
        let content_json = serde_json::to_string(&mem.content).unwrap();
        let id_str = mem.id.to_string();
        let (fact_subject, fact_object, pref_key) = extract_materialized_columns(&mem.content);
        let mut stmt = conn
            .prepare_cached(
                "UPDATE memories SET tier=?2, content_json=?3, embedding_blob=?4, temporal_json=?5, source_json=?6, salience_json=?7, privacy_json=?8, tags_json=?9, metadata_json=?10, links_json=?11, content_hash=?12, namespace=?13, salience_score=?14, fact_subject=?15, fact_object=?16, pref_key=?17, source_channel=?18, source_identity_id=?19, updated_at=?20 WHERE id=?1",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        stmt.execute(params![
                id_str,
                mem.tier.as_str(),
                content_json,
                embedding_blob,
                serde_json::to_string(&mem.temporal).unwrap(),
                serde_json::to_string(&mem.source).unwrap(),
                serde_json::to_string(&mem.salience).unwrap(),
                serialize_privacy(&mem.privacy),
                serialize_tags(&mem.tags),
                serialize_metadata(&mem.metadata),
                serialize_links(&mem.links),
                mem.content_hash,
                mem.namespace,
                mem.salience.effective_score,
                fact_subject,
                fact_object,
                pref_key,
                mem.source.channel,
                mem.source.identity_id.map(|id| id.to_string()),
                now,
            ])
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        // FTS5 synced via trigger (memories_fts_au)
        self.cache_put(mem);
        Ok(())
    }

    fn delete_memory(&self, id: Uuid) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        let id_str = id.to_string();
        let mut stmt = conn
            .prepare_cached("DELETE FROM memories WHERE id = ?1")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        stmt.execute(params![id_str])
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        // FTS5 synced via trigger (memories_fts_ad)
        self.cache_evict(&id);
        Ok(())
    }

    fn list_by_tier(
        &self,
        tier: MemoryTier,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![tier.as_str(), limit as i64], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_by_tier_ordered_by_ingestion(
        &self,
        tier: MemoryTier,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        // Order by the event time (temporal.ingestion_time), not the created_at
        // insertion column — they diverge for imported/synced memories. Parse the
        // RFC3339 timestamp with julianday() rather than sorting the raw string:
        // chrono emits variable fractional precision ("…00Z" vs "…00.500Z") that does
        // not compare chronologically as text.
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 ORDER BY julianday(json_extract(temporal_json, '$.ingestion_time')) DESC LIMIT ?2",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![tier.as_str(), limit as i64], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_by_channel(
        &self,
        channel: &str,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE source_channel = ?1 ORDER BY created_at DESC LIMIT ?2",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![channel, limit as i64], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_by_salience_below(
        &self,
        tier: MemoryTier,
        threshold: f32,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 AND salience_score < ?2",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![tier.as_str(), threshold], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_in_time_range(
        &self,
        tier: MemoryTier,
        start: DateTime<Utc>,
        end: DateTime<Utc>,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 AND created_at >= ?2 AND created_at <= ?3 ORDER BY created_at DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(
                params![tier.as_str(), start.to_rfc3339(), end.to_rfc3339()],
                Self::parse_mem_row,
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn store_link(
        &self,
        source_id: Uuid,
        target_id: Uuid,
        relation: LinkRelation,
        strength: f32,
    ) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        let mut stmt = conn
            .prepare_cached("INSERT OR REPLACE INTO links (source_id, target_id, relation, strength) VALUES (?1, ?2, ?3, ?4)")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        stmt.execute(params![source_id.to_string(), target_id.to_string(), relation.as_str(), strength])
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn get_links(
        &self,
        source_id: Uuid,
    ) -> Result<Vec<(Uuid, LinkRelation, f32)>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT target_id, relation, strength FROM links WHERE source_id = ?1")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![source_id.to_string()], |row| {
                let target_str: String = row.get(0)?;
                let rel_str: String = row.get(1)?;
                let strength: f32 = row.get(2)?;
                Ok((target_str, rel_str, strength))
            })
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            let (target_str, rel_str, strength) =
                row.map_err(|e| CortexError::Storage(e.to_string()))?;
            let target = Uuid::parse_str(&target_str)
                .map_err(|e| CortexError::Storage(e.to_string()))?;
            let relation =
                LinkRelation::parse(&rel_str).unwrap_or(LinkRelation::RelatedTo);
            results.push((target, relation, strength));
        }
        Ok(results)
    }

    // People
    fn store_person(&self, person: &Person) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute(
            "INSERT INTO people (id, display_name, relationship, first_seen, last_seen, interaction_count, tags_json, notes_json, communication_style_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                person.id.to_string(),
                person.display_name,
                person.relationship_to_user,
                person.first_seen.to_rfc3339(),
                person.last_seen.to_rfc3339(),
                person.interaction_count,
                serde_json::to_string(&person.tags).unwrap(),
                serde_json::to_string(&person.notes).unwrap(),
                serde_json::to_string(&person.communication_style).unwrap(),
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;

        for identity in &person.identities {
            conn.execute(
                "INSERT OR REPLACE INTO channel_identities (person_id, channel, channel_user_id, username, display_name) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    person.id.to_string(),
                    identity.channel,
                    identity.channel_user_id,
                    identity.username,
                    identity.display_name,
                ],
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    fn get_person(&self, id: Uuid) -> Result<Option<Person>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, display_name, relationship, first_seen, last_seen, interaction_count, tags_json, notes_json, communication_style_json FROM people WHERE id = ?1",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let person = stmt
            .query_row(
                params![id.to_string()],
                |row| {
                    Ok(parse_person_row(row))
                },
            )
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        match person {
            Some(mut p) => {
                p.identities = self.load_identities(&conn, p.id)?;
                Ok(Some(p))
            }
            None => Ok(None),
        }
    }

    fn find_person_by_channel_identity(
        &self,
        channel: &str,
        channel_user_id: &str,
    ) -> Result<Option<Person>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT p.id, p.display_name, p.relationship, p.first_seen, p.last_seen, p.interaction_count, p.tags_json, p.notes_json, p.communication_style_json
                 FROM people p
                 INNER JOIN channel_identities ci ON ci.person_id = p.id
                 WHERE ci.channel = ?1 AND ci.channel_user_id = ?2",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let person = stmt
            .query_row(
                params![channel, channel_user_id],
                |row| Ok(parse_person_row(row)),
            )
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        match person {
            Some(mut p) => {
                p.identities = self.load_identities(&conn, p.id)?;
                Ok(Some(p))
            }
            None => Ok(None),
        }
    }

    fn update_person(&self, person: &Person) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute(
            "UPDATE people SET display_name=?2, relationship=?3, first_seen=?4, last_seen=?5, interaction_count=?6, tags_json=?7, notes_json=?8, communication_style_json=?9 WHERE id=?1",
            params![
                person.id.to_string(),
                person.display_name,
                person.relationship_to_user,
                person.first_seen.to_rfc3339(),
                person.last_seen.to_rfc3339(),
                person.interaction_count,
                serde_json::to_string(&person.tags).unwrap(),
                serde_json::to_string(&person.notes).unwrap(),
                serde_json::to_string(&person.communication_style).unwrap(),
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;

        // Replace identities
        conn.execute(
            "DELETE FROM channel_identities WHERE person_id = ?1",
            params![person.id.to_string()],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        for identity in &person.identities {
            conn.execute(
                "INSERT INTO channel_identities (person_id, channel, channel_user_id, username, display_name) VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    person.id.to_string(),
                    identity.channel,
                    identity.channel_user_id,
                    identity.username,
                    identity.display_name,
                ],
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        }
        Ok(())
    }

    fn delete_person(&self, id: Uuid) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute("DELETE FROM people WHERE id = ?1", params![id.to_string()])
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn list_people(&self) -> Result<Vec<Person>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT id, display_name, relationship, first_seen, last_seen, interaction_count, tags_json, notes_json, communication_style_json FROM people ORDER BY last_seen DESC")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| Ok(parse_person_row(row)))
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            let mut p = row.map_err(|e| CortexError::Storage(e.to_string()))?;
            p.identities = self.load_identities(&conn, p.id)?;
            results.push(p);
        }
        Ok(results)
    }

    // Beliefs
    fn store_belief(&self, belief: &Belief) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute(
            "INSERT INTO beliefs (id, key, probability, observations_json, last_updated) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                belief.id.to_string(),
                belief.key,
                belief.probability,
                serde_json::to_string(&belief.observations).unwrap(),
                belief.last_updated.to_rfc3339(),
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn get_belief(&self, key: &str) -> Result<Option<Belief>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, key, probability, observations_json, last_updated FROM beliefs WHERE key = ?1",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let result = stmt
            .query_row(params![key], parse_belief_row)
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(result)
    }

    fn update_belief(&self, belief: &Belief) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute(
            "UPDATE beliefs SET probability=?2, observations_json=?3, last_updated=?4 WHERE id=?1",
            params![
                belief.id.to_string(),
                belief.probability,
                serde_json::to_string(&belief.observations).unwrap(),
                belief.last_updated.to_rfc3339(),
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn list_beliefs_above(&self, threshold: f32) -> Result<Vec<Belief>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT id, key, probability, observations_json, last_updated FROM beliefs WHERE probability >= ?1 ORDER BY probability DESC")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![threshold], parse_belief_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_beliefs_confident(&self, threshold: f32) -> Result<Vec<Belief>, CortexError> {
        let conn = self.read_conn()?;
        let low = 1.0 - threshold;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, key, probability, observations_json, last_updated FROM beliefs \
                 WHERE probability >= ?1 OR probability <= ?2 ORDER BY ABS(probability - 0.5) DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![threshold, low], parse_belief_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    // Patterns
    fn store_pattern(&self, pattern: &Pattern) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute(
            "INSERT INTO patterns (id, trigger, actions_json, frequency, last_seen) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                pattern.id.to_string(),
                pattern.trigger,
                serde_json::to_string(&pattern.actions).unwrap(),
                pattern.frequency,
                pattern.last_seen.to_rfc3339(),
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn get_pattern(&self, trigger: &str) -> Result<Option<Pattern>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, trigger, actions_json, frequency, last_seen FROM patterns WHERE trigger = ?1",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let result = stmt
            .query_row(params![trigger], parse_pattern_row)
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(result)
    }

    fn update_pattern(&self, pattern: &Pattern) -> Result<(), CortexError> {
        let conn = self.write_conn.lock();
        conn.execute(
            "UPDATE patterns SET trigger=?2, actions_json=?3, frequency=?4, last_seen=?5 WHERE id=?1",
            params![
                pattern.id.to_string(),
                pattern.trigger,
                serde_json::to_string(&pattern.actions).unwrap(),
                pattern.frequency,
                pattern.last_seen.to_rfc3339(),
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn list_patterns(&self, min_frequency: u32) -> Result<Vec<Pattern>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT id, trigger, actions_json, frequency, last_seen FROM patterns WHERE frequency >= ?1 ORDER BY frequency DESC")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![min_frequency], parse_pattern_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn get_memories_batch(&self, ids: &[Uuid]) -> Result<Vec<MemObject>, CortexError> {
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        // Partition into cache hits and misses
        let mut results = Vec::with_capacity(ids.len());
        let mut miss_ids: Vec<Uuid> = Vec::new();
        {
            let mut cache = self.mem_cache.lock();
            for id in ids {
                if let Some(cached) = cache.get(id) {
                    results.push((**cached).clone());
                } else {
                    miss_ids.push(*id);
                }
            }
        }
        // Fetch misses from SQLite
        if !miss_ids.is_empty() {
            let conn = self.read_conn()?;
            let placeholders: Vec<&str> = miss_ids.iter().map(|_| "?").collect();
            let sql = format!(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE id IN ({})",
                placeholders.join(", ")
            );
            let mut stmt = conn.prepare(&sql).map_err(|e| CortexError::Storage(e.to_string()))?;
            let id_strings: Vec<String> = miss_ids.iter().map(|id| id.to_string()).collect();
            let params: Vec<&dyn rusqlite::types::ToSql> = id_strings.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
            let rows = stmt
                .query_map(params.as_slice(), Self::parse_mem_row)
                .map_err(|e| CortexError::Storage(e.to_string()))?;
            let mut cache = self.mem_cache.lock();
            for row in rows {
                let mem = row.map_err(|e| CortexError::Storage(e.to_string()))?;
                cache.put(mem.id, Arc::new(mem.clone()));
                results.push(mem);
            }
        }
        Ok(results)
    }

    fn query_facts_by_entity(&self, entity: &str) -> Result<Vec<MemObject>, CortexError> {
        let cache_key = entity.to_lowercase();
        // Check entity cache first
        {
            let mut cache = self.entity_cache.lock();
            if let Some(cached) = cache.get(&cache_key) {
                return Ok(cached.clone());
            }
        }
        let conn = self.read_conn()?;
        let pattern = format!("%{}%", cache_key);
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, \
                 salience_json, privacy_json, tags_json, metadata_json, links_json, \
                 content_hash, namespace \
                 FROM memories WHERE tier = 'semantic' \
                 AND (fact_subject LIKE ?1 OR fact_object LIKE ?1) \
                 ORDER BY created_at DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![pattern], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        // Populate entity cache
        self.entity_cache.lock().put(cache_key, results.clone());
        Ok(results)
    }

    fn query_facts_by_entities(&self, entities: &[String]) -> Result<Vec<MemObject>, CortexError> {
        if entities.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_conn()?;
        // Build OR conditions using materialized columns
        let conditions: Vec<String> = entities
            .iter()
            .enumerate()
            .map(|(i, _)| {
                let p = i + 1;
                format!("(fact_subject LIKE ?{p} OR fact_object LIKE ?{p})")
            })
            .collect();
        let sql = format!(
            "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, \
             salience_json, privacy_json, tags_json, metadata_json, links_json, \
             content_hash, namespace \
             FROM memories WHERE tier = 'semantic' AND ({}) \
             ORDER BY created_at DESC",
            conditions.join(" OR ")
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(entities.len());
        for entity in entities {
            params_vec.push(Box::new(format!("%{}%", entity.to_lowercase())));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn query_preferences_by_key(&self, key_pattern: &str) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let pattern = format!("%{}%", key_pattern.to_lowercase());
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, \
                 salience_json, privacy_json, tags_json, metadata_json, links_json, \
                 content_hash, namespace \
                 FROM memories WHERE tier = 'semantic' \
                 AND pref_key LIKE ?1 \
                 ORDER BY created_at DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![pattern], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn list_namespaces(&self) -> Result<Vec<(String, usize)>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT COALESCE(namespace, '(default)'), COUNT(*) FROM memories GROUP BY namespace ORDER BY COUNT(*) DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map([], |row| {
                let ns: String = row.get(0)?;
                let count: i64 = row.get(1)?;
                Ok((ns, count as usize))
            })
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn count_by_tier(&self, tier: MemoryTier) -> Result<usize, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached("SELECT COUNT(*) FROM memories WHERE tier = ?1")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let count: i64 = stmt
            .query_row(params![tier.as_str()], |row| row.get(0))
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(count as usize)
    }

    fn store_memories_batch(&self, mems: &[MemObject]) -> Result<usize, CortexError> {
        if mems.is_empty() {
            return Ok(0);
        }
        let conn = self.write_conn.lock();
        let now = Utc::now().to_rfc3339();
        conn.execute_batch("BEGIN TRANSACTION;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        let mut count = 0;
        for mem in mems {
            let embedding_blob = mem.embedding.as_ref().map(|e| f32_vec_to_bytes(e));
            let content_json = serde_json::to_string(&mem.content).unwrap();
            let id_str = mem.id.to_string();
            let (fact_subject, fact_object, pref_key) = extract_materialized_columns(&mem.content);
            let result = conn.execute(
                "INSERT INTO memories (id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace, salience_score, fact_subject, fact_object, pref_key, source_channel, source_identity_id, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21)",
                params![
                    id_str,
                    mem.tier.as_str(),
                    content_json,
                    embedding_blob,
                    serde_json::to_string(&mem.temporal).unwrap(),
                    serde_json::to_string(&mem.source).unwrap(),
                    serde_json::to_string(&mem.salience).unwrap(),
                    serde_json::to_string(&mem.privacy).unwrap(),
                    serde_json::to_string(&mem.tags).unwrap(),
                    serde_json::to_string(&mem.metadata).unwrap(),
                    serde_json::to_string(&mem.links).unwrap(),
                    mem.content_hash,
                    mem.namespace,
                    mem.salience.effective_score,
                    fact_subject,
                    fact_object,
                    pref_key,
                    mem.source.channel,
                    mem.source.identity_id.map(|id| id.to_string()),
                    now,
                    now,
                ],
            );
            if result.is_ok() {
                // FTS5 synced via trigger (memories_fts_ai)
                count += 1;
            }
        }

        conn.execute_batch("COMMIT;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        // Populate cache for batch-stored memories
        {
            let mut cache = self.mem_cache.lock();
            for mem in mems {
                cache.put(mem.id, Arc::new(mem.clone()));
            }
        }
        Ok(count)
    }

    fn find_by_content_hash(&self, hash: &str) -> Result<Option<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE content_hash = ?1 LIMIT 1",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let result = stmt
            .query_row(params![hash], Self::parse_mem_row)
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(result)
    }

    fn list_by_tier_and_namespace(
        &self,
        tier: MemoryTier,
        namespace: Option<&str>,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut results = Vec::new();
        match namespace {
            Some(ns) => {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 AND namespace = ?2 ORDER BY created_at DESC LIMIT ?3",
                    )
                    .map_err(|e| CortexError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map(params![tier.as_str(), ns, limit as i64], Self::parse_mem_row)
                    .map_err(|e| CortexError::Storage(e.to_string()))?;
                for row in rows {
                    results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
                }
            }
            None => {
                let mut stmt = conn
                    .prepare_cached(
                        "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 ORDER BY created_at DESC LIMIT ?2",
                    )
                    .map_err(|e| CortexError::Storage(e.to_string()))?;
                let rows = stmt
                    .query_map(params![tier.as_str(), limit as i64], Self::parse_mem_row)
                    .map_err(|e| CortexError::Storage(e.to_string()))?;
                for row in rows {
                    results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
                }
            }
        }
        Ok(results)
    }

    fn search_episodic_by_terms(
        &self,
        terms: &[String],
        limit: usize,
    ) -> Result<Vec<(Uuid, String)>, CortexError> {
        if terms.is_empty() {
            return Ok(Vec::new());
        }
        let conn = self.read_conn()?;
        // Build OR-ed LIKE conditions for each term
        let conditions: Vec<String> = terms
            .iter()
            .enumerate()
            .map(|(i, _)| format!("LOWER(content_json) LIKE ?{}", i + 2))
            .collect();
        let sql = format!(
            "SELECT id, content_json FROM memories WHERE tier = ?1 AND ({}) ORDER BY created_at DESC LIMIT {}",
            conditions.join(" OR "),
            limit
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut params_vec: Vec<Box<dyn rusqlite::types::ToSql>> = Vec::with_capacity(terms.len() + 1);
        params_vec.push(Box::new("episodic".to_string()));
        for term in terms {
            params_vec.push(Box::new(format!("%{}%", term.to_lowercase())));
        }
        let param_refs: Vec<&dyn rusqlite::types::ToSql> = params_vec.iter().map(|p| p.as_ref()).collect();
        let rows = stmt
            .query_map(param_refs.as_slice(), |row| {
                let id_str: String = row.get(0)?;
                let content_json: String = row.get(1)?;
                Ok((id_str, content_json))
            })
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        for row in rows {
            let (id_str, content_json) = row.map_err(|e| CortexError::Storage(e.to_string()))?;
            let id = Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4());
            // Extract text from content JSON
            if let Ok(crate::types::MemContent::Text(text)) = serde_json::from_str::<crate::types::MemContent>(&content_json) {
                results.push((id, text));
            }
        }
        Ok(results)
    }

    fn list_by_tier_paged(
        &self,
        tier: MemoryTier,
        offset: usize,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        // Skip embedding_blob (NULL) for lighter memory footprint in bulk processing
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, NULL, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 ORDER BY created_at DESC LIMIT ?2 OFFSET ?3",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![tier.as_str(), limit as i64, offset as i64], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn batch_update_salience(
        &self,
        updates: &[(Uuid, String, f32)],
    ) -> Result<usize, CortexError> {
        if updates.is_empty() {
            return Ok(0);
        }
        let conn = self.write_conn.lock();
        let now = chrono::Utc::now().to_rfc3339();
        conn.execute_batch("BEGIN TRANSACTION;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        let mut count = 0;
        {
            let mut stmt = conn
                .prepare_cached(
                    "UPDATE memories SET salience_json = ?2, salience_score = ?3, updated_at = ?4 WHERE id = ?1",
                )
                .map_err(|e| CortexError::Storage(e.to_string()))?;
            for (id, salience_json, score) in updates {
                stmt.execute(params![id.to_string(), salience_json, score, now])
                    .map_err(|e| CortexError::Storage(e.to_string()))?;
                count += 1;
            }
        }

        conn.execute_batch("COMMIT;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        // Evict updated entries from cache (salience changed)
        {
            let mut cache = self.mem_cache.lock();
            for (id, _, _) in updates {
                cache.pop(id);
            }
        }
        Ok(count)
    }

    fn delete_memories_batch(&self, ids: &[Uuid]) -> Result<usize, CortexError> {
        if ids.is_empty() {
            return Ok(0);
        }
        let conn = self.write_conn.lock();
        conn.execute_batch("BEGIN TRANSACTION;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        let mut count = 0;
        {
            let mut del_stmt = conn
                .prepare_cached("DELETE FROM memories WHERE id = ?1")
                .map_err(|e| CortexError::Storage(e.to_string()))?;
            for id in ids {
                let id_str = id.to_string();
                del_stmt.execute(params![id_str])
                    .map_err(|e| CortexError::Storage(e.to_string()))?;
                count += 1;
            }
        }
        // FTS5 synced via trigger (memories_fts_ad)

        conn.execute_batch("COMMIT;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        // Evict deleted entries from cache
        {
            let mut cache = self.mem_cache.lock();
            for id in ids {
                cache.pop(id);
            }
        }
        Ok(count)
    }

    fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(Uuid, f64)>, CortexError> {
        SqliteStorage::fts_search(self, query, limit)
    }

    fn list_memories_by_source_identity(
        &self,
        identity_id: Uuid,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let id_str = identity_id.to_string();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE source_identity_id = ?1 ORDER BY created_at DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![id_str], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }
}

impl SqliteStorage {
    /// Full-text search using FTS5. Returns (id, BM25 rank) pairs.
    /// Rank is normalized to 0.0–1.0 range.
    pub fn fts_search(&self, query: &str, limit: usize) -> Result<Vec<(Uuid, f64)>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, rank FROM memories_fts WHERE memories_fts MATCH ?1 ORDER BY rank LIMIT ?2",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        // FTS5 match syntax: escape special chars, use implicit AND
        let safe_query: String = query
            .chars()
            .filter(|c| c.is_alphanumeric() || c.is_whitespace() || *c == '*')
            .collect();
        if safe_query.trim().is_empty() {
            return Ok(Vec::new());
        }

        let rows = stmt
            .query_map(params![safe_query, limit as i64], |row| {
                let id_str: String = row.get(0)?;
                let rank: f64 = row.get(1)?;
                Ok((id_str, rank))
            })
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        let mut results = Vec::new();
        let mut min_rank = 0.0_f64;
        let mut max_rank = 0.0_f64;
        let mut raw: Vec<(Uuid, f64)> = Vec::new();

        for row in rows {
            let (id_str, rank) = row.map_err(|e| CortexError::Storage(e.to_string()))?;
            if let Ok(id) = Uuid::parse_str(&id_str) {
                if raw.is_empty() {
                    min_rank = rank;
                    max_rank = rank;
                } else {
                    min_rank = min_rank.min(rank);
                    max_rank = max_rank.max(rank);
                }
                raw.push((id, rank));
            }
        }

        // Normalize ranks to 0.0–1.0 (FTS5 ranks are negative, more negative = better match)
        let range = (max_rank - min_rank).abs();
        for (id, rank) in raw {
            let normalized = if range > 0.0 {
                (max_rank - rank) / range // invert: most negative becomes 1.0
            } else {
                1.0
            };
            results.push((id, normalized));
        }

        Ok(results)
    }

    fn load_identities(
        &self,
        conn: &Connection,
        person_id: Uuid,
    ) -> Result<Vec<ChannelIdentity>, CortexError> {
        let mut stmt = conn
            .prepare_cached("SELECT channel, channel_user_id, username, display_name FROM channel_identities WHERE person_id = ?1")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let rows = stmt
            .query_map(params![person_id.to_string()], |row| {
                Ok(ChannelIdentity {
                    channel: row.get(0)?,
                    channel_user_id: row.get(1)?,
                    username: row.get(2)?,
                    display_name: row.get(3)?,
                })
            })
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }
}

fn parse_person_row(row: &rusqlite::Row) -> Person {
    let id_str: String = row.get(0).unwrap();
    let display_name: String = row.get(1).unwrap();
    let relationship: String = row.get(2).unwrap();
    let first_seen_str: String = row.get(3).unwrap();
    let last_seen_str: String = row.get(4).unwrap();
    let interaction_count: u32 = row.get(5).unwrap();
    let tags_json: String = row.get(6).unwrap();
    let notes_json: String = row.get(7).unwrap();
    let style_json: String = row.get(8).unwrap();

    Person {
        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
        identities: Vec::new(), // loaded separately
        display_name,
        relationship_to_user: relationship,
        first_seen: DateTime::parse_from_rfc3339(&first_seen_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        last_seen: DateTime::parse_from_rfc3339(&last_seen_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
        interaction_count,
        communication_style: serde_json::from_str(&style_json).unwrap_or_default(),
        tags: serde_json::from_str(&tags_json).unwrap_or_default(),
        notes: serde_json::from_str(&notes_json).unwrap_or_default(),
    }
}

fn parse_belief_row(row: &rusqlite::Row) -> Result<Belief, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let key: String = row.get(1)?;
    let probability: f32 = row.get(2)?;
    let obs_json: String = row.get(3)?;
    let updated_str: String = row.get(4)?;

    Ok(Belief {
        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
        key,
        probability,
        observations: serde_json::from_str(&obs_json).unwrap_or_default(),
        last_updated: DateTime::parse_from_rfc3339(&updated_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    })
}

fn parse_pattern_row(row: &rusqlite::Row) -> Result<Pattern, rusqlite::Error> {
    let id_str: String = row.get(0)?;
    let trigger: String = row.get(1)?;
    let actions_json: String = row.get(2)?;
    let frequency: u32 = row.get(3)?;
    let last_seen_str: String = row.get(4)?;

    Ok(Pattern {
        id: Uuid::parse_str(&id_str).unwrap_or_else(|_| Uuid::new_v4()),
        trigger,
        actions: serde_json::from_str(&actions_json).unwrap_or_default(),
        frequency,
        last_seen: DateTime::parse_from_rfc3339(&last_seen_str)
            .map(|d| d.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now()),
    })
}
