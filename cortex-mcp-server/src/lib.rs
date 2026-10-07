//! Cortex MCP server library: the stdio MCP tool layer and the Muse gateway, shared by the
//! `cortex-mcp-server` binary and the hosted Cortex Cloud service.

// The built-in tool schema is one large `serde_json::json!([...])` literal; with 30 tools
// it expands past the default macro recursion limit (128). Raise it for this crate.
#![recursion_limit = "512"]

pub mod capabilities;
#[cfg(feature = "gateway")]
pub mod gateway;
pub mod rpc;
pub mod tools;

pub use rpc::{JsonRpcError, JsonRpcRequest, JsonRpcResponse, SERVER_VERSION};
