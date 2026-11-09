//! Extension system (RFC-011).
//!
//! Extension types: Search Provider / Fetch Provider / Site Crawler /
//! Extractor / Embedding / Reranker / Memory / Intelligence.
//! Runtimes: Declarative (YAML) → Native (Rust trait, 官方随主程序编译) →
//! WASM (后续阶段，不阻塞 MVP).
//!
//! Lifecycle: Discovered → Manifest Validation → Compatibility → Permission
//! → Dependency → Initialize → Health Check → Registered → Active;
//! failures → Degraded / Quarantined / Disabled.

pub mod lifecycle;
pub mod manifest;
pub mod snapshot;

pub use lifecycle::{
    DiscoveryReport, ExtensionRecord, ExtensionRegistry, ExtensionState, ValidationError,
};
pub use manifest::{ExtensionManifest, ExtensionType, RuntimeKind};
pub use snapshot::VersionSnapshot;
