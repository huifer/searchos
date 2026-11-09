//! PostgreSQL full-text backend (`tsvector` GIN + `ts_rank_cd`).
//!
//! - Schema `searchos_chunks_pg` is created on open if missing; the
//!   `tsv` column is a STORED generated column over `title || body` so
//!   ranking does not require application-side text processing.
//! - Queries use `websearch_to_tsquery` (safe with arbitrary user text)
//!   scoped by workspace (`requester OR *`) and language.
//! - `rebuild_from` truncates and re-inserts SQLite payloads (ADR-022).
//! - All value positions are bound parameters; only the table name is
//!   interpolated, and it is validated to be a plain identifier.

use async_trait::async_trait;
use chrono::Utc;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::{ChunkId, DocumentId, SearchOsError};
use searchos_retrieval::SearchIndex;
use sqlx::Row;

use crate::shared::{candidate_allowed, ChunkDoc, RemoteBackendConfig, SqlChunkRow};

/// PostgreSQL-backed BM25-ish (ts_rank_cd) index.
pub struct PostgresFtsIndex {
    pool: sqlx::PgPool,
    table: String,
}

impl PostgresFtsIndex {
    /// Connects, pings and ensures the schema exists (fail-fast on a bad
    /// DSN or unreachable server).
    pub async fn open(config: RemoteBackendConfig) -> Result<Self, SearchOsError> {
        config.validate("postgres")?;
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .connect(&config.url)
            .await
            .map_err(|e| SearchOsError::Storage(format!("postgres connect: {e}")))?;
        let table = sanitize_table(&config.index, "searchos_chunks_pg");
        let idx = Self { pool, table };
        idx.ensure_schema().await?;
        Ok(idx)
    }

    async fn ensure_schema(&self) -> Result<(), SearchOsError> {
        // Generated tsvector column requires PostgreSQL 12+; documented.
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
                chunk_id TEXT PRIMARY KEY, \
                document_id TEXT NOT NULL, \
                version_id TEXT NOT NULL, \
                title TEXT NOT NULL DEFAULT '', \
                body TEXT NOT NULL, \
                url TEXT NOT NULL, \
                language TEXT NOT NULL DEFAULT '', \
                workspace_id TEXT NOT NULL, \
                fetched_at BIGINT NOT NULL\
            ); \
            ALTER TABLE {table} ADD COLUMN IF NOT EXISTS tsv tsvector \
                GENERATED ALWAYS AS (\
                    setweight(to_tsvector('english', coalesce(title, '')), 'A') || \
                    setweight(to_tsvector('english', body), 'B')\
                ) STORED; \
            CREATE INDEX IF NOT EXISTS {table}_tsv_idx ON {table} USING GIN (tsv); \
            CREATE INDEX IF NOT EXISTS {table}_doc_idx ON {table} (document_id);",
            table = self.table
        );
        // Table name is validated by `sanitize_table`; the rest is a fixed
        // DDL literal — audited as injection-free.
        sqlx::raw_sql(sql.as_str().assert_safe())
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("postgres schema: {e}")))?;
        Ok(())
    }
}

/// Asserts that a dynamic SQL string is injection-free.
///
/// Mirrors `sqlx::AssertSqlSafe` for sqlx 0.9's `SqlSafeStr` bound, with an
/// explicit audit note.
trait AssertSqlSafe {
    fn assert_safe(&self) -> sqlx::AssertSqlSafe<&str>;
}

impl AssertSqlSafe for str {
    fn assert_safe(&self) -> sqlx::AssertSqlSafe<&str> {
        sqlx::AssertSqlSafe(self)
    }
}

/// Restricts a table name to `[A-Za-z_][A-Za-z0-9_]*`, falling back to a
/// default when empty. Never returns a value containing quotes/semicolons.
pub(crate) fn sanitize_table(name: &str, default: &str) -> String {
    let candidate = if name.trim().is_empty() {
        default
    } else {
        name.trim()
    };
    let ok = candidate
        .chars()
        .next()
        .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
        && candidate
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_');
    if ok {
        candidate.to_string()
    } else {
        default.to_string()
    }
}

#[async_trait]
impl SearchIndex for PostgresFtsIndex {
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError> {
        let wire = ChunkDoc::from_document(&document);
        let fetched_at = chrono::DateTime::parse_from_rfc3339(&wire.fetched_at)
            .map(|d| d.timestamp())
            .unwrap_or_else(|_| Utc::now().timestamp());
        // Fixed SQL literal; only bound parameters carry values.
        let sql = format!(
            "INSERT INTO {table} \
             (chunk_id, document_id, version_id, title, body, url, language, workspace_id, fetched_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) \
             ON CONFLICT (chunk_id) DO UPDATE SET \
             document_id = EXCLUDED.document_id, version_id = EXCLUDED.version_id, \
             title = EXCLUDED.title, body = EXCLUDED.body, url = EXCLUDED.url, \
             language = EXCLUDED.language, workspace_id = EXCLUDED.workspace_id, \
             fetched_at = EXCLUDED.fetched_at",
            table = self.table
        );
        sqlx::query(sql.as_str().assert_safe())
            .bind(&wire.chunk_id)
            .bind(&wire.document_id)
            .bind(&wire.version_id)
            .bind(&wire.title)
            .bind(&wire.body)
            .bind(&wire.url)
            .bind(&wire.language)
            .bind(&wire.workspace_id)
            .bind(fetched_at)
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("postgres upsert: {e}")))?;
        Ok(())
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        // The table name is validated; all values are bound parameters.
        let sql = format!(
            "SELECT chunk_id, document_id, version_id, title, body, url, language, \
             workspace_id, to_char(to_timestamp(fetched_at), 'YYYY-MM-DD\"T\"HH24:MI:SS\"Z\"') AS fetched_at, \
             ts_rank_cd(tsv, websearch_to_tsquery('english', $1)) AS score \
             FROM {table} WHERE tsv @@ websearch_to_tsquery('english', $1) \
             AND (workspace_id = $2 OR workspace_id = '*') \
             AND ($3::text IS NULL OR language = $3) \
             AND ($4::bigint IS NULL OR fetched_at >= $4) \
             ORDER BY score DESC LIMIT $5",
            table = self.table
        );
        let mut q = sqlx::query(sql.as_str().assert_safe())
            .bind(&query.text)
            .bind(query.workspace_id.clone().unwrap_or_default());
        q = q.bind(query.language.clone());
        q = q.bind(
            query
                .fetched_after
                .filter(|_| !query.include_stale)
                .map(|d| d.timestamp()),
        );
        q = q.bind(query.top_k.max(1) as i64);
        let rows = q
            .fetch_all(&self.pool)
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("postgres search: {e}")))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let row = SqlChunkRow {
                chunk_id: row.try_get("chunk_id").unwrap_or_default(),
                document_id: row.try_get("document_id").unwrap_or_default(),
                version_id: row.try_get("version_id").unwrap_or_default(),
                title: row.try_get("title").unwrap_or_default(),
                body: row.try_get("body").unwrap_or_default(),
                url: row.try_get("url").unwrap_or_default(),
                language: row.try_get("language").unwrap_or_default(),
                workspace_id: row.try_get("workspace_id").unwrap_or_default(),
                fetched_at: row.try_get("fetched_at").unwrap_or_default(),
                score: row.try_get::<f32, _>("score").unwrap_or(0.0) as f64,
            };
            let candidate = row.to_candidate();
            if candidate_allowed(query, &candidate) {
                out.push(candidate);
            }
        }
        out.truncate(query.top_k);
        Ok(out)
    }

    async fn delete(&self, document_id: &DocumentId) -> Result<(), SearchOsError> {
        let sql = format!("DELETE FROM {} WHERE document_id = $1", self.table);
        sqlx::query(sql.as_str().assert_safe())
            .bind(document_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("postgres delete: {e}")))?;
        Ok(())
    }

    async fn delete_by_chunk_ids(&self, chunk_ids: &[ChunkId]) -> Result<(), SearchOsError> {
        let sql = format!("DELETE FROM {} WHERE chunk_id = $1", self.table);
        for id in chunk_ids {
            sqlx::query(sql.as_str().assert_safe())
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(|e| SearchOsError::Storage(format!("postgres chunk delete: {e}")))?;
        }
        Ok(())
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        let sql = format!("SELECT count(*)::bigint AS n FROM {}", self.table);
        let row = sqlx::query(sql.as_str().assert_safe())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("postgres count: {e}")))?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0) as u64)
    }

    async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        let sql = format!("TRUNCATE TABLE {}", self.table);
        sqlx::query(sql.as_str().assert_safe())
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("postgres truncate: {e}")))?;
        for d in documents {
            self.index(d.clone()).await?;
        }
        Ok(documents.len() as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_table_rejects_injection() {
        assert_eq!(sanitize_table("my_table", "d"), "my_table");
        assert_eq!(sanitize_table("", "d"), "d");
        assert_eq!(sanitize_table("a; DROP TABLE x", "d"), "d");
        assert_eq!(sanitize_table("1abc", "d"), "d");
        assert_eq!(sanitize_table("_ok_9", "d"), "_ok_9");
    }
}
