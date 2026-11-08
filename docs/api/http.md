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
| GET | `/decisions/:trace_id` | Explainable decision trace |
| GET | `/health` | Status + index consistency |

Auth: `Authorization: Bearer <token>` when `server.auth_token` is configured
(RFC-012 12.3; empty token disables auth for local dev).

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
  "trace_id": "trace_xxx"
}
```

`actual: null` means no real bill data yet — never "cost is zero".

## Tavily compatibility

The gateway accepts Tavily-shaped requests on `/search`
(`search_depth`, `topic`, `max_results`, `include_domains`,
`exclude_domains`, `days`, `include_raw_content`) and maps them to the
SearchOS domain model. Unsupported features are reported explicitly in
`unsupported_features` (RFC-012 12.7). `answer` is always `null` — SearchOS
does not fabricate answers (RFC-001 1.2).
