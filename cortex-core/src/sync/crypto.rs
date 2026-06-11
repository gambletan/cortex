//! Encryption for sync oplog files — AES-256-GCM with Argon2id key derivation.
//!
//! Each JSONL line is independently encrypted with a unique nonce.
//! Format: `ENC1:<base64(nonce[12] || ciphertext || tag[16])>`

use aes_gcm::aead::{Aead, OsRng};
use aes_gcm::{Aes256Gcm, AeadCore, Key, KeyInit};
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use subtle::ConstantTimeEq;
use zeroize::{Zeroize, ZeroizeOnDrop};

use crate::CortexError;

type HmacSha256 = Hmac<Sha256>;

const ENC_PREFIX: &str = "ENC1:";
/// Versioned line format introduced for key rotation. Payload: version[2 LE] || nonce || ct.
const ENC2_PREFIX: &str = "ENC2:";
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;
/// Width of the key-version prefix inside an ENC2 payload (u16, little-endian).
const VERSION_LEN: usize = 2;
/// PBKDF2 rounds layered over the Argon2id base when deriving keys for versions > 0.
const PBKDF2_ROUNDS: u32 = 100_000;

/// Holds the derived encryption keys for the sync oplog. Zeroized on drop.
///
/// Supports key rotation: `base_key` is the version-0 AES key (Argon2id over the manifest
/// salt — the key all existing `ENC1` data was written with). `active_key` is the key for the
/// manifest's current `active_version`; new writes use it. Keys for versions > 0 are derived
/// from the passphrase (not from `base_key`), so exfiltrating one version's AES key does not
/// reveal any other version's data — forward secrecy against key (not passphrase) compromise.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct CryptoContext {
    base_key: [u8; 32],   // version-0 AES key; used for ENC1 lines and version 0
    active_key: [u8; 32], // AES key for `active_version`
    hmac_key: [u8; 32],   // separate key for HMAC (always version-0 derived)
    passphrase: Vec<u8>,  // retained to derive keys for versions > 0 on demand
    salt: Vec<u8>,        // manifest salt (not secret) — input to version-key derivation
    active_version: u32,
}

impl CryptoContext {
    fn cipher_for(key: &[u8; 32]) -> Aes256Gcm {
        Aes256Gcm::new(Key::<Aes256Gcm>::from_slice(key))
    }

    /// The version new writes are encrypted under.
    pub fn active_version(&self) -> u32 {
        self.active_version
    }

    /// AES key for a given encryption version. Version 0 is the Argon2id base key; higher
    /// versions are derived from the passphrase, so they are independent of `base_key`.
    fn key_for_version(&self, version: u32) -> [u8; 32] {
        if version == 0 {
            self.base_key
        } else if version == self.active_version {
            self.active_key
        } else {
            derive_version_key(&self.passphrase, &self.salt, version)
        }
    }

    fn compute_hmac(&self, data: &[u8]) -> [u8; 32] {
        let mut mac = <HmacSha256 as Mac>::new_from_slice(&self.hmac_key)
            .expect("HMAC key size is valid");
        mac.update(data);
        let result = mac.finalize();
        let mut hmac_bytes = [0u8; 32];
        hmac_bytes.copy_from_slice(result.into_bytes().as_slice());
        hmac_bytes
    }

    fn verify_hmac(&self, data: &[u8], expected: &[u8; 32]) -> bool {
        let computed = self.compute_hmac(data);
        computed.as_slice().ct_eq(expected.as_slice()).into()
    }

    /// Public method to compute HMAC for operation integrity (used by OpLogWriter).
    pub fn compute_operation_hmac(&self, data: &[u8]) -> [u8; 32] {
        self.compute_hmac(data)
    }

    /// Public method to verify HMAC for operation integrity (used by oplog reader).
    pub fn verify_operation_hmac(&self, data: &[u8], expected_hmac_hex: &str) -> Result<bool, CortexError> {
        let expected_bytes = base64::engine::general_purpose::STANDARD
            .decode(expected_hmac_hex)
            .map_err(|e| CortexError::Storage(format!("Invalid HMAC encoding: {}", e)))?;
        if expected_bytes.len() != 32 {
            return Err(CortexError::Storage(format!("Invalid HMAC length: {} (expected 32)", expected_bytes.len())));
        }
        let mut expected = [0u8; 32];
        expected.copy_from_slice(&expected_bytes);
        Ok(self.verify_hmac(data, &expected))
    }
}

/// Encryption metadata stored in manifest.json.
/// Supports key versioning for forward secrecy and key rotation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncryptionManifest {
    pub algorithm: String,
    pub kdf: String,
    pub salt: String, // base64-encoded
    pub kdf_params: KdfParams,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key_version: Option<u32>, // For key rotation: version 0 is default, higher = newer rotations
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hmac_salt: Option<String>, // base64-encoded salt for HMAC key derivation
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hmac: Option<String>, // base64-encoded HMAC of manifest content (computed without hmac and hmac_salt fields)
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KdfParams {
    pub time_cost: u32,
    pub mem_cost: u32,
    pub parallelism: u32,
}

/// Generate a new encryption manifest with random salt.
pub fn new_encryption_manifest() -> EncryptionManifest {
    use rand::RngCore;
    let mut salt = [0u8; SALT_LEN];
    OsRng.fill_bytes(&mut salt);

    EncryptionManifest {
        algorithm: "aes-256-gcm".to_string(),
        kdf: "argon2id".to_string(),
        salt: base64::engine::general_purpose::STANDARD.encode(salt),
        kdf_params: KdfParams {
            time_cost: 3,
            mem_cost: 65536, // 64 MB
            parallelism: 1,
        },
        key_version: Some(0), // Start at version 0; higher versions for rotated keys
        hmac_salt: None,      // Will be computed during sync initialization
        hmac: None,           // HMAC will be computed during sync initialization
    }
}

/// Derive encryption and HMAC keys from passphrase and manifest.
/// Derive the AES key for an encryption version > 0 from the passphrase and manifest salt.
/// Version 0 is the Argon2id base key (see [`derive_key`]); this is only for versions > 0.
/// Deterministic in `(passphrase, salt, version)`, so any device with the passphrase can
/// derive every version on demand without storing or syncing key material.
fn derive_version_key(passphrase: &[u8], salt: &[u8], version: u32) -> [u8; 32] {
    debug_assert!(version > 0, "version 0 is the Argon2id base key, not a PBKDF2 derivation");
    let mut versioned_salt = Vec::with_capacity(salt.len() + 4);
    versioned_salt.extend_from_slice(salt);
    versioned_salt.extend_from_slice(&version.to_le_bytes());
    let mut out = [0u8; 32];
    pbkdf2::pbkdf2_hmac::<Sha256>(passphrase, &versioned_salt, PBKDF2_ROUNDS, &mut out);
    versioned_salt.zeroize();
    out
}

/// Derive encryption and HMAC keys from a passphrase and manifest.
///
/// Version 0 (default) is standard Argon2id and is the key all `ENC1` data uses. When the
/// manifest's `key_version` is > 0 (after a rotation), the active key is derived from the
/// passphrase via [`derive_version_key`]; older versions remain readable on demand.
pub fn derive_key(passphrase: &str, manifest: &EncryptionManifest) -> Result<CryptoContext, CortexError> {
    let salt = base64::engine::general_purpose::STANDARD
        .decode(&manifest.salt)
        .map_err(|e| CortexError::Storage(format!("Invalid salt: {}", e)))?;

    let params = argon2::Params::new(
        manifest.kdf_params.mem_cost,
        manifest.kdf_params.time_cost,
        manifest.kdf_params.parallelism,
        Some(32), // Original output length for AES key derivation (backward compatible)
    )
    .map_err(|e| CortexError::Storage(format!("Invalid Argon2 params: {}", e)))?;

    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);

    // Derive the version-0 AES key (the key all existing ENC1 data uses).
    let mut base_key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut base_key)
        .map_err(|e| CortexError::Storage(format!("Key derivation failed: {}", e)))?;

    // Derive HMAC key separately using passphrase + "HMAC" domain separator.
    // The HMAC key stays version-0 derived so the manifest and operation HMACs remain
    // verifiable across rotations (HMAC protects integrity, not secrecy).
    let mut hmac_salt = salt.clone();
    let salt_len = hmac_salt.len();
    for i in 0..3.min(salt_len) {
        hmac_salt[salt_len - 1 - i] ^= b"HMAC"[i];
    }

    let mut hmac_key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), &hmac_salt, &mut hmac_key)
        .map_err(|e| CortexError::Storage(format!("HMAC key derivation failed: {}", e)))?;

    // Select the active version's key. Version 0 is the Argon2id base; higher versions
    // (after a rotation) are derived from the passphrase, independent of base_key.
    let active_version = manifest.key_version.unwrap_or(0);
    let active_key = if active_version == 0 {
        base_key
    } else {
        derive_version_key(passphrase.as_bytes(), &salt, active_version)
    };

    Ok(CryptoContext {
        base_key,
        active_key,
        hmac_key,
        passphrase: passphrase.as_bytes().to_vec(),
        salt,
        active_version,
    })
}

/// Encrypt a plaintext line. Version 0 keeps the legacy `ENC1:<base64(nonce || ct || tag)>`
/// envelope unchanged; after a rotation (active version > 0) it writes the versioned
/// `ENC2:<base64(version[2 LE] || nonce || ct || tag)>` envelope.
pub fn encrypt_line(ctx: &CryptoContext, plaintext: &[u8]) -> Result<String, CortexError> {
    let cipher = CryptoContext::cipher_for(&ctx.active_key);
    let nonce = Aes256Gcm::generate_nonce(&mut OsRng);

    let ciphertext = cipher
        .encrypt(&nonce, plaintext)
        .map_err(|e| CortexError::Storage(format!("Encryption failed: {}", e)))?;

    if ctx.active_version == 0 {
        // Legacy envelope — byte-for-byte unchanged until the first rotation.
        let mut combined = Vec::with_capacity(NONCE_LEN + ciphertext.len());
        combined.extend_from_slice(&nonce);
        combined.extend_from_slice(&ciphertext);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&combined);
        Ok(format!("{}{}", ENC_PREFIX, encoded))
    } else {
        let version = u16::try_from(ctx.active_version)
            .map_err(|_| CortexError::Storage("key version exceeds u16".into()))?;
        let mut combined = Vec::with_capacity(VERSION_LEN + NONCE_LEN + ciphertext.len());
        combined.extend_from_slice(&version.to_le_bytes());
        combined.extend_from_slice(&nonce);
        combined.extend_from_slice(&ciphertext);
        let encoded = base64::engine::general_purpose::STANDARD.encode(&combined);
        Ok(format!("{}{}", ENC2_PREFIX, encoded))
    }
}

/// Decrypt an `ENC1:` (version 0) or `ENC2:` (versioned) line → plaintext bytes.
pub fn decrypt_line(ctx: &CryptoContext, encrypted_line: &str) -> Result<Vec<u8>, CortexError> {
    if let Some(payload) = encrypted_line.strip_prefix(ENC2_PREFIX) {
        let combined = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|e| CortexError::Storage(format!("Base64 decode failed: {}", e)))?;
        if combined.len() < VERSION_LEN + NONCE_LEN + 16 {
            return Err(CortexError::Storage("Encrypted data too short".into()));
        }
        let version = u16::from_le_bytes([combined[0], combined[1]]) as u32;
        let (nonce_bytes, ciphertext) = combined[VERSION_LEN..].split_at(NONCE_LEN);
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
        let mut key = ctx.key_for_version(version);
        let cipher = CryptoContext::cipher_for(&key);
        let result = cipher.decrypt(nonce, ciphertext).map_err(|e| {
            CortexError::Storage(format!("Decryption failed (wrong key or tampered data): {}", e))
        });
        key.zeroize();
        result
    } else if let Some(payload) = encrypted_line.strip_prefix(ENC_PREFIX) {
        let combined = base64::engine::general_purpose::STANDARD
            .decode(payload)
            .map_err(|e| CortexError::Storage(format!("Base64 decode failed: {}", e)))?;
        if combined.len() < NONCE_LEN + 16 {
            return Err(CortexError::Storage("Encrypted data too short".into()));
        }
        let (nonce_bytes, ciphertext) = combined.split_at(NONCE_LEN);
        let nonce = aes_gcm::Nonce::from_slice(nonce_bytes);
        let cipher = CryptoContext::cipher_for(&ctx.base_key);
        cipher.decrypt(nonce, ciphertext).map_err(|e| {
            CortexError::Storage(format!("Decryption failed (wrong key or tampered data): {}", e))
        })
    } else {
        Err(CortexError::Storage("Missing ENC1:/ENC2: prefix".into()))
    }
}

/// Check if a line is encrypted (either envelope version).
pub fn is_encrypted_line(line: &str) -> bool {
    line.starts_with(ENC_PREFIX) || line.starts_with(ENC2_PREFIX)
}

/// Internal: Compute HMAC with explicit salt (for testing and verification)
fn compute_manifest_hmac_with_salt(
    manifest_json: &[u8],
    passphrase: &str,
    salt: &[u8; 16],
) -> Result<String, CortexError> {
    let params = argon2::Params::new(
        8192,     // 8MB for manifest verification (faster than oplog encryption)
        1,        // 1 iteration
        1,        // 1 parallelism
        Some(32), // 32-byte HMAC key
    )
    .map_err(|e| CortexError::Storage(format!("Invalid Argon2 params: {}", e)))?;

    let argon2 = argon2::Argon2::new(argon2::Algorithm::Argon2id, argon2::Version::V0x13, params);
    let mut hmac_key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), salt, &mut hmac_key)
        .map_err(|e| CortexError::Storage(format!("HMAC key derivation failed: {}", e)))?;

    let mut mac = <HmacSha256 as Mac>::new_from_slice(&hmac_key)
        .expect("HMAC key size is valid");
    mac.update(manifest_json);
    let result = mac.finalize();

    hmac_key.zeroize();
    Ok(base64::engine::general_purpose::STANDARD.encode(result.into_bytes()))
}

/// Compute HMAC for manifest integrity protection.
/// Returns (hmac_value, salt) so salt can be persisted and reused for verification.
pub fn compute_manifest_hmac(manifest_json: &[u8], passphrase: &str) -> Result<(String, String), CortexError> {
    // Generate random salt for HMAC key derivation
    let mut salt = [0u8; 16];
    use rand::RngCore;
    OsRng.fill_bytes(&mut salt);

    let hmac_value = compute_manifest_hmac_with_salt(manifest_json, passphrase, &salt)?;
    let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
    Ok((hmac_value, salt_b64))
}

/// Internal: Verify manifest integrity with explicit salt (for testing)
#[cfg(test)]
fn verify_manifest_integrity_with_salt(
    manifest_json: &[u8],
    expected_hmac: &str,
    passphrase: &str,
    salt: &[u8; 16],
) -> Result<bool, CortexError> {
    let computed = compute_manifest_hmac_with_salt(manifest_json, passphrase, salt)?;
    // Use constant-time comparison
    Ok(computed.as_bytes().ct_eq(expected_hmac.as_bytes()).into())
}

/// Verify manifest integrity using HMAC.
/// If salt_b64 is provided, it uses the stored salt; otherwise generates a new one.
pub fn verify_manifest_integrity(
    manifest_json: &[u8],
    expected_hmac: &str,
    passphrase: &str,
    salt_b64: Option<&str>,
) -> Result<bool, CortexError> {
    let computed = match salt_b64 {
        Some(salt_b64_str) => {
            // Use stored salt for verification
            let salt_bytes = base64::engine::general_purpose::STANDARD
                .decode(salt_b64_str)
                .map_err(|e| CortexError::Storage(format!("Invalid HMAC salt: {}", e)))?;
            if salt_bytes.len() != 16 {
                return Err(CortexError::Storage("Invalid HMAC salt length".into()));
            }
            let mut salt = [0u8; 16];
            salt.copy_from_slice(&salt_bytes);
            compute_manifest_hmac_with_salt(manifest_json, passphrase, &salt)?
        }
        None => {
            // Generate new salt (legacy path for old manifests)
            compute_manifest_hmac(manifest_json, passphrase)?.0
        }
    };
    // Use constant-time comparison
    Ok(computed.as_bytes().ct_eq(expected_hmac.as_bytes()).into())
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

    /// Same salt/params, different active key version (simulates a rotation).
    fn manifest_at_version(base: &EncryptionManifest, version: u32) -> EncryptionManifest {
        let mut m = base.clone();
        m.key_version = Some(version);
        m
    }

    #[test]
    fn test_version0_keeps_enc1_envelope() {
        let ctx = derive_key("pass", &test_manifest()).unwrap();
        assert_eq!(ctx.active_version(), 0);
        let line = encrypt_line(&ctx, b"hello").unwrap();
        assert!(line.starts_with("ENC1:"), "v0 must keep the legacy envelope");
        assert_eq!(decrypt_line(&ctx, &line).unwrap(), b"hello");
    }

    #[test]
    fn test_rotated_version_uses_enc2_and_roundtrips() {
        let m1 = manifest_at_version(&test_manifest(), 1);
        let ctx1 = derive_key("pass", &m1).unwrap();
        assert_eq!(ctx1.active_version(), 1);
        let line = encrypt_line(&ctx1, b"new secret").unwrap();
        assert!(line.starts_with("ENC2:"), "rotated writes use the versioned envelope");
        assert_eq!(decrypt_line(&ctx1, &line).unwrap(), b"new secret");
    }

    #[test]
    fn test_rotated_context_still_reads_old_v0_data() {
        let m0 = test_manifest();
        let m1 = manifest_at_version(&m0, 1);
        let ctx0 = derive_key("pass", &m0).unwrap();
        let ctx1 = derive_key("pass", &m1).unwrap();
        let old = encrypt_line(&ctx0, b"old data").unwrap();
        assert!(old.starts_with("ENC1:"));
        // A rotated (v1) context must still decrypt pre-rotation v0 data.
        assert_eq!(decrypt_line(&ctx1, &old).unwrap(), b"old data");
    }

    #[test]
    fn test_mixed_version_lines_all_readable() {
        let m0 = test_manifest();
        let ctx0 = derive_key("pass", &m0).unwrap();
        let ctx2 = derive_key("pass", &manifest_at_version(&m0, 2)).unwrap();
        let l_v0 = encrypt_line(&ctx0, b"a").unwrap(); // ENC1
        let l_v2 = encrypt_line(&ctx2, b"b").unwrap(); // ENC2@2
        assert_eq!(decrypt_line(&ctx2, &l_v0).unwrap(), b"a");
        assert_eq!(decrypt_line(&ctx2, &l_v2).unwrap(), b"b");
    }

    #[test]
    fn test_version_keys_are_independent_forward_secrecy() {
        // The version-0 AES key must not decrypt version-1 data: leaking one version's key
        // does not expose other versions.
        let m1 = manifest_at_version(&test_manifest(), 1);
        let ctx1 = derive_key("pass", &m1).unwrap();
        assert_ne!(ctx1.base_key, ctx1.active_key, "v0 and v1 keys must differ");

        let line = encrypt_line(&ctx1, b"v1 only").unwrap();
        let combined = base64::engine::general_purpose::STANDARD
            .decode(line.strip_prefix("ENC2:").unwrap())
            .unwrap();
        let (nonce_bytes, ciphertext) = combined[VERSION_LEN..].split_at(NONCE_LEN);
        let v0_cipher = CryptoContext::cipher_for(&ctx1.base_key);
        let res = v0_cipher.decrypt(aes_gcm::Nonce::from_slice(nonce_bytes), ciphertext);
        assert!(res.is_err(), "the v0 key must not decrypt v1 ciphertext");
    }

    #[test]
    fn test_version_key_derivation_deterministic() {
        let m = test_manifest();
        let salt = base64::engine::general_purpose::STANDARD.decode(&m.salt).unwrap();
        let k1a = derive_version_key(b"pass", &salt, 1);
        let k1b = derive_version_key(b"pass", &salt, 1);
        let k2 = derive_version_key(b"pass", &salt, 2);
        assert_eq!(k1a, k1b, "same inputs derive the same key");
        assert_ne!(k1a, k2, "different versions derive different keys");
    }

    #[test]
    fn test_truncated_enc2_line_errors_gracefully() {
        let ctx = derive_key("pass", &manifest_at_version(&test_manifest(), 1)).unwrap();
        let bad = format!(
            "ENC2:{}",
            base64::engine::general_purpose::STANDARD.encode([0u8; 3])
        );
        assert!(decrypt_line(&ctx, &bad).is_err());
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
        assert_eq!(ctx1.base_key, ctx2.base_key);
    }

    #[test]
    fn test_different_salt_different_key() {
        let m1 = test_manifest();
        let m2 = test_manifest(); // new random salt
        let ctx1 = derive_key("same-passphrase", &m1).unwrap();
        let ctx2 = derive_key("same-passphrase", &m2).unwrap();
        assert_ne!(ctx1.base_key, ctx2.base_key);
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
    fn test_manifest_hmac_computation_deterministic() {
        let manifest_json = br#"{"algorithm":"aes-256-gcm","kdf":"argon2id","salt":"test","kdf_params":{"time_cost":1,"mem_cost":1024,"parallelism":1}}"#;
        let passphrase = "test-passphrase";
        let fixed_salt = [0u8; 16]; // Fixed salt for reproducible tests

        let hmac1 = compute_manifest_hmac_with_salt(manifest_json, passphrase, &fixed_salt).unwrap();
        let hmac2 = compute_manifest_hmac_with_salt(manifest_json, passphrase, &fixed_salt).unwrap();

        // Same content, passphrase, and salt should produce same HMAC
        assert_eq!(hmac1, hmac2, "HMAC should be deterministic with same salt");
    }

    #[test]
    fn test_manifest_hmac_verification_success() {
        let manifest_json = br#"{"algorithm":"aes-256-gcm","kdf":"argon2id","salt":"test","kdf_params":{"time_cost":1,"mem_cost":1024,"parallelism":1}}"#;
        let passphrase = "test-passphrase";
        let fixed_salt = [0u8; 16];

        let hmac = compute_manifest_hmac_with_salt(manifest_json, passphrase, &fixed_salt).unwrap();
        let verified = verify_manifest_integrity_with_salt(manifest_json, &hmac, passphrase, &fixed_salt).unwrap();

        assert!(verified, "Valid HMAC should verify successfully");
    }

    #[test]
    fn test_manifest_hmac_verification_fails_on_tampering() {
        let manifest_json = br#"{"algorithm":"aes-256-gcm","kdf":"argon2id","salt":"test","kdf_params":{"time_cost":1,"mem_cost":1024,"parallelism":1}}"#;
        let passphrase = "test-passphrase";
        let fixed_salt = [0u8; 16];

        let hmac = compute_manifest_hmac_with_salt(manifest_json, passphrase, &fixed_salt).unwrap();

        // Tamper with manifest content
        let tampered = br#"{"algorithm":"aes-256-gcm","kdf":"argon2id","salt":"tampered","kdf_params":{"time_cost":1,"mem_cost":1024,"parallelism":1}}"#;
        let verified = verify_manifest_integrity_with_salt(tampered, &hmac, passphrase, &fixed_salt).unwrap();

        assert!(!verified, "Tampered manifest should fail verification");
    }

    #[test]
    fn test_is_encrypted_line() {
        assert!(is_encrypted_line("ENC1:abc123"));
        assert!(!is_encrypted_line("{\"op_id\":\"...\"}"));
    }
}
