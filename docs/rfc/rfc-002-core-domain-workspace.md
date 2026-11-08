# RFC-002: Core Domain & Rust Workspace

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 2.1 Workspace Structure

```
searchos/
├── Cargo.toml
├── rust-toolchain.toml
│
├── crates/
│   ├── searchos-domain/
│   ├── searchos-core/
│   ├── searchos-application/
│   │
│   ├── searchos-policy/
│   ├── searchos-query/
│   ├── searchos-decision/
│   ├── searchos-planner/
│   ├── searchos-cost/
│   │
│   ├── searchos-cache/
│   ├── searchos-retrieval/
│   ├── searchos-document/
│   ├── searchos-memory/
│   │
│   ├── searchos-provider/
│   ├── searchos-fetch/
│   ├── searchos-crawler/
│   ├── searchos-extension/
│   │
│   ├── searchos-storage-sqlite/
│   ├── searchos-index-tantivy/
│   ├── searchos-vector-lancedb/
│   ├── searchos-storage-fs/
│   │
│   ├── searchos-interface-cli/
│   ├── searchos-interface-http/
│   ├── searchos-interface-mcp/
│   │
│   ├── searchos-observability/
│   └── searchos-evaluation/
│
├── extensions/
│   ├── providers/
│   ├── crawlers/
│   ├── extractors/
│   ├── embeddings/
│   └── rerankers/
│
├── fixtures/
│   ├── queries/
│   ├── documents/
│   ├── providers/
│   └── evaluations/
│
├── docs/
│   ├── RFC/
│   ├── ADR/
│   └── architecture/
│
└── tests/
    ├── integration/
    ├── e2e/
    ├── performance/
    └── compatibility/
```

## 2.2 Crate Responsibilities

| Crate          | 职责                                   |
| -------------- | ------------------------------------ |
| domain         | 核心领域对象和领域规则                          |
| core           | Runtime 组装和生命周期                      |
| application    | Search、Fetch、Memory 等 Use Cases      |
| policy         | Search Profile、Quality Policy、Budget |
| query          | Query Normalization 和 Intent         |
| decision       | 本地与外部执行决策                            |
| planner        | 执行 DAG 和资源规划                         |
| cost           | 成本估算、记录、归因                           |
| cache          | 多级缓存                                 |
| retrieval      | BM25、Vector、Hybrid                   |
| document       | 文档、版本、Chunk                          |
| memory         | 持久化知识和 Workspace Namespace           |
| provider       | 外部 Provider 抽象                       |
| fetch          | HTTP Fetch 和内容下载                     |
| crawler        | Crawl Engine                         |
| extension      | Extension Registry 和 Lifecycle       |
| storage-sqlite | SQLite Adapter                       |
| index-tantivy  | 全文索引 Adapter                         |
| vector-lancedb | 向量索引 Adapter                         |
| storage-fs     | 文件系统 Adapter                         |
| interface-cli  | CLI                                  |
| interface-http | HTTP Gateway                         |
| interface-mcp  | MCP stdio                            |
| observability  | Metrics、Tracing、Logging              |
| evaluation     | Search Quality Benchmark             |

## 2.3 Domain Types

```
pub struct SearchRequest {
    pub request_id: RequestId,
    pub query: String,
    pub profile: SearchProfileId,
    pub mode: SearchMode,
    pub workspace_id: WorkspaceId,
    pub agent_id: Option<AgentId>,
    pub budget: SearchBudget,
    pub constraints: SearchConstraints,
}

pub enum SearchMode {
    LocalOnly,
    LocalFirst,
    ExternalFirst,
    ExternalOnly,
    Hybrid,
}

pub struct SearchConstraints {
    pub freshness: FreshnessRequirement,
    pub minimum_coverage: f32,
    pub minimum_quality: f32,
    pub required_domains: Vec<String>,
    pub excluded_domains: Vec<String>,
    pub language: Option<String>,
}

pub enum FreshnessRequirement {
    Any,
    Recent,
    Strict,
    MaximumAgeSeconds(u64),
}
```

所有 ID 使用强类型封装，避免把不同实体的 String 混用。

## 2.4 核心 Trait

```
pub trait SearchService: Send + Sync {
    async fn search(
        &self,
        request: SearchRequest,
    ) -> Result<SearchResponse>;
}

pub trait DocumentRepository: Send + Sync {
    async fn get(
        &self,
        id: DocumentId,
    ) -> Result<Option<Document>>;

    async fn save(
        &self,
        document: Document,
    ) -> Result<DocumentId>;

    async fn get_current_version(
        &self,
        id: DocumentId,
    ) -> Result<Option<DocumentVersion>>;
}

pub trait SearchIndex: Send + Sync {
    async fn index(
        &self,
        document: IndexedDocument,
    ) -> Result<()>;

    async fn search(
        &self,
        query: RetrievalQuery,
    ) -> Result<Vec<RetrievalCandidate>>;

    async fn delete(
        &self,
        document_id: DocumentId,
    ) -> Result<()>;
}

pub trait CostRepository: Send + Sync {
    async fn append(
        &self,
        event: CostEvent,
    ) -> Result<()>;

    async fn summarize(
        &self,
        query: CostQuery,
    ) -> Result<CostSummary>;
}
```

生产实现可以使用 `async_trait`，也可以在 Rust 版本和 Trait 设计允许时使用原生 async Trait。

## 2.5 依赖规则

- Domain 不依赖 Infrastructure。
- Application 只依赖 Domain 和抽象接口。
- Decision 不直接依赖 Tavily、Brave 或 SQLite。
- Provider 实现不能反向依赖 Application。
- HTTP Handler 不包含业务决策。
- CLI 不包含业务决策。
- Storage Adapter 不能改变 Domain 语义。

## 2.6 错误类型

统一错误分类：

```
pub enum SearchOsError {
    Validation(ValidationError),
    Policy(PolicyError),
    Budget(BudgetError),
    Provider(ProviderError),
    Retrieval(RetrievalError),
    Storage(StorageError),
    Extension(ExtensionError),
    Quality(QualityError),
    Security(SecurityError),
    Timeout(TimeoutError),
    Internal(InternalError),
}
```

每个错误必须具备：

- 稳定 Error Code。
- 是否可重试。
- 是否允许降级。
- 是否影响任务质量。
- 是否产生费用。
- 关联 Trace ID。
