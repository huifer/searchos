# RFC-002: Core Crate Specification

**Engineering Specification · Version 1.0 · 2026-11-08 · Status: Proposed**

Companion to [RFC-001](rfc-001-searchos-v2.md). This document defines the Cargo Workspace layout,
per-crate responsibilities, the core Traits/contracts, and the inter-crate dependency rules that
implement the RFC-001 architecture.

---

## 1. Workspace Layout

SearchOS is a Cargo Workspace, not a single monolithic crate.

```text
search-os/
│
├── Cargo.toml                # workspace manifest
│
├── apps/
│   ├── search-cli/           # Local Mode entry (CLI)
│   ├── search-server/        # Gateway Mode entry (Axum HTTP)
│   ├── search-worker/        # Crawl / Embedding / Index workers
│   └── search-mcp/           # MCP stdio server
│
├── crates/
│   ├── core/                 # types, errors, events
│   ├── config/               # config loading & validation
│   ├── api-compat/           # Tavily API protocol mapping
│   ├── gateway/              # request ingress, auth, rate limit
│   ├── query/                # query normalization, intent
│   ├── intelligence/         # AI planning, reranking, expansion
│   ├── memory/               # search/document/semantic/research memory
│   ├── retrieval/            # hybrid retrieval, fusion
│   ├── indexing/             # Tantivy / LanceDB index management
│   ├── embeddings/           # embedding model management
│   ├── reranker/             # second-pass ranking
│   ├── crawler/              # fetch, scheduling, selection
│   ├── extractor/            # HTML / PDF / Markdown extraction
│   ├── providers/            # external search providers
│   ├── cache/                # multi-level cache
│   ├── storage/              # SQLite, file, object storage
│   ├── scheduler/            # background jobs
│   ├── cluster/              # node coordination & dispatch
│   ├── observability/        # metrics, cost, tracing
│   └── security/             # SSRF, access control, secrets
│
├── migrations/               # SQLite schema migrations
├── models/                   # default embedding / reranker models
├── configs/                  # example configs
├── extensions/               # crawler / provider / extractor extensions
├── docs/
└── tests/
```

## 2. Crate Responsibilities

| Crate | Responsibility |
|---|---|
| core | Common types, error type, event definitions, Search IR. |
| config | Load & validate `config.yaml`, provider/extension manifests. |
| api-compat | Tavily protocol mapping (request/response DTOs, status codes). |
| gateway | HTTP ingress, authentication, rate limiting, usage tracking. |
| query | Query normalization, intent classification. |
| intelligence | Query understanding, search planner, expansion, reranking. |
| memory | Search Memory, Document Memory, Semantic Memory, Research Memory. |
| retrieval | Hybrid retrieval (BM25 + Vector + Metadata) and fusion. |
| indexing | Tantivy (BM25) and LanceDB (vector) index management. |
| embeddings | Embedding model registry, version tracking, generation. |
| reranker | Cross-encoder / remote reranking. |
| crawler | URL canonicalization, fetch scheduling, crawler selection. |
| extractor | HTML / PDF / Markdown content extraction. |
| providers | External search provider adapters & registry. |
| cache | Exact / semantic cache, TTL & freshness. |
| storage | SQLite metadata, local file store, object storage. |
| scheduler | Background job queue and execution. |
| cluster | Node coordination, task dispatch, sharding metadata. |
| observability | Metrics, cost accounting, OpenTelemetry tracing. |
| security | SSRF protection, access control, secret storage. |

## 3. Dependency Rules

- **No crate may reach into another crate's internals.** Communication is through public Traits
  and the data contracts defined in `core`. (ADR-002)
- **Core has no I/O and no async runtime dependency** beyond `tokio`; it holds only types,
  errors, and pure logic.
- **Apps depend on Application Services, never on Infrastructure directly.** `search-cli` and
  `search-server` call the same `Application Service` layer; the only difference is transport.
- **Crawler, Provider, Extractor, Embedding, Reranker, Memory, Intelligence are Extension
  surfaces** (ADR-010). Built-in implementations live in their crates; third-party ones load via
  the extension runtime.
- **`storage` and `indexing` are the only crates that touch persistence.** All writes go through
  `storage` (SQLite source of truth) + the outbox worker; nothing else writes indexes directly.

## 4. Core Traits & Contracts

### 4.1 Search IR (internal unified result)

```rust
pub struct SearchResult {
    pub id: String,
    pub url: String,
    pub canonical_url: String,
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub content: Option<String>,
    pub score: f32,
    pub published_at: Option<DateTime<Utc>>,
    pub crawled_at: Option<DateTime<Utc>>,
    pub source: String,
    pub content_hash: Option<String>,
    pub metadata: HashMap<String, serde_json::Value>,
}
```

External providers return their own shapes; `api-compat` and `providers` normalize them into
`SearchResult` (the Search IR). HTTP handlers convert Search IR → Tavily DTO on output.

### 4.2 Provider

```rust
#[async_trait]
pub trait SearchProvider: Send + Sync {
    fn id(&self) -> &str;
    async fn health_check(&self) -> Result<ProviderHealth>;
    async fn search(&self, request: ProviderSearchRequest) -> Result<Vec<SearchResult>>;
    fn capabilities(&self) -> ProviderCapabilities;
}
```

### 4.3 Engine / Service entry

```rust
#[async_trait]
pub trait SearchEngine: Send + Sync {
    async fn open(config: &Config) -> Result<Arc<Self>>;
    async fn search(&self, req: SearchRequest) -> Result<SearchResponse>;
    async fn fetch(&self, url: &str, opts: FetchOptions) -> Result<Document>;
}
```

`search-cli` opens the engine in-process; `search-server` opens the same engine and serves it
over HTTP. No HTTP loopback is used by Local Mode. (ADR-003, ADR-004)

### 4.4 Extension

```rust
#[async_trait]
pub trait Extension: Send + Sync {
    fn manifest(&self) -> &ExtensionManifest;
    async fn validate(&self) -> Result<()>;
    async fn health(&self) -> ExtensionHealth;
}
```

Crawler / Extractor / Embedding / Reranker / Provider / Memory / Intelligence / Search
extensions all implement `Extension` plus their domain trait.

## 5. Build & Quality Standards

- `cargo fmt` + `cargo clippy -D warnings` enforced in CI.
- Every crate exposes public unit tests; integration tests live in `tests/`.
- Feature flags isolate optional capabilities (e.g., `rerank`, `wasm-extensions`, `cluster`).
- `core` must remain `#![forbid(unsafe_code)]` unless a documented exception is approved.
