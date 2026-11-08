//! SQLite metadata store (Part 17 schema baseline, ADR-007).
//!
//! SQLite 是元数据 Source of Truth; Tantivy / LanceDB 是可重建索引.
//! - [`migrations`] — versioned migrations (never edit a released migration).
//! - [`store`] — the connection wrapper.
//! - [`repos`] — repositories for workspaces, agents, tasks, decisions,
//!   sources, documents, versions, chunks, cache, providers, cost ledger,
//!   outbox, memory, traces, providers.

pub mod migrations;
pub mod repos;
pub mod store;

pub use store::SqliteStore;
