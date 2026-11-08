# RFC-006: Knowledge Store & Document Lifecycle

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 6.1 存储职责

| 存储              | 职责                  |
| --------------- | ------------------- |
| SQLite          | 元数据 Source of Truth |
| Filesystem      | 原始内容和派生内容           |
| Tantivy         | 全文检索                |
| LanceDB         | 向量检索                |
| In-Memory Cache | 短期运行态数据             |

## 6.2 Document Lifecycle

```
Discovered
    |
    v
Fetched
    |
    v
Normalized
    |
    v
Content Hash
    |
    +------ Unchanged ------> Update Fetch Metadata
    |
    v
New Version
    |
    v
Chunking
    |
    v
Index Outbox
    |
    +------ Tantivy
    |
    +------ Embedding
              |
              v
           LanceDB
```

## 6.3 Document Model

```
pub struct Document {
    pub id: DocumentId,
    pub source_id: SourceId,

    pub canonical_url: String,
    pub current_version_id: Option<DocumentVersionId>,

    pub content_type: String,
    pub language: Option<String>,

    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

pub struct DocumentVersion {
    pub id: DocumentVersionId,
    pub document_id: DocumentId,

    pub content_hash: String,
    pub raw_content_ref: String,
    pub markdown_ref: Option<String>,

    pub fetched_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,

    pub extractor_version: String,
    pub crawler_version: Option<String>,
}
```

## 6.4 Versioning

同一个 URL 可以存在多个版本。

内容 Hash 不变：

- 不创建新的内容版本。
- 更新 Fetch Metadata。
- 不重复 Chunking。
- 不重复 Embedding。
- 不重复索引写入。

内容 Hash 变化：

- 创建新版本。
- 生成新的 Chunk。
- 生成新的索引更新事件。
- 保留旧版本。
- 更新 Current Version Pointer。

## 6.5 Content Storage

建议：

```
~/.searchos/
├── config/
├── data/
│   ├── searchos.db
│   ├── objects/
│   ├── documents/
│   └── backups/
│
├── indexes/
│   ├── tantivy/
│   └── lancedb/
│
├── extensions/
├── models/
├── cache/
├── logs/
└── temp/
```

原始内容使用 Content-Addressed Storage：

```
objects/
  └── sha256/
       └── ab/
            └── abcdef...
```

## 6.6 Transactional Outbox

SQLite 事务内：

1. 写入 Document Version。
2. 写入 Chunk Metadata。
3. 写入 Outbox Event。
4. Commit。

后台 Worker 消费 Outbox：

- 更新 Tantivy。
- 更新 LanceDB。
- 更新索引状态。

失败时可以重试。

Tantivy 和 LanceDB 都是可重建索引，不是元数据 Source of Truth。

## 6.7 Deletion

删除文档时：

- 标记 Document Deleted。
- 删除检索索引。
- 删除关联 Chunk。
- 删除关联 Embedding。
- 按保留策略处理原始内容。
- 保留必要的审计信息，但不得保留已经被明确要求删除的正文内容。

删除操作必须支持失败恢复和一致性检查。
