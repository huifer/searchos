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

- **Graded relevance labels, live remote-provider integration, and durable
  agent sessions.** Evaluation qrels now support 0..3 graded labels
  (`relevance_grades`): nDCG uses `2^grade - 1`, while MRR/Recall count only
  grade >= 2 as a correct answer, so a cross-encoder that surfaces a merely
  related page ahead of the canonical one is measurable as a ranking defect.
  A live HTTP integration test wires a real OpenAI-compatible mock server into
  the assembled runtime and proves the remote embedding provider and remote
  cross-encoder reranker are actually reached (vectors land in LanceDB with
  the model's dimension). Agent sessions gained migration v7
  (`agent_sessions`) and a SQLite persistence port, so the follow-up contract
  (turn history + open requirements) survives a gateway/CLI restart; the
  in-memory store remains the fast read path.

- **Bounded iterative retrieval (search-agent loop).** When the final quality
  gate still has open requirements, the pipeline re-plans reformulated queries
  and acquires a second round (`SearchBudget.max_acquisition_rounds`, default
  2). Query and cost caps are cumulative across rounds, no query is re-bought,
  a provider failure never triggers re-buying, and singleflight/idempotency are
  unchanged. Exposed over HTTP (`max_acquisition_rounds`), MCP and CLI.
- **Real remote embedding provider.** `RemoteEmbeddingProvider` implements the
  OpenAI-compatible `/v1/embeddings` contract (batching, index-order
  restoration, dimension validation, API key from the environment only). The
  runtime's single embedder is shared by the vector index, memory retrieval
  and the semantic cache; LanceDB's Arrow schema now follows the model's
  dimension instead of a hard-coded 256.
- **Reranker implemented and wired.** `FeatureReranker` (offline, title/body/
  phrase/freshness features) is attached by default and billed as a
  `Reranking` cost event; `RemoteReranker` supports cross-encoder endpoints.
  `NeverReranker` is no longer the runtime path.
- **Field-weighted BM25 ranking, typo tolerance, highlight.** Title terms get
  an explicit boost (aligned with `title^2` in Elasticsearch); a one-edit-
  distance fuzzy pass recovers misspelled queries; snippets are generated
  around the matched region and returned with `<b>…</b>` highlights.
- **Agent sessions and plan/follow-up surfaces.** `POST /search/plan`
  (plan-only: queries + open requirements + estimated cost, no execution or
  billing), `POST /search/follow-up` (continues a session on its open
  requirements), MCP `research` / `plan` tools, and a bounded in-process
  session store. `SearchResponse` gained `session_id` and
  `focus_requirements`.
- **Ranking-quality evaluation + Chinese corpus.** `nDCG@5`, MRR, Recall@5
  and Precision@5 against per-question qrels (`relevant_urls`), a 12-document
  Chinese seed corpus plus `dataset-zh.jsonl`, and a CI gate that asserts CJK
  ranking quality. Keyword coverage alone could not detect ranking regressions.

- **Language-aware analysis: CJK content is now searchable.** The embedded
  Tantivy index switched from the built-in `en_stem` chain to the SearchOS
  analyzer (`crates/searchos-index-tantivy/src/analyzer.rs`): CJK runs (Han,
  Kana, Hangul) are split into overlapping character bigrams (Lucene
  `CJKBigramFilter` semantics) while Latin runs keep the previous lowercasing
  and English stemming. Before this, a Chinese sentence was one giant token
  and `RemoveLongFilter(40)` — which counts **bytes** — silently dropped every
  run longer than 13 characters, so Chinese content was unretrievable end to
  end. Indexes created before the change keep working (the analyzer is also
  registered under the legacy `en_stem` name); rows written before the
  upgrade are re-tokenized by `searchos index rebuild`, and startup logs a
  warning when that drift is detected.
- **Agent-facing coverage gaps.** `SearchResponse` gained `coverage_gaps[]`:
  the final quality gate now exposes each open sub-question with its coverage
  state, priority, minimal acquisition strategy and a stable `requirement_id`,
  so a search agent can issue a targeted follow-up instead of re-searching
  blindly. Previously the gaps were computed and discarded.
- **Vector writes are wired into the runtime.** The outbox write path now
  embeds every indexed chunk and upserts it into LanceDB (and removes it on
  delete); `searchos index rebuild` and document reindexing rebuild vectors
  too, and RFC-031 promotion captures are vector-indexed. `VectorIndex`
  gained `delete_by_chunk_ids` and `rebuild_from`. Before this,
  `VectorIndex::upsert` had zero production callers: the vector table stayed
  empty and hybrid RRF fused BM25 against nothing. The runtime's embedder
  instance is now shared with the vector adapter so stored and query vectors
  come from the same model.

### Fixed

- **Queries are no longer parsed as Tantivy syntax.** User text is analyzed
  into literal terms (matching the Elasticsearch `multi_match` contract) and
  OR'd across title+body under a Must-wrapped block. A query containing a
  URL used to fail with `Field does not exist: 'https'` and HTTP 500; a
  trailing `AND`/`OR` or an unclosed quote failed the same way. Punctuation-
  only queries now match nothing instead of relying on parser fallbacks.
- **The retrieval candidate pool is oversized before freshness filtering.**
  Freshness/source filtering runs at the application layer after retrieval,
  so the index now returns up to 4x (capped at 200) the response cap — stale
  documents ranked above fresh ones could previously occupy the whole
  truncated window and the fresh document was never returned.

- **RFC-031 external evidence promotion & knowledge compounding**: a purchased
  external answer now becomes a durable, rebuildable document instead of a
  throwaway snippet.
  - **Quality-gated promotion.** External results are attributed to the final
    quality gate verdict; only accepted evidence is captured and indexed.
    Rejected or conflicting evidence is recorded with a reason and never
    reaches the index (fixes the knowledge-pollution defect where unverified
    snippets were indexed *before* the gate ran).
  - **Outbox-driven capture.** A dedicated bounded-concurrency worker fetches
    the page through the governed path (robots, SSRF guard, canonical URL,
    versioning, chunking) and indexes the full body, not the provider
    snippet. Capture never blocks the search response, and it invalidates the
    query/retrieval caches so the purchase cannot stay hidden behind a stale
    miss.
  - **Migration v6** (`external_evidence_promotion`): `external_acquisitions`,
    `evidence_items`, `document_bindings`, `evidence_reuse_events`,
    `documents.visibility`, plus a partial unique index that coalesces
    duplicate pending refresh rows.
  - **Plan/ledger agreement**: query expansion moved inside the external
    planner and the budget reservation now covers the final query list plus
    the promotion estimate, so the ledger records exactly what was planned.
  - **Honest savings**: local resolutions are split into first-party and
    external-sourced (`evidence_reuse_events`), and the new
    `searchos_external_*` / `searchos_refresh_coalesced_total` /
    `searchos_negative_cache_hits_total` metrics expose the compound ratio.
  - **Negative-result cache** (`search.negative_cache_ttl_seconds`, default
    300): a terminal "external search found nothing" outcome replays with
    `status = insufficient_evidence` and `replayed: true` instead of re-paying
    for the same empty search on every retry. `ExternalOnly`, `ExternalFirst`
    and strict-freshness requests bypass it exactly as they bypass the exact
    cache.
  - **Observability**: `GET|CLI evidence acquisitions|stats|reuse`,
    `searchos evidence list|stats`, and `GET /evidence/*` in the OpenAPI
    document (route-table parity enforced).
  - Fixes the migration runner: a `;` inside a `--` comment no longer splits a
    statement (it silently broke every fresh database once a comment contained
    one).

- **GHCR aliases workflow**: a `workflow_dispatch` workflow
  (`ghcr-aliases.yml`) that copies an already published multi-arch manifest
  to the documented simple names with `docker buildx imagetools create` — no
  rebuild, seconds instead of hours.
- **Published images are anonymously pullable**: verified against the v0.1.1
  artifacts — `docker pull` succeeds without `docker login`, and the pulled
  gateway image answers `/health` (reporting `version 0.1.1`), serves
  `/metrics` and completes an ingest → search round trip. The
  `package-visibility` workflow stays as a maintenance tool for the case
  where a future package is published private; its own `GITHUB_TOKEN` cannot
  change visibility (`/user/packages` resolves to the bot identity, so the
  call answers 404), so it needs a classic PAT with `write:packages`.

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
  the job timeout. (Correction: the published packages turned out to be
  public — an anonymous `docker pull` works — so no visibility change was
  needed; see `[Unreleased]`.)
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
