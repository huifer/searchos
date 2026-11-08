# Roadmap & Architecture Decisions

> Roadmap & ADRs

## Phased Roadmap (~16 weeks to v1.0)

| Phase | Scope | Acceptance |
|------:|-------|------------|
| P0 | Architecture foundation: workspace, core traits, dual runtime | CLI & Gateway call the same core; no HTTP loopback |
| P1 | Dual runtime + Tavily-compatible gateway | Local Mode HTTP-free; Tavily clients work; shared data model |
| P2 | Document & crawler foundation | Persistent content; per-site extractors; isolated crawler failures |
| P3 | Local search & memory | Offline retrieval; memory survives restart; model-version rebuild |
| P4 | Extension system | Add site crawler without touching core; disable/rollback; quarantine |
| P5 | AI search intelligence | Similar-query reuse; no stale reuse; explainable routing; budget caps |
| P6 | Agent integration | MCP stdio, Rust SDK, Codex/Claude Code/Pi integration |
| P7 | Production & release | Security/perf tests, backup/restore, Docker, v1.0 GA |

## Version Strategy

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

## Non-goals (v1.0)

No self-built global web index, LLM, vector DB, or browser engine; no general autonomous-agent
platform; no full distributed database; no geo-strong-consistent vector retrieval; no universal
crawler for all sites; no unlimited anti-bot bypass; no comprehensive multi-language FFI.

## Architecture Decision Records

All ADRs are tracked in [adr/index.md](adr/index.md). Highlights: dual runtime (ADR-003),
in-process Local Mode (ADR-004), Tavily-compatible API (ADR-006), extension-first (ADR-010),
site-specific crawler directory (ADR-011), crawler failure isolation (ADR-013), Memory/Cache
separation (ADR-014), and Open Core (ADR-020).

## Related RFCs

- [RFC-002: Core Crate Specification](rfc/archive/rfc-002-core-crate-specification.md) — workspace, crates, traits.
- [RFC-003: Extension System](rfc/archive/rfc-003-extension-system.md) — extension protocol & crawler isolation.
- [RFC-004: Storage & Memory](rfc/archive/rfc-004-storage-and-memory.md) — schema, indexing, memory lifecycle.
- [RFC-005: Execution Plan](rfc/archive/rfc-005-execution-plan.md) — issues, dependency order, acceptance gates.
