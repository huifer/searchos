# RFC Coverage Matrix (RFC-001…087)

> Single authoritative status table for the frozen RFC catalogue.
> 一个权威的 RFC 覆盖状态表。

**As of:** 2026-11-19 working tree · **Workspace version:** 0.2.0 · **Verified v0.2.0 release suite:** 865 tests / 0 failed

This tree adds RFC-060 on top of that release; its named storage/HTTP gates, compatibility/runtime/integration/e2e suites, clippy and SDK builds are locally green. It is not yet a tagged release.

This table answers one question per RFC: *is the capability in the repository
today, and if so, how far?* It supersedes the scattered status notes in
[`gap-analysis-master-index.md`](gap-analysis-master-index.md) §4 (which remains
the detailed evidence log for RFC-062…087) and the historical planning tables
in [`feature-roadmap.md`](feature-roadmap.md) / [`roadmap.md`](roadmap.md).

**Current totals:** 74 Delivered · 9 Partial · 4 Reject (87 numbered slots; the
four Rejects are frozen v1 non-goals, so 74 of 83 in-scope slots are fully
delivered).

**Legend**

| Status | Meaning |
|---|---|
| **Delivered** | Implemented and gated by named tests; the v1 scope of the RFC is in the repo. |
| **Partial** | Something real exists, but narrower than the RFC's full scope. The gap is named in the row. |
| **Gap** | Not implemented. |
| **Reject** | Explicit v1 non-goal (a frozen scope decision, not missing work). |

**Numbering notes.** Three collisions are recorded, never silently rewritten:
planned **029** (Research Workspace) and planned **031** (Freshness Scheduler)
collide with the shipped `rfc-029-local-data-ingestion.md` and
`rfc-031-external-evidence-promotion.md`; the shipped files keep their
numbers. The planned 087 was Federated Search in an earlier draft — it was
renamed to `rfc-062-federated-search-architecture.md`, and 087 became Privacy
per turn #81.

---

## English

| RFC | Capability | Status | Evidence / remaining gap |
|---|---|---|---|
| 001–025 | Legacy Foundation Baseline (product architecture → deployment) | **Delivered** | `docs/rfc/rfc-001…016` + annexes 17–24; the whole workspace, CLI, gateway, MCP and extension seams shipped in v0.1.0. |
| 026 | Visualization & Analytics Architecture | **Delivered** | `crates/searchos-observability`, `docs/analytics` read models, console `/analytics`. |
| 027 | Analytics Data Model & Aggregation | **Delivered** | migrations v3 `analytics_events` + `analytics_read_models`; `AnalyticsRepo::rebuild` / `query_dataset`. |
| 028 | Dashboard API & Frontend Integration | **Delivered** | `/api/v1/dashboard/*` over the Read Model; Next.js console in `web/`. |
| 029 | Local Data Ingestion (shipped file) | **Delivered** | `IngestService`, `POST /ingest{,/file,/batch}`, `GET /ingest/jobs/:id`, CLI `ingest`. The *planned* "Research Workspace" scope moved to 071. |
| 030 | Knowledge Management | **Delivered (v1)** | Document lifecycle (create/version/reindex/soft-delete) plus workspace-scoped **collections** (deterministic ids, live member counts, HTTP + CLI). Gap: taxonomy/tag hierarchies beyond flat collections. |
| 031 | External Evidence Promotion (shipped file) | **Delivered** | promotion state machine, robots/fetch-failure skip reasons, per-task caps, reuse accounting, `GET /evidence/*`. The *planned* "Freshness Scheduler" scope is partially covered by 057 + freshness profiles. |
| 032 | Source Trust | **Partial** | content communities + original-publisher detection (074), per-source reliability and latency (080) exist. Gap: publisher-level trust scoring. |
| 033 | Online Feedback | **Delivered (v1)** | append-only human judgements on retrieved results (`canonical`=3 / `helpful`=2 / `related`=1 / `irrelevant`/`wrong`=0) via `POST /feedback`, CLI `feedback add|list|qrels|dataset`; aggregation only accepts a verdict with `min_agreement` votes and NO tie — contradictory pairs are `disputed` and excluded, never averaged into a fabricated label. `GET /feedback/qrels` + `/feedback/dataset` export graded `EvaluationDataset` JSONL that `searchos eval run` and `watch.dataset_path` consume directly — the learning loop closes. Workspace-scoped, notes never sent to a provider. Gap: no UI for collecting judgements; no model training from feedback (v1 only produces qrels). |
| 034 | Policy Management | **Delivered (v1)** | versioned append-only policies, live pointer, rollback; `GET/PUT /policies*`, CLI `policy`. |
| 035 | Search Watch | **Delivered (v1 + dispatch)** | deterministic quality watchdog over the Analytics Read Model: optional quality/failure/trend thresholds, an optional labelled-dataset ranking check, append-only snapshots (migration v16), `searchos watch run|history`, `POST /watch/run`, `GET /watch/snapshots`. An unavailable signal is `skipped` (never a pass), a trend only fires when BOTH snapshots have `min_sample` observations, and a regression is a result — not an HTTP error. **Dispatch**: `watch.notify_on` statuses (default `regressed`+`degraded`) drive a log notifier plus an optional webhook that receives the report ONLY (no document bodies, queries or notes), and `watch.schedule_interval_seconds` runs it inside the gateway with a clean shutdown. A failing notifier is recorded in the run, never raised. Gap: no per-notifier retry/backoff and no email/Slack-native formatting (webhook JSON only). |
| 036 | Document Intelligence | **Delivered (v1)** | deterministic extractive summary + keywords computed at ingest, persisted per version (migration v14), surfaced by `document inspect` and `GET /documents/:id`. Extractive by design — no generated text. |
| 037 | Multilingual Search | **Delivered (v1)** | deterministic cross-lingual recall via a curated bidirectional term dictionary (`searchos-query::crosslingual`, 34 domain pairs): a Chinese query generates ONE translated recall list (`intelligence.cross_lingual = true`, opt-in), fused with the original results by weighted RRF — the original query is never replaced, and a cross-language variant in the external plan competes for the SAME query budget. No LLM, no network, fully reproducible and auditable. Gap: term-level translation only, not full grammatical MT; the table is a starter set, not a general dictionary. |
| 038 | Evidence Graph | **Delivered (v1)** | `duplicate_of` / `same_content` edges on the response, built from the merge. |
| 039 | Provider Intelligence | **Delivered (v1)** | key rotation (ADR-029), health, outcome-driven circuit breaker, live p50/p95, and **quality-signal provider selection**: `federation.selection = cost_adaptive` orders sources by `expected_quality` (observed success × declared trust × learned judged-result quality once ≥3 samples) per unit cost, and `/search/plan` exposes `source_scores` (`expected_quality`, `cost_micros`, `value_density`, `selected`) for every eligible source, selected or rejected. Default `reliability_first` is unchanged. |
| 040 | Search Replay | **Delivered (v1)** | `POST /replay/{trace_id}` replays a recorded decision offline with an optional what-if policy. |
| 041 | Identity & RBAC | **Reject** | managed multi-tenant RBAC/SSO is an explicit v1 non-goal; the workspace token is the only identity. |
| 042 | Usage, Rate Limiting & Billing | **Partial** | rate limiting and the cost ledger exist. Gap: money billing / invoicing. |
| 043 | Backup & Disaster Recovery | **Delivered** | `backup create|restore` (`VACUUM INTO`, checksum manifest, refuses tampered backups) + CI drill that once caught two real bugs. |
| 044 | Health & Operations | **Delivered** | `/health`, `/system/status`, `doctor`, `/metrics`, graceful shutdown. |
| 045 | Import, Export & Portability | **Delivered** | ingest paths, `export` (secrets never exported), OpenAPI + SDKs. |
| 046 | Result Normalization | **Delivered** | canonical URL implementation, `merge_evidence` normalization and conflict retention. |
| 047 | Ranking Engine | **Delivered** | weighted RRF fusion, title boost, fuzzy fallback, feature + remote rerankers, vertical routing. |
| 048 | Vertical Search | **Delivered (v1)** | `RetrievalVertical` / `VerticalRouter` with declared weights (RFC-065 slice). |
| 049 | Structured Extraction | **Delivered (v1)** | deterministic structure extraction in `searchos-document::structures`: fenced code blocks (with language), markdown tables (parsed into header-keyed RECORDS — the table's own header names, nothing invented), lists, headings (level) and links, each block carrying its 1-based source line so it is traceable. `POST /structures` (text or a preview URL — nothing persisted) and CLI `searchos structures --text|--file|--url`. No LLM, no interpretation, fully reproducible. Gap: no schema-driven extraction (a caller-defined field mapping), and HTML tables are not converted to markdown upstream. |
| 050 | Source Discovery | **Partial** | source registry + `GET /sources` catalog exist. Gap: automatic source discovery. |
| 051 | Crawl Frontier | **Partial** | crawler selection, robots/Crawl-delay, failure isolation. Gap: frontier scheduling across hosts. |
| 052 | Browser Rendering | **Reject** | browser engine is an explicit v1 non-goal. |
| 053 | Content Extraction | **Delivered** | `FetchService` extraction pipeline, HTML sanitization, Tavily-shaped `/extract`. |
| 054 | Task Orchestration | **Partial** | bounded research rounds, background outbox worker, budget reserve→settle. Gap: general job scheduling. |
| 055 | Cancellation & Deadline | **Delivered** | absolute request deadlines, per-source timeout caps, cooperative cancellation, MCP `notifications/cancelled` acknowledged. |
| 056 | Partial Streaming | **Partial** | SSE dashboard event stream exists. Gap: per-result token streaming. |
| 057 | Temporal Knowledge | **Delivered (v1)** | `as_of` historical views: freshness judged at an instant, future documents excluded. |
| 058 | Entity Resolution | **Delivered (v1)** | entity alias registry, `POST /entities/resolve`, `entity_id` on results. |
| 059 | Contradiction Detection | **Partial** | conflicting claims are retained and force `QualityStatus::Conflict`. Gap: a contradiction graph over claims. |
| 060 | Agent Registry | **Delivered (v1)** | Declared, workspace-scoped agent identities over additive migration v19: normalized slug ids (trim + lowercase, `[a-z0-9._-]`, ≤64 chars), display name, description, enabled lifecycle flag and advisory request budget. Storage `AgentRepo` upserts while preserving `created_at`, lists by workspace, enables/disables; HTTP `GET/POST /agents`, `GET /agents/:id`, `POST /agents/:id/enabled`; CLI `agent list|register|show|enable|disable`; OpenAPI + both SDKs. Disabled agents keep analytics/trace history. Gap: enforcement of the quota flag and of `enabled` at search admission is not wired (the rate limiter remains the executor). |
| 061 | SDK | **Delivered** | TypeScript + Python clients covering every OpenAPI path, refreshed to 0.2.0. |
| 062 | Federated Search Architecture | **Delivered (v1)** | `SearchSource` kernel, planner with auditable rejection chain, executor with per-source deadline/isolation, `execute` mode with service-side billing, `GET /sources`. |
| 063 | Source Registry | **Delivered (v1)** | scoped registry, precedence resolution, live reliability + latency profiles, circuit breaker. |
| 064 | Unified Resource Model | **Delivered (v1)** | `SearchResource` with 11 resource types, stable identity, fail-closed `PermissionRef`, egress default off. |
| 065 | Hybrid Retrieval (FTS + BM25 + KNN) | **Delivered (v1)** | Tantivy (CJK bigrams, literal-term, title boost, fuzzy) + LanceDB written by the outbox, weighted RRF, vertical routing. Gap: specialized knowledge-graph / code verticals. |
| 066 | Trust-aware Result Fusion | **Delivered (v1)** | `rrf_fuse_weighted` + kernel-generic `fuse_ranks`; reliability-weighted cross-source ordering; syndication dedup. |
| 067 | Context Assembly | **Delivered** | `crates/searchos-context/`; `SearchRequest.context_budget` → `SearchResponse.context`. |
| 068 | Token Budget Management | **Delivered** | `ContextBudget` validated at every transport edge; invalid budgets are client errors. |
| 069 | Agent Session Memory | **Delivered (v1)** | `agent_sessions` (migration v7) survives restarts; session transcripts are a federated source. |
| 070 | Memory Lifecycle | **Delivered (v1)** | retention-driven expiry, consolidation by normalized-text fingerprint, dry-run by default (`searchos memory lifecycle --apply`). |
| 071 | Deep Search Engine | **Delivered (v1)** | research workspace with bounded rounds, durable evidence, validity-gated carry-over (`workspace_evidence`), `POST /research/*`, CLI `research`. Gap: scheduled research tasks. |
| 072 | Query Expansion | **Delivered (v1)** | synonym + sub-question expansion with an explicit mode; never exceeds the budget. Gap: semantic query rewriting. |
| 073 | Evidence Coverage | **Delivered** | `RequirementStatus`, `GapAnalyzer`, `coverage_gaps`, `focus_requirements`, graded qrels. |
| 074 | Source Independence | **Delivered (v1)** | content-community counting, original-publisher identification, cross-URL same-content collapse. |
| 075 | Search Policy Simulation | **Delivered (v1)** | offline replay with what-if policies. |
| 076 | Adaptive Search Planner | **Delivered (v1)** | decision state machine + stage trace; source-aware decisions; cost-adaptive plan downsizing. |
| 077 | Search Cache Intelligence | **Delivered (v1)** | exact/canonical/semantic caches with mode+freshness keys and a 0.92 similarity gate; hit-time evidence validity across processes. |
| 078 | Cost Attribution | **Delivered** | full cost ledger, reserve→execute→settle, reconciliation, savings views. |
| 079 | Search Benchmark | **Delivered** | 500-question dataset, CJK corpus, CI eval gate, MRR / nDCG@5 / Recall@5. |
| 080 | Provider Fallback & Routing | **Delivered (v1 slice + result-level learning)** | multi-key rotation, failure isolation, outcome-driven circuit breaker, per-source latency budget, **cost-adaptive source selection** (`federation.selection = cost_adaptive`: expected quality per unit cost instead of configured priority, with locality and requirement authority still dominating), AND **result-level quality learning**: after each non-cached search the router learns, per source, what fraction of that source's JUDGED results were relevant. Judgements come from RFC-033 feedback (and an optional evaluated dataset); only judged results count, an unjudged source is never penalised, the score is an EMA, and it persists across restarts (migration v18). Gap: learning is global per source (not per workspace or per query class), and the legacy (non-federated) path learns only where `SearchResultItem.source` names a registered source. |
| 081 | Extension SDK | **Partial** | versioned extension registry, manifest discovery, quarantine. Gap: external-process extensions (frozen "Both" decision). |
| 082 | Crawler Extension System | **Partial** | per-site crawler extensions with isolation. |
| 083 | Plugin Isolation | **Partial** | fault isolation for native extensions. Gap: sandboxed external plugins. |
| 084 | Distributed Search Runtime | **Reject** | frozen *Staged* decision — single node first, seams reserved only. |
| 085 | Multi-node Coordination | **Reject** | same frozen decision. |
| 086 | Search Intelligence Evaluation | **Delivered (v1)** | batch replay evaluation + CI gates over keyword coverage, ranking metrics and p95 latency. |
| 087 | Privacy-aware Ingestion & Egress | **Delivered (v1)** | workspace isolation, token→workspace binding, redacted config export, fail-closed egress with per-kind overrides, ingest-time redaction with an audit report, append-only deletion-propagation audit (`GET /deletions`). |
| — | LLM answer generation | **Reject** | non-goal by decision: SearchOS serves evidence, not generated answers. |

---

## 中文

| RFC | 能力 | 状态 | 证据 / 剩余差距 |
|---|---|---|---|
| 001–025 | 遗留基础基线（产品架构 → 部署） | **Delivered** | `docs/rfc/rfc-001…016` + 附件 17–24；整个 workspace、CLI、网关、MCP 与扩展接缝已在 v0.1.0 交付。 |
| 026 | 可视化与分析架构 | **Delivered** | `crates/searchos-observability`、分析读模型、控制台 `/analytics`。 |
| 027 | 分析数据模型与聚合 | **Delivered** | migration v3 `analytics_events` + `analytics_read_models`；`AnalyticsRepo::rebuild` / `query_dataset`。 |
| 028 | 看板 API 与前端集成 | **Delivered** | `/api/v1/dashboard/*` 基于读模型；`web/` 下的 Next.js 控制台。 |
| 029 | 本地数据导入（已交付文件） | **Delivered** | `IngestService`、`POST /ingest{,/file,/batch}`、`GET /ingest/jobs/:id`、CLI `ingest`。*规划中的*「Research Workspace」范围已并入 071。 |
| 030 | 知识管理 | **Delivered (v1)** | 文档生命周期（创建/版本/重建索引/软删除）+ 工作区**知识集合**（确定性 id、存活成员计数、HTTP + CLI）。差距：集合之上的标签/层级分类。 |
| 031 | 外部证据沉淀（已交付文件） | **Delivered** | 沉淀状态机、robots/抓取失败跳过原因、单任务上限、复用统计、`GET /evidence/*`。*规划中的*「Freshness Scheduler」范围由 057 + 新鲜度画像部分覆盖。 |
| 032 | 来源信任 | **Delivered (v1)** | 内容社群与原始发布方判定（074）、来源级可靠性/延迟（080）、质量信号选源（039），以及发布方级信任评分：按规范发布方 host 聚合只追加反馈为 `PublisherTrust`；达到 ≥2 个判定才有 `trust = relevant/judged`，`related` 不算 relevant，低样本发布方是 `trust: null`（未知，不是坏）。HTTP `GET /publishers`、CLI `publisher list`、OpenAPI + 两个 SDK。差距：信任已暴露可审计，但尚未参与 `results` 重排。 |
| 033 | 在线反馈 | **Delivered (v1)** | 对检索结果的只追加人工判断（`canonical`=3 / `helpful`=2 / `related`=1 / `irrelevant`/`wrong`=0），通过 `POST /feedback` 和 CLI `feedback add|list|qrels|dataset` 录入；聚合只接受达到 `min_agreement` 票且**无平票**的判断——矛盾对被标记为 `disputed` 并排除，绝不平均出一个伪造的标签。`GET /feedback/qrels` 与 `/feedback/dataset` 导出分级 `EvaluationDataset` JSONL，直接喂给 `searchos eval run` 和 `watch.dataset_path`——学习闭环就此合拢。工作区隔离，备注绝不发给 provider。差距：没有收集判断的 UI；v1 只用反馈产出 qrels，不做模型训练。 |
| 034 | 策略管理 | **Delivered (v1)** | 版本化只追加策略、生效指针、回滚；`GET/PUT /policies*`、CLI `policy`。 |
| 035 | 检索监控订阅 | **Delivered (v1 + 分发)** | 基于分析读模型的确定性质量看门狗：可选的质量/失败率/趋势阈值、可选标注数据集排序检查、只追加快照（migration v16）、`searchos watch run|history`、`POST /watch/run`、`GET /watch/snapshots`。信号不可用即为 `skipped`（绝不当作通过），趋势只有在前后两次快照都达到 `min_sample` 时才触发，退化是结果而不是 HTTP 错误。**分发**：`watch.notify_on` 指定触发状态（默认 `regressed`+`degraded`），驱动日志通知器与可选 webhook（只发报告，不含文档正文、查询或备注），`watch.schedule_interval_seconds` 让网关内置调度并能干净停机。通知失败会记录在本次运行里，绝不抛出。差距：没有按通知器的重试/退避，也没有邮件/Slack 原生格式（仅 webhook JSON）。 |
| 036 | 文档智能 | **Delivered (v1)** | 摄取时计算确定性抽取式摘要 + 关键词，随版本持久化（migration v14），`document inspect` 与 `GET /documents/:id` 可见。刻意只做抽取式——不生成文本。 |
| 037 | 多语言检索 | **Delivered (v1)** | 通过精选双向术语词典（`searchos-query::crosslingual`，34 条领域术语对）实现确定性跨语言召回：中文查询生成一条翻译召回列表（`intelligence.cross_lingual = true`，可选启用），与原始结果用加权 RRF 融合——原查询绝不被替换，外部计划里的跨语言变体与其余扩展**共享同一查询预算**。无 LLM、无网络，完全可复现可审计。差距：只做术语级翻译，不做完整语法机器翻译；词典是起步集，不是通用词典。 |
| 038 | 证据图 | **Delivered (v1)** | 响应上的 `duplicate_of` / `same_content` 边，由合并结果构建。 |
| 039 | Provider 智能 | **Delivered (v1)** | 多 key 轮换（ADR-029）、健康检查、结果驱动的熔断、实时 p50/p95，以及**按质量信号选 provider**：`federation.selection = cost_adaptive` 按单位成本的 `expected_quality`（观测成功率 × 声明信任 × 达到 ≥3 个判定样本后的结果质量）排序；`/search/plan` 对每个合格来源（无论选中与否）输出 `source_scores`（`expected_quality`、`cost_micros`、`value_density`、`selected`）。默认 `reliability_first` 不变。 |
| 040 | 检索重放 | **Delivered (v1)** | `POST /replay/{trace_id}` 离线重放已记录决策，可带假设策略。 |
| 041 | 身份与 RBAC | **Reject** | 托管多租户 RBAC/SSO 是明确的 v1 非目标；工作区 token 是唯一身份。 |
| 042 | 用量、限流与计费 | **Partial** | 限流与成本账本已有。差距：金额计费/开票。 |
| 043 | 备份与灾难恢复 | **Delivered** | `backup create|restore`（`VACUUM INTO`、校验清单、拒绝被篡改备份）+ CI 演练（曾借此发现两个真实 bug）。 |
| 044 | 健康与运维 | **Delivered** | `/health`、`/system/status`、`doctor`、`/metrics`、优雅停机。 |
| 045 | 导入、导出与可移植 | **Delivered** | 导入路径、`export`（绝不导出密钥）、OpenAPI + SDK。 |
| 046 | 结果归一化 | **Delivered** | 统一 canonical URL 实现、`merge_evidence` 归一化与冲突保留。 |
| 047 | 排序引擎 | **Delivered** | 加权 RRF 融合、标题加权、模糊回退、特征 + 远程 reranker、垂直路由。 |
| 048 | 垂直检索 | **Delivered (v1)** | `RetrievalVertical` / `VerticalRouter` + 声明权重（RFC-065 切片）。 |
| 049 | 结构化抽取 | **Delivered (v1)** | `searchos-document::structures` 确定性抽取：围栏代码块（含语言）、Markdown 表格（解析成以**表头自身字段名**为键的记录——不臆造字段）、列表、标题（层级）与链接，每个块都带 1-based 源行号，可回溯。`POST /structures`（文本或预览 URL，不落库）与 CLI `searchos structures --text|--file|--url`。无 LLM、无解释性加工、完全可复现。差距：没有 schema 驱动的抽取（调用方自定义字段映射），且上游 HTML 表格未转成 Markdown。 |
| 050 | 来源发现 | **Partial** | 来源注册表 + `GET /sources` 目录已有。差距：自动发现来源。 |
| 051 | 爬取前沿 | **Partial** | 爬虫选择、robots/Crawl-delay、失败隔离已有。差距：跨主机的调度前沿。 |
| 052 | 浏览器渲染 | **Reject** | 浏览器引擎是明确的 v1 非目标。 |
| 053 | 内容抽取 | **Delivered** | `FetchService` 抽取管线、HTML 清洗、Tavily 形状的 `/extract`。 |
| 054 | 任务编排 | **Partial** | 有界研究轮次、后台 outbox worker、预算 reserve→settle 已有。差距：通用作业调度。 |
| 055 | 取消与截止时间 | **Delivered** | 绝对请求截止时间、按来源的超时上限、协作式取消、MCP `notifications/cancelled` 已被确认。 |
| 056 | 部分流式 | **Partial** | 看板 SSE 事件流已有。差距：按结果/按 token 的流式返回。 |
| 057 | 时态知识 | **Delivered (v1)** | `as_of` 历史视图：在某一时刻判定新鲜度，未来文档被排除。 |
| 058 | 实体消解 | **Delivered (v1)** | 实体别名注册表、`POST /entities/resolve`、结果上的 `entity_id`。 |
| 059 | 矛盾检测 | **Partial** | 冲突声明被保留并强制 `QualityStatus::Conflict`。差距：声明之上的矛盾图。 |
| 060 | Agent 注册表 | **Delivered (v1)** | 基于增量 migration v19 的、按工作区声明的 agent 身份：规范化 slug（trim + 小写、`[a-z0-9._-]`、≤64 字符）、显示名、描述、enabled 生命周期标记与建议性请求配额。存储层 `AgentRepo` upsert 时保留 `created_at`，支持按工作区列出、启用/禁用；HTTP `GET/POST /agents`、`GET /agents/:id`、`POST /agents/:id/enabled`；CLI `agent list|register|show|enable|disable`；OpenAPI 与两个 SDK 同步。禁用 agent 保留分析与 trace 历史。差距：配额与 `enabled` 尚未接到搜索准入强制（限流器仍是执行者）。 |
| 061 | SDK | **Delivered** | TypeScript + Python 客户端覆盖全部 OpenAPI 路径，已刷新到 0.2.0。 |
| 062 | 联邦检索架构 | **Delivered (v1)** | `SearchSource` 内核、带审计拒绝链的 planner、带按来源截止时间/隔离的 executor、`execute` 模式在服务端计费、`GET /sources`。 |
| 063 | 来源注册表 | **Delivered (v1)** | 作用域注册表、优先级解析、实时可靠性 + 延迟画像、熔断器。 |
| 064 | 统一资源模型 | **Delivered (v1)** | `SearchResource` 含 11 种资源类型、稳定身份、失败即拒绝的 `PermissionRef`、默认关闭的外发。 |
| 065 | 混合检索（FTS + BM25 + KNN） | **Delivered (v1)** | Tantivy（CJK bigram、字面词、标题加权、模糊）+ 由 outbox 写入的 LanceDB、加权 RRF、垂直路由。差距：知识图谱/代码等专用垂直。 |
| 066 | 信任感知融合 | **Delivered (v1)** | `rrf_fuse_weighted` + 内核通用 `fuse_ranks`；按可靠性的跨来源排序；同文去重。 |
| 067 | 上下文装配 | **Delivered** | `crates/searchos-context/`；`SearchRequest.context_budget` → `SearchResponse.context`。 |
| 068 | Token 预算管理 | **Delivered** | `ContextBudget` 在每个传输边界校验；非法预算视为客户端错误。 |
| 069 | Agent 会话记忆 | **Delivered (v1)** | `agent_sessions`（migration v7）跨重启存活；会话转录是联邦来源。 |
| 070 | 记忆生命周期 | **Delivered (v1)** | 保留期驱动的过期、按归一化文本指纹的合并、默认 dry-run（`searchos memory lifecycle --apply`）。 |
| 071 | 深度检索引擎 | **Delivered (v1)** | 研究工作区：有界轮次、持久证据、带有效性校验的证据承接（`workspace_evidence`）、`POST /research/*`、CLI `research`。差距：定时研究任务。 |
| 072 | 查询扩展 | **Delivered (v1)** | 同义词 + 子问题扩展，显式模式，绝不超预算。差距：语义改写。 |
| 073 | 证据覆盖 | **Delivered** | `RequirementStatus`、`GapAnalyzer`、`coverage_gaps`、`focus_requirements`、分级 qrels。 |
| 074 | 来源独立性 | **Delivered (v1)** | 内容社群计数、原始发布者判定、跨 URL 同文折叠。 |
| 075 | 策略模拟 | **Delivered (v1)** | 带假设策略的离线重放。 |
| 076 | 自适应检索规划 | **Delivered (v1)** | 决策状态机 + 阶段 trace；来源感知的决策；成本自适应的计划缩减。 |
| 077 | 检索缓存智能 | **Delivered (v1)** | 精确/canonical/语义缓存，键含模式与新鲜度，0.92 相似度门槛；跨进程的命中时证据有效性校验。 |
| 078 | 成本归因 | **Delivered** | 完整成本账本、reserve→execute→settle、对账、节省视图。 |
| 079 | 检索基准 | **Delivered** | 500 题数据集、中文语料、CI 评估门禁、MRR / nDCG@5 / Recall@5。 |
| 080 | Provider 回退与路由 | **Delivered (v1 切片 + 结果级学习)** | 多 key 轮换、失败隔离、结果驱动熔断、按来源延迟预算、**成本自适应选源**（`federation.selection = cost_adaptive`：按每单位成本的预期质量排序而非配置 priority，本地优先与需求权威仍优先），以及**结果级质量学习**：每次非缓存搜索后，按来源学习"该来源被判定过的结果中相关比例"。判定来自 RFC-033 反馈（及可选评估数据集）；只统计被判定过的结果，未被判定的来源绝不被惩罚，分数是 EMA，跨重启持久化（migration v18）。差距：学习按来源全局（不按工作区/查询类别），legacy（非联邦）路径只在 `SearchResultItem.source` 命中已注册来源时才学习。 |
| 081 | 扩展 SDK | **Partial** | 版本化扩展注册表、manifest 发现、隔离。差距：外部进程扩展（冻结的「Both」决策）。 |
| 082 | 爬虫扩展系统 | **Partial** | 带隔离的按站点爬虫扩展。 |
| 083 | 插件隔离 | **Partial** | 原生扩展的故障隔离。差距：沙箱化的外部插件。 |
| 084 | 分布式检索运行时 | **Reject** | 冻结的 *Staged* 决策——先单节点，只预留接缝。 |
| 085 | 多节点协同 | **Reject** | 同上冻结决策。 |
| 086 | 检索智能评估 | **Delivered (v1)** | 批量重放评估 + CI 门禁（关键词覆盖、排序指标、p95 延迟）。 |
| 087 | 隐私感知的导入与外发 | **Delivered (v1)** | 工作区隔离、token→工作区绑定、脱敏配置导出、失败即拒绝的按类外发覆盖、导入期脱敏与审计报告、只追加的删除传播审计（`GET /deletions`）。 |
| — | LLM 生成答案 | **Reject** | 明确的非目标：SearchOS 提供证据，而不是生成答案。 |

---

## Gaps that remain, by size

Ordered by how much they would close a real user-visible hole:


Everything marked **Reject** is a frozen decision, not a backlog item.
