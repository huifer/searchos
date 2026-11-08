//! SearchOS core runtime (RFC-002 2.2 core crate).
//!
//! Assembles every adapter behind the application use cases from config:
//! SQLite + Tantivy + LanceDB + FS + providers + caches + extension and
//! crawler registries. Local Mode uses this struct in-process — the CLI
//! never loops back through HTTP (P-003).

pub mod config;
pub mod hash_util;
pub mod runtime;

pub use config::{load_config, SearchOsConfig};
pub use runtime::SearchOsRuntime;

/// Crate version.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
