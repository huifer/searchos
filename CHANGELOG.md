# Changelog

All notable changes to this project are documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Version numbers are the single source of truth in `Cargo.toml` (workspace
`version`), mirrored into the binary (`searchos version`), the OpenAPI document
(`info.version`) and the Docker image tags. A release is cut by pushing a
`v<version>` tag; the `release` workflow builds the artifacts, checksums and
multi-arch images, and attaches the matching section of this file.

## [Unreleased]

Nothing yet — all shipped work is recorded under `[0.1.0]`.

## [0.1.0] - 2026-11-10

The first release candidate: a Rust-native search infrastructure that runs
in-process as a Local Mode and over HTTP as a Gateway Mode, sharing one core.

### Added

- **Dual runtime**: Local Mode (CLI / Rust library crates / MCP stdio, no HTTP loopback) and
  Gateway Mode (Axum HTTP), both backed by the same application services.
- **Search decision core**: explicit `status` on every response, a decision
  state machine with structured stage traces, gap analysis, cost-aware planning
  and a final quality gate that uses measured relevance scores.
- **Local-first retrieval**: hybrid BM25 + vector retrieval fused with
  reciprocal rank fusion, canonical/semantic/retrieval/embedding/rerank caches,
  and a similarity-gated semantic cache.
- **Knowledge lifecycle**: URL extraction, local text/file ingestion (RFC-029),
  document versioning, a transactional outbox, and rebuildable full-text indexes
  with selectable remote backends (Elasticsearch, Meilisearch, PostgreSQL, MySQL).
- **Search memory**: namespaced, embedding + BM25 retrieval with reciprocal rank
  fusion and namespace isolation.
- **Cost ledger**: append-only events, reserve → execute → settle budget
  reservations, provider bill reconciliation, savings and reuse-cost views.
- **Operator console**: a Next.js 16 / React 19 console over the Analytics Read
  Model (RFC-026/027/028) with documents, memory, traces, cost and dashboards.
- **Observability**: Prometheus text exposition on `GET /metrics`, structured
  traces, and every Part 30 execution invariant exported as a counter or gauge.
- **Contracts**: a frozen Tavily compatibility subset, an OpenAPI 3.1 document
  verified against the route table, MCP stdio-only transport (ADR-031), and
  generated TypeScript and Python SDKs.
- **Correctness baseline**: all Part 30 invariants implemented with named
  acceptance tests, covering idempotency, budget settlement, workspace isolation,
  SSRF pinning, deletion, migration and cache identity.
- **Operations**: graceful shutdown on `SIGTERM`, a consistent backup format with
  checksum verification, and evaluation baselines for quality regression testing.
- **Deployment**: multi-stage `Dockerfile` (gateway + console), `docker-compose.yml`
  with a persistent volume and healthchecks, optional Prometheus / OTel collector
  profiles, and the `docs/deployment.md` runbook.
- **Release automation**: `.github/workflows/release.yml` builds release binaries,
  checksums, multi-arch container images and the GitHub release from this file.
- **Evaluation quality gate**: CI runs a real retrieval evaluation against a
  deterministic seed corpus and asserts keyword coverage, pass rate and p95 latency.
- **Backup/restore drill**: `tests/e2e/tests/backup_restore.rs` rehearses the full
  round trip — ingest, backup, destroy, restore, rebuild, verify — plus refusal of
  a tampered backup.
- **OpenTelemetry export (opt-in)**: traces and the same metric snapshot behind
  `/metrics` can be pushed over OTLP/HTTP; off by default, env-configured and
  flushed on graceful shutdown.
- **Bilingual console + document lifecycle**: EN/中文 UI, text/file ingest with batch
  job polling, a workspace-scoped document library, version history, re-index and
  soft delete; dashboard filters are pushed into SQL with declared provenance.
- **Frontend test suite**: 51 Vitest tests (client, proxy, analytics, ingest,
  library, i18n) with a dedicated CI job.

### Fixed

- **Backup missed WAL-resident transactions**: `backup create` copied the bare
  `.db` file, so work still sitting in the write-ahead log was absent from the
  snapshot. It now uses `VACUUM INTO`, which writes a consistent snapshot of
  the whole database.
- **Restore could be silently overwritten**: the restore command initialised a
  runtime (opening an empty database) before writing the restored snapshot, and
  that connection's WAL checkpointed over the restored file on exit. Restore is
  now a config-only file operation that never opens the database.
- **Environment overrides were inert**: `SearchOsConfig::effective()` was never
  called on the real load path, so `SEARCHOS_AUTH_TOKEN` and the other overrides
  had no effect; all overrides now go through `apply_env_overrides()`.
- **Release-pass consistency**: aligned the workspace license with `LICENSE`
  (MIT), corrected the documented MSRV to 1.94 (sqlx 0.9), fixed stale schema /
  roadmap / SDK claims in the docs, documented the local hashing embedder and the
  unattached reranker as explicit limitations, and removed a React key warning
  from the analytics test fixtures.
