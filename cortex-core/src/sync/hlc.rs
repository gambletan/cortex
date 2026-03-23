//! Hybrid Logical Clock — causally consistent ordering across devices.
//!
//! Combines wall-clock time with a logical counter to handle clock skew.
//! Two events on different devices are always orderable even if wall clocks disagree.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use std::sync::Mutex;

/// A hybrid logical clock timestamp.
/// Ordering: wall_ms → counter → device_id (lexicographic tiebreak).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HlcTimestamp {
    /// Wall clock in milliseconds since Unix epoch.
    pub wall_ms: u64,
    /// Logical counter for same-millisecond ordering.
    pub counter: u32,
    /// Device ID for deterministic tiebreaking.
    pub device_id: String,
}

impl HlcTimestamp {
    pub fn new(wall_ms: u64, counter: u32, device_id: impl Into<String>) -> Self {
        Self {
            wall_ms,
            counter,
            device_id: device_id.into(),
        }
    }
}

impl Ord for HlcTimestamp {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        self.wall_ms
            .cmp(&other.wall_ms)
            .then(self.counter.cmp(&other.counter))
            .then(self.device_id.cmp(&other.device_id))
    }
}

impl PartialOrd for HlcTimestamp {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

/// A hybrid logical clock that produces monotonically increasing timestamps.
pub struct HlcClock {
    device_id: String,
    state: Mutex<HlcState>,
}

struct HlcState {
    last_wall_ms: u64,
    counter: u32,
}

fn now_ms() -> u64 {
    Utc::now().timestamp_millis() as u64
}

impl HlcClock {
    pub fn new(device_id: impl Into<String>) -> Self {
        Self {
            device_id: device_id.into(),
            state: Mutex::new(HlcState {
                last_wall_ms: 0,
                counter: 0,
            }),
        }
    }

    /// Generate a new timestamp for a local event.
    pub fn tick(&self) -> HlcTimestamp {
        let mut state = self.state.lock().unwrap();
        let now = now_ms();

        if now > state.last_wall_ms {
            state.last_wall_ms = now;
            state.counter = 0;
        } else {
            state.counter += 1;
        }

        HlcTimestamp {
            wall_ms: state.last_wall_ms,
            counter: state.counter,
            device_id: self.device_id.clone(),
        }
    }

    /// Update the clock after receiving a remote timestamp.
    /// Ensures local clock advances past the remote timestamp.
    pub fn update(&self, remote: &HlcTimestamp) -> HlcTimestamp {
        let mut state = self.state.lock().unwrap();
        let now = now_ms();

        if now > state.last_wall_ms && now > remote.wall_ms {
            // Wall clock is ahead of both — use it
            state.last_wall_ms = now;
            state.counter = 0;
        } else if remote.wall_ms > state.last_wall_ms {
            // Remote is ahead — adopt remote wall, increment counter
            state.last_wall_ms = remote.wall_ms;
            state.counter = remote.counter + 1;
        } else if state.last_wall_ms == remote.wall_ms {
            // Same wall — take max counter + 1
            state.counter = state.counter.max(remote.counter) + 1;
        } else {
            // Local is ahead — just increment
            state.counter += 1;
        }

        HlcTimestamp {
            wall_ms: state.last_wall_ms,
            counter: state.counter,
            device_id: self.device_id.clone(),
        }
    }

    pub fn device_id(&self) -> &str {
        &self.device_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tick_monotonic() {
        let clock = HlcClock::new("device-a");
        let t1 = clock.tick();
        let t2 = clock.tick();
        let t3 = clock.tick();
        assert!(t1 < t2);
        assert!(t2 < t3);
    }

    #[test]
    fn test_ordering_wall_ms() {
        let a = HlcTimestamp::new(100, 0, "a");
        let b = HlcTimestamp::new(200, 0, "a");
        assert!(a < b);
    }

    #[test]
    fn test_ordering_counter() {
        let a = HlcTimestamp::new(100, 0, "a");
        let b = HlcTimestamp::new(100, 1, "a");
        assert!(a < b);
    }

    #[test]
    fn test_ordering_device_id_tiebreak() {
        let a = HlcTimestamp::new(100, 0, "aaa");
        let b = HlcTimestamp::new(100, 0, "bbb");
        assert!(a < b);
    }

    #[test]
    fn test_update_advances_past_remote() {
        let clock_a = HlcClock::new("device-a");
        let clock_b = HlcClock::new("device-b");

        let t1 = clock_a.tick();
        // Simulate clock_b receiving t1 from clock_a
        let t2 = clock_b.update(&t1);
        // t2 must be after t1
        assert!(t2 > t1);
    }

    #[test]
    fn test_update_with_future_remote() {
        let clock = HlcClock::new("device-a");
        // Simulate receiving a timestamp far in the future
        let future = HlcTimestamp::new(u64::MAX / 2, 5, "device-b");
        let local = clock.update(&future);
        assert!(local > future);
    }

    #[test]
    fn test_serde_roundtrip() {
        let ts = HlcTimestamp::new(1234567890, 42, "my-device");
        let json = serde_json::to_string(&ts).unwrap();
        let parsed: HlcTimestamp = serde_json::from_str(&json).unwrap();
        assert_eq!(ts, parsed);
    }
}
