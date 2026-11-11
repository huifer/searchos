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

### Added

- **GHCR aliases workflow**: a `workflow_dispatch` workflow
  (`ghcr-aliases.yml`) that copies an already published multi-arch manifest
  to the documented simple names with `docker buildx imagetools create` — no
  rebuild, seconds instead of hours.

### Fixed

- **Image naming / tag mismatch (real user-facing bug)**: `release.yml` derived
  names from `${GITHUB_REPOSITORY}`, publishing
  `ghcr.io/huifer/searchos/gateway:0.1.1`, while the README and runbook told
  users to pull `ghcr.io/huifer/searchos-gateway:v0.1.1` — both the name and
  the leading `v` were wrong. The workflow now publishes
  `ghcr.io/<owner>/searchos-{gateway,console}:<version>`, `docker-compose.yml`
  and the build examples use those exact names/tags, and the already published
  `0.1.1` manifests are copied to the new names by `ghcr-aliases`.
- The architecture diagrams now mark the browser engine as a later-phase
  target (a v1 non-goal) instead of listing it as shipped infrastructure.
- `sdk/typescript/README.md`, `sdk/python/README.md` and `tests/README.md`
  gained 中文 sections, completing the bilingual convention for user-facing
  documentation.
- Corrected the `[0.1.1]` date to the actual publish date.

## [0.1.1] - 2026-11-11

Post-release corrections: container images are published, the remaining
engineering reference docs are bilingual, and the repository metadata is
consistent.

### Added

- **Container images**: the release workflow now logs in to GHCR with the
  workflow's own `GITHUB_TOKEN` — the workflow already declares
  `packages: write`, so no extra repository secret is needed — and pushes
  multi-arch `linux/amd64` + `linux/arm64` images for the gateway and the
  console, tagged with the release version. Each architecture is built
  natively on its own runner (`ubuntu-latest` and `ubuntu-24.04-arm`) and a
  manifest list joins them — building arm64 under QEMU took hours and risked
  the job timeout. A freshly pushed package is private by default; the
  runbook explains how to make it public.
- **Bilingual engineering docs**: `docs/architecture.md`, `docs/extensions.md`,
  `docs/roadmap.md`, `docs/adr/index.md`, `docs/api/cli.md`, `docs/api/http.md`
  and `docs/development/README.md` gained 中文 sections, completing the
  English + 中文 convention across the documentation set.

### Fixed

- Corrected the only broken local documentation link (`docs/api/http.md` now
  points at `../mcp.md`).
- Aligned the workspace `repository` URL and every crawler / provider
  User-Agent contact URL with the real remote (`github.com/huifer/searchos`);
  the Python SDK already used it.
- Added the Keep a Changelog link references and a `repository` field to the
  TypeScript SDK manifest.
- Bumped the workspace version to 0.1.1, so the published binary, the OpenAPI
  document, the SDK manifests and the image tags all carry the same number.
- The v0.1.0 notes claimed the workflow builds multi-arch container images. It
  built the gateway and console images for verification only — no registry
  secret was configured, so nothing was published. The claim is corrected here
  and the workflow now really publishes them.

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
- **CI could not build on GitHub runners**: they do not ship `protoc`, so every
  Rust job (clippy / test / docs / openapi / eval-gate / backup-drill) failed in
  `lance-encoding`'s build script. `.github/actions/setup-protoc` now installs
  `protobuf-compiler` **and** `libprotobuf-dev` (the well-known `.proto` includes
  are only a Recommends of the former) on every Rust job, including the release
  `verify` and `artifacts` jobs.
- **OTLP export test was environment-dependent**: `logging::init_with_otlp`
  applies `EnvFilter::try_from_default_env()` and CI sets `RUST_LOG=warn`, which
  filtered out the test's info-level span so `/v1/traces` never arrived. The
  smoke span and event are now ERROR-level; the test passes with `RUST_LOG`
  unset, `warn` and `error`.
- **release workflow rejected at parse time**: the `secrets` context is not
  allowed in a step-level `if:`. The `images` job now exposes the registry token
  as a job-level env var and uses `if: env.REGISTRY_TOKEN != ''` plus
  `$REGISTRY_TOKEN` in shell steps. Both workflows pass `actionlint`, now
  enforced by the `workflow-lint` CI job.

[Unreleased]: https://github.com/huifer/searchos/compare/v0.1.1...HEAD
[0.1.1]: https://github.com/huifer/searchos/releases/tag/v0.1.1
[0.1.0]: https://github.com/huifer/searchos/releases/tag/v0.1.0
