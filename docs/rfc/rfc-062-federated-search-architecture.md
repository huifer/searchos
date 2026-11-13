# RFC-062: Federated Search Architecture

> 联邦检索架构

**Architecture Baseline:** SearchOS RFC Master Index — Final Architecture Baseline (RFC-001…087)
**Document Status:** Implemented (kernel: `searchos-federated` — trait, registry, resource model, planner; 35 invariant tests) / runtime wiring is Phase 2
**Category:** Core Architecture
**Priority:** P0
**Series:** RFC Master Index, group D — Unified Search Intelligence (062–068)
**Source:** `6af5fcc9-8ca8-83e8-9ca8-31dc0d5f77f0` turns #47/#48/#52 (draft) · `6af69118-d0f4-83ee-bbbb-948eeeb623c8` turns #69/#71/#81 (final Master Index)
**Depends on (real files):** `rfc-001-product-architecture.md`, `rfc-002-core-domain-workspace.md`, `rfc-003-search-request-query-intelligence.md`, `rfc-004-search-decision-engine.md`, `rfc-005-quality-gate-evidence-evaluation.md`, `rfc-006-knowledge-store-document-lifecycle.md`, `rfc-007-retrieval-cache-architecture.md`, `rfc-008-cost-accounting-cost-model.md`, `rfc-009-execution-planner-provider-router.md`, `rfc-012-local-runtime-gateway-runtime.md`, `rfc-013-memory-cross-agent-reuse.md`, `rfc-014-security-reliability.md`, `part-23-decision-engineering-baseline.md`, `part-24-api-governance-baseline.md` (RFC-017…025), `part-30-execution-correctness-baseline.md`, `rfc-026/027/028`, `rfc-029`, `rfc-031`.
**Related:** [`rfc-master-index.md`](rfc-master-index.md) (final catalogue, ownership, dependencies), [`../gap-analysis-master-index.md`](../gap-analysis-master-index.md) (repo reconciliation).
**Supersedes:** None.

> **Numbering note.** The earlier share proposed this as RFC-087 in an
> 087–094 series. The later share (`6af69118-…`, 83 nodes) **froze the final
> RFC Master Index (001–087)** at turn #81 and explicitly dissolved that series:
> *"087–094 不再重复创建，而是将其内容合并到对应 RFC"*. Federated Search is
> therefore **RFC-062**; Source Registry is 063, Unified Resource Model 064,
> Trust-aware Fusion 066, Context Assembly 067, Token Budget 068, and Privacy
> 087. This document has been renamed accordingly. See
> [`rfc-master-index.md`](rfc-master-index.md) §4 and
> [`../gap-analysis-master-index.md`](../gap-analysis-master-index.md).

---

## English

### 1. Context

SearchOS v1.x is built around one pipeline:

```text
Local Knowledge Retrieval → Quality Gate → (on failure) External Search
```

That shape is correct for the original goal — minimise paid search API calls by
reusing local knowledge — but it hard-codes two branches. Local knowledge goes
through `LocalKnowledgePort`; external results go through `ProviderRouter`;
agents' own past work (sessions, tool executions, research transcripts) is not a
retrievable source at all. Adding a fifth source today means editing
`search_service.rs`, not registering an adapter.

A review of Magpie Search (`https://github.com/xfloukiex-lab/magpie-search`)
in the source conversation reframes the product: an Agent needs **federated
information retrieval** — one query, many heterogeneous sources — with unified
normalization, trust-aware fusion, evidence evaluation, and delivery shaped to
the Agent's context window. SearchOS's existing strengths (quality gate,
evidence provenance, cost accounting, knowledge lifecycle, analytics console)
are exactly what a plain federated search engine lacks, so this is an
**extension**, not a rewrite.

### 2. Goals

| ID | Goal |
|----|------|
| G1 | One `SearchSource` abstraction covering local knowledge, vector store, memory/sessions, web search, web documents, external APIs and structured data. |
| G2 | A scope-aware `SearchSourceRegistry` (Global → Workspace → Project → Agent precedence) with capability, health and enable/disable state. |
| G3 | A `FederatedPlanner` that selects sources from policy → permission → scope → capability → existing evidence → cost → reliability, and never fans out to every source by default. |
| G4 | Deterministic per-source execution with independent deadline, budget, cancellation and status; one source failing must not fail the request. |
| G5 | A unified `SearchCandidate` with stable `resource_id` separated from `candidate_id`, preserved raw scores, provenance and permission reference. |
| G6 | Cross-source dedup that retains every occurrence's provenance, and trust-aware fusion that never mistakes N reposts for N independent sources. |
| G7 | `Context Assembly` that fits evidence into a caller-declared token budget without dropping citations, conflicts or counter-evidence. |
| G8 | Explicit `Partial` / `QualityFailed` / budget-exhausted outcomes. Fail-open is availability, never a quality pass. |
| G9 | Local Mode stays a true in-process Rust runtime; Gateway Mode only adapts DTOs. No mandatory LLM, no mandatory distributed infrastructure. |

### 3. Non-goals

| ID | Non-goal |
|----|----------|
| N1 | LLM answer generation. Core API returns structured evidence; the calling Agent writes the answer. |
| N2 | Multi-agent swarms or one LLM per search branch. |
| N3 | A full knowledge graph implementation, a mandated vector DB, or a mandated crawler. |
| N4 | Renumbering / rewriting RFC-001…031 or replacing `search_service.rs` in one shot. |
| N5 | Querying every registered source on every request. |
| N6 | Persisting all retrieved results into long-term knowledge (RFC-031 already governs promotion). |
| N7 | Adding new `SearchMode` variants (see §5.2). |

### 4. Core principles

1. Search once, retrieve from multiple sources.
2. Sources are independent; evidence quality is unified.
3. Federation does not mean querying every source on every request.
4. Retrieval ranking and evidence quality are separate concerns.
5. A source failure must not automatically fail the entire search.
6. Partial results must never be represented as complete.
7. SearchOS remains retrieval infrastructure, not a mandatory answer generator.

### 5. Architecture

#### 5.1 Flow

```text
                 Search Request
                       |
                       v
                Request Normalizer
                       |
                       v
                 Policy Resolver
                       |
                       v
               Query Understanding
                       |
                       v
              Federated Search Planner
                       |
                       v
                Source Registry
                       |
                       v
              Source Selection Engine
          +------------+-------------+
          |            |             |
          v            v             v
     Local Source   Memory Source  Web Source
          |            |             |
          v            v             v
      BM25/Vector   Session Search  Search API
          |            |             |
          +------------+-------------+
                       |
                       v
                Result Normalizer
                       |
                       v
                Cross-source Dedup
                       |
                       v
                Trust-aware Fusion
                       |
                       v
                 Evidence Engine
                       |
                       v
                  Quality Gate
                       |
                       v
                Coverage Evaluation
             +---------+---------+
             |                   |
             v                   v
        Quality Pass        Quality Fail
             |                   |
             v                   v
       Context Assembly     Gap Analysis
             |                   |
             v                   v
       Search Response      Search Expansion
```

Two side paths stay non-blocking: Knowledge Persistence (RFC-031) and the
Analytics Pipeline (RFC-026…028).

#### 5.2 Layers and the `SearchMode` decision

| Layer | Responsibility |
|---|---|
| Request | parsing, identity, permissions, policy |
| Understanding | query normalization, intent, information requirements |
| Planning | source selection, execution plan, budget allocation |
| Source | independent retrieval per source |
| Normalization | unified candidate model |
| Fusion | dedup, ranking, source combination |
| Evidence | trust, coverage, quality |
| Response | assembly, citations, status, statistics |

**ADR-062-008 (correction to the source draft):** federation strategy is
orthogonal to execution mode. `SearchMode` (`local_only | local_first |
external_first | external_only | hybrid`) is the **permission boundary** and is
frozen by `INV-30-18` (all transports agree), the Tavily compatibility
contract, the analytics `search_mode` SQL filter and cache keys. New variants
would break four gates for no new capability. Introduce instead:

```rust
pub enum FederationStrategy {
    Auto,       // planner selects legal sources
    Parallel,   // execute the selected sources concurrently
    Sequential, // execute by priority, stop when coverage is met
    Selective,  // only sources that can fill an open requirement
}
```

Invariants: `local_only` may never reach a web source through `Auto`;
`external_only` may never be short-circuited by a local cache hit;
`local_first` may not declare quality passed on one local result;
`Parallel` does not imply all registered sources.

### 6. Domain model

All types below are **additive**; existing types are extended, not replaced.

#### 6.1 Source descriptor and trait

```rust
pub enum SourceCategory {
    LocalKnowledge, WebSearch, WebDocument, FileSystem, AgentMemory,
    Conversation, KnowledgeGraph, VectorStore, ExternalApi,
    StructuredDatabase, Custom,
}

pub struct SourceCapabilities {
    pub lexical_search: bool,
    pub semantic_search: bool,
    pub exact_lookup: bool,
    pub metadata_filter: bool,
    pub time_filter: bool,
    pub full_document_fetch: bool,
    pub incremental_sync: bool,
    pub pagination: bool,
    pub freshness_metadata: bool,
    pub provenance_metadata: bool,
    pub permission_filtering: bool,
}

pub struct SearchSourceDescriptor {
    pub source_id: SourceId,
    pub name: String,
    pub category: SourceCategory,
    pub version: String,
    pub enabled: bool,
    pub priority: i32,
    pub capabilities: SourceCapabilities,
    pub reliability_profile: ReliabilityProfile,
    pub cost_profile: SourceCostProfile,
    pub latency_profile: SourceLatencyProfile,
    pub scope: SourceScope,
}

#[async_trait]
pub trait SearchSource: Send + Sync {
    fn descriptor(&self) -> SearchSourceDescriptor;
    async fn search(
        &self,
        request: SourceSearchRequest,
        context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError>;
    async fn health_check(&self) -> Result<SourceHealth, SourceError>;
}
```

A Source must not decide global quality, select other sources, mutate global
policy, or generate an answer. Adding a source must not require editing the
planner's core.

#### 6.2 Registry

```rust
pub trait SearchSourceRegistry: Send + Sync {
    fn register(&self, source: Box<dyn SearchSource>) -> Result<(), RegistryError>;
    fn unregister(&self, source_id: &SourceId) -> Result<(), RegistryError>;
    fn get(&self, source_id: &SourceId) -> Option<Arc<dyn SearchSource>>;
    fn list(&self, filter: SourceFilter) -> Vec<SearchSourceDescriptor>;
    fn set_health(&self, source_id: &SourceId, health: SourceHealth);
}
```

Scope precedence: `Agent → Project → Workspace → Global`. A higher-priority
scope may override defaults but **never** relax permission or security policy.

#### 6.3 Plan and execution

```rust
pub struct FederatedSearchPlan {
    pub plan_id: PlanId,
    pub stages: Vec<SearchStage>,
    pub budget: SearchBudget,
    pub stop_conditions: Vec<StopCondition>,
}

pub struct SearchStage {
    pub stage_id: String,
    pub execution_mode: StageExecutionMode, // Sequential | Parallel | Adaptive
    pub source_tasks: Vec<SourceTask>,
    pub timeout_ms: u64,
}

pub struct SourceExecutionContext {
    pub request_id: RequestId,
    pub source_id: SourceId,
    pub deadline: DateTime<Utc>,
    pub cancellation_token: CancellationToken,
    pub budget: SourceBudget,
    pub scope: SearchScope,
}

pub enum SourceExecutionStatus {
    Success, Partial, Timeout, RateLimited, Unavailable,
    PermissionDenied, Failed, Skipped, Cancelled,
}
```

Default planning order is mandatory: Policy → Permission → Scope → Capability →
Existing Evidence → Cost → Reliability → Selection → Stage generation.

#### 6.4 Candidate, budget and response

```rust
pub struct SearchCandidate {
    pub candidate_id: CandidateId,
    pub resource_id: ResourceId,      // separated from candidate_id
    pub source_id: SourceId,
    pub resource_type: ResourceType,  // RFC-064 defines the full enum
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub canonical_ref: Option<String>,
    pub content_ref: Option<ContentRef>,
    pub observed_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub relevance_score: Option<f32>, // raw, never comparable across sources
    pub provenance: ProvenanceRef,
    pub permission: PermissionRef,
}

pub struct DeduplicatedCandidate {
    pub primary: SearchCandidate,
    pub occurrences: Vec<SourceOccurrence>, // never discard provenance
}
```

`SearchBudget` gains (all `#[serde(default)]`):

```rust
pub max_sources: u32,
pub max_parallel_sources: u32,
pub max_candidates: u32,
pub max_context_tokens: Option<u32>,
```

`SearchResponse` gains additive, defaulted fields only:

```rust
pub context: Option<ContextBlock>,       // RFC-067 + RFC-068 / searchos-context
pub source_results: Vec<SourceExecutionResult>,
pub budget_usage: Option<SearchBudgetUsage>,
```

`SearchStatus` and `derive_status` remain the single source of truth. A second
status enum is forbidden.

### 7. Interface contract

- **CLI / MCP / HTTP / SDK** all consume the same `SearchRequest` DTO. A new
  optional `context_budget` object is additive and must be accepted identically
  by every transport (`INV-30-18` pattern).
- Any new HTTP route must be added to `api_route_table()` **and**
  `docs/openapi.yaml` in the same change; `openapi_spec_matches_the_route_table`
  and `every_registered_route_resolves` are hard gates.
- `/crawl` and `/map` stay 404 and stay out of the spec (Tavily contract).
- `SearchResponse` additions must be `#[serde(default)]` so cached payloads and
  the TypeScript/Python SDKs stay deserializable.
- Fusion must not require an LLM to run.

### 8. Data model

No schema change in P0/P1. Phase 3+ adds append-only migration v6 (never edit
released migrations) with:

- `search_sources` — descriptor, scope, enabled, health, version.
- `federated_plans` — plan id, request id, stages, stop conditions, replayable.
- `source_executions` — request/plan/source, status, latency, candidate count,
  cost, error, deadline.
- `source_occurrences` — candidate → source observations for dedup provenance.

All belong to `analytics_read_models` as a **source** dimension, not a new
global payload; filter pushdown rules from the analytics pass still apply.

### 9. Failure handling

| Failure | Behaviour |
|---|---|
| One source timeout / error / rate limit | Isolated. Other sources continue. Record `SourceExecutionStatus`. |
| Required official source unavailable | Return `Partial` or `QualityFailed` with `failed_sources` and `missing_requirements`. Never claim success. |
| Planner failure | No fallback to "query everything"; permissions and budget still apply. |
| Fusion failure | May return explicitly *unfused* candidates; must not claim trust-ranked ordering. |
| Persistence failure | Follow RFC-018 consistency policy; never silently drop audit events. |
| Budget exhausted | Keep results, mark unexecuted sources, emit `budget_exhausted`, never lower the quality gate. |

**INV-FED-01 (red line):** fail-open is an availability property, not a quality
verdict. This preserves the existing `Reject { ProviderUnavailable }` →
`insufficient_evidence` invariant.

### 10. Security considerations

- Every source obeys workspace scope; no cross-workspace reads.
- Query parameters must not leak to unauthorised sources.
- External egress passes through policy; external web content is untrusted input
  and prompt-injection defence covers every external source.
- Every candidate carries a `PermissionRef`; candidates without a valid
  permission may not enter Context Assembly, and their filtering is audited.
- Permission is inherited from the original resource when knowledge is shared
  across agents — one agent's access does not grant the whole workspace access.
- Secret/PII redaction happens **before** persistence into any index (RFC-087).

### 11. Performance considerations

| Parameter | Initial value | Notes |
|---|---:|---|
| Max parallel sources | 8 | configurable |
| Default source timeout | 5 s | independent of global deadline |
| Max retries per source | 2 | with backoff |
| Max results per source | 20 | bounded |
| Max total candidates | 100 | bounded; prevents unbounded memory |

These are benchmark starting points, not architectural constants. A single slow
source must never block the request until the global deadline. Local Mode must
not create an HTTP loopback. Concurrency limits exist at global / source /
workspace / agent / per-request-fan-out levels.

### 12. Acceptance criteria

Functional — register heterogeneous sources; select by scope + policy; run
sequential and parallel; normalise results; dedup across sources with
occurrences preserved; handle timeout/failure; evaluate evidence coverage;
return honest `Partial` / `QualityFailed`.

Performance — configurable concurrency; one source's timeout does not block
others; bounded candidate memory; fusion without an LLM; no HTTP loopback in
Local Mode.

Security — no cross-workspace retrieval; no unauthorised source execution; no
permission-less candidate in final context; federation cannot bypass egress
policy.

Reliability — one source's failure does not fail others; registry state is
traceable; plans are replayable; decisions auditable; budget exhaustion has an
explicit status.

Named tests to be added when each phase lands:
`federated_auto_never_escapes_local_only`,
`scope_precedence_cannot_relax_permissions`,
`source_timeout_does_not_fail_the_request`,
`required_source_failure_forces_partial`,
`dedup_preserves_every_occurrence`,
`reposts_do_not_count_as_independent_sources`,
`fusion_score_is_not_a_quality_verdict`,
`context_assembly_never_drops_citations`,
`budget_exhaustion_never_lowers_the_quality_gate`.

### 13. Dependencies and compatibility

Under the final Master Index, **RFC-062 depends on 046 (Result Normalization), 063 (Source Registry), 064 (Unified Resource Model), 054 (Task Orchestration) and 055 (Cancellation & Deadline)**. Those are planned RFCs; the table below records the repo files that exist today and must stay compatible.

| Existing file | Required change |
|---|---|
| `rfc-002` | extend Search IR with `Resource` / `Source` / `Evidence` relations |
| `rfc-003` | query understanding gains source-scope recognition |
| `rfc-004` | decision engine gains source selection (strategy is orthogonal to mode) |
| `rfc-005` | cross-source evidence independence and corroboration |
| `rfc-007` | `rrf_fuse` generalised to N weighted lists |
| `rfc-009` | source-level execution planning |
| `rfc-006` / `rfc-029` | local ingestion registered as a Source |
| `rfc-012` / `rfc-013` | memory and sessions registered as Sources |
| `part-24` (RFC-017/021) | source permission, egress policy, unified Search + Context API |
| `part-30` | new invariants appended, existing INV-30-* untouched |
| `rfc-026/027/028` | source-level analytics and console views |
| `rfc-031` | promotion stays the only path into persistent knowledge |

Migration rules: keep local retrieval and the external provider router; wrap
both as Sources; keep old interfaces working; migrate internal callers
gradually; never rewrite the whole Search Core at once; every migration needs
regression tests.

### 14. Implementation tasks

| Phase | Tasks | Status |
|---|---|---|
| P0 | Bilingual RFC, gap analysis, README/CHANGELOG | **Done** |
| P1 | `SearchSource` trait, descriptor, registry, unified candidate, `searchos-context` crate (RFC-067/068) + tests | **Delivered** (`searchos-federated` kernel + context crate) |
| P2 | Migrate existing sources: local BM25, local vector, web search, web document | Planned |
| P3 | Planner: selection, sequential/parallel, timeout, budget, cancellation | Planned |
| P4 | Normalization, exact/canonical/content-hash dedup, weighted RRF | **Weighted RRF delivered** (`rrf_fuse_weighted`); dedup fusion pending wiring |
| P5 | Evidence coverage, source independence, quality gate integration, partial handling | Planned |
| P6 | Advanced sources: agent memory, transcripts, KG, external API, structured DB | Planned |

### 15. Architectural decisions (RFC-062 series)

`ADR-062-001` Unified Search Source — every retrievable source implements `SearchSource`.
`ADR-062-002` Planner controls fan-out — no default "query all sources".
`ADR-062-003` Evidence quality is independent from ranking — fusion score never replaces the quality gate.
`ADR-062-004` Fail-open with explicit partial state — availability ≠ quality pass.
`ADR-062-005` Retrieval does not require generation — model-agnostic core.
`ADR-062-006` Local-first remains core — federation does not weaken it.
`ADR-062-007` Source isolation — independent budget, timeout, status, error handling.
`ADR-062-008` Federation strategy is orthogonal to `SearchMode` — protects `INV-30-18` and the frozen Tavily contract.
`ADR-062-009` Additive response evolution — one `SearchStatus`, `#[serde(default)]` additions only.
`ADR-062-010` The final Master Index (001–087) is frozen; new requirements join existing RFCs instead of creating new numbers.

> These ADRs live in the RFC-number namespace (`062-*`); the repo's existing
> `docs/adr/` sequence (ADR-001…033) is unaffected.

### 16. Planned series

| RFC | Name | Core responsibility |
|---|---|---|
| 062 | Federated Search Architecture | this document (coordinates sources; owns no retrieval algorithm) |
| 063 | Search Source Registry | registration, discovery, capability, scope, health |
| 064 | Unified Resource Model | `SearchResource` / `ResourceType` beyond `Document` |
| 065 | Hybrid Retrieval | FTS / BM25 / vector KNN recall (separate owner) |
| 066 | Trust-aware Result Fusion | weighted RRF, dedup, source independence |
| 067 | Context Assembly | evidence assembly and citation preservation |
| 068 | Token Budget Management | token budget, truncation, priority |
| 069–074 | Agent Memory & Deep Research | sessions, lifecycle, deep search, expansion, coverage, independence |
| 087 | Privacy-aware Ingestion & Egress | redaction, permission inheritance, egress control |

Delivery order is **not** RFC-number order: the Master Index puts 062–064 in P0,
065–066 in P1, 067–068 in P4 and 087 in P3.

---

## 中文

### 1. 背景

SearchOS v1.x 围绕一条链路构建：

```text
本地知识检索 → 质量门 →（不通过时）外部搜索
```

这个形状对原始目标（通过复用本地知识尽量减少付费搜索 API 调用）是正确的，
但它把两个分支写死了：本地走 `LocalKnowledgePort`，外部走 `ProviderRouter`，
而 Agent 自己过去的工作（会话、工具执行、研究记录）根本不是可检索来源。
今天要接入第五种来源，只能改 `search_service.rs`，而不是注册一个适配器。

源对话中对 Magpie Search（`https://github.com/xfloukiex-lab/magpie-search`）
的复盘重新定义了产品：Agent 需要的是**联邦信息检索** —— 一次查询、多个异构
来源 —— 加上统一标准化、信任加权融合、证据评估，以及按 Agent 上下文窗口
裁剪的交付。SearchOS 已有的强项（质量门、证据 provenance、成本核算、知识
生命周期、分析控制台）恰恰是普通联邦搜索引擎缺少的，因此这是**扩展**而非重写。

### 2. 目标

| 编号 | 目标 |
|----|------|
| G1 | 一个 `SearchSource` 抽象覆盖本地知识、向量库、记忆/会话、Web 搜索、网页文档、外部 API 与结构化数据。 |
| G2 | 带作用域的 `SearchSourceRegistry`（Global → Workspace → Project → Agent 优先级），具备能力、健康、启停状态。 |
| G3 | `FederatedPlanner` 按策略 → 权限 → 作用域 → 能力 → 已有证据 → 成本 → 可靠性选择来源，且默认绝不向所有来源扇出。 |
| G4 | 逐来源确定性执行，独立 deadline、预算、取消与状态；单一来源失败不得让整个请求失败。 |
| G5 | 统一 `SearchCandidate`，稳定的 `resource_id` 与 `candidate_id` 分离，保留原始分数、provenance 与权限引用。 |
| G6 | 跨源去重保留每一次出现的 provenance；信任加权融合绝不把 N 个转载站当成 N 个独立来源。 |
| G7 | `Context Assembly` 在调用方声明的 Token 预算内交付证据，且不丢失引用、冲突或反例。 |
| G8 | 明确返回 `Partial` / `QualityFailed` / 预算耗尽。Fail-open 是可用性，不是质量通过。 |
| G9 | Local Mode 仍是真正的进程内 Rust runtime；Gateway Mode 只做 DTO 适配。不强制 LLM、不强制分布式基础设施。 |

### 3. 非目标

| 编号 | 非目标 |
|----|----------|
| N1 | LLM 答案生成。核心 API 返回结构化证据，答案由调用方 Agent 生成。 |
| N2 | Multi-agent swarm，或每个搜索分支各起一次 LLM。 |
| N3 | 完整知识图谱实现、指定向量数据库、指定爬虫。 |
| N4 | 重编号 / 重写 RFC-001…031，或一次性替换 `search_service.rs`。 |
| N5 | 每次请求都查询全部已注册来源。 |
| N6 | 把所有检索结果持久化进长期知识库（沉淀规则已由 RFC-031 规定）。 |
| N7 | 新增 `SearchMode` 变体（见 §5.2）。 |

### 4. 核心原则

1. 一次搜索，多源召回。
2. 来源彼此独立，证据质量统一。
3. 联邦不等于每次都查所有来源。
4. 检索排序与证据质量是两个关注点。
5. 单一来源失败不得自动让整个搜索失败。
6. 部分结果绝不能表现为完整结果。
7. SearchOS 仍是检索基础设施，不是强制性答案生成器。

### 5. 架构

#### 5.1 流程

```text
                 Search Request
                       |
                       v
                Request Normalizer
                       |
                       v
                 Policy Resolver
                       |
                       v
               Query Understanding
                       |
                       v
              Federated Search Planner
                       |
                       v
                Source Registry
                       |
                       v
              Source Selection Engine
          +------------+-------------+
          |            |             |
          v            v             v
     Local Source   Memory Source  Web Source
          |            |             |
          v            v             v
      BM25/Vector   Session Search  Search API
          |            |             |
          +------------+-------------+
                       |
                       v
                Result Normalizer
                       |
                       v
                Cross-source Dedup
                       |
                       v
                Trust-aware Fusion
                       |
                       v
                 Evidence Engine
                       |
                       v
                  Quality Gate
                       |
                       v
                Coverage Evaluation
             +---------+---------+
             |                   |
             v                   v
        Quality Pass        Quality Fail
             |                   |
             v                   v
       Context Assembly     Gap Analysis
             |                   |
             v                   v
       Search Response      Search Expansion
```

两条旁路保持非阻塞：知识沉淀（RFC-031）与分析流水线（RFC-026…028）。

#### 5.2 分层与 `SearchMode` 决策

| 层 | 职责 |
|---|---|
| Request | 请求解析、身份、权限、策略 |
| Understanding | 查询归一化、意图、信息需求 |
| Planning | 来源选择、执行计划、预算分配 |
| Source | 各来源独立检索 |
| Normalization | 统一候选模型 |
| Fusion | 去重、排序、来源合并 |
| Evidence | 可信度、覆盖率、质量 |
| Response | 组装、引用、状态、统计 |

**ADR-062-008（对源草案的修正）：** 联邦策略与执行模式正交。
`SearchMode`（`local_only | local_first | external_first | external_only |
hybrid`）是**权限边界**，且已被 `INV-30-18`（各传输层一致）、Tavily 兼容契约、
看板 `search_mode` SQL 筛选与缓存键冻结。新增变体会为了没有新能力而打穿四道
门禁。改为引入：

```rust
pub enum FederationStrategy {
    Auto,       // planner 自动选择合法来源
    Parallel,   // 并发执行已选来源
    Sequential, // 按优先级执行，覆盖达标即停
    Selective,  // 只选能填补开放需求的来源
}
```

不变量：`local_only` 不得借 `Auto` 触及 Web；`external_only` 不得被本地缓存
命中短路；`local_first` 不得凭一条本地结果就判定质量通过；`Parallel` 不意味着
启动全部已注册来源。

### 6. 领域模型

以下类型全部是**加法式**；扩展既有类型，不替换。

#### 6.1 来源描述符与 trait

```rust
pub enum SourceCategory {
    LocalKnowledge, WebSearch, WebDocument, FileSystem, AgentMemory,
    Conversation, KnowledgeGraph, VectorStore, ExternalApi,
    StructuredDatabase, Custom,
}

pub struct SourceCapabilities {
    pub lexical_search: bool,
    pub semantic_search: bool,
    pub exact_lookup: bool,
    pub metadata_filter: bool,
    pub time_filter: bool,
    pub full_document_fetch: bool,
    pub incremental_sync: bool,
    pub pagination: bool,
    pub freshness_metadata: bool,
    pub provenance_metadata: bool,
    pub permission_filtering: bool,
}

pub struct SearchSourceDescriptor {
    pub source_id: SourceId,
    pub name: String,
    pub category: SourceCategory,
    pub version: String,
    pub enabled: bool,
    pub priority: i32,
    pub capabilities: SourceCapabilities,
    pub reliability_profile: ReliabilityProfile,
    pub cost_profile: SourceCostProfile,
    pub latency_profile: SourceLatencyProfile,
    pub scope: SourceScope,
}

#[async_trait]
pub trait SearchSource: Send + Sync {
    fn descriptor(&self) -> SearchSourceDescriptor;
    async fn search(
        &self,
        request: SourceSearchRequest,
        context: SourceExecutionContext,
    ) -> Result<SourceSearchResponse, SourceError>;
    async fn health_check(&self) -> Result<SourceHealth, SourceError>;
}
```

Source 不得决定全局质量、选择其他来源、修改全局策略或生成答案。新增来源
不得要求修改 planner 核心实现。

#### 6.2 Registry

```rust
pub trait SearchSourceRegistry: Send + Sync {
    fn register(&self, source: Box<dyn SearchSource>) -> Result<(), RegistryError>;
    fn unregister(&self, source_id: &SourceId) -> Result<(), RegistryError>;
    fn get(&self, source_id: &SourceId) -> Option<Arc<dyn SearchSource>>;
    fn list(&self, filter: SourceFilter) -> Vec<SearchSourceDescriptor>;
    fn set_health(&self, source_id: &SourceId, health: SourceHealth);
}
```

作用域优先级：`Agent → Project → Workspace → Global`。高优先级可覆盖默认配置，
但**绝不能**放宽权限与安全策略。

#### 6.3 计划与执行

```rust
pub struct FederatedSearchPlan {
    pub plan_id: PlanId,
    pub stages: Vec<SearchStage>,
    pub budget: SearchBudget,
    pub stop_conditions: Vec<StopCondition>,
}

pub struct SearchStage {
    pub stage_id: String,
    pub execution_mode: StageExecutionMode, // Sequential | Parallel | Adaptive
    pub source_tasks: Vec<SourceTask>,
    pub timeout_ms: u64,
}

pub struct SourceExecutionContext {
    pub request_id: RequestId,
    pub source_id: SourceId,
    pub deadline: DateTime<Utc>,
    pub cancellation_token: CancellationToken,
    pub budget: SourceBudget,
    pub scope: SearchScope,
}

pub enum SourceExecutionStatus {
    Success, Partial, Timeout, RateLimited, Unavailable,
    PermissionDenied, Failed, Skipped, Cancelled,
}
```

规划顺序强制为：策略 → 权限 → 作用域 → 能力 → 已有证据 → 成本 → 可靠性 →
选择 → 生成阶段。

#### 6.4 候选、预算与响应

```rust
pub struct SearchCandidate {
    pub candidate_id: CandidateId,
    pub resource_id: ResourceId,      // 与 candidate_id 分离
    pub source_id: SourceId,
    pub resource_type: ResourceType,  // 完整枚举由 RFC-064 定义
    pub title: Option<String>,
    pub snippet: Option<String>,
    pub canonical_ref: Option<String>,
    pub content_ref: Option<ContentRef>,
    pub observed_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub relevance_score: Option<f32>, // 原始分数，跨来源不可直接比较
    pub provenance: ProvenanceRef,
    pub permission: PermissionRef,
}

pub struct DeduplicatedCandidate {
    pub primary: SearchCandidate,
    pub occurrences: Vec<SourceOccurrence>, // 绝不丢弃 provenance
}
```

`SearchBudget` 新增（均 `#[serde(default)]`）：

```rust
pub max_sources: u32,
pub max_parallel_sources: u32,
pub max_candidates: u32,
pub max_context_tokens: Option<u32>,
```

`SearchResponse` 只新增带默认值的字段：

```rust
pub context: Option<ContextBlock>,       // RFC-067 + RFC-068 / searchos-context
pub source_results: Vec<SourceExecutionResult>,
pub budget_usage: Option<SearchBudgetUsage>,
```

`SearchStatus` 与 `derive_status` 仍是唯一真源，禁止第二套状态枚举。

### 7. 接口契约

- **CLI / MCP / HTTP / SDK** 共用同一 `SearchRequest` DTO。新增可选
  `context_budget` 对象必须是加法字段，且各传输层行为一致（沿用 `INV-30-18` 模式）。
- 任何新 HTTP 路由必须在同一次改动里同时加入 `api_route_table()` 与
  `docs/openapi.yaml`；`openapi_spec_matches_the_route_table` 与
  `every_registered_route_resolves` 是硬门禁。
- `/crawl` 与 `/map` 保持 404 且不进 spec（Tavily 契约）。
- `SearchResponse` 新增字段必须带 `#[serde(default)]`，保证缓存载荷与
  TypeScript/Python SDK 可反序列化。
- 融合过程不得依赖 LLM 才能执行。

### 8. 数据模型

P0/P1 不改 schema。Phase 3+ 增加追加式 migration v6（绝不修改已发布迁移）：

- `search_sources` —— 描述符、作用域、启停、健康、版本。
- `federated_plans` —— plan id、request id、stages、停止条件，可回放。
- `source_executions` —— request/plan/source、状态、延迟、候选数、成本、错误、deadline。
- `source_occurrences` —— 候选到来源观察的去重 provenance 记录。

全部作为 `analytics_read_models` 的 **source 维度**接入，不另建全局载荷；
分析下推规则继续适用。

### 9. 失败处理

| 失败 | 行为 |
|---|---|
| 单来源超时 / 错误 / 限流 | 隔离，其他来源继续；记录 `SourceExecutionStatus`。 |
| 必需的官方来源不可用 | 返回 `Partial` 或 `QualityFailed`，带 `failed_sources` 与 `missing_requirements`，绝不宣称成功。 |
| Planner 失败 | 不回退成「全量查询」；权限与预算仍然生效。 |
| Fusion 失败 | 可返回明确标记为「未融合」的候选，但不得宣称已完成可信度排序。 |
| 持久化失败 | 遵循 RFC-018 一致性策略，绝不静默丢弃审计事件。 |
| 预算耗尽 | 保留已有结果，标记未执行来源，返回 `budget_exhausted`，绝不降低质量门。 |

**INV-FED-01（红线）：** fail-open 是可用性属性，不是质量裁决。它保全了现有
`Reject { ProviderUnavailable }` → `insufficient_evidence` 不变量。

### 10. 安全考量

- 每个来源遵守 workspace 作用域，禁止跨 workspace 读取。
- 查询参数不得泄漏给未授权来源。
- 外发必须经过策略；外部网页内容视为不可信输入，prompt injection 防护覆盖所有外部来源。
- 每个候选携带 `PermissionRef`；无有效权限的候选不得进入 Context Assembly，且其过滤行为被审计。
- 跨 Agent 共享知识时权限继承自原始资源 —— 一个 Agent 能访问某文档，不代表整个 workspace 都能访问。
- Secret/PII 脱敏必须在写入任何索引**之前**完成（RFC-087）。

### 11. 性能考量

| 参数 | 初始值 | 说明 |
|---|---:|---|
| 最大并行来源数 | 8 | 可配置 |
| 默认单来源超时 | 5 s | 独立于全局 deadline |
| 单来源最大重试 | 2 | 带退避 |
| 单来源最大结果数 | 20 | 有界 |
| 总候选上限 | 100 | 有界，防止无界内存增长 |

以上只是 benchmark 起点，不是架构常量。单个慢来源不得把请求阻塞到全局
deadline。Local Mode 不得产生 HTTP 回环。并发上限分全局 / 来源 / workspace /
agent / 单请求扇出五级。

### 12. 验收条件

功能 —— 可注册异构来源；按作用域 + 策略选择；串行与并行执行；结果标准化；
跨源去重并保留 occurrences；处理超时与失败；执行证据覆盖评估；诚实返回
`Partial` / `QualityFailed`。

性能 —— 并发可配置；单来源超时不阻塞其他来源；候选内存有界；融合不依赖
LLM；Local Mode 无 HTTP 回环。

安全 —— 不跨 workspace 检索；不执行未授权来源；无权限候选不进最终上下文；
联邦不能绕过外发策略。

可靠性 —— 单来源失败不影响其他来源；registry 状态可追踪；计划可回放；
决策可审计；预算耗尽有明确状态。

各阶段落地时须补充的具名测试：
`federated_auto_never_escapes_local_only`、
`scope_precedence_cannot_relax_permissions`、
`source_timeout_does_not_fail_the_request`、
`required_source_failure_forces_partial`、
`dedup_preserves_every_occurrence`、
`reposts_do_not_count_as_independent_sources`、
`fusion_score_is_not_a_quality_verdict`、
`context_assembly_never_drops_citations`、
`budget_exhaustion_never_lowers_the_quality_gate`。

### 13. 依赖与兼容性

按最终 Master Index，**RFC-062 依赖 046（结果标准化）、063（来源注册）、064（统一资源模型）、054（任务编排）、055（取消与截止时间）**。这些是规划中的 RFC；下表记录仓库当前真实存在、必须保持兼容的文件。

| 既有文件 | 需要的改动 |
|---|---|
| `rfc-002` | Search IR 增加 `Resource` / `Source` / `Evidence` 关系 |
| `rfc-003` | 查询理解增加来源作用域识别 |
| `rfc-004` | 决策引擎增加来源选择（策略与 mode 正交） |
| `rfc-005` | 跨来源独立性与相互印证 |
| `rfc-007` | `rrf_fuse` 泛化为 N 个带权列表 |
| `rfc-009` | 来源级执行计划 |
| `rfc-006` / `rfc-029` | 本地摄取注册为 Source |
| `rfc-012` / `rfc-013` | 记忆与会话注册为 Source |
| `part-24`（RFC-017/021） | 来源权限、外发策略、统一 Search + Context API |
| `part-30` | 追加新不变量，既有 INV-30-* 不动 |
| `rfc-026/027/028` | 来源级分析与控制台视图 |
| `rfc-031` | 沉淀仍是进入持久知识的唯一路径 |

迁移规则：保留本地检索与外部 provider router；把两者包装成 Source；旧接口
继续可用；内部调用逐步迁移；绝不一次性重写整个 Search Core；每次迁移都要有
回归测试。

### 14. 实施任务

| 阶段 | 任务 | 状态 |
|---|---|---|
| P0 | 双语 RFC、差距分析、README/CHANGELOG | **已完成** |
| P1 | `SearchSource` trait、descriptor、registry、统一候选、`searchos-context` crate（RFC-067/068）+ 测试 | **部分完成**（context crate 已交付；trait/registry 归入 P3） |
| P2 | 迁移既有来源：本地 BM25、本地向量、Web 搜索、网页文档 | 计划中 |
| P3 | Planner：选择、串行/并行、超时、预算、取消 | **已交付**（`FederatedPlanner` + `FederatedExecutor`）；planner 已通过 `federation.strategy` 接入 `search_service.rs`（默认 legacy / plan_only / execute 告警） |
| P4 | 标准化、精确/规范化 URL/内容哈希去重、带权 RRF | **带权 RRF 已交付**（`rrf_fuse_weighted`）；去重融合待接线 |
| P5 | 证据覆盖、来源独立性、质量门集成、部分结果处理 | 计划中 |
| P6 | 高级来源：Agent 记忆、对话记录、知识图谱、外部 API、结构化 DB | 计划中 |

### 15. 架构决策（RFC-062 系列）

`ADR-062-001` 统一 Search Source —— 所有可检索来源实现 `SearchSource`。
`ADR-062-002` Planner 控制扇出 —— 默认不得「查询全部来源」。
`ADR-062-003` 证据质量独立于排序 —— 融合分数不得替代质量门。
`ADR-062-004` Fail-open 配显式部分状态 —— 可用性 ≠ 质量通过。
`ADR-062-005` 检索不依赖生成 —— 核心与模型无关。
`ADR-062-006` Local-first 仍是核心 —— 联邦不削弱它。
`ADR-062-007` 来源隔离 —— 独立预算、超时、状态、错误处理。
`ADR-062-008` 联邦策略与 `SearchMode` 正交 —— 保护 `INV-30-18` 与已冻结的 Tavily 契约。
`ADR-062-009` 响应加法演进 —— 单一 `SearchStatus`，只用 `#[serde(default)]`。
`ADR-062-010` 最终 Master Index（001–087）已冻结；新增需求归入既有 RFC，不再随意新增编号。

> 这些 ADR 位于 RFC 编号命名空间（`062-*`），不影响仓库既有
> `docs/adr/` 序列（ADR-001…033）。

### 16. 系列规划

| RFC | 名称 | 核心职责 |
|---|---|---|
| 062 | Federated Search Architecture | 本文档（协调来源，不拥有检索算法） |
| 063 | Search Source Registry | 注册、发现、能力、作用域、健康 |
| 064 | Unified Resource Model | 超越 `Document` 的 `SearchResource` / `ResourceType` |
| 065 | Hybrid Retrieval | FTS / BM25 / 向量 KNN 召回（独立归属） |
| 066 | Trust-aware Result Fusion | 带权 RRF、去重、来源独立性 |
| 067 | Context Assembly | 证据组装与引用保留 |
| 068 | Token Budget Management | Token 预算、截断、优先级 |
| 069–074 | Agent Memory & Deep Research | 会话、生命周期、深度搜索、扩展、覆盖、独立性 |
| 087 | Privacy-aware Ingestion & Egress | 脱敏、权限继承、外发控制 |

交付顺序**不等于** RFC 编号顺序：Master Index 把 062–064 放在 P0，065–066 在 P1，
067–068 在 P4，087 在 P3。
