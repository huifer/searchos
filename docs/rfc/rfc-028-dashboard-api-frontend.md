# RFC-028: Dashboard API & Frontend Integration

> Dashboard API 与前端集成

## English

### 1. Base path and envelope

```text
/api/v1/dashboard
```

Every response is wrapped:

```json
{
  "data": {},
  "meta": {
    "generated_at": "...",
    "time_range": { "from": null, "to": null, "timezone": "UTC" },
    "interval": "day",
    "aggregation_version": "1",
    "data_freshness": "complete",
    "workspace_scope": "*"
  }
}
```

Errors use the standard gateway envelope (`INVALID_TIME_RANGE` maps to
`VALIDATION_FAILED` in the unified error taxonomy). Internal SQL and stack
traces are never returned.

### 2. Endpoints implemented

| Group | Paths |
|---|---|
| Overview | `GET /overview` |
| Search | `GET /search/summary`, `/search/timeseries`, `/search/strategies`, `/search/requests` |
| Cost | `GET /cost/summary`, `/cost/breakdown`, `/cost/savings` |
| Knowledge | `GET /knowledge/summary`, `/knowledge/documents`, `/knowledge/freshness` |
| Quality | `GET /quality/summary`, `/quality/dimensions`, `/quality/failures` |
| Providers | `GET /providers` |
| Crawlers | `GET /crawlers/summary` |
| Infrastructure | `GET /infrastructure/summary` |
| Realtime | `GET /events` (SSE `analytics.events`) |
| Export | `GET /export?format=json|csv` |

Common query contract: `workspace_id`, `project_id`, `agent_id`, `provider_id`,
`search_mode`, `quality_status`, `from`, `to`, `interval`, `timezone`, `limit`,
`cursor`. Inverted ranges and unsupported intervals are rejected. MVP accepts
UTC only.

### 3. Security

All Dashboard handlers authenticate and enforce `server.workspace_tokens`
workspace binding. The bearer token stays server-side in the Next.js proxy.
Sensitive provider keys, private tokens and raw document bodies are never
exposed by default.

### 4. Frontend stack

```text
Next.js 16 App Router (RSC + Route Handlers)
React 19 + TypeScript strict
Tailwind CSS v4 (CSS-first @theme tokens)
shadcn/ui (Radix UI primitives + class-variance-authority)
lucide-react (official shadcn icon family)
Recharts 3 (replaceable Chart Adapter with Tooltip)
TanStack Query v5 (Query Hook / dedup / polling)
next-themes (Light / Dark)
Geist fonts (bundled locally)
```

Data flow: `Page -> React Query hook -> API service -> /api/searchos proxy ->
SearchOS Dashboard API`. Chart components receive standardized series only.

### 5. Frontend structure

```text
web/
  app/                     pages + /api/searchos/[...path] proxy
  components/ui/           shadcn/ui primitives
  components/              Shell, providers, theme toggle, chart adapters
  lib/client.ts            fetch + React Query hook + formatters
  lib/analytics.ts         typed Dashboard API hooks
  lib/types.ts             transport types
  app/globals.css          Tailwind v4 + shadcn tokens + industrial layout
  components.json          shadcn CLI config
```

### 6. UI guarantees

- Light / Dark toggle with `prefers-reduced-motion` support.
- KPI cards with denominator, definition tooltip and drill-down link.
- Recharts charts with Tooltip, explicit time range, legend and an accessible
  data-table alternative.
- Loading skeletons, empty states and inline error states on every page.
- Estimate badges; missing metrics shown as `--`, not zero.
- CSV / JSON export from the Overview Read Model.
- SSE is a refetch hint only; the Read Model remains the historical truth.

### 7. Deferred

Full cursor pagination on detail endpoints, dedicated realtime task board,
PNG/PDF export, metric formula version migration UI, and multi-tenant RBAC
remain later-stage. These do not block the implemented acceptance surface.

---

## 中文

所有 Dashboard 端点统一挂在 `/api/v1/dashboard`，响应统一带 `data` 与 `meta`
（生成时间、时间范围、聚合版本、数据新鲜度、Workspace 范围）。Handler 只做鉴权、
参数校验、调用 Analytics Query Service 与响应包装，不含 SQL。

前端明确采用 Next.js 16、React 19、TypeScript、Tailwind CSS v4、shadcn/ui、
Radix、lucide-react、Recharts、TanStack Query 与 next-themes。页面通过 React
Query Hook 调用 Dashboard API，图表只接受标准化数据；Token 仅存在于 Next.js
服务端代理。

UI 保证：Light / Dark、减少动效、KPI 带分母与定义 Tooltip、图表带 Tooltip 与
时间范围、加载 / 空 / 错误态齐全、估算值标记、缺失数据不显示为 0、CSV / JSON
导出，以及 SSE 仅作为刷新提示。全量游标分页、实时任务看板、PNG/PDF 导出与
多租户 RBAC 留待后续阶段。
