# RFC-026: Visualization & Analytics Architecture

> 可视化与分析层架构

## English

Source: ChatGPT share `6af054b8-a43c-83ee-9218-d443cbac9f47`, messages #92/#95/#96.
Status: Implemented (MVP scope), with later-stage modules explicitly deferred.

### 1. Position

Visualization is a first-class architecture layer, not a post-hoc dashboard.
SearchOS persists decisions, evidence, document versions, provider calls,
crawler runs, resource consumption and cost events; those facts need one
standardized Analytics Read Model.

### 2. Layered architecture (implemented)

```text
SearchOS Core (search / cost / document / crawler)
  -> Domain Events (best-effort, append-only analytics_events ledger)
  -> Aggregation Engine (AnalyticsRepo::rebuild)
  -> Analytics Read Models (analytics_read_models)
  -> AnalyticsQueryService (application/analytics.rs)
  -> Dashboard API (/api/v1/dashboard/*)
  -> Next.js server proxy
  -> React Query hooks
  -> shadcn/ui + Recharts components
```

The browser never reads `search_tasks`, `cost_events`, `decision_traces` or any
other operational table directly. Handler code contains no SQL.

### 3. Runtime boundary

- **Local Mode stays in-process.** No HTTP server is started for analytics.
  CLI export and Rust SDK queries remain the local surface.
- **Gateway Mode** exposes the Web Dashboard API. Both modes share the same
  Analytics Core, metric definitions and Read Model contracts.

### 4. Non-blocking analytics

`TaskRecorder::record_search_completed` is best-effort: event write failures
are ignored by the search pipeline. Read Model refresh failures are reported in
`meta.data_freshness` as `stale`, never as fabricated zeros.

### 5. Information architecture implemented

Overview, Search, Traces, Analytics, Cost, Knowledge. The Analytics page groups
Search / Cost / Quality / Knowledge / Providers / Crawlers / Infrastructure
views through shadcn Tabs. The source RFC lists nine first-level modules; the
remaining dedicated pages can be split later without changing the Read Model.

### 6. Design system

Per the source design: professional SaaS dashboard, Light / Dark, high
information density, charts plus data tables, color only for status, no large
empty cards, no excessive rounding, tooltips on charts, explicit time range,
empty states, missing data never shown as zero, estimates separated from
actuals. Implemented with Tailwind CSS v4 and shadcn/ui (Radix primitives +
CVA), Recharts as the replaceable Chart Adapter, and TanStack Query as the
Query Hook layer.

### 7. Deferred

Dedicated SSE-driven live task views, GPU/RAM/CPU sampling, document-level
freshness buckets, cursor pagination on all detail endpoints, and full
multi-tenant RBAC remain later-stage. Where data is not instrumented the API
returns `null` plus a reason, not `0`.

---

## 中文

源设计明确要求把可视化提升为独立的 **Visualization & Analytics Layer**，并规定
「网页不是直接读取内部数据表，而是通过统一 Analytics Read Model 获取经过聚合、
授权和标准化的数据」。本次实现：追加 `analytics_events` 事件账本、构建
`analytics_read_models` 读模型、在应用层定义 `AnalyticsQueryService`、在网关
暴露 `/api/v1/dashboard/*`，前端只通过 Dashboard API 与 React Query 取数。

Local Mode 仍保持 In-Process，不因为可视化启动 HTTP Server；Gateway Mode 才
提供 Web Dashboard API。事件写入与读模型刷新失败都不阻塞搜索主流程，读模型
过期时在 `meta.data_freshness` 标注 `stale`。

视觉规范按源文档落地：支持 Light / Dark、信息密度优先、图表与表格并重、
颜色只表达状态、避免大面积空白与过度圆角、图表带 Tooltip 与时间范围、
空态明确、缺失数据不伪装为 0、估算值与实际值分离。技术上采用 Tailwind CSS v4、
shadcn/ui（Radix + CVA）、Recharts、TanStack Query 与 next-themes。

SSE 实时任务视图、资源采样、文档新鲜度分桶、全量游标分页与多租户 RBAC 属于
后续阶段；未埋点的指标返回 `null` 并说明原因，不返回伪造的 0。
