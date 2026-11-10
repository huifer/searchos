//! MCP stdio server (RFC-012 12.8).
//!
//! **Transport: stdio only (ADR-031).** The server speaks JSON-RPC 2.0 over
//! stdin/stdout: `initialize`, `tools/list`, `tools/call`. There is no HTTP
//! listener and no streamable-HTTP transport in v1; the gateway does not
//! expose an MCP route. Remote MCP is a deliberately open decision, not a
//! half-built feature — see `docs/mcp.md`.
//!
//! Tools: search, fetch, extract, ingest_text, ingest_file, memory_search,
//! document_search, cost_summary, trace_inspect — each with an explicit
//! parameter schema.

pub mod server;

pub use server::{handle_message, run_stdio, TOOLS};

/// The one transport this crate implements. Asserted by the transport gate in
/// `tests/compatibility/tests/mcp_transport.rs`.
pub const TRANSPORT: &str = "stdio";
