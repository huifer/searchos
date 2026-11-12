//! Tantivy full-text index adapter (ADR-008).
//!
//! Tantivy 是可重建全文索引 — SQLite stays the metadata source of truth;
//! this index can be dropped and rebuilt from document chunks (ADR-022).
//! Index writes flow through the transactional outbox; this adapter also
//! serves direct BM25 queries for the retrieval layer.
//!
//! User text is analyzed with the SearchOS analyzer (see [`analyzer`]): CJK
//! runs become character bigrams, Latin runs are stemmed. Queries are built
//! as literal analyzer terms — tantivy query syntax is never interpreted
//! from user input, so a URL or a stray `:` in a query can never fail the
//! request.

pub mod analyzer;

use analyzer::{register_analyzers, SEARCHOS_TOKENIZER};
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
use tantivy::query::{BooleanQuery, BoostQuery, FuzzyTermQuery, Occur, Query, TermQuery};
use tantivy::schema::{Field, IndexRecordOption, Schema, TextFieldIndexing, TextOptions, STORED};
use tantivy::tokenizer::TextAnalyzer;
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
        // Register the SearchOS analyzer (CJK bigrams + English stemming)
        // before any writer/searcher is created. Also overrides the legacy
        // `en_stem` name referenced by pre-CJK index schemas, so writes,
        // queries and a plain `rebuild_from` all use the new analysis.
        register_analyzers(&index);
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
                    .set_tokenizer(SEARCHOS_TOKENIZER)
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

    /// Resolves the text analyzer the stored schema actually references, so
    /// index and query analysis always agree (legacy indexes override the
    /// `en_stem` name; fresh ones use [`analyzer::SEARCHOS_TOKENIZER`]).
    fn body_analyzer(&self) -> Result<TextAnalyzer, SearchOsError> {
        let schema = self.index.schema();
        let name = match schema.get_field_entry(self.f_body).field_type() {
            tantivy::schema::FieldType::Str(options) => options
                .get_indexing_options()
                .map(|o| o.tokenizer().to_string())
                .unwrap_or_default()
                .to_string(),
            _ => SEARCHOS_TOKENIZER.to_string(),
        };
        self.index
            .tokenizers()
            .get(&name)
            .ok_or_else(|| SearchOsError::Retrieval(format!("analyzer {name} not registered")))
    }

    /// Extracts literal query terms with the index analyzer.
    ///
    /// Tantivy query syntax is never interpreted from user input: a URL, a
    /// colon, an unclosed quote or a trailing `AND` used to fail the whole
    /// search (`Field does not exist: 'https'` → HTTP 500). Literal analyzer
    /// terms also match the remote-backend contract — the Elasticsearch
    /// adapter sends `multi_match`, which analyzes the raw text too.
    fn query_terms(&self, text: &str) -> Result<Vec<String>, SearchOsError> {
        let mut analyzer = self.body_analyzer()?;
        let mut stream = analyzer.token_stream(text);
        let mut terms: Vec<String> = Vec::new();
        while let Some(token) = stream.next() {
            let term = token.text.clone();
            if !terms.contains(&term) {
                terms.push(term);
            }
            if terms.len() >= analyzer::MAX_QUERY_TERMS {
                break;
            }
        }
        Ok(terms)
    }

    /// Typo-tolerant fallback query: edit-distance-1 OR across title+body
    /// for long ASCII terms only. `None` when no term qualifies (then the
    /// caller keeps the empty exact result — honest zero, not noise).
    fn build_fuzzy_query(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Option<Box<dyn Query>>, SearchOsError> {
        let terms = self.query_terms(&query.text)?;
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::new();
        for term in &terms {
            let is_long_ascii = term.is_ascii() && term.chars().count() >= 4;
            if !is_long_ascii {
                continue;
            }
            clauses.push((
                Occur::Should,
                Box::new(FuzzyTermQuery::new(
                    Term::from_field_text(self.f_body, term),
                    1,
                    true,
                )),
            ));
            clauses.push((
                Occur::Should,
                Box::new(FuzzyTermQuery::new(
                    Term::from_field_text(self.f_title, term),
                    1,
                    true,
                )),
            ));
        }
        if clauses.is_empty() {
            return Ok(None);
        }
        let terms_query = BooleanQuery::new(clauses);
        let mut top: Vec<(Occur, Box<dyn Query>)> = vec![(Occur::Must, Box::new(terms_query))];
        if let Some(ws) = &query.workspace_id {
            top.push((
                Occur::Must,
                Box::new(BooleanQuery::new(vec![
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(self.f_workspace, ws),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    ),
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(self.f_workspace, "*"),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    ),
                ])),
            ));
        }
        Ok(Some(Box::new(BooleanQuery::new(top))))
    }

    /// Builds the query: literal analyzer terms OR'd across title+body,
    /// scoped to the requesting workspace (plus the global `*` namespace,
    /// RFC-013 8.7) and the requested language. Other workspaces are never
    /// included, so requester isolation is preserved.
    fn build_query(&self, query: &RetrievalQuery) -> Result<Box<dyn Query>, SearchOsError> {
        let terms = self.query_terms(&query.text)?;
        if terms.is_empty() {
            // Nothing analyzable (punctuation-only or empty query): match
            // nothing rather than the whole workspace.
            return Ok(Box::new(tantivy::query::EmptyQuery));
        }
        let mut clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(3);
        // The user terms form their own OR-only BooleanQuery: a Should
        // clause alongside a Must clause would be optional (scoring only),
        // so it must be nested under Must to actually filter.
        // Field-weighted ranking (BM25F-lite): title terms carry an explicit
        // boost so a title hit outranks the same terms buried in the body.
        // Aligned with the Elasticsearch adapter's `title^2`.
        let title_boost = if query.title_boost > 0.0 {
            query.title_boost
        } else {
            2.0
        };
        let mut user_clauses: Vec<(Occur, Box<dyn Query>)> = Vec::with_capacity(terms.len() * 2);
        for term in &terms {
            user_clauses.push((
                Occur::Should,
                Box::new(BoostQuery::new(
                    Box::new(TermQuery::new(
                        Term::from_field_text(self.f_title, term),
                        IndexRecordOption::WithFreqsAndPositions,
                    )),
                    title_boost,
                )),
            ));
            user_clauses.push((
                Occur::Should,
                Box::new(TermQuery::new(
                    Term::from_field_text(self.f_body, term),
                    IndexRecordOption::WithFreqsAndPositions,
                )),
            ));
        }
        clauses.push((Occur::Must, Box::new(BooleanQuery::new(user_clauses))));
        if let Some(ws) = &query.workspace_id {
            clauses.push((
                Occur::Must,
                Box::new(BooleanQuery::new(vec![
                    (
                        Occur::Should,
                        Box::new(TermQuery::new(
                            Term::from_field_text(self.f_workspace, ws),
                            IndexRecordOption::Basic,
                        )) as Box<dyn Query>,
                    ),
                    (
                        Occur::Should,
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
                Occur::Must,
                Box::new(TermQuery::new(
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

    /// Whether the index's stored schema uses the current SearchOS analyzer.
    /// `false` for indexes created before CJK analysis existed: they keep
    /// working (the analyzer is registered under the legacy `en_stem` name),
    /// but old CJK rows only become searchable after `searchos index rebuild`.
    pub fn analyzer_is_current(&self) -> bool {
        analyzer::schema_uses_current_analyzer(&self.index.schema(), self.f_body)
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
        let mut hits = searcher
            .search(&q, &top)
            .map_err(|e| SearchOsError::Retrieval(format!("search: {e}")))?;
        if hits.is_empty() {
            // Typo tolerance (RFC-007 recall): when no exact term matches,
            // retry once with edit-distance-1 terms. Only long ASCII terms
            // participate — short/CJK bigram terms would match noise.
            if let Some(fuzzy) = self.build_fuzzy_query(query)? {
                hits = searcher
                    .search(&fuzzy, &top)
                    .map_err(|e| SearchOsError::Retrieval(format!("search: fuzzy {e}")))?;
            }
        }

        // Query-matched snippet/highlight (RFC-007 7.1 result shaping): the
        // returned passage is the region around the MATCH, not the first 400
        // characters of the body. Best-effort — an analysis hiccup degrades
        // to the leading window, never fails the search.
        let mut snippet_gen =
            tantivy::snippet::SnippetGenerator::create(&searcher, q.as_ref(), self.f_body).ok();
        if let Some(gen) = snippet_gen.as_mut() {
            gen.set_max_num_chars(400);
        }

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
            let body = get(self.f_body).unwrap_or_default();
            let (snippet, highlight) = match snippet_gen.as_ref().map(|gen| gen.snippet(&body)) {
                Some(s) if !s.is_empty() => {
                    let marked = s.to_html();
                    let plain: String = marked.replace("<b>", "").replace("</b>", "");
                    (plain, Some(marked))
                }
                _ => (body.chars().take(400).collect(), None),
            };
            out.push(RetrievalCandidate {
                chunk_id: ChunkId::new(get(self.f_chunk_id).unwrap_or_default()),
                document_id: DocumentId::new(get(self.f_document_id).unwrap_or_default()),
                version_id: DocumentVersionId::new(get(self.f_version_id).unwrap_or_default()),
                title: get(self.f_title).filter(|s| !s.is_empty()),
                snippet,
                url: get(self.f_url).unwrap_or_default(),
                score,
                source: RetrievalSource::Bm25,
                fetched_at: chrono::DateTime::from_timestamp_secs(fetched_at).unwrap_or_default(),
                vector_score: None,
                bm25_score: Some(score),
                workspace_id: get(self.f_workspace),
                language: get(self.f_language),
                highlight,
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

    fn doc_titled(chunk: &str, title: &str, body: &str) -> IndexedDocument {
        IndexedDocument {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new(format!("doc_{chunk}")),
            version_id: DocumentVersionId::new(format!("docv_{chunk}")),
            title: Some(title.into()),
            body: body.into(),
            url: "https://example.com".into(),
            language: Some("en".into()),
            workspace_id: "ws_test".into(),
            fetched_at: Utc::now(),
        }
    }

    fn zh_doc(chunk: &str, title: &str, body: &str) -> IndexedDocument {
        IndexedDocument {
            chunk_id: ChunkId::new(chunk),
            document_id: DocumentId::new("doc_zh"),
            version_id: DocumentVersionId::new("docv_zh"),
            title: Some(title.into()),
            body: body.into(),
            url: "searchos-local://ws/zh".into(),
            language: Some("zh".into()),
            workspace_id: "ws_test".into(),
            fetched_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn cjk_content_is_retrievable() {
        // Regression: the old `en_stem` chain tokenized this sentence body to
        // ZERO terms (RemoveLongFilter counts bytes; 3 bytes/char), so every
        // Chinese query returned `insufficient_evidence`.
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(zh_doc(
            "zh1",
            "本地优先检索引擎",
            "这是关于本地优先检索引擎的详细说明。系统会先在本地知识库解析问题。",
        ))
        .await
        .unwrap();
        idx.commit().await.unwrap();

        for q in ["本地优先检索引擎", "检索引擎", "知识库解析", "本地知识库"]
        {
            let hits = idx.search(&RetrievalQuery::new(q, 5)).await.unwrap();
            assert!(!hits.is_empty(), "CJK query {q:?} must match");
            assert!(hits[0].bm25_score.is_some());
        }
    }

    #[tokio::test]
    async fn cjk_subphrase_matches_with_lower_score_than_full_phrase() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(zh_doc(
            "zh1",
            "本地优先检索引擎",
            "这是关于本地优先检索引擎的详细说明",
        ))
        .await
        .unwrap();
        idx.index(zh_doc("zh2", "检索", "其他检索相关的独立内容"))
            .await
            .unwrap();
        idx.commit().await.unwrap();
        let hits = idx
            .search(&RetrievalQuery::new("本地优先检索引擎", 5))
            .await
            .unwrap();
        // The doc containing the full phrase outranks the partial match.
        assert_eq!(hits[0].chunk_id.as_str(), "zh1");
    }

    #[tokio::test]
    async fn query_with_url_does_not_fail_and_still_matches() {
        // Regression: `QueryParser` interpreted `https:` as a field selector
        // and the whole search failed with `Field does not exist: 'https'`.
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc("c1", "see the tokio runtime docs for rust async"))
            .await
            .unwrap();
        idx.commit().await.unwrap();
        let hits = idx
            .search(&RetrievalQuery::new(
                "see https://tokio.rs/tokio for rust",
                5,
            ))
            .await
            .unwrap();
        assert!(!hits.is_empty(), "URL-bearing query must degrade to terms");
    }

    #[tokio::test]
    async fn malformed_query_syntax_never_fails_the_search() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc("c1", "rust is a programming language"))
            .await
            .unwrap();
        idx.commit().await.unwrap();
        for raw in [
            "rust \"unclosed",
            "rust AND",
            "rust OR",
            "field:x rust",
            "rust -rust",
            "!!!",
            "",
        ] {
            let result = idx.search(&RetrievalQuery::new(raw, 5)).await;
            assert!(
                result.is_ok(),
                "query {raw:?} must not error: {:?}",
                result.err()
            );
        }
        // Punctuation-only query matches nothing (not the whole workspace).
        let hits = idx.search(&RetrievalQuery::new("!!!", 5)).await.unwrap();
        assert!(hits.is_empty());
    }

    #[tokio::test]
    async fn title_hit_outranks_body_hit() {
        // BM25F-lite: identical query terms; the title match must win.
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc("in_body", "rust tokio runtime"))
            .await
            .unwrap();
        idx.index(doc_titled(
            "in_title",
            "rust tokio runtime",
            "unrelated body",
        ))
        .await
        .unwrap();
        idx.commit().await.unwrap();
        let hits = idx
            .search(&RetrievalQuery::new("rust tokio runtime", 5))
            .await
            .unwrap();
        assert_eq!(hits[0].chunk_id.as_str(), "in_title");
    }

    #[tokio::test]
    async fn typo_query_falls_back_to_edit_distance_one() {
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(doc(
            "c1",
            "tokio provides an asynchronous runtime for rust applications",
        ))
        .await
        .unwrap();
        idx.commit().await.unwrap();
        // Both terms misspelled ("asynchronus" / "runtme"): the exact pass
        // finds nothing, and the built-in fuzzy fallback recovers the
        // document. No spelling-corrected "did you mean" round trip needed.
        let fuzzy = idx
            .search(&RetrievalQuery::new("asynchronus runtme", 5))
            .await
            .unwrap();
        assert_eq!(fuzzy.len(), 1, "one-char typos must still match");
        assert_eq!(fuzzy[0].chunk_id.as_str(), "c1");
    }

    #[tokio::test]
    async fn snippet_is_query_matched_with_highlight() {
        let idx = TantivyIndex::in_memory().unwrap();
        let filler = "filler ".repeat(80);
        idx.index(doc_titled(
            "s1",
            "needle doc",
            &format!("{filler}NEEDLE {filler}second {filler}"),
        ))
        .await
        .unwrap();
        idx.commit().await.unwrap();
        let hits = idx.search(&RetrievalQuery::new("needle", 5)).await.unwrap();
        assert_eq!(hits.len(), 1);
        // Regression: the old snippet was the leading 400 chars and did not
        // contain the match at all.
        assert!(
            hits[0].snippet.contains("NEEDLE"),
            "snippet must contain the matched term: {}",
            hits[0].snippet
        );
        assert!(hits[0].highlight.is_some());
        let marked = hits[0].highlight.as_ref().unwrap();
        assert!(
            marked.contains("<b>"),
            "highlight carries markers: {marked}"
        );
        assert!(
            marked.to_lowercase().contains("<b>needle</b>"),
            "got {marked}"
        );
        // The plain snippet is the same passage with markers removed.
        assert_eq!(
            hits[0].snippet,
            marked.replace("<b>", "").replace("</b>", "")
        );
    }

    #[tokio::test]
    async fn unrelated_documents_do_not_match_cjk_queries() {
        // Regression: Should terms nested beside a Must clause are optional
        // in tantivy (scoring only), so the workspace filter alone matched
        // every document regardless of terms. The term query must be its
        // own Must-wrapped OR block.
        let idx = TantivyIndex::in_memory().unwrap();
        idx.index(zh_doc(
            "zh1",
            "本地优先检索引擎",
            "这是关于本地优先检索引擎的详细说明",
        ))
        .await
        .unwrap();
        idx.index(doc("en1", "tokio and axum are rust frameworks"))
            .await
            .unwrap();
        idx.commit().await.unwrap();

        let hits = idx
            .search(&RetrievalQuery::new("熊猫篮球比赛", 5))
            .await
            .unwrap();
        assert!(hits.is_empty(), "nonsense query must not match: {hits:?}");

        let hits = idx
            .search(&RetrievalQuery::new("本地优先检索引擎", 5))
            .await
            .unwrap();
        assert_eq!(hits.len(), 1, "only the CJK doc matches");
        assert_eq!(hits[0].chunk_id.as_str(), "zh1");
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
