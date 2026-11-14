# Final RFC Master Index — Repo Gap Analysis

> 最终 RFC Master Index —— 仓库差距分析
>
> **Sources**
> - `https://chatgpt.com/share/6af5fcc9-8ca8-83e8-9ca8-31dc0d5f77f0` — 53 nodes; Magpie review + the RFC-087 draft (**superseded**).
> - `https://chatgpt.com/share/6af69118-d0f4-83ee-bbbb-948eeeb623c8` — 83 nodes; the **final** RFC Master Index. Governing turns: #50 ("你先和我明确一下所有的 rfc 到底有啥"), #67/#69/#70 (reconciliation), #71 (four frozen decisions), #75/#79/#81/#82 — **#81 is the complete 22,642-character final baseline**.
>
> Both were fetched via `GET /backend-api/share/<id>` (2.36 MB / 1.63 MB) with cache-busting; no hidden mapping branches. Related: [`docs/rfc/rfc-master-index.md`](rfc/rfc-master-index.md), [`docs/rfc/rfc-062-federated-search-architecture.md`](rfc/rfc-062-federated-search-architecture.md).

---

## English

### 1. What changed between the two shares

| | Earlier share (53 nodes) | **Final share (83 nodes)** |
|---|---|---|
| Product | Federated Search & Evidence-Native Knowledge Infrastructure | **Agent Search & Memory Infrastructure** (All-in-One) |
| RFC plan | new series **087–094** (8 RFCs) | **final catalogue 001–087**; 087–094 explicitly dissolved |
| Federated Search | RFC-087 | **RFC-062** |
| Source Registry | RFC-088 | **RFC-063** |
| Unified Resource Model | RFC-089 | **RFC-064** |
| Trust-aware Fusion | RFC-090 | **RFC-066** |
| Context + Token Budget | RFC-091 (one RFC) | **RFC-067 + RFC-068** (split) |
| Agent Memory | RFC-092 | **RFC-069 + RFC-070** (split) |
| Deep Search | RFC-093 | **RFC-071 + RFC-072 + RFC-073** (split) |
| Privacy | RFC-094 | **RFC-087** |

Turn #67 stops RFC authoring until the catalogue is frozen: *"先暂停继续写 RFC，先把 SearchOS 的完整 RFC 体系、模块边界、依赖关系和开发优先级确定下来"*. Turn #71 freezes four decisions — **All-in-One / Both extensions / Staged distributed / Full Agent Memory**. Turn #81 delivers the catalogue, ownership rules, dependency matrix, P0–P6 priority, M0–M8 milestones and a 24-section RFC template, and states: *"087–094 不再重复创建，而是将其内容合并到对应 RFC"*.

### 2. Dissolution map (from #69 §五, Privacy per #81)

| Earlier proposal | Final home | Treatment |
|---|---|---|
| 087 Federated Search Architecture | **062** | merge |
| 088 Search Source Registry | **063** | merge |
| 089 Unified Resource Model | **064** | merge |
| 090 Trust-aware Result Fusion | **066** | merge |
| 091 Context Assembly & Token Budget | **067 + 068** | split & merge |
| 092 Agent Session Memory | **069 + 070** | split & merge |
| 093 Deep Search Expansion Engine | **071 + 072 + 073** | split & merge |
| 094 Privacy-aware Ingestion & Egress | **087** | merge (#69 had suggested 013/017/083) |

### 3. Repo reconciliation — three number collisions

The repo predates the Master Index. Collisions are **recorded, not silently rewritten**:

| Planned number | Master Index title | Existing repo file | Resolution |
|---|---|---|---|
| 029 | Research Workspace | `rfc-029-local-data-ingestion.md` (implemented, RFC-029) | keep shipped file; planned 029 stays **unassigned** |
| 031 | Freshness Scheduler | `rfc-031-external-evidence-promotion.md` (implemented, RFC-031) | keep shipped file; planned 031 stays **unassigned** |
| 087 | Privacy-aware Ingestion & Egress | a draft `rfc-087-federated-search-architecture.md` from the superseded share | **renamed to `rfc-062-federated-search-architecture.md`**; 087 reserved for Privacy |

Also: RFC-001…016 titles **do** exist here (`docs/rfc/README.md`), and 017–025 are consolidated in `part-17`…`part-24`, so #81's "original titles not recovered" caveat does not require inventing anything. `Part 30` (INV-30-01…21) is an implementation baseline, not a numbered RFC. The full catalogue is in [`docs/rfc/rfc-master-index.md`](rfc/rfc-master-index.md) §11.

### 4. Capability gap map against the final numbering

Legend: **Have** = implemented & tested · **Partial** = exists but narrower than the final spec · **Gap** = absent · **Reject** = explicit non-goal.

| Final RFC | Requirement | Status | Repo evidence | Action |
|---|---|---|---|---|
| 062 | `SearchSource` abstraction + federated planner | **Phases 1–3 + pipeline wiring delivered (incl. task 4b execution); P0 residual list closed** | `crates/searchos-federated` kernel/executor/adapters; planner consulted by `search_service.rs` behind `federation.strategy`; `execute` mode runs providers through `FederatedExecutor` with service-side billing (double-billing gate passes); `/search/plan` reports sources; `/system/status` and `GET /sources` expose the catalog | none for P0 (session-transcript source, per-source trace fields and reliability-weighted fusion all wired) |
| 063 | scoped Source Registry (capability, health, enable/disable) | **Kernel delivered** | `InMemorySourceRegistry`: precedence resolution, filters, disable-across-scopes, one-descriptor-per-id; adapters register cleanly into it (see `registry_drives_a_full_federated_execution`) | SQLite projection with the Gateway surface |
| 064 | Unified Resource Model beyond `Document` | **Kernel delivered** | `SearchResource` + 11 `ResourceType`s, stable identity, hash-first dedup, fail-closed `PermissionRef`, egress default off | adapters map document/session rows |
| 065 | Hybrid Retrieval (FTS + BM25 + KNN) | **Partial → vertical routing delivered** | `HybridRetriever` + weighted RRF + Tantivy (CJK bigrams, literal-term, title boost, fuzzy) + LanceDB actually written by the outbox; `RetrievalVertical` / `VerticalRouter` route N recall lists per query (routing predicate + declared weight, fail-open extras) and the retriever fuses all routed lists — byte-identical to the two-list form when none are registered | specialized vertical indexes (knowledge graph / code) to register |
| 066 | Trust-aware Fusion (weighted RRF, dedup, independence) | **Delivered and wired** | `rrf_fuse_weighted` + kernel-generic `fuse_ranks`; federated `execute` orders cross-source outcomes by `ReliabilityProfile.success_rate`; the retriever fuses all routed verticals with declared weights (RFC-065); original-publisher detection + `occurrences[]` dedup delivered (RFC-074 slice) | none for the v1 slice |
| 067 | Context Assembly | **Delivered + wired** | `crates/searchos-context/` engine; `SearchRequest.context_budget` → application assembly → `SearchResponse.context` | none for v1 |
| 068 | Token Budget Management | **Delivered + wired** | `ContextBudget` validated at every transport edge; invalid budgets are client errors; OpenAPI documents both schemas | none for v1 |
| 069 | Agent Session Memory | **Partial → memory AND session transcripts are Sources** | `session.rs` + `agent_sessions` (migration v7, commit `ff8b2be`) persists follow-ups across restart; `AgentMemorySource` + `SessionTranscriptSource` both registered as federated sources in the runtime | memory lifecycle (RFC-070, P3) |
| 070 | Memory Lifecycle (forget / consolidate / expire) | **Gap** | `searchos-memory` has bounded in-process sessions but no extraction/consolidation/expiry pipeline | RFC-070 (P3) |
| 071 | Deep Search Engine | **Partial** | bounded iterative acquisition (`max_acquisition_rounds`), `coverage_gaps`, `POST /search/plan`, `POST /search/follow-up`; no task-level research workspace | RFC-071 (P4) |
| 072 | Query Expansion | **Partial** | decomposition for comparison-with-≥2-entities + reformulated follow-up queries; no synonym/sub-question expansion | RFC-072 (P4) |
| 073 | Evidence Coverage | **Have** | `RequirementStatus`, `GapAnalyzer`, `coverage_gaps`, `focus_requirements`, graded qrels / nDCG / MRR / Recall@5 | reuse |
| 074 | Source Independence | **Partial → syndication detection delivered** | host-based counting upgraded to content communities: the same normalized text on N hosts is ONE source for the quality gate's diversity check (`content_community_count` union-find); `merge_evidence` reports syndication sets with the identified original publisher (local → earliest publication → first-seen); context assembly dedups cross-URL same content with `occurrences` | full citation graph / reference-chain analysis |
| 076 | Adaptive Search Planner | **Partial → source-aware delivered** | `SearchDecision` state machine + stage trace; PLUS source-aware decisions (`DecisionInput.external_sources_healthy`): when no healthy web source exists (circuit open / none registered), external-requiring modes reject immediately with `ProviderUnavailable` instead of executing into certain failure; Hybrid keeps serving local evidence; unobserved state preserves the legacy decision | cost-adaptive path selection (quality/coverage-weighted) |
| 077 | Search Cache Intelligence | **Delivered (v1 slice)** | exact/canonical/semantic caches with mode+freshness keys and a 0.92 similarity gate; PLUS hit-time evidence validity (`EvidenceValidityPort`): a cache hit whose cited documents were deleted or moved to a newer version is rejected and recomputed — even when the invalidating write came from ANOTHER process; fail-open on port errors, bounded lookups, `cache.validity_reject_total` metric | near-duplicate semantic validity heuristics |
| 078 | Cost Attribution | **Have** | full cost ledger, reserve→settle, reconciliation, savings views | reuse |
| 079 | Search Benchmark | **Have** | 500-question dataset, CJK corpus, CI eval gate, ranking metrics | reuse |
| 080 | Provider Fallback & Routing | **Partial → v1 federation slice delivered** | ADR-029 multi-key rotation, crawler failure isolation; outcome-driven circuit breaker (3 failures → open, cooldown → half-open probe, success → reset) and a per-source latency budget (planner `Latency` rejection + executor deadline cap + live p50/p95 feedback) | adaptive cost/source-aware routing (RFC-076/080 full scope) |
| 081–083 | Extension SDK / Crawler Extension / Plugin Isolation | **Partial** | versioned extension registry, per-site crawler extensions, fault isolation; **no external-process extension** (Both is frozen) | RFC-081…083 (P5) |
| 084–085 | Distributed Runtime / Multi-node | **Reject for v1** | explicitly *Staged* — single-node first, reserve seams only | later |
| 087 | Privacy-aware Ingestion & Egress | **Delivered (v1 slice)** | workspace isolation, token→workspace binding, rate limiting, redacted config export, fail-closed `PermissionRef.egress_allowed`; versioned ingest-time rule sets (`redact_secrets` secure by default; `redact_pii` opt-in) replacing matches with stable `[REDACTED:rule]` markers before persistence/hash/index with a per-rule audit report; append-only deletion-propagation audit (migration v10, `index_verified` post-drain re-check, `GET /deletions`); AND per-source egress enforcement: workspace-owned content (`searchos-*://`) never reaches the remote reranker unless the operator opts in (`privacy.egress_remote_rerank`, fail-closed, `privacy.egress_blocked_total`) | fine-grained per-source policies beyond the workspace-owned/public split |
| — | LLM answer generation | **Reject** | non-goal in #81 and in the repo's Part 0 | do not build |

### 5. Engineering corrections carried over from the earlier draft

These remain valid under the final numbering:

- **Federation strategy is orthogonal to `SearchMode`.** The draft proposed `FederatedAuto / Parallel / Sequential / Selective` as new `SearchMode` variants. `SearchMode` has exactly 5 variants; `INV-30-18` asserts CLI/HTTP/MCP agreement, `search_mode` is a dashboard SQL filter and cache-key component, and the Tavily compatibility contract is frozen. Adding variants breaks four gates. Use a separate `FederationStrategy` (`ADR-062-008`).
- **Do not fork `SearchResponse`.** It already carries `status`, `warnings`, `task_id`, `replayed`, `coverage_gaps`, `session_id`, `focus_requirements`; `SearchStatus` + `derive_status` are red-line invariants. Extend with `#[serde(default)]` fields only.
- **Do not redefine `SearchBudget`.** It already has query/page/cost/time caps and reserve→execute→settle. Add source fan-out and context-token bounds.
- **Generalise `rrf_fuse`, do not rebuild it.** Keep the two-list signature as a wrapper so the existing suite stays green.
- **Reuse provenance/independence code.** `merge.rs` already handles canonical URLs, conflicts and host counting.
- **Fail-open ≠ quality pass.** `Reject { ProviderUnavailable }` → `insufficient_evidence` stays an invariant (`INV-FED-01`).

### 6. Delivery priority and this iteration

The Master Index orders work **Foundation → Retrieval → Intelligence → Memory → Research → Extension → Distributed** with P0–P6 and milestones M0–M8; **M0–M3 is the first shippable version**.

| Phase | RFCs | This iteration |
|---|---|---|
| P0 | 001–025 verification, 046, 054, 055, **063, 064, 062**, 061 | **Delivered**: final Master Index, bilingual RFC-062/063/064, [P0 dependency review](rfc-062-dependency-review.md), `searchos-federated` kernel (trait + registry + resource model + planner + executor + Phase 2 adapters, 56 tests) |
| P1 | 065, 047, **066**, 050, 039, 080, 032, 079 | weighted RRF delivered and wired (federated execute + retrieval verticals); 065 vertical routing, outcome-driven per-source circuit breaker and per-source latency budget (080 slice) delivered; rest planned (047, 050, 039, 032, 079) |
| P2 | 034, 076, 077, 078, 075, 086, 040 | **Context wiring delivered** (`context_budget` end to end); **077 cache validity delivered** (hit-time evidence-validity gate, cross-process safe); **076 source-aware decisions delivered** (external-requiring modes reject when no healthy source) |
| P3 | 030, 036, 057, 058, **069, 070, 087** | planned (sessions already persist, `ff8b2be`) |
| P4 | 038, 071, 072, 073, 074, 059, **067, 068** | **Delivered**: `searchos-context` engine + assembly, ahead of its slot (pure/additive/independently testable) |
| P5–P6 | extensions, Gateway/Dashboard, distributed | planned / staged |

**Delivered in this iteration (on top of the corrections below):**
`crates/searchos-federated` (062/063/064 kernel + executor + Phase 2 adapters, 56 tests), `rrf_fuse_weighted`
(066, +9 tests), end-to-end `context_budget` wiring (067/068, application +
named test + HTTP round-trip test + MCP + CLI + OpenAPI), the [P0 dependency
review](rfc-062-dependency-review.md), RFC-063/064 spec documents, and README /
index / CHANGELOG status updates. Full suite: green (654+ tests across the workspace; the tree is actively evolving with parallel RFC-087 work).

**Corrections applied to the previous iteration's output:**
1. `rfc-087-federated-search-architecture.md` → **`rfc-062-federated-search-architecture.md`**, with header, dependencies, ADR namespace (`ADR-062-*`), series table and Chinese block rewritten to the final numbering.
2. `docs/gap-analysis-v2.1.md` → **`docs/gap-analysis-master-index.md`** (this file), fully rewritten.
3. `crates/searchos-context/` is now attributed to **RFC-067 + RFC-068**, not RFC-091.
4. `docs/rfc/README.md` / `docs/index.md` / `CHANGELOG.md` now register the **001–087 Master Index**, not a "v2.1 087–094 series".
5. No new RFC numbers were invented — per #81's freeze rule.

### 7. Verification

```bash
cargo test -p searchos-context           # 25 passed
cargo test -p searchos-federated         # 56 passed (10 unit + 25 invariants + 9 execution + 12 adapters)
cargo test -p searchos-retrieval         # 15 passed
cargo test -p searchos-application       # 33 passed
cargo test --workspace                   # green (660+ tests)
cargo fmt --all -- --check               # clean
cargo clippy --workspace --all-targets -- -D warnings   # clean
SRC_DIR=crates/searchos-interface-http/src \
  bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh   # clean
```

The design documents are text-only and cannot break the Rust or frontend gates.

---

## 中文

### 1. 两个分享之间发生了什么

| | 前一个分享（53 节点） | **最终分享（83 节点）** |
|---|---|---|
| 产品定位 | Federated Search & Evidence-Native Knowledge Infrastructure | **Agent Search & Memory Infrastructure**（All-in-One） |
| RFC 规划 | 新增 **087–094** 共 8 份 | **最终目录 001–087**；087–094 明确解散 |
| 联邦检索 | RFC-087 | **RFC-062** |
| 来源注册 | RFC-088 | **RFC-063** |
| 统一资源模型 | RFC-089 | **RFC-064** |
| 信任融合 | RFC-090 | **RFC-066** |
| 上下文 + Token 预算 | RFC-091（一份） | **RFC-067 + RFC-068**（拆分） |
| Agent 记忆 | RFC-092 | **RFC-069 + RFC-070**（拆分） |
| 深度搜索 | RFC-093 | **RFC-071 + RFC-072 + RFC-073**（拆分） |
| 隐私 | RFC-094 | **RFC-087** |

#67 叫停继续写 RFC：*「先暂停继续写 RFC，先把 SearchOS 的完整 RFC 体系、模块边界、依赖关系和开发优先级确定下来」*。#71 冻结四项决策 —— **All-in-One / Both 扩展 / Staged 分布式 / Full Agent Memory**。#81 给出总目录、职责归属、依赖矩阵、P0–P6 优先级、M0–M8 里程碑与 24 节 RFC 模板，并明确：*「087–094 不再重复创建，而是将其内容合并到对应 RFC」*。

### 2. 解散映射（#69 第五节，Privacy 以 #81 为准）

| 早前提案 | 最终归属 | 处理 |
|---|---|---|
| 087 Federated Search Architecture | **062** | 合并 |
| 088 Search Source Registry | **063** | 合并 |
| 089 Unified Resource Model | **064** | 合并 |
| 090 Trust-aware Result Fusion | **066** | 合并 |
| 091 Context Assembly & Token Budget | **067 + 068** | 拆分合并 |
| 092 Agent Session Memory | **069 + 070** | 拆分合并 |
| 093 Deep Search Expansion Engine | **071 + 072 + 073** | 拆分合并 |
| 094 Privacy-aware Ingestion & Egress | **087** | 合并（#69 曾建议 013/017/083） |

### 3. 仓库对照 —— 三处编号冲突

仓库早于 Master Index 存在，冲突**登记而不静默改写**：

| 规划编号 | Master Index 标题 | 该编号下已有文件 | 处理 |
|---|---|---|---|
| 029 | Research Workspace | `rfc-029-local-data-ingestion.md`（已实现） | 保留已交付文件；规划 029 保持**未分配** |
| 031 | Freshness Scheduler | `rfc-031-external-evidence-promotion.md`（已实现） | 保留；规划 031 保持**未分配** |
| 087 | Privacy-aware Ingestion & Egress | 上一轮的 `rfc-087-federated-search-architecture.md` 草案 | **已改名为 `rfc-062-federated-search-architecture.md`**；087 预留给 Privacy |

另外：本仓库**已有** RFC-001…016 的准确标题（`docs/rfc/README.md`），017–025 合并于 `part-17`…`part-24`，因此 #81 中「未恢复原始标题」的说明无需补造内容。`Part 30`（INV-30-01…21）是实现基线，不是编号 RFC。完整对照见 [`docs/rfc/rfc-master-index.md`](rfc/rfc-master-index.md) §11。

### 4. 按最终编号的逐能力差距表

图例：**已有** = 已实现且有测试 · **部分** = 存在但窄于最终规格 · **缺口** = 缺失 · **不做** = 明确非目标。

| 最终 RFC | 要求 | 状态 | 仓库证据 | 动作 |
|---|---|---|---|---|
| 062 | `SearchSource` 抽象 + 联邦 Planner | **Phase 1–3 + 管线接线已交付（含 4b 执行迁移）；P0 残留清单已关闭** | `crates/searchos-federated` 内核/执行器/适配器；`search_service.rs` 在 `federation.strategy` 背后咨询 planner；`execute` 模式经 `FederatedExecutor` 执行 provider 且计费留在服务侧（防双计费门禁通过）；`/search/plan` 报告来源；`/system/status` 与 `GET /sources` 暴露目录 | 无 P0 残留（会话逐字稿 Source、逐源 trace 字段、可靠性权重融合均已接线） |
| 063 | 带作用域的 Source Registry | **内核已交付** | `InMemorySourceRegistry`：优先级解析、过滤器、跨作用域禁用、一 id 一描述符 | Gateway 交付面时做 SQLite 投影 |
| 064 | 超越 `Document` 的统一资源模型 | **内核已交付** | `SearchResource` + 11 种 `ResourceType`、稳定身份、哈希优先去重、fail-closed `PermissionRef`、egress 默认禁止 | 适配器映射文档/会话行 |
| 065 | 混合检索（FTS + BM25 + KNN） | **部分 → 垂直路由已交付** | `HybridRetriever` + 带权 RRF + Tantivy（CJK 二元分词、字面词查询、标题加权、模糊）+ outbox 真实写入 LanceDB；`RetrievalVertical` / `VerticalRouter` 按查询路由 N 条召回列表（路由谓词 + 声明权重，额外垂直 fail-open），检索器融合全部路由列表 —— 未注册垂直时与双列表形式逐字节一致 | 待注册专用垂直索引（知识图谱 / 代码） |
| 066 | 信任融合（带权 RRF、去重、独立性） | **已交付并接线** | `rrf_fuse_weighted` + 内核 `fuse_ranks`；联邦 `execute` 跨源排序按 `ReliabilityProfile.success_rate` 加权；检索器按声明权重融合全部路由垂直列表（RFC-065）；原始发布方识别 + `occurrences[]` 去重已交付（RFC-074 切片） | v1 切片无剩余 |
| 067 | Context Assembly | **已交付并接入** | `crates/searchos-context/` 引擎；`SearchRequest.context_budget` → application 装配 → `SearchResponse.context` | v1 无剩余 |
| 068 | Token Budget Management | **已交付并接入** | `ContextBudget` 在每个传输边缘校验；非法预算是客户端错误；OpenAPI 已登记两个 schema | v1 无剩余 |
| 069 | Agent Session Memory | **部分 → 记忆与会话逐字稿均为 Source** | `session.rs` + `agent_sessions`（migration v7，提交 `ff8b2be`）已支持重启后 follow-up；`AgentMemorySource` + `SessionTranscriptSource` 均已在 runtime 注册为联邦 Source | 记忆生命周期（RFC-070，P3） |
| 070 | Memory Lifecycle（遗忘/归纳/过期） | **缺口** | `searchos-memory` 只有有界进程内会话，无提取/巩固/过期流水线 | RFC-070（P3） |
| 071 | Deep Search Engine | **部分** | 有界迭代采集（`max_acquisition_rounds`）、`coverage_gaps`、`POST /search/plan`、`POST /search/follow-up`；无任务级 Research Workspace | RFC-071（P4） |
| 072 | Query Expansion | **部分** | 比较类 ≥2 实体拆解 + 追问改写；无同义词/子问题扩展 | RFC-072（P4） |
| 073 | Evidence Coverage | **已有** | `RequirementStatus`、`GapAnalyzer`、`coverage_gaps`、`focus_requirements`、分级 qrels / nDCG / MRR / Recall@5 | 复用 |
| 074 | Source Independence | **部分 → 同文识别已交付** | 按 host 计数升级为「内容社区」：同一规范化文本出现在 N 个 host 上，质量门禁的多样性检查只算一个来源（`content_community_count` 并查集）；`merge_evidence` 报告同文集合与识别出的原始发布方（本地 → 最早发布 → 首见）；context 装配对跨 URL 同文去重并记录 `occurrences` | 完整引用图 / 引用链分析 |
| 076 | Adaptive Search Planner | **部分 → 来源感知已交付** | `SearchDecision` 状态机 + stage trace；另加来源感知决策（`DecisionInput.external_sources_healthy`）：无健康外部来源（熔断打开 / 未注册）时，强制外部模式立即以 `ProviderUnavailable` 拒绝，而不是执行必败计划；Hybrid 继续提供本地证据；未观察状态保持既有决策 | 成本自适应路径选择（按质量/覆盖加权） |
| 077 | Search Cache Intelligence | **已交付（v1 切片）** | exact/canonical/semantic 缓存、mode+freshness 键、0.92 相似度门；另加命中时证据有效性校验（`EvidenceValidityPort`）：命中引用的文档被删除或换版本即拒绝并重算 —— 即使失效写入来自另一个进程；端口故障 fail-open、查询次数有上界、`cache.validity_reject_total` 指标 | 近重复语义有效性启发 |
| 078 | Cost Attribution | **已有** | 完整成本账本、reserve→settle、对账、节省视图 | 复用 |
| 079 | Search Benchmark | **已有** | 500 题数据集、中文语料、CI eval gate、排序指标 | 复用 |
| 080 | Provider Fallback & Routing | **部分 → v1 联邦切片已交付** | ADR-029 多 key 轮换、爬虫故障隔离；执行结果驱动的熔断（3 次失败开、cooldown 半开探针、成功复位）+ 逐源延迟预算（planner `Latency` 拒绝、executor deadline 截断、实时 p50/p95 反馈） | 成本/来源自适应路由（RFC-076/080 完整范围） |
| 081–083 | Extension SDK / Crawler / Isolation | **部分** | 版本化扩展注册表、站点专属爬虫、故障隔离；**无外部进程扩展**（Both 已冻结） | RFC-081…083（P5） |
| 084–085 | 分布式 / 多节点 | **v1 不做** | 明确 *Staged* —— 单机优先，只预留接缝 | 后续 |
| 087 | Privacy-aware Ingestion & Egress | **已交付（v1 切片）** | workspace 隔离、token→workspace 绑定、限流、脱敏配置导出、fail-closed `PermissionRef.egress_allowed`；版本化摄取时规则（`redact_secrets` 默认开启；`redact_pii` 主动开启）在持久化前替换为稳定标记并给出逐规则审计报告；只增不改的删除传播审计（migration v10，`index_verified` 排空后复查，`GET /deletions`）；以及**逐源外发执行**：workspace 自有内容（`searchos-*://`）在操作者主动开启（`privacy.egress_remote_rerank`，fail-closed，`privacy.egress_blocked_total`）之前绝不进入远程 reranker | 超出「workspace 自有 vs 公开」二分的细粒度逐源策略 |
| — | LLM 答案生成 | **不做** | #81 与仓库 Part 0 均列为非目标 | 不实现 |

### 5. 从前一轮草案保留下来的工程修正

在最终编号下依然成立：

- **联邦策略与 `SearchMode` 正交。** 草案想把 `FederatedAuto / Parallel / Sequential / Selective` 加进 `SearchMode`；该枚举恰好 5 个变体，`INV-30-18` 要求各传输层一致，`search_mode` 还是看板 SQL 筛选与缓存键，Tavily 契约已冻结 —— 新增变体会打穿四道门禁。改用独立的 `FederationStrategy`（`ADR-062-008`）。
- **不要分裂 `SearchResponse`。** 已有 `status` / `warnings` / `task_id` / `replayed` / `coverage_gaps` / `session_id` / `focus_requirements`；`SearchStatus` + `derive_status` 是红线。只用 `#[serde(default)]` 加法扩展。
- **不要重定义 `SearchBudget`。** 已有查询/页面/成本/时间上限与 reserve→execute→settle；补 source fan-out 与 context token 上界即可。
- **泛化 `rrf_fuse`，不重建。** 保留双列表签名作包装，让现有测试套件保持通过。
- **复用 provenance / 独立性代码。** `merge.rs` 已处理规范化 URL、冲突与 host 计数。
- **Fail-open ≠ 质量通过。** `Reject { ProviderUnavailable }` → `insufficient_evidence` 保持不变量（`INV-FED-01`）。

### 6. 交付优先级与本轮

Master Index 的推进顺序是 **Foundation → Retrieval → Intelligence → Memory → Research → Extension → Distributed**，P0–P6 + 里程碑 M0–M8；**M0–M3 是第一个可独立交付版本**。

| 阶段 | RFC | 本轮 |
|---|---|---|
| P0 | 001–025 核对、046、054、055、**063、064、062**、061 | **已交付**：最终 Master Index、双语 RFC-062/063/064、[P0 依赖审查](rfc-062-dependency-review.md)、`searchos-federated` 内核（trait + registry + 资源模型 + planner + 执行器 + Phase 2 适配器，56 测试） |
| P1 | 065、047、**066**、050、039、080、032、079 | 带权 RRF 已交付并接线（联邦 execute + 检索垂直列表）；065 垂直路由、逐源熔断 + 逐源延迟预算（080 切片）已交付；其余计划中（047、050、039、032、079） |
| P2 | 034、076、077、078、075、086、040 | **上下文接入已交付**（`context_budget` 全链路）；**077 缓存有效性已交付**（四层缓存命中时证据校验，跨进程安全）；**076 来源感知决策已交付**（无健康来源时强制外部模式直接拒绝） |
| P3 | 030、036、057、058、**069、070、087** | 计划中（会话已持久化，`ff8b2be`） |
| P4 | 038、071、072、073、074、059、**067、068** | **已交付**：`searchos-context` 引擎与装配，先于其位置落地（纯函数、加法式、可独立验证） |
| P5–P6 | 扩展、Gateway/Dashboard、分布式 | 计划中 / 分阶段 |

**对上一轮产出的修正：**
1. `rfc-087-federated-search-architecture.md` → **`rfc-062-federated-search-architecture.md`**，头部、依赖、ADR 命名空间（`ADR-062-*`）、系列表与中文块全部按最终编号重写。
2. `docs/gap-analysis-v2.1.md` → **`docs/gap-analysis-master-index.md`**（本文件），全文重写。
3. `crates/searchos-context/` 归属改为 **RFC-067 + RFC-068**，不再是 RFC-091。
4. `docs/rfc/README.md` / `docs/index.md` / `CHANGELOG.md` 登记的是 **001–087 Master Index**，不再是「v2.1 087–094 系列」。
5. 未新增任何 RFC 编号 —— 遵守 #81 的冻结规则。

### 7. 验证

```bash
cargo test -p searchos-context           # 25 passed
cargo test -p searchos-federated         # 56 passed (10 unit + 25 invariants + 9 execution + 12 adapters)
cargo test -p searchos-retrieval         # 15 passed
cargo test -p searchos-application       # 33 passed
cargo test --workspace                   # green (660+ tests)
cargo fmt --all -- --check               # clean
cargo clippy --workspace --all-targets -- -D warnings   # clean
SRC_DIR=crates/searchos-interface-http/src \
  bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh   # clean
```

设计文档为纯文本，不会影响 Rust 与前端门禁。
