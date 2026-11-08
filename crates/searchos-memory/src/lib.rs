//! Memory & cross-agent reuse (RFC-013).
//!
//! Cache 不是 Memory。Memory 不是最终答案缓存。
//! Types of memory: Query Cache (short), Document Store (long),
//! Search Memory (mid/long: task history + evidence), Research Memory
//! (project lifecycle), Semantic Memory (long: semantic knowledge).
//!
//! Default namespace isolation: 私人知识默认不跨 Workspace 泄露.

pub mod governance;
pub mod service;

pub use governance::{NamespaceGuard, RetentionPolicy};
pub use service::{MemorySearchHit, MemoryService, MemoryStats};
