# SearchOS RFC Master Index — Final Architecture Baseline

> SearchOS RFC 总目录 —— 最终架构基线

**Status:** Frozen architecture baseline (adopted for this repo)
**Source:** `https://chatgpt.com/share/6af69118-d0f4-83ee-bbbb-948eeeb623c8`
(`本地搜索基础设施 (1)`), 83 linear nodes. The governing turns are #50
(user: "你先和我明确一下所有的 rfc 到底有啥"), #67/#69/#70 (RFC
reconciliation), #71 (the four architecture decisions), and #75/#79/#81/#82 —
of which **#81 is the complete 22,642-character final Master Index** (the
others are fragments of it).
**Supersedes:** the earlier share `6af5fcc9-...` (53 nodes), whose proposed
**RFC-087…094 series is explicitly dissolved** and merged into 062–087.
**Related:** [`../gap-analysis-master-index.md`](../gap-analysis-master-index.md),
[`rfc-062-federated-search-architecture.md`](rfc-062-federated-search-architecture.md).

---

## English

### 0. Why this document exists

The previous share proposed eight new RFCs (087–094) for federated search.
Turn #67 of this share stops that work:

> "我建议我们先暂停继续写 RFC，先把 SearchOS 的**完整 RFC 体系、模块边界、依赖关系和开发优先级**确定下来 … 之前的 RFC 已经规划到 RFC-061，而且有些后续设计与刚才提出的 RFC-087～094 存在重叠。"

Turn #71 then freezes four product decisions; #81 answers with the final
**RFC-001…087** catalogue, ownership rules, dependency matrix, delivery
priority and RFC template. Per #81: *"087–094 不再重复创建，而是将其内容合并到对应 RFC"*.

### 1. The four confirmed architecture decisions

| Decision | Final choice | Architectural meaning |
|---|---|---|
| Product scope | **All-in-One** | Search, crawler, retrieval, Memory, Research, Evidence and Analytics are one product |
| Extended | **Both** | Rust-native extensions **and** external-process extensions |
| Distributed | **Staged** | Single-node first; core reserves multi-node, enabled gradually |
| Memory | **Full Agent Memory** | Session / Project / Research / Operational memory, not just search history |

Positioning: **Agent Search & Memory Infrastructure** — unified search,
knowledge acquisition, evidence management, long-term memory and research for
AI Agents, reducing repeated search and external provider cost **under a
quality-first constraint**. Not an API cache layer, not a Rust rewrite of Magpie.

### 2. Six frozen principles

1. **Quality First** — cost optimisation must not lower search quality.
2. **Local First** — local retrieval/store/execution first; a local hit still passes the quality gate.
3. **Single-node First** — v1 does not require Redis, Kafka, Kubernetes or external HTTP services.
4. **Unified Core** — Local Mode and Gateway Mode share domain models, search engine and decision logic.
5. **Extension Isolation** — one crawler/provider/extension failure must not take down the core.
6. **Evidence & Provenance** — results keep source, time, version, trust and citation relationships.

### 3. Final RFC catalogue (A–H, RFC-001…087)

> Historical boundary, quoted from #81: *"目前可确认 001–025 属于既有基础 RFC，
> 但没有恢复出它们完整、准确的原始标题。因此不伪造历史标题，也不擅自重新编号。"*
> **Repo correction:** this repo **does** hold accurate titles for RFC-001…016
> and consolidated annexes covering 017–025 (see §11). No title needs to be
> invented here.

#### A. Foundation & Runtime — 001–028

| RFC | Responsibility |
|---|---|
| 001–025 | Legacy Foundation Baseline (preserve existing numbering; verify against source) |
| 026 | Visualization & Analytics Architecture |
| 027 | Analytics Data Model & Aggregation |
| 028 | Dashboard API & Frontend Integration |

#### B. Knowledge & Research Workspace — 029–045

| RFC | Responsibility |
|---|---|
| 029 | Research Workspace |
| 030 | Knowledge Management |
| 031 | Freshness Scheduler |
| 032 | Source Trust |
| 033 | Online Feedback |
| 034 | Policy Management |
| 035 | Search Watch |
| 036 | Document Intelligence |
| 037 | Multilingual Search |
| 038 | Evidence Graph |
| 039 | Provider Intelligence |
| 040 | Search Replay |
| 041 | Identity & RBAC |
| 042 | Usage, Rate Limiting & Billing |
| 043 | Backup & Disaster Recovery |
| 044 | Health & Operations |
| 045 | Import, Export & Portability |

#### C. Search Engine & Retrieval Foundation — 046–061

| RFC | Responsibility |
|---|---|
| 046 | Result Normalization |
| 047 | Ranking Engine |
| 048 | Vertical Search |
| 049 | Structured Extraction |
| 050 | Source Discovery |
| 051 | Crawl Frontier |
| 052 | Browser Rendering |
| 053 | Content Extraction |
| 054 | Task Orchestration |
| 055 | Cancellation & Deadline |
| 056 | Partial Streaming |
| 057 | Temporal Knowledge |
| 058 | Entity Resolution |
| 059 | Contradiction Detection |
| 060 | Agent Registry |
| 061 | SDK |

#### D. Unified Search Intelligence — 062–068

| RFC | Responsibility |
|---|---|
| **062** | **Federated Search Architecture** — coordinate Local / Web / Provider / Crawler / Memory sources |
| 063 | Search Source Registry — registration, capability declaration, health, lifecycle |
| 064 | Unified Resource Model — Document / Page / Chunk / Entity / Evidence |
| 065 | Hybrid Retrieval — FTS, BM25, vector KNN, lexical+semantic |
| 066 | Trust-aware Result Fusion — multi-source fusion, trust weights, RRF, dedup |
| 067 | Context Assembly — Agent-facing evidence selection and citation preservation |
| 068 | Token Budget Management — budget, truncation, compression, priority |

Ownership boundary: 062 coordinates but implements no BM25/vector algorithm;
063 registers sources but does not rank; 064 defines shared structures but no
storage engine; 065 recalls but does not fuse; 066 fuses but does not generate
answers; 067 assembles; 068 constrains size without changing evidence trust.

#### E. Agent Memory & Deep Research — 069–074

| RFC | Responsibility |
|---|---|
| 069 | Agent Session Memory |
| 070 | Memory Lifecycle (extract / consolidate / forget / update / expire) |
| 071 | Deep Search Engine |
| 072 | Query Expansion |
| 073 | Evidence Coverage |
| 074 | Source Independence |

Memory types frozen as **Session / Project / Research / Operational**. The
system must distinguish fact, inference, preference and transient state, and
retain source + time. Conversations must not be treated wholesale as facts.

#### F. Search Cost & Provider Intelligence — 075–080

| RFC | Responsibility |
|---|---|
| 075 | Search Policy Simulation |
| 076 | Adaptive Search Planner |
| 077 | Search Cache Intelligence |
| 078 | Cost Attribution |
| 079 | Search Benchmark |
| 080 | Provider Fallback & Routing |

`077` is explicitly **not** a plain Redis cache: it must judge semantic
equivalence, evidence validity, source changes, real-time requirements,
independent-source sufficiency and quality-gate compliance before reuse.

#### G. Extension & Distributed Runtime — 081–086

| RFC | Responsibility |
|---|---|
| 081 | Extension SDK |
| 082 | Crawler Extension System |
| 083 | Plugin Isolation |
| 084 | Distributed Search Runtime |
| 085 | Multi-node Coordination |
| 086 | Search Intelligence Evaluation |

`Extended = Both`: Rust-native Trait/compile-time extensions (fast path, panic
isolation, version compatibility) **and** external-process extensions (stable
IPC/JSON-RPC, own timeout/limits/restart/health). A shared Extension Contract
covers manifest, version, capability, I/O schema, timeout, cancellation, error,
health, metrics, tracing and compatibility tests. Dynamic Rust ABI plugins are
**not** a v1 requirement.

#### H. Privacy & Data Governance — 087

| RFC | Responsibility |
|---|---|
| 087 | Privacy-aware Ingestion & Egress |

Owns: what may enter indexes and long-term memory; detection and redaction of
API keys, access tokens, credentials and personally sensitive fields; what may
be sent to external providers/LLMs; Workspace/Agent/Session isolation; deletion
propagation across full-text index, vector index, cache and derived memory;
Local vs Gateway security defaults.
The final index merges the earlier RFC-094 here rather than scattering it.

### 4. Dissolution map: 087–094 → final numbering

From turn #69 (§五), with the final #81 override for Privacy:

| Earlier proposal | Final home | Treatment |
|---|---|---|
| 087 Federated Search Architecture | **062** | merge |
| 088 Search Source Registry | **063** | merge |
| 089 Unified Resource Model | **064** | merge |
| 090 Trust-aware Result Fusion | **066** | merge |
| 091 Context Assembly & Token Budget | **067 + 068** | split & merge |
| 092 Agent Session Memory | **069 + 070** | split & merge |
| 093 Deep Search Expansion Engine | **071 + 072 + 073** | split & merge |
| 094 Privacy-aware Ingestion & Egress | **087** | merge (#81 final; #69 had suggested 013/017/083) |

Magpie's concrete mechanisms (five source types, five retrieval modes,
trust-weighted RRF, cross-source dedup, token budget) therefore enter the
existing RFCs — no parallel architecture is created.

### 5. Ownership: one domain object, one owning RFC

| Domain object / capability | Sole owning RFC | Others may only reference it |
|---|---|---|
| Search Resource | 064 | — |
| Search Source | 063 | Provider / Crawler implement the Source Contract |
| Search Execution | 062 | schedules all sources |
| Retrieval Algorithm | 065 | produces recall results |
| Ranking & Fusion | 066 | — |
| Agent Context | 067 | — |
| Token Budget | 068 | bounds 067 without changing trust |
| Agent Session Memory | 069 | — |
| Memory Lifecycle | 070 | — |
| Deep Research | 071 | — |
| Query Expansion | 072 | — |
| Evidence Coverage | 073 | — |
| Source Independence | 074 | — |
| Search Policy | 034 | — |
| Search Planner | 076 | decides, never executes provider SDKs |
| Cache Intelligence | 077 | — |
| Provider Routing | 080 | — |
| Cost Attribution | 078 | — |
| Benchmark | 079 | — |
| Extension Contract | 081 | — |
| Crawler Extension | 082 | — |
| Plugin Isolation | 083 | — |
| Distributed Runtime | 084 | — |
| Node Coordination | 085 | — |
| Evaluation | 086 | — |
| Privacy | 087 | — |

Forbidden duplicate definitions (examples): 065 must not redefine the Source
Registry; 066 must not implement a vector DB; 069 must not implement full-text
search; 071 must not reimplement query expansion; 076 must not call provider
SDKs directly; 077 must not own failover; 082 must not define the shared
extension protocol; 085 must not force a distributed message queue into Local
Mode.

**RFC-076 constraint:** it is the decision hub but must never become a
god-module. It only decides *whether* to search, *which* sources, *whether* to
reuse history, *whether* to expand queries, *whether* to parallelise,
*whether* to acquire more evidence, *whether* the quality gate is met, and
*whether* more cost is justified. Execution stays with the owning modules.

### 6. Dependency matrix

Six layers: (1) foundation — 001–025, 046, 054, 061, 064, 063; (2) retrieval —
062, 065, 047, 066, 050, 051–053; (3) intelligence — 034, 076, 077, 080, 078,
079; (4) knowledge/memory — 030, 036, 069, 070, 057, 058; (5) research/evidence —
071, 072, 073, 074, 038, 059; (6) extension/scale — 081–085. Cross-cutting:
041, 043, 044, 087, 026–028.

| Downstream RFC | Must depend on | Reason |
|---|---|---|
| 062 Federated Search | 046, 063, 064, 054, 055 | unified results, sources, resources, task/deadline contracts |
| 065 Hybrid Retrieval | 064, storage baseline | shared resource & index structure |
| 066 Result Fusion | 046, 047, 032, 063 | normalised results, ranking, trust |
| 067 Context Assembly | 064, 066 | assembles unified resources + fused results |
| 068 Token Budget | 067 | bounds the assembled context |
| 069 Agent Memory | 060, 061, 064, 041, 087 | agent, resource, identity, privacy |
| 070 Memory Lifecycle | 030, 031, 057, 069, 087 | knowledge lifecycle & governance |
| 071 Deep Search | 062, 054, 055, 067, 073 | multi-round search, tasks, coverage |
| 072 Query Expansion | 071, 046 | sub-questions & variants |
| 073 Evidence Coverage | 038, 064, 071 | coverage over the evidence graph |
| 074 Source Independence | 032, 038, 058 | trust and independence |
| 076 Adaptive Planner | 034, 062, 066, 073, 078, 080 | quality/cost/source-aware decisions |
| 077 Cache Intelligence | 064, 031, 032, 066, 087 | is a resource still reusable? |
| 078 Cost Attribution | 039, 042, 060 | provider/usage/agent attribution |
| 079 Benchmark | 040, 047, 066, 078, 086 | replay + compare |
| 080 Provider Routing | 039, 042, 063, 078 | capability/quota/cost routing |
| 081 Extension SDK | 046, 063, 064, 054, 055 | unified extension interfaces |
| 082 Crawler Extension | 051, 052, 053, 063, 081 | crawling + parsing extensions |
| 083 Plugin Isolation | 081, 087, 044 | isolation, security, health |
| 084 Distributed Runtime | 054, 060, 041, 043, 044 | nodes, identity, recovery, ops |
| 085 Multi-node Coordination | 084 | — |
| 086 Search Intelligence Evaluation | 040, 075, 078, 079 | replay, policy, cost, benchmark |

### 7. Delivery priority (not RFC-number order)

**Foundation → Retrieval → Intelligence → Memory → Research → Extension → Distributed**

| Phase | RFCs | Deliverable |
|---|---|---|
| P0 | 001–025 verification, 046, 054, 055, 063, 064, 062, 061 | Local Mode runs; unified interface executes retrieval tasks |
| P1 | 065, 047, 066, 050, 039, 080, 032, 079 | local + external + unified ranking + quality evaluation |
| P2 | 034, 076, 077, 078, 075, 086, 040 | explainable decisions, cost ledger, reuse, policy evaluation |
| P3 | 030, 036, 057, 058, 069, 070, 087 | four memory types, searchable/updatable/deletable knowledge |
| P4 | 038, 071, 072, 073, 074, 059, 067, 068 | multi-round research, cross-source verification, evidence graph |
| P5 | 051–053, 081, 082, 083, 048, 049 | site-specific crawlers, native + process extensions |
| P6 | 026–028, 041, 042, 043, 044, 045, 084, 085 | Gateway, Workspace, Dashboard, multi-node |

Milestones M0–M8: kernel → unified search → search intelligence → cost
optimisation → Agent Memory → Deep Research → Extension → Gateway/Dashboard →
Distributed. **M0–M3 is the first independently shippable product version.**
Do not wait for all RFCs before implementing; do not skip foundational data
contracts to ship early.

### 8. Storage, runtimes, crawler extensions

| Store | Sole responsibility |
|---|---|
| SQLite | Source of Truth: tasks, metadata, policy, memory metadata, cost ledger |
| Tantivy | full-text / BM25 |
| LanceDB | vector index / semantic retrieval |
| Filesystem | raw documents, attachments, fetch snapshots, exports, backups |

Constraint: Tantivy and LanceDB are **rebuildable indexes**; no knowledge asset
may exist only in the vector DB without a traceable SQLite record.

Local Mode calls Rust Core in-process (CLI / Rust SDK / MCP stdio), no HTTP
server required. Gateway Mode exposes HTTP with Workspace/Agent/API key/usage;
the Tavily-compatible API is a compatibility layer and must not bind internal
domain models to Tavily structures. Single-node first.

Site-specific crawlers are independently maintained, tested and upgraded under
`extensions/crawlers/<site>/` (`manifest.toml`, `rules.toml`, `extractor.rs`,
`fixtures/`, `tests/`, `README.md`) with a `registry.toml`. Each records version,
last success, success/failure/timeout rates, last detected structure change,
fixture regression result, field completeness, fallback extractor result,
circuit-breaker state and whether human rule edits are needed. A global Crawler
Health view is required; one site's redesign must not break other sites.

### 9. RFC template and acceptance standard

Every RFC must contain 24 sections: Abstract, Motivation, Goals, Non-goals,
Terminology, Architecture, Module Boundaries, Domain Models, Rust Traits /
Structs / Enums, Storage Schema, Index Design, APIs / CLI / MCP / Gateway,
Algorithms, Error Handling, Timeout / Retry / Cancellation, Security / Privacy,
Observability / Metrics / Tracing, Performance Requirements, Testing Strategy,
Acceptance Criteria, Migration Strategy, Dependencies, ADRs, Implementation
Roadmap.

Three mandatory rules: **(1)** explicit Ownership — one model, one defining
RFC, others only reference it. **(2)** testable acceptance criteria (examples:
Local Mode needs no HTTP server; a provider timeout does not block other
providers; one crawler failure does not affect other sources; deleting a memory
propagates to every derived index; cache reuse passes freshness + quality
checks; multi-source results remain traceable). **(3)** explicit MVP vs Future
Scope — no multi-node / cross-region scheduling forced into v1.

### 10. Frozen checklist and open items

Frozen: All-in-One scope; Rust native + external process extensions; single-node
first with multi-node reserved; Full Agent Memory; Quality First; Local Mode
without mandatory HTTP; Gateway with HTTP + Tavily-compatible API;
SQLite + Tantivy + LanceDB + Filesystem; per-site crawler extensions; Dashboard
and Analytics in the architecture. Totals: **87 planned RFCs, 8 milestones,
2 runtime modes, 2 extension execution modes**.

Open: verify RFC-001–025 original titles/content; check 026–087 against existing
docs for duplicate definitions; build the formal inter-RFC dependency matrix;
add implementation tasks and acceptance tests to each RFC.

**Final rule (#81):** freeze this index; do not add RFC numbers casually. New
requirements go into existing RFCs; a new specification is created only for a
genuinely independent domain responsibility.

### 11. Repo reconciliation (added by this project)

This repo predates the Master Index, so three number collisions must be
recorded rather than silently "fixed":

| Planned number | Master Index title | Existing repo file at that number | Resolution |
|---|---|---|---|
| 029 | Research Workspace | `rfc-029-local-data-ingestion.md` (implemented) | Keep the implemented file; treat the planned 029 as **unassigned**, do not rename shipped work |
| 031 | Freshness Scheduler | `rfc-031-external-evidence-promotion.md` (implemented) | Same: keep shipped file; planned 031 remains **unassigned** |
| 087 | Privacy-aware Ingestion & Egress | previously a draft `rfc-087-federated-search-architecture.md` from the superseded share | **Renamed to `rfc-062-federated-search-architecture.md`**; 087 is reserved for Privacy |

Also: 001–016 titles **do** exist here (`docs/rfc/README.md`), and 017–025 are
consolidated in `part-17`…`part-24` — so the "no original titles recovered"
caveat in #81 does not require inventing anything in this repo. `Part 30`
(INV-30-01…21) and the analytics pushdown work are implementation baselines,
not numbered RFCs.

Delivered so far for this baseline:

* `crates/searchos-federated/` — **RFC-062 kernel** (`SearchSource` trait +
  planner + executor + Phase 2 adapters), **RFC-063** (scoped registry),
  **RFC-064** (resource model); 56 tests including the named gates.
* `crates/searchos-retrieval/` — **RFC-066 slice**: `rrf_fuse_weighted`.
* `crates/searchos-context/` + pipeline wiring — **RFC-067/068** end to end
  (`context_budget` on the request, `context` on the response, all transports
  + OpenAPI). P4 in the priority model, delivered early because it is pure,
  additive and independently testable.
* [`../rfc-062-dependency-review.md`](../rfc-062-dependency-review.md) — the
  P0 contract review and the ordered residue.

See [`../gap-analysis-master-index.md`](../gap-analysis-master-index.md).

---

## 中文

### 0. 本文档的由来

前一个分享提出过 8 份新 RFC（087–094）用于联邦检索。本分享 #67 明确叫停：

> 「我建议我们先暂停继续写 RFC，先把 SearchOS 的**完整 RFC 体系、模块边界、依赖关系和开发优先级**确定下来 … 之前的 RFC 已经规划到 RFC-061，而且有些后续设计与刚才提出的 RFC-087～094 存在重叠。」

#71 冻结四项决策，#81 给出最终 **RFC-001…087** 总目录、职责归属、依赖矩阵、
交付优先级与 RFC 模板。按 #81：**「087–094 不再重复创建，而是将其内容合并到对应 RFC」**。

### 1. 四项已确认的架构决策

| 决策项 | 最终选择 | 架构含义 |
|---|---|---|
| 产品范围 | **All-in-One** | 搜索、爬虫、检索、Memory、Research、Evidence、Analytics 纳入统一产品 |
| Extended | **Both** | 同时支持 Rust 原生扩展与外部进程扩展 |
| 分布式 | **Staged** | 单机优先，核心预留多节点，后续逐步启用 |
| Memory | **Full Agent Memory** | Session / Project / Research / Operational 四类完整记忆，不局限于搜索历史 |

定位：**Agent Search & Memory Infrastructure** —— 为 AI Agent 提供统一搜索、
知识获取、证据管理、长期记忆与研究能力，在**质量优先**的前提下降低重复搜索与
外部 Provider 调用成本。不是 API 缓存层，也不是 Magpie 的 Rust 重写。

### 2. 六条冻结原则

1. **Quality First** —— 成本优化不能降低搜索质量。
2. **Local First** —— 本地检索/存储/执行优先；本地命中仍须经过质量评估。
3. **Single-node First** —— v1 不强制 Redis、Kafka、Kubernetes 或外部 HTTP 服务。
4. **Unified Core** —— Local 与 Gateway 共用领域模型、搜索引擎与决策逻辑。
5. **Extension Isolation** —— 单个爬虫/Provider/扩展失败不得拖垮核心。
6. **Evidence & Provenance** —— 结果必须保留来源、时间、版本、可信度与引用关系。

### 3. 最终 RFC 总目录（A–H，001–087）

> 历史边界（#81 原文）：「目前可确认 001–025 属于既有基础 RFC，但没有恢复出
> 它们完整、准确的原始标题。因此不伪造历史标题，也不擅自重新编号。」
> **仓库校正：** 本仓库**已有** RFC-001…016 的准确标题，以及覆盖 017–025 的
> 合并附录（见 §11），无需凭空补造任何标题。

#### A. Foundation & Runtime：001–028

| 编号 | 核心职责 |
|---|---|
| 001–025 | Legacy Foundation Baseline（保留原编号，待原文核对） |
| 026 | Visualization & Analytics Architecture |
| 027 | Analytics Data Model & Aggregation |
| 028 | Dashboard API & Frontend Integration |

#### B. Knowledge & Research Workspace：029–045

| 编号 | 核心职责 |
|---|---|
| 029 | Research Workspace |
| 030 | Knowledge Management |
| 031 | Freshness Scheduler |
| 032 | Source Trust |
| 033 | Online Feedback |
| 034 | Policy Management |
| 035 | Search Watch |
| 036 | Document Intelligence |
| 037 | Multilingual Search |
| 038 | Evidence Graph |
| 039 | Provider Intelligence |
| 040 | Search Replay |
| 041 | Identity & RBAC |
| 042 | Usage, Rate Limiting & Billing |
| 043 | Backup & Disaster Recovery |
| 044 | Health & Operations |
| 045 | Import, Export & Portability |

#### C. Search Engine & Retrieval Foundation：046–061

| 编号 | 核心职责 |
|---|---|
| 046 | Result Normalization |
| 047 | Ranking Engine |
| 048 | Vertical Search |
| 049 | Structured Extraction |
| 050 | Source Discovery |
| 051 | Crawl Frontier |
| 052 | Browser Rendering |
| 053 | Content Extraction |
| 054 | Task Orchestration |
| 055 | Cancellation & Deadline |
| 056 | Partial Streaming |
| 057 | Temporal Knowledge |
| 058 | Entity Resolution |
| 059 | Contradiction Detection |
| 060 | Agent Registry |
| 061 | SDK |

#### D. Unified Search Intelligence：062–068

| 编号 | 核心职责 |
|---|---|
| **062** | **Federated Search Architecture** —— 协调 Local / Web / Provider / Crawler / Memory 来源 |
| 063 | Search Source Registry —— 注册、能力声明、健康、生命周期 |
| 064 | Unified Resource Model —— Document / Page / Chunk / Entity / Evidence |
| 065 | Hybrid Retrieval —— FTS、BM25、向量 KNN、词法+语义混合 |
| 066 | Trust-aware Result Fusion —— 多源融合、可信度权重、RRF、去重 |
| 067 | Context Assembly —— 面向 Agent 的证据选择与引用保留 |
| 068 | Token Budget Management —— 预算、截断、压缩、优先级 |

职责边界：062 只协调流程，不实现 BM25/向量算法；063 只注册来源，不排序；
064 只定义共享结构，不指定存储引擎；065 只召回，不做最终信任融合；066 只融合，
不生成答案；067 只组装；068 只约束规模，不改变证据可信度。

#### E. Agent Memory & Deep Research：069–074

| 编号 | 核心职责 |
|---|---|
| 069 | Agent Session Memory |
| 070 | Memory Lifecycle（提取/归纳/遗忘/更新/过期） |
| 071 | Deep Search Engine |
| 072 | Query Expansion |
| 073 | Evidence Coverage |
| 074 | Source Independence |

Memory 冻结为 **Session / Project / Research / Operational** 四类。系统必须区分
事实、推断、偏好与临时状态，并保留来源与时间；不能把对话全文直接当长期事实。

#### F. Search Cost & Provider Intelligence：075–080

| 编号 | 核心职责 |
|---|---|
| 075 | Search Policy Simulation |
| 076 | Adaptive Search Planner |
| 077 | Search Cache Intelligence |
| 078 | Cost Attribution |
| 079 | Search Benchmark |
| 080 | Provider Fallback & Routing |

`077` 明确**不是**普通 Redis 缓存：复用前必须判断查询语义等价、历史证据是否仍
有效、来源是否变化、是否要求实时、是否需要补充独立来源、是否满足质量门。

#### G. Extension & Distributed Runtime：081–086

| 编号 | 核心职责 |
|---|---|
| 081 | Extension SDK |
| 082 | Crawler Extension System |
| 083 | Plugin Isolation |
| 084 | Distributed Search Runtime |
| 085 | Multi-node Coordination |
| 086 | Search Intelligence Evaluation |

`Extended = Both`：Rust 原生 Trait / 编译期集成（高性能、panic 隔离、版本兼容）
**与**外部进程扩展（稳定 IPC / JSON-RPC、独立超时/资源限制/重启/健康检查）。
共用 Extension Contract 覆盖 manifest、版本、能力、输入输出 Schema、超时、取消、
错误、健康、指标、追踪与兼容测试。动态 Rust ABI 插件**不是** v1 要求。

#### H. Privacy & Data Governance：087

| 编号 | 核心职责 |
|---|---|
| 087 | Privacy-aware Ingestion & Egress |

统一规定：哪些数据可进索引与长期记忆；API Key / Token / 密码 / 个人敏感字段的
识别与脱敏；哪些内容可外发给 Provider 或 LLM；Workspace / Agent / Session 隔离；
删除后如何同步清除全文索引、向量索引、缓存与派生记忆；Local 与 Gateway 各自的
安全默认值。最终版把此前的 RFC-094 落在这里，而不是散落在多个模块。

### 4. 解散映射：087–094 → 最终编号

来自 #69（第五节），Privacy 以 #81 最终版为准：

| 早前提案 | 最终归属 | 处理方式 |
|---|---|---|
| 087 Federated Search Architecture | **062** | 合并 |
| 088 Search Source Registry | **063** | 合并 |
| 089 Unified Resource Model | **064** | 合并 |
| 090 Trust-aware Result Fusion | **066** | 合并 |
| 091 Context Assembly & Token Budget | **067 + 068** | 拆分合并 |
| 092 Agent Session Memory | **069 + 070** | 拆分合并 |
| 093 Deep Search Expansion Engine | **071 + 072 + 073** | 拆分合并 |
| 094 Privacy-aware Ingestion & Egress | **087** | 合并（#69 曾建议 013/017/083） |

Magpie 的具体机制（五类信息源、五种检索模式、Trust-weighted RRF、跨源去重、
Token Budget）因此分别进入既有 RFC，不再另建平行架构。

### 5. 职责归属：一个领域对象，一个负责 RFC

| 领域对象 / 能力 | 唯一负责 RFC | 其他 RFC 只能引用 |
|---|---|---|
| Search Resource | 064 | — |
| Search Source | 063 | Provider / Crawler 实现 Source Contract |
| Search Execution | 062 | 负责调度各类来源 |
| Retrieval Algorithm | 065 | 提供召回结果 |
| Ranking & Fusion | 066 | — |
| Agent Context | 067 | — |
| Token Budget | 068 | 约束 067，不改变可信度 |
| Agent Session Memory | 069 | — |
| Memory Lifecycle | 070 | — |
| Deep Research | 071 | — |
| Query Expansion | 072 | — |
| Evidence Coverage | 073 | — |
| Source Independence | 074 | — |
| Search Policy | 034 | — |
| Search Planner | 076 | 只决策，不直接操作 Provider SDK |
| Cache Intelligence | 077 | — |
| Provider Routing | 080 | — |
| Cost Attribution | 078 | — |
| Benchmark | 079 | — |
| Extension Contract | 081 | — |
| Crawler Extension | 082 | — |
| Plugin Isolation | 083 | — |
| Distributed Runtime | 084 | — |
| Node Coordination | 085 | — |
| Evaluation | 086 | — |
| Privacy | 087 | — |

禁止重复定义（示例）：065 不得重定义 Source Registry；066 不得实现向量数据库；
069 不得自己实现全文搜索；071 不得重做 Query Expansion；076 不得直接调用
Provider SDK；077 不得负责故障转移；082 不得定义扩展公共协议；085 不得要求
Local Mode 使用分布式消息队列。

**RFC-076 约束**：它是搜索决策中枢，但绝不能变成包办一切的超级模块。它只决定
是否搜索、搜哪些来源、是否复用历史、是否扩展查询、是否并行、是否补证据、
是否达到质量门、是否继续投入成本；执行仍归各模块。

### 6. 依赖关系矩阵

六层：① 基础规范（001–025、046、054、061、064、063）；② 检索基础（062、065、
047、066、050、051–053）；③ 智能决策（034、076、077、080、078、079）；
④ 知识与记忆（030、036、069、070、057、058）；⑤ 研究与证据（071、072、073、
074、038、059）；⑥ 扩展与规模化（081–085）。横向贯穿：041、043、044、087、
026–028。

| 下游 RFC | 必须依赖 | 原因 |
|---|---|---|
| 062 Federated Search | 046、063、064、054、055 | 统一结果、来源、资源与任务/截止时间契约 |
| 065 Hybrid Retrieval | 064、存储基础规范 | 统一资源与索引结构 |
| 066 Result Fusion | 046、047、032、063 | 标准化结果、排序与信任信息 |
| 067 Context Assembly | 064、066 | 组装统一资源与融合结果 |
| 068 Token Budget | 067 | 控制上下文规模 |
| 069 Agent Memory | 060、061、064、041、087 | 关联 Agent、资源、身份与隐私 |
| 070 Memory Lifecycle | 030、031、057、069、087 | 知识生命周期与记忆治理 |
| 071 Deep Search | 062、054、055、067、073 | 多轮搜索、任务管理与证据评估 |
| 072 Query Expansion | 071、046 | 生成子问题与变体 |
| 073 Evidence Coverage | 038、064、071 | 基于证据图评估覆盖度 |
| 074 Source Independence | 032、038、058 | 可信度与独立性判断 |
| 076 Adaptive Planner | 034、062、066、073、078、080 | 综合质量、成本与来源能力决策 |
| 077 Cache Intelligence | 064、031、032、066、087 | 判断资源是否仍可复用 |
| 078 Cost Attribution | 039、042、060 | 归因 Provider、用量与 Agent |
| 079 Benchmark | 040、047、066、078、086 | 回放并比较质量、延迟、成本 |
| 080 Provider Routing | 039、042、063、078 | 能力、额度与成本路由 |
| 081 Extension SDK | 046、063、064、054、055 | 扩展统一接口 |
| 082 Crawler Extension | 051、052、053、063、081 | 爬取与解析扩展 |
| 083 Plugin Isolation | 081、087、044 | 隔离、安全与健康监控 |
| 084 Distributed Runtime | 054、060、041、043、044 | 节点运行、身份、恢复与运维 |
| 085 Multi-node Coordination | 084 | — |
| 086 Search Intelligence Evaluation | 040、075、078、079 | 回放、策略、成本与基准评估 |

### 7. 开发优先级（不按 RFC 编号顺序）

**Foundation → Retrieval → Intelligence → Memory → Research → Extension → Distributed**

| 阶段 | RFC | 交付 |
|---|---|---|
| P0 | 001–025 核对、046、054、055、**063、064、062**、061 | Local Mode 可运行，统一接口执行检索任务 |
| P1 | 065、047、066、050、039、080、032、079 | 本地 + 外部 + 统一排序 + 质量评估 |
| P2 | 034、076、077、078、075、086、040 | 可解释决策、成本账本、历史复用、策略评估 |
| P3 | 030、036、057、058、**069、070、087** | 四类记忆，可检索/更新/删除的知识资产 |
| P4 | 038、071、072、073、074、059、**067、068** | 多轮研究、交叉验证、证据图、结论追溯 |
| P5 | 051–053、081、082、083、048、049 | 站点专属爬虫、原生与进程扩展 |
| P6 | 026–028、041、042、043、044、045、084、085 | Gateway、Workspace、Dashboard、多节点 |

里程碑 M0–M8：内核 → 统一搜索 → 搜索智能 → 成本优化 → Agent Memory →
Deep Research → Extension → Gateway/Dashboard → Distributed。**M0–M3 是第一个
可独立交付版本。** 不要等所有 RFC 写完才实现，也不要为早发布跳过基础数据契约。

### 8. 存储、运行模式与 Crawler Extension

| 存储 | 唯一职责 |
|---|---|
| SQLite | 事实数据源：任务、元数据、策略、记忆元数据、成本账本 |
| Tantivy | 全文检索 / BM25 |
| LanceDB | 向量索引 / 语义检索 |
| Filesystem | 原始文档、附件、抓取快照、导出与备份 |

约束：Tantivy 与 LanceDB 都是**可重建索引**；不允许出现「只有向量库里有、
SQLite 没有可追溯原始记录」的知识资产。

Local Mode 进程内直调 Rust Core（CLI / Rust SDK / MCP stdio），不要求启动 HTTP
Server。Gateway Mode 以 HTTP 暴露 Core，支持 Workspace / Agent / API Key /
Usage；Tavily 兼容 API 只是兼容层，不得让内部领域模型绑定 Tavily 结构。第一阶段单节点。

站点专属爬虫在 `extensions/crawlers/<site>/` 下独立维护、独立测试、独立升级
（`manifest.toml`、`rules.toml`、`extractor.rs`、`fixtures/`、`tests/`、`README.md`），
并有 `registry.toml`。每个爬虫记录版本、最后成功时间、成功/失败/超时率、最近一次
结构变化检测、Fixture 回归结果、字段完整率、降级解析器结果、是否触发熔断、
是否需要人工修订。还需要全局 Crawler Health 视图；某站改版不得影响其他站点。

### 9. RFC 编写与验收标准

每份 RFC 必须包含 24 节：Abstract、Motivation、Goals、Non-goals、Terminology、
Architecture、Module Boundaries、Domain Models、Rust Traits/Structs/Enums、
Storage Schema、Index Design、APIs/CLI/MCP/Gateway、Algorithms、Error Handling、
Timeout/Retry/Cancellation、Security/Privacy、Observability/Metrics/Tracing、
Performance Requirements、Testing Strategy、Acceptance Criteria、Migration
Strategy、Dependencies、ADRs、Implementation Roadmap。

三条强制要求：**(1)** 明确 Ownership —— 一个模型只能由一个 RFC 定义，其他 RFC
只能引用；**(2)** 可测试的验收条件（例：Local Mode 不依赖 HTTP Server；Provider
超时不阻塞其他 Provider；单个爬虫失败不影响其他来源；删除长期记忆后派生索引
全部删除；缓存复用必须经过新鲜度与质量检查；多源结果可追溯到原始来源）；
**(3)** 明确 MVP 与 Future Scope —— 不把多节点、跨区域调度塞进第一版。

### 10. 冻结清单与待办

已冻结：All-in-One 范围；Rust 原生 + 外部进程扩展；单机优先、预留多节点；
Full Agent Memory；Quality First；Local Mode 不强制 HTTP；Gateway 支持 HTTP 与
Tavily 兼容接口；SQLite + Tantivy + LanceDB + Filesystem；Crawler 按站点独立扩展；
Dashboard 与 Analytics 纳入整体架构。总计：**87 份规划 RFC、8 个里程碑、
2 种运行模式、2 种扩展运行方式**。

待办：核对 RFC-001–025 原始标题与内容；确认 026–087 与既有文档无重复定义；
建立 RFC 间正式依赖矩阵；为每份 RFC 补充实现任务与验收测试。

**最终规则（#81）：** 先冻结这份 Master Index，后续不要随意增加 RFC 编号。新增
需求优先归入现有 RFC；只有出现真正独立的领域职责时才新增规范。

### 11. 仓库对照（本项目补充）

本仓库早于 Master Index 存在，因此有三处编号冲突必须**登记而不是静默改写**：

| 规划编号 | Master Index 标题 | 该编号下已有仓库文件 | 处理 |
|---|---|---|---|
| 029 | Research Workspace | `rfc-029-local-data-ingestion.md`（已实现） | 保留已交付文件；规划 029 视为**未分配**，不重命名已发布工作 |
| 031 | Freshness Scheduler | `rfc-031-external-evidence-promotion.md`（已实现） | 同上，规划 031 保持**未分配** |
| 087 | Privacy-aware Ingestion & Egress | 上一轮曾写入 `rfc-087-federated-search-architecture.md`（来自已被取代的分享） | **已改名为 `rfc-062-federated-search-architecture.md`**；087 预留给 Privacy |

另外：本仓库**已有** RFC-001…016 的准确标题（`docs/rfc/README.md`），017–025
合并于 `part-17`…`part-24`，所以 #81 中「未恢复原始标题」的说明在此不需要补造
任何内容。`Part 30`（INV-30-01…21）与分析下推属于实现基线，不是编号 RFC。

本基线迄今交付：

* `crates/searchos-federated/` —— **RFC-062 内核**（`SearchSource` trait +
  planner + executor + Phase 2 适配器）、**RFC-063**（作用域注册表）、
  **RFC-064**（资源模型）；56 个测试，含具名门禁。
* `crates/searchos-retrieval/` —— **RFC-066 切片**：`rrf_fuse_weighted`。
* `crates/searchos-context/` + 管线接入 —— **RFC-067/068** 端到端
  （请求带 `context_budget`、响应带 `context`，三传输层 + OpenAPI）。
  在优先级模型里属 P4，因纯函数、加法式、可独立验证而先行交付。
* [`../rfc-062-dependency-review.md`](../rfc-062-dependency-review.md) ——
  P0 契约审查与有序剩余清单。

详见 [`../gap-analysis-master-index.md`](../gap-analysis-master-index.md)。
