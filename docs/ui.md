# Visual Console & Analytics Layer

> 可视化控制台与分析层

## English

Before this work SearchOS had **no visual interface at all**: CLI text, MCP
JSON-RPC and gateway JSON only. The source UI design share
(`https://chatgpt.com/share/6af054b8-a43c-83ee-9218-d443cbac9f47`) requires an
independent Visualization & Analytics Layer, not a thin dashboard. Three new
RFCs are now part of the baseline:

- [RFC-026](rfc/rfc-026-visualization-analytics-architecture.md)
- [RFC-027](rfc/rfc-027-analytics-data-model.md)
- [RFC-028](rfc/rfc-028-dashboard-api-frontend.md)

### 1. Architecture

```text
Browser (localhost:3100)
  -> Next.js Route Handler proxy (/api/searchos/*)
  -> SEARCHOS_GATEWAY_URL (Axum, 127.0.0.1:8787)
  -> /api/v1/dashboard/* (Dashboard API)
  -> AnalyticsQueryService
  -> analytics_read_models (Analytics Read Model)
  -> analytics_events + operational records
```

The browser never reads operational tables. The gateway handlers contain no
SQL; aggregation lives in `AnalyticsRepo::rebuild`. Event writes are
best-effort and never fail a search. SSE (`/api/v1/dashboard/events`) is a
refetch hint only; the Read Model remains the historical truth.

### 2. Concrete mainstream stack

| Layer | Technology |
|---|---|
| Framework | Next.js 16 App Router, React 19, TypeScript strict |
| Styling | Tailwind CSS v4, CSS-first `@theme` tokens |
| Components | shadcn/ui (`components.json`, Radix UI primitives, CVA, `cn`) |
| Icons | lucide-react (shadcn official icon library) |
| Charts | Recharts 3, wrapped as a replaceable Chart Adapter with Tooltip |
| Data | TanStack Query v5 hooks, server-side proxy, no direct client SQL |
| Theme | next-themes, Light / Dark, `prefers-reduced-motion` |
| Fonts | Geist Sans / Geist Mono bundled locally |
| Backend | Rust Axum gateway, SQLite migration v3, standard error envelope |

`design-taste-frontend` explicitly excludes dashboards; only its universal
rules are applied (single accent, shape lock, WCAG contrast, reduced motion,
state coverage, zero em-dashes). `industrial-brutalist-ui` Tactical Telemetry
supplies the visual language: dark substrate, one hazard-red accent, a single
green connectivity indicator, square corners, rigid grids, monospace
telemetry. `high-end-visual-design` landing-page directives are not applied.

### 3. Pages

| Route | Content |
|---|---|
| `/` | 8 KPI cards with denominators, definitions, drill-down, time/interval filter, Recharts search activity and strategy charts, recent requests, CSV/JSON export |
| `/search` | shadcn form workbench, decision/evidence tabs, warnings, raw JSON, trace links |
| `/traces` | staged decision timeline, metadata, raw JSON |
| `/analytics` | Tabs for Search / Cost / Quality / Knowledge / Providers / Crawlers / Infrastructure from the Dashboard API |
| `/cost` | ledger, four-dimension cost breakdown, reuse views, reservations, billing reconciliation |
| `/knowledge` | document explorer, memory search, versioned profiles, Read Model growth/freshness, runtime health |
| `/seo` | social card previews (X / GitHub), the meta tags this site emits, and the bilingual OG asset set |

### 4. UI requirements from the source design, implemented

- Professional SaaS dashboard, high information density, charts plus tables.
- Light / Dark theme toggle.
- Color only expresses state and metrics; one accent, no decorative gradients.
- No excessive radius (`--radius: 0rem`), no large empty cards.
- Every KPI has a denominator, definition tooltip and drill-down.
- Every chart has a Tooltip, explicit time range, legend and accessible data
  table alternative.
- Loading skeletons, empty states and inline error states everywhere.
- Missing data is shown as `--` / `null` with a reason, never as `0`.
- Estimates are badged and separated from reconciled actuals.
- Workspace authorization is enforced in the Dashboard API.
- CSV / JSON export from the Overview Read Model.

### 5. Deferred (not gaps in this pass)

Dedicated realtime task board, CPU/GPU/RAM/network sampling, document-level
freshness buckets, cursor pagination on all detail endpoints, PNG/PDF export,
formula-version migration UI and multi-tenant RBAC. Where data is not
instrumented, the API returns `null` plus a reason.

### 6. Run and verify

```bash
# Rust gateway
cargo build --bin searchos
SEARCHOS_DATA_DIR=$(mktemp -d) ./target/debug/searchos gateway start --port 8787

# Console
cd web
npm ci
npm run dev        # http://localhost:3100

# Gates
npm run typecheck
npm run build
cargo test --workspace
SRC_DIR=crates/searchos-interface-http/src \
  bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh
```

CI runs the `console` job (`npm ci`, `typecheck`, `build`) alongside the Rust
gates in `.github/workflows/ci.yml`.

---

## 中文

源 UI 设计分享要求把可视化提升为独立架构层，新增 RFC-026/027/028，并明确：
前端不能直接读业务表，必须通过 Analytics Read Model；Local Mode 不因为可视化
启动 HTTP Server；Light / Dark、信息密度、图表 Tooltip、时间范围、空态、
估算/实际区分、缺失数据不伪装为 0、KPI 可解释与可下钻都要落地。

本次在 Rust 侧新增 `analytics_events` 与 `analytics_read_models`（迁移 v3）、
`AnalyticsQueryService`、`/api/v1/dashboard/*`、CSV/JSON 导出与 SSE；在前端把
原生 CSS 控制台升级为 Next.js 16 + React 19 + Tailwind CSS v4 + shadcn/ui
（Radix + CVA）+ lucide-react + Recharts + TanStack Query + next-themes 的具体
主流技术组合。

页面覆盖 Overview、Search、Traces、Analytics、Cost、Knowledge、SEO；Analytics 页用
Tabs 组织 Search / Cost / Quality / Knowledge / Providers / Crawlers /
Infrastructure。所有 KPI 带分母与定义 Tooltip，所有图表带 Tooltip、时间范围、
图例及可访问数据替代，所有页面有加载 / 空 / 错误态。

未实现的资源采样、实时任务看板、文档新鲜度分桶、全量游标分页、PNG/PDF 导出
与多租户 RBAC 属于后续阶段；未埋点指标返回 `null` 加原因，不返回伪造的 0。
