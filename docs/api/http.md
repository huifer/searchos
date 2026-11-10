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
| POST | `/documents/search` | Persisted document lookup |
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
shutdown concern (see [`docs/mcp.md`](mcp.md)); SSE is the only long-lived
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
