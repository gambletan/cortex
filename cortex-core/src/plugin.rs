//! Plugin system — trait-object based, compile-time registration.
//!
//! Plugins can hook into the memory lifecycle (ingest, retrieve, consolidation, decay)
//! and register custom MCP tools.

use serde_json::Value;
use std::collections::HashMap;

use crate::consolidation::ConsolidationReport;
use crate::retrieval::RetrievalResult;
use crate::storage::memory_index::MemoryIndex;
use crate::storage::traits::StorageBackend;
use crate::types::MemObject;

/// Plugin metadata.
#[derive(Debug, Clone)]
pub struct PluginMeta {
    pub name: String,
    pub version: String,
    pub description: String,
}

/// Read-only access to Cortex internals, passed to plugin hooks.
pub struct PluginContext<'a> {
    pub storage: &'a dyn StorageBackend,
    pub index: &'a MemoryIndex,
}

/// Actions a plugin can request during hooks.
/// Collected by the plugin manager and applied by Cortex.
#[derive(Debug, Clone)]
pub enum PluginAction {
    /// Add tags to a memory object.
    AddTags(Vec<String>),
    /// Set metadata key-value pairs on a memory object.
    SetMetadata(HashMap<String, Value>),
    /// Adjust the salience base_score by a multiplier (e.g., 1.2 = +20%).
    AdjustSalience(f32),
    /// Add a semantic fact.
    AddFact {
        subject: String,
        predicate: String,
        object: String,
        confidence: f32,
    },
    /// Add a user preference.
    AddPreference {
        key: String,
        value: String,
        confidence: f32,
    },
    /// Observe evidence for a belief.
    ObserveBelief {
        key: String,
        supports: bool,
        strength: f32,
    },
    /// Skip further processing (e.g., don't store this memory).
    Skip,
}

/// A custom tool that a plugin can register with the MCP server.
#[derive(Debug, Clone)]
pub struct PluginTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// The core plugin trait. All methods have default (no-op) implementations,
/// so plugins only need to implement the hooks they care about.
pub trait Plugin: Send + Sync {
    /// Return plugin metadata.
    fn meta(&self) -> PluginMeta;

    /// Initialize the plugin with optional configuration.
    fn init(&mut self, _config: Option<Value>) {}

    /// Called before a memory is stored. Can modify the memory or return actions.
    fn on_pre_ingest(
        &self,
        _mem: &MemObject,
        _text: &str,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        Vec::new()
    }

    /// Called after a memory has been stored.
    fn on_post_ingest(&self, _mem: &MemObject, _ctx: &PluginContext<'_>) {}

    /// Called before retrieval results are returned. Can reorder or filter.
    fn on_pre_retrieve(
        &self,
        _query: &str,
        _results: &mut Vec<RetrievalResult>,
        _ctx: &PluginContext<'_>,
    ) {
    }

    /// Called after a consolidation cycle completes.
    fn on_consolidation(
        &self,
        _report: &ConsolidationReport,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        Vec::new()
    }

    /// Called after temporal decay runs.
    fn on_decay(&self, _count: usize, _ctx: &PluginContext<'_>) {}

    /// Return custom MCP tools this plugin provides.
    fn tools(&self) -> Vec<PluginTool> {
        Vec::new()
    }

    /// Handle a call to a custom tool registered by this plugin.
    fn call_tool(
        &self,
        _name: &str,
        _args: &Value,
        _ctx: &PluginContext<'_>,
    ) -> Result<String, String> {
        Err("tool not found".into())
    }
}
