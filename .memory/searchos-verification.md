---
name: SearchOS verification
description: Exact commands that verify the SearchOS implementation end-to-end
type: project-fact
---

The SearchOS implementation (`~/git_repo/searchos`) is verified by these commands (all green as of the implementation turn):

**Why:** goal-mode completion audits require reproducible, authoritative evidence — not conversation summaries.

**How to apply:**
- Unit + integration + suites: `cargo test --workspace` → 428 passed / 0 failed (62 suites) after the Part 30 execution-correctness pass; earlier baselines were 362, 345, 298, 295, 253 then 291. `cargo test --workspace --locked -- --list | grep -c ': test$'` = 428 and CI asserts no drift.
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
- CLI help gate: `searchos` with no args prints the full help and exits 0 (manual `Cli::command().print_help()` in `main()`); `--help`/`-h`/`help`/`help <cmd> <sub>` all exit 0. All 65 command paths have a `--help` Usage line and no empty subcommand/argument descriptions; `--format` renders a `Possible values:` section (clap ValueEnum doc comments). Long `--help` shows `long_about`; `-h` shows the compact summary. Unit tests `every_subcommand_has_a_description` / `every_argument_has_help` / `completions_generate_for_every_shell` lock the invariant.
- CLI basics gate: `searchos completions <bash|zsh|fish|powershell|elvish>` emits a script (clap_complete); `-v/-vv/-vvv` raises the log level (warn/info/debug/trace, RUST_LOG overrides); logs always go to **stderr** (`.with_writer(std::io::stderr)` in `logging.rs`, ANSI only on a TTY) so `--format json` stdout and MCP stdio stdout stay parseable; with `--format json` errors are emitted as `{"error": "...", "code": N}` on stderr.
- Dashboard API smoke: `/api/v1/dashboard/overview`, `/search/timeseries`, `/quality/dimensions`, `/export?format=csv`, `/events` (SSE), plus `GET /api/searchos/api/v1/dashboard/overview` through the Next.js proxy.
- Console gates: `cd web && npm ci && npm run typecheck && npm run build`; live pages `/`, `/search`, `/traces`, `/analytics`, `/cost`, `/knowledge` all 200; interactions browser-verified (see searchos-console memory).

- Release build: `cargo build --release --bin searchos` (15m09s, 0 errors/warnings, 273M binary); the release binary passes init/search/doctor/gateway smoke tests.
- Full clean rebuild verified: `cargo clean` (removed 59.1GiB) → `cargo build --workspace --all-targets` (575 crates, 5m55s, 0 errors/warnings) → `cargo test --workspace` (253 passed) → `cargo clippy --workspace --all-targets` (0 warnings) → `cargo fmt --all -- --check` clean.

**Known environment notes:**
- LanceDB requires `protoc` at build time (`brew install protobuf`).
- Live network tests (fetch of https://example.com) tolerate sandbox timeouts by design.

**Related:** [[searchos-project]], tests/README.md
