# Security Audit & Improvements — Iteration 10

**Date**: 2026-06-02  
**Focus**: Privacy-first memory engine — hardening against information disclosure and integrity attacks

## Vulnerabilities Fixed (Critical/High Severity)

### 1. **Private Memory Leaks in Query Layer** [CRITICAL] ✅
- **Issue**: FTS search and fact queries returned ALL memories without filtering by privacy level
- **Impact**: Private memories could be leaked to LLMs via context generation
- **Fix**: Added privacy filters to `fts_search()`, `query_facts_by_entity()`, `query_facts_by_entities()`, `query_preferences_by_key()`
- **Commit**: `8476475`

### 2. **Remote Device Can Inject Private Memories** [CRITICAL] ✅
- **Issue**: `apply_op()` accepted Private memories from remote sync without validation
- **Impact**: Malicious device could inject Private memories into local database
- **Fix**: Added `memory.privacy.is_syncable()` check in merge logic before accepting operations
- **Commit**: `8476475`

### 3. **Sensitive Data Not Zeroized from Memory** [HIGH] ✅
- **Issue**: Passphrases and JSON serialized memory contents left in heap memory after use
- **Impact**: Sensitive data exposed to memory inspection, core dumps, cold boot attacks
- **Fix**: 
  - Zeroize encryption passphrase on SyncConfig drop
  - Zeroize plaintext JSON after serialization in oplog
  - Zeroize decrypted JSON after deserialization in oplog reading
- **Commit**: `365a8b6`

### 4. **HLC Clock Manipulation Enables Data Overwrites** [CRITICAL] ✅
- **Issue**: System clock could be set backward to create older timestamps that overwrite newer data via LWW
- **Impact**: Attackers with OS access could rewrite history by backdating operations
- **Fix**: Implemented monotonic HLC enforcement — wall_ms never decreases even if system clock goes backward
- **Commit**: `20d6690`

### 5. **Device Identity Spoofing** [HIGH] ✅
- **Issue**: No validation that operation's device_id matches the directory it came from
- **Impact**: Attackers could create `sync/devices/legitimate-device/` with spoofed operations
- **Fix**: Added device_id validation — operations must claim to come from their directory
- **Commit**: `6e23f76`

## Vulnerabilities Identified But Not Yet Fixed

### High Priority
1. **Encryption Manifest Tampering** [HIGH]
   - Salt and KDF parameters stored in plaintext without HMAC
   - Attacker with cloud storage access could modify KDF parameters
   - Mitigation: Current system fails on wrong key derivation, but explicit HMAC would be better

2. **Timing Attack Vectors** [MEDIUM-HIGH]
   - Search operations leak timing information about memory patterns
   - Examples: namespace/entity enumeration, index state inference
   - 24+ timing leaks identified in retrieval and indexing

3. **No Key Rotation Support** [HIGH]
   - Single static salt means compromised passphrase exposes all past syncs
   - No mechanism to rotate encryption key without re-encrypting all data
   - Recommendation: Add epoch-based key versioning

### Medium Priority
4. **Embedding Vectors Not Automatically Zeroized** [MEDIUM]
   - Cached embeddings contain sensitive semantic information
   - Not cleared from memory when memories are deleted
   - Recommendation: Implement ZeroizeOnDrop wrapper for embedding caches

5. **Snapshot Replay/Poisoning** [MEDIUM]
   - Old snapshots could overwrite newer data without HLC-based versioning
   - No per-entity versioning information in snapshot exports
   - Recommendation: Add HLC tracking to snapshot data

6. **Device Identity Path Traversal** [LOW-MEDIUM]
   - Device IDs used directly in filesystem paths without validation
   - No rejection of `../` or other path traversal sequences
   - Recommendation: Validate device_id format, reject special characters

## Test Coverage

All 125 unit tests pass with the security improvements:
- Privacy enforcement tests
- HLC monotonicity tests
- Device identity validation tests
- Encryption and decryption roundtrips
- Memory corruption handling

## Recommendations for Future Work

### Phase 2 (High Impact)
1. Add timing-safe search operations (constant-time comparisons, padding)
2. Implement HMAC-based manifest integrity protection
3. Add key rotation capability with epoch versioning
4. Validate and sanitize device_id format

### Phase 3 (Medium Impact)
1. Implement automatic embedding vector zeroization
2. Add snapshot HLC versioning for integrity
3. Implement differential privacy for aggregations
4. Add audit logging for access to sensitive data

### Phase 4 (Long Term)
1. Hybrid quantum-resistant encryption (post-quantum KEM)
2. Zero-knowledge proofs for certain operations
3. Hardware security module (HSM) support
4. Secure multi-party computation for cross-device operations

## Architecture Notes

**Privacy Model**:
- Private (default): Never syncs, stays on device only
- Shared {scope}: Syncs to specific device scope
- Public: Syncs to all devices

**Encryption**:
- AES-256-GCM for sync oplog (optional, per-passphrase)
- Argon2id KDF (time_cost=3, mem_cost=64MB)
- Per-line random nonces to prevent reuse

**Sync Integrity**:
- Last-Writer-Wins based on Hybrid Logical Clocks
- Monotonic enforcement prevents clock manipulation
- Device ID validation prevents spoofing
- Tombstones prevent deletion resurrection

## References

- [CWE-226: Sensitive Information Uncleared Before Release](https://cwe.mitre.org/data/definitions/226.html)
- [CWE-316: Cleartext Storage of Sensitive Information in Memory](https://cwe.mitre.org/data/definitions/316.html)
- [Cortex Security Model](./SECURITY.md)
- [Cortex Architecture](./README.md)

---

**Status**: 5 critical/high vulnerabilities fixed. System now provides defense-in-depth privacy protection.

**Next Iteration**: Focus on timing attacks and key rotation for forward secrecy.
