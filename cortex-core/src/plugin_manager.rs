//! Plugin manager — registration, hook dispatch, and action application.

use std::panic;

use serde_json::Value;
use tracing::{info, warn};

use crate::consolidation::ConsolidationReport;
use crate::plugin::{Plugin, PluginAction, PluginContext, PluginTool};
use crate::retrieval::RetrievalResult;
use crate::types::MemObject;

/// Manages registered plugins and dispatches lifecycle hooks.
pub struct PluginManager {
    plugins: Vec<Box<dyn Plugin>>,
}

impl Default for PluginManager {
    fn default() -> Self {
        Self::new()
    }
}

impl PluginManager {
    pub fn new() -> Self {
        Self {
            plugins: Vec::new(),
        }
    }

    /// Register a plugin. Calls `init(None)` on registration.
    pub fn register(&mut self, mut plugin: Box<dyn Plugin>) {
        let meta = plugin.meta();
        info!(
            name = %meta.name,
            version = %meta.version,
            "Registering plugin"
        );
        plugin.init(None);
        self.plugins.push(plugin);
    }

    /// Register a plugin with configuration.
    pub fn register_with_config(&mut self, mut plugin: Box<dyn Plugin>, config: Value) {
        let meta = plugin.meta();
        info!(
            name = %meta.name,
            version = %meta.version,
            "Registering plugin (with config)"
        );
        plugin.init(Some(config));
        self.plugins.push(plugin);
    }

    /// Number of registered plugins.
    pub fn count(&self) -> usize {
        self.plugins.len()
    }

    /// Dispatch `on_pre_ingest` to all plugins, collecting actions.
    /// Each plugin runs inside `catch_unwind` to isolate panics.
    pub fn on_pre_ingest(
        &self,
        mem: &MemObject,
        text: &str,
        ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        let mut actions = Vec::new();
        for plugin in &self.plugins {
            let meta = plugin.meta();
            match panic::catch_unwind(panic::AssertUnwindSafe(|| {
                plugin.on_pre_ingest(mem, text, ctx)
            })) {
                Ok(plugin_actions) => actions.extend(plugin_actions),
                Err(_) => {
                    warn!(plugin = %meta.name, "Plugin panicked in on_pre_ingest");
                }
            }
        }
        actions
    }

    /// Dispatch `on_post_ingest` to all plugins.
    pub fn on_post_ingest(&self, mem: &MemObject, ctx: &PluginContext<'_>) {
        for plugin in &self.plugins {
            let meta = plugin.meta();
            if panic::catch_unwind(panic::AssertUnwindSafe(|| {
                plugin.on_post_ingest(mem, ctx);
            })).is_err() {
                warn!(plugin = %meta.name, "Plugin panicked in on_post_ingest");
            }
        }
    }

    /// Dispatch `on_pre_retrieve` to all plugins (mutates results in place).
    pub fn on_pre_retrieve(
        &self,
        query: &str,
        results: &mut Vec<RetrievalResult>,
        ctx: &PluginContext<'_>,
    ) {
        for plugin in &self.plugins {
            let meta = plugin.meta();
            if panic::catch_unwind(panic::AssertUnwindSafe(|| {
                plugin.on_pre_retrieve(query, results, ctx);
            })).is_err() {
                warn!(plugin = %meta.name, "Plugin panicked in on_pre_retrieve");
            }
        }
    }

    /// Dispatch `on_consolidation` to all plugins, collecting actions.
    pub fn on_consolidation(
        &self,
        report: &ConsolidationReport,
        ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        let mut actions = Vec::new();
        for plugin in &self.plugins {
            let meta = plugin.meta();
            match panic::catch_unwind(panic::AssertUnwindSafe(|| {
                plugin.on_consolidation(report, ctx)
            })) {
                Ok(plugin_actions) => actions.extend(plugin_actions),
                Err(_) => {
                    warn!(plugin = %meta.name, "Plugin panicked in on_consolidation");
                }
            }
        }
        actions
    }

    /// Dispatch `on_decay` to all plugins.
    pub fn on_decay(&self, count: usize, ctx: &PluginContext<'_>) {
        for plugin in &self.plugins {
            let meta = plugin.meta();
            if panic::catch_unwind(panic::AssertUnwindSafe(|| {
                plugin.on_decay(count, ctx);
            })).is_err() {
                warn!(plugin = %meta.name, "Plugin panicked in on_decay");
            }
        }
    }

    /// Collect all custom tools from all plugins.
    pub fn list_tools(&self) -> Vec<PluginTool> {
        let mut tools = Vec::new();
        for plugin in &self.plugins {
            tools.extend(plugin.tools());
        }
        tools
    }

    /// Try to call a custom tool. Returns `None` if no plugin handles it.
    pub fn call_tool(
        &self,
        name: &str,
        args: &Value,
        ctx: &PluginContext<'_>,
    ) -> Option<Result<String, String>> {
        for plugin in &self.plugins {
            // Check if this plugin owns the tool
            if plugin.tools().iter().any(|t| t.name == name) {
                let meta = plugin.meta();
                return match panic::catch_unwind(panic::AssertUnwindSafe(|| {
                    plugin.call_tool(name, args, ctx)
                })) {
                    Ok(result) => Some(result),
                    Err(_) => {
                        warn!(plugin = %meta.name, tool = %name, "Plugin panicked in call_tool");
                        Some(Err(format!("Plugin '{}' panicked handling tool '{}'", meta.name, name)))
                    }
                };
            }
        }
        None
    }

    /// Apply a list of `PluginAction`s to a `MemObject` (mutating in place).
    /// Returns `true` if the memory should be skipped (not stored).
    pub fn apply_actions(actions: &[PluginAction], mem: &mut MemObject) -> bool {
        let mut skip = false;
        for action in actions {
            match action {
                PluginAction::AddTags(tags) => {
                    for tag in tags {
                        if !mem.tags.contains(tag) {
                            mem.tags.push(tag.clone());
                        }
                    }
                }
                PluginAction::SetMetadata(kvs) => {
                    for (k, v) in kvs {
                        mem.metadata.insert(k.clone(), v.clone());
                    }
                }
                PluginAction::AdjustSalience(multiplier) => {
                    mem.salience.base_score = (mem.salience.base_score * multiplier).clamp(0.0, 1.0);
                    mem.salience.recompute();
                }
                PluginAction::Skip => {
                    skip = true;
                }
                // AddFact, AddPreference, ObserveBelief are handled by Cortex after storage
                _ => {}
            }
        }
        skip
    }
}
