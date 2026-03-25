//! Filesystem watcher for sync directory changes.
//!
//! Uses the `notify` crate to detect when remote devices write new oplog files,
//! then triggers `pull_remote()` after a debounce period.

use notify::{Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

/// Handle returned by `SyncWatcher::start()`. Call `stop()` to shut down.
pub struct WatcherHandle {
    stop_flag: Arc<AtomicBool>,
    thread: Option<thread::JoinHandle<()>>,
}

impl WatcherHandle {
    /// Signal the watcher thread to stop, and wait for it to finish.
    pub fn stop(mut self) {
        self.stop_flag.store(true, Ordering::Release);
        if let Some(handle) = self.thread.take() {
            let _ = handle.join();
        }
    }

    /// Check if the watcher is still running.
    pub fn is_running(&self) -> bool {
        !self.stop_flag.load(Ordering::Acquire)
    }
}

impl Drop for WatcherHandle {
    fn drop(&mut self) {
        self.stop_flag.store(true, Ordering::Release);
        // Don't join on drop to avoid blocking; the thread will exit on next loop iteration.
    }
}

/// Callback type invoked when the watcher detects relevant changes.
pub type OnChangeCallback = Box<dyn Fn() + Send + 'static>;

/// Configuration for the filesystem watcher.
pub struct WatcherConfig {
    /// Directory to watch (the sync devices dir).
    pub watch_dir: PathBuf,
    /// This device's ID (to ignore our own changes).
    pub device_id: String,
    /// Debounce duration: wait this long after the last change before triggering.
    pub debounce: Duration,
}

/// Start a filesystem watcher that calls `on_change` when .jsonl files from other
/// devices are created or modified, with debouncing.
///
/// Returns a `WatcherHandle` to control the watcher lifecycle.
pub fn start_watcher(
    config: WatcherConfig,
    on_change: OnChangeCallback,
) -> Result<WatcherHandle, crate::CortexError> {
    let stop_flag = Arc::new(AtomicBool::new(false));
    let stop_flag_clone = stop_flag.clone();

    let (tx, rx) = mpsc::channel::<notify::Result<Event>>();

    // Create the watcher
    let mut watcher = RecommendedWatcher::new(
        move |res| {
            let _ = tx.send(res);
        },
        notify::Config::default(),
    )
    .map_err(|e| crate::CortexError::Storage(format!("Failed to create file watcher: {}", e)))?;

    // Ensure directory exists before watching
    if !config.watch_dir.exists() {
        std::fs::create_dir_all(&config.watch_dir)
            .map_err(|e| crate::CortexError::Storage(format!("Failed to create watch dir: {}", e)))?;
    }

    watcher
        .watch(&config.watch_dir, RecursiveMode::Recursive)
        .map_err(|e| crate::CortexError::Storage(format!("Failed to watch directory: {}", e)))?;

    let thread = thread::Builder::new()
        .name("cortex-sync-watcher".into())
        .spawn(move || {
            // Keep watcher alive — it's dropped when this thread ends
            let _watcher = watcher;
            let mut last_change: Option<Instant> = None;

            loop {
                if stop_flag_clone.load(Ordering::Acquire) {
                    break;
                }

                // Check for events with a short timeout so we can check the stop flag
                match rx.recv_timeout(Duration::from_millis(200)) {
                    Ok(Ok(event)) => {
                        if is_relevant_event(&event, &config.device_id) {
                            last_change = Some(Instant::now());
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::warn!(error = %e, "Sync watcher error");
                    }
                    Err(mpsc::RecvTimeoutError::Timeout) => {}
                    Err(mpsc::RecvTimeoutError::Disconnected) => break,
                }

                // Debounce: trigger callback if enough time has passed since last change
                if let Some(last) = last_change {
                    if last.elapsed() >= config.debounce {
                        tracing::debug!("Sync watcher: debounce elapsed, triggering pull");
                        on_change();
                        last_change = None;
                    }
                }
            }

            tracing::debug!("Sync watcher thread exiting");
        })
        .map_err(|e| crate::CortexError::Storage(format!("Failed to spawn watcher thread: {}", e)))?;

    Ok(WatcherHandle {
        stop_flag,
        thread: Some(thread),
    })
}

/// Check whether a filesystem event is relevant (i.e., a .jsonl file from another device).
fn is_relevant_event(event: &Event, my_device_id: &str) -> bool {
    // Only care about creates, modifications, and renames
    match event.kind {
        EventKind::Create(_) | EventKind::Modify(_) => {}
        _ => return false,
    }

    // Check if any path is a .jsonl file not in our own device folder
    event.paths.iter().any(|path| {
        let is_jsonl = path
            .extension()
            .is_some_and(|ext| ext == "jsonl");
        if !is_jsonl {
            return false;
        }

        // Check that the file is NOT in our own device subfolder
        let path_str = path.to_string_lossy();
        !path_str.contains(&format!("/{}/", my_device_id))
            && !path_str.contains(&format!("\\{}\\", my_device_id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use notify::event::{CreateKind, DataChange, ModifyKind};

    #[test]
    fn test_is_relevant_event_remote_jsonl() {
        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/sync/devices/device-b/oplog-001.jsonl")],
            attrs: Default::default(),
        };
        assert!(is_relevant_event(&event, "device-a"));
    }

    #[test]
    fn test_is_relevant_event_own_device_ignored() {
        let event = Event {
            kind: EventKind::Create(CreateKind::File),
            paths: vec![PathBuf::from("/sync/devices/device-a/oplog-001.jsonl")],
            attrs: Default::default(),
        };
        assert!(!is_relevant_event(&event, "device-a"));
    }

    #[test]
    fn test_is_relevant_event_non_jsonl_ignored() {
        let event = Event {
            kind: EventKind::Modify(ModifyKind::Data(DataChange::Any)),
            paths: vec![PathBuf::from("/sync/devices/device-b/device.json")],
            attrs: Default::default(),
        };
        assert!(!is_relevant_event(&event, "device-a"));
    }
}
