# SearchOS Console

> 搜索运行时可视化控制台

## English

A Next.js 16 / React 19 operator console over the Rust SearchOS **Analytics
Read Model**. It is a presentation layer, not a second search engine and not a
direct SQLite client.

### Concrete stack

```text
Next.js 16 App Router (RSC + Route Handlers)
React 19 + TypeScript strict
Tailwind CSS v4 (CSS-first @theme tokens, @tailwindcss/postcss)
shadcn/ui (components.json, Radix UI primitives, CVA, cn)
lucide-react (shadcn official icon family)
Recharts 3 (replaceable Chart Adapter with Tooltip)
TanStack Query v5 (Query Hook layer, dedup, polling)
next-themes (Light / Dark)
Geist Sans / Geist Mono (locally bundled)
```

Backend: Rust Axum gateway, SQLite schema v5 (analytics tables added in v3), `AnalyticsQueryService`,
`/api/v1/dashboard/*`, SSE events, CSV/JSON export.

### Pages

| Route | What it shows |
|---|---|
| `/` | 8 KPI cards with denominators/definitions/drill-down, Recharts activity and strategy charts, recent requests, export |
| `/search` | shadcn form workbench; decision, evidence, warnings, raw JSON |
| `/traces` | staged decision timeline, metadata, raw JSON |
| `/analytics` | Tabs: Search / Cost / Quality / Knowledge / Providers / Crawlers / Infrastructure |
| `/cost` | ledger, four cost dimensions, reuse views, reservations, reconciliation |
| `/knowledge` | documents, memory, profiles, growth/freshness, runtime health |
| `/seo` | social card previews, emitted meta tags, bilingual OG assets |

### Data flow

```text
Page -> TanStack Query hook -> lib/client.ts -> Next.js proxy
     -> /api/v1/dashboard/* -> AnalyticsQueryService -> analytics_read_models
```

Chart components receive standardized series only. The bearer token is
server-side only. SSE is a refetch hint, not the source of historical truth.

### Run

```bash
# Terminal 1: Rust gateway
cd ..
cargo build --bin searchos
SEARCHOS_DATA_DIR=$(mktemp -d) ./target/debug/searchos gateway start --port 8787

# Terminal 2: console
cd web
cp .env.example .env.local      # optional, defaults to http://127.0.0.1:8787
npm ci
npm run dev                     # http://localhost:3100
```

Production: `npm run build && npm start`.

### Design

Per [RFC-026](../docs/rfc/rfc-026-visualization-analytics-architecture.md):
professional SaaS dashboard, Light / Dark, dense data, one accent, square
corners, rigid grids, monospace telemetry, tooltips on charts, explicit time
ranges, empty states, estimates separated from actuals, missing data never
shown as zero. `prefers-reduced-motion` is honored. Landing-page design skills
are explicitly out of scope for dashboards.

### Social preview (OG images)

Bilingual social preview set, rendered from code (never an AI image model, so
text stays exact) by `python3 web/scripts/generate-og.py`, which converts the
`.dark` oklch tokens in `app/globals.css` to sRGB and draws with the Geist TTFs
bundled by `geist` (Latin) and a local Noto Sans CJK SC (中文):

| File | Size | Use |
|---|---|---|
| `og.png` | 1200x630 | default Open Graph / Twitter card (EN), wired in `app/layout.tsx` |
| `og-zh.png` | 1200x630 | 中文版卡片，中文社区分享时直接引用 |
| `og-github.png` | 1280x640 | EN GitHub repo social preview |
| `og-zh-github.png` | 1280x640 | 中文 GitHub 仓库社交预览 |

`og:image` accepts one image per page, so the EN card is the default and the
中文 card is linked directly in Chinese contexts. Re-render after a token
change: `python3 web/scripts/generate-og.py [--lang en|zh]`. Set
`NEXT_PUBLIC_SITE_URL` for absolute image URLs in production.

### Verify

```bash
npm run typecheck
npm run build
npm test                     # Vitest + Testing Library
curl -s http://localhost:3100/api/searchos/api/v1/dashboard/overview
```

The unit suite (`**/*.test.ts{,x}`) runs in jsdom and covers the API client
(transport, error envelopes, `useApi`, formatters), the server-side proxy
(nested paths, bearer token, multipart byte-for-byte forwarding, path traversal
rejection), Analytics filter provenance (Live SQL / Precomputed and ignored
filters), and the knowledge UI (text/file/batch ingest, strict UTF-8,
document detail, re-index, delete confirmation, workspace/source filters).
CI runs it in the `frontend-test` job.

### Localization

The console ships **English and Simplified Chinese**. Use the `EN / 中文` toggle
in the top bar; the choice is stored in `localStorage` and mirrored to
`<html lang>` (a `zh-*` browser language selects Chinese on the first visit).
`lib/i18n.tsx` holds the dictionary: the English string is the key, so any
untranslated copy falls back to English instead of rendering blank. Technical
identifiers — API paths, field names, metric codes — deliberately stay in
English so logs, screenshots and docs remain greppable. Page intros, panel
titles, KPI/table labels, controls, tabs and empty/error states are translated
through the shared primitives in `components/ui.tsx`; `npm test` covers the
dictionary, fallback, persistence and the language toggle.

---

## 中文

这是基于 **Next.js 16 + React 19 + Tailwind CSS v4 + shadcn/ui + Radix +
lucide-react + Recharts + TanStack Query + next-themes** 的可视化控制台，
数据源是 Rust 网关的 `/api/v1/dashboard/*` Analytics Read Model，不直接读
SQLite 业务表。

七个页面覆盖 Overview、Search、Traces、Analytics、Cost、Knowledge、SEO。所有 KPI
带分母、定义与下钻，图表带 Tooltip 与时间范围，加载 / 空 / 错误态齐全；
支持 Light / Dark、CSV / JSON 导出与 SSE 刷新提示。

启动与验证命令见上方。单元套件（`**/*.test.ts{,x}`）在 jsdom 中运行，覆盖 API
client（传输、错误 envelope、`useApi`、格式化）、服务端代理（嵌套路径、bearer token、
multipart 字节级转发、路径穿越拒绝）、Analytics 筛选溯源（Live SQL / Precomputed
与忽略项提示），以及知识库 UI（文本/文件/批量导入、严格 UTF-8、文档详情、重新索引、
删除确认、workspace/来源过滤）。CI 在 `frontend-test` job 中运行。

### 本地化

控制台内置**英文与简体中文**。顶栏的 `EN / 中文` 切换会把选择写入 `localStorage` 并同步到 `<html lang>`（浏览器语言为 `zh-*` 时首次访问默认中文）。
`lib/i18n.tsx` 以英文原文作为字典键，未翻译文案自动回退英文，不会出现空白。API 路径、字段名、指标码等技术标识刻意保留英文，方便日志与文档对照检索。
页面标题、面板标题、KPI / 表格标签、控件、Tab、空态与错误态都通过 `components/ui.tsx` 的共享组件翻译；`npm test` 覆盖字典、回退、持久化与语言切换。

视觉遵循 RFC-026：信息密度优先、单一强调色、直角、状态色不用于装饰、
缺失数据不伪装为 0、估算值与实际值分离。

### 社交预览图（OG image）

中英文各一套，均由代码生成（不使用 AI 生图，保证文字精确）：
`python3 web/scripts/generate-og.py` 依据 `app/globals.css` 的 oklch token
与 `geist` 包内字体（拉丁文）加本地 Noto Sans CJK SC（中文）重新渲染：

| 文件 | 尺寸 | 用途 |
|---|---|---|
| `og.png` | 1200x630 | 默认 Open Graph / Twitter 卡片（英文），已接入 `app/layout.tsx` |
| `og-zh.png` | 1200x630 | 中文版卡片，中文社区分享时直接引用 |
| `og-github.png` | 1280x640 | 英文 GitHub 仓库社交预览 |
| `og-zh-github.png` | 1280x640 | 中文 GitHub 仓库社交预览 |

每个页面的 `og:image` 只能挂一张图，因此默认英文卡片，中文场景直接引用中文
文件。改 token 后重新生成：`python3 web/scripts/generate-og.py
[--lang en|zh]`。生产环境请设置 `NEXT_PUBLIC_SITE_URL` 以输出绝对图片地址。
