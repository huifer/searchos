# Architecture

> System Architecture

SearchOS is organized as a layered system with a **shared core** reused by two runtime
modes. The single most important principle is: **business logic lives in Core / Application
Services, never only in an HTTP handler**, so Local Mode and Gateway Mode behave identically.

## Layers

```text
┌──────────────────────────────────────────┐
│              Interface Layer             │
│  CLI   ·   Rust SDK   ·   MCP   ·   HTTP API │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│             Application Layer             │
│  Search · Fetch · Crawl · Memory · Research │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│               Core Runtime                │
│  Query · Planner · Router · Cache ·        │
│  Retrieval · Memory · Crawler · Extension  │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│            Infrastructure                 │
│  SQLite · Tantivy · LanceDB · File/Object  │
│  Queue · Provider · Embedding · Browser    │
└──────────────────────────────────────────┘
```

| Module | Responsibility |
|---|---|
| Interface | CLI, HTTP, MCP, SDK |
| Application | use-case orchestration and entry points |
| Core | search, routing, memory, retrieval |
| Extension | site/provider/capability extensions |
| Infrastructure | database, index, file, queue |
| Runtime | local and distributed execution |

Modules communicate only through explicit Traits and data contracts.

## Dual Runtime

| Capability | Local Mode | Gateway Mode |
|---|---|---|
| Entry | CLI / Rust SDK / MCP stdio | HTTP API / remote MCP |
| Tavily API | not needed | supported |
| Multi-user / Multi-node | not primary | supported |
| Hard constraint | in-process Core, no HTTP loopback | handlers are thin transport only |

## Search Pipeline

```text
Request → Normalize → Intent → Exact Cache → Semantic Cache → Local Retrieval
  → Coverage Eval → (sufficient? return)
  → Planner → Provider Router → External Search → Normalize
  → Dedup → Rerank → Persist → Response
```

## Storage (single-node)

| Concern | Technology |
|---|---|
| Metadata | SQLite (source of truth) |
| Full-text | Tantivy (BM25) — default; remote backends selectable (ADR-027) |
| Vector | LanceDB |
| Object | Local filesystem |
| Cache | SQLite + memory |
| Queue | SQLite-backed job queue |

Indexes are rebuildable and synced from SQLite via a **Transactional Outbox + Index Worker +
Reconciliation**. Local Mode must not require Redis, PostgreSQL, Kafka, NATS, or Docker.

### Remote full-text backends (ADR-027)

`storage.fulltext` selects the retrieval engine. The default stays embedded Tantivy;
remote backends are opt-in and user-configured under `storage.remote.<backend>`:

| `storage.fulltext` | Engine | Adapter crate | Notes |
|---|---|---|---|
| `tantivy` (default) | Tantivy BM25 | `searchos-index-tantivy` | zero-dependency, offline |
| `elasticsearch` | Elasticsearch REST | `searchos-index-remote` | BM25 multi_match, `refresh=wait_for` |
| `meilisearch` | Meilisearch REST | `searchos-index-remote` | task-queue writes awaited |
| `postgres` | PostgreSQL FTS | `searchos-index-remote` | GIN tsvector, `ts_rank_cd` |
| `mysql` | MySQL FULLTEXT | `searchos-index-remote` | ngram parser (CJK), boolean mode |

Contract preserved across backends: SQLite remains the metadata source of truth, every
remote index stays rebuildable via the outbox/`rebuild_index` path (ADR-022), and
workspace isolation (`requester OR *`) is enforced server-side or re-checked client-side.

## Retrieval

BM25 + Vector + Metadata → **RRF Fusion** → Reranker → Top-K. Embedding must record model id,
version, dimensions, normalization, and generation time so models can be swapped and indexes
rebuilt safely.

## Local ingestion (RFC-029)

Local content (text, files, directories) is imported through `IngestService`,
a sibling of `FetchService`. Both feed the **same** `DocumentPort`, so local
documents get identical versioning, chunking, outbox indexing, workspace
isolation and rebuild behaviour — they are first-class citizens in
`local_only` / `local_first` search, not a parallel store.

- Canonical URI: `searchos-local://<workspace>/<source_key>`; `source_key` is
  caller-supplied or content-addressed (`sha256(content)[:16]`).
- `sources.source_type` records `web` | `local_text` | `local_file` |
  `local_directory`; local sources are never age-stale (`ContentCategory::Local`).
- Entries: CLI `searchos ingest` (Local Mode, in-process) and HTTP
  `POST /ingest`, `/ingest/file`, `/ingest/batch`, `GET /ingest/jobs/:id`.
- Bounds: 20 MB per document / 200 MB per batch by default; content-type
  sniffing, strict UTF-8, and HTML sanitization reuse the fetch path's rules.

See [`docs/rfc/rfc-029-local-data-ingestion.md`](rfc/rfc-029-local-data-ingestion.md).

## Provider Keys (multi-key rotation)

Upstream providers accept multiple API keys; SearchOS rotates across them so one
rate-limited or exhausted key never fails the whole search (RFC-009 9.7).

| Setting | Meaning |
|---|---|
| `TAVILY_API_KEYS` / `BRAVE_API_KEYS` | comma/space-separated keys; wins over the single-key env |
| `providers.<id>.api_keys` | config-file key list (env still wins) |
| `providers.<id>.rotation` | `round_robin` (default) or `failover` (sticky until failure) |

Per-key health is tracked independently: 429 → rate-limited until Retry-After; 401/403/quota
→ blocked (1800s / 3600s); timeouts/network → consecutive-failure counter. Keys are never logged
or exposed — status output shows masked forms only (`tvly-****-abcd`). When every key is
exhausted, the provider reports 429 with the earliest release time so the caller can honor it.

See the canonical spec: [RFC-001](rfc/rfc-001-product-architecture.md).
