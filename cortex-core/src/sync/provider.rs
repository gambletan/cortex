//! Cloud provider detection — find the sync folder path automatically.
//!
//! Supports iCloud Drive, Google Drive, OneDrive, Dropbox.
//! All providers work the same way: a local folder that auto-syncs to cloud.

use std::path::PathBuf;

/// Detected cloud storage provider.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudProvider {
    ICloud,
    GoogleDrive,
    OneDrive,
    Dropbox,
    Custom,
}

impl CloudProvider {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::ICloud => "iCloud Drive",
            Self::GoogleDrive => "Google Drive",
            Self::OneDrive => "OneDrive",
            Self::Dropbox => "Dropbox",
            Self::Custom => "Custom",
        }
    }
}

/// Result of provider detection.
#[derive(Debug, Clone)]
pub struct DetectedProvider {
    pub provider: CloudProvider,
    pub sync_dir: PathBuf,
}

/// Detect available cloud storage providers and return the first available one.
/// Checks: iCloud Drive → Google Drive → OneDrive → Dropbox.
pub fn detect_provider() -> Option<DetectedProvider> {
    detect_all_providers().into_iter().next()
}

/// List all available providers (not just the first).
pub fn detect_all_providers() -> Vec<DetectedProvider> {
    let home = match dirs::home_dir() {
        Some(h) => h,
        None => return Vec::new(),
    };

    let mut results = Vec::new();
    for (provider, path) in provider_candidates(&home) {
        if path.exists() && path.is_dir() {
            results.push(DetectedProvider {
                provider,
                sync_dir: path.join("cortex-sync"),
            });
        }
    }
    results
}

fn provider_candidates(home: &std::path::Path) -> Vec<(CloudProvider, PathBuf)> {
    let mut candidates = vec![
        // iCloud Drive (macOS)
        (
            CloudProvider::ICloud,
            home.join("Library/Mobile Documents/com~apple~CloudDocs"),
        ),
        // Google Drive (legacy path)
        (
            CloudProvider::GoogleDrive,
            home.join("Google Drive/My Drive"),
        ),
        (
            CloudProvider::GoogleDrive,
            home.join("Google Drive"),
        ),
        // OneDrive (legacy path)
        (CloudProvider::OneDrive, home.join("OneDrive")),
        // Dropbox
        (CloudProvider::Dropbox, home.join("Dropbox")),
    ];

    // macOS CloudStorage paths (Google Drive, OneDrive use ~/Library/CloudStorage/)
    let cloud_storage = home.join("Library/CloudStorage");
    if let Ok(entries) = std::fs::read_dir(&cloud_storage) {
        for entry in entries.flatten() {
            let name = entry.file_name().to_string_lossy().to_string();
            let path = entry.path();
            if name.starts_with("GoogleDrive-") {
                // Google Drive on macOS: ~/Library/CloudStorage/GoogleDrive-{email}/My Drive
                let my_drive = path.join("My Drive");
                if my_drive.exists() {
                    candidates.insert(0, (CloudProvider::GoogleDrive, my_drive));
                } else {
                    candidates.insert(0, (CloudProvider::GoogleDrive, path));
                }
            } else if name.starts_with("OneDrive-") {
                candidates.insert(0, (CloudProvider::OneDrive, path));
            } else if name.starts_with("Dropbox") {
                candidates.insert(0, (CloudProvider::Dropbox, path));
            }
        }
    }

    candidates
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_as_str() {
        assert_eq!(CloudProvider::ICloud.as_str(), "iCloud Drive");
        assert_eq!(CloudProvider::GoogleDrive.as_str(), "Google Drive");
        assert_eq!(CloudProvider::OneDrive.as_str(), "OneDrive");
        assert_eq!(CloudProvider::Dropbox.as_str(), "Dropbox");
    }

    #[test]
    fn test_detect_returns_none_or_some() {
        // This test just verifies detect_provider doesn't panic
        let _ = detect_provider();
    }
}
