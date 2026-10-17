//! `delete_memory` is documented as permanent. On a plaintext database the deleted text
//! must not survive in the database file's free pages, the FTS index, or the WAL —
//! otherwise any later copy of the file (backup, forensics) still recovers it.

use std::path::Path;

use cortex_core::Cortex;

const MARKER: &str = "ZQXDELMARKER";

fn file_contains(p: &Path, needle: &[u8]) -> bool {
    match std::fs::read(p) {
        Ok(bytes) => bytes.windows(needle.len()).any(|w| w == needle),
        Err(_) => false,
    }
}

fn assert_marker_gone(dir: &Path) {
    for name in ["memory.db", "memory.db-wal"] {
        let p = dir.join(name);
        // The FTS index stores lowercased tokens.
        let lower = MARKER.to_lowercase();
        assert!(
            !file_contains(&p, MARKER.as_bytes()) && !file_contains(&p, lower.as_bytes()),
            "deleted text still recoverable from {}",
            p.display()
        );
    }
}

#[test]
fn deleted_memory_text_is_not_recoverable_from_disk() {
    let dir = tempfile::tempdir().unwrap();
    let db = dir.path().join("memory.db");
    let cortex = Cortex::open(db.to_str().unwrap()).unwrap();
    cortex.ingest("the weather is mild today", "test", None, None, None).unwrap();
    let secret = cortex
        .ingest(&format!("{MARKER} my medical diagnosis is confidential"), "test", None, None, None)
        .unwrap();
    cortex.ingest("lunch was a sandwich", "test", None, None, None).unwrap();

    let on_disk = |n: &str| file_contains(&dir.path().join(n), MARKER.as_bytes());
    assert!(on_disk("memory.db") || on_disk("memory.db-wal"), "precondition: text is on disk");

    cortex.delete_memory(secret.id).unwrap();

    // Checked while the database is still open (the WAL is live) …
    assert_marker_gone(dir.path());
    drop(cortex);
    // … and after close.
    assert_marker_gone(dir.path());
}
