//! MySQL / MariaDB full-text backend (`FULLTEXT` ngram index + boolean mode).
//!
//! - Schema `searchos_chunks_my` created on open; `body`/`title` carry an
//!   `ngram` FULLTEXT index so CJK text (the repo's bilingual corpus) is
//!   searchable without a separate segmentation step.
//! - Queries use `MATCH ... AGAINST (... IN BOOLEAN MODE)` which accepts
//!   arbitrary user text safely (no query-syntax errors, unlike natural
//!   language mode with special characters).
//! - `rebuild_from` truncates and re-inserts SQLite payloads (ADR-022).
//! - All value positions are bound parameters; only the table name is
//!   interpolated, and it is validated to be a plain identifier.

use async_trait::async_trait;
use chrono::Utc;
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, RetrievalQuery};
use searchos_domain::{ChunkId, DocumentId, SearchOsError};
use searchos_retrieval::SearchIndex;
use sqlx::Row;

use crate::postgres::sanitize_table;
use crate::shared::{candidate_allowed, ChunkDoc, RemoteBackendConfig, SqlChunkRow};

/// MySQL/MariaDB-backed FULLTEXT index.
pub struct MySqlFtsIndex {
    pool: sqlx::MySqlPool,
    table: String,
}

impl MySqlFtsIndex {
    /// Connects, pings and ensures the schema exists (fail-fast on a bad
    /// DSN or unreachable server).
    pub async fn open(config: RemoteBackendConfig) -> Result<Self, SearchOsError> {
        config.validate("mysql")?;
        let pool = sqlx::mysql::MySqlPoolOptions::new()
            .max_connections(5)
            .acquire_timeout(std::time::Duration::from_secs(
                config.timeout_seconds.max(1),
            ))
            .connect(&config.url)
            .await
            .map_err(|e| SearchOsError::Storage(format!("mysql connect: {e}")))?;
        let table = sanitize_table(&config.index, "searchos_chunks_my");
        let idx = Self { pool, table };
        idx.ensure_schema().await?;
        Ok(idx)
    }

    async fn ensure_schema(&self) -> Result<(), SearchOsError> {
        let sql = format!(
            "CREATE TABLE IF NOT EXISTS {table} (\
                chunk_id VARCHAR(64) PRIMARY KEY, \
                document_id VARCHAR(64) NOT NULL, \
                version_id VARCHAR(64) NOT NULL, \
                title TEXT NOT NULL, \
                body MEDIUMTEXT NOT NULL, \
                url VARCHAR(2048) NOT NULL, \
                language VARCHAR(32) NOT NULL DEFAULT '', \
                workspace_id VARCHAR(64) NOT NULL, \
                fetched_at BIGINT NOT NULL, \
                FULLTEXT KEY ft_title_body (title, body) WITH PARSER ngram, \
                KEY idx_document (document_id)\
            ) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4",
            table = self.table
        );
        // Table name is validated by `sanitize_table`; the rest is a fixed
        // DDL literal — audited as injection-free.
        sqlx::raw_sql(sql.as_str().assert_safe())
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("mysql schema: {e}")))?;
        Ok(())
    }

    /// Boolean-mode expression: plain terms joined by AND with each term
    /// prefixed by `+`. Boolean mode treats user text as terms, not
    /// operators, so arbitrary queries are safe.
    fn boolean_query(text: &str) -> String {
        let terms: Vec<String> = text
            .split_whitespace()
            .filter(|t| !t.is_empty())
            .map(|t| format!("+{t}"))
            .collect();
        terms.join(" ")
    }
}

/// Asserts that a dynamic SQL string is injection-free (see postgres.rs).
trait AssertSqlSafe {
    fn assert_safe(&self) -> sqlx::AssertSqlSafe<&str>;
}

impl AssertSqlSafe for str {
    fn assert_safe(&self) -> sqlx::AssertSqlSafe<&str> {
        sqlx::AssertSqlSafe(self)
    }
}

#[async_trait]
impl SearchIndex for MySqlFtsIndex {
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError> {
        let wire = ChunkDoc::from_document(&document);
        let fetched_at = chrono::DateTime::parse_from_rfc3339(&wire.fetched_at)
            .map(|d| d.timestamp())
            .unwrap_or_else(|_| Utc::now().timestamp());
        let sql = format!(
            "INSERT INTO {table} \
             (chunk_id, document_id, version_id, title, body, url, language, workspace_id, fetched_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?) \
             ON DUPLICATE KEY UPDATE \
             document_id = VALUES(document_id), version_id = VALUES(version_id), \
             title = VALUES(title), body = VALUES(body), url = VALUES(url), \
             language = VALUES(language), workspace_id = VALUES(workspace_id), \
             fetched_at = VALUES(fetched_at)",
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
            .map_err(|e| SearchOsError::Storage(format!("mysql upsert: {e}")))?;
        Ok(())
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        let boolean = Self::boolean_query(&query.text);
        if boolean.is_empty() {
            return Ok(Vec::new());
        }
        let sql = format!(
            "SELECT chunk_id, document_id, version_id, title, body, url, language, \
             workspace_id, FROM_UNIXTIME(fetched_at) AS fetched_at, \
             MATCH(title, body) AGAINST (? IN BOOLEAN MODE) AS score \
             FROM {table} WHERE MATCH(title, body) AGAINST (? IN BOOLEAN MODE) \
             AND (workspace_id = ? OR workspace_id = '*') \
             AND (? IS NULL OR language = ?) \
             AND (? IS NULL OR fetched_at >= ?) \
             ORDER BY score DESC LIMIT ?",
            table = self.table
        );
        let mut q = sqlx::query(sql.as_str().assert_safe())
            .bind(&boolean)
            .bind(&boolean)
            .bind(query.workspace_id.clone().unwrap_or_default());
        q = q.bind(query.language.clone());
        q = q.bind(query.language.clone());
        q = q.bind(
            query
                .fetched_after
                .filter(|_| !query.include_stale)
                .map(|d| d.timestamp()),
        );
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
            .map_err(|e| SearchOsError::Retrieval(format!("mysql search: {e}")))?;
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
        let sql = format!("DELETE FROM {} WHERE document_id = ?", self.table);
        sqlx::query(sql.as_str().assert_safe())
            .bind(document_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("mysql delete: {e}")))?;
        Ok(())
    }

    async fn delete_by_chunk_ids(&self, chunk_ids: &[ChunkId]) -> Result<(), SearchOsError> {
        let sql = format!("DELETE FROM {} WHERE chunk_id = ?", self.table);
        for id in chunk_ids {
            sqlx::query(sql.as_str().assert_safe())
                .bind(id.as_str())
                .execute(&self.pool)
                .await
                .map_err(|e| SearchOsError::Storage(format!("mysql chunk delete: {e}")))?;
        }
        Ok(())
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        let sql = format!("SELECT COUNT(*) AS n FROM {}", self.table);
        let row = sqlx::query(sql.as_str().assert_safe())
            .fetch_one(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("mysql count: {e}")))?;
        Ok(row.try_get::<i64, _>("n").unwrap_or(0) as u64)
    }

    async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        let sql = format!("TRUNCATE TABLE {}", self.table);
        sqlx::query(sql.as_str().assert_safe())
            .execute(&self.pool)
            .await
            .map_err(|e| SearchOsError::Storage(format!("mysql truncate: {e}")))?;
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
    fn boolean_query_prefixes_terms() {
        assert_eq!(
            MySqlFtsIndex::boolean_query("tokio runtime"),
            "+tokio +runtime"
        );
        assert_eq!(MySqlFtsIndex::boolean_query("  rust   web  "), "+rust +web");
        assert!(MySqlFtsIndex::boolean_query("   ").is_empty());
    }
}
