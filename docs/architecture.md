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
| Full-text | Tantivy (BM25) |
| Vector | LanceDB |
| Object | Local filesystem |
| Cache | SQLite + memory |
| Queue | SQLite-backed job queue |

Indexes are rebuildable and synced from SQLite via a **Transactional Outbox + Index Worker +
Reconciliation**. Local Mode must not require Redis, PostgreSQL, Kafka, NATS, or Docker.

## Retrieval

BM25 + Vector + Metadata → **RRF Fusion** → Reranker → Top-K. Embedding must record model id,
version, dimensions, normalization, and generation time so models can be swapped and indexes
rebuilt safely.

See the canonical spec: [RFC-001](rfc/rfc-001-searchos.md).
