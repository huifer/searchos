//! LanceDB vector index adapter (ADR-009).
//!
//! LanceDB 是可重建向量索引; embeddings are recorded with model id/version/
//! dimension/normalization/chunk hash (RFC-007 7.6). Model upgrades may run
//! dual versions in parallel before migration.
//!
//! MVP embedding: the deterministic hashing embedder
//! (`searchos-provider::embedding::HashingEmbedder`); remote model providers
//! plug in via [`EmbeddingProvider`].

use async_trait::async_trait;
use chrono::Utc;
use futures::StreamExt;
use lancedb::arrow::arrow::datatypes::Float32Type;
use lancedb::arrow::arrow::record_batch::RecordBatch;
use lancedb::arrow::arrow::record_batch::RecordBatchIterator;
use lancedb::arrow::arrow_array::{
    Array, FixedSizeListArray, Float32Array, StringArray, UInt64Array,
};
use lancedb::arrow::arrow_schema::{DataType, Field as ArrowField, Schema as ArrowSchema};
use lancedb::query::{ExecutableQuery, QueryBase};
use searchos_domain::retrieval::{RetrievalCandidate, RetrievalQuery, RetrievalSource};
use searchos_domain::{ChunkId, DocumentId, DocumentVersionId, SearchOsError};
use searchos_provider::embedding::{cosine_similarity, EmbeddingProvider, HashingEmbedder};
use searchos_retrieval::VectorIndex;
use std::path::Path;
use std::sync::Arc;

/// LanceDB-backed vector index.
pub struct LanceVectorIndex {
    db: lancedb::connection::Connection,
    table_name: String,
    embedder: Arc<dyn EmbeddingProvider>,
}

impl LanceVectorIndex {
    /// Opens (creating) a vector DB at `path` with table `table_name`.
    pub async fn open(path: &Path, table_name: &str) -> Result<Self, SearchOsError> {
        std::fs::create_dir_all(path)
            .map_err(|e| SearchOsError::Storage(format!("mkdir vectors: {e}")))?;
        let db = lancedb::connect(path.to_string_lossy().as_ref())
            .execute()
            .await
            .map_err(|e| SearchOsError::Storage(format!("lancedb connect: {e}")))?;
        Ok(Self {
            db,
            table_name: table_name.to_string(),
            embedder: Arc::new(HashingEmbedder::new(256)),
        })
    }

    /// Custom embedder (pluggable model provider).
    pub fn with_embedder(mut self, embedder: Arc<dyn EmbeddingProvider>) -> Self {
        self.embedder = embedder;
        self
    }

    fn arrow_schema() -> Arc<ArrowSchema> {
        Arc::new(ArrowSchema::new(vec![
            ArrowField::new("chunk_id", DataType::Utf8, false),
            ArrowField::new("document_id", DataType::Utf8, false),
            ArrowField::new("version_id", DataType::Utf8, false),
            ArrowField::new("title", DataType::Utf8, true),
            ArrowField::new("body", DataType::Utf8, false),
            ArrowField::new("url", DataType::Utf8, false),
            ArrowField::new("language", DataType::Utf8, true),
            ArrowField::new("workspace_id", DataType::Utf8, false),
            ArrowField::new("model_id", DataType::Utf8, false),
            ArrowField::new("model_version", DataType::Utf8, false),
            ArrowField::new("fetched_at", DataType::UInt64, false),
            ArrowField::new(
                "vector",
                DataType::new_fixed_size_list(DataType::Float32, 256, true),
                false,
            ),
        ]))
    }

    async fn table(&self) -> Result<lancedb::table::Table, SearchOsError> {
        let names = self
            .db
            .table_names()
            .execute()
            .await
            .map_err(|e| SearchOsError::Storage(format!("table names: {e}")))?;
        if names.iter().any(|n| n.as_str() == self.table_name) {
            self.db
                .open_table(&self.table_name)
                .execute()
                .await
                .map_err(|e| SearchOsError::Storage(format!("open table: {e}")))
        } else {
            self.db
                .create_empty_table(&self.table_name, Self::arrow_schema())
                .execute()
                .await
                .map_err(|e| SearchOsError::Storage(format!("create table: {e}")))
        }
    }

    fn batch_from(documents: &[IndexedVec], schema: &Arc<ArrowSchema>) -> RecordBatch {
        let dim = documents.first().map(|d| d.vector.len()).unwrap_or(256);
        let mut flat = Vec::with_capacity(documents.len() * dim);
        for d in documents {
            flat.extend_from_slice(&d.vector);
        }
        let _n = documents.len();
        let mut builder_counter = 0usize;
        let _ = &mut builder_counter;
        RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.chunk_id.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.document_id.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.version_id.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.title.clone().unwrap_or_default())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.body.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents.iter().map(|d| d.url.as_str()).collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.language.clone().unwrap_or_default())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.workspace_id.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.model_id.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(StringArray::from(
                    documents
                        .iter()
                        .map(|d| d.model_version.as_str())
                        .collect::<Vec<_>>(),
                )),
                Arc::new(UInt64Array::from(
                    documents.iter().map(|d| d.fetched_at).collect::<Vec<_>>(),
                )),
                Arc::new(
                    FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                        documents
                            .iter()
                            .map(|d| Some(d.vector.iter().map(|v| Some(*v)).collect::<Vec<_>>())),
                        dim as i32,
                    ),
                ),
            ],
        )
        .expect("record batch")
    }

    /// Embedding status tracking is recorded by the application layer via
    /// the `embeddings` table; here we only expose the embedder metadata.
    pub fn embedder_meta(&self) -> (&str, &str, usize) {
        (
            self.embedder.model_id(),
            self.embedder.model_version(),
            self.embedder.dimension(),
        )
    }

    /// Rebuilds by deleting everything and re-upserting (ADR-022).
    pub async fn rebuild_from(&self, documents: &[IndexedVec]) -> Result<u64, SearchOsError> {
        let table = self.table().await?;
        // Delete all rows (predicate matches everything).
        table
            .delete("true")
            .await
            .map_err(|e| SearchOsError::Storage(format!("delete all: {e}")))?;
        if !documents.is_empty() {
            let schema = Self::arrow_schema();
            let batch = Self::batch_from(documents, &schema);
            let stream = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
            let reader: Box<dyn lancedb::arrow::arrow::record_batch::RecordBatchReader + Send> =
                Box::new(stream);
            table
                .add(reader)
                .execute()
                .await
                .map_err(|e| SearchOsError::Storage(format!("add: {e}")))?;
        }
        Ok(documents.len() as u64)
    }
}

/// Single-element string array containing NULL (for nullable columns).
fn arrow_str_null() -> StringArray {
    StringArray::from(vec![None::<&str>])
}

/// A chunk with its embedding, ready for the vector store.
pub struct IndexedVec {
    pub chunk_id: String,
    pub document_id: String,
    pub version_id: String,
    pub title: Option<String>,
    pub body: String,
    pub url: String,
    pub language: Option<String>,
    pub workspace_id: String,
    pub model_id: String,
    pub model_version: String,
    pub fetched_at: u64,
    pub vector: Vec<f32>,
}

#[async_trait]
impl VectorIndex for LanceVectorIndex {
    async fn upsert(
        &self,
        chunk_id: &str,
        document_id: &str,
        vector: Vec<f32>,
        _model_id: &str,
    ) -> Result<(), SearchOsError> {
        let table = self.table().await?;
        // Remove stale rows for this chunk, then add.
        table
            .delete(&format!("chunk_id = '{chunk_id}'"))
            .await
            .map_err(|e| SearchOsError::Storage(format!("delete stale: {e}")))?;
        let schema = Self::arrow_schema();
        let dim = vector.len();
        let batch = RecordBatch::try_new(
            schema.clone(),
            vec![
                Arc::new(StringArray::from(vec![chunk_id])),
                Arc::new(StringArray::from(vec![document_id])),
                Arc::new(StringArray::from(vec![""])),
                Arc::new(arrow_str_null()) as Arc<dyn Array>,
                Arc::new(StringArray::from(vec![""])),
                Arc::new(StringArray::from(vec![""])),
                Arc::new(arrow_str_null()) as Arc<dyn Array>,
                Arc::new(StringArray::from(vec![""])),
                Arc::new(StringArray::from(vec![self.embedder.model_id()])),
                Arc::new(StringArray::from(vec![self.embedder.model_version()])),
                Arc::new(UInt64Array::from(vec![Utc::now().timestamp() as u64])),
                Arc::new(
                    FixedSizeListArray::from_iter_primitive::<Float32Type, _, _>(
                        vec![Some(vector.iter().map(|v| Some(*v)).collect::<Vec<_>>())],
                        dim as i32,
                    ),
                ),
            ],
        )
        .map_err(|e| SearchOsError::Storage(format!("batch: {e}")))?;
        let stream = RecordBatchIterator::new(vec![Ok(batch)].into_iter(), schema);
        let reader: Box<dyn lancedb::arrow::arrow::record_batch::RecordBatchReader + Send> =
            Box::new(stream);
        table
            .add(reader)
            .execute()
            .await
            .map_err(|e| SearchOsError::Storage(format!("add: {e}")))?;
        Ok(())
    }

    async fn search(
        &self,
        query: &RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>, SearchOsError> {
        let table = self.table().await?;
        // Embed the query text.
        let qvec = self
            .embedder
            .embed(std::slice::from_ref(&query.text))
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("embed query: {e}")))?
            .pop()
            .ok_or_else(|| SearchOsError::Retrieval("empty embedding".into()))?;

        let builder = table
            .query()
            .nearest_to(qvec.clone())
            .map_err(|e| SearchOsError::Retrieval(format!("nearest: {e}")))?;
        let builder = builder.limit(query.top_k.max(1));
        let batches = builder
            .execute()
            .await
            .map_err(|e| SearchOsError::Retrieval(format!("vector search: {e}")))?;
        let mut out = Vec::new();
        let mut stream = batches;
        while let Some(batch) = stream
            .next()
            .await
            .transpose()
            .map_err(|e| SearchOsError::Retrieval(format!("batch read: {e}")))?
        {
            let n = batch.num_rows();
            if n == 0 {
                continue;
            }
            let col = |name: &str| batch.column_by_name(name);
            let chunk_ids: &StringArray = col("chunk_id")
                .and_then(|c| c.as_any().downcast_ref())
                .unwrap();
            let document_ids: &StringArray = col("document_id")
                .and_then(|c| c.as_any().downcast_ref())
                .unwrap();
            let version_ids: &StringArray = col("version_id")
                .and_then(|c| c.as_any().downcast_ref())
                .unwrap();
            let titles: &StringArray = col("title")
                .and_then(|c| c.as_any().downcast_ref())
                .unwrap();
            let bodies: &StringArray = col("body").and_then(|c| c.as_any().downcast_ref()).unwrap();
            let urls: &StringArray = col("url").and_then(|c| c.as_any().downcast_ref()).unwrap();
            let fetched: &UInt64Array = col("fetched_at")
                .and_then(|c| c.as_any().downcast_ref())
                .unwrap();
            let vectors: Option<&FixedSizeListArray> =
                col("vector").and_then(|c| c.as_any().downcast_ref());
            for i in 0..n {
                let vec_score = vectors
                    .map(|l| {
                        let item = l.value(i);
                        let list = item
                            .as_any()
                            .downcast_ref::<Float32Array>()
                            .expect("vector list item");
                        let stored: Vec<f32> = (0..list.len()).map(|j| list.value(j)).collect();
                        cosine_similarity(&qvec, &stored)
                    })
                    .unwrap_or(0.0);
                let fetched_at = fetched.value(i);
                let title = titles.value(i);
                out.push(RetrievalCandidate {
                    chunk_id: ChunkId::new(chunk_ids.value(i).to_string()),
                    document_id: DocumentId::new(document_ids.value(i).to_string()),
                    version_id: DocumentVersionId::new(version_ids.value(i).to_string()),
                    title: if title.is_empty() {
                        None
                    } else {
                        Some(title.to_string())
                    },
                    snippet: bodies.value(i).chars().take(400).collect(),
                    url: urls.value(i).to_string(),
                    score: vec_score,
                    source: RetrievalSource::Vector,
                    fetched_at: chrono::DateTime::from_timestamp_secs(fetched_at as i64)
                        .unwrap_or_default(),
                    vector_score: Some(vec_score),
                    bm25_score: None,
                });
            }
        }
        // Domain/metadata filtering (client-side, RFC-007 7.5).
        out.retain(|c| {
            if query.required_domains.is_empty() {
                return true;
            }
            let host = c
                .url
                .strip_prefix("https://")
                .or_else(|| c.url.strip_prefix("http://"))
                .unwrap_or(&c.url)
                .split(['/', '?', '#'])
                .next()
                .unwrap_or("")
                .to_lowercase();
            query
                .required_domains
                .iter()
                .any(|d| host == *d || host.ends_with(&format!(".{d}")))
        });
        Ok(out)
    }

    async fn delete_document(&self, document_id: &str) -> Result<(), SearchOsError> {
        let table = self.table().await?;
        table
            .delete(&format!("document_id = '{document_id}'"))
            .await
            .map_err(|e| SearchOsError::Storage(format!("delete: {e}")))?;
        Ok(())
    }

    async fn count(&self) -> Result<u64, SearchOsError> {
        let table = self.table().await?;
        let n = table
            .count_rows(None)
            .await
            .map_err(|e| SearchOsError::Storage(format!("count: {e}")))?;
        Ok(n as u64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn vec_doc(chunk: &str, doc_id: &str, text: &str) -> IndexedVec {
        let embedder = HashingEmbedder::new(256);
        let vector = embedder.embed_one(text);
        IndexedVec {
            chunk_id: chunk.into(),
            document_id: doc_id.into(),
            version_id: "docv_1".into(),
            title: Some("title".into()),
            body: text.into(),
            url: "https://example.com".into(),
            language: Some("en".into()),
            workspace_id: "ws".into(),
            model_id: "hashing-mvp".into(),
            model_version: "v1".into(),
            fetched_at: Utc::now().timestamp() as u64,
            vector,
        }
    }

    #[tokio::test]
    async fn upsert_search_roundtrip() {
        let dir = std::env::temp_dir().join(format!(
            "searchos-lancedb-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let idx = LanceVectorIndex::open(&dir, "embeddings").await.unwrap();
        let v = HashingEmbedder::new(256).embed_one("seed");
        idx.upsert("c1", "doc_1", v, "m").await.unwrap();
        assert_eq!(idx.count().await.unwrap(), 1);
        idx.delete_document("doc_1").await.unwrap();
        assert_eq!(idx.count().await.unwrap(), 0);
        std::fs::remove_dir_all(&dir).ok();
    }

    #[tokio::test]
    async fn search_returns_vector_candidates() {
        let dir = std::env::temp_dir().join(format!(
            "searchos-lancedb-s-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .subsec_nanos()
        ));
        let idx = LanceVectorIndex::open(&dir, "embeddings").await.unwrap();
        idx.rebuild_from(&[
            vec_doc("c1", "doc_1", "tokio async runtime for rust"),
            vec_doc("c2", "doc_2", "chocolate cake recipe with sugar"),
        ])
        .await
        .unwrap();
        let q = RetrievalQuery::new("tokio async rust runtime", 2);
        let hits = idx.search(&q).await.unwrap();
        assert!(!hits.is_empty());
        assert_eq!(hits[0].source, RetrievalSource::Vector);
        assert!(hits[0].vector_score.is_some());
        assert!(
            hits[0].chunk_id.as_str() == "c1",
            "expected tokio chunk first, got {}",
            hits[0].chunk_id
        );
        std::fs::remove_dir_all(&dir).ok();
    }
}
