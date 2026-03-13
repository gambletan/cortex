use crate::belief::Belief;
use crate::people::{ChannelIdentity, Person};
use crate::procedural::Pattern;
use crate::storage::traits::StorageBackend;
use crate::types::*;
use crate::CortexError;
use chrono::{DateTime, Utc};
use rusqlite::{params, Connection, OptionalExtension};
use std::sync::Mutex;
use uuid::Uuid;

/// Number of read-only connections in the pool.
const READ_POOL_SIZE: usize = 4;

/// SQLite-backed storage for Cortex. Local-first, zero-config.
/// Uses WAL mode with separate read/write connections for concurrency.
pub struct SqliteStorage {
    write_conn: Mutex<Connection>,
    read_pool: Vec<Mutex<Connection>>,
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
        let conn = Connection::open(path).map_err(|e| CortexError::Storage(e.to_string()))?;
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
            Self::apply_read_pragmas(&reader)?;
            read_pool.push(Mutex::new(reader));
        }

        let storage = Self {
            write_conn: Mutex::new(conn),
            read_pool,
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
        };
        storage.init()?;
        Ok(storage)
    }

    /// Get a read connection. Tries each reader first, falls back to write conn.
    fn read_conn(&self) -> Result<std::sync::MutexGuard<'_, Connection>, CortexError> {
        for reader in &self.read_pool {
            if let Ok(guard) = reader.try_lock() {
                return Ok(guard);
            }
        }
        // All readers busy or no readers (in-memory mode): use write conn
        if let Some(reader) = self.read_pool.first() {
            return reader.lock().map_err(|e| CortexError::Storage(e.to_string()));
        }
        self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))
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
        let embedding = embedding_blob.map(|b| bytes_to_f32_vec(&b));
        let temporal: TemporalInfo = serde_json::from_str(&temporal_json).unwrap();
        let source: MemSource = serde_json::from_str(&source_json).unwrap();
        let salience: Salience = serde_json::from_str(&salience_json).unwrap();
        let privacy: PrivacyLevel = serde_json::from_str(&privacy_json).unwrap();
        let tags: Vec<String> = serde_json::from_str(&tags_json).unwrap();
        let metadata: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&metadata_json).unwrap();
        let links: Vec<MemLink> = serde_json::from_str(&links_json).unwrap();

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

fn f32_vec_to_bytes(v: &[f32]) -> Vec<u8> {
    let mut buf = Vec::with_capacity(v.len() * 4);
    for f in v {
        buf.extend_from_slice(&f.to_le_bytes());
    }
    buf
}

fn bytes_to_f32_vec(b: &[u8]) -> Vec<f32> {
    let mut result = Vec::with_capacity(b.len() / 4);
    for chunk in b.chunks_exact(4) {
        result.push(f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]));
    }
    result
}

impl StorageBackend for SqliteStorage {
    fn init(&self) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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

        Ok(())
    }

    fn store_memory(&self, mem: &MemObject) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let embedding_blob = mem.embedding.as_ref().map(|e| f32_vec_to_bytes(e));
        conn.execute(
            "INSERT INTO memories (id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace, salience_score, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
            params![
                mem.id.to_string(),
                mem.tier.as_str(),
                serde_json::to_string(&mem.content).unwrap(),
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
                now,
                now,
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn get_memory(&self, id: Uuid) -> Result<Option<MemObject>, CortexError> {
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
        Ok(result)
    }

    fn update_memory(&self, mem: &MemObject) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        let embedding_blob = mem.embedding.as_ref().map(|e| f32_vec_to_bytes(e));
        conn.execute(
            "UPDATE memories SET tier=?2, content_json=?3, embedding_blob=?4, temporal_json=?5, source_json=?6, salience_json=?7, privacy_json=?8, tags_json=?9, metadata_json=?10, links_json=?11, content_hash=?12, namespace=?13, salience_score=?14, updated_at=?15 WHERE id=?1",
            params![
                mem.id.to_string(),
                mem.tier.as_str(),
                serde_json::to_string(&mem.content).unwrap(),
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
                now,
            ],
        )
        .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(())
    }

    fn delete_memory(&self, id: Uuid) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
        conn.execute("DELETE FROM memories WHERE id = ?1", params![id.to_string()])
            .map_err(|e| CortexError::Storage(e.to_string()))?;
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
        self.list_by_tier(tier, limit)
    }

    fn list_by_channel(
        &self,
        channel: &str,
        limit: usize,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE json_extract(source_json, '$.channel') = ?1 ORDER BY created_at DESC LIMIT ?2",
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
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
        conn.execute(
            "INSERT OR REPLACE INTO links (source_id, target_id, relation, strength) VALUES (?1, ?2, ?3, ?4)",
            params![source_id.to_string(), target_id.to_string(), relation.as_str(), strength],
        )
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
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let person = conn
            .query_row(
                "SELECT id, display_name, relationship, first_seen, last_seen, interaction_count, tags_json, notes_json, communication_style_json FROM people WHERE id = ?1",
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
        let person = conn
            .query_row(
                "SELECT p.id, p.display_name, p.relationship, p.first_seen, p.last_seen, p.interaction_count, p.tags_json, p.notes_json, p.communication_style_json
                 FROM people p
                 INNER JOIN channel_identities ci ON ci.person_id = p.id
                 WHERE ci.channel = ?1 AND ci.channel_user_id = ?2",
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
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let result = conn
            .query_row(
                "SELECT id, key, probability, observations_json, last_updated FROM beliefs WHERE key = ?1",
                params![key],
                parse_belief_row,
            )
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(result)
    }

    fn update_belief(&self, belief: &Belief) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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

    // Patterns
    fn store_pattern(&self, pattern: &Pattern) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let result = conn
            .query_row(
                "SELECT id, trigger, actions_json, frequency, last_seen FROM patterns WHERE trigger = ?1",
                params![trigger],
                parse_pattern_row,
            )
            .optional()
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        Ok(result)
    }

    fn update_pattern(&self, pattern: &Pattern) -> Result<(), CortexError> {
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let conn = self.read_conn()?;
        let placeholders: Vec<&str> = ids.iter().map(|_| "?").collect();
        let sql = format!(
            "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE id IN ({})",
            placeholders.join(", ")
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| CortexError::Storage(e.to_string()))?;
        let id_strings: Vec<String> = ids.iter().map(|id| id.to_string()).collect();
        let params: Vec<&dyn rusqlite::types::ToSql> = id_strings.iter().map(|s| s as &dyn rusqlite::types::ToSql).collect();
        let rows = stmt
            .query_map(params.as_slice(), Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::with_capacity(ids.len());
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }

    fn query_facts_by_entity(&self, entity: &str) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let pattern = format!("%{}%", entity.to_lowercase());
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, \
                 salience_json, privacy_json, tags_json, metadata_json, links_json, \
                 content_hash, namespace \
                 FROM memories WHERE tier = 'semantic' \
                 AND (LOWER(json_extract(content_json, '$.Fact.subject')) LIKE ?1 \
                   OR LOWER(json_extract(content_json, '$.Fact.object')) LIKE ?1) \
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

    fn query_preferences_by_key(&self, key_pattern: &str) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let pattern = format!("%{}%", key_pattern.to_lowercase());
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, \
                 salience_json, privacy_json, tags_json, metadata_json, links_json, \
                 content_hash, namespace \
                 FROM memories WHERE tier = 'semantic' \
                 AND LOWER(json_extract(content_json, '$.Preference.key')) LIKE ?1 \
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
        let conn = self.write_conn.lock().map_err(|e| CortexError::Storage(e.to_string()))?;
        let now = Utc::now().to_rfc3339();
        conn.execute_batch("BEGIN TRANSACTION;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;

        let mut count = 0;
        for mem in mems {
            let embedding_blob = mem.embedding.as_ref().map(|e| f32_vec_to_bytes(e));
            let result = conn.execute(
                "INSERT INTO memories (id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace, salience_score, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)",
                params![
                    mem.id.to_string(),
                    mem.tier.as_str(),
                    serde_json::to_string(&mem.content).unwrap(),
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
                    now,
                    now,
                ],
            );
            if result.is_ok() {
                count += 1;
            }
        }

        conn.execute_batch("COMMIT;")
            .map_err(|e| CortexError::Storage(e.to_string()))?;
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
        let (sql, params_vec): (String, Vec<Box<dyn rusqlite::types::ToSql>>) = match namespace {
            Some(ns) => (
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 AND namespace = ?2 ORDER BY created_at DESC LIMIT ?3".to_string(),
                vec![
                    Box::new(tier.as_str().to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(ns.to_string()),
                    Box::new(limit as i64),
                ],
            ),
            None => (
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE tier = ?1 ORDER BY created_at DESC LIMIT ?2".to_string(),
                vec![
                    Box::new(tier.as_str().to_string()) as Box<dyn rusqlite::types::ToSql>,
                    Box::new(limit as i64),
                ],
            ),
        };
        let mut stmt = conn.prepare(&sql).map_err(|e| CortexError::Storage(e.to_string()))?;
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

    fn list_memories_by_source_identity(
        &self,
        identity_id: Uuid,
    ) -> Result<Vec<MemObject>, CortexError> {
        let conn = self.read_conn()?;
        let id_str = identity_id.to_string();
        let mut stmt = conn
            .prepare_cached(
                "SELECT id, tier, content_json, embedding_blob, temporal_json, source_json, salience_json, privacy_json, tags_json, metadata_json, links_json, content_hash, namespace FROM memories WHERE json_extract(source_json, '$.identity_id') = ?1 OR json_extract(source_json, '$.identity_id') = ?2 ORDER BY created_at DESC",
            )
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let quoted_id_str = format!("\"{}\"", id_str);
        let rows = stmt
            .query_map(params![id_str, quoted_id_str], Self::parse_mem_row)
            .map_err(|e| CortexError::Storage(e.to_string()))?;
        let mut results = Vec::new();
        for row in rows {
            results.push(row.map_err(|e| CortexError::Storage(e.to_string()))?);
        }
        Ok(results)
    }
}

impl SqliteStorage {
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
