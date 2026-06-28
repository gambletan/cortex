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
use base64::Engine;
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use uuid::Uuid;

/// Maximum oplog file size before rotation (1 MB).
const MAX_FILE_SIZE: u64 = 1_048_576;

/// A single sync operation — one line in the JSONL file.
/// Device ID is embedded in `hlc.device_id` — no separate field needed.
/// HMAC protects operation integrity against tampering (computed without the hmac field itself).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncOp {
    pub op_id: Uuid,
    pub hlc: HlcTimestamp,
    pub payload: SyncPayload,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hmac: Option<String>, // base64-encoded HMAC-SHA256 of (op_id, hlc, payload) without hmac field
}

/// Helper: SyncOp without HMAC field (for computing the HMAC).
#[derive(Debug, Serialize, Deserialize)]
struct SyncOpWithoutHmac {
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
    pub fn append(&mut self, op: SyncOp) -> Result<(), CortexError> {
        self.append_buffered(op)?;
        self.flush()
    }

    /// Append without flushing — use with `flush()` after a batch.
    pub fn append_buffered(&mut self, mut op: SyncOp) -> Result<(), CortexError> {
        use zeroize::Zeroize;
        self.rotate_if_needed()?;

        // Compute HMAC if we have crypto context (for integrity protection)
        if let Some(ref ctx) = self.crypto {
            let op_without_hmac = SyncOpWithoutHmac {
                op_id: op.op_id,
                hlc: op.hlc.clone(),
                payload: op.payload.clone(),
            };
            // Serialize without whitespace (canonical form) for deterministic HMAC
            let hmac_json = serde_json::to_string(&op_without_hmac)
                .map_err(|e| CortexError::Serialization(e.to_string()))?;

            // Compute HMAC-SHA256 of the canonical JSON (without the hmac field itself)
            let hmac_bytes = ctx.compute_operation_hmac(hmac_json.as_bytes());
            op.hmac = Some(base64::engine::general_purpose::STANDARD.encode(hmac_bytes));
        }

        let mut json = serde_json::to_string(&op)
            .map_err(|e| CortexError::Serialization(e.to_string()))?;

        let line = if let Some(ref ctx) = self.crypto {
            crate::sync::crypto::encrypt_line(ctx, json.as_bytes())?
        } else {
            json.clone()
        };

        let file = self.current_file.as_mut()
            .ok_or_else(|| CortexError::Storage("No open oplog file".into()))?;
        writeln!(file, "{}", line)
            .map_err(|e| CortexError::Storage(format!("Failed to write oplog: {}", e)))?;

        // Zeroize plaintext JSON from memory
        json.zeroize();

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
    let mut consecutive_hmac_failures = 0;
    const CORRUPTION_THRESHOLD: u32 = 5; // Alert if 5+ consecutive HMAC failures

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
        } else if crypto.is_some() {
            // SECURITY: encryption is enabled, but this line is not an encrypted envelope.
            // The writer always encrypts when a crypto context is present, so a plaintext
            // line here is never legitimate — it is corruption or an injection attempt by
            // someone with write access to the (untrusted) cloud sync directory. Accepting
            // it would bypass BOTH AES-GCM authentication and the operation HMAC (an injected
            // op simply omits the `hmac` field), so it must be rejected, not parsed.
            tracing::error!(
                "Rejecting unencrypted oplog line at offset {} while encryption is enabled — possible injection or downgrade attack",
                offset
            );
            offset += bytes_read as u64;
            continue;
        } else {
            trimmed.to_string()
        };

        let result = serde_json::from_str::<SyncOp>(&json_str);

        match result {
            Ok(op) => {
                // SECURITY: in encryption mode the per-op HMAC is MANDATORY. Its key is derived
                // independently of the AES content key, so it — not the AES-GCM envelope — is
                // what stops an attacker who merely leaked a content key from forging ops. A
                // legitimate writer always attaches an HMAC when crypto is present (see
                // append_buffered), so an encrypted op with `hmac: None` is never genuine and
                // must be rejected; accepting it would silently bypass the forgery defense.
                if crypto.is_some() && op.hmac.is_none() {
                    tracing::error!(
                        "Rejecting encrypted op {} at offset {} with no HMAC while encryption is enabled — possible forgery using a leaked content key",
                        op.op_id, offset
                    );
                    {
                        use zeroize::Zeroize;
                        let mut disposable = json_str;
                        disposable.zeroize();
                    }
                    offset += bytes_read as u64;
                    continue;
                }

                // Verify HMAC if present (backward compatible: old operations lack HMAC field)
                if let Some(hmac_str) = &op.hmac {
                    if let Some(ctx) = crypto {
                        // Reconstruct the operation without the HMAC field for verification
                        let op_without_hmac = SyncOpWithoutHmac {
                            op_id: op.op_id,
                            hlc: op.hlc.clone(),
                            payload: op.payload.clone(),
                        };
                        // Re-serialize in canonical form (same as write-time) for verification
                        if let Ok(hmac_json) = serde_json::to_string(&op_without_hmac) {
                            match ctx.verify_operation_hmac(hmac_json.as_bytes(), hmac_str) {
                                Ok(valid) => {
                                    if !valid {
                                        consecutive_hmac_failures += 1;
                                        tracing::error!("HMAC verification FAILED for operation {} at offset {} (failure #{}) — possible tampering or large-scale corruption", op.op_id, offset, consecutive_hmac_failures);

                                        // Check for sustained corruption pattern
                                        if consecutive_hmac_failures >= CORRUPTION_THRESHOLD {
                                            return Err(CortexError::Storage(format!(
                                                "OPLOG corruption detected: {} consecutive HMAC failures from offset {} onwards. Data integrity compromised. Consider restore from backup.",
                                                consecutive_hmac_failures, offset
                                            )));
                                        }

                                        // Zeroize plaintext JSON and skip this corrupted operation
                                        {
                                            use zeroize::Zeroize;
                                            let mut disposable = json_str;
                                            disposable.zeroize();
                                        }
                                        offset += bytes_read as u64;
                                        continue;  // Skip this operation, do not add to ops
                                    } else {
                                        consecutive_hmac_failures = 0;  // Reset on successful verification
                                    }
                                }
                                Err(e) => {
                                    consecutive_hmac_failures += 1;
                                    tracing::error!("HMAC verification error at offset {} (failure #{}) — unable to verify integrity: {}", offset, consecutive_hmac_failures, e);

                                    // Check for sustained corruption pattern
                                    if consecutive_hmac_failures >= CORRUPTION_THRESHOLD {
                                        return Err(CortexError::Storage(format!(
                                            "OPLOG corruption detected: {} consecutive verification errors from offset {} onwards. Data integrity compromised. Consider restore from backup.",
                                            consecutive_hmac_failures, offset
                                        )));
                                    }

                                    // Zeroize plaintext JSON and skip this operation
                                    {
                                        use zeroize::Zeroize;
                                        let mut disposable = json_str;
                                        disposable.zeroize();
                                    }
                                    offset += bytes_read as u64;
                                    continue;  // Skip this operation, do not add to ops
                                }
                            }
                        }
                    } else {
                        tracing::warn!("Encrypted operation with HMAC but no crypto context at offset {} — skipping", offset);
                        // Zeroize plaintext JSON and skip
                        {
                            use zeroize::Zeroize;
                            let mut disposable = json_str;
                            disposable.zeroize();
                        }
                        offset += bytes_read as u64;
                        continue;
                    }
                }

                // Zeroize plaintext JSON from memory
                {
                    use zeroize::Zeroize;
                    let mut disposable = json_str;
                    disposable.zeroize();
                }

                ops.push(op);
                offset += bytes_read as u64;
            }
            Err(_) => {
                // Distinguish partial write (incomplete line) from permanent corruption:
                // - If the raw line ends with '\n', it's a complete but corrupt line → skip it
                // - If at EOF without '\n', it's likely a partial write → don't advance, retry
                // Always advance past bad lines to avoid stalling sync.
                // Partial writes (no trailing \n) are also skipped — the writer
                // uses flush() which guarantees complete lines on disk.
                tracing::warn!("Skipping invalid oplog line at offset {}", offset);

                // Zeroize plaintext JSON from memory
                {
                    use zeroize::Zeroize;
                    let mut disposable = json_str;
                    disposable.zeroize();
                }

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
            hmac: None,}
    }

    /// Fast crypto context for tests (low KDF cost).
    fn test_crypto() -> crate::sync::crypto::CryptoContext {
        let mut m = crate::sync::crypto::new_encryption_manifest();
        m.kdf_params.time_cost = 1;
        m.kdf_params.mem_cost = 1024;
        crate::sync::crypto::derive_key("test-pass", &m).unwrap()
    }

    /// SECURITY: when encryption is enabled, an attacker with write access to the
    /// (untrusted) cloud sync directory must not be able to inject operations by
    /// writing an unencrypted, un-HMAC'd plaintext JSON line. Such lines bypass both
    /// AES-GCM authentication and the operation HMAC, so the reader must reject them.
    #[test]
    fn test_plaintext_injection_rejected_when_encryption_enabled() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("oplog-2026-01-01-001.jsonl");

        // Attacker forges a plaintext op (no encryption, no HMAC) and drops it into the dir.
        let forged = make_op("victim-device", 9999);
        let forged_id = forged.op_id;
        let line = serde_json::to_string(&forged).unwrap();
        assert!(!crate::sync::crypto::is_encrypted_line(&line));
        std::fs::write(&path, format!("{}\n", line)).unwrap();

        // The reader is operating in encryption mode (crypto context present).
        let ctx = test_crypto();
        let (ops, _offset) = read_oplog(&path, 0, Some(&ctx)).unwrap();

        assert!(
            ops.iter().all(|o| o.op_id != forged_id),
            "forged plaintext op must NOT be accepted when encryption is enabled"
        );
        assert!(ops.is_empty(), "no plaintext op may be replayed in encryption mode");
    }

    /// SECURITY: the per-operation HMAC is keyed independently of the AES content key, so it
    /// is the defense that stops an attacker who has merely leaked a content key from FORGING
    /// operations. An encrypted op carrying `hmac: None` therefore bypasses that defense: the
    /// AES-GCM envelope only proves the writer had the content key. A legitimate writer ALWAYS
    /// attaches an HMAC when encryption is on, so an encrypted op with no HMAC is never genuine
    /// — the reader must reject it instead of accepting it unverified.
    #[test]
    fn test_encrypted_op_without_hmac_rejected_in_encryption_mode() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("oplog-2026-01-01-001.jsonl");

        let ctx = test_crypto();

        // Attacker has the content key and produces a *valid* ENC1 envelope, but omits the
        // per-op HMAC (they lack the separate HMAC key). hmac defaults to None here.
        let forged = make_op("victim-device", 9999);
        let forged_id = forged.op_id;
        let plaintext = serde_json::to_string(&forged).unwrap();
        assert!(forged.hmac.is_none(), "this op intentionally has no HMAC");
        let enc_line = crate::sync::crypto::encrypt_line(&ctx, plaintext.as_bytes()).unwrap();
        assert!(crate::sync::crypto::is_encrypted_line(&enc_line));
        std::fs::write(&path, format!("{}\n", enc_line)).unwrap();

        let (ops, _offset) = read_oplog(&path, 0, Some(&ctx)).unwrap();
        assert!(
            ops.iter().all(|o| o.op_id != forged_id),
            "encrypted op with no HMAC must NOT be accepted in encryption mode"
        );
        assert!(ops.is_empty(), "no unauthenticated op may be replayed in encryption mode");
    }

    /// Legitimately encrypted lines must still be read back correctly in encryption mode.
    #[test]
    fn test_encrypted_lines_still_read_in_encryption_mode() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("device-a");
        let ctx = std::sync::Arc::new(test_crypto());

        let mut writer = OpLogWriter::new(device_dir.clone(), Some(ctx.clone())).unwrap();
        let op = make_op("device-a", 1000);
        let op_id = op.op_id;
        writer.append(op).unwrap();

        let files = list_oplog_files(&device_dir).unwrap();
        let (ops, _) = read_oplog(&files[0], 0, Some(&ctx)).unwrap();
        assert_eq!(ops.len(), 1);
        assert_eq!(ops[0].op_id, op_id);
    }

    #[test]
    fn test_write_and_read_roundtrip() {
        let tmp = TempDir::new().unwrap();
        let device_dir = tmp.path().join("device-a");

        let mut writer = OpLogWriter::new(device_dir.clone(), None).unwrap();
        let op1 = make_op("device-a", 1000);
        let op2 = make_op("device-a", 2000);
        let op1_id = op1.op_id;
        let op2_id = op2.op_id;
        writer.append(op1).unwrap();
        writer.append(op2).unwrap();

        let files = list_oplog_files(&device_dir).unwrap();
        assert_eq!(files.len(), 1);

        let (ops, offset) = read_oplog(&files[0], 0, None).unwrap();
        assert_eq!(ops.len(), 2);
        assert_eq!(ops[0].op_id, op1_id);
        assert_eq!(ops[1].op_id, op2_id);
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
        writer.append(op1).unwrap();

        let files = list_oplog_files(&device_dir).unwrap();
        let (ops, offset1) = read_oplog(&files[0], 0, None).unwrap();
        assert_eq!(ops.len(), 1);

        // Write more ops
        let op2 = make_op("device-a", 2000);
        let op2_id = op2.op_id;
        writer.append(op2).unwrap();

        // Read from previous offset — should only get op2
        let (ops2, _) = read_oplog(&files[0], offset1, None).unwrap();
        assert_eq!(ops2.len(), 1);
        assert_eq!(ops2[0].op_id, op2_id);
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
            hmac: None,};

        let json = serde_json::to_string(&op).unwrap();
        let parsed: SyncOp = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.op_id, op.op_id);
    }
}
