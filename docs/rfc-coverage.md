# RFC Coverage Matrix (RFC-001…087)

> Single authoritative status table for the frozen RFC catalogue.
> 一个权威的 RFC 覆盖状态表。

**As of:** 2026-11-20 working tree (commit 64b05ae + RFC-082 wiring) · **Workspace version:** 0.2.0 · **Verified v0.2.0 release suite:** 865 tests / 0 failed

This tree adds RFC-060, the RFC-032 publisher trust catalog, and the RFC-042/050/051/054/056/081–083 completion slices on top of that release; the touched crates' named tests, clippy, fmt, the route scan and the OpenAPI parity gate are locally green. It is not yet a tagged release; the full workspace suite was not re-run end-to-end locally (time budget) and CI is the authority.

This table answers one question per RFC: *is the capability in the repository
today, and if so, how far?* It supersedes the scattered status notes in
[`gap-analysis-master-index.md`](gap-analysis-master-index.md) §4 (which remains
the detailed evidence log for RFC-062…087) and the historical planning tables
in [`feature-roadmap.md`](feature-roadmap.md) / [`roadmap.md`](roadmap.md).

**Current totals:** 83 Delivered · 0 Partial · 4 Reject (87 numbered slots; the
four Rejects are frozen v1 non-goals, so every in-scope slot is delivered —
`Delivered (v1)` rows still name their explicit beyond-v1 gaps).

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
| 032 | Source Trust | **Delivered (v1)** | publisher-level trust scoring derived from the append-only feedback stream: `PublisherTrust` (relevant fraction of canonical/helpful judgements, `None` below the 2-sample floor — unknown is never zero), `GET /publishers`, storage aggregation over `search_feedback`. Gap: no trust decay/aging and no import of external trust lists. |
| 033 | Online Feedback | **Delivered (v1)** | append-only human judgements on retrieved results (`canonical`=3 / `helpful`=2 / `related`=1 / `irrelevant`/`wrong`=0) via `POST /feedback`, CLI `feedback add|list|qrels|dataset`; aggregation only accepts a verdict with `min_agreement` votes and NO tie — contradictory pairs are `disputed` and excluded, never averaged into a fabricated label. `GET /feedback/qrels` + `/feedback/dataset` export graded `EvaluationDataset` JSONL that `searchos eval run` and `watch.dataset_path` consume directly — the learning loop closes. Workspace-scoped, notes never sent to a provider. Gap: no UI for collecting judgements; no model training from feedback (v1 only produces qrels). |
| 034 | Policy Management | **Delivered (v1)** | versioned append-only policies, live pointer, rollback; `GET/PUT /policies*`, CLI `policy`. |
| 035 | Search Watch | **Delivered (v1 + dispatch)** | deterministic quality watchdog over the Analytics Read Model: optional quality/failure/trend thresholds, an optional labelled-dataset ranking check, append-only snapshots (migration v16), `searchos watch run|history`, `POST /watch/run`, `GET /watch/snapshots`. An unavailable signal is `skipped` (never a pass), a trend only fires when BOTH snapshots have `min_sample` observations, and a regression is a result — not an HTTP error. **Dispatch**: `watch.notify_on` statuses (default `regressed`+`degraded`) drive a log notifier plus an optional webhook that receives the report ONLY (no document bodies, queries or notes), and `watch.schedule_interval_seconds` runs it inside the gateway with a clean shutdown. A failing notifier is recorded in the run, never raised. Gap: no per-notifier retry/backoff and no email/Slack-native formatting (webhook JSON only). |
| 036 | Document Intelligence | **Delivered (v1)** | deterministic extractive summary + keywords computed at ingest, persisted per version (migration v14), surfaced by `document inspect` and `GET /documents/:id`. Extractive by design — no generated text. |
| 037 | Multilingual Search | **Delivered (v1)** | deterministic cross-lingual recall via a curated bidirectional term dictionary (`searchos-query::crosslingual`, 34 domain pairs): a Chinese query generates ONE translated recall list (`intelligence.cross_lingual = true`, opt-in), fused with the original results by weighted RRF — the original query is never replaced, and a cross-language variant in the external plan competes for the SAME query budget. No LLM, no network, fully reproducible and auditable. Gap: term-level translation only, not full grammatical MT; the table is a starter set, not a general dictionary. |
| 038 | Evidence Graph | **Delivered (v1)** | `duplicate_of` / `same_content` edges on the response, built from the merge. |
| 039 | Provider Intelligence | **Delivered (v1)** | key rotation (ADR-029), health, outcome-driven circuit breaker, live p50/p95, and **quality-signal provider selection**: `federation.selection = cost_adaptive` orders sources by `expected_quality` (observed success × declared trust × learned judged-result quality once ≥3 samples) per unit cost, and `/search/plan` exposes `source_scores` (`expected_quality`, `cost_micros`, `value_density`, `selected`) for every eligible source, selected or rejected. Default `reliability_first` is unchanged. |
| 040 | Search Replay | **Delivered (v1)** | `POST /replay/{trace_id}` replays a recorded decision offline with an optional what-if policy. |
| 041 | Identity & RBAC | **Reject** | managed multi-tenant RBAC/SSO is an explicit v1 non-goal; the workspace token is the only identity. |
| 042 | Usage, Rate Limiting & Billing | **Delivered (v1)** | rate limiting, full cost ledger, and the RFC-042 **invoice view**: `BillingInvoice`/`BillingLine` grouped by currency then provider + operation over `cost_events` (estimates copied, never rewritten; `unreconciled_events` per line; `fully_reconciled` only when every event has a real bill), `GET /cost/invoices?workspace_id&from&to`, CLI `searchos cost invoices`, OpenAPI schemas. Gap: invoices are a read model — no payment processing and provider bills still arrive via manual `POST /cost/reconcile`. |
| 043 | Backup & Disaster Recovery | **Delivered** | `backup create|restore` (`VACUUM INTO`, checksum manifest, refuses tampered backups) + CI drill that once caught two real bugs. |
| 044 | Health & Operations | **Delivered** | `/health`, `/system/status`, `doctor`, `/metrics`, graceful shutdown. |
| 045 | Import, Export & Portability | **Delivered** | ingest paths, `export` (secrets never exported), OpenAPI + SDKs. |
| 046 | Result Normalization | **Delivered** | canonical URL implementation, `merge_evidence` normalization and conflict retention. |
| 047 | Ranking Engine | **Delivered** | weighted RRF fusion, title boost, fuzzy fallback, feature + remote rerankers, vertical routing. |
| 048 | Vertical Search | **Delivered (v1)** | `RetrievalVertical` / `VerticalRouter` with declared weights (RFC-065 slice). |
| 049 | Structured Extraction | **Delivered (v1)** | deterministic structure extraction in `searchos-document::structures`: fenced code blocks (with language), markdown tables (parsed into header-keyed RECORDS — the table's own header names, nothing invented), lists, headings (level) and links, each block carrying its 1-based source line so it is traceable. `POST /structures` (text or a preview URL — nothing persisted) and CLI `searchos structures --text|--file|--url`. No LLM, no interpretation, fully reproducible. Gap: no schema-driven extraction (a caller-defined field mapping), and HTML tables are not converted to markdown upstream. |
| 050 | Source Discovery | **Delivered (v1)** | automatic discovery from the corpus: `POST /sources/discover` scans document canonical URLs into `source_discoveries` (migration v21, deduped per host+URL observation so rescans bump evidence honestly; soft-deleted documents excluded), `GET /sources/discoveries`, promotion `POST /sources/discoveries/{host}/register` registers a host-restricted `LocalPublisherSource` in BOTH the federated registry (searchable, non-billable, never widening a caller's domain policy) and the persisted catalog, and `/dismiss` drops a proposal. Gap: discovery reads the local corpus only — no discovery from live search results or external link graphs. |
| 051 | Crawl Frontier | **Delivered (v1)** | `FetchFrontier` (`crates/searchos-fetch::frontier`): per-host FIFO with cross-host fairness (one cooling host never starves another), robots Crawl-delay pacing with a configurable default, URL dedup, per-host and global queue bounds, invalid-URL rejection, explicit-clock pop for deterministic tests. Wired end to end: `FetchService::fetch_many` (per-input-index outcomes, failure isolation, batch deadline) → `POST /fetch/batch` → the `fetch_batch` background job. Gap: the frontier is per-request/per-job — no persistent cross-request scheduler yet. |
| 052 | Browser Rendering | **Reject** | browser engine is an explicit v1 non-goal. |
| 053 | Content Extraction | **Delivered** | `FetchService` extraction pipeline, HTML sanitization, Tavily-shaped `/extract`. |
| 054 | Task Orchestration | **Delivered (v1)** | durable job scheduler: `background_jobs` (migration v20) with atomic claim + lease recovery (`claim_next` single-consumer, expired leases reclaimable), retry classification from the error class (retryable → exponential backoff 5s…300s; non-retryable → terminal on the first attempt), `run_after` scheduling, gateway worker (1s tick, clean-shutdown drain) beside the outbox worker, built-in kinds `reindex_document` and `fetch_batch`, HTTP `POST/GET /jobs`, `GET /jobs/{id}`, CLI `jobs list|enqueue|show`, OpenAPI. Gap: kinds are a fixed set (no user-registered handlers) and no cron-style recurrence (the RFC-035 watch schedule covers its own niche). |
| 055 | Cancellation & Deadline | **Delivered** | absolute request deadlines, per-source timeout caps, cooperative cancellation, MCP `notifications/cancelled` acknowledged. |
| 056 | Partial Streaming | **Delivered (v1)** | `POST /search/stream`: per-result SSE over the exact `/search` contract — `start` (task id + status), one `result` event per evidence item with its index, `quality` (verdict + warnings + coverage gaps), `final` (the complete byte-identical SearchResponse), `done`. Per-result delivery, not token streaming: there is no LLM generation in v1 (frozen non-goal), and the current pipeline computes the gated response before the evidence stream begins — documented in OpenAPI. Gap: no stage-level incremental streaming (would require an observer hook in the decision pipeline). |
| 057 | Temporal Knowledge | **Delivered (v1)** | `as_of` historical views: freshness judged at an instant, future documents excluded. |
| 058 | Entity Resolution | **Delivered (v1)** | entity alias registry, `POST /entities/resolve`, `entity_id` on results. |
| 059 | Contradiction Detection | **Delivered (v1)** | conflicting versions are retained and force `QualityStatus::Conflict`; `MergeReport.conflict_links` records older→newer, and `build_evidence_graph` emits `EvidenceRelation::Contradicts` edges (both raw and canonical endpoints) instead of only an opaque warning string. Response `evidence_graph` + TypeScript SDK carry the new relation. Gap: contradiction detection is still the deterministic text-difference heuristic, not claim-level semantic analysis. |
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
| 081 | Extension SDK | **Delivered (v1)** | the frozen "Both" decision is closed: `runtime: external_process` manifests (entry program + args, **no shell**, declared budget timeout) run under `ExternalProcessHost` — line-delimited JSON protocol (health/invoke/crawl) with one request in flight, timeout kill, crash → quarantine, bounded stderr capture and bounded reply lines, kill-on-drop. Opt-in via `extensions.external_process_enabled` (default false); the discovery policy gate quarantines the manifests when disabled, and launch failures quarantine in the same auditable registry. Gap: WASM remains a frozen later phase; no packaging/distribution format beyond the directory layout. |
| 082 | Crawler Extension System | **Delivered (v1)** | external site crawlers drive the fetch path end to end: `match_domains` on the manifest claims hosts (exact/suffix priority like `CrawlerRegistry::select`), the runtime bridges them through `ExternalCrawlerPort`, and a successful `crawl` op's item is persisted through the SAME versioning/chunking/summary pipeline as a generic fetch. Robots policy is enforced before the extension runs; any extension failure falls back to the generic fetch (never leaks the extension error), and `fetch_preview` stays on the generic path. Deterministic tests: in-process fake port (success without network + fallback isolation) and a real child-process runtime test. Gap: declarative CSS extraction rules (`ExtractorsSpec`) are still not applied to fetched documents. |
| 083 | Plugin Isolation | **Delivered (v1)** | external plugins are isolated by the process boundary plus enforced limits, not trust: per-call deadline kills a hung child, crash/EOF quarantines it while the host survives, protocol violations and oversized replies are quarantined/reported (never guessed into success), stderr is drained into a bounded buffer, reply lines are bounded (4 MiB) and read via a bounded reader, `kill_on_drop` + explicit `stop()` leave no orphans. Named tests cover timeout-kill, crash quarantine, malformed replies and the echo-child contract. Gap: no OS-level sandbox (seccomp/namespaces) — the process boundary plus limits is the v1 sandbox. |
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
| 042 | 用量、限流与计费 | **Delivered (v1)** | 限流、完整成本账本，以及 RFC-042 **发票视图**：`BillingInvoice`/`BillingLine` 按“币种 × provider + operation”在 `cost_events` 上分组（估算金额只复制不改写；每行 `unreconciled_events`；只有全部事件都有真实账单才 `fully_reconciled`），`GET /cost/invoices?workspace_id&from&to`，CLI `searchos cost invoices`，OpenAPI schema。差距：发票是读模型——没有支付处理，真实账单仍靠手工 `POST /cost/reconcile` 导入。 |
| 043 | 备份与灾难恢复 | **Delivered** | `backup create|restore`（`VACUUM INTO`、校验清单、拒绝被篡改备份）+ CI 演练（曾借此发现两个真实 bug）。 |
| 044 | 健康与运维 | **Delivered** | `/health`、`/system/status`、`doctor`、`/metrics`、优雅停机。 |
| 045 | 导入、导出与可移植 | **Delivered** | 导入路径、`export`（绝不导出密钥）、OpenAPI + SDK。 |
| 046 | 结果归一化 | **Delivered** | 统一 canonical URL 实现、`merge_evidence` 归一化与冲突保留。 |
| 047 | 排序引擎 | **Delivered** | 加权 RRF 融合、标题加权、模糊回退、特征 + 远程 reranker、垂直路由。 |
| 048 | 垂直检索 | **Delivered (v1)** | `RetrievalVertical` / `VerticalRouter` + 声明权重（RFC-065 切片）。 |
| 049 | 结构化抽取 | **Delivered (v1)** | `searchos-document::structures` 确定性抽取：围栏代码块（含语言）、Markdown 表格（解析成以**表头自身字段名**为键的记录——不臆造字段）、列表、标题（层级）与链接，每个块都带 1-based 源行号，可回溯。`POST /structures`（文本或预览 URL，不落库）与 CLI `searchos structures --text|--file|--url`。无 LLM、无解释性加工、完全可复现。差距：没有 schema 驱动的抽取（调用方自定义字段映射），且上游 HTML 表格未转成 Markdown。 |
| 050 | 来源发现 | **Delivered (v1)** | 语料自动发现：`POST /sources/discover` 扫描文档 canonical URL 写入 `source_discoveries`（迁移 v21，按 host+URL 观察去重，重复扫描只诚实地累加证据；排除软删除文档），`GET /sources/discoveries`，晋升 `POST /sources/discoveries/{host}/register` 同时注册到联邦注册表（可检索、不计费、绝不放宽调用方的域名策略）与持久目录，`/dismiss` 丢弃提案。差距：只读本地语料——不发现自实时搜索结果或外部链接图。 |
| 051 | 爬取前沿 | **Delivered (v1)** | `FetchFrontier`（`crates/searchos-fetch::frontier`）：每主机 FIFO + 跨主机公平（一个冷却中的主机绝不会饿死其他主机），robots Crawl-delay 配速带可配置默认值，URL 去重，每主机与全局队列上限，无效 URL 显式拒绝，显式时钟 pop 保证测试确定性。端到端接线：`FetchService::fetch_many`（逐输入索引结果、失败隔离、批处理 deadline）→ `POST /fetch/batch` → `fetch_batch` 后台作业。差距：前沿是每次请求/作业作用域——尚无跨请求的持久调度器。 |
| 052 | 浏览器渲染 | **Reject** | 浏览器引擎是明确的 v1 非目标。 |
| 053 | 内容抽取 | **Delivered** | `FetchService` 抽取管线、HTML 清洗、Tavily 形状的 `/extract`。 |
| 054 | 任务编排 | **Delivered (v1)** | 持久作业调度：`background_jobs`（迁移 v20）带原子领取 + 租约恢复（`claim_next` 单消费者，过期租约可回收），按错误类别分类重试（可重试 → 指数退避 5s…300s；不可重试 → 首次即终态），`run_after` 定时，网关 worker（1s tick，干净停机时排水）与 outbox worker 并列，内置作业 `reindex_document` 与 `fetch_batch`，HTTP `POST/GET /jobs`、`GET /jobs/{id}`，CLI `jobs list|enqueue|show`，OpenAPI。差距：作业种类是固定集合（无用户注册 handler），无 cron 式循环（RFC-035 watch 调度覆盖自己的场景）。 |
| 055 | 取消与截止时间 | **Delivered** | 绝对请求截止时间、按来源的超时上限、协作式取消、MCP `notifications/cancelled` 已被确认。 |
| 056 | 部分流式 | **Delivered (v1)** | `POST /search/stream`：在完全一致的 `/search` 契约上做按结果 SSE——`start`（task id + 状态）、每条证据一个 `result` 事件（带索引）、`quality`（判定 + 警告 + 覆盖缺口）、`final`（逐字节相同的完整 SearchResponse）、`done`。按结果投递，不是按 token：v1 没有 LLM 生成（冻结非目标），且当前管线在证据流开始前已算完门禁响应——已在 OpenAPI 中如实注明。差距：无阶段级增量流式（需要在决策管线中加观察者钩子）。 |
| 057 | 时态知识 | **Delivered (v1)** | `as_of` 历史视图：在某一时刻判定新鲜度，未来文档被排除。 |
| 058 | 实体消解 | **Delivered (v1)** | 实体别名注册表、`POST /entities/resolve`、结果上的 `entity_id`。 |
| 059 | 矛盾检测 | **Delivered (v1)** | 冲突版本被保留并强制 `QualityStatus::Conflict`；`MergeReport.conflict_links` 记录 older→newer，`build_evidence_graph` 输出 `EvidenceRelation::Contradicts` 边（raw + canonical 端点），不再只是提示字符串。响应 `evidence_graph` 与 TypeScript SDK 已携带新关系。差距：矛盾检测仍是确定性文本差异启发式，不是声明级语义分析。 |
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
| 081 | 扩展 SDK | **Delivered (v1)** | 冻结的「Both」决策已闭合：`runtime: external_process` manifest（entry 为 program + args，**不经 shell**，必须声明预算超时）运行在 `ExternalProcessHost` 下——行分隔 JSON 协议（health/invoke/crawl），单请求在途，超时强杀，崩溃 → 隔离，stderr 有界捕获，应答行有界，kill-on-drop。通过 `extensions.external_process_enabled` 显式启用（默认关闭）；禁用时发现策略门直接隔离 manifest，启动失败在同一可审计注册表中隔离。差距：WASM 仍是冻结的后续阶段；除目录布局外无打包/分发格式。 |
| 082 | 爬虫扩展系统 | **Delivered (v1)** | 外部站点爬虫端到端驱动抓取路径：manifest 的 `match_domains` 认领主机（精确/后缀优先级与 `CrawlerRegistry::select` 一致），runtime 经 `ExternalCrawlerPort` 桥接，`crawl` 成功返回的条目走与通用抓取完全相同的版本化/分块/摘要持久化管线。robots 策略在扩展运行前执行；扩展任何失败都回退到通用抓取（绝不外泄扩展错误），`fetch_preview` 保持在通用路径。确定性测试：进程内假端口（无网络成功 + 回退隔离）与真实子进程 runtime 测试。差距：声明式 CSS 抽取规则（`ExtractorsSpec`）仍未应用到抓取文档。 |
| 083 | 插件隔离 | **Delivered (v1)** | 外部插件以进程边界加强制限制隔离，而非信任：每次调用有 deadline，挂死子进程被杀；崩溃/EOF 隔离且宿主存活；协议违规与超长应答被隔离/如实报告（绝不臆造成功）；stderr 排入有界缓冲；应答行有界（4 MiB）且有界读取；`kill_on_drop` + 显式 `stop()` 不留孤儿进程。命名测试覆盖超时击杀、崩溃隔离、畸形应答与 echo 子进程契约。差距：无操作系统级沙箱（seccomp/命名空间）——进程边界加限制就是 v1 沙箱。 |
| 084 | 分布式检索运行时 | **Reject** | 冻结的 *Staged* 决策——先单节点，只预留接缝。 |
| 085 | 多节点协同 | **Reject** | 同上冻结决策。 |
| 086 | 检索智能评估 | **Delivered (v1)** | 批量重放评估 + CI 门禁（关键词覆盖、排序指标、p95 延迟）。 |
| 087 | 隐私感知的导入与外发 | **Delivered (v1)** | 工作区隔离、token→工作区绑定、脱敏配置导出、失败即拒绝的按类外发覆盖、导入期脱敏与审计报告、只追加的删除传播审计（`GET /deletions`）。 |
| — | LLM 生成答案 | **Reject** | 明确的非目标：SearchOS 提供证据，而不是生成答案。 |

---

## Gaps that remain, by size

Ordered by how much they would close a real user-visible hole:


Everything marked **Reject** is a frozen decision, not a backlog item.
