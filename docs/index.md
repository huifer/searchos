# SearchOS Documentation

> Documentation Hub

Welcome to the SearchOS documentation. SearchOS is a Rust-native, extensible AI Search
Runtime with two runtime modes (Local + Gateway), Tavily-compatible API, search memory, and a
fault-isolated extension system.

The canonical specification is the **RFC Suite v1.0** under [`docs/rfc/`](rfc/).

## RFC Suite (v1.0 — Engineering Baseline)

> Index, governance, and core design principles: [`docs/rfc/README.md`](rfc/README.md)

- [RFC-001: Product Architecture & System Boundaries](rfc/rfc-001-product-architecture.md)
- [RFC-002: Core Domain & Rust Workspace](rfc/rfc-002-core-domain-workspace.md)
- [RFC-003: Search Request & Query Intelligence](rfc/rfc-003-search-request-query-intelligence.md)
- [RFC-004: Search Decision Engine](rfc/rfc-004-search-decision-engine.md)
- [RFC-005: Quality Gate & Evidence Evaluation](rfc/rfc-005-quality-gate-evidence-evaluation.md)
- [RFC-006: Knowledge Store & Document Lifecycle](rfc/rfc-006-knowledge-store-document-lifecycle.md)
- [RFC-007: Retrieval & Cache Architecture](rfc/rfc-007-retrieval-cache-architecture.md)
- [RFC-008: Cost Accounting & Cost Model](rfc/rfc-008-cost-accounting-cost-model.md)
- [RFC-009: Execution Planner & Provider Router](rfc/rfc-009-execution-planner-provider-router.md)
- [RFC-010: Web Fetch & Crawler Extension](rfc/rfc-010-web-fetch-crawler-extension.md)
- [RFC-011: Extension System](rfc/rfc-011-extension-system.md)
- [RFC-012: Local Runtime & Gateway Runtime](rfc/rfc-012-local-runtime-gateway-runtime.md)
- [RFC-013: Memory & Cross-Agent Reuse](rfc/rfc-013-memory-cross-agent-reuse.md)
- [RFC-014: Security & Reliability](rfc/rfc-014-security-reliability.md)
- [RFC-015: Observability & Evaluation](rfc/rfc-015-observability-evaluation.md)
- [RFC-016: Deployment, Release & Engineering Roadmap](rfc/rfc-016-deployment-release-roadmap.md)

## RFC Annexes

- [Part 17: Database Schema Baseline](rfc/part-17-database-schema-baseline.md)
- [Part 18: CLI Command Contract](rfc/part-18-cli-command-contract.md)
- [Part 19: ADR Decisions](rfc/part-19-adr-decisions.md)
- [Part 20: Definition of Done](rfc/part-20-definition-of-done.md)
- [Part 21: GitHub Engineering Execution](rfc/part-21-github-engineering-execution.md)
- [Part 22: Final Architecture Decisions](rfc/part-22-final-architecture-decisions.md)
- [Part 23: Search Decision Engine — Engineering Baseline](rfc/part-23-decision-engineering-baseline.md)
- [Part 24: Product API, Security, Reliability & Governance](rfc/part-24-api-governance-baseline.md)
- [RFC-026: Visualization & Analytics Architecture](rfc/rfc-026-visualization-analytics-architecture.md)
- [RFC-027: Analytics Data Model & Aggregation](rfc/rfc-027-analytics-data-model.md)
- [RFC-028: Dashboard API & Frontend Integration](rfc/rfc-028-dashboard-api-frontend.md)
- [RFC-029: Local Data Ingestion & Local-First Retrieval](rfc/rfc-029-local-data-ingestion.md)
- [Part 30: Search Execution Correctness Baseline](rfc/part-30-execution-correctness-baseline.md) — 执行侧正确性不变量（INV-30-01…21）与验收测试映射（已实现） |

## Gap Analysis

- [Feature Gap Analysis: source conversation vs. implementation](gap-analysis.md) — 源对话功能遗漏分析与补强清单。
- [RFC Implementation Gap Register](rfc-implementation-gaps.md) — RFC / Part 30 缺口登记（已全部修复，含验收测试映射）。

## Guides & References

- [Architecture](architecture.md) — layered architecture, dual runtime, storage, providers.
- [Visual Console](ui.md) — Next.js + Tailwind + shadcn/ui operator console over the Analytics Read Model.
- [Extension & Crawler System](extensions.md) — versioned extension registry, manifest discovery/quarantine, native providers/crawlers, and the declarative/WASM roadmap.
- [Roadmap & ADRs](roadmap.md) — phased plan, version strategy, architecture decisions.
- [ADR Index](adr/index.md) — all Architecture Decision Records.

## Start here

1. Read [README.md](../README.md) (or [README.zh.md](../README.zh.md)) for a one-page overview.
2. Read the [RFC Suite index](rfc/README.md) for governance, scope, and design principles.
3. Use [RFC-001](rfc/rfc-001-product-architecture.md) for the full product/architecture design.
4. Use [Architecture](architecture.md), [Extensions](extensions.md), and [Roadmap](roadmap.md) as the engineering reference. `docs/roadmap.md` is a historical plan: the shipped v0.1.0 status and the explicit non-goals live in [`docs/feature-roadmap.md`](feature-roadmap.md), the release notes in [`CHANGELOG.md`](../CHANGELOG.md), and the canonical design in [`docs/rfc/`](rfc/README.md).

# Implementation & API

- [API Reference (HTTP gateway)](api/http.md)
- [CLI Reference](api/cli.md)
- [Visual Console (Next.js)](ui.md) — browser UI under `web/`
- [Feature Gap Analysis](gap-analysis.md)
- [RFC Implementation Gap Register](rfc-implementation-gaps.md)
- [Development Guide](development/README.md)

> The RFC suite describes the *spec*; the `crates/` workspace implements it.
> Docs are synced with every release (Definition of Done item 10).
