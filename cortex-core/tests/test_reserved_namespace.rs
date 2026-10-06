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
