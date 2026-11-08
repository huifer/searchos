# RFC-004: Storage & Memory

**Engineering Specification · Version 1.0 · 2026-11-08 · Status: Proposed**

Companion to [RFC-001](rfc-001-searchos.md). This document specifies the storage engines, the
SQLite schema, indexing strategy, embedding/version model, Memory lifecycle, and the
consistency mechanism.

---

## 1. Storage Engines (single-node)

| Concern | Engine | Role |
|---|---|---|
| Metadata | SQLite | transactions, metadata, jobs, search history — **source of truth** |
| Full-text | Tantivy | BM25 keyword index |
| Vector | LanceDB | embeddings & semantic index |
| Object | Local filesystem | raw HTML / PDF / Markdown snapshots |
| Cache | SQLite + in-memory | exact & semantic cache with TTL |
| Queue | SQLite-backed job queue | background tasks (local mode) |

Local Mode must not require Redis, PostgreSQL, Kafka, NATS, or Docker. In cluster mode these
become PostgreSQL / NATS JetStream / S3-compatible object storage (ADR-017/018/019).

## 2. Data Directory

```text
~/.searchos/
├── config/      config.yaml · providers.yaml · extensions.yaml
├── data/        metadata.db · cache/ · objects/ · documents/ · snapshots/ · jobs/
├── indexes/     tantivy/ · lancedb/
├── extensions/  providers/ · crawlers/ · extractors/ · embeddings/ · rerankers/
├── models/
├── logs/
└── backups/
```

Program, data, extensions, models, and cache are separated so that upgrading the binary never
destroys user data, and cache can be cleared independently.

## 3. SQLite Schema (core tables)

| Table | Key columns |
|---|---|
| workspaces | id, name, config |
| api_keys | id, key_hash, workspace_id |
| search_queries | id, query, normalized_query, intent |
| search_runs | id, query_id, provider, cost, status |
| search_results | id, run_id, document_id, rank |
| documents | id, canonical_url, title, content_hash |
| document_versions | id, document_id, version, fetched_at |
| document_chunks | id, document_id, chunk_index |
| embeddings | id, chunk_id, model, dimensions |
| memories | id, namespace, content, metadata |
| research_projects | id, name, description |
| research_evidence | id, project_id, document_id |
| providers | id, name, enabled, config |
| provider_usage | id, provider_id, requests, cost |
| crawl_jobs | id, url, status, priority |
| cache_entries | id, cache_key, expires_at |
| feedback | id, result_id, relevance, reason |

Web pages, search results, and memory are **not** collapsed into a single JSON blob; structured
relations and provenance are preserved.

## 4. Document / Chunk Model

A document has versions; a version has chunks.

```text
Document
   ├── Version 1 ── Chunk 1 · Chunk 2 · Chunk 3
   └── Version 2 ── Chunk 1 · Chunk 2
```

This enables update detection, historical diffing, incremental embedding, partial content
refresh, and citation traceability in research reports.

## 5. Indexing & Consistency

SQLite, Tantivy, and LanceDB cannot share one transaction. Therefore:

**SQLite = source of truth + Transactional Outbox + Reconciliation Worker.**

```text
SQLite Transaction (Document + Outbox Event)
        │
        ▼
   Index Worker ──► Tantivy  (BM25)
        │        └─► LanceDB  (Vector)
        ▼
   Index Sync Status
```

- Embedding failure must never block saving the raw document or keyword search.
- A periodic reconciliation job detects and repairs index drift.
- Corrupt indexes are rebuilt from SQLite metadata + original documents.

## 6. Embedding Model Versioning

Every embedding row stores: model id, version, dimensions, normalization, generated_at.

Rules:

- Vectors from different models are never mixed in one query.
- Switching models triggers a background re-embedding job (old vectors stay readable until
  the new index is ready).
- Namespaces are per-model-version to keep retrieval deterministic.

## 7. Memory Engine

Four Memory kinds:

| Kind | Content |
|---|---|
| Search Memory | queries, providers, results, timing, cost, hit info |
| Document Memory | page bodies, markdown, PDF, titles, authors, versions |
| Semantic Memory | embedding-based similarity & association |
| Research Memory | projects, evidence sets, conclusions |

Vector namespaces: `queries`, `documents`, `chunks`, `research`, `knowledge`, `feedback`.

Required capabilities: incremental write, semantic search, full-text search, metadata filter,
TTL, content versioning, provenance, re-embedding, deletion/cleanup, workspace isolation,
research-project association.

**Memory is not a cache.** Cache returns already-fetched data fast; Memory stores long-lived,
retrievable knowledge with its own lifecycle (versions, provenance, trust, TTL).

## 8. TTL & Freshness Policy (initial defaults)

| Content type | Default TTL |
|---|---:|
| Technical / API docs | 30 days |
| GitHub project pages | 7 days |
| Product / competitor pages | 7 days |
| SEO industry research | 3 days |
| News | 1 hour |
| Real-time prices | 1 minute |
| Weather / FX | 5 minutes |

These are configurable defaults; the freshness evaluator may override per intent (e.g., a
"latest news" query must not serve stale cache even if it is technically unexpired).

## 9. Backup & Recovery

- `searchos backup create` produces a consistent snapshot (SQLite + index manifests + object
  manifest).
- `searchos backup restore` rebuilds from the snapshot; indexes can be rebuilt from SQLite +
  object store if missing.
