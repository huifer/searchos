# RFC-007: Retrieval & Cache Architecture

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 7.1 Retrieval Pipeline

```
Query
  |
  v
Exact Cache
  |
  v
Canonical Cache
  |
  v
BM25
  |
  v
Vector Retrieval
  |
  v
Metadata Filtering
  |
  v
RRF Fusion
  |
  v
Optional Reranking
  |
  v
Evidence Evaluation
```

## 7.2 Cache Layers

| Cache                 | 作用       | 失效条件                        |
| --------------------- | -------- | --------------------------- |
| Exact Query Cache     | 完全相同请求   | TTL / Policy Version        |
| Canonical Query Cache | 归一化查询复用  | TTL / Constraint Change     |
| Semantic Cache        | 语义近似请求复用 | Quality / Freshness Failure |
| Document Cache        | 网页内容复用   | Content Change / Refresh    |
| Retrieval Cache       | 候选检索结果复用 | Index Version Change        |
| Embedding Cache       | 相同文本向量复用 | Model Version Change        |
| Rerank Cache          | 相同候选排序复用 | Model / Candidate Change    |

## 7.3 Cache Key

```
pub struct SearchCacheKey {
    pub query_fingerprint: String,
    pub profile_version: String,
    pub freshness_policy_version: String,
    pub source_policy_version: String,
    pub language: String,
    pub workspace_scope: String,
}
```

## 7.4 Semantic Reuse

语义复用不能只比较向量相似度。

必须检查：

- Query Intent 是否兼容。
- 时间要求是否兼容。
- 来源要求是否兼容。
- 语言是否兼容。
- Workspace 是否允许共享。
- 证据覆盖是否充分。
- 文档是否仍然有效。

Semantic Similarity 只是候选召回条件，不是最终通过条件。

## 7.5 Hybrid Retrieval

初期使用：

- Tantivy BM25。
- LanceDB Vector Search。
- Metadata Filtering。
- Reciprocal Rank Fusion。

RRF：

[\
RRF(d)=\
\sum\_{i=1}^{n}\
\frac{1}{k+rank_i(d)}\
]

初始 k 可以设为 60，后续根据测试集调优。

## 7.6 Embedding

必须记录：

- Model ID。
- Model Version。
- Dimension。
- Normalization。
- Chunk Hash。
- Embedding Status。

相同文本和相同模型版本可以复用 Embedding。

模型升级时允许双版本并行索引，再完成迁移。

## 7.7 Reranker

Reranker 不默认执行。

触发条件：

- 候选数量较大。
- BM25 和 Vector 结果差异明显。
- 相关性不确定。
- Profile 要求高精度。

Reranker 的成本必须计入任务成本。

## 7.8 Index Consistency

- SQLite 是权威数据源。
- Tantivy 是可重建全文索引。
- LanceDB 是可重建向量索引。
- Outbox 负责索引同步。
- 定期执行索引一致性检查。
