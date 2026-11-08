# SearchOS

> The Search & Memory Infrastructure for AI Agents.
> One search engine. Two runtime modes. Shared intelligence.

## Why SearchOS

AI-agent workflows depend heavily on Web Search, but today every agent, every tool, and
every task tends to use its own search service (Tavily, Brave, Exa…). That produces:

- The same or near-duplicate searches executed over and over.
- Search quotas that can't be managed or shared across tools.
- Fetched web content that is never reused.
- Search results with no persistence or accumulated knowledge.
- Different websites that a generic crawler can't reliably handle.
- Two different usage models for local use vs. server deployment.

SearchOS fixes this with one unifying infrastructure.

## Highlights

- **Dual Runtime** — the same Rust core runs as an in-process **Local Mode** (CLI / Rust
  SDK / MCP stdio) and as an HTTP **Gateway Mode** (Tavily-compatible REST + MCP).
- **Tavily-compatible API** — point your existing client at `http://127.0.0.1:8787` and
  keep your workflow unchanged.
- **Local-first search** — answers are served from local index/memory before any external
  request is made.
- **Search Memory** — search history, document versions, semantic memory, and research
  memory are first-class, durable assets.
- **Hybrid retrieval** — BM25 + Vector + Metadata fused with Reciprocal Rank Fusion (RRF)
  and a pluggable reranker.
- **Extension-first** — providers, site-specific crawlers, extractors, embeddings, and
  rerankers are all loaded through a unified, fault-isolated extension system.
- **Cost-aware routing** — a scoring model that balances quality, freshness, latency, cost,
  and availability; plus Singleflight request coalescing.
- **Distributed-ready** — the data model and interfaces are designed for multi-node from
  day one, without forcing distributed complexity in v1.

## Architecture

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

See [`docs/architecture.md`](docs/architecture.md) and the canonical design in
[`docs/rfc/rfc-001-searchos.md`](docs/rfc/rfc-001-searchos.md).

## Roadmap

A phased, deliverable-driven plan (~16 weeks to v1.0). Full detail in
[`docs/roadmap.md`](docs/roadmap.md).

| Phase | Scope |
|------:|-------|
| P0 | Architecture foundation: workspace, core traits, dual runtime |
| P1 | Dual runtime + Tavily-compatible search gateway |
| P2 | Document & crawler foundation |
| P3 | Local search & memory |
| P4 | Extension system |
| P5 | AI search intelligence |
| P6 | Agent integration (MCP, SDK) |
| P7 | Production & release (v1.0) |

## Documentation

- [Architecture](docs/architecture.md)
- RFC set:
  - [RFC-001: SearchOS](docs/rfc/rfc-001-searchos.md)
  - [RFC-002: Core Crate Specification](docs/rfc/rfc-002-core-crate-specification.md)
  - [RFC-003: Extension System](docs/rfc/rfc-003-extension-system.md)
  - [RFC-004: Storage & Memory](docs/rfc/rfc-004-storage-and-memory.md)
  - [RFC-005: Execution Plan](docs/rfc/rfc-005-execution-plan.md)
- [Extension & Crawler System](docs/extensions.md)
- [Roadmap & ADRs](docs/roadmap.md)
- [Docs hub](docs/index.md)
- [Chinese README](README.zh.md)

## License

See [LICENSE](LICENSE). (Open Core: the community edition is open source; hosted/enterprise
capabilities are offered separately.)
