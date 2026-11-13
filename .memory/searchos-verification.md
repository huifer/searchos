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

**Known environment notes:**
- LanceDB requires `protoc` at build time (`brew install protobuf`).
- Live network tests (fetch of https://example.com) tolerate sandbox timeouts by design.

**Related:** [[searchos-project]], tests/README.md
