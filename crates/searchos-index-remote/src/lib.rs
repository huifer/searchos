//! Remote full-text index adapters (ADR-027).
//!
//! These adapters let the user pick a **remote retrieval backend** through
//! `storage.fulltext` instead of the default embedded Tantivy index:
//!
//! | Backend | Crate type | Query model |
//! |---|---|---|
//! | `elasticsearch` | [`ElasticsearchIndex`] | `multi_match` BM25 |
//! | `meilisearch` | [`MeilisearchIndex`] | REST search endpoint |
//! | `postgres` | [`PostgresFtsIndex`] | `tsvector` GIN + `ts_rank_cd` |
//! | `mysql` | [`MySqlFtsIndex`] | `FULLTEXT` ngram index + boolean mode |
//!
//! Contract preserved from the local adapters (ADR-022):
//! - The remote index is **rebuildable** — SQLite stays the metadata source
//!   of truth, chunk content is authoritative, and `rebuild_from` /
//!   [`reindex_all`] can reconstruct the index after drift or loss.
//! - Writes flow through the transactional outbox just like Tantivy; the
//!   adapter only needs to be idempotent per chunk.
//! - Workspace isolation (RFC-013 8.7): every query is scoped to the
//!   requesting workspace OR the shared global `*` namespace.
//!
//! Each backend talks to a server the user owns; SearchOS never embeds or
//! bundles these engines. Configuration lives under `storage.remote.fulltext`
//! (see `configs/searchos.example.yaml`).

pub mod elasticsearch;
pub mod meilisearch;
pub mod mysql;
pub mod postgres;
pub mod shared;

pub use elasticsearch::ElasticsearchIndex;
pub use meilisearch::MeilisearchIndex;
pub use mysql::MySqlFtsIndex;
pub use postgres::PostgresFtsIndex;
pub use shared::{candidate_allowed, ChunkDoc, RemoteBackendConfig, SqlChunkRow};

/// Which concrete backend a config selects (`storage.fulltext` value).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// Embedded default (not built here; `searchos-index-tantivy`).
    Tantivy,
    Elasticsearch,
    Meilisearch,
    PostgresFts,
    MySqlFts,
}

impl BackendKind {
    /// Parses the `storage.fulltext` string.
    pub fn from_config(value: &str) -> Self {
        match value.trim().to_lowercase().as_str() {
            "elasticsearch" | "es" => Self::Elasticsearch,
            "meilisearch" | "meili" => Self::Meilisearch,
            "postgres" | "postgresql" | "pg" => Self::PostgresFts,
            "mysql" | "mariadb" => Self::MySqlFts,
            _ => Self::Tantivy,
        }
    }

    /// Canonical config string.
    pub fn config_name(&self) -> &'static str {
        match self {
            Self::Tantivy => "tantivy",
            Self::Elasticsearch => "elasticsearch",
            Self::Meilisearch => "meilisearch",
            Self::PostgresFts => "postgres",
            Self::MySqlFts => "mysql",
        }
    }

    /// Whether this kind is served by this crate.
    pub fn is_remote(self) -> bool {
        !matches!(self, Self::Tantivy)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_backend_config_values() {
        assert_eq!(BackendKind::from_config("tantivy"), BackendKind::Tantivy);
        assert_eq!(
            BackendKind::from_config("elasticsearch"),
            BackendKind::Elasticsearch
        );
        assert_eq!(BackendKind::from_config("ES"), BackendKind::Elasticsearch);
        assert_eq!(
            BackendKind::from_config("meilisearch"),
            BackendKind::Meilisearch
        );
        assert_eq!(
            BackendKind::from_config("postgres"),
            BackendKind::PostgresFts
        );
        assert_eq!(
            BackendKind::from_config("postgresql"),
            BackendKind::PostgresFts
        );
        assert_eq!(BackendKind::from_config("mysql"), BackendKind::MySqlFts);
        assert_eq!(BackendKind::from_config("mariadb"), BackendKind::MySqlFts);
        assert!(BackendKind::from_config("postgres").is_remote());
        assert!(!BackendKind::from_config("tantivy").is_remote());
    }
}
