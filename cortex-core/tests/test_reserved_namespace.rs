//! The Muse export namespace is reserved: only `cortex-mcp-server gateway allow` may
//! write it. Every ordinary ingest path must refuse.

use cortex_core::types::{BatchIngestItem, MUSE_EXPORT_NAMESPACE};
use cortex_core::Cortex;

#[test]
fn single_ingest_into_reserved_namespace_is_rejected() {
    let c = Cortex::in_memory().unwrap();
    let r = c.ingest_with_options("secret", "test", None, None, None, Some(MUSE_EXPORT_NAMESPACE), None);
    assert!(r.is_err());
    assert!(c.list_namespaces().unwrap().iter().all(|(ns, _)| ns != MUSE_EXPORT_NAMESPACE));
}

#[test]
fn batch_ingest_never_writes_reserved_namespace() {
    let c = Cortex::in_memory().unwrap();
    let item = BatchIngestItem {
        text: "secret".into(),
        channel: "test".into(),
        user_id: None,
        salience_hint: None,
        embedding: None,
        namespace: Some(MUSE_EXPORT_NAMESPACE.into()),
        privacy: None,
    };
    let _ = c.ingest_batch(vec![item]);
    assert!(c.list_namespaces().unwrap().iter().all(|(ns, _)| ns != MUSE_EXPORT_NAMESPACE));
}

#[test]
fn other_namespaces_still_work() {
    let c = Cortex::in_memory().unwrap();
    assert!(c.ingest_with_options("fine", "test", None, None, None, Some("work"), None).is_ok());
}

#[test]
fn export_copy_cannot_be_made_syncable() {
    use cortex_core::types::{MemContent, MemObjectBuilder, MemSource, MemoryTier, PrivacyLevel};
    let c = Cortex::in_memory().unwrap();
    let m = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text("x".into()), MemSource::new("t"))
        .privacy(PrivacyLevel::Private)
        .namespace(MUSE_EXPORT_NAMESPACE)
        .build();
    c.storage().store_memory(&m).unwrap();
    assert!(c.set_memory_privacy(m.id, PrivacyLevel::Public).is_err());
    assert!(c.set_memory_privacy(m.id, PrivacyLevel::Private).is_ok());
}

#[test]
fn near_dedup_never_merges_into_an_export_copy() {
    use cortex_core::types::{DeduplicationConfig, MemContent, MemObjectBuilder, MemSource, MemoryTier, PrivacyLevel};
    let c = Cortex::in_memory()
        .unwrap()
        .with_dedup_config(DeduplicationConfig { near_dedup: true, ..DeduplicationConfig::default() });
    let emb = vec![1.0f32, 0.0, 0.0, 0.0];
    let mut m = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text("likes sushi".into()), MemSource::new("t"))
        .privacy(PrivacyLevel::Private)
        .namespace(MUSE_EXPORT_NAMESPACE)
        .embedding(emb.clone())
        .build();
    // As `gateway allow` stores it: hash kept out of the global exact-dedup space.
    m.content_hash = m.content_hash.map(|h| format!("{MUSE_EXPORT_NAMESPACE}:{h}"));
    c.storage().store_memory(&m).unwrap();
    c.index().insert(m.id, emb.clone());
    let id = c.ingest_with_options("likes sushi", "test", None, None, Some(emb), None, None).unwrap().id;
    assert_ne!(id, m.id);
    assert!(c.storage().get_memory(id).unwrap().is_some(), "the user's own memory must be stored");
}

#[test]
fn import_never_recreates_muse_export_rows() {
    use cortex_core::types::{MemContent, MemObjectBuilder, MemSource, MemoryTier};
    let c = Cortex::in_memory().unwrap();
    let mut forged = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text("planted".into()), MemSource::new("x"))
        .namespace(MUSE_EXPORT_NAMESPACE)
        .build();
    forged.content_hash = forged.content_hash.map(|h| format!("muse-export:{h}"));
    let normal = MemObjectBuilder::new(MemoryTier::Semantic, MemContent::Text("kept".into()), MemSource::new("x")).build();
    let data = cortex_core::export::ImportData { version: None, memories: Some(vec![forged, normal]), people: None, beliefs: None };
    let report = cortex_core::export::import_all(c.storage(), c.index(), data).unwrap();
    assert_eq!(report.memories, 1);
    assert!(c.list_namespaces().unwrap().iter().all(|(ns, _)| ns != MUSE_EXPORT_NAMESPACE));
}
