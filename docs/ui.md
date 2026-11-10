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
       ├─ no filters + interval=day -> analytics_read_models (precomputed fast path)
       └─ any filter / other interval -> live SQL with the predicates in the WHERE clause
  -> analytics_events + operational records
```

The browser never reads operational tables. The gateway handlers contain no
SQL; aggregation lives in `AnalyticsRepo` — `rebuild` for the precomputed
Read Models and `query_dataset` for filtered, live queries. A stored payload
is reused only while the operational source watermark is unchanged; a new
search, cost event or document forces a rebuild. Event writes are best-effort
and never fail a search. SSE (`/api/v1/dashboard/events`) is a refetch hint
only; the Read Model remains the historical truth.

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
| Backend | Rust Axum gateway, SQLite schema v5 (analytics tables introduced in v3), standard error envelope |

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
| `/analytics` | Tabs for Search / Cost / Quality / Knowledge / Providers / Crawlers / Infrastructure from the Dashboard API; a Live SQL / Precomputed badge plus per-filter applicability notices |
| `/cost` | ledger, four-dimension cost breakdown, reuse views, reservations, billing reconciliation |
| `/knowledge` | import text / files into the same store as URL fetches (batch job status + per-item failures), a workspace-scoped document library with versions / re-index / soft delete, memory search, versioned profiles, Read Model growth/freshness, runtime health |
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

### 5. Bilingual UI (EN / 简体中文)

A language toggle in the top bar switches the console between English and
Simplified Chinese. The choice is persisted in `localStorage` and mirrored to
`<html lang>`; a `zh-*` browser language selects Chinese on the first visit.
`web/lib/i18n.tsx` uses the English string as the dictionary key, so
untranslated copy falls back to English instead of rendering blank, and
technical identifiers (API paths, field names, metric codes) deliberately stay
in English so logs and screenshots remain greppable. Page intros, panel titles,
KPI/table labels, controls, tabs and empty/error states are translated through
the shared primitives in `web/components/ui.tsx`; `web/lib/i18n.test.tsx`
covers the dictionary, fallback, persistence and the toggle.

### 6. Deferred (not gaps in this pass)

Dedicated realtime task board, CPU/GPU/RAM/network sampling, document-level
freshness buckets, cursor pagination on all detail endpoints, PNG/PDF export,
formula-version migration UI and multi-tenant RBAC. Where data is not
instrumented, the API returns `null` plus a reason.

### 7. Run and verify

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

本次在 Rust 侧新增 `analytics_events` 与 `analytics_read_models`（迁移 v3 引入，当前 schema 为 v5）、
`AnalyticsQueryService`、`/api/v1/dashboard/*`、CSV/JSON 导出与 SSE；在前端把
原生 CSS 控制台升级为 Next.js 16 + React 19 + Tailwind CSS v4 + shadcn/ui
（Radix + CVA）+ lucide-react + Recharts + TanStack Query + next-themes 的具体
主流技术组合。

页面覆盖 Overview、Search、Traces、Analytics、Cost、Knowledge、SEO；Analytics 页用
Tabs 组织 Search / Cost / Quality / Knowledge / Providers / Crawlers /
Infrastructure，并用 Live SQL / Precomputed 徽章与逐条筛选可用性提示声明每个
视图到底用了哪些筛选。所有 KPI 带分母与定义 Tooltip，所有图表带 Tooltip、
时间范围、图例及可访问数据替代，所有页面有加载 / 空 / 错误态。

时间范围、interval、workspace、agent、mode、provider、quality 全部下推到 SQL，
而不是拿全局预计算结果事后过滤；无筛选且 `interval=day` 时复用已存储的 Read
Model，仅当源表水位线变化才重建。

未实现的资源采样、实时任务看板、文档新鲜度分桶、全量游标分页、PNG/PDF 导出
与多租户 RBAC 属于后续阶段；未埋点指标返回 `null` 加原因，不返回伪造的 0。

### 双语文案

控制台顶栏的 `EN / 中文` 开关会切换英文与简体中文，选择写入 `localStorage`
并同步到 `<html lang>`；浏览器语言为 `zh-*` 时首次访问默认中文。
`web/lib/i18n.tsx` 以英文原文作为字典键，未翻译文案自动回退英文，
API 路径、字段名、指标码等技术标识刻意保留英文。页面标题、面板标题、
KPI / 表格标签、控件、Tab 与空态 / 错误态都通过共享组件翻译，
`web/lib/i18n.test.tsx` 覆盖字典、回退、持久化与切换。
