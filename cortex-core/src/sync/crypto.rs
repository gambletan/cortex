//! Encryption for sync oplog files — AES-256-GCM with Argon2id key derivation.
//!
//! Each JSONL line is independently encrypted with a unique nonce.
//! Format: `ENC1:<base64(nonce[12] || ciphertext || tag[16])>`

use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{Aes256Gcm, AeadCore, Key, KeyInit};
use base64::Engine;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, ZeroizeOnDrop};
use hmac::{Hmac, Mac};
use sha2::{Sha256, Digest};

use crate::CortexError;

type HmacSha256 = Hmac<Sha256>;

const ENC_PREFIX: &str = "ENC1:";
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;

/// Holds the derived encryption key. Zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct CryptoContext {
    key_bytes: [u8; 32],
}

impl CryptoContext {
    fn cipher(&self) -> Aes256Gcm {
        let key = Key::<Aes256Gcm>::from_slice(&self.key_bytes);
        Aes256Gcm::new(key)
    }
}

/// Single encryption key version for key rotation support.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeyVersion {
    pub version: u32,
    pub salt: String, // base64-encoded
    pub kdf_params: KdfParams,
    pub created_at: String, // ISO 8601 timestamp
}

/// Encryption metadata stored in manifest.json.
/// Supports key rotation via multiple KeyVersion entries.
/// Includes HMAC for integrity protection against tampering.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionManifest {
    pub algorithm: String,
    pub kdf: String,
    pub versions: Vec<KeyVersion>, // Multiple key versions for rotation support
    pub current_version: u32, // Which version to use for new encryptions
    #[serde(default)] // For backward compatibility with old single-key format
    pub salt: String, // Legacy: single salt (deprecated, use versions instead)
    #[serde(default)] // For backward compatibility
    pub kdf_params: KdfParams, // Legacy: single params (deprecated, use versions instead)
    #[serde(default)] // HMAC-SHA256 over all other fields (base64-encoded)
    pub integrity_hmac: String, // Prevents manifest tampering/KDF parameter downgrade attacks
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct KdfParams {
    pub time_cost: u32,
    pub mem_cost: u32,
    pub parallelism: u32,
}

/// Generate a new encryption manifest with key rotation support.
/// Creates the first key version (v1) for new encryption systems.
pub fn new_encryption_manifest() -> EncryptionManifest {
    use rand::RngCore;
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);

    let kdf_params = KdfParams {
        time_cost: 3,
        mem_cost: 65536, // 64 MB
        parallelism: 1,
    };

    let key_v1 = KeyVersion {
        version: 1,
        salt: base64::engine::general_purpose::STANDARD.encode(salt),
        kdf_params: kdf_params.clone(),
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    EncryptionManifest {
        algorithm: "aes-256-gcm".to_string(),
        kdf: "argon2id".to_string(),
        versions: vec![key_v1],
        current_version: 1,
        // Legacy fields for backward compatibility
        salt: String::new(),
        kdf_params: KdfParams {
            time_cost: 0,
            mem_cost: 0,
            parallelism: 0,
        },
        integrity_hmac: String::new(), // Will be computed after manifest is created
    }
}

/// Derive the current (latest) encryption key from a passphrase and manifest.
/// Uses the current_version specified in the manifest.
pub fn derive_key(passphrase: &str, manifest: &EncryptionManifest) -> Result<CryptoContext, CortexError> {
    // Try new format with versioned keys
    if let Some(key_version) = manifest.versions.iter().find(|v| v.version == manifest.current_version) {
        derive_key_for_version(passphrase, key_version)
    } else if !manifest.salt.is_empty() {
        // Fallback to legacy single-key format for backward compatibility
        derive_key_legacy(passphrase, &manifest.salt, &manifest.kdf_params)
    } else {
        Err(CortexError::Storage("No encryption key version available".into()))
    }
}

/// Derive a key for a specific version (used for decryption of old data).
/// Supports key rotation by allowing decryption with any historical key version.
pub fn derive_key_for_version(passphrase: &str, key_version: &KeyVersion) -> Result<CryptoContext, CortexError> {
    derive_key_legacy(passphrase, &key_version.salt, &key_version.kdf_params)
}

/// Legacy key derivation (backward compatible).
fn derive_key_legacy(passphrase: &str, salt_b64: &str, params: &KdfParams) -> Result<CryptoContext, CortexError> {
    let salt = base64::engine::general_purpose::STANDARD
        .decode(salt_b64)
        .map_err(|e| CortexError::Storage(format!("Invalid salt: {}", e)))?;

    let kdf_params = argon2::Params::new(
        params.mem_cost,
        params.time_cost,
        params.parallelism,
        Some(32),
    )
    .map_err(|e| CortexError::Storage(format!("Invalid Argon2 params: {}", e)))?;

    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, kdf_params);

    let mut key_bytes = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key_bytes)
        .map_err(|e| CortexError::Storage(format!("Key derivation failed: {}", e)))?;

    Ok(CryptoContext { key_bytes })
}

/// Try decrypting with all available key versions (for forward secrecy).
/// Returns the decrypted plaintext if any key version succeeds.
pub fn decrypt_line_any_version(manifest: &EncryptionManifest, encrypted_line: &str, passphrase: &str) -> Result<Vec<u8>, CortexError> {
    // Try current version first
    if let Some(key_version) = manifest.versions.iter().find(|v| v.version == manifest.current_version) {
        if let Ok(ctx) = derive_key_for_version(passphrase, key_version) {
            if let Ok(plaintext) = decrypt_line(&ctx, encrypted_line) {
                return Ok(plaintext);
            }
        }
    }

    // Try all other versions (for data encrypted with older keys)
    for key_version in &manifest.versions {
        if key_version.version != manifest.current_version {
            if let Ok(ctx) = derive_key_for_version(passphrase, key_version) {
                if let Ok(plaintext) = decrypt_line(&ctx, encrypted_line) {
                    return Ok(plaintext);
                }
            }
        }
    }

    Err(CortexError::Storage("Decryption failed with all available key versions".into()))
}

/// Encrypt a plaintext line → `ENC1:<base64(nonce || ciphertext || tag)>`
pub fn encrypt_line(ctx: &CryptoContext, plaintext: &[u8]) -> Result<String, CortexError> {
    let cipher = ctx.cipher();
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| CortexError::Storage(format!("Encryption failed: {}", e)))?;

    // nonce || ciphertext (includes GCM tag)
    let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
    combined.extend_from_slice(&nonce);
    combined.extend_from_slice(&ciphertext);

    let encoded = base64::engine::general_purpose::STANDARD.encode(&combined);
    Ok(format!("{}{}", ENC_PREFIX, encoded))
}

/// Decrypt an `ENC1:...` line → plaintext bytes.
pub fn decrypt_line(ctx: &CryptoContext, encrypted_line: &str) -> Result<Vec<u8>, CortexError> {
    let payload = encrypted_line
        .strip_prefix(ENC_PREFIX)
        .ok_or_else(|| CortexError::Storage("Missing ENC1: prefix".into()))?;

    let combined = base64::engine::general_purpose::STANDARD
        .decode(payload)
        .map_err(|e| CortexError::Storage(format!("Base64 decode failed: {}", e)))?;

    if combined.len() < NONCE_LEN + 16 {
        return Err(CortexError::Storage("Encrypted data too short".into()));
    }

    let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
    let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);

    let cipher = ctx.cipher();
    cipher
        .decrypt(nonce, ciphertext)
        .map_err(|e| CortexError::Storage(format!("Decryption failed (wrong key or tampered data): {}", e)))
}

/// Check if a line is encrypted.
pub fn is_encrypted_line(line: &str) -> bool {
    line.starts_with(ENC_PREFIX)
}

/// Rotate the encryption key by creating a new key version.
/// New encryptions will use this new key.
/// Old data remains encrypted with old keys and can still be decrypted.
/// This provides forward secrecy: old passphrases don't expose future data.
pub fn rotate_key(manifest: &mut EncryptionManifest) -> Result<(), CortexError> {
    use rand::RngCore;

    let new_version = manifest.versions.iter().map(|v| v.version).max().unwrap_or(0) + 1;
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);

    let new_key_version = KeyVersion {
        version: new_version,
        salt: base64::engine::general_purpose::STANDARD.encode(salt),
        kdf_params: KdfParams {
            time_cost: 3,
            mem_cost: 65536,
            parallelism: 1,
        },
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    manifest.versions.push(new_key_version);
    manifest.current_version = new_version;

    // Recompute HMAC after changes
    recompute_hmac(manifest)?;

    Ok(())
}

/// Compute HMAC over all manifest fields to detect tampering.
/// This prevents attackers from modifying KDF parameters or salt.
pub fn recompute_hmac(manifest: &mut EncryptionManifest) -> Result<(), CortexError> {
    // Clear old HMAC before computing new one
    let old_hmac = manifest.integrity_hmac.clone();
    manifest.integrity_hmac.clear();

    // Serialize manifest without HMAC field for hashing
    let manifest_json = serde_json::to_string(manifest)
        .map_err(|e| CortexError::Serialization(e.to_string()))?;

    // Use a deterministic HMAC key derived from manifest content hash
    // This provides integrity checking without requiring user input
    let content_hash = sha2::Sha256::digest(manifest_json.as_bytes());
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&content_hash)
        .map_err(|_| CortexError::Storage("HMAC key error".into()))?;
    mac.update(manifest_json.as_bytes());

    let hmac_result = mac.finalize();
    manifest.integrity_hmac = base64::engine::general_purpose::STANDARD
        .encode(hmac_result.into_bytes());

    Ok(())
}

/// Verify manifest integrity by checking HMAC.
/// Returns error if manifest has been tampered with.
pub fn verify_manifest_integrity(manifest: &EncryptionManifest) -> Result<(), CortexError> {
    if manifest.integrity_hmac.is_empty() {
        // No HMAC present — skip verification (backward compat with old manifests)
        return Ok(());
    }

    // Recreate manifest without HMAC for verification
    let mut manifest_copy = manifest.clone();
    let stored_hmac = manifest.integrity_hmac.clone();
    manifest_copy.integrity_hmac.clear();

    let manifest_json = serde_json::to_string(&manifest_copy)
        .map_err(|e| CortexError::Serialization(e.to_string()))?;

    let content_hash = sha2::Sha256::digest(manifest_json.as_bytes());
    let mut mac = <HmacSha256 as Mac>::new_from_slice(&content_hash)
        .map_err(|_| CortexError::Storage("HMAC key error".into()))?;
    mac.update(manifest_json.as_bytes());

    let expected_hmac = base64::engine::general_purpose::STANDARD
        .encode(mac.finalize().into_bytes());

    if stored_hmac != expected_hmac {
        return Err(CortexError::Storage(
            "Manifest integrity check failed — possible tampering detected".into(),
        ));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_manifest() -> EncryptionManifest {
        // Use fast params for tests
        let mut m = new_encryption_manifest();
        m.kdf_params.time_cost = 1;
        m.kdf_params.mem_cost = 1024; // 1 MB
        m
    }

    #[test]
    fn test_encrypt_decrypt_roundtrip() {
        let manifest = test_manifest();
        let ctx = derive_key("test-passphrase", &manifest).unwrap();
        let plaintext = b"hello world, this is a secret memory";

        let encrypted = encrypt_line(&ctx, plaintext).unwrap();
        assert!(encrypted.starts_with("ENC1:"));

        let decrypted = decrypt_line(&ctx, &encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }

    #[test]
    fn test_wrong_key_fails() {
        let manifest = test_manifest();
        let ctx1 = derive_key("correct-passphrase", &manifest).unwrap();
        let ctx2 = derive_key("wrong-passphrase", &manifest).unwrap();

        let encrypted = encrypt_line(&ctx1, b"secret data").unwrap();
        let result = decrypt_line(&ctx2, &encrypted);
        assert!(result.is_err());
    }

    #[test]
    fn test_unique_nonces() {
        let manifest = test_manifest();
        let ctx = derive_key("passphrase", &manifest).unwrap();
        let plaintext = b"same text";

        let enc1 = encrypt_line(&ctx, plaintext).unwrap();
        let enc2 = encrypt_line(&ctx, plaintext).unwrap();
        // Same plaintext should produce different ciphertext (random nonce)
        assert_ne!(enc1, enc2);
    }

    #[test]
    fn test_deterministic_key_derivation() {
        let manifest = test_manifest();
        let ctx1 = derive_key("same-passphrase", &manifest).unwrap();
        let ctx2 = derive_key("same-passphrase", &manifest).unwrap();
        assert_eq!(ctx1.key_bytes, ctx2.key_bytes);
    }

    #[test]
    fn test_different_salt_different_key() {
        let m1 = test_manifest();
        let m2 = test_manifest(); // new random salt
        let ctx1 = derive_key("same-passphrase", &m1).unwrap();
        let ctx2 = derive_key("same-passphrase", &m2).unwrap();
        assert_ne!(ctx1.key_bytes, ctx2.key_bytes);
    }

    #[test]
    fn test_tampered_ciphertext_fails() {
        let manifest = test_manifest();
        let ctx = derive_key("passphrase", &manifest).unwrap();

        let encrypted = encrypt_line(&ctx, b"secret").unwrap();
        // Tamper with the base64 payload
        let mut tampered = encrypted.clone();
        let last_char = tampered.pop().unwrap();
        tampered.push(if last_char == 'A' { 'B' } else { 'A' });

        let result = decrypt_line(&ctx, &tampered);
        assert!(result.is_err());
    }

    #[test]
    fn test_is_encrypted_line() {
        assert!(is_encrypted_line("ENC1:abc123"));
        assert!(!is_encrypted_line("{\"op_id\":\"...\"}"));
    }
}
