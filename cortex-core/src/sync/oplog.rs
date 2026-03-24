//! Operation log — append-only JSONL files for sync.
//!
//! Each device writes to its own oplog files. Other devices read and replay them.
//! Files are rotated when they exceed `MAX_FILE_SIZE` or at date boundaries.

use crate::belief::Belief;
use crate::people::Person;
use crate::procedural::Pattern;
use crate::sync::hlc::HlcTimestamp;
use crate::types::*;
use crate::CortexError;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Maximum oplog file size before rotation (1 MB).
const MAX_FILE_SIZE: u64 = 1_048_576;

/// A single sync operation — one line in the JSONL file.
/// Device ID is embedded in `hlc.device_id` — no separate field needed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOp {
    pub op_id: Uuid,
    pub hlc: HlcTimestamp,
    pub payload: SyncPayload,
}

/// The actual operation payload.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
#[allow(clippy::large_enum_variant)]
pub enum SyncPayload {
    MemoryUpsert { memory: MemObject },
    MemoryDelete { id: Uuid },
    PersonUpsert { person: Person },
    PersonDelete { id: Uuid },
    BeliefUpsert { belief: Belief },
    PatternUpsert { pattern: Pattern },
    LinkUpsert {
        source_id: Uuid,
        target_id: Uuid,
        relation: crate::types::LinkRelation,
        strength: f32,
    },
}

/// Append-only JSONL writer for a device's oplog.
pub struct OpLogWriter {
    device_dir: PathBuf,
    current_file: Option<File>,
    current_path: PathBuf,
    current_date: String,
    current_seq: u32,
    crypto: Option<std::sync::Arc<crate::sync::crypto::CryptoContext>>,
}

impl OpLogWriter {
    pub fn new(
        device_dir: PathBuf,
        crypto: Option<std::sync::Arc<crate::sync::crypto::CryptoContext>>,
    ) -> Result<Self, CortexError> {
        fs::create_dir_all(&device_dir)
            .map_err(|e| CortexError::Storage(format!("Failed to create device dir: {}", e)))?;

        let mut writer = Self {
            device_dir,
            current_file: None,
            current_path: PathBuf::new(),
            current_date: String::new(),
            current_seq: 0,
            crypto,
        };
        writer.rotate_if_needed()?;
        Ok(writer)
    }

    /// Append a SyncOp to the current oplog file and flush.
    pub fn append(&mut self, op: &SyncOp) -> Result<(), CortexError> {
        self.append_buffered(op)?;
        self.flush()
    }

    /// Append without flushing — use with `flush()` after a batch.
    pub fn append_buffered(&mut self, op: &SyncOp) -> Result<(), CortexError> {
        self.rotate_if_needed()?;

        let json = serde_json::to_string(op)
            .map_err(|e| CortexError::Serialization(e.to_string()))?;

        let line = if let Some(ref ctx) = self.crypto {
            crate::sync::crypto::encrypt_line(ctx, json.as_bytes())?
        } else {
            json
        };

        let file = self.current_file.as_mut()
            .ok_or_else(|| CortexError::Storage("No open oplog file".into()))?;
        writeln!(file, "{}", line)
            .map_err(|e| CortexError::Storage(format!("Failed to write oplog: {}", e)))?;

        Ok(())
    }

    /// Flush the oplog file to disk.
    pub fn flush(&mut self) -> Result<(), CortexError> {
        if let Some(ref mut file) = self.current_file {
            file.flush()
                .map_err(|e| CortexError::Storage(format!("Failed to flush oplog: {}", e)))?;
        }
        Ok(())
    }

    fn rotate_if_needed(&mut self) -> Result<(), CortexError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();

        // Check if we need a new file (date change or size limit)
        let needs_new = if self.current_file.is_none() || today != self.current_date {
            true
        } else {
            let size = fs::metadata(&self.current_path).map(|m| m.len()).unwrap_or(0);
            size >= MAX_FILE_SIZE
        };

        if needs_new {
            if today != self.current_date {
                self.current_date = today.clone();
                self.current_seq = 1;
            } else {
                self.current_seq += 1;
            }

            // Find next available sequence number
            loop {
                let name = format!("oplog-{}-{:03}.jsonl", self.current_date, self.current_seq);
                let path = self.device_dir.join(&name);
                if !path.exists() || fs::metadata(&path).map(|m| m.len()).unwrap_or(0) < MAX_FILE_SIZE {
                    self.current_path = path;
                    break;
                }
                self.current_seq += 1;
            }

            let file = OpenOptions::new()
                .create(true)
                .append(true)
                .open(&self.current_path)
                .map_err(|e| CortexError::Storage(format!("Failed to open oplog: {}", e)))?;
            self.current_file = Some(file);
        }

        Ok(())
    }
}

/// Read operations from an oplog file, starting from a byte offset.
/// Returns (operations, new_byte_offset).
pub fn read_oplog(
    path: &Path,
    start_offset: u64,
    crypto: Option<&crate::sync::crypto::CryptoContext>,
) -> Result<(Vec<SyncOp>, u64), CortexError> {
    let file = File::open(path)
        .map_err(|e| CortexError::Storage(format!("Failed to open oplog {}: {}", path.display(), e)))?;

    let mut reader = BufReader::new(file);
    // Seek to start offset
    if start_offset > 0 {
        std::io::Seek::seek(&mut reader, std::io::SeekFrom::Start(start_offset))
            .map_err(|e| CortexError::Storage(format!("Failed to seek oplog: {}", e)))?;
    }

    let mut ops = Vec::new();
    let mut offset = start_offset;

    loop {
        let mut line = String::new();
        let bytes_read = reader.read_line(&mut line)
            .map_err(|e| CortexError::Storage(format!("Failed to read oplog line: {}", e)))?;

        if bytes_read == 0 {
            break; // EOF
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            offset += bytes_read as u64;
            continue;
        }

        // Decrypt if needed, then deserialize
        let json_str = if crate::sync::crypto::is_encrypted_line(trimmed) {
            match crypto {
                Some(ctx) => {
                    match crate::sync::crypto::decrypt_line(ctx, trimmed) {
                        Ok(bytes) => match String::from_utf8(bytes) {
                            Ok(s) => s,
                            Err(_) => {
                                tracing::warn!("Invalid UTF-8 after decryption at offset {}", offset);
                                offset += bytes_read as u64;
                                continue;
                            }
                        },
                        Err(_) => {
                            tracing::warn!("Decryption failed at offset {}", offset);
                            offset += bytes_read as u64;
                            continue;
                        }
                    }
                }
                None => {
                    tracing::warn!("Encrypted line but no decryption key at offset {}", offset);
                    offset += bytes_read as u64;
                    continue;
                }
            }
        } else {
            trimmed.to_string()
        };

        match serde_json::from_str::<SyncOp>(&json_str) {
            Ok(op) => {
                ops.push(op);
                offset += bytes_read as u64;
            }
            Err(_) => {
                tracing::warn!("Skipping invalid oplog line at offset {}", offset);
                offset += bytes_read as u64;
            }
        }
    }

    Ok((ops, offset))
}

/// List all oplog files for a given device directory, sorted by name (chronological).
pub fn list_oplog_files(device_dir: &Path) -> Result<Vec<PathBuf>, CortexError> {
    let mut files = Vec::new();
    if !device_dir.exists() {
        return Ok(files);
    }

    let entries = fs::read_dir(device_dir)
        .map_err(|e| CortexError::Storage(format!("Failed to read device dir: {}", e)))?;

    for entry in entries {
        let entry = entry
            .map_err(|e| CortexError::Storage(format!("Failed to read dir entry: {}", e)))?;
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("oplog-") && name.ends_with(".jsonl") {
            files.push(entry.path());
        }
    }

    files.sort();
    Ok(files)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn make_op(device_id: &str, wall_ms: u64) -> SyncOp {
        SyncOp {
            op_id: Uuid::new_v4(),
            hlc: HlcTimestamp::new(wall_ms, 0, device_id),
            payload: SyncPayload::MemoryDelete { id: Uuid::new_v4() },
        }
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("device-a");

        let mut writer = OpLogWriter::new(device_dir.clone(), None).unwrap();
        let op1 = make_op("device-a", 1000);
        let op2 = make_op("device-a", 2000);
        writer.append(&op1).unwrap();
        writer.append(&op2).unwrap();

        let files = list_oplog_files(&device_dir).unwrap();
        assert_eq!(files.len(), 1);

        let (ops, offset) = read_oplog(&files[0], 0, None).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].op_id, op1.op_id);
        assert_eq!(ops[1].op_id, op2.op_id);
        assert!(offset > 0);

        // Reading from the end offset should return nothing
        let (ops2, _) = read_oplog(&files[0], offset, None).unwrap();
        assert!(ops2.is_empty());
    }

    #[test]
    fn test_incremental_read() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("device-a");

        let mut writer = OpLogWriter::new(device_dir.clone(), None).unwrap();
        let op1 = make_op("device-a", 1000);
        writer.append(&op1).unwrap();

        let files = list_oplog_files(&device_dir).unwrap();
        let (ops, offset1) = read_oplog(&files[0], 0, None).unwrap();
        assert_eq!(ops.len(), 1);

        // Write more ops
        let op2 = make_op("device-a", 2000);
        writer.append(&op2).unwrap();

        // Read from previous offset — should only get op2
        let (ops2, _) = read_oplog(&files[0], offset1, None).unwrap();
        assert_eq!(ops2.len(), 1);
        assert_eq!(ops2[0].op_id, op2.op_id);
    }

    #[test]
    fn test_serde_memory_upsert() {
        let mem = MemObject {
            id: Uuid::new_v4(),
            tier: MemoryTier::Episodic,
            content: MemContent::Text("hello world".into()),
            embedding: None,
            temporal: TemporalInfo::now(),
            source: MemSource::new("test"),
            salience: Salience::default(),
            privacy: PrivacyLevel::Private,
            links: Vec::new(),
            tags: Vec::new(),
            metadata: std::collections::HashMap::new(),
            content_hash: None,
            namespace: None,
        };

        let op = SyncOp {
            op_id: Uuid::new_v4(),
            hlc: HlcTimestamp::new(1000, 0, "device-a"),
            payload: SyncPayload::MemoryUpsert { memory: mem },
        };

        let json = serde_json::to_string(&op).unwrap();
        let parsed: SyncOp = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op_id, op.op_id);
    }
}
