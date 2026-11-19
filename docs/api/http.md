# HTTP API Reference

> RFC-012 12.4/12.5/12.6 — Axum gateway surface. Handlers are thin
> transport: auth, DTO validation, application call, response mapping.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| POST | `/search` | Execute a search (quality-gated, cost-aware) |
| POST | `/extract` | Fetch + extract + persist a URL |
| POST | `/ingest` | Import pasted / typed text locally (RFC-029) |
| POST | `/ingest/file` | Multipart file upload (RFC-029) |
| POST | `/ingest/batch` | Batch text import with per-item outcomes (RFC-029) |
| GET | `/ingest/jobs/:id` | Batch job + item status (RFC-029) |
| POST | `/documents` | Fetch and persist a document |
| GET | `/documents` | Live document list: workspace-scoped, optional `source_type` / `status` filter; soft-deleted hidden unless asked for |
| GET | `/documents/:id` | Document + lifecycle summary + current version + version history |
| GET | `/documents/:id/versions` | Immutable content version history |
| POST | `/documents/:id/reindex` | Rewrite only the full-text index entries |
| DELETE | `/documents/:id` | Soft delete + chunk removal + immediate index removal |
| POST | `/memory/search` | Namespaced memory search (RFC-013 isolation) |
| GET | `/cost/summary` | Ledger summary (estimates, not real bills) |
| GET | `/cost/providers` | Per-provider health + spend |
| GET | `/stats/search` | Metrics snapshot (RFC-015 15.2) |
| GET | `/stats/retrieval` | Retrieval/reuse rates + exact-cache counters |
| GET | `/stats/savings` | Savings / CPQPA / reuse views (alias: `/cost/savings`) |
| GET | `/cost/reservations` | Budget reserve→settle audit trail |
| POST | `/cost/reconcile` | Import provider billing (`actual_cost`, `source`) |
| GET | `/api/v1/dashboard/*` | RFC-028 Dashboard API over the Analytics Read Model |
| GET | `/traces?limit=N` | Recent decision traces for the visual console |
| GET | `/system/status` | Aggregated runtime health (doctor-style) |
| GET | `/decisions/:trace_id` | Explainable decision trace |
| GET | `/trace/:trace_id` | RFC-021 21.2 alias of the above |
| GET | `/profiles`, `/profiles/:id` | Versioned SearchProfiles |
| GET | `/evidence/acquisitions`, `/evidence/acquisitions/:id` | RFC-031 external acquisitions + per-item promotion state |
| GET | `/evidence/reuse` | RFC-031 compound report (reuse events per acquisition) |
| POST | `/documents/search` | Persisted document lookup |
| POST | `/search/plan` | Plan-only: what would be searched, without buying anything (RFC-004/031) |
| POST | `/search/follow-up` | Continue an agent session; earlier evidence is reused when still valid (RFC-071 v2) |
| POST | `/research/run` | Bounded deep search; each round is a full search recorded as a turn (RFC-071) |
| GET | `/research/:session_id` | Research workspace: turns + accumulated evidence (RFC-071) |
| GET | `/entities` | Registered entities and aliases (RFC-058) |
| POST | `/entities` | Register an entity + aliases (RFC-058) |
| POST | `/entities/resolve` | Map free text to a stable entity id (RFC-058) |
| GET | `/collections` | Knowledge collections with live member counts (RFC-030) |
| POST | `/collections` | Create (or fetch) a named collection (RFC-030) |
| GET | `/collections/:id/documents` | Live document ids in a collection (RFC-030) |
| POST | `/collections/:id/documents` | Add a document to a collection (RFC-030) |
| DELETE | `/collections/:id/documents/:document_id` | Remove a document from a collection (RFC-030) |
| GET | `/agents` | Declared agent identities, newest update first (RFC-060) |
| POST | `/agents` | Create or update a declared agent (RFC-060) |
| GET | `/agents/:id` | One declared agent (RFC-060) |
| POST | `/agents/:id/enabled` | Enable or disable an agent; history is kept (RFC-060) |
| POST | `/replay/:trace_id` | Offline replay of a recorded decision; optional what-if policy (RFC-040) |
| POST | `/evaluation/replay` | Batch evaluation over recent traces (RFC-086) |
| GET | `/policies` | Versioned policies + the live one (RFC-034) |
| GET | `/policies/:kind/versions` | Policy version history (RFC-034) |
| PUT | `/policies/:kind` | Submit a new policy version (RFC-034) |
| POST | `/policies/:kind/rollback` | Move the live pointer back to an earlier version (RFC-034) |
| GET | `/deletions` | Deletion audit trail (RFC-087) |
| POST | `/memory/lifecycle` | Report (or apply) memory lifecycle actions (RFC-070) |
| GET | `/sources` | Federated source catalog with reliability + latency (RFC-063) |
| GET | `/health` | Status + index consistency |
| GET | `/metrics` | Prometheus text exposition 0.0.4 (same auth as the rest) |

Every route is also mounted under `/v1` (RFC-013 13.7) and responses carry
`x-searchos-api-version`. No catch-all routes are used; the Axum route-conflict
scanner is run on every route change. `server.cors_origin` enables OPTIONS
preflight and CORS headers for direct browser access; the bundled Next.js
console uses a server-side proxy instead (see [`docs/ui.md`](../ui.md)).
`POST /search` accepts an optional `request_id` idempotency key.

## Dashboard API (RFC-028)

All Dashboard responses are `{ data, meta }` with `generated_at`, `time_range`,
`aggregation_version`, `data_freshness`, `query_mode`, `applied_filters`,
`ignored_filters`, `refresh_skipped`, `model_age_seconds` and `workspace_scope`.
Handlers never write SQL; they call `AnalyticsQueryService`, which either reuses
the precomputed `analytics_read_models` payload (no predicate and
`interval=day`) or runs a live aggregation with every predicate in the SQL
WHERE clause. A stored payload is rebuilt only when the operational source
watermark has moved. `meta.applied_filters` / `meta.ignored_filters` declare
exactly which filters each dataset consumed and which its schema cannot
apply, so a filter is never silently dropped.

| Method | Path | Read Model |
|---|---|---|
| GET | `/api/v1/dashboard/overview` | overview KPIs, denominators, definitions |
| GET | `/api/v1/dashboard/search/{summary,timeseries,strategies,requests}` | search analytics |
| GET | `/api/v1/dashboard/cost/{summary,breakdown,savings}` | cost analytics |
| GET | `/api/v1/dashboard/knowledge/{summary,documents,freshness}` | knowledge analytics |
| GET | `/api/v1/dashboard/quality/{summary,dimensions,failures}` | quality dimensions |
| GET | `/api/v1/dashboard/providers` | provider usage and cost |
| GET | `/api/v1/dashboard/crawlers/summary` | crawler summary |
| GET | `/api/v1/dashboard/infrastructure/summary` | measured resources; null otherwise |
| GET | `/api/v1/dashboard/events` | SSE `analytics.events` refetch hint |
| GET | `/api/v1/dashboard/export?format=json|csv` | Overview export |

Common query contract: `workspace_id`, `project_id`, `agent_id`, `provider_id`,
`search_mode`, `quality_status`, `from`, `to`, `interval`, `timezone`, `limit`,
`cursor`. Inverted time ranges and unsupported intervals are rejected. See
[`docs/ui.md`](../ui.md) and
[`RFC-028`](../rfc/rfc-028-dashboard-api-frontend.md).

Auth: `Authorization: Bearer <token>` when `server.auth_token` is configured
(RFC-012 12.3; empty token disables auth for local dev). When
`server.workspace_tokens` is set, the token is bound to exactly one workspace
and cross-workspace requests return 403 (RFC-017 17.6). A fixed-window
`server.rate_limit_per_minute` returns 429 when exceeded (RFC-013 13.3).

## Prometheus metrics (`GET /metrics`)

`GET /metrics` returns **Prometheus text exposition format 0.0.4**
(`text/plain; version=0.0.4; charset=utf-8`) and uses the same bearer-token
authentication as every other endpoint, so a scraper supplies the token rather
than the endpoint being unauthenticated.

One scrape carries both halves of the picture:

| Group | Examples |
|---|---|
| Execution correctness | `searchos_search_requests_total`, `searchos_quality_pass_total`, `searchos_quality_partial_total`, `searchos_freshness_violations_total`, `searchos_conflicts_total`, `searchos_provider_failures_total` |
| Efficiency | `searchos_cache_hits_total`, `searchos_semantic_reuse_total`, `searchos_singleflight_merges_total`, `searchos_external_avoided_total` |
| Cost | `searchos_cost_micros_total`, `searchos_cost_micros_per_quality_pass` |
| Index | `searchos_index_outbox_processed_total`, `searchos_index_outbox_dead_total`, `searchos_index_outbox_pending` |
| Latency | `searchos_search_latency_p50_seconds`, `_p95_`, `_p99_` |
| Part 30 invariants | `searchos_invariant_*` (every invariant counter/gauge) |
| Process health | `searchos_uptime_seconds`, `searchos_sse_active`, `searchos_sse_connections_total`, `searchos_schema_version`, `searchos_build_info` |

The only labelled series is `searchos_search_status_total{status="…"}`, and its
label set is the bounded status vocabulary, never user input.

These are **in-process** counters: they are the same numbers the console shows,
not a substitute for an external monitor. They reset on restart.

## OpenTelemetry (OTLP/HTTP, opt-in)

`GET /metrics` is always available. In addition, traces and the same metric
snapshot can be pushed over OTLP/HTTP protobuf by setting
`SEARCHOS_OTLP_ENABLED=true` and `SEARCHOS_OTLP_ENDPOINT=http://host:4318`.
It is off by default; no exporter, background thread or network call exists
when disabled. Signals are flushed on graceful shutdown. Collector tokens go
in `SEARCHOS_OTLP_HEADERS` (`k=v,k2=v2`) and never appear in logs or
validation errors. See [`docs/observability.md`](../observability.md).

## Graceful shutdown

`searchos gateway start` handles `SIGTERM` and `SIGINT`:

1. The listener stops accepting new connections.
2. In-flight requests drain; the SSE dashboard streams terminate.
3. The outbox worker performs one final flush, so documents fetched just
   before shutdown still reach the full-text index.
4. The process exits `0`.

A **second** signal forces an immediate exit, so a wedged process can always be
recovered. Because every request is handled inline, MCP cancellation is not a
shutdown concern (see [`docs/mcp.md`](../mcp.md)); SSE is the only long-lived
connection and it is wired to the shutdown signal.

## POST /search

Request (native SearchOS contract):

```json
{
  "query": "What is the latest Tokio version?",
  "profile": "latest",
  "mode": "local_first",
  "workspace_id": "default",
  "max_external_queries": 3,
  "max_total_cost": 0.05,
  "max_results": 5
}
```

`mode` accepts the canonical snake_case names and the documented
kebab-case aliases (`local_only` / `local-only`, etc.). Anything else is a
400 `VALIDATION_FAILED`; it is never silently downgraded to an
external-capable mode. `max_results` caps the response **after** the
quality gate (1–100).

Response (RFC-012 12.6):

```json
{
  "query": "What is the latest Tokio version?",
  "decision": { "path": "external", "reason": "strict_freshness_required" },
  "results": [],
  "quality": { "status": "passed", "freshness": "verified", "coverage": "sufficient", "score": 0.0, "failed_constraints": [] },
  "cost": { "estimated": 0.0032, "actual": null, "currency": "USD" },
  "provenance": [],
  "trace_id": "trace_xxx",
  "status": "completed",
  "warnings": [],
  "task_id": "task_xxx",
  "replayed": false,
  "coverage_gaps": []
}
```

`actual: null` means no real bill data yet — never "cost is zero".
`status` is explicit: `completed | completed_with_warnings | partial |
insufficient_evidence | budget_exhausted | failed | cancelled` (RFC-002 2.8).
`replayed: true` means the response came from the `request_id` idempotency
ledger; no new external search or bill was created. `coverage_gaps` lists the
open sub-questions from the final quality gate, with a stable
`requirement_id`, priority and minimal acquisition strategy for a targeted
follow-up by a search agent. `trace show` /
`/trace/{id}` returns `stages[]` with structured stage records,
`final_strategy` and `final_outcome` (RFC-004 v1.1 11.1).

## Search-agent surfaces

`POST /search/plan` — plan-only: analyzes the query, retrieves local knowledge
and returns the queries that WOULD be executed, the open requirements and the
estimated cost. **No external call, no provider bill, no document write.** An
agent can inspect the plan and its cost before committing budget.

`POST /search/follow-up` — continues a recorded session. Without
`focus_requirements`, the turn targets exactly the requirements the previous
turn left open (its `coverage_gaps`). The turn is recorded back into the
session, so multi-turn research keeps its coverage state.

`POST /search` accepts `max_acquisition_rounds` (bounded iterative retrieval;
total query and cost caps stay cumulative), `session_id` (turn recording) and
`focus_requirements` (explicit follow-up requirements, replacing the derived
decomposition). Responses carry `coverage_gaps`, `session_id` and
`focus_requirements`; local results also carry a query-matched `content`
passage and `<b>…</b>` `highlight`.

## Local data ingestion (RFC-029)

Local content is imported into the **same** knowledge store as URL-fetched
documents: same versioning, same transactional outbox, same `SearchIndex`
port, same workspace isolation. It is immediately visible to `local_only` and
`local_first` search.

`POST /ingest` (JSON):

```json
{
  "content": "The quokka is a small macropod found on Rottnest Island.",
  "source_key": "notes/quokka",
  "title": "Quokka notes",
  "content_type": "text/plain",
  "workspace_id": "default"
}
```

`source_key` is the logical identity. When omitted, the source is
content-addressed (`sha256(content)[:16]`), so identical content dedupes.
Re-importing the same `source_key` with identical content returns
`changed: false` and creates no new version; changed content creates a new
version and retains the old one. Canonical URI: `searchos-local://<workspace>/<source_key>`.

`POST /ingest/file` is `multipart/form-data` with a `file` field plus
optional `source_key`, `title`, `content_type`, `language`, `workspace_id`.
The client filename is a label only — it never touches the server filesystem.

`POST /ingest` and `/ingest/file` return **201 Created**; `/ingest/batch`
returns 201 with a `job_id`, per-item outcomes, and a terminal `state` of
`completed`, `partial`, or `failed`. `/ingest/batch` also accepts an in-memory
`directory.files[]` array for virtual directory imports (a server-side path is
never accepted from HTTP).

Error codes: `EMPTY_CONTENT`, `CONTENT_TOO_LARGE`,
`INVALID_ENCODING`, `UNSUPPORTED_CONTENT_TYPE`, `WORKSPACE_DENIED`.
Default bounds: 20 MB per document, 200 MB per batch, configurable via
`ingest.max_bytes_per_document` / `ingest.max_bytes_per_batch`.

## Tavily compatibility

The frozen Tavily subset is documented in
[`docs/tavily-compatibility.md`](../tavily-compatibility.md) and enforced by the
fixture suite (`fixtures/tavily/` +
`tests/compatibility/tests/tavily_contract.rs`).

The gateway routes a `/search` body to the compatibility layer when it contains
any Tavily key (`search_depth`, `topic`, `include_answer`, `include_raw_content`,
`include_domains`, `exclude_domains`, `include_images`, `chunks_per_source`,
`time_range`, `country`, `include_favicon`, `days`, …) and maps it onto the
SearchOS domain model. Every knob that is not honored — and every unknown field —
is reported explicitly in `unsupported_features`, which is always present
(possibly empty). `answer` is always `null`: SearchOS does not fabricate answers
(RFC-001 1.2). A body without any Tavily key stays on the native envelope and
never grows Tavily-only fields.

Tavily-shaped `/extract` (`{ "urls": [...] }`) extracts one URL per call: the
first is honored, the rest are returned in `failed_results` with an explicit
reason, and `extract_depth` / `format` / `include_images` /
`include_favicon` are reported as unsupported.

Errors use the standard gateway envelope; there is no Tavily-specific error
shape. `POST /crawl` and `POST /map` are **not** part of the v1 subset (they
return `404` and are absent from `docs/openapi.yaml`).

## 中文

# HTTP API 参考

> RFC-012 12.4/12.5/12.6 —— Axum 网关接口面。handler 是薄传输层：鉴权、DTO 校验、
> 应用层调用、响应映射。

## 端点

| 方法 | 路径 | 用途 |
|---|---|---|
| POST | `/search` | 执行搜索（质量门控、成本感知） |
| POST | `/extract` | 抓取 + 抽取 + 持久化一个 URL |
| POST | `/ingest` | 导入粘贴/输入的文本（RFC-029） |
| POST | `/ingest/file` | multipart 文件上传（RFC-029） |
| POST | `/ingest/batch` | 批量文本导入，逐条给出结果（RFC-029） |
| GET | `/ingest/jobs/:id` | 批量任务与条目状态（RFC-029） |
| POST | `/documents` | 抓取并持久化一个文档 |
| GET | `/documents` | 实时文档列表：按 workspace 隔离，可选 `source_type` / `status` 过滤；软删除项默认隐藏 |
| GET | `/documents/:id` | 文档 + 生命周期摘要 + 当前版本 + 版本历史 |
| GET | `/documents/:id/versions` | 不可变的内容版本历史 |
| POST | `/documents/:id/reindex` | 只重写全文索引条目 |
| DELETE | `/documents/:id` | 软删除 + 移除分块 + 立即从索引移除 |
| POST | `/memory/search` | 带命名空间的记忆检索（RFC-013 隔离） |
| GET | `/cost/summary` | 账本摘要（估算值，不是真实账单） |
| GET | `/cost/providers` | 各 provider 健康度与花费 |
| GET | `/stats/search` | 指标快照（RFC-015 15.2） |
| GET | `/stats/retrieval` | 检索/复用率与精确缓存计数 |
| GET | `/stats/savings` | 节省 / CPQPA / 复用视图（别名 `/cost/savings`） |
| GET | `/cost/reservations` | 预算 reserve→settle 审计轨迹 |
| POST | `/cost/reconcile` | 导入 provider 账单（`actual_cost`、`source`） |
| GET | `/api/v1/dashboard/*` | 基于 Analytics Read Model 的 RFC-028 Dashboard API |
| GET | `/traces?limit=N` | 可视化控制台用的近期决策 trace |
| GET | `/system/status` | 聚合的运行时健康度（doctor 风格） |
| GET | `/decisions/:trace_id` | 可解释的决策 trace |
| GET | `/trace/:trace_id` | 上一条接口的 RFC-021 21.2 别名 |
| GET | `/profiles`、`/profiles/:id` | 版本化的 SearchProfile |
| GET | `/evidence/acquisitions`、`/evidence/acquisitions/:id` | RFC-031 外部获取与逐条沉淀状态 |
| GET | `/evidence/reuse` | RFC-031 复利报告（每次获取带来的复用） |
| POST | `/documents/search` | 持久化文档查找 |
| POST | `/search/plan` | 仅规划：将会搜什么，不产生任何购买（RFC-004/031） |
| POST | `/search/follow-up` | 继续 agent 会话；早期证据在仍然有效时被复用（RFC-071 v2） |
| POST | `/research/run` | 有界深度检索；每一轮都是一次完整搜索并记为 turn（RFC-071） |
| GET | `/research/:session_id` | 研究工作区：轮次 + 累积证据（RFC-071） |
| GET | `/entities` | 已注册实体与别名（RFC-058） |
| POST | `/entities` | 注册实体与别名（RFC-058） |
| POST | `/entities/resolve` | 把自由文本映射为稳定实体 id（RFC-058） |
| GET | `/collections` | 知识集合及其存活成员计数（RFC-030） |
| POST | `/collections` | 创建（或复用）具名集合（RFC-030） |
| GET | `/collections/:id/documents` | 集合内存活文档 id（RFC-030） |
| POST | `/collections/:id/documents` | 向集合加入文档（RFC-030） |
| DELETE | `/collections/:id/documents/:document_id` | 从集合移除文档（RFC-030） |
| POST | `/replay/:trace_id` | 离线重放已记录的决策，可带假设策略（RFC-040） |
| POST | `/evaluation/replay` | 对近期 trace 做批量评估（RFC-086） |
| GET | `/policies` | 版本化策略 + 当前生效版本（RFC-034） |
| GET | `/policies/:kind/versions` | 策略版本历史（RFC-034） |
| PUT | `/policies/:kind` | 提交新的策略版本（RFC-034） |
| POST | `/policies/:kind/rollback` | 把生效指针回退到早期版本（RFC-034） |
| GET | `/deletions` | 删除审计轨迹（RFC-087） |
| POST | `/memory/lifecycle` | 报告（或执行）记忆生命周期动作（RFC-070） |
| GET | `/sources` | 联邦来源目录，含可靠性与延迟（RFC-063） |
| GET | `/health` | 状态 + 索引一致性 |
| GET | `/metrics` | Prometheus text exposition 0.0.4（鉴权方式与其他端点相同） |

所有路由也挂在 `/v1` 下（RFC-013 13.7），响应带 `x-searchos-api-version`。
不使用 catch-all 路由；每次改动路由都会跑 Axum 路由冲突扫描。`server.cors_origin`
开启 OPTIONS 预检与 CORS 头，以便浏览器直连；随附的 Next.js 控制台改用服务端代理
（见 [`docs/ui.md`](../ui.md)）。`POST /search` 接受可选的 `request_id` 幂等键。

## Dashboard API（RFC-028）

所有 Dashboard 响应都是 `{ data, meta }`，`meta` 含 `generated_at`、`time_range`、
`aggregation_version`、`data_freshness`、`query_mode`、`applied_filters`、
`ignored_filters`、`refresh_skipped`、`model_age_seconds` 和 `workspace_scope`。
handler 从不写 SQL；它们调用 `AnalyticsQueryService`，后者要么复用预计算的
`analytics_read_models` 载荷（无谓词且 `interval=day`），要么把所有谓词放进 SQL
WHERE 子句做实时聚合。只有当运营源 watermark 变动时才会重建已存载荷。
`meta.applied_filters` / `meta.ignored_filters` 明确声明每个数据集消费了哪些过滤、
哪些因 schema 限制无法应用，因此过滤条件绝不会被静默丢弃。

| 方法 | 路径 | Read Model |
|---|---|---|
| GET | `/api/v1/dashboard/overview` | 总览 KPI、分母、定义 |
| GET | `/api/v1/dashboard/search/{summary,timeseries,strategies,requests}` | 搜索分析 |
| GET | `/api/v1/dashboard/cost/{summary,breakdown,savings}` | 成本分析 |
| GET | `/api/v1/dashboard/knowledge/{summary,documents,freshness}` | 知识分析 |
| GET | `/api/v1/dashboard/quality/{summary,dimensions,failures}` | 质量维度 |
| GET | `/api/v1/dashboard/providers` | provider 使用量与成本 |
| GET | `/api/v1/dashboard/crawlers/summary` | 爬虫汇总 |
| GET | `/api/v1/dashboard/infrastructure/summary` | 实测资源；无数据时为 null |
| GET | `/api/v1/dashboard/events` | SSE `analytics.events` 重取提示 |
| GET | `/api/v1/dashboard/export?format=json|csv` | 总览导出 |

通用查询契约：`workspace_id`、`project_id`、`agent_id`、`provider_id`、
`search_mode`、`quality_status`、`from`、`to`、`interval`、`timezone`、`limit`、
`cursor`。反向时间区间与不支持的 interval 会被拒绝。见 [`docs/ui.md`](../ui.md) 和
[`RFC-028`](../rfc/rfc-028-dashboard-api-frontend.md)。

鉴权：配置了 `server.auth_token` 时使用 `Authorization: Bearer <token>`
（RFC-012 12.3；空 token 表示本地开发关闭鉴权）。设置 `server.workspace_tokens` 后，
token 绑定到唯一 workspace，跨 workspace 请求返回 403（RFC-017 17.6）。
固定窗口的 `server.rate_limit_per_minute` 超限返回 429（RFC-013 13.3）。

## Prometheus 指标（`GET /metrics`）

`GET /metrics` 返回 **Prometheus text exposition format 0.0.4**
（`text/plain; version=0.0.4; charset=utf-8`），并使用与其他端点相同的 bearer token
鉴权，所以抓取方需要提供 token，而不是让该端点处于未鉴权状态。

一次抓取同时覆盖两半画面：

| 分组 | 示例 |
|---|---|
| 执行正确性 | `searchos_search_requests_total`、`searchos_quality_pass_total`、`searchos_quality_partial_total`、`searchos_freshness_violations_total`、`searchos_conflicts_total`、`searchos_provider_failures_total` |
| 效率 | `searchos_cache_hits_total`、`searchos_semantic_reuse_total`、`searchos_singleflight_merges_total`、`searchos_external_avoided_total` |
| 成本 | `searchos_cost_micros_total`、`searchos_cost_micros_per_quality_pass` |
| 索引 | `searchos_index_outbox_processed_total`、`searchos_index_outbox_dead_total`、`searchos_index_outbox_pending` |
| 延迟 | `searchos_search_latency_p50_seconds`、`_p95_`、`_p99_` |
| Part 30 不变量 | `searchos_invariant_*`（每个不变量计数/仪表） |
| 进程健康 | `searchos_uptime_seconds`、`searchos_sse_active`、`searchos_sse_connections_total`、`searchos_schema_version`、`searchos_build_info` |

唯一带 label 的序列是 `searchos_search_status_total{status="…"}`，其 label 集合是
有界的状态词表，绝不来自用户输入。

这些是**进程内**计数器：它们和控制台显示的是同一批数字，不能替代外部监控，重启即清零。

## OpenTelemetry（OTLP/HTTP，可选）

`GET /metrics` 始终可用。此外，通过设置 `SEARCHOS_OTLP_ENABLED=true` 和
`SEARCHOS_OTLP_ENDPOINT=http://host:4318`，可以用 OTLP/HTTP protobuf 推送 trace 与
同一份指标快照。它默认关闭；关闭时不存在任何 exporter、后台线程或网络调用。
信号在优雅停机时 flush。采集器 token 放在 `SEARCHOS_OTLP_HEADERS`（`k=v,k2=v2`），
绝不会出现在日志或校验错误中。见 [`docs/observability.md`](../observability.md)。

## 优雅停机

`searchos gateway start` 会处理 `SIGTERM` 和 `SIGINT`：

1. 监听器停止接受新连接。
2. 在途请求排空；SSE dashboard 流被终止。
3. outbox worker 做最后一次 flush，因此刚在停机前抓取的文档仍会进入全文索引。
4. 进程以 `0` 退出。

**第二次**信号会强制立即退出，因此卡死的进程总能被恢复。由于每个请求都是内联处理的，
MCP 取消不属于停机问题（见 [`docs/mcp.md`](../mcp.md)）；SSE 是唯一的长连接，
且已接到 shutdown 信号上。

## POST /search

请求（SearchOS 原生契约）：

```json
{
  "query": "What is the latest Tokio version?",
  "profile": "latest",
  "mode": "local_first",
  "workspace_id": "default",
  "max_external_queries": 3,
  "max_total_cost": 0.05,
  "max_results": 5
}
```

`mode` 接受规范的 snake_case 名称以及文档化的 kebab-case 别名
（`local_only` / `local-only` 等）。其他值返回 400 `VALIDATION_FAILED`；绝不会被
静默降级为可外部调用的模式。`max_results` 在**质量门之后**限制响应条数（1–100）。

响应（RFC-012 12.6）：

```json
{
  "query": "What is the latest Tokio version?",
  "decision": { "path": "external", "reason": "strict_freshness_required" },
  "results": [],
  "quality": { "status": "passed", "freshness": "verified", "coverage": "sufficient", "score": 0.0, "failed_constraints": [] },
  "cost": { "estimated": 0.0032, "actual": null, "currency": "USD" },
  "provenance": [],
  "trace_id": "trace_xxx",
  "status": "completed",
  "warnings": [],
  "task_id": "task_xxx",
  "replayed": false,
  "coverage_gaps": []
}
```

`actual: null` 表示还没有真实账单数据 —— 绝不表示“成本为零”。`status` 是显式的：
`completed | completed_with_warnings | partial | insufficient_evidence |
budget_exhausted | failed | cancelled`（RFC-002 2.8）。`replayed: true` 表示响应来自
`request_id` 幂等账本，没有产生新的外部搜索或账单。`coverage_gaps` 列出最终质量门发现的
未覆盖子问题，并提供稳定的 `requirement_id`、优先级与最小获取策略，供搜索 agent 发起定向
follow-up。`trace show` / `/trace/{id}`
返回带 `stages[]` 结构化阶段记录、`final_strategy` 与 `final_outcome` 的结果
（RFC-004 v1.1 11.1）。

## 搜索 Agent 接口

`POST /search/plan` 只做规划：分析查询、检索本地知识，返回「将要执行」的查询、未覆盖需求与
预估成本，**不发起外部调用、不计费、不写文档**。`POST /search/follow-up` 延续已记录的会话，
未指定 `focus_requirements` 时自动锁定上一轮留下的未覆盖需求。

`POST /search` 接受 `max_acquisition_rounds`（有界迭代检索，查询数与成本上限跨轮累计）、
`session_id`（记录轮次）和 `focus_requirements`（显式跟进需求）。响应包含
`coverage_gaps`、`session_id` 与 `focus_requirements`；本地结果同时返回命中区域的
`content` 与带 `<b>…</b>` 标记的 `highlight`。

## 本地数据导入（RFC-029）

本地内容导入的是与 URL 抓取**同一个**知识库：同样的版本管理、同样的事务性 outbox、
同样的 `SearchIndex` 端口、同样的 workspace 隔离。它立即对 `local_only` 和
`local_first` 搜索可见。

`POST /ingest`（JSON）：

```json
{
  "content": "The quokka is a small macropod found on Rottnest Island.",
  "source_key": "notes/quokka",
  "title": "Quokka notes",
  "content_type": "text/plain",
  "workspace_id": "default"
}
```

`source_key` 是逻辑身份。省略时按内容寻址（`sha256(content)[:16]`），因此相同内容
会去重。用相同 `source_key` 再次导入相同内容返回 `changed: false` 且不产生新版本；
内容变化则创建新版本并保留旧版本。规范 URI：
`searchos-local://<workspace>/<source_key>`。

`POST /ingest/file` 是 `multipart/form-data`，含 `file` 字段以及可选的 `source_key`、
`title`、`content_type`、`language`、`workspace_id`。客户端文件名仅作为标签，
绝不触碰服务器文件系统。

`POST /ingest` 与 `/ingest/file` 返回 **201 Created**；`/ingest/batch` 返回 201，
带 `job_id`、逐条结果和终态 `state`（`completed`、`partial` 或 `failed`）。
`/ingest/batch` 还接受内存中的 `directory.files[]` 数组以做虚拟目录导入
（HTTP 永远不接受服务端路径）。

错误码：`EMPTY_CONTENT`、`CONTENT_TOO_LARGE`、`INVALID_ENCODING`、
`UNSUPPORTED_CONTENT_TYPE`、`WORKSPACE_DENIED`。默认上限：单文档 20 MB、
单批 200 MB，可通过 `ingest.max_bytes_per_document` / `ingest.max_bytes_per_batch` 配置。

## Tavily 兼容

冻结的 Tavily 子集在 [`docs/tavily-compatibility.md`](../tavily-compatibility.md)
中有文档，并由 fixture 套件保证（`fixtures/tavily/` +
`tests/compatibility/tests/tavily_contract.rs`）。

当 `/search` 请求体包含任意 Tavily 键（`search_depth`、`topic`、`include_answer`、
`include_raw_content`、`include_domains`、`exclude_domains`、`include_images`、
`chunks_per_source`、`time_range`、`country`、`include_favicon`、`days`…）时，
网关会把它路由到兼容层并映射到 SearchOS 领域模型。所有不被支持的旋钮 —— 以及所有
未知字段 —— 都会在 `unsupported_features` 中显式报告，该字段始终存在（可能为空）。
`answer` 始终为 `null`：SearchOS 不编造答案（RFC-001 1.2）。不含任何 Tavily 键的
请求体保持原生 envelope，不会长出 Tavily 专属字段。

Tavily 形状的 `/extract`（`{ "urls": [...] }`）每次调用抽取一个 URL：第一个被处理，
其余在 `failed_results` 中附带明确原因返回，并且 `extract_depth` / `format` /
`include_images` / `include_favicon` 会被报告为不支持。

错误使用标准网关 envelope；没有 Tavily 专属的错误形状。`POST /crawl` 与 `POST /map`
**不**属于 v1 子集（返回 `404`，且不在 `docs/openapi.yaml` 中）。
