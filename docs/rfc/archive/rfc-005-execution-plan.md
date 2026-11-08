# RFC-005: Execution Plan

**Engineering Specification · Version 1.0 · 2026-11-08 · Status: Proposed**

Companion to [RFC-001](rfc-001-searchos-v2.md). This document turns the architecture into an
executable plan: issue breakdown, dependency order, acceptance criteria, agent-assisted dev
workflow (Codex / Claude Code), and release strategy.

---

## 1. Principles

- Deliver by **product capability**, not by module.
- Every phase has explicit **acceptance gates** — "code written" is not "done".
- AI coding agents work on **bounded tasks with clear inputs/outputs**, never on the whole repo
  at once.

## 2. Phase Plan (~16 weeks to v1.0)

### Phase 0 — Architecture Foundation (Week 1–2)

Tasks: Cargo workspace · core Traits · Application Services · Local Runtime · Gateway Runtime ·
Search IR · Storage Trait · Extension Manifest · Error Model · ADRs.

Acceptance: CLI calls Core directly; Gateway calls the same Core; both modes return the same
result structure for one search use case; no HTTP loopback.

### Phase 1 — Dual Runtime + Search Gateway (Week 3–4)

Tasks: CLI · Tavily `/search` · Tavily `/extract` · Provider Trait · Tavily Adapter · Brave
Adapter · SQLite · Query Cache · Exact Dedup · Singleflight.

Acceptance: Local Mode starts no HTTP server; existing Tavily clients work against Gateway;
both modes share search results and the internal data model.

### Phase 2 — Document & Crawler Foundation (Week 5–6)

Tasks: HTTP Fetcher · HTML Parser · Markdown Extractor · PDF Extractor · URL canonicalization ·
Document versioning · Crawler Engine · Generic Crawler · Crawler Registry · Site Extension
manifest.

Acceptance: content persists; different sites use different extractors; one crawler failure
does not affect others; raw page snapshots are retained.

### Phase 3 — Local Search & Memory (Week 7–8)

Tasks: Tantivy · LanceDB · Chunking · Embedding Provider · BM25 · Vector Retrieval · RRF ·
Memory Namespaces · Document Memory · Search Memory.

Acceptance: retrieval works offline on indexed documents; Memory survives restart; model
version change triggers re-index.

### Phase 4 — Extension System (Week 9–10)

Tasks: Extension Registry · Lifecycle · Crawler/Provider/Extractor Extensions · YAML Rule
Engine · Native Extension Interface · Validation · Health Check · Quarantine · Debug CLI.

Acceptance: adding a site crawler requires no Core change; an extension can be disabled,
upgraded, and rolled back; a broken site auto-degrades to the generic crawler; untrusted
extensions never get full host permissions.

### Phase 5 — AI Search Intelligence (Week 11–12)

Tasks: Query Understanding · Semantic Dedup · Search Planner · Incremental Search · Query
Expansion · Reranker · Freshness Evaluation · Cost-aware Router · Search Feedback.

Acceptance: similar queries reuse existing knowledge; real-time queries never serve stale
cache; a search's routing decision is explainable; budgets cap AI-driven extra calls.

### Phase 6 — Agent Integration (Week 13–14)

Tasks: MCP stdio · Tavily Compatibility Suite · Codex / Claude Code / Pi integration · Rust
SDK · installer & upgrader.

Acceptance: local agents work via CLI or MCP stdio; remote agents via HTTP Gateway; both modes
share the same Memory and search logic.

### Phase 7 — Production & Release (Week 15–16)

Tasks: security tests · performance tests · Backup/Restore · Docker · multi-process workers ·
PostgreSQL / NATS / S3 adapters · Dashboard · documentation · GitHub Release.

Acceptance: reproducible deployment; data recoverable; multi-node architecture validated at a
basic level; core features covered by automated tests; formal v1.0 release.

## 3. Issue Breakdown (suggested GitHub issues)

| Area | Example issues |
|---|---|
| Foundation | `chore: init workspace`, `feat(core): Search IR`, `feat(core): error model` |
| Gateway | `feat(gateway): axum server`, `feat(api-compat): /search`, `feat(api-compat): /extract` |
| Providers | `feat(providers): tavily adapter`, `feat(providers): brave adapter`, `feat(providers): singleflight` |
| Crawler | `feat(crawler): fetcher`, `feat(crawler): canonicalization`, `feat(crawler): registry` |
| Index/Memory | `feat(indexing): tantivy`, `feat(indexing): lancedb`, `feat(memory): namespaces` |
| Extensions | `feat(ext): manifest`, `feat(ext): yaml rule engine`, `feat(ext): quarantine` |
| Intelligence | `feat(intel): planner`, `feat(intel): freshness`, `feat(intel): reranker` |
| Ops | `feat(cli): doctor`, `feat(ops): backup/restore`, `chore(ci): clippy+tests` |

Dependency order: Foundation → Gateway → Providers → Crawler → Index/Memory → Extensions →
Intelligence → Agent Integration → Release.

## 4. Quality Evaluation

Build a fixed Evaluation Dataset early (target 300–500 questions) covering technical docs,
GitHub projects, news, product comparison, pricing, and Chinese/English/multilingual queries.
For each item store: query, expected relevant documents, reference sources, freshness
requirement, and results from keyword / vector / hybrid / external provider retrieval.

Track Recall@K, NDCG@K, MRR, latency, and cost. The core question the dataset must answer:

> Does Local Search reduce external requests **without** lowering search quality?

## 5. Version Strategy

| Version | Capability |
|--------|-----------|
| v0.1 | Rust Core + CLI + Gateway |
| v0.2 | Tavily API + Provider Router |
| v0.3 | Generic Crawler + Site Extensions |
| v0.4 | Tantivy + LanceDB + Memory |
| v0.5 | Semantic Search + Search Planner |
| v0.6 | Extension Lifecycle + MCP |
| v0.8 | Reliability + Observability |
| v1.0 | Dual-mode GA |
| v1.1 | Advanced crawler extensions |
| v1.2 | Multi-process workers |
| v1.5 | Distributed cluster |
| v2.0 | Multi-tenant + hosted search |

## 6. Release & Launch

- Publish on GitHub with RFCs, benchmarks, and a reproducible demo.
- Flagship demo: **10 agents search the same query → 1 external request, 9 reuses**, with a
  visible cost delta.
- Channels: GitHub, Hacker News (architecture write-up), Reddit (AI Coding / self-hosted), X,
  Product Hunt, YouTube, Dev.to, V2EX.
- Open Core: the community edition keeps full core search/memory/vector/extension/CLI/MCP;
  hosted & enterprise capabilities (multi-tenant, SSO/RBAC, cluster management, central cost
  governance) are separate. Core search is never artificially crippled.
