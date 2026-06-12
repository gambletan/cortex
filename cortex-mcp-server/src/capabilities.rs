//! Deny-by-default capability grants for the MCP surface.
//!
//! Without a policy file every MCP client gets all tools — full read/write/sync over
//! the entire memory store. With a policy file present, the server flips to
//! deny-by-default: a tool is callable (and visible in `tools/list`) only if the
//! policy grants it, either by group (`read`, `write`, `sync`, `plugins`, `all`)
//! or by exact tool name. See docs/design/query-budget-and-mcp-capabilities.md.
//!
//! Policy location: `$CORTEX_CAPABILITIES_FILE`, else `capabilities.json` next to
//! the database file. A malformed policy fails **closed** (zero grants) — never
//! open. Setting `CORTEX_CAPABILITIES_FILE` to a path that does not exist also
//! fails closed: explicitly configuring a policy expresses intent to restrict, so
//! a missing file must never silently widen access back to allow-all.
//!
//! Scope: this policy gates the **MCP JSON-RPC surface only**. The CLI subcommands
//! of this binary are operator tools — anyone who can run them already has
//! filesystem access to the database and to the policy file itself, so gating them
//! adds no security boundary. The HTTP surface (cortex-http) is gated separately
//! (input bounds today; unifying it under this policy is tracked in docs/ROADMAP.md).

use std::collections::HashSet;

use serde::Deserialize;
use tracing::{info, warn};

/// Read-only tools: query/list/inspect, no state mutation.
const READ_TOOLS: &[&str] = &[
    "memory_search",
    "memory_context",
    "fact_query",
    "preference_query",
    "person_list",
    "belief_list",
    "memory_stats",
    "namespace_list",
    "tag_list_taxonomy",
    "contradiction_check",
];

/// Mutating tools: ingest, edit, lifecycle, identity merges.
const WRITE_TOOLS: &[&str] = &[
    "memory_ingest",
    "memory_ingest_batch",
    "fact_add",
    "preference_set",
    "belief_observe",
    "person_resolve",
    "person_merge",
    "relationship_extract",
    "memory_infer",
    "memory_consolidate",
    "memory_compress",
    "memory_decay",
    "memory_archive",
    "memory_delete",
    "memory_restore",
];

/// Cloud-sync tools: enabling sync moves (encrypted) data off the local store.
const SYNC_TOOLS: &[&str] = &["sync_enable", "sync_pull", "sync_status", "sync_providers"];

/// Capability policy for one server instance, loaded once at startup.
pub enum CapabilityPolicy {
    /// No policy file present: legacy behavior, every tool allowed.
    AllowAll,
    /// Policy file present: deny-by-default, only granted atoms allowed.
    Grants(GrantSet),
}

/// The set of grant atoms (group names and/or exact tool names) from the policy file.
pub struct GrantSet {
    atoms: HashSet<String>,
}

#[derive(Deserialize)]
struct PolicyFile {
    #[allow(dead_code)]
    #[serde(default)]
    version: u32,
    grants: Vec<String>,
}

impl GrantSet {
    /// Parse a policy JSON document. Errors are returned so the caller can fail closed.
    pub fn from_json_str(raw: &str) -> Result<Self, String> {
        let parsed: PolicyFile =
            serde_json::from_str(raw).map_err(|e| format!("invalid capabilities policy: {e}"))?;
        Ok(Self {
            atoms: parsed.grants.into_iter().collect(),
        })
    }

    fn empty() -> Self {
        Self {
            atoms: HashSet::new(),
        }
    }

    fn allows(&self, tool: &str) -> bool {
        if self.atoms.contains("all") || self.atoms.contains(tool) {
            return true;
        }
        if !is_builtin(tool) {
            return self.atoms.contains("plugins");
        }
        (self.atoms.contains("read") && READ_TOOLS.contains(&tool))
            || (self.atoms.contains("write") && WRITE_TOOLS.contains(&tool))
            || (self.atoms.contains("sync") && SYNC_TOOLS.contains(&tool))
    }
}

/// Whether a tool name is one of the built-in tools (vs. plugin-registered).
pub fn is_builtin(tool: &str) -> bool {
    READ_TOOLS.contains(&tool) || WRITE_TOOLS.contains(&tool) || SYNC_TOOLS.contains(&tool)
}

impl CapabilityPolicy {
    /// Whether `tool` may be listed and called under this policy.
    pub fn is_allowed(&self, tool: &str) -> bool {
        match self {
            CapabilityPolicy::AllowAll => true,
            CapabilityPolicy::Grants(grants) => grants.allows(tool),
        }
    }

    /// Load the policy for a server instance rooted at `db_path`.
    ///
    /// Resolution order: `$CORTEX_CAPABILITIES_FILE` (fails closed if set but
    /// missing), else `capabilities.json` in the database directory, else legacy
    /// allow-all. An unreadable or malformed file fails closed (zero grants).
    pub fn load(db_path: &str) -> Self {
        let explicit = std::env::var("CORTEX_CAPABILITIES_FILE").ok();
        let path = match explicit {
            Some(p) => {
                let p = std::path::PathBuf::from(p);
                if !p.exists() {
                    // The operator explicitly asked for a policy: a missing file
                    // must never widen access. Distinct diagnostic from "malformed".
                    warn!(policy = %p.display(),
                          "CORTEX_CAPABILITIES_FILE is set but the file does not exist — \
                           failing CLOSED (all tools denied)");
                    return CapabilityPolicy::Grants(GrantSet::empty());
                }
                Some(p)
            }
            None => std::path::Path::new(db_path)
                .parent()
                .map(|dir| dir.join("capabilities.json"))
                .filter(|p| p.exists()),
        };

        let Some(path) = path else {
            warn!(
                "no capability policy found — all MCP tools enabled (legacy). \
                 Create capabilities.json next to the database to switch to deny-by-default."
            );
            return CapabilityPolicy::AllowAll;
        };

        match std::fs::read_to_string(&path)
            .map_err(|e| format!("cannot read capabilities policy: {e}"))
            .and_then(|raw| GrantSet::from_json_str(&raw))
        {
            Ok(grants) => {
                info!(policy = %path.display(), grants = grants.atoms.len(),
                      "capability policy loaded — deny-by-default active");
                CapabilityPolicy::Grants(grants)
            }
            Err(e) => {
                // Fail closed: a broken policy must never widen access.
                error_fail_closed(&path, &e);
                CapabilityPolicy::Grants(GrantSet::empty())
            }
        }
    }
}

fn error_fail_closed(path: &std::path::Path, err: &str) {
    warn!(policy = %path.display(), error = %err,
          "capability policy unusable — failing CLOSED (all tools denied)");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grants(atoms: &[&str]) -> CapabilityPolicy {
        CapabilityPolicy::Grants(GrantSet {
            atoms: atoms.iter().map(|s| s.to_string()).collect(),
        })
    }

    #[test]
    fn allow_all_permits_everything() {
        let policy = CapabilityPolicy::AllowAll;
        assert!(policy.is_allowed("memory_search"));
        assert!(policy.is_allowed("memory_delete"));
        assert!(policy.is_allowed("some_plugin_tool"));
    }

    #[test]
    fn empty_grants_deny_everything() {
        let policy = grants(&[]);
        assert!(!policy.is_allowed("memory_search"));
        assert!(!policy.is_allowed("memory_ingest"));
        assert!(!policy.is_allowed("sync_status"));
        assert!(!policy.is_allowed("some_plugin_tool"));
    }

    #[test]
    fn read_group_grants_only_read_tools() {
        let policy = grants(&["read"]);
        assert!(policy.is_allowed("memory_search"));
        assert!(policy.is_allowed("memory_stats"));
        assert!(!policy.is_allowed("memory_ingest"));
        assert!(!policy.is_allowed("memory_delete"));
        assert!(!policy.is_allowed("sync_enable"));
        assert!(!policy.is_allowed("some_plugin_tool"));
    }

    #[test]
    fn write_group_does_not_imply_read() {
        let policy = grants(&["write"]);
        assert!(policy.is_allowed("memory_ingest"));
        assert!(policy.is_allowed("memory_delete"));
        assert!(!policy.is_allowed("memory_search"));
    }

    #[test]
    fn sync_group_grants_sync_tools() {
        let policy = grants(&["sync"]);
        assert!(policy.is_allowed("sync_status"));
        assert!(policy.is_allowed("sync_enable"));
        assert!(!policy.is_allowed("memory_search"));
    }

    #[test]
    fn exact_tool_name_grant() {
        let policy = grants(&["memory_search"]);
        assert!(policy.is_allowed("memory_search"));
        assert!(!policy.is_allowed("memory_context"));
    }

    #[test]
    fn plugins_group_covers_only_non_builtin_tools() {
        let policy = grants(&["plugins"]);
        assert!(policy.is_allowed("some_plugin_tool"));
        assert!(!policy.is_allowed("memory_search"));
    }

    #[test]
    fn all_group_permits_everything() {
        let policy = grants(&["all"]);
        assert!(policy.is_allowed("memory_search"));
        assert!(policy.is_allowed("memory_delete"));
        assert!(policy.is_allowed("some_plugin_tool"));
    }

    #[test]
    fn parses_valid_policy_json() {
        let set = GrantSet::from_json_str(r#"{"version": 1, "grants": ["read", "sync_status"]}"#)
            .unwrap();
        assert!(set.allows("memory_search"));
        assert!(set.allows("sync_status"));
        assert!(!set.allows("memory_ingest"));
    }

    #[test]
    fn malformed_policy_is_an_error_so_caller_fails_closed() {
        assert!(GrantSet::from_json_str("not json").is_err());
        assert!(GrantSet::from_json_str(r#"{"version": 1}"#).is_err());
    }

    #[test]
    fn builtin_groups_cover_all_29_dispatch_tools() {
        // Keep the group tables in lock-step with the dispatch match in tools.rs.
        assert_eq!(READ_TOOLS.len() + WRITE_TOOLS.len() + SYNC_TOOLS.len(), 29);
        for t in READ_TOOLS {
            assert!(!WRITE_TOOLS.contains(t) && !SYNC_TOOLS.contains(t));
        }
        for t in WRITE_TOOLS {
            assert!(!SYNC_TOOLS.contains(t));
        }
    }
}
