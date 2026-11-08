---
name: SearchOS visual console
description: The Next.js operator console under web/, its mainstream stack and Analytics Read Model contract
type: project-fact
---

The repo's only visual interface is `web/` (Next.js 16 App Router + React 19 +
TypeScript + Tailwind CSS v4 + shadcn/ui). Before it existed, interfaces were
CLI text, MCP JSON-RPC and gateway JSON only. Documented in `docs/ui.md`,
`web/README.md` and RFC-026/027/028.

**Why:** the user asked whether all UI surfaces existed, requested a React /
Next.js visual layer, then required a concrete mainstream stack (Next.js +
shadcn/ui) and pointed at a UI design share that specifies a first-class
Visualization & Analytics Layer.

**How to apply:**
- Browser never reads operational tables. Data flow: page -> TanStack Query
  hook -> Next.js server proxy `app/api/searchos/[...path]/route.ts` ->
  `/api/v1/dashboard/*` -> `AnalyticsQueryService` -> `analytics_read_models`.
  `SEARCHOS_AUTH_TOKEN` is server-side only.
- Concrete stack: Tailwind v4 CSS-first `@theme`, shadcn/ui (`components.json`,
  Radix primitives, CVA, `cn`), lucide-react, Recharts 3 Chart Adapter with
  Tooltip, TanStack Query v5, next-themes Light/Dark, bundled Geist fonts.
- Console routes: `/` overview, `/search` workbench, `/traces` staged trace
  inspector, `/analytics` tabbed Read Model views, `/cost` ledger +
  reconciliation, `/knowledge` documents/memory.
- Gateway additions for the console: `GET /traces`, `GET /system/status`,
  `request_id` on `POST /search`, `server.cors_origin` + OPTIONS preflight,
  `/api/v1/dashboard/*`, SSE `/events`, CSV/JSON export.
- Rust Analytics layer: migration v3 tables `analytics_events` and
  `analytics_read_models`; `AnalyticsRepo::rebuild`; best-effort
  `TaskRecorder::record_search_completed`; `data_freshness` in `meta`.
- Design: tactical-telemetry / industrial-grid (`industrial-brutalist-ui`),
  Light/Dark via next-themes, one hazard-red accent, a single green
  connectivity indicator, square corners (`--radius: 0rem`), monospace
  telemetry, one icon family, no Tailwind-default AI-purple. `design-taste-frontend`
  excludes dashboards, so only its universal rules apply.
- OG/social preview images are generated in code, never by an image model:
  `web/scripts/generate-og.py` converts the `.dark` oklch tokens from
  `web/app/globals.css` to sRGB, renders with the Geist Sans/Mono TTFs from
  `node_modules/geist` (Latin) plus a local Noto Sans CJK SC (Chinese),
  supersamples at 2x then LANCZOS-downsamples. Four outputs in `web/public/`:
  `og.png` (1200x630 EN), `og-zh.png` (1200x630 中文), `og-github.png` and
  `og-zh-github.png` (1280x640 GitHub). The same script is the only way to
  re-render them after a token change. `og:image` holds one image per page so
  EN is wired via `metadataBase` (`NEXT_PUBLIC_SITE_URL`, fallback
  `http://localhost:3100`) + `openGraph`/`twitter` metadata in `app/layout.tsx`
  (with `og:locale:alternate zh_CN`); verified served through
  `next start -p <port>`.
- Verification: `cd web && npm ci && npm run typecheck && npm run build`;
  `cargo test --workspace`; `SRC_DIR=crates/searchos-interface-http/src bash
  ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh`; CI has a
  `console` job in `.github/workflows/ci.yml`.

**Related:** [[searchos-project]], [[searchos-verification]], [[searchos-decision-red-lines]], docs/ui.md
