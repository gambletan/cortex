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
const NONCE_LEN: usize = 12;
const SALT_LEN: usize = 16;

/// Holds the derived encryption key. Zeroized on drop.
#[derive(Zeroize, ZeroizeOnDrop)]
pub struct CryptoContext {
    key_bytes: [u8; 32],
    hmac_key: [u8; 32], // Separate key for HMAC
}

impl CryptoContext {
    fn cipher(&self) -> Aes256Gcm {
        let key = Key::<Aes256Gcm>::from_slice(&self.key_bytes);
        Aes256Gcm::new(key)
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
/// Maintains backward compatibility: AES key derived from 32-byte output,
/// HMAC key derived separately to avoid breaking existing encrypted data.
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

    // Derive AES key (preserved for backward compatibility)
    let mut key_bytes = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), &salt, &mut key_bytes)
        .map_err(|e| CortexError::Storage(format!("Key derivation failed: {}", e)))?;

    // Derive HMAC key separately using passphrase + "HMAC" domain separator
    let mut hmac_salt = salt.clone();
    // Append domain separator to salt to ensure HMAC key is independent
    let salt_len = hmac_salt.len();
    for i in 0..3.min(salt_len) {
        hmac_salt[salt_len - 1 - i] ^= b"HMAC"[i] as u8;
    }

    let mut hmac_key = [0u8; 32];
    argon2
        .hash_password_into(passphrase.as_bytes(), &hmac_salt, &mut hmac_key)
        .map_err(|e| CortexError::Storage(format!("HMAC key derivation failed: {}", e)))?;

    Ok(CryptoContext { key_bytes, hmac_key })
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
    let salt_b64 = base64::engine::general_purpose::STANDARD.encode(&salt);
    Ok((hmac_value, salt_b64))
}

/// Internal: Verify manifest integrity with explicit salt (for testing)
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
