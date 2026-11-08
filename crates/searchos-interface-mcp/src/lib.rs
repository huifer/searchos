//! MCP stdio server (RFC-012 12.8).
//!
//! JSON-RPC 2.0 over stdio: initialize, tools/list, tools/call. Tools:
//! search, extract, memory_search, cost_summary, search_trace — each with an
//! explicit parameter schema.

pub mod server;

pub use server::{handle_message, run_stdio, TOOLS};
