//! Sync passphrase storage — env var and OS keychain.
//!
//! The sync passphrase is never written to the database or any Cortex file.
//! Resolution order for resume:
//! 1. `CORTEX_SYNC_PASSPHRASE` environment variable (all platforms).
//! 2. macOS login Keychain (`security` CLI), service `cortex-sync`, account =
//!    the sync device ID. Stored automatically when sync is enabled with
//!    encryption on macOS.
//!
//! Known trade-off: `security add-generic-password -w <pass>` exposes the value
//! in the process argument list for the lifetime of that (sub-millisecond)
//! process. This is strictly better than persisting the passphrase in a file
//! and is the standard mechanism available without a native keychain binding.

use std::process::Command;

const ENV_VAR: &str = "CORTEX_SYNC_PASSPHRASE";
/// Set to any non-empty value to disable all keychain access (used by tests so
/// they never touch the developer's real login keychain).
const NO_KEYCHAIN_ENV: &str = "CORTEX_NO_KEYCHAIN";
const KEYCHAIN_SERVICE: &str = "cortex-sync";

fn keychain_disabled() -> bool {
    std::env::var(NO_KEYCHAIN_ENV).map(|v| !v.is_empty()).unwrap_or(false)
}

/// Resolve the sync passphrase for `device_id`: env var first, then OS keychain.
pub fn load_passphrase(device_id: &str) -> Option<String> {
    if let Ok(p) = std::env::var(ENV_VAR) {
        if !p.is_empty() {
            return Some(p);
        }
    }
    if keychain_disabled() {
        return None;
    }
    keychain_get(device_id)
}

/// Best-effort store of the passphrase in the OS keychain (macOS only).
/// Returns true if stored. Failure is non-fatal: the user can always supply
/// the passphrase via the env var instead.
pub fn store_passphrase(device_id: &str, passphrase: &str) -> bool {
    if keychain_disabled() {
        return false;
    }
    keychain_set(device_id, passphrase)
}

#[cfg(target_os = "macos")]
fn keychain_get(account: &str) -> Option<String> {
    let out = Command::new("/usr/bin/security")
        .args(["find-generic-password", "-s", KEYCHAIN_SERVICE, "-a", account, "-w"])
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let pass = String::from_utf8(out.stdout).ok()?;
    let trimmed = pass.trim_end_matches('\n');
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

#[cfg(target_os = "macos")]
fn keychain_set(account: &str, passphrase: &str) -> bool {
    Command::new("/usr/bin/security")
        .args([
            "add-generic-password",
            "-s",
            KEYCHAIN_SERVICE,
            "-a",
            account,
            "-w",
            passphrase,
            "-U", // update if it already exists
        ])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

#[cfg(not(target_os = "macos"))]
fn keychain_get(_account: &str) -> Option<String> {
    None
}

#[cfg(not(target_os = "macos"))]
fn keychain_set(_account: &str, _passphrase: &str) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn env_var_takes_priority() {
        // Uses a unique var-free assertion: load for a nonexistent device with no
        // env var set must be None (no keychain entry for a random account).
        std::env::remove_var(ENV_VAR);
        assert!(load_passphrase("cortex-test-nonexistent-device-xyz").is_none());
        std::env::set_var(ENV_VAR, "from-env");
        assert_eq!(
            load_passphrase("cortex-test-nonexistent-device-xyz").as_deref(),
            Some("from-env")
        );
        std::env::remove_var(ENV_VAR);
    }
}
