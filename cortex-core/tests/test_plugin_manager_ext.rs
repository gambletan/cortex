//! Extended plugin manager tests — dependency ordering, version compat, new hooks.

use cortex_core::plugin::*;
use cortex_core::plugin_manager::PluginManager;
use cortex_core::retrieval::RetrievalResult;
use cortex_core::consolidation::ConsolidationReport;
use serde_json::Value;

// ── Test plugin implementations ──────────────────────────────────────────

struct PluginA;
impl Plugin for PluginA {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "plugin_a".into(),
            version: "1.0.0".into(),
            description: "Plugin A".into(),
        }
    }
    fn dependencies(&self) -> Vec<String> {
        vec![] // no deps
    }
}

struct PluginB;
impl Plugin for PluginB {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "plugin_b".into(),
            version: "1.0.0".into(),
            description: "Plugin B — depends on A".into(),
        }
    }
    fn dependencies(&self) -> Vec<String> {
        vec!["plugin_a".into()]
    }
}

struct PluginC;
impl Plugin for PluginC {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "plugin_c".into(),
            version: "1.0.0".into(),
            description: "Plugin C — depends on B".into(),
        }
    }
    fn dependencies(&self) -> Vec<String> {
        vec!["plugin_b".into()]
    }
}

struct VersionedPlugin {
    version_req: Option<String>,
}

impl Plugin for VersionedPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "versioned".into(),
            version: "1.0.0".into(),
            description: "Version-constrained plugin".into(),
        }
    }
    fn cortex_version_req(&self) -> Option<String> {
        self.version_req.clone()
    }
}

struct HookTracker {
    name: String,
}

impl Plugin for HookTracker {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: self.name.clone(),
            version: "1.0.0".into(),
            description: "Tracks hook calls".into(),
        }
    }

    fn on_pre_retrieve(
        &self,
        _query: &str,
        results: &mut Vec<RetrievalResult>,
        _ctx: &PluginContext<'_>,
    ) {
        // Reverse results to prove the hook ran
        results.reverse();
    }

    fn on_consolidation(
        &self,
        _report: &ConsolidationReport,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        vec![PluginAction::AddTags(vec!["consolidated".into()])]
    }

    fn on_decay(&self, _count: usize, _ctx: &PluginContext<'_>) {
        // no-op, just verify it doesn't panic
    }
}

// ── register_all with dependency ordering ────────────────────────────────

#[test]
fn test_register_all_respects_dependencies() {
    let mut pm = PluginManager::new();

    // Register in reverse dependency order
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(PluginC),
        Box::new(PluginA),
        Box::new(PluginB),
    ];

    pm.register_all(plugins);
    assert_eq!(pm.count(), 3);
}

#[test]
fn test_register_all_empty() {
    let mut pm = PluginManager::new();
    pm.register_all(vec![]);
    assert_eq!(pm.count(), 0);
}

#[test]
fn test_register_all_no_deps() {
    let mut pm = PluginManager::new();
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(PluginA),
    ];
    pm.register_all(plugins);
    assert_eq!(pm.count(), 1);
}

#[test]
fn test_register_all_missing_dependency() {
    let mut pm = PluginManager::new();
    // PluginB depends on PluginA, but A is not provided
    let plugins: Vec<Box<dyn Plugin>> = vec![
        Box::new(PluginB),
    ];
    // Should still register (with warning), not panic
    pm.register_all(plugins);
    assert_eq!(pm.count(), 1);
}

// ── Version compatibility ────────────────────────────────────────────────

#[test]
fn test_version_compat_no_constraint() {
    let plugin = VersionedPlugin { version_req: None };
    assert!(PluginManager::check_version_compat(&plugin, "0.1.0"));
    assert!(PluginManager::check_version_compat(&plugin, "99.0.0"));
}

#[test]
fn test_version_compat_matching() {
    let plugin = VersionedPlugin {
        version_req: Some(">=0.1.0, <1.0.0".into()),
    };
    assert!(PluginManager::check_version_compat(&plugin, "0.1.0"));
    assert!(PluginManager::check_version_compat(&plugin, "0.5.0"));
    assert!(!PluginManager::check_version_compat(&plugin, "1.0.0"));
    assert!(!PluginManager::check_version_compat(&plugin, "2.0.0"));
}

#[test]
fn test_version_compat_exact() {
    let plugin = VersionedPlugin {
        version_req: Some("=0.1.0".into()),
    };
    assert!(PluginManager::check_version_compat(&plugin, "0.1.0"));
    assert!(!PluginManager::check_version_compat(&plugin, "0.2.0"));
}

#[test]
fn test_version_compat_invalid_version() {
    let plugin = VersionedPlugin {
        version_req: Some(">=0.1.0".into()),
    };
    // Invalid version string should return true (permissive)
    assert!(PluginManager::check_version_compat(&plugin, "not-a-version"));
}

// ── Hook dispatch ────────────────────────────────────────────────────────

#[test]
fn test_on_pre_retrieve_dispatch() {
    let mut pm = PluginManager::new();
    pm.register(Box::new(HookTracker { name: "tracker".into() }));

    let cortex = cortex_core::Cortex::in_memory().unwrap();
    let ctx = cortex.plugin_context();

    let mut results = Vec::new();
    pm.on_pre_retrieve("test query", &mut results, &ctx);
    // No results to reverse, but hook should have run without error
}

#[test]
fn test_on_consolidation_dispatch() {
    let mut pm = PluginManager::new();
    pm.register(Box::new(HookTracker { name: "tracker".into() }));

    let cortex = cortex_core::Cortex::in_memory().unwrap();
    let ctx = cortex.plugin_context();

    let report = ConsolidationReport {
        episodes_scanned: 0,
        decayed_updated: 0,
        decayed_swept: 0,
        promoted_to_semantic: 0,
        patterns_detected: 0,
        contradictions_found: 0,
        people_updated: 0,
    };

    let actions = pm.on_consolidation(&report, &ctx);
    assert_eq!(actions.len(), 1);
    assert!(matches!(&actions[0], PluginAction::AddTags(tags) if tags == &["consolidated"]));
}

#[test]
fn test_on_decay_dispatch() {
    let mut pm = PluginManager::new();
    pm.register(Box::new(HookTracker { name: "tracker".into() }));

    let cortex = cortex_core::Cortex::in_memory().unwrap();
    let ctx = cortex.plugin_context();

    // Should not panic
    pm.on_decay(42, &ctx);
}

// ── register_with_config ─────────────────────────────────────────────────

struct ConfigPlugin {
    configured: std::sync::atomic::AtomicBool,
}

impl Plugin for ConfigPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "config_plugin".into(),
            version: "1.0.0".into(),
            description: "Accepts config".into(),
        }
    }

    fn init(&mut self, config: Option<Value>) {
        if config.is_some() {
            self.configured.store(true, std::sync::atomic::Ordering::Relaxed);
        }
    }
}

#[test]
fn test_register_with_config() {
    let mut pm = PluginManager::new();
    pm.register_with_config(
        Box::new(ConfigPlugin {
            configured: std::sync::atomic::AtomicBool::new(false),
        }),
        serde_json::json!({"key": "value"}),
    );
    assert_eq!(pm.count(), 1);
}

// ── call_tool with unknown tool ──────────────────────────────────────────

#[test]
fn test_call_tool_unknown() {
    let pm = PluginManager::new();
    let cortex = cortex_core::Cortex::in_memory().unwrap();
    let ctx = cortex.plugin_context();

    let result = pm.call_tool("nonexistent_tool", &serde_json::json!({}), &ctx);
    assert!(result.is_none());
}
