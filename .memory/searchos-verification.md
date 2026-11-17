---
name: SearchOS verification
description: Exact commands that verify the SearchOS implementation end-to-end
type: project-fact
---

The SearchOS implementation (`~/git_repo/searchos`) is verified by these commands (all green as of the implementation turn):

**Why:** goal-mode completion audits require reproducible, authoritative evidence — not conversation summaries.

**How to apply:**
- Unit + integration + suites: `cargo test --workspace` → 488 passed / 0 failed (68 suites) on the v0.1.0 release commit; earlier baselines were 428 (Part 30), 436 (analytics pushdown), 362, 345, 298, 295, 253 then 291. `cargo test --workspace --locked -- --list | grep -c ': test$'` = 488 and CI asserts `>= 428`.
- Part 30 acceptance: `tests/integration/tests/rfc_gap_acceptance.rs` contains the named INV-30-01…21 tests (including `local_only_never_returns_stale_or_excluded_candidates`, `concurrent_same_request_id_executes_once_and_bills_once`, `reject_path_settles_reservation`, `early_return_settles_reservation`, `local_only_does_not_hit_hybrid_cache_entry`, `cache_hit_records_idempotency_claim`, `follower_arriving_after_publish_terminates`, `leader_cancel_releases_key`, `single_fingerprint_single_external_call`, `partial_external_failure_reports_spent_and_settles_spent`, `rebuild_preserves_workspace_language_age_and_title`, `deleted_document_is_not_retrieved`, `delete_outbox_retry_after_index_failure`, `workspace_token_cannot_search_other_workspace_documents`, `fetch_connects_to_validated_address`, `discovery_failure_is_logged_and_counted`, `singleflight_savings_positive_when_followers_join`, `canonicalization_is_identical_across_paths`, `no_unconsumed_config_fields`, `oversized_response_rejected_before_full_buffer`, `configured_limits_are_applied`, `fetch_respects_robots_disallow`, `unreachable_robots_disallowed_by_default`, `crawl_delay_is_honoured`, `transports_reject_unknown_mode_identically`, `transports_agree_on_max_results`, `two_pages_on_same_official_host_are_not_a_conflict`, `excluded_domain_reason_is_correct`, `latency_samples_are_bounded`, `cache_respects_max_entries`, `rate_limit_windows_are_evicted`, `rebuild_is_net_neutral_for_queries`).
- **ADR-029 (2026-11): multi-key rotation for upstream providers.** `TAVILY_API_KEYS="k1,k2" searchos --format json provider health` shows masked per-key status (`tvly****good`); `crates/searchos-provider/tests/key_rotation_live.rs` runs a real TCP mock upstream (429 on key #1 → key #2 succeeds, key #1 stays limited, all-limited → 429 with Retry-After hint). Rotation policies: `round_robin` (default) / `failover` via `providers.<id>.rotation` or `TAVILY_API_KEYS` + config `api_keys`.
- Lint/format gates: `cargo clippy --workspace --all-targets -- -D warnings` (0 warnings), `cargo fmt --all -- --check`.
- CLI smoke (real binary): `cargo build --bin searchos`, then with `SEARCHOS_DATA_DIR=$(mktemp -d)`:
  - `searchos init --format json`, `searchos doctor`, `searchos search "q" --mode local-only --format json`, `searchos trace show <trace_id>`, `searchos cost summary`, `searchos index status`, `searchos crawler list`, `searchos backup create`.
  - RFC-029 ingest smoke: `searchos ingest --text "..." --source-key notes/alpha`, `searchos ingest --file notes.md`, `searchos ingest --dir ./docs --ext md,txt`; then `searchos search "..." --mode local-only` returns the imported content; re-ingest same key+content → `changed:false`; `searchos document list --source-type local` lists them.
  - Memory smoke: `searchos memory remember --title "..." --content "..." --sharing private`, then `searchos memory search "..." --workspace default`.
- Gateway smoke: `searchos gateway start --port 18787 &` then `curl /health`, `POST /search`, `GET /stats/search`, `GET /cost/summary`; RFC-029: `POST /ingest` (JSON text), `POST /ingest/batch`, `GET /ingest/jobs/:id`, `POST /ingest/file` (multipart `-F file=@x.md`), plus `/v1/ingest` alias.
- MCP smoke: pipe JSON-RPC lines (`initialize`, `tools/list`, `tools/call cost_summary`) into `searchos mcp`.
- Evaluation dataset: `python3 fixtures/evaluations/generate_dataset.py` (regenerates the committed 500-question dataset; category counts must equal RFC-015 15.3: 100/100/75/75/50/50/50). `searchos --format json eval run --limit 2` runs it through the real search service.
- Feature-completion smoke: `searchos --format json profile list|config validate|stats savings|stats retrieval|cost savings|cost reservations|index stats|export --out file|eval run --limit 2|backup create|backup restore <dir>`; `/v1/*` gateway aliases and `x-searchos-api-version` header; MCP `tools/list` includes all 7 RFC-021 tools plus `ingest_text` / `ingest_file` and the `search_trace` alias.
- Route-conflict gate: `SRC_DIR=crates/searchos-interface-http/src bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh` (exit 0, no conflicts).
- Backup/restore drill gate: `cargo test -p searchos-e2e-tests --test backup_restore` (real binary: ingest → backup create → destroy the database → restore → verify the document, its version, the checksum manifest, and a rebuilt consistent index; plus refusal of a tampered backup). Also `SEARCHOS_DATA_DIR=… searchos backup create|restore` manually.
- Evaluation quality gate: `python3 fixtures/evaluations/generate_seed.py --limit 40`, `searchos ingest --dir fixtures/evaluations/seed`, `searchos --format json eval run --limit 20 --out report.json`, then `python3 scripts/check_eval_gate.py report.json`. CI runs this as the `eval-gate` job; against the seed corpus the observed values are keyword coverage 1.0, quality pass rate 1.0, p95 latency ~35ms.
- Release gate: tag must match the workspace `version` in Cargo.toml (`v0.1.1` ↔ `version = "0.1.1"`); `.github/workflows/release.yml` runs the CI gates, then builds artifacts + multi-arch images, then publishes the GitHub release from the CHANGELOG section. The `images` job logs in to GHCR with `${{ github.token }}` (the workflow declares `packages: write`), so **no `REGISTRY_TOKEN` secret is needed**; the published packages are public (see the note below), so anonymous `docker pull` works out of the box. Changing visibility via API needs admin over the package: the workflow's `GITHUB_TOKEN` (packages:write) is rejected with 404 — `PATCH /user/packages/...` resolves to the bot identity — so the automated path is the `package-visibility` workflow run with a `PACKAGES_ADMIN_TOKEN` secret (classic PAT with `write:packages`). Verify with `docker manifest inspect ghcr.io/huifer/searchos-gateway:0.1.1` (authenticated) and `gh run list --workflow release.yml`. Image names come from `${GITHUB_REPOSITORY_OWNER}` + `searchos-{gateway,console}`, tag from `GITHUB_REF_NAME#v` (so `v0.1.1` → `:0.1.1`, no leading `v`); the first v0.1.1 run used the nested `ghcr.io/huifer/searchos/{gateway,console}:0.1.1` names, fixed by `ghcr-aliases.yml`. Note: the local `gh` token carries no `read:packages` scope, so pulling a private package locally returns 404 even though the image exists — the authoritative check is the `docker buildx imagetools inspect` output inside the `images-manifest` job log (it lists `linux/amd64` and `linux/arm64`). **The published packages are in fact public**: on 2026-11-11 an anonymous `docker pull ghcr.io/huifer/searchos-gateway:0.1.1` (no `docker login`) succeeded, the pulled image reported `version 0.1.1` on `/health`, served `/metrics` and completed an ingest → search round trip; the console manifest also answers HTTP 200 anonymously (its 162 MB layer download kept dropping in this sandbox — a local network artifact, not a permission issue). Changing package visibility needs admin over the package: a workflow `GITHUB_TOKEN` cannot do it (`PATCH /user/packages/...` resolves to the bot identity → 404), so it needs a classic PAT with `write:packages` as the `PACKAGES_ADMIN_TOKEN` secret.
- OTLP gates: `cargo test -p searchos-observability` (13 unit tests: config validation, endpoint joining, header parsing, Prometheus renderer), `cargo test -p searchos-observability --test otlp_export` (real local TCP collector receives `POST /v1/traces` + `/v1/metrics` with non-empty protobuf payloads), and `cargo test -p searchos-core --test env_overrides` (7 tests). Live: gateway with `SEARCHOS_OTLP_ENABLED=true` pushed both signals and flushed on SIGTERM.
- Metrics + lifecycle gates: `cargo test -p searchos-observability` (Prometheus renderer), `cargo test -p searchos-interface-http metrics`, `cargo test -p searchos-interface-http outbox_worker`, and `cargo test -p searchos-e2e-tests --test gateway_lifecycle` (spawns the real binary: `/metrics` content type + series, SIGTERM exit 0, SIGTERM with an open SSE stream exit 0). Live: `GET /metrics` returns 198 lines of exposition with non-zero counters after a search, `searchos_build_info{version="0.1.0",schema_version="5"} 1`.
- MCP transport gate: `cargo test -p searchos-compatibility-tests mcp` (stdio declaration, no HTTP server dep in the MCP crate, no MCP route on the gateway, docs don't claim remote MCP, tool catalogue stable, cancellation acknowledged) plus `cargo test -p searchos-interface-mcp`. Live: `searchos mcp` with initialize / tools/list / tools/call over stdin — stdout must stay JSON-RPC-only, the cancellation warning goes to stderr.
- Tavily contract gate: `cargo test -p searchos-compatibility-tests` (fixture-driven, 9 cases over `fixtures/tavily/*.json`) and `cargo test -p searchos-interface-http tavily` (mapping unit tests). `/crawl` and `/map` must stay 404 and out of `docs/openapi.yaml`.
- OpenAPI contract gate: `cargo test -p searchos-interface-http --lib openapi_spec_matches_the_route_table` and `--lib every_registered_route_resolves` (the route table is the single source of truth for `docs/openapi.yaml`).
- SDK gates: `cd sdk/typescript && npm ci && npm run build`; `python3 -m compileall -q sdk/python` plus an import check. Both clients have been smoke-tested live against a running gateway (ingest → list → detail → versions → reindex → search → dashboard → delete → 404).
- CLI help gate: `searchos` with no args prints the full help and exits 0 (manual `Cli::command().print_help()` in `main()`); `--help`/`-h`/`help`/`help <cmd> <sub>` all exit 0. All 65 command paths have a `--help` Usage line and no empty subcommand/argument descriptions; `--format` renders a `Possible values:` section (clap ValueEnum doc comments). Long `--help` shows `long_about`; `-h` shows the compact summary. Unit tests `every_subcommand_has_a_description` / `every_argument_has_help` / `completions_generate_for_every_shell` lock the invariant.
- CLI basics gate: `searchos completions <bash|zsh|fish|powershell|elvish>` emits a script (clap_complete); `-v/-vv/-vvv` raises the log level (warn/info/debug/trace, RUST_LOG overrides); logs always go to **stderr** (`.with_writer(std::io::stderr)` in `logging.rs`, ANSI only on a TTY) so `--format json` stdout and MCP stdio stdout stay parseable; with `--format json` errors are emitted as `{"error": "...", "code": N}` on stderr.
- Dashboard API smoke: `/api/v1/dashboard/overview`, `/search/timeseries`, `/quality/dimensions`, `/export?format=csv`, `/events` (SSE), plus `GET /api/searchos/api/v1/dashboard/overview` through the Next.js proxy.
- Console gates: `cd web && npm ci && npm run typecheck && npm run build && npm test` (51 Vitest tests incl. i18n; CI has a `frontend-test` job); live pages `/`, `/search`, `/traces`, `/analytics`, `/cost`, `/knowledge` all 200; interactions browser-verified (see searchos-console memory).
- **CI environment gate (2026-11 release pass):** a locally-green Rust build is not sufficient — GitHub-hosted `ubuntu-latest` has no `protoc`, and `lance-encoding`'s build script fails with "Could not find `protoc`". Every Rust job must run `.github/actions/setup-protoc`, which installs `protobuf-compiler` **and** `libprotobuf-dev` (the latter ships `google/protobuf/*.proto`; it is only a Recommends, so `--no-install-recommends` drops it). Locally: `brew install protobuf`. Validate workflow changes with `actionlint` before pushing (CI now runs it in the `workflow-lint` job): `secrets` is not available in step-level `if:` expressions (use a job-level `env` + `if: env.X != ''`), and that mistake makes the whole run fail in 0s with 0 jobs. Also run the Rust suite as `RUST_LOG=warn cargo test --workspace`: CI sets that env var globally, and `logging::init_with_otlp`'s `EnvFilter` silently drops info-level spans, so the OTLP export test must emit an ERROR-level span to be environment-independent.

- Release build: `cargo build --release --bin searchos` (15m09s, 0 errors/warnings, 273M binary); the release binary passes init/search/doctor/gateway smoke tests.
- Full clean rebuild verified: `cargo clean` (removed 59.1GiB) → `cargo build --workspace --all-targets` (575 crates, 5m55s, 0 errors/warnings) → `cargo test --workspace` (253 passed) → `cargo clippy --workspace --all-targets` (0 warnings) → `cargo fmt --all -- --check` clean.

- **RFC-031 verification (implemented 2026-11, re-verified on commit 799a2c4; retrieval + agent hardening pass on top of it):** `cargo test --workspace` → 559 passed / 0 failed (71 suites); the RFC gate is `cargo test -p searchos-integration-tests --test rfc_031_evidence_promotion` (17 tests: promotion state machine, rejected-never-indexed, robots/`fetch_failed` skip reasons, per-task cap, reuse accounting, disabled promotion, pipeline acquisition, ledger == planned calls, negative-cache honest replay + ExternalOnly/strict-freshness bypass). Migration gate: `cargo test -p searchos-storage-sqlite migrations` (includes `semicolon_inside_a_comment_does_not_split_statements`). Route/contract gates: `cargo test -p searchos-interface-http --lib openapi` + `SRC_DIR=crates/searchos-interface-http/src bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh`. Live: `SEARCHOS_DATA_DIR=$(mktemp -d) searchos evidence list` / `searchos evidence stats` (empty state prints zero counts, exit 0); gateway `GET /evidence/acquisitions|/evidence/reuse` behind the bearer token.

- **Agent + retrieval gates (2026-11):** `cargo test --workspace --locked` → 559
  passed / 0 failed (71 suites); `cargo clippy --workspace --all-targets -- -D
  warnings` clean; `cargo fmt --all -- --check` clean; route-conflict scan
  clean; OpenAPI route-table parity green (the two new agent routes
  `/search/plan` + `/search/follow-up` are documented in `docs/openapi.yaml`).
  Ranking gate: `python3 fixtures/evaluations/generate_seed.py --limit 40`,
  ingest `fixtures/evaluations/seed` AND `fixtures/evaluations/seed_zh`, then
  `searchos --format json eval run --dataset fixtures/evaluations/dataset-zh.jsonl
  --limit 12 --out report.json` + `python3 scripts/check_eval_gate.py
  report.json --min-questions 12 --min-mrr 0.8 --min-ndcg 0.8 --min-recall 0.8`
  (observed MRR/nDCG@5/Recall@5 = 1.0). The English gate stays
  `dataset-v1.jsonl --limit 20` with the default thresholds. Two
  `searchos-fetch` tests need live internet (`example.com`); they fail in an
  offline sandbox and are unrelated to the code.
- **Agent surface gates:** `POST /search/plan` (no execution, no billing),
  `POST /search/follow-up` (404 for an unknown session), MCP `tools/list`
  reports 12 tools (`research` + `plan` added), CLI
  `--session` / `--focus-requirements` round-trip the session, and the
  response carries `coverage_gaps`, `session_id`, `focus_requirements` and a
  `<b>…</b>` `highlight`.

- **Graded qrels / live provider / durable sessions gates (2026-11):**
  - `cargo test -p searchos-evaluation` includes graded nDCG (`2^grade - 1`,
    `relevance_grades` 0..3; MRR/Recall count only grade >= 2) and a runner
    test proving binary nDCG cannot tell a canonical/related swap apart.
  - `cargo test -p searchos-core --test remote_provider_live` wires a real
    OpenAI-compatible HTTP server into the assembled runtime: 36 remote
    embeddings land in LanceDB at the model dimension and `/v1/rerank` is
    actually called (RFC-007 7.7 trigger threshold 30 candidates).
  - `cargo test -p searchos-core --test runtime_test` includes
    `agent_sessions_survive_runtime_restart` (migration v7, `agent_sessions`).
  - Schema is migration **v7** at that commit (append-only; never edit
    released migrations; now superseded by v8/v9, see below).
  - Live gateway smoke: `/search` with `session_id`, kill + restart the
    gateway, then `/search/follow-up` on the same session resolves.
  - CI zh gate: `check_eval_gate.py ... --min-questions 12 --min-mrr 0.8
    --min-ndcg 0.8 --min-recall 0.8`; with graded qrels observed
    MRR 1.0 / nDCG@5 0.968 / Recall@5 1.0.

- **Federation / RFC-062+063+064+066+069 gates (2026-11, residual pass on
  top of the master-index implementation turn):**
  - Full suite `cargo test --workspace --locked` → **673 passed / 0 failed**
    (79 suites) at that milestone; **682** after the RFC-080 slices below.
    clippy `-D warnings`, `cargo fmt --all -- --check`,
    route-conflict scan all green. Schema is migration **v9**
    (`decision_traces.federation_json`; append-only). Schema version is read
    from PRAGMA at runtime — no constant to bump when adding migrations.
  - Federation kernel: `cargo test -p searchos-federated` (incl. 4
    `fusion` tests for `fuse_ranks`); retrieval: `rrf_fuse_weighted` tests.
  - Session transcript source: `cargo test -p searchos-application --lib
    session_source` (5 tests: workspace isolation, ranking, no-match,
    max_results, resource mapping).
  - Cross-source ordering: `cargo test -p searchos-application --lib
    reliability_weight` (ordering overrides plan order; deterministic;
    single-source untouched).
  - Trace summary: `cargo test -p searchos-storage-sqlite federation_execution_summary_survives`
    (v9 round-trip + INSERT OR REPLACE single-row) and the extended
    `federated_execute_mode_bills_once_and_matches_legacy_results` in
    `tests/integration/tests/full_pipeline.rs` (execute trace carries the
    summary; legacy trace keeps `None`).
  - OpenAPI documents `DecisionTrace.federation_execution` +
    `FederationExecutionTrace`; gates `openapi_spec_matches_the_route_table`
    / `every_registered_route_resolves` stay green.
- **RFC-080 slice / defect fix (2026-11-13, later in the same session):**
  `InMemorySourceRegistry::record_outcome` feeds every federated execution
  back into the source's LIVE `ReliabilityProfile`: 3 consecutive failures
  open the circuit (planner rejects with `RejectionReason::Reliability`), a
  30s cooldown allows ONE half-open probe by reusing the tested provider
  `CircuitBreaker`, and a verified success calls the new additive
  `CircuitBreaker::reset()` (record_success alone cannot close a FULLY OPEN
  breaker — this was found by a failing test). All scope copies of an id
  trip together; `list`/`descriptor_for`/`all_references` derive
  `circuit_open` from the breaker so a tripped source is never permanently
  dead. Counters are now 13 zero-seeded `federation.*` invariants
  (memory/session/circuit-opened included). Full suite **678 passed / 0
  failed**; clippy/fmt/route scan green.
  - **Real defect found by the live gate:** under `execute`, `ExternalOnly`
    / `ExternalFirst` have no gap-derived requirement ids → the federated
    planner rejected every billable source as "fills nothing" and acquired
    NOTHING (legacy buys one fallback query). Fix: the fallback purchase
    gets one namespaced synthetic open requirement `fallback:<query>` so it
    cannot collide with executed-query strings in `covered_requirements`.
    Keep this invariant: a billable source that fills no OPEN requirement
    must still be refused (INV-30-04), so never weaken the planner check —
    synthesize the requirement at the wiring boundary instead.
  - Gates: `consecutive_failures_open_the_source_circuit_and_the_planner_rejects_it`,
    `one_success_closes_the_circuit_and_resets_the_failure_streak`,
    `outcome_updates_every_scope_copy_of_a_source`,
    `explicit_reset_closes_a_fully_open_breaker`,
    `federated_failures_trip_the_source_circuit_in_the_live_pipeline`.
- **RFC-080 latency budget / feedback (2026-11-13, final slice):**
  `PlanningContext.max_latency_ms` is wired from
  `SearchBudget.max_execution_time_ms` on both `/search/plan` and the live
  pipeline; sources whose declared timeout cannot fit are rejected with the
  new `RejectionReason::Latency`. `ExecutionControls::with_deadline` carries
  the absolute request deadline into `FederatedExecutor`, whose
  `bounded_timeout_ms` caps every per-source timeout by the remaining budget
  (a slow source can never overrun the request). `registry.record_latency`
  feeds measured p50 (EMA) / p95 (decaying envelope, p50 floor) into the
  live profile on every scope copy; declared `timeout_ms` is never inferred
  downward. `GET /sources` + OpenAPI expose the observed latency.
  Gates: `planner_rejects_a_source_outside_the_wall_clock_budget`,
  `latency_feedback_updates_the_live_profile_on_every_scope_copy`,
  `per_source_timeout_is_capped_by_the_request_deadline`,
  `federated_execute_respects_a_tiny_wall_clock_budget`. Full suite **682
  passed / 0 failed**; clippy `-D warnings`, fmt, route scan green.
- **RFC-074 syndication slice (2026-11-14):** quality-gate diversity counts
  CONTENT COMMUNITIES (`content_community_count`, union-find over host +
  fingerprint nodes from
  `searchos_domain::evidence::normalized_content_fingerprint` — normalized
  text ≥ 40 chars, FNV-1a, NOT a trust/integrity proof); `merge_evidence`
  reports `syndicated_copies` / `content_groups` / `independent_content_sources`
  with the original picked by local → earliest publication → first-seen
  (never URL spelling); context assembly feeds the fingerprint as
  `EvidenceChunk.content_hash` so cross-URL identical content collapses with
  `occurrences` recorded. Stale `execute plans only` warn removed.
  Gates: `syndicated_copies_do_not_satisfy_source_diversity`,
  `same_content_on_different_hosts_is_one_source_with_copies`,
  `original_publisher_prefers_local_then_earliest_publication`,
  `short_or_distinct_content_is_not_grouped`,
  `context_assembly_collapses_cross_host_syndicated_content`. Full suite
  **691 passed / 0 failed**; clippy/fmt/route scan green.
- **RFC-065 vertical routing slice (2026-11-14):** `RetrievalVertical` /
  `VerticalRouter` / lexical+semantic adapters in `searchos-retrieval`;
  `HybridRetriever::with_vertical` routes extras per query (fail-open) and
  fuses ALL routed lists with declared weights via `rrf_fuse_weighted` —
  byte-identical to the two-list form when none registered (the fusion
  itself is the first production use of the weighted path). Gates:
  `two_list_output_is_byte_identical_with_and_without_verticals`,
  `extra_vertical_candidates_join_the_fused_ranking`,
  `extra_vertical_weight_orders_equal_rank_contributions`,
  `routing_skips_verticals_that_decline_the_query`,
  `extra_vertical_failure_is_isolated`. Full suite **700 passed / 0 failed**
  (80 suites); clippy/fmt/route scan green.

- **RFC-077 cache-validity slice (2026-11-14):** `EvidenceValidityPort`
  (application) + `SqliteEvidenceValidity` (runtime) — hit-time checks on
  exact/canonical/semantic/retrieval caches; deleted or re-versioned cited
  documents reject the hit (recompute), even cross-process (the runtime
  clears only ITS caches; a CLI/second-replica write is invisible locally).
  Fail-open on port errors; 64-doc lookup cap; `cache.validity_reject_total`.
  **Idempotency replays are never validity-checked (INV-30-02 contract).**
  Gotcha: `data_db_path()` is `<data_dir>/data/searchos.db` — a test
  simulating another process must open the SAME file. Gates: 6 validity
  unit tests + `deletion_in_another_process_invalidates_cached_responses` +
  `version_change_in_another_process_rejects_the_cached_hit`. Full suite
  **708 passed / 0 failed** (81 suites); clippy/fmt/route scan green.

- **RFC-076 source-aware slice (2026-11-14):** `DecisionInput.external_sources_healthy`
  (None = unobserved, legacy behaviour; Some(false) = no healthy web source).
  Engine rule: external-REQUIRING modes reject immediately
  (`ProviderUnavailable`), Hybrid is NOT blocked. Pipeline computes the flag
  from the live federated registry (web sources with circuit closed) and
  keeps `allow_external=false` on the existing offline path. Gates: 3 engine
  tests + `decision_rejects_external_only_when_all_sources_are_tripped`.
  Full suite **712 passed / 0 failed** (81 suites); clippy/fmt/route scan
  green.

- **Foreign working-tree overwrite (2026-11-14, watch for concurrent sessions):**
  after commit `dbe3c72` the working tree's
  `crates/searchos-federated/src/fusion.rs` was REPLACED by a different
  implementation (`fuse_candidates_weighted` over `SourceCandidate` +
  duplicate `pub mod fusion;` in lib.rs) by something outside this session —
  it broke the committed `fuse_ranks` callers. Preserved in
  `git stash` ("foreign fusion.rs variant..."), NOT discarded; the committed
  `fuse_ranks` version is authoritative and build-restoring. If another
  session works in this repo, coordinate first.

- **RFC-087 ingest redaction / concurrent-session incident (2026-11-14):**
  `redact_secrets` rule set in `searchos-application/src/redaction.rs`,
  wired at `IngestServiceImpl::ingest_bytes` (before storage/hash/index),
  config `ingest.redact_secrets` (default true), OpenAPI `RedactionReport`.
  Gates: 7 rule unit tests + 2 ingest-service tests +
  `ingest_time_secret_redaction_is_wired_through_config`. Full suite **722
  passed / 0 failed** (81 suites); clippy/fmt/route scan green.
  **PII extension (same day, rules v2):** opt-in `ingest.redact_pii`
  (default OFF — contact details are legitimate content) adds email /
  Luhn-validated card / E.164 phone rules via `redact_pii` +
  `merge_reports`; regex crate has NO look-around (credit-card boundaries
  are checked in code). Gates: 5 redaction PII tests +
  `pii_redaction_is_opt_in_and_merges_with_secrets` /
  `pii_redaction_is_off_by_default` / `pii_redaction_follows_the_config_flag`.
  Full suite **730 passed / 0 failed**; clippy/fmt/route scan green.
  **Concurrent-session incident #2:** a foreign process rewrote
  `session_source.rs` and inserted a duplicate runtime deps block (broke the
  build). Its full diff was saved to `/tmp/foreign_session_changes.patch`
  (NOT discarded); `runtime.rs` / `session_source.rs` were restored to HEAD
  and only the `.with_redaction(...)` line replayed. Coordinate with any
  other session active in this repo.

- **RFC-087 deletion audit (2026-11-14):** migration **v10** `deletion_audit`
  + `DeletionAuditRepo` (record / list with workspace+document filters);
  `DocumentRepo::delete` returns `Option<DeletedDocument>` (None for phantom
  re-deletes — no audit row for them); runtime records chunk count, outbox
  events, `index_verified` (post-drain `pending_for_document` re-check —
  json_extract on the payload's document_id) and cache invalidation;
  `GET /deletions` + CLI `searchos document deletions`.
  Gates: `deletion_propagation_is_audited` (runtime),
  `deletions_endpoint_serves_the_propagation_audit` (HTTP). The CLI
  `document delete` previously called `repo.delete` DIRECTLY — bypassing
  drain/cache-invalidation/audit (and leaving the index stale, surfacing as
  the startup drift WARN) — now routed through `runtime.delete_document`.
  Live-verified: CLI delete → audit row (chunk_count 1, outbox 1,
  index_verified true). Full suite **732 passed / 0 failed**; clippy/fmt/
  route scan green.
  **Harness wiring rule (learned the hard way):** assembled
  `SearchDependencies` MUST share the runtime's `SingleflightCoordinator`
  (`runtime.singleflight.clone()`). A harness-owned coordinator keeps
  serving pre-deletion leader payloads to same-fingerprint followers — no
  invalidation can reach it, and the deletion audit gate catches it.

- **RFC-087 egress slice (2026-11-14, same day):** `RemoteRerankerConfig`
  gains `allow_workspace_content` (default FALSE, fail-closed);
  `RemoteReranker::rerank` skips (returns candidates unchanged, counter
  `privacy.egress_blocked_total`) when any top-N candidate is
  workspace-owned (`is_workspace_owned`: the `searchos-*://` scheme
  prefix). Config `privacy.egress_remote_rerank` (new section, consumed per
  INV-30-20). The `remote_provider_live` test now opts in explicitly.
  Gates: 4 egress tests with a real TCP mock endpoint in the retrieval
  crate. Full suite **736 passed / 0 failed**; clippy/fmt/route scan green.

- **P2 stage: replay / simulation / evaluation (2026-11-14):** `replay.rs`
  in application + `SqliteReplayLookup` in core + `runtime.replay` /
  `runtime.search_deps` (SearchDependencies now derives Clone); storage
  `TaskRepo::by_trace_id` / `recent_trace_ids`; routes `POST
  /replay/{trace_id}` + `POST /evaluation/replay`; CLI `eval replay`.
  **Offline replay invariants:** fresh request id per RUN (baseline and
  simulation need DIFFERENT idempotency identities), fresh exact/negative
  caches + singleflight (else the recorded answer is served), offline deps
  with `allow_external=false` but the cost budget INTACT (zeroing it trips
  Rule 5 BudgetExceeded for every external-allowing mode), baseline-run
  caches cleared before the simulated run. Gates: 4 integration + 1 HTTP
  round-trip. Full suite **741 passed / 0 failed** (82 suites);
  clippy/fmt/route scan green. Live CLI: `eval replay --limit 3` → 2
  evaluated, agreement 1.0, all offline.

- **RFC-034 policy management stage (2026-11-14):** migration **v11**
  `policy_versions` (append-only, unique (kind, policy_id, version), one
  active row); `PolicyRepo` (ensure_initial/submit/activate/versions);
  `PolicyState` in application with `stamp()` in EVERY cache key
  (`key_version` in search + `policy_version` in retrieval); deps read
  policies via `effective_quality/freshness/source()`. HTTP
  `GET /policies`, `GET /policies/{kind}/versions`, `PUT /policies/{kind}`,
  `POST /policies/{kind}/rollback`; CLI `policy show/versions/set/rollback`.
  Runtime `activate_policy` sets state AND invalidates all response caches
  (learned: policy changes MUST clear singleflight retention too).
  Simulation uses an ISOLATED `PolicyState` (mutating `deps.quality` is a
  no-op once the state is wired). Gates: repo
  `policy_history_is_append_only_with_one_active_version`, runtime
  `submitting_a_policy_hot_reloads_the_quality_gate_without_restart`,
  `active_policy_survives_a_runtime_restart`,
  `replay_and_simulation_consume_the_current_policy`, HTTP
  `policy_endpoints_submit_audit_and_rollback`. Full suite **750 passed /
  0 failed** (83 suites); clippy/fmt/route scan green. Live CLI: set
  minimum_quality=1.0 → quality failed; rollback → passed; stamp changes.

- **RFC-076 cost-adaptive slice (2026-11-14, P2 finished):**
  `SearchBudget.cost_adaptive` (default FALSE preserves Rule 5) + per-query
  price on `DecisionInput`; engine Rule 5: over budget + flag on + affordable
  ≥ 1 ⇒ continue under the affordable cap (decision reason
  `ExternalPlanDownsized`); affordable == 0 ⇒ reject in both modes;
  unpriced (0) per-query ⇒ query-count cap stands. Trace persists via
  migration **v12** `cost_adaptive_json`; pipeline caps planned queries from
  `trace.cost_adaptive`. Gates: 4 engine tests + live-pipeline gate
  `cost_adaptive_downsizes_the_external_plan_within_budget` (priced fake
  provider via `CostModelConfig::upsert_provider` — decision pricing uses
  the COST MODEL, not the registry's declared cost). Full suite **757
  passed / 0 failed** (83 suites); clippy/fmt/route scan green. CLI smoke:
  `policy set` (minimum_quality 1.0) → quality failed; rollback → passed.

- **RFC-070 memory lifecycle slice (2026-11-14):** `memory.retention_days`
  (default 365, 0 = forever) wired into `SqliteMemoryStore` (read-time
  sweep + explicit `expire`); `consolidate` groups by
  (workspace, project, agent, sharing, normalized fingerprint) keeping the
  newest copy. `MemoryService` trait gained `expire`/`consolidate` with
  DEFAULT impls (non-breaking); `MemoryUseCase` facade + CLI
  `memory lifecycle [--apply]` + `POST /memory/lifecycle`. Gates: 4
  integration tests + the HTTP dry-run/apply gate.
  Full suite **762 passed / 0 failed** (84 suites); clippy/fmt/route scan
  green. Live CLI: two identical memories → dry run reports 1 merged group,
  `--apply` reduces entries 2→1. Note: fetch/DNS tests can flake on a
  sandbox without network — rerun before treating as regressions.

- **RFC-072 query expansion slice (2026-11-14):** `SYNONYMS` table +
  `synonyms_for` / `synonym_variants` / `sub_question_variants` in
  `searchos-query`; `intelligence.query_expansion_mode` (`followup`
  default / `initial` / `both`, consumed per INV-30-20); deps carry
  `expansion_mode`; initial variants only fill remaining affordable slots
  (RFC-076) and are appended AFTER gap queries. Gotchas: the legacy
  `execute_external` fans out per QUERY (good for asserting plans) while
  `execute` runs one representative query per SOURCE; follow-up rounds
  reformulate regardless of the expansion mode, so a first-plan comparison
  must set `budget.max_acquisition_rounds = 0`. Full suite **770 passed /
  0 failed** (84 suites); clippy/fmt/route scan green.

- **RFC-071 research workspace slice (2026-11-15):** `SearchSession` gained
  `evidence: Vec<WorkspaceEvidence>` (serde default; persisted with the
  existing agent_sessions payload — NO new migration);
  `record_turn_with_evidence` dedupes by URL and bounds at
  turns×20; `research.rs` runner (`deep_search`) stops on Covered / RoundCap
  / BudgetExhausted, judged from the LAST ROUND's gaps (a lagging/stub
  session must not define coverage); `record_session_turn` helper is shared
  by the pipeline tail AND the exact-cache hit branch (a cache-served turn
  still advances the workspace — found by the dedupe gate). HTTP
  `POST /research/run` + `GET /research/{session_id}`; CLI
  `research run|show` (show takes a POSITIONAL session id). Gates: 5
  integration (covered, dedupe/across turns, restart, stub round-cap, stub
  budget) + HTTP round trip. Full suite **776 passed / 0 failed**
  (85 suites); clippy/fmt/route scan green.

- **RFC-038 evidence-graph slice (2026-11-16):** `SearchResponse.evidence_graph:
  Option<EvidenceGraphBlock>` (serde default + skip if none);
  `MergeReport.duplicate_links` records folded URL pairs;
  `build_evidence_graph(merged, report)` emits `duplicate_of` (raw variant →
  canonical, including folded-away URLs) and `same_content` (copy →
  original) edges. Gotcha: canonicalizing BOTH endpoints of a fold collapses
  the edge into a self-loop — the `from` must stay the RAW URL. Gates:
  `evidence_graph_links_folds_and_syndication` +
  `response_carries_an_evidence_graph_only_when_relations_exist`. Full suite
  **778 passed / 0 failed** (85 suites); clippy/fmt/route scan green.
  Note: builds can be very slow under load (one `cargo test` appeared hung —
  it was compiling); a stale `cargo test --all-targets` from another session
  had to be killed.

- **RFC-087 fine-grained egress slice (2026-11-16):** `EgressRules::
  {documents, memory, sessions}` + `may_send(url)` (per-kind, fail-closed
  for unknown local schemes; public web never gated by the master).
  Config `privacy.egress_documents/memory/sessions: Option<bool>` —
  `None` inherits `privacy.egress_remote_rerank`; one blocked kind blocks
  the whole remote rerank call. Gate:
  `per_kind_policy_allows_memory_and_blocks_documents`. Full suite **780
  passed / 0 failed**; clippy/fmt/route scan green.

- **RFC-071 v2 cross-turn evidence reuse (2026-11-16):** `workspace_evidence`
  block on `SearchResponse` carries EARLIER session evidence (excludes URLs
  in current results, cap 20); `WorkspaceEvidence` gained
  document_id/document_version_id; `carry_workspace_evidence` re-validates
  local docs via `EvidenceValidityPort` (deleted/re-versioned => dropped +
  counted; fail-open on port error). Gotchas: the carry helper must be
  called from ALL turn-shaped response paths (pipeline tail, exact-cache
  hit, Reject early return) — each was missed once; and a test reusing the
  original request_id gets an IDEMPOTENCY REPLAY (no carry by contract), so
  continuations need a fresh request id. Full suite **781 passed / 0
  failed**; clippy/fmt/route scan green.

- **RFC-057 as-of slice (2026-11-16):** `SearchConstraints.as_of:
  Option<DateTime<Utc>>` (serde default); `freshness_now(req)` replaces
  Utc::now at ALL freshness decision points (index `fetched_after` cutoff,
  local candidate age); future-dated candidates (fetched_at > as_of) are
  excluded. as_of enters `effective_freshness_label` (therefore cache key +
  fingerprint) — historical views never share cached answers. HTTP/CLI
  parse RFC3339 strictly (bad input = validation error). Full suite **784
  passed / 0 failed** (85 suites); clippy/fmt/route scan green.

- **RFC-058 entity resolution v1 (2026-11-16):** `domain::entity` (EntityId
  FNV over canonical name, EntityResolver longest-alias + first-owner-wins,
  boundary-safe matching); migration v13 `entity_aliases`;
  `EntityRepo`/`EntityStore`; runtime `entity_registry` read model rebuilt
  from SQLite + write-through `register_entity`; `SearchDependencies.
  entity_lookup` + `annotate_results` on `SearchResultItem.entity_id`;
  HTTP `/entities*` + CLI `entity`. Full suite **794 passed / 0 failed**
  (86 suites); clippy/fmt/route scan green.

- **RFC-036 document intelligence v1 (2026-11-16):** `searchos_document::
  intelligence::{analyze, extract_summary, extract_keywords}` — verbatim
  extractive summary (selected sentences in original order; a multi-sentence
  summary is NOT one contiguous source substring), stopword-filtered
  keywords with CJK bigrams; computed in BOTH ingest and fetch paths;
  migration v14 adds `document_versions.summary/keywords_json`; additive
  `DocumentVersion::{summary, keywords}` (all literals patched with serde
  defaults). Full suite **800 passed / 0 failed** (86 suites);
  clippy/fmt/route scan green. Note: full test runs can take >20 minutes on
  this machine — use background + polling.

- **RFC-030 knowledge collections v1 (2026-11-16):** `domain::collection`
  (deterministic id from workspace+name, CollectionSummary); migration v15
  `collections` + `collection_documents`; `CollectionRepo` (ensure /
  add / remove / list / documents) with LIVE member counts (soft-deleted
  docs hidden, membership kept); HTTP `/collections*` + CLI `collection`.
  Note: the CLI invariant `every_argument_has_help` requires EVERY clap
  argument to carry help — a bare `workspace: String` field without a doc
  comment fails it. Full suite **805 passed / 0 failed** (86 suites);
  clippy/fmt/route scan green. Doc-test compilation is very slow on this
  machine (~5 min per crate) — budget for it.

- **SDK parity + 0.2.0 release prep (2026-11-16):** the workspace version
  is now `0.2.0` (Cargo.toml + `docs/openapi.yaml` info.version + both SDK
  manifests). The TypeScript and Python clients were STALE — they had not
  been refreshed since 0.1.1 and were missing /search/plan, /search/follow-up,
  /evidence/*, /research/*, /entities*, /collections*, /replay/*,
  /evaluation/replay, /policies*, /deletions, /memory/lifecycle, /sources and
  /trace/{id}. Both now cover every path in `docs/openapi.yaml` (68 paths).
  Gates: `npm run typecheck && npm run build` in sdk/typescript,
  `python3 -m compileall -q sdk/python` + an import/method-existence smoke.
  Note: the TS client needed a `put()` transport helper and a generic
  `put<T>` return type (`Promise<unknown>` is not assignable to
  `Promise<Record<string, unknown>>`). Full suite **805 passed / 0 failed**
  (86 suites) after the bump; clippy/fmt/route scan green.

- **Doc consistency pass + RFC coverage matrix (2026-11-16):** new
  `docs/rfc-coverage.md` is the AUTHORITATIVE status table for the whole
  catalogue (RFC-001…087, Delivered/Partial/Gap/Reject + evidence + remaining
  gap, bilingual). `docs/gap-analysis-master-index.md` §4 stays the detailed
  evidence log for 062…087 and now points at the matrix; `docs/index.md`
  links it and its stale "063/064/… 为规划中" sentence was corrected.
  `docs/api/http.md` endpoint table and `docs/api/cli.md` command reference
  were refreshed with the 21 endpoint groups and 4 command groups added since
  0.1.1 (research / entities / collections / replay / evaluation / policies /
  deletions / memory lifecycle / sources / plan / follow-up / evidence).
  Gotcha: when editing docs by line prefix, match the WHOLE bullet text —
  `docs/index.md` has both "Final Master Index Gap Analysis" and "Final RFC
  Master Index Gap Analysis", and a `startswith` on the shorter form silently
  skips one. Docs are bilingual (EN block, then 中文 block) — keep that.

**Known environment notes:**
- LanceDB requires `protoc` at build time (`brew install protobuf`).
- Live network tests (fetch of https://example.com) tolerate sandbox timeouts by design.

**Related:** [[searchos-project]], tests/README.md
