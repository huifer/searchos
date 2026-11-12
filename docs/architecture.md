# Architecture

> System Architecture

SearchOS is organized as a layered system with a **shared core** reused by two runtime
modes. The single most important principle is: **business logic lives in Core / Application
Services, never only in an HTTP handler**, so Local Mode and Gateway Mode behave identically.

## Layers

```text
┌──────────────────────────────────────────┐
│              Interface Layer             │
│  CLI   ·   Rust lib   ·   MCP   ·   HTTP API │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│             Application Layer             │
│  Search · Fetch · Crawl · Memory · Research │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│               Core Runtime                │
│  Query · Planner · Router · Cache ·        │
│  Retrieval · Memory · Crawler · Extension  │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│            Infrastructure                 │
│  SQLite · Tantivy · LanceDB · File/Object  │
│  Queue · Provider · Embedding · Browser*   │
└──────────────────────────────────────────┘
```

`Browser*` is a later-phase architecture target — v1 ships no browser engine
(see the non-goals in [`docs/roadmap.md`](roadmap.md)).

| Module | Responsibility |
|---|---|
| Interface | CLI, HTTP, MCP stdio, SDK |
| Application | use-case orchestration and entry points |
| Core | search, routing, memory, retrieval |
| Extension | site/provider/capability extensions |
| Infrastructure | database, index, file, queue |
| Runtime | local and distributed execution |

Modules communicate only through explicit Traits and data contracts.

## Dual Runtime

| Capability | Local Mode | Gateway Mode |
|---|---|---|
| Entry | CLI / Rust workspace crates / MCP stdio | HTTP API |
| Tavily API | not needed | supported |
| Multi-user / Multi-node | not primary | supported |
| Hard constraint | in-process Core, no HTTP loopback | handlers are thin transport only |

**MCP is stdio-only in v1 (ADR-031).** The gateway does not expose an MCP
route; remote agents use the REST/Tavily surface. The rationale, the stdio
contract and the gates that keep this honest are in [`docs/mcp.md`](mcp.md).

## Process lifecycle

| Signal | Behaviour |
|---|---|
| `SIGTERM` / `SIGINT` (first) | Stop accepting, drain in-flight requests, terminate SSE streams, flush the outbox once, exit `0` |
| Second signal | Force an immediate exit (recovery path for a wedged process) |
| SSE stream | Subscribes to the shutdown signal, so it cannot pin the connection drain |
| Outbox worker | Ticks every 1.5 s and performs one final drain on shutdown |

`GET /metrics` exports the same in-process counters the console shows, in
Prometheus text exposition 0.0.4. See
[`docs/api/http.md`](api/http.md#prometheus-metrics-get-metrics).

## Search Pipeline

```text
Request → Normalize → Intent → Exact Cache → Semantic Cache → Local Retrieval
  → Coverage Eval → (sufficient? return)
  → Planner → Provider Router → External Search → Normalize
  → Dedup → Rerank → Persist → Response
```

## Storage (single-node)

| Concern | Technology |
|---|---|
| Metadata | SQLite (source of truth) |
| Full-text | Tantivy (BM25) — default; remote backends selectable (ADR-027) |
| Vector | LanceDB |
| Object | Local filesystem |
| Cache | SQLite + memory |
| Queue | SQLite-backed job queue |

Indexes are rebuildable and synced from SQLite via a **Transactional Outbox + Index Worker +
Reconciliation**. Local Mode must not require Redis, PostgreSQL, Kafka, NATS, or Docker.

### Remote full-text backends (ADR-027)

`storage.fulltext` selects the retrieval engine. The default stays embedded Tantivy;
remote backends are opt-in and user-configured under `storage.remote.<backend>`:

| `storage.fulltext` | Engine | Adapter crate | Notes |
|---|---|---|---|
| `tantivy` (default) | Tantivy BM25 | `searchos-index-tantivy` | zero-dependency, offline; SearchOS analyzer (CJK bigrams + English stemming), literal-term queries |
| `elasticsearch` | Elasticsearch REST | `searchos-index-remote` | BM25 multi_match, `refresh=wait_for` |
| `meilisearch` | Meilisearch REST | `searchos-index-remote` | task-queue writes awaited |
| `postgres` | PostgreSQL FTS | `searchos-index-remote` | GIN tsvector, `ts_rank_cd` |
| `mysql` | MySQL FULLTEXT | `searchos-index-remote` | ngram parser (CJK), boolean mode |

Contract preserved across backends: SQLite remains the metadata source of truth, every
remote index stays rebuildable via the outbox/`rebuild_index` path (ADR-022), and
workspace isolation (`requester OR *`) is enforced server-side or re-checked client-side.

## Retrieval

BM25 + Vector + Metadata → **RRF Fusion** → Reranker → Top-K. Embedding must record model id,
version, dimensions, normalization, and generation time so models can be swapped and indexes
rebuilt safely.

The embedded index analyzes text with the SearchOS analyzer: CJK runs become overlapping
character bigrams, Latin runs are lowercased and English-stemmed. User queries are analyzed
into literal terms (never parsed as index query syntax) and OR'd across title and body.

The transactional outbox populates **both** legs: each indexed chunk is embedded and upserted
into LanceDB (and deleted from it on delete), so hybrid retrieval has a real vector candidate
list rather than an empty one. `searchos index rebuild` rebuilds full-text and vectors
together from the object store. The runtime's `EmbeddingProvider` instance is shared with the
vector adapter, so stored vectors and query vectors come from the same model.

`memory.embedding.provider: local` uses the offline deterministic hashing embedder (256 dims);
`remote` uses an OpenAI-compatible `/v1/embeddings` endpoint whose API key is read from the
environment variable named by `api_key_env`. The LanceDB Arrow schema follows the model's
dimension, so switching models requires only a rebuild.

A real reranker is attached to the hybrid retriever (RFC-007 7.7): the default
`FeatureReranker` is offline (title/body term coverage, phrase, recency), while
`RemoteReranker` targets cross-encoder endpoints. Reranking is billed as a `Reranking` cost
event. Title terms carry an explicit boost in the local backend (BM25F-lite, aligned with ES
`title^2`), an edit-distance-1 fallback recovers misspelled queries, and result snippets are
generated around the matched region with `<b>…</b>` highlights.

### Agent surface

`POST /search/plan` returns the queries that WOULD run, the open requirements and the
estimated cost — no external call, no billing, no document write. `POST /search/follow-up`
continues a recorded session on exactly the requirements the previous turn left open. MCP
exposes `research` (bounded iterative rounds + session recording) and `plan` alongside the
single-shot `search`. The session store is bounded and in-process (MCP stdio and the gateway
are single processes); it never shortcuts a search.

Evaluation now includes ranking quality: MRR, nDCG@5, Recall@5 and Precision@5 measured
against per-question qrels, plus a Chinese seed corpus and dataset so CJK retrieval
regressions are visible in CI. Qrels support graded labels (`relevance_grades`, 0..3):
nDCG uses `2^grade - 1` while MRR/Recall count only grade >= 2 as a correct answer, so
burying the canonical page behind a merely-related one is a measurable regression.
Remote embedding / cross-encoder adapters have a live HTTP integration test that
exercises the assembled runtime end to end.

Agent sessions are durable (migration v7, `agent_sessions`): the bounded in-process
store mirrors every mutation to SQLite and rehydrates on startup, so the follow-up
contract survives a restart without a schema-breaking change to earlier migrations.

## Local ingestion (RFC-029)

Local content (text, files, directories) is imported through `IngestService`,
a sibling of `FetchService`. Both feed the **same** `DocumentPort`, so local
documents get identical versioning, chunking, outbox indexing, workspace
isolation and rebuild behaviour — they are first-class citizens in
`local_only` / `local_first` search, not a parallel store.

- Canonical URI: `searchos-local://<workspace>/<source_key>`; `source_key` is
  caller-supplied or content-addressed (`sha256(content)[:16]`).
- `sources.source_type` records `web` | `local_text` | `local_file` |
  `local_directory`; local sources are never age-stale (`ContentCategory::Local`).
- Entries: CLI `searchos ingest` (Local Mode, in-process) and HTTP
  `POST /ingest`, `/ingest/file`, `/ingest/batch`, `GET /ingest/jobs/:id`.
- Bounds: 20 MB per document / 200 MB per batch by default; content-type
  sniffing, strict UTF-8, and HTML sanitization reuse the fetch path's rules.

See [`docs/rfc/rfc-029-local-data-ingestion.md`](rfc/rfc-029-local-data-ingestion.md).

## Provider Keys (multi-key rotation)

Upstream providers accept multiple API keys; SearchOS rotates across them so one
rate-limited or exhausted key never fails the whole search (RFC-009 9.7).

| Setting | Meaning |
|---|---|
| `TAVILY_API_KEYS` / `BRAVE_API_KEYS` | comma/space-separated keys; wins over the single-key env |
| `providers.<id>.api_keys` | config-file key list (env still wins) |
| `providers.<id>.rotation` | `round_robin` (default) or `failover` (sticky until failure) |

Per-key health is tracked independently: 429 → rate-limited until Retry-After; 401/403/quota
→ blocked (1800s / 3600s); timeouts/network → consecutive-failure counter. Keys are never logged
or exposed — status output shows masked forms only (`tvly-****-abcd`). When every key is
exhausted, the provider reports 429 with the earliest release time so the caller can honor it.

See the canonical spec: [RFC-001](rfc/rfc-001-product-architecture.md).

## 中文

# 系统架构

SearchOS 是一个分层系统，两个运行模式复用**同一个核心**。最重要的原则是：业务逻辑必须放在
Core / Application Services，绝不能只写在 HTTP handler 里，这样 Local Mode 与 Gateway
Mode 的行为才完全一致。

### 分层

| 模块 | 职责 |
|---|---|
| Interface（接口层） | CLI、HTTP、MCP stdio、SDK |
| Application（应用层） | 用例编排与入口 |
| Core（核心层） | 搜索、路由、记忆、检索 |
| Extension（扩展层） | 站点 / Provider / 能力扩展 |
| Infrastructure（基础设施层） | 数据库、索引、文件、队列 |
| Runtime（运行时） | 本地与分布式执行 |

模块之间只通过显式 Trait 和数据契约通信。

### 双运行模式

| 能力 | Local Mode | Gateway Mode |
|---|---|---|
| 入口 | CLI / Rust workspace crates / MCP stdio | HTTP API |
| Tavily API | 不需要 | 支持 |
| 多用户 / 多节点 | 非主要目标 | 支持 |
| 硬约束 | 进程内 Core，禁止 HTTP 回环 | handler 只做薄传输层 |

**v1 的 MCP 仅支持 stdio（ADR-031）。** 网关不暴露 MCP 路由；远程 agent 使用
REST/Tavily 面。理由、stdio 契约和保证口径的门禁见 [`docs/mcp.md`](mcp.md)。

### 进程生命周期

| 信号 | 行为 |
|---|---|
| `SIGTERM` / `SIGINT`（第一次） | 停止接受连接、排空在途请求、终止 SSE 流、最后一次 outbox flush，以 `0` 退出 |
| 第二次信号 | 立即强制退出（卡死进程的恢复路径） |
| SSE 流 | 订阅 shutdown 信号，因此不会卡住连接排空 |
| Outbox worker | 每 1.5s tick 一次，shutdown 时做最后一次 drain |

`GET /metrics` 以 Prometheus text exposition 0.0.4 暴露控制台看到的同一批进程内计数器，
见 [`docs/api/http.md`](api/http.md#prometheus-metrics-get-metrics)。

### 搜索流水线

```text
Request → Normalize → Intent → Exact Cache → Semantic Cache → Local Retrieval
  → Coverage Eval →（足够则直接返回）
  → Planner → Provider Router → External Search → Normalize
  → Dedup → Rerank → Persist → Response
```

### 存储（单节点）

| 关注点 | 技术 |
|---|---|
| 元数据 | SQLite（唯一事实来源） |
| 全文 | Tantivy（BM25）—— 默认；可选远程后端（ADR-027） |
| 向量 | LanceDB |
| 对象 | 本地文件系统 |
| 缓存 | SQLite + 内存 |
| 队列 | 基于 SQLite 的任务队列 |

索引可通过 **Transactional Outbox + Index Worker + 对账**从 SQLite 重建并保持同步。
Local Mode 不得依赖 Redis、PostgreSQL、Kafka、NATS 或 Docker。

#### 远程全文后端（ADR-027）

`storage.fulltext` 选择检索引擎。默认仍是内嵌 Tantivy；远程后端是可选的、
由用户在 `storage.remote.<backend>` 下自行配置：

| `storage.fulltext` | 引擎 | Adapter crate | 说明 |
|---|---|---|---|
| `tantivy`（默认） | Tantivy BM25 | `searchos-index-tantivy` | 零额外依赖、离线可用；SearchOS 分析器（CJK bigram + 英文词干），字面词项查询 |
| `elasticsearch` | Elasticsearch REST | `searchos-index-remote` | BM25 multi_match，`refresh=wait_for` |
| `meilisearch` | Meilisearch REST | `searchos-index-remote` | 写入等待任务队列完成 |
| `postgres` | PostgreSQL FTS | `searchos-index-remote` | GIN tsvector，`ts_rank_cd` |
| `mysql` | MySQL FULLTEXT | `searchos-index-remote` | ngram 解析器（支持 CJK），boolean 模式 |

跨后端保持的契约：SQLite 始终是元数据事实来源；每个远程索引都能通过
outbox/`rebuild_index` 路径重建（ADR-022）；workspace 隔离（`requester OR *`）
由服务端强制或客户端复查。

### 检索

BM25 + 向量 + 元数据 → **RRF 融合** → Reranker → Top-K。Embedding 必须记录模型 id、
版本、维度、归一化方式和生成时间，才能安全地更换模型并重建索引。

内嵌索引使用 SearchOS 分析器：CJK 连续文本切分为重叠字符 bigram，拉丁文本做小写与英文词干。
用户查询只被分析为字面词项（绝不当作索引查询语法解析），在 title 与 body 上做 OR。

事务性 outbox 同时写入**两条腿**：每个入库 chunk 都会被嵌入并 upsert 到 LanceDB（删除时同步删除），
因此混合检索拥有真实的向量候选集而非空列表。`searchos index rebuild` 会同时从对象存储重建全文与向量。
runtime 的 `EmbeddingProvider` 实例与向量适配器共享，保证存储向量与查询向量出自同一模型。

`memory.embedding.provider: local` 使用离线确定性哈希嵌入器（256 维）；`remote` 使用
OpenAI 兼容的 `/v1/embeddings` 端点，API key 只从 `api_key_env` 指定的环境变量读取。
LanceDB 的 Arrow schema 跟随模型维度，切换模型只需一次重建。

混合检索器上挂接了真实 reranker（RFC-007 7.7）：默认 `FeatureReranker`（离线，title/body
词项覆盖、短语、时效），`RemoteReranker` 支持 cross-encoder 端点。重排序以 `Reranking`
成本事件计费。本地后端的 title 词项带显式加权（BM25F-lite，与 ES `title^2` 对齐），
编辑距离 1 的回退查询可恢复拼写错误，结果 snippet 生成于命中区域并带 `<b>…</b>` 高亮。

### Agent 接口

`POST /search/plan` 返回「将要执行」的查询、未覆盖需求与预估成本 —— 不发起外部调用、不计费、
不写文档。`POST /search/follow-up` 沿上一轮留下的未覆盖需求继续会话。MCP 除单趟 `search`
外提供 `research`（有界迭代 + 会话记录）和 `plan`。会话存储为进程内有界结构，只承载跟进契约，
绝不用于跳过搜索。

评测新增排序质量：基于每道题 qrels 的 MRR、nDCG@5、Recall@5、Precision@5，并附带中文种子
语料与数据集，使 CJK 检索回归在 CI 中可见。qrels 支持分级标注（`relevance_grades`，0..3）：
nDCG 使用 `2^grade - 1`，MRR/Recall 只把 grade >= 2 视为正确答案，因此「规范答案被相关页面
压在后面」是可量化的回归。远程嵌入与 cross-encoder 适配器带有贯穿真实 runtime 的 HTTP 联调测试。

Agent 会话已持久化（migration v7，`agent_sessions`）：进程内有界存储把每次变更同步到 SQLite，
启动时重新载入，因此跟进契约可跨重启保留，且不修改任何已发布迁移。

### 本地导入（RFC-029）

本地内容（文本、文件、目录）通过 `IngestService` 导入，它是 `FetchService` 的兄弟服务。
两者都写入**同一个** `DocumentPort`，所以本地文档获得与抓取文档完全相同的版本管理、
分块、outbox 索引、workspace 隔离与重建行为——它们在 `local_only` / `local_first`
搜索里是一等公民，而不是另一套平行存储。

- 规范 URI：`searchos-local://<workspace>/<source_key>`；`source_key` 由调用方提供，
  或按内容寻址（`sha256(content)[:16]`）。
- `sources.source_type` 记录 `web` \| `local_text` \| `local_file` \| `local_directory`；
  本地来源永不因时间过期（`ContentCategory::Local`）。
- 入口：CLI `searchos ingest`（Local Mode，进程内）与 HTTP `POST /ingest`、
  `/ingest/file`、`/ingest/batch`、`GET /ingest/jobs/:id`。
- 上限：默认单文档 20 MB / 单批 200 MB；content-type 嗅探、严格 UTF-8 和 HTML
  清洗复用抓取路径的同一套规则。

见 [`docs/rfc/rfc-029-local-data-ingestion.md`](rfc/rfc-029-local-data-ingestion.md)。

### Provider 密钥（多 key 轮转）

上游 provider 支持多个 API key；SearchOS 在它们之间轮转，让单个被限流或耗尽的 key
不至于拖垮整个搜索（RFC-009 9.7）。

| 配置 | 含义 |
|---|---|
| `TAVILY_API_KEYS` / `BRAVE_API_KEYS` | 逗号/空格分隔的 key 列表；优先于单 key 环境变量 |
| `providers.<id>.api_keys` | 配置文件中的 key 列表（env 仍优先） |
| `providers.<id>.rotation` | `round_robin`（默认）或 `failover`（粘住直到失败） |

每个 key 的健康状态独立跟踪：429 → 限流直到 Retry-After；401/403/配额 →
封禁（1800s / 3600s）；超时/网络 → 连续失败计数。key 永远不会被日志输出或暴露——
状态输出只显示掩码形式（`tvly-****-abcd`）。当所有 key 都耗尽时，provider 返回
429 并附带最早的恢复时间，让调用方遵守。

参见规范：[RFC-001](rfc/rfc-001-product-architecture.md)。
