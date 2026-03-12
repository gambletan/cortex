use cortex_core::plugin::{Plugin, PluginAction, PluginContext, PluginMeta, PluginTool};
use cortex_core::plugin_manager::PluginManager;
use cortex_core::plugins::tag_classifier::TagClassifierPlugin;
use cortex_core::types::*;
use cortex_core::Cortex;
use serde_json::{json, Value};

// ── Test plugin: tracks calls ─────────────────────────────────────────────

struct CountingPlugin {
    pre_ingest_count: std::sync::atomic::AtomicU32,
    post_ingest_count: std::sync::atomic::AtomicU32,
}

#[allow(dead_code)]
impl CountingPlugin {
    fn new() -> Self {
        Self {
            pre_ingest_count: std::sync::atomic::AtomicU32::new(0),
            post_ingest_count: std::sync::atomic::AtomicU32::new(0),
        }
    }
}

impl Plugin for CountingPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "counter".into(),
            version: "0.1.0".into(),
            description: "Counts hook invocations".into(),
        }
    }

    fn on_pre_ingest(
        &self,
        _mem: &MemObject,
        _text: &str,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        self.pre_ingest_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        Vec::new()
    }

    fn on_post_ingest(&self, _mem: &MemObject, _ctx: &PluginContext<'_>) {
        self.post_ingest_count
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

// ── Test plugin: returns actions ──────────────────────────────────────────

struct TaggingPlugin;

impl Plugin for TaggingPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "tagger".into(),
            version: "0.1.0".into(),
            description: "Always adds a test tag".into(),
        }
    }

    fn on_pre_ingest(
        &self,
        _mem: &MemObject,
        _text: &str,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        vec![PluginAction::AddTags(vec!["test-tag".into()])]
    }
}

// ── Test plugin: skips ingest ─────────────────────────────────────────────

struct SkipPlugin;

impl Plugin for SkipPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "skipper".into(),
            version: "0.1.0".into(),
            description: "Skips everything".into(),
        }
    }

    fn on_pre_ingest(
        &self,
        _mem: &MemObject,
        _text: &str,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        vec![PluginAction::Skip]
    }
}

// ── Test plugin: with custom tools ────────────────────────────────────────

struct ToolPlugin;

impl Plugin for ToolPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "tool_plugin".into(),
            version: "0.1.0".into(),
            description: "Plugin with custom tools".into(),
        }
    }

    fn tools(&self) -> Vec<PluginTool> {
        vec![PluginTool {
            name: "my_custom_tool".into(),
            description: "A test tool".into(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "msg": { "type": "string" }
                }
            }),
        }]
    }

    fn call_tool(
        &self,
        name: &str,
        args: &Value,
        _ctx: &PluginContext<'_>,
    ) -> Result<String, String> {
        match name {
            "my_custom_tool" => {
                let msg = args
                    .get("msg")
                    .and_then(|v| v.as_str())
                    .unwrap_or("hello");
                Ok(format!("{{\"echo\": \"{msg}\"}}"))
            }
            _ => Err(format!("Unknown tool: {name}")),
        }
    }
}

// ── PluginManager unit tests ──────────────────────────────────────────────

#[test]
fn test_plugin_manager_empty() {
    let mgr = PluginManager::new();
    assert_eq!(mgr.count(), 0);
    assert!(mgr.list_tools().is_empty());
}

#[test]
fn test_plugin_manager_register() {
    let mut mgr = PluginManager::new();
    mgr.register(Box::new(TaggingPlugin));
    assert_eq!(mgr.count(), 1);
}

#[test]
fn test_plugin_manager_pre_ingest_actions() {
    let mut mgr = PluginManager::new();
    mgr.register(Box::new(TaggingPlugin));

    let mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("test"),
    )
    .build();

    let cortex = Cortex::in_memory().unwrap();
    let ctx = cortex.plugin_context();
    let actions = mgr.on_pre_ingest(&mem, "test", &ctx);

    assert_eq!(actions.len(), 1);
    match &actions[0] {
        PluginAction::AddTags(tags) => assert_eq!(tags, &["test-tag"]),
        _ => panic!("Expected AddTags action"),
    }
}

#[test]
fn test_apply_actions_tags() {
    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("test"),
    )
    .build();

    let actions = vec![PluginAction::AddTags(vec!["a".into(), "b".into()])];
    let skip = PluginManager::apply_actions(&actions, &mut mem);

    assert!(!skip);
    assert_eq!(mem.tags, vec!["a", "b"]);
}

#[test]
fn test_apply_actions_skip() {
    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("test"),
    )
    .build();

    let actions = vec![PluginAction::Skip];
    let skip = PluginManager::apply_actions(&actions, &mut mem);
    assert!(skip);
}

#[test]
fn test_apply_actions_salience() {
    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("test"),
    )
    .salience(Salience::new(0.5))
    .build();

    let actions = vec![PluginAction::AdjustSalience(1.5)];
    PluginManager::apply_actions(&actions, &mut mem);

    assert!((mem.salience.base_score - 0.75).abs() < 0.001);
}

#[test]
fn test_apply_actions_metadata() {
    let mut mem = MemObjectBuilder::new(
        MemoryTier::Episodic,
        MemContent::Text("test".into()),
        MemSource::new("test"),
    )
    .build();

    let mut meta = std::collections::HashMap::new();
    meta.insert("key".into(), json!("value"));
    let actions = vec![PluginAction::SetMetadata(meta)];
    PluginManager::apply_actions(&actions, &mut mem);

    assert_eq!(mem.metadata.get("key").unwrap(), &json!("value"));
}

#[test]
fn test_plugin_custom_tools() {
    let mut mgr = PluginManager::new();
    mgr.register(Box::new(ToolPlugin));

    let tools = mgr.list_tools();
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].name, "my_custom_tool");

    let cortex = Cortex::in_memory().unwrap();
    let ctx = cortex.plugin_context();

    let result = mgr.call_tool("my_custom_tool", &json!({"msg": "world"}), &ctx);
    assert!(result.is_some());
    assert_eq!(result.unwrap().unwrap(), r#"{"echo": "world"}"#);

    // Unknown tool returns None
    let result = mgr.call_tool("nonexistent", &json!({}), &ctx);
    assert!(result.is_none());
}

#[test]
fn test_multiple_plugins() {
    let mut mgr = PluginManager::new();
    mgr.register(Box::new(TaggingPlugin));
    mgr.register(Box::new(ToolPlugin));

    assert_eq!(mgr.count(), 2);
    assert_eq!(mgr.list_tools().len(), 1); // Only ToolPlugin has tools
}

// ── Cortex integration tests ──────────────────────────────────────────────

#[test]
fn test_cortex_with_plugin_ingest() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(TaggingPlugin));

    let mem = cortex
        .ingest("hello world", "test", None, None, None)
        .unwrap();

    assert!(mem.tags.contains(&"test-tag".to_string()));
}

#[test]
fn test_cortex_skip_plugin() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(SkipPlugin));

    let mem = cortex
        .ingest("hello world", "test", None, None, None)
        .unwrap();

    // Memory is returned but not stored (skipped)
    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 0);
    assert_eq!(mem.tier, MemoryTier::Episodic);
}

#[test]
fn test_cortex_zero_plugins_no_overhead() {
    let cortex = Cortex::in_memory().unwrap();

    // Ingest should work normally with no plugins
    let mem = cortex
        .ingest("test memory", "test", None, None, None)
        .unwrap();

    assert_eq!(mem.tags.len(), 0); // No tags added
    let stats = cortex.stats().unwrap();
    assert_eq!(stats.episodic, 1);
}

// ── TagClassifier integration tests ───────────────────────────────────────

#[test]
fn test_tag_classifier_tech() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(TagClassifierPlugin::new()));

    let mem = cortex
        .ingest("I love programming in Rust", "test", None, None, None)
        .unwrap();

    assert!(mem.tags.contains(&"tech".to_string()));
    assert!(mem.tags.contains(&"preference".to_string())); // "love"
}

#[test]
fn test_tag_classifier_personal() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(TagClassifierPlugin::new()));

    let mem = cortex
        .ingest("I live in Shanghai with my family", "test", None, None, None)
        .unwrap();

    assert!(mem.tags.contains(&"personal".to_string()));
}

#[test]
fn test_tag_classifier_no_match() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(TagClassifierPlugin::new()));

    let mem = cortex
        .ingest("the sky is blue", "test", None, None, None)
        .unwrap();

    assert!(mem.tags.is_empty());
}

#[test]
fn test_tag_classifier_custom_tool() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(TagClassifierPlugin::new()));

    let ctx = cortex.plugin_context();
    let result = cortex
        .plugin_manager()
        .call_tool("tag_list_taxonomy", &json!({}), &ctx);

    assert!(result.is_some());
    let json_str = result.unwrap().unwrap();
    let parsed: Value = serde_json::from_str(&json_str).unwrap();
    assert!(parsed["total_rules"].as_u64().unwrap() >= 5);
}

#[test]
fn test_tag_classifier_with_custom_config() {
    let config = json!({
        "rules": [
            { "tag": "ai", "keywords": ["llm", "neural", "machine learning"] },
            { "tag": "food", "keywords": ["pizza", "sushi", "ramen"] }
        ]
    });

    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin_config(Box::new(TagClassifierPlugin::new()), config);

    let mem = cortex
        .ingest("I love ramen and sushi", "test", None, None, None)
        .unwrap();

    assert!(mem.tags.contains(&"food".to_string()));
    assert!(!mem.tags.contains(&"tech".to_string())); // Default rules replaced
}

// ── Panic isolation tests ─────────────────────────────────────────────────

struct PanicPlugin;

impl Plugin for PanicPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "panic".into(),
            version: "0.1.0".into(),
            description: "Always panics".into(),
        }
    }

    fn on_pre_ingest(
        &self,
        _mem: &MemObject,
        _text: &str,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        panic!("plugin panic!");
    }
}

#[test]
fn test_panic_isolation() {
    let cortex = Cortex::in_memory()
        .unwrap()
        .with_plugin(Box::new(PanicPlugin))
        .with_plugin(Box::new(TaggingPlugin));

    // PanicPlugin panics, but TaggingPlugin still runs
    let mem = cortex
        .ingest("hello world", "test", None, None, None)
        .unwrap();

    assert!(mem.tags.contains(&"test-tag".to_string()));
}
