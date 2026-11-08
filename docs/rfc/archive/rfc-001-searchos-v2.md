# RFC-001: SearchOS — Rust-native AI Search Infrastructure

**Architecture Specification · Version 2.0 · 2026-11-08 · Status: Proposed**

| Attribute | Value |
|---|---|
| Project | SearchOS (tentative) |
| Type | Rust-native AI Search Infrastructure |
| Status | Proposed |
| Language | Rust |
| Runtime | Local Mode + Gateway Mode |
| External protocol | Tavily-compatible API |
| Local interface | CLI + Rust Library + MCP stdio |
| Deployment | Standalone / Server / Distributed Cluster |
| Strategy | Local-first · Extensible · AI-native |
| Goals | Search reuse · Knowledge accumulation · Intelligent retrieval · Cost optimization |

---

## 1. Executive Summary

### 1.1 Background

AI-agent workflows depend heavily on Web Search. Different agents and tasks typically use
different search tools (Tavily, Brave, Exa…). This creates several problems:

1. Identical or near-duplicate searches are executed repeatedly.
2. Quotas across different search services cannot be managed uniformly.
3. Already-fetched web content is not reused effectively.
4. Search results lack persistence and accumulated knowledge.
5. Different websites have different structures that a generic crawler cannot handle reliably.
6. There is no unified knowledge-memory layer between search services and agents.
7. Local use and server deployment often require two different ways of operating.

SearchOS is designed to solve these problems. It is not a simple Search API proxy and not a
local vector database; it is a unified infrastructure that integrates search, crawling,
retrieval, memory, AI decision-making, and extensibility.

### 1.2 Definition

**SearchOS is an extensible AI Search Runtime built in Rust, providing both an in-process
local runtime and an HTTP gateway runtime.**

Through a unified Search Engine, Memory Engine, Crawler Engine, and Extension Runtime, it
provides consistent search capabilities to AI agents, developer tools, and other
applications.

### 1.3 Core Design Principles

- **Rust-native** — core business logic is written in Rust.
- **Dual Runtime** — local in-process execution and HTTP gateway share the same core.
- **Local-first** — prefer search results already accumulated locally.
- **Tavily-compatible** — externally compatible with existing Tavily workflows.
- **Provider-agnostic** — not bound to a single external search provider.
- **Extension-first** — all non-generic capabilities enter through extension interfaces.
- **Memory-native** — search data naturally flows into the knowledge-memory system.
- **Incremental** — prefer supplementing missing information over re-running a full search.
- **Fault-isolated** — a single website, provider, or extension failure must not take down core.
- **Distributed-ready** — consider multi-node from the data model, but defer distributed complexity.

## 2. Core Architecture Decision: Dual Runtime

This is the most important change in this RFC.

SearchOS exposes two independent runtime modes.

### 2.1 Mode A — Gateway Mode

**Positioning: HTTP Search Gateway.** Runs a long-lived SearchOS Server exposing an HTTP API.

Use it for: multiple AI agents sharing one search service; remote access from Codex, Claude
Code, Pi, OpenClaw; multi-device shared memory; team/server deployment; multi-node clusters.

```bash
searchos gateway start
searchos gateway start --host 0.0.0.0 --port 8787
```

```bash
curl http://127.0.0.1:8787/search \
  -H "Authorization: Bearer local-key" \
  -H "Content-Type: application/json" \
  -d '{"query":"Rust AI search engine","search_depth":"advanced","max_results":10}'
```

Gateway Mode provides: Tavily-compatible API, REST API, HTTP MCP (optional), auth & access
control, unified provider management, search cache, local knowledge base, multi-node scheduling.

### 2.2 Mode B — Local Mode

**Positioning: In-Process Local Search Runtime.** This is a true local execution mode. It is
not "start a local HTTP server and then call yourself over HTTP"; it calls SearchOS Core
directly in the current process.

```text
User / Agent
     │
     ▼
SearchOS CLI
     │
     ▼
SearchOS Application
     │
     ▼
SearchOS Core
     ├── Query Engine
     ├── Search Engine
     ├── Memory Engine
     ├── Crawler Engine
     └── Provider Router
     │
     ▼
Local Storage
```

```bash
searchos init
searchos search "Rust AI search engine"
searchos search "Tavily alternatives" --local
searchos search "Latest Rust releases" --fresh
searchos fetch https://example.com
searchos memory search "Rust search engine"
searchos stats
```

```rust
let engine = SearchEngine::open(config).await?;
let result = engine.search(SearchRequest {
    query: "Rust AI search engine".into(),
    ..Default::default()
}).await?;
```

Other Local Mode entry points: a Rust Library for direct dependency, MCP stdio for
agent communication, and CLI subcommands. MCP stdio does **not** require the Gateway.

### 2.3 Unified Dual-Mode Principles

Both modes must share: Search Core, Query Planner, Memory Engine, Crawler Engine, Provider
Registry, Extension Runtime, Retrieval Engine, Storage Abstraction. Only the Application Host
and Transport differ.

| Capability | Local Mode | Gateway Mode |
|---|---|---|
| CLI | native | admin entry |
| Rust Library | direct | internal |
| Tavily API | not needed | supported |
| REST API | not started | supported |
| MCP stdio | supported | optional |
| Local SQLite | supported | single-node |
| Local Tantivy | supported | single-node |
| Local Vector | supported | single-node |
| Multi-user | not primary | supported |
| Multi-node | not supported | supported |

**Hard constraint: no business logic may live only in an HTTP handler. All logic must be in
Core or an Application Service.**

## 3. System Architecture

### 3.1 Layered Architecture

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

### 3.2 Core Module Boundaries

| Module | Responsibility |
|---|---|
| Interface | CLI, HTTP, MCP, SDK |
| Application | use-case orchestration and entry points |
| Core | search, routing, memory, retrieval |
| Extension | site/provider/capability extensions |
| Infrastructure | database, index, file, queue |
| Runtime | local and distributed execution |

Modules communicate only through explicit Traits and data contracts. Crates must not reach
into each other's internals.

## 4. Extension System

Extensions are a first-class architectural capability, not a late-stage plugin add-on.

### 4.1 Extension Types

| Type | Purpose |
|---|---|
| Provider Extension | external search provider |
| Site Crawler Extension | site-specific crawler |
| Extractor Extension | content extraction |
| Search Extension | recall strategy |
| Embedding Extension | vector generation |
| Reranker Extension | result reranking |
| Memory Extension | memory storage & retrieval |
| Intelligence Extension | AI decision & planning |

### 4.2 Extension Registry

All extensions register through a unified registry: Provider, Crawler, Extractor, Embedding,
Reranker, Memory, Intelligence. Each extension must declare: ID, Name, Version, Type,
Compatibility Version, Dependencies, Capabilities, Configuration Schema, Permissions, Health
Check, Lifecycle, Failure Policy.

### 4.3 Implementation Levels

- **Level 1 — Declarative Extension**: YAML/JSON rules (URL match, page-type detection, CSS
  selector, XPath, pagination, fields, headers, cleaning). Highest priority to support.
- **Level 2 — Native Extension**: complex logic in Rust via a unified Trait. Official
  extensions may ship compiled with the binary to avoid ABI issues.
- **Level 3 — WASM Extension**: third-party extensions via WASM/WASI with explicit
  network/file/resource permissions. Default must not grant full host access.

## 5. Site Crawler Extension

### 5.1 Directory Layout

```text
extensions/crawlers/
├── generic/        manifest.yaml · rules.yaml · tests/
├── github.com/     manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
├── docs.rs/        manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
├── example.com/    manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
└── custom/
```

Directory names should use a normalized Domain ID and support domain aliases.

### 5.2 Manifests

`manifest.yaml` declares id, name, version, type, match domains, capabilities, runtime,
permissions, healthcheck. `crawler.yaml` declares fetch strategy, URL include/exclude,
content selectors, limits. `extractors.yaml` declares per page-type field extraction. All
rules must be validated against real HTML fixtures — never assume a selector stays valid.

### 5.3 Crawler Selection

```text
Incoming URL → Normalize URL → Extract Domain → Crawler Registry
  → Exact Domain Match → Domain Alias Match → Generic Crawler → Execution
  → Validation → Content Extraction
```

Priority: exact extension → declared alias extension → generic crawler → fallback. Crawlers
must not handle one URL competitively and out of order.

### 5.4 Failure Management

Each site crawler has its own health state: Healthy, Degraded, Unstable, Quarantined,
Disabled, Updating. Support consecutive-failure thresholds, success-rate thresholds,
circuit breaking, automatic retry, fallback crawler, generic-crawler degradation, manual
recovery, and failure-cause logging. Failures are classified (NETWORK_ERROR, DNS_ERROR,
TIMEOUT, HTTP_403/404/429/5XX, ROBOTS_DISALLOWED, SELECTOR_NOT_FOUND, CONTENT_EMPTY,
PARSER_ERROR, BROWSER_RENDER_ERROR, RATE_LIMITED) with per-class handling.

### 5.5 Crawler Debugging

```bash
searchos crawler inspect github.com
searchos crawler test https://github.com/owner/repo
searchos crawler status
searchos crawler failures
searchos crawler preview https://example.com
searchos extension validate
searchos extension reload crawler.github
```

Test results should retain raw HTML, final DOM, extracted Markdown, matched rules, failure
reason, latency, and rule version.

## 6. Extension Lifecycle

Discovered → Manifest Validation → Compatibility Check → Permission Validation → Dependency
Resolution → Initialization → Health Check → Registered → Active → (Failure → Degraded →
Quarantined) → Disabled/Unloaded. Support versioning, dependencies, config validation,
permissions, isolation, hot-reload (safe types only), rollback, health, logs, metrics.
Hot updates on in-flight tasks use version snapshots: new tasks use the new version; old
tasks finish or time out before the old version is released.

## 7. Search Engine

### 7.1 Pipeline

```text
Search Request → Query Normalization → Intent Classification → Exact Cache → Semantic Cache
  → Local Retrieval → Coverage Evaluation → (sufficient? return)
  → Search Planner → Provider Router → External Search → Result Normalization
  → Deduplication → Reranking → Persistence → Response
```

### 7.2 Search Types

Exact, Semantic, Hybrid, Fresh, Incremental, Research, Local, Federated.

### 7.3 Search Planner

Decides whether to use cache, vector search, fetching, external provider, query expansion,
reranking, and answer generation. It is budget-bounded: `max_expansions`,
`max_external_queries`, `max_fetch_pages`, `max_retries`, `max_execution_time_ms`. No AI
planning may bypass the budget.

## 8. Memory Engine

Four kinds: Search Memory (history/queries/results), Document Memory (pages/versions),
Semantic Memory (embeddings/associations), Research Memory (projects/evidence/conclusions).
Retrieval: BM25 + Vector + Metadata → RRF Fusion → Reranker → Top-K. Memory must support
incremental write, semantic & full-text search, metadata filter, TTL, content versioning,
provenance, re-embedding, deletion/cleanup, workspace isolation, and research-project
association. **Memory is not a cache**: cache returns already-fetched data fast; memory
stores long-lived, retrievable knowledge with its own lifecycle.

## 9. Storage Architecture

**Single-node** components: Metadata = SQLite; Full-text = Tantivy; Vector = LanceDB; Object =
local filesystem; Cache = SQLite + memory; Queue = SQLite-backed job queue. Local Mode must
not require Redis, PostgreSQL, Kafka, NATS, or Docker. Data directory default `~/.searchos/`
separates program, data, extensions, models, and cache. SQLite is the source of truth for
metadata; Tantivy/LanceDB are rebuildable indexes, synced via a Transactional Outbox +
Index Worker + Reconciliation, so an index failure never blocks document persistence.

## 10. Provider Architecture

All external services implement a unified `SearchProvider` trait, exposing `id`,
`health_check`, `search`, and `capabilities`. First batch: Tavily, Brave, Exa, SearXNG,
Custom HTTP. The Provider Router scores by quality, cost, latency, availability, freshness,
and task type. It must support key management, quota/cost accounting, budget control,
concurrency limits, failover, dedup, Singleflight, and health status. HTTP handlers only do
validation, auth, DTO conversion, service call, and response conversion — never search,
cache, memory, or crawler logic.

## 11. API Compatibility

Gateway Mode must be compatible with common Tavily workflows: Phase 1 `POST /search` +
`POST /extract`; Phase 2 `POST /crawl`, `/map`, `/research`. Internal Search IR is canonical;
external uses a Tavily DTO Adapter. A protocol-compatibility test suite must verify real
call behavior, not just field-name equality.

## 12. Distributed Architecture

Distributed capability is enabled only in Gateway Mode; Local Mode stays single-node.
Cluster: Load Balancer → Gateways → Coordinator → Message Broker → Search Node / Crawl Worker
/ Embedding Worker → Metadata DB → Object Storage. Each Search Node owns its index shard;
documents live in object storage; metadata is coordinated centrally; indexes propagate via
events; queries are fanned out and fused by the Coordinator. Hard limits: never let multiple
nodes write the same SQLite data dir, the same Tantivy index, or use a single-node LanceDB
dir as a distributed database.

## 13. Security & Reliability

- **SSRF Protection**: block localhost/private IP/cloud metadata, validate DNS, re-validate
  every redirect, limit response size, hops, time, and per-domain concurrency.
- **Extension Isolation**: per-extension permissions and budgets; extension failure must not
  crash Core; WASM resource limits; only local/signed native extensions; no direct DB access.
- **Reliability**: timeout, retry, backoff, circuit breaker, fallback, health check, job
  recovery, dead-letter queue, graceful shutdown, backup & restore.
- **Data security**: secure API-key storage, no plaintext keys, workspace isolation,
  cascading deletion of document/chunk/embedding, web content treated as untrusted input,
  prompt-injection protection, and compliance with access/rules/licensing/privacy.

## 14. CLI Specification

```bash
searchos init
searchos start / searchos gateway start
searchos search "Rust AI search engine" [--local|--fresh]
searchos fetch https://example.com
searchos crawl https://example.com
searchos memory search "AI search" / searchos memory stats
searchos provider list / searchos provider add tavily
searchos crawler list / inspect / test
searchos extension list / validate / install ./my-extension
searchos index status / rebuild
searchos stats / stats cost
searchos backup create / restore
searchos doctor
```

CLI must support JSON output, Markdown output, human-readable output, exit codes, shell
completion, non-interactive mode, config overrides, and separated stdout/stderr.

## 15. Observability & Search Quality

Every request produces a Trace (normalization, cache, semantic retrieval, provider routing,
external search, crawler, embedding, reranking, persistence). Key metrics include cache-hit
rate, local-resolution rate, external-search rate, duplicate avoidance, P95 latency, provider
failure rate, crawler success rate, extension health, embedding throughput, Recall@K, NDCG@K,
and cost per query. A fixed Evaluation Dataset (300–500 questions across technical docs,
GitHub, news, comparison, pricing, Chinese/English/multilingual) is mandatory; cache-hit rate
must never be the sole success criterion.

## 16. Development Roadmap

Eight phases (~16 weeks to v1.0). P0 foundation (workspace, core traits, dual runtime); P1
dual runtime + Tavily gateway; P2 document & crawler foundation; P3 local search & memory;
P4 extension system; P5 AI search intelligence; P6 agent integration (MCP, SDK); P7
production & release. Each phase has explicit acceptance gates. The 16-week figure is an
engineering target, not a fixed launch commitment. Full issue breakdown and dependency order:
[RFC-005](rfc-005-execution-plan.md).

## 17. Version Strategy

v0.1 Rust Core + CLI + Gateway → v0.2 Tavily API + Provider Router → v0.3 Generic Crawler +
Site Extensions → v0.4 Tantivy + LanceDB + Memory → v0.5 Semantic Search + Planner →
v0.6 Extension Lifecycle + MCP → v0.8 Reliability + Observability → v1.0 dual-mode GA →
v1.1 advanced crawler extensions → v1.2 multi-process workers → v1.5 distributed cluster →
v2.0 multi-tenant + hosted search.

## 18. Growth Strategy

Positioning: *The Search & Memory Infrastructure for AI Agents.* First users: AI Coding /
Codex / Claude Code / Pi / OpenClaw users, self-hosted developers, AI-infra developers. Key
demos: (A) dual-mode parity, (B) 10 agents, 1 external search, 9 reuses, (C) crawler
extension isolate-and-degrade. Channels: GitHub, Hacker News, Reddit, X, Product Hunt,
YouTube, Dev.to, V2EX. Open Core: community edition keeps full core search/memory/vector/
extension/CLI/MCP; enterprise/hosted adds multi-tenant, SSO, RBAC, cluster, remote memory,
central cost governance, managed crawl, advanced monitoring, SLA. Core search must not be
artificially crippled for commercial value.

## 19. Non-goals (v1.0)

No self-built global web index; no self-built LLM; no self-built vector DB; no self-built
browser engine; no general autonomous-agent platform; no full distributed database; no
strongly-consistent geo-distributed vector retrieval; no universal crawler for all sites; no
unlimited anti-bot bypass; no comprehensive multi-language FFI. The project must stay focused
on Search Infrastructure.

## 20. Acceptance Criteria (v1.0)

Covers runtime modes, search, memory, extension, reliability, and agent integration (
Local Mode standalone and HTTP-free; Gateway HTTP API; shared core; Tavily `/search` &
`/extract`; pluggable providers; exact cache; Singleflight; semantic dedup; incremental
search; SQLite/Tantivy/LanceDB; document/search/research memory; rebuildable index; extension
registry & lifecycle; failure isolation & quarantine; SSRF; retry/backoff; circuit breaker;
backup/restore; graceful shutdown; extension permissions; cost tracking; quality evaluation;
CLI; Rust SDK; MCP stdio; HTTP API; Codex/Claude Code/Pi integration).

## 21. Final Architecture Decisions (ADRs)

ADR-001 Workspace · ADR-002 Core/Transport decoupling · ADR-003 Dual Runtime · ADR-004
Local In-Process Execution · ADR-005 Axum gateway · ADR-006 Tavily-compatible API · ADR-007
SQLite metadata · ADR-008 Tantivy full-text · ADR-009 LanceDB vector · ADR-010
Extension-first · ADR-011 Site-specific crawler directory · ADR-012 Declarative+Native+WASM
extensions · ADR-013 Crawler failure isolation · ADR-014 Memory/Cache separation · ADR-015
Search IR · ADR-016 Transactional Outbox · ADR-017 PostgreSQL cluster metadata · ADR-018
NATS JetStream queue · ADR-019 S3-compatible object storage · ADR-020 Open Core.

## 22. Conclusion

SearchOS is not a search API and not a vector database. It is a unified search infrastructure
that runs in-process locally and as a server gateway. The three long-term principles: (1) dual
runtime shares one core; (2) site-specific capability is managed through extensions; (3)
search memory is a long-term accumulated asset. **Final goal: make SearchOS the unified search
runtime for AI agents, so search is no longer a one-off API call but infrastructure that
accumulates, reuses, extends, and evolves.**
