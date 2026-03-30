//! Tag Classifier Plugin — automatic keyword-based tagging on ingest.
//!
//! Scans incoming text for known keyword patterns and adds tags accordingly.
//! Also registers a custom MCP tool `tag_classifier_taxonomy` to inspect the taxonomy.

use serde_json::{json, Value};

use crate::plugin::{Plugin, PluginAction, PluginContext, PluginMeta, PluginTool};
use crate::types::MemObject;

/// A rule mapping keywords to a tag.
#[derive(Debug, Clone)]
struct TagRule {
    tag: String,
    keywords: Vec<String>,
}

/// Automatic tag classifier plugin.
pub struct TagClassifierPlugin {
    rules: Vec<TagRule>,
}

impl Default for TagClassifierPlugin {
    fn default() -> Self {
        Self::new()
    }
}

impl TagClassifierPlugin {
    pub fn new() -> Self {
        Self {
            rules: default_taxonomy(),
        }
    }

    /// Create with custom rules (for testing or configuration).
    pub fn with_rules(rules: Vec<(String, Vec<String>)>) -> Self {
        Self {
            rules: rules
                .into_iter()
                .map(|(tag, keywords)| TagRule { tag, keywords })
                .collect(),
        }
    }
}

impl Plugin for TagClassifierPlugin {
    fn meta(&self) -> PluginMeta {
        PluginMeta {
            name: "tag_classifier".into(),
            version: "0.1.0".into(),
            description: "Automatic keyword-based tag classification for memories".into(),
        }
    }

    fn init(&mut self, config: Option<Value>) {
        // Allow overriding taxonomy via config
        if let Some(config) = config {
            if let Some(rules) = config.get("rules").and_then(|v| v.as_array()) {
                let mut custom_rules = Vec::new();
                for rule in rules {
                    if let (Some(tag), Some(keywords)) = (
                        rule.get("tag").and_then(|v| v.as_str()),
                        rule.get("keywords").and_then(|v| v.as_array()),
                    ) {
                        let keywords: Vec<String> = keywords
                            .iter()
                            .filter_map(|v| v.as_str().map(|s| s.to_lowercase()))
                            .collect();
                        custom_rules.push(TagRule {
                            tag: tag.to_string(),
                            keywords,
                        });
                    }
                }
                if !custom_rules.is_empty() {
                    self.rules = custom_rules;
                }
            }
        }
    }

    fn on_pre_ingest(
        &self,
        _mem: &MemObject,
        text: &str,
        _ctx: &PluginContext<'_>,
    ) -> Vec<PluginAction> {
        let text_lower = text.to_lowercase();
        let mut tags = Vec::new();

        for rule in &self.rules {
            if rule.keywords.iter().any(|kw| text_lower.contains(kw)) {
                tags.push(rule.tag.clone());
            }
        }

        if tags.is_empty() {
            Vec::new()
        } else {
            vec![PluginAction::AddTags(tags)]
        }
    }

    fn tools(&self) -> Vec<PluginTool> {
        vec![PluginTool {
            name: "tag_classifier_taxonomy".into(),
            description: "List the tag classification taxonomy used by the tag_classifier plugin"
                .into(),
            input_schema: json!({
                "type": "object",
                "properties": {}
            }),
        }]
    }

    fn call_tool(
        &self,
        name: &str,
        _args: &Value,
        _ctx: &PluginContext<'_>,
    ) -> Result<String, String> {
        match name {
            "tag_classifier_taxonomy" => {
                let taxonomy: Vec<Value> = self
                    .rules
                    .iter()
                    .map(|r| {
                        json!({
                            "tag": r.tag,
                            "keywords": r.keywords,
                        })
                    })
                    .collect();
                Ok(json!({
                    "taxonomy": taxonomy,
                    "total_rules": self.rules.len(),
                })
                .to_string())
            }
            _ => Err(format!("Unknown tool: {name}")),
        }
    }
}

/// Default taxonomy covering common memory categories.
fn default_taxonomy() -> Vec<TagRule> {
    vec![
        TagRule {
            tag: "tech".into(),
            keywords: vec![
                "rust", "python", "javascript", "typescript", "golang", "java",
                "programming", "code", "api", "database", "sql", "docker",
                "kubernetes", "linux", "git", "framework", "library",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        },
        TagRule {
            tag: "preference".into(),
            keywords: vec![
                "prefer", "like", "favorite", "favourite", "love", "hate",
                "dislike", "want", "enjoy",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        },
        TagRule {
            tag: "personal".into(),
            keywords: vec![
                "live in", "born in", "from", "family", "wife", "husband",
                "child", "parent", "friend", "colleague",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        },
        TagRule {
            tag: "work".into(),
            keywords: vec![
                "work", "job", "company", "project", "deadline", "meeting",
                "team", "manager", "client", "sprint",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        },
        TagRule {
            tag: "learning".into(),
            keywords: vec![
                "learn", "study", "course", "tutorial", "book", "reading",
                "research", "understand",
            ]
            .into_iter()
            .map(String::from)
            .collect(),
        },
    ]
}
