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

- **Final RFC Master Index (RFC-001…087).** A bilingual
  [`RFC Master Index`](docs/rfc/rfc-master-index.md) freezes the architecture
  baseline from the 83-node share: All-in-One scope, Both extension modes,
  Staged distribution and Full Agent Memory, with 8 RFC groups, ownership rules,
  a dependency matrix, P0–P6 priority, M0–M8 milestones and a 24-section RFC
  template. The earlier 087–094 proposal is dissolved and merged into 062–087.
  [`RFC-062: Federated Search Architecture`](docs/rfc/rfc-062-federated-search-architecture.md)
  and [`Master Index Gap Analysis`](docs/gap-analysis-master-index.md) record the
  repo reconciliation (029/031/087 number collisions) and the engineering
  corrections (federation strategy orthogonal to `SearchMode`, additive response
  evolution, RRF generalisation instead of a rewrite).
- **Federated planner wired into the pipeline (RFC-062, task 4 first half).**
  A new `federation` config section (`strategy`, `max_sources`,
  `max_parallel_sources`, `source_timeout_ms`) is consumed per INV-30-20.
  `legacy` (the default) leaves the search pipeline byte-identical;
  `plan_only` makes `/search/plan` report the selected federated sources and
  logs the plan inside the pipeline without perturbing the verdict;
  `execute` migrates provider execution into `FederatedExecutor` (per-source
  deadline, bounded fan-out, failure isolation) while BILLING stays in the
  service: one WebSearch cost event per executed source, priced by the same
  estimator as the legacy loop, settled by the pipeline's reservation guard —
  so the executor can never double-bill (integration gate
  `federated_execute_mode_bills_once_and_matches_legacy_results` asserts one
  event under `execute` vs the legacy ledger's per-query re-buy). The runtime
  registers the local index and every configured provider as federated
  sources; `/system/status` exposes the catalog. The pipeline-level
  equivalence gate `federated_default_matches_legacy_pipeline` runs the
  assembled service under `legacy` and `plan_only` over identical knowledge
  and asserts decision, status, quality, results, provenance content, cost
  and coverage are identical.
- **Outcome-driven per-source circuit breaker (RFC-080 slice).** Every
  federated execution now feeds its result back into the source's LIVE
  `ReliabilityProfile`: three consecutive failures open the circuit so the
  next plan rejects the source with `RejectionReason::Reliability` instead of
  paying for a dead provider; a 30s cooldown allows exactly one half-open
  probe (reusing the tested `CircuitBreaker`), and a verified success resets
  it. All scope registrations of an id trip together, so a workspace override
  cannot keep a broken global source alive. The transition increments
  `federation.source.circuit_opened_total`. Gates:
  `consecutive_failures_open_the_source_circuit_and_the_planner_rejects_it`,
  `one_success_closes_the_circuit_and_resets_the_failure_streak`,
  `outcome_updates_every_scope_copy_of_a_source`, `explicit_reset_closes_a_fully_open_breaker`
  and the live-pipeline gate
  `federated_failures_trip_the_source_circuit_in_the_live_pipeline`.
  That gate also found and fixed a real defect: under `execute`,
  `ExternalOnly`/`ExternalFirst` carry no gap-derived requirement ids, so the
  federated planner rejected every billable source and acquired NOTHING. The
  fallback purchase now gets one namespaced synthetic open requirement
  (`fallback:<query>`), preserving INV-30-04's no-re-buy rule while restoring
  parity with the legacy loop.
- **Per-source latency budget (RFC-080 slice).** `PlanningContext.max_latency_ms`
  is wired from `SearchBudget.max_execution_time_ms` on BOTH planning surfaces
  (`/search/plan` and the live pipeline): a source whose declared timeout
  cannot fit the wall-clock budget is rejected with the new auditable
  `RejectionReason::Latency` instead of being started and silently cut off.
  `ExecutionControls.with_deadline` now carries the request deadline into the
  executor, whose `bounded_timeout_ms` caps every per-source timeout by the
  REMAINING budget — a slow source can never overrun the request. Execution
  results also feed measured latency back into the live `p50_ms` / `p95_ms`
  (`registry.record_latency`, declared `timeout_ms` never inferred downward),
  and `GET /sources` exposes the observed profile. Gates:
  `planner_rejects_a_source_outside_the_wall_clock_budget`,
  `latency_feedback_updates_the_live_profile_on_every_scope_copy`,
  `per_source_timeout_is_capped_by_the_request_deadline` and the live-pipeline
  gate `federated_execute_respects_a_tiny_wall_clock_budget`.
- **Federated residuals closed (RFC-062 tasks 5/6/7).** Session transcripts
  are now a federated source (`SessionTranscriptSource`, registered at
  runtime): past research turns are workspace-scoped, mapped onto RFC-064
  `ResourceType::Transcript`, and never billed — a follow-up can see what the
  workspace already asked. Trust-weighted cross-source fusion is wired into
  the `execute` path: `fuse_ranks` orders per-source outcomes by the sources'
  `ReliabilityProfile.success_rate`, never by plan order or incomparable
  provider scores; the full math stays in `searchos-retrieval`'s
  `rrf_fuse_weighted` and this generic kernel form only ORDERS (payload,
  billing and per-provider attribution are untouched). Single-source plans
  are unchanged; gates `reliability_weight_overrides_plan_order_in_federated_outcomes`,
  `federated_outcome_order_is_deterministic` and `single_source_outcomes_are_left_untouched`
  lock the semantics. Finally, a `FederationExecutionTrace` summary (planned /
  executed / failed / timed-out / not-executed source ids + candidate count)
  is attached to the `DecisionTrace` and re-recorded by the pipeline;
  append-only migration **v9** adds `decision_traces.federation_json`, so
  `trace show` / `GET /traces` expose what happened to every planned source.
  Legacy traces keep `null` (byte-compatible); gate
  `federation_execution_summary_survives_trace_re_record` and the extended
  `federated_execute_mode_bills_once_and_matches_legacy_results` verify both
  paths.
- **Syndication detection & original-publisher identification (RFC-074
  slice).** The quality gate's source-diversity check now counts content
  communities, not hosts: `content_community_count` unions host and content-
  fingerprint nodes (`searchos_domain::evidence::normalized_content_fingerprint`,
  whitespace-collapsed/lowercased text with a 40-char comparability floor),
  so the same article republished on N hosts is ONE source and can no longer
  masquerade as independent corroboration. `merge_evidence` reports the
  syndication sets (`MergeReport.syndicated_copies`, `content_groups`,
  `independent_content_sources`) with the identified ORIGINAL publisher
  (local knowledge → earliest publication → first-seen) without mutating the
  merged evidence list, and context assembly now dedups cross-URL identical
  content with `occurrences` recorded. Gates:
  `syndicated_copies_do_not_satisfy_source_diversity`,
  `same_content_on_different_hosts_is_one_source_with_copies`,
  `original_publisher_prefers_local_then_earliest_publication`,
  `short_or_distinct_content_is_not_grouped`, and the end-to-end
  `context_assembly_collapses_cross_host_syndicated_content`.
  Also removed the stale `execute currently plans only` warning left over
  from the billing migration.
- **Agent memory as a federated source (RFC-069).** `AgentMemorySource`
  (application layer, so the kernel keeps no storage dependency) exposes
  distilled agent knowledge through the same `SearchSource` contract, reading
  with the requester's namespace and letting `NamespaceGuard` decide sharing —
  a workspace-level read never surfaces an agent-private entry and never leaks
  across workspaces. `memory_resource` maps entries onto the RFC-064 resource
  model with permission derived from the sharing policy (egress still forbidden).
- **Federated telemetry (RFC-062 §16).** Thirteen zero-seeded `federation.*`
  invariant counters — plan totals, selected/rejected sources, executed/failed/
  timeout/candidate/not-executed/circuit-opened sources, memory searched/hits,
  session transcripts searched/hits — emitted at the planner and executor call
  sites and exported through `/metrics`.
- **Durable source catalog + `GET /sources` (RFC-063).** Append-only migration
  **v8** adds `search_sources`; `SourceCatalogRepo` upserts the resolved catalog
  at startup (the in-process registry stays authoritative for execution), and
  `GET /sources` serves either the live registry or the persisted projection
  (`?persisted=true`), workspace-scoped, with `enabled_only` filtering. Route
  table and OpenAPI updated in the same change.
- **`searchos-federated` crate (RFC-062/063/064 kernel).** The unified
  `SearchSource` contract (descriptor, capabilities, cost/latency/reliability
  profiles, execution status), the scoped `InMemorySourceRegistry`
  (Agent > Project > Workspace > Global precedence, capability filtering,
  enable/disable, health), the unified `SearchResource` model (11 resource
  kinds, stable cross-source identity, content-hash-first dedup keys,
  fail-closed permission with agent allow-lists and egress-forbidden defaults),
  and the pure `FederatedPlanner` (policy → permission → scope → capability →
  existing evidence → cost → reliability, with a recorded rejection reason for
  every excluded source). 56 tests (incl. executor + adapter suites), including the named gates
  `federated_auto_never_escapes_local_only`,
  `required_source_failure_forces_partial`,
  `covered_requirements_are_never_repurchased` and
  `plan_rejections_are_auditable`.
- **Trust-weighted RRF (RFC-066 slice).** `rrf_fuse_weighted` generalises
  reciprocal rank fusion to N ranked lists with per-source weights; the
  two-list `rrf_fuse` stays as the compatibility wrapper. Ties now break by
  chunk id, so fused ordering is reproducible (P-011).
- **Context budget wired end to end (RFC-067/068).** `SearchRequest` accepts a
  `context_budget`; the application assembles an Agent-facing `context` block
  (dedup, source diversity, counter-evidence ranked first, citations reserved
  before content, verbatim-prefix truncation, honest
  `complete`/`budget_limited`/`citation_limited` status) and `SearchResponse`
  carries it additively. HTTP, MCP and CLI accept the identical object and
  validate it at the edge; the invalid budget is a client error, never a
  silently ignored field. `docs/openapi.yaml` documents `ContextBudget` and
  `ContextBlockSummary`.
- **[RFC-062 P0 dependency review](docs/rfc-062-dependency-review.md).**
  Contract-by-contract mapping of 046/054/055/063/064 → 062 with the residual
  ordered task list, and it separates the two
  equivalence-gate levels: the delivered **adapter-level** gate
  `legacy_pipeline_matches_federated_adapters` (the adapter's query and mapped
  output equal the legacy local path field by field) and the still-planned
  **pipeline-level** gate `federated_default_matches_legacy_pipeline`, which is
  the first test of the change that replaces the inline paths in
  `search_service.rs`.
- **`searchos-context` crate (RFC-067 + RFC-068).** Pure, deterministic
  Context Assembly & Token Budget: permission gate, cross-source dedup with
  provenance, counter-evidence ranked first, source diversity caps,
  citation-first budget allocation, UTF-8-safe verbatim-prefix truncation (never
  a generated summary), and honest `Complete` / `BudgetLimited` /
  `CitationLimited` reporting. 25 unit tests.

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
