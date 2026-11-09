//! Tantivy full-text index adapter (ADR-008).
//!
//! Tantivy 是可重建全文索引 — SQLite stays the metadata source of truth;
//! this index can be dropped and rebuilt from document chunks (ADR-022).
//! Index writes flow through the transactional outbox; this adapter also
//! serves direct BM25 queries for the retrieval layer.

use async_trait::async_trait;
use searchos_domain::retrieval::{
    IndexedDocument, RetrievalCandidate, RetrievalQuery, RetrievalSource,
};
use searchos_domain::{ChunkId, DocumentId, DocumentVersionId, SearchOsError};
use searchos_retrieval::SearchIndex;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, RwLock};
use tantivy::collector::TopDocs;
use tantivy::query::{BooleanQuery, Query, QueryParser, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, STORED};
use tantivy::{doc, Index, IndexWriter, Term};

/// Tantivy-backed BM25 index.
pub struct TantivyIndex {
    index: Index,
    writer: Arc<tokio::sync::Mutex<IndexWriter>>,
    f_chunk_id: Field,
    f_document_id: Field,
    f_version_id: Field,
    f_title: Field,
    f_body: Field,
    f_url: Field,
    f_language: Field,
    f_workspace: Field,
    f_fetched_at: Field,
    index_version: Arc<AtomicU64>,
    count: Arc<RwLock<u64>>,
}

impl TantivyIndex {
    /// Opens (creating) an index at a directory.
    pub fn open(path: &Path) -> Result<Self, SearchOsError> {
        let schema = Self::build_schema();
        std::fs::create_dir_all(path)
            .map_err(|e| SearchOsError::Storage(format!("mkdir index: {e}")))?;
        let index = if path
            .read_dir()
            .map(|mut d| d.next().is_some())
            .unwrap_or(false)
        {
            Index::open_in_dir(path)
                .map_err(|e| SearchOsError::Storage(format!("open index: {e}")))?
        } else {
            Index::create_in_dir(path, schema.clone())
                .map_err(|e| SearchOsError::Storage(format!("create index: {e}")))?
        };
        Self::from_index(index, schema)
    }

    /// In-memory index (tests).
    pub fn in_memory() -> Result<Self, SearchOsError> {
        let schema = Self::build_schema();
        let index = Index::create_in_ram(schema.clone());
        Self::from_index(index, schema)
    }

    fn from_index(index: Index, schema: Schema) -> Result<Self, SearchOsError> {
        let writer = index
            .writer(15_000_000)
            .map_err(|e| SearchOsError::Storage(format!("index writer: {e}")))?;
        let get_field = |name: &str| -> Result<Field, SearchOsError> {
            schema
                .get_field(name)
                .map_err(|e| SearchOsError::Internal(format!("field {name}: {e}")))
        };
        let f_chunk_id = get_field("chunk_id")?;
        let f_document_id = get_field("document_id")?;
        let f_version_id = get_field("version_id")?;
        let f_title = get_field("title")?;
        let f_body = get_field("body")?;
        let f_url = get_field("url")?;
        let f_language = get_field("language")?;
        let f_workspace = get_field("workspace_id")?;
        let f_fetched_at = get_field("fetched_at")?;
        let idx = Self {
            index,
            writer: Arc::new(tokio::sync::Mutex::new(writer)),
            f_chunk_id,
            f_document_id,
            f_version_id,
            f_title,
            f_body,
            f_url,
            f_language,
            f_workspace,
            f_fetched_at,
            index_version: Arc::new(AtomicU64::new(1)),
            count: Arc::new(RwLock::new(0)),
        };
        Ok(idx)
    }

    fn build_schema() -> Schema {
        let mut sb = Schema::builder();
        let text_index = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("en_stem")
                    .set_fieldnorms(true)
                    .set_index_option(IndexRecordOption::WithFreqsAndPositions),
            )
            .set_stored();
        // Indexed raw text AND stored (tantivy's bare `STRING` is
        // indexed-only, not stored — keyword metadata must be readable back).
        let keyword = TextOptions::default()
            .set_indexing_options(
                TextFieldIndexing::default()
                    .set_tokenizer("raw")
                    .set_index_option(IndexRecordOption::Basic),
            )
            .set_stored();
        sb.add_text_field("chunk_id", keyword.clone());
        sb.add_text_field("document_id", keyword.clone());
        sb.add_text_field("version_id", keyword.clone());
        sb.add_text_field("title", text_index.clone());
        sb.add_text_field("body", text_index);
        sb.add_text_field("url", keyword.clone());
        sb.add_text_field("language", keyword.clone());
        sb.add_text_field("workspace_id", keyword);
        sb.add_date_field("fetched_at", STORED);
        sb.build()
    }

    /// BM25 query parse + filter composition.
    fn build_query(&self, query: &RetrievalQuery) -> Result<Box<dyn Query>, SearchOsError> {
        let parser = QueryParser::for_index(&self.index, vec![self.f_title, self.f_body]);
        let user_q = parser
            .parse_query(&query.text)
            .map_err(|e| SearchOsError::Retrieval(format!("query parse: {e}")))?;
        let mut clauses: Vec<(tantivy::query::Occur, Box<dyn Query>)> =
            vec![(tantivy::query::Occur::Must, Box::new(user_q))];
        // Workspace scope: the requesting workspace OR the global `*`
        // namespace (RFC-013 8.7). Other workspaces are never included, so
        // requester isolation is preserved.
        if let Some(ws) = &query.workspace_id {
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(BooleanQuery::new(vec![
                    (
                        tantivy::query::Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(self.f_workspace, ws),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    ),
                    (
                        tantivy::query::Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(self.f_workspace, "*"),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    ),
                ])),
            ));
        }
        if let Some(lang) = &query.language {
            clauses.push((
                tantivy::query::Occur::Must,
                Box::new(tantivy::query::TermQuery::new(
                    Term::from_field_text(self.f_language, lang),
                    IndexRecordOption::Basic,
                )),
            ));
        }
        Ok(Box::new(BooleanQuery::new(clauses)))
    }

    /// Commits pending writes and refreshes readers (explicit consistency).
    pub async fn commit(&self) -> Result<(), SearchOsError> {
        self.commit_impl().await
    }

    async fn commit_impl(&self) -> Result<(), SearchOsError> {
        let mut writer = self.writer.lock().await;
        writer
            .commit()
            .map_err(|e| SearchOsError::Storage(format!("commit: {e}")))?;
        drop(writer);
        self.index
            .reader()
            .map_err(|e| SearchOsError::Storage(format!("reader: {e}")))?
            .reload()
            .map_err(|e| SearchOsError::Storage(format!("reload: {e}")))?;
        Ok(())
    }

    /// Drops and recreates from chunk payloads (ADR-022 rebuildable).
    pub async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        self.rebuild_from_impl(documents).await
    }

    async fn rebuild_from_impl(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        {
            let mut writer = self.writer.lock().await;
            writer
                .delete_all_documents()
                .map_err(|e| SearchOsError::Storage(e.to_string()))?;
            writer
                .commit()
                .map_err(|e| SearchOsError::Storage(e.to_string()))?;
            *self.count.write().unwrap() = 0;
        }
        for d in documents {
            self.index(d.clone()).await?;
        }
        self.commit().await?;
        self.bump_version();
        Ok(self.num_docs())
    }

    /// Current index version (invalidates retrieval caches on change).
    pub fn version(&self) -> u64 {
        self.index_version.load(Ordering::SeqCst)
    }

    /// Authoritative committed doc count from the tantivy searcher.
    /// Restarts must not report false index drift (RFC-018 18.4).
    pub fn num_docs(&self) -> u64 {
        self.index
            .reader()
            .map(|r| r.searcher().num_docs())
            .unwrap_or(0)
    }

    /// Raw index version counter.
    pub fn index_version(&self) -> u64 {
        self.index_version.load(Ordering::SeqCst)
    }

    /// Bumps after bulk changes.
    pub fn bump_version(&self) {
        self.index_version.fetch_add(1, Ordering::SeqCst);
    }

    /// Resets the counter from the authoritative committed searcher.
    fn refresh_count(&self) -> Result<(), SearchOsError> {
        self.index
            .reader()
            .map_err(|e| SearchOsError::Storage(format!("reader: {e}")))?
            .reload()
            .map_err(|e| SearchOsError::Storage(format!("reload: {e}")))?;
        *self.count.write().unwrap() = self.num_docs();
        Ok(())
    }
}

#[async_trait]
impl SearchIndex for TantivyIndex {
    async fn index(&self, document: IndexedDocument) -> Result<(), SearchOsError> {
        let fetched =
            tantivy::DateTime::from_timestamp_millis(document.fetched_at.timestamp_millis());
        let writer = self.writer.lock().await;
        writer
            .add_document(doc!(
                self.f_chunk_id => document.chunk_id.as_str(),
                self.f_document_id => document.document_id.as_str(),
                self.f_version_id => document.version_id.as_str(),
                self.f_title => document.title.clone().unwrap_or_default(),
                self.f_body => document.body,
                self.f_url => document.url,
                self.f_language => document.language.clone().unwrap_or_default(),
                self.f_workspace => document.workspace_id,
                self.f_fetched_at => fetched,
            ))
            .map_err(|e| SearchOsError::Storage(format!("add_document: {e}")))?;
        *self.count.write().unwrap() += 1;
        Ok(())
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        self.commit().await?;
        let reader = self
            .index
            .reader()
            .map_err(|e| SearchOsError::Storage(format!("reader: {e}")))?;
        reader
            .reload()
            .map_err(|e| SearchOsError::Storage(format!("reload: {e}")))?;
        let searcher = reader.searcher();
        let q = self.build_query(query)?;
        let top = TopDocs::with_limit(query.top_k.max(1)).order_by_score();
        let hits = searcher
            .search(&q, &top)
            .map_err(|e| SearchOsError::Retrieval(format!("search: {e}")))?;

        let mut out = Vec::with_capacity(hits.len());
        for (score, addr) in hits {
            let doc: tantivy::TantivyDocument = searcher
                .doc(addr)
                .map_err(|e| SearchOsError::Retrieval(format!("doc fetch: {e}")))?;
            let get = |f: Field| -> Option<String> {
                let owned: tantivy::schema::document::OwnedValue = doc.get_first(f)?.into();
                match owned {
                    tantivy::schema::document::OwnedValue::Str(s) => Some(s),
                    _ => None,
                }
            };
            let fetched_at = doc
                .get_first(self.f_fetched_at)
                .and_then(|v| -> Option<i64> {
                    let owned: tantivy::schema::document::OwnedValue = v.into();
                    match owned {
                        tantivy::schema::document::OwnedValue::Date(d) => {
                            Some(d.into_utc().unix_timestamp())
                        }
                        _ => None,
                    }
                })
                .unwrap_or_else(|| chrono::Utc::now().timestamp());
            out.push(RetrievalCandidate {
                chunk_id: ChunkId::new(get(self.f_chunk_id).unwrap_or_default()),
                document_id: DocumentId::new(get(self.f_document_id).unwrap_or_default()),
                version_id: DocumentVersionId::new(get(self.f_version_id).unwrap_or_default()),
                title: get(self.f_title).filter(|s| !s.is_empty()),
                snippet: get(self.f_body)
                    .unwrap_or_default()
                    .chars()
                    .take(400)
                    .collect(),
                url: get(self.f_url).unwrap_or_default(),
                score,
                source: RetrievalSource::Bm25,
                fetched_at: chrono::DateTime::from_timestamp_secs(fetched_at).unwrap_or_default(),
                vector_score: None,
                bm25_score: Some(score),
                workspace_id: get(self.f_workspace),
                language: get(self.f_language),
            });
        }
        Ok(out)
    }

    async fn delete(&self, document_id: &DocumentId) -> Result<(), SearchOsError> {
        let mut writer = self.writer.lock().await;
        writer.delete_term(Term::from_field_text(
            self.f_document_id,
            document_id.as_str(),
        ));
        writer
            .commit()
            .map_err(|e| SearchOsError::Storage(e.to_string()))?;
        drop(writer);
        self.refresh_count()?;
        self.bump_version();
        Ok(())
    }

    async fn delete_by_chunk_ids(&self, chunk_ids: &[ChunkId]) -> Result<(), SearchOsError> {
        let mut writer = self.writer.lock().await;
        for id in chunk_ids {
            writer.delete_term(Term::from_field_text(self.f_chunk_id, id.as_str()));
        }
        writer
            .commit()
            .map_err(|e| SearchOsError::Storage(e.to_string()))?;
        drop(writer);
        self.refresh_count()?;
        self.bump_version();
        Ok(())
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        Ok(self.num_docs())
    }

    async fn commit(&self) -> Result<(), SearchOsError> {
        self.commit_impl().await
    }

    async fn rebuild_from(&self, documents: &[IndexedDocument]) -> Result<u64, SearchOsError> {
        self.rebuild_from_impl(documents).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn doc(chunk: &str, body: &str) -> IndexedDocument {
        IndexedDocument {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new("doc_1"),
            version_id: DocumentVersionId::new("docv_1"),
            title: Some("Tokio tutorial".into()),
            body: body.into(),
            url: "https://tokio.rs/tokio/tutorial".into(),
            language: Some("en".into()),
            workspace_id: "ws_test".into(),
            fetched_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn index_and_search_bm25() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc(
            "c1",
            "Tokio is an asynchronous runtime for the Rust language",
        ))
        .await
        .unwrap();
        idx.index(doc(
            "c2",
            "Axum is a web application framework on top of tokio",
        ))
        .await
        .unwrap();
        idx.commit().await.unwrap();

        let q = RetrievalQuery::new("tokio async runtime", 5);
        let hits = idx.search(&q).await.unwrap();
        assert!(!hits.is_empty());
        assert!(hits[0].bm25_score.is_some());
        assert_eq!(hits[0].source, RetrievalSource::Bm25);
        // The runtime chunk should outrank the plain-framework chunk.
        assert!(
            hits[0].snippet.contains("runtime"),
            "top hit: {}",
            hits[0].snippet
        );
    }

    #[tokio::test]
    async fn workspace_filter_applies() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc("c1", "rust web framework content"))
            .await
            .unwrap();
        idx.commit().await.unwrap();
        let mut q = RetrievalQuery::new("rust web framework", 5);
        q.workspace_id = Some("ws_other".into());
        let hits = idx.search(&q).await.unwrap();
        assert!(
            hits.is_empty(),
            "workspace filter excludes other workspaces"
        );
    }

    #[tokio::test]
    async fn delete_document_removes_hits() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc("c1", "special searchable term zanzibar"))
            .await
            .unwrap();
        idx.commit().await.unwrap();
        let q = RetrievalQuery::new("zanzibar", 5);
        assert!(!idx.search(&q).await.unwrap().is_empty());
        idx.delete(&DocumentId::new("doc_1")).await.unwrap();
        assert!(idx.search(&q).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn delete_by_chunk_ids_removes_hits() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc("c1", "special searchable term zanzibar"))
            .await
            .unwrap();
        idx.index(doc("c2", "other content")).await.unwrap();
        idx.commit().await.unwrap();
        let q = RetrievalQuery::new("zanzibar", 5);
        assert!(!idx.search(&q).await.unwrap().is_empty());
        idx.delete_by_chunk_ids(&[ChunkId::new("c1")])
            .await
            .unwrap();
        assert!(idx.search(&q).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rebuild_from_documents() {
        let idx = TantivyIndex::in_memory().unwrap();
        let docs = vec![doc("c1", "alpha content"), doc("c2", "beta content")];
        let n = idx.rebuild_from(&docs).await.unwrap();
        assert_eq!(n, 2);
        let hits = idx.search(&RetrievalQuery::new("beta", 5)).await.unwrap();
        assert_eq!(hits.len(), 1);
        assert!(idx.version() >= 2, "rebuild bumps index version");
    }
}
