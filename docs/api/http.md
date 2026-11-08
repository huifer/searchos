# HTTP API Reference

> RFC-012 12.4/12.5/12.6 — Axum gateway surface. Handlers are thin
> transport: auth, DTO validation, application call, response mapping.

## Endpoints

| Method | Path | Purpose |
|---|---|---|
| POST | `/search` | Execute a search (quality-gated, cost-aware) |
| POST | `/extract` | Fetch + extract + persist a URL |
| POST | `/documents` | Fetch and persist a document |
| GET | `/documents/:id` | Document + current version |
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
| POST | `/documents/search` | Persisted document lookup |
| GET | `/health` | Status + index consistency |

Every route is also mounted under `/v1` (RFC-013 13.7) and responses carry
`x-searchos-api-version`. No catch-all routes are used; the Axum route-conflict
scanner is run on every route change. `server.cors_origin` enables OPTIONS
preflight and CORS headers for direct browser access; the bundled Next.js
console uses a server-side proxy instead (see [`docs/ui.md`](../ui.md)).
`POST /search` accepts an optional `request_id` idempotency key.

## Dashboard API (RFC-028)

All Dashboard responses are `{ data, meta }` with `generated_at`, `time_range`,
`aggregation_version`, `data_freshness` and `workspace_scope`. Handlers never
write SQL; they call `AnalyticsQueryService` over `analytics_read_models`.

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

## POST /search

Request:

```json
{
  "query": "What is the latest Tokio version?",
  "profile": "latest",
  "mode": "local_first",
  "workspace_id": "default",
  "max_external_queries": 3,
  "max_total_cost": 0.05
}
```

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
  "replayed": false
}
```

`actual: null` means no real bill data yet — never "cost is zero".
`status` is explicit: `completed | completed_with_warnings | partial |
insufficient_evidence | budget_exhausted | failed | cancelled` (RFC-002 2.8).
`replayed: true` means the response came from the `request_id` idempotency
ledger; no new external search or bill was created. `trace show` /
`/trace/{id}` returns `stages[]` with structured stage records,
`final_strategy` and `final_outcome` (RFC-004 v1.1 11.1).

## Tavily compatibility

The gateway accepts Tavily-shaped requests on `/search`
(`search_depth`, `topic`, `max_results`, `include_domains`,
`exclude_domains`, `days`, `include_raw_content`) and maps them to the
SearchOS domain model. Unsupported features are reported explicitly in
`unsupported_features` (RFC-012 12.7). `answer` is always `null` — SearchOS
does not fabricate answers (RFC-001 1.2).
