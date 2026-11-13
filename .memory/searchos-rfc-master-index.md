---
name: SearchOS final RFC Master Index (001–087)
description: The frozen architecture baseline that dissolves the earlier RFC-087…094 proposal and renumbers federated search to RFC-062
type: project-fact
---

The authoritative long-term RFC plan is the **final RFC Master Index** from
ChatGPT share `6af69118-d0f4-83ee-bbbb-948eeeb623c8` (83 linear nodes;
governing turns #50/#67/#69/#70/#71 and the complete 22,642-char #81). It is
documented bilingually in `docs/rfc/rfc-master-index.md`, with the repo
reconciliation in `docs/gap-analysis-master-index.md` and the federated-search
engineering baseline in `docs/rfc/rfc-062-federated-search-architecture.md`.

**Why:** the earlier share `6af5fcc9-...` (53 nodes) proposed a new v2.1 series
RFC-087…094. Turn #67 of the later share stops RFC authoring until the whole
catalogue is frozen, and #81 explicitly says *"087–094 不再重复创建，而是将其内容
合并到对应 RFC"*. Continuing with 087…094 would have produced two RFCs defining
the same Resource Model / Source Registry.

**How to apply:**
- Four frozen decisions: product scope **All-in-One**; extensions **Both**
  (Rust-native + external process); distributed **Staged** (single-node first);
  memory **Full Agent Memory** (Session / Project / Research / Operational).
  Positioning: *Agent Search & Memory Infrastructure*, not a Magpie rewrite.
- Final catalogue is **RFC-001…087** in 8 groups (A Foundation 001–028,
  B Knowledge/Research 029–045, C Retrieval Foundation 046–061,
  D Unified Search Intelligence 062–068, E Memory/Deep Research 069–074,
  F Cost/Provider 075–080, G Extension/Distributed 081–086, H Privacy 087).
- Dissolution map: 087→**062** Federated Search, 088→**063** Source Registry,
  089→**064** Unified Resource Model, 090→**066** Trust-aware Fusion,
  091→**067 Context Assembly + 068 Token Budget** (split),
  092→069+070, 093→071+072+073, 094→**087** Privacy (final #81 override).
- Ownership rule: one domain object, one owning RFC; 062 coordinates but owns
  no BM25/vector algorithm, 066 fuses but never generates answers, 076 decides
  but never calls provider SDKs. Do not add new RFC numbers — new requirements
  join existing RFCs.
- Priority is **not** RFC-number order: P0–P6 = Foundation → Retrieval →
  Intelligence → Memory → Research → Extension → Distributed; milestones M0–M8,
  with **M0–M3** the first shippable version. Every RFC needs 24 sections,
  explicit Ownership, testable acceptance criteria and MVP/Future split.
- **Three number collisions are recorded, not silently rewritten**: planned
  029 (Research Workspace) collides with the shipped `rfc-029-local-data-ingestion.md`;
  planned 031 (Freshness Scheduler) collides with the shipped
  `rfc-031-external-evidence-promotion.md`; planned 087 is Privacy while the
  superseded draft had put Federated Search at 087 — that file was renamed to
  `rfc-062-federated-search-architecture.md`. Keep shipped files/numbers intact.
- Engineering corrections that still hold: federation strategy is an orthogonal
  `FederationStrategy`, **never** a new `SearchMode` variant (would break
  `INV-30-18`, the frozen Tavily contract, dashboard `search_mode` pushdown and
  cache keys); extend `SearchResponse`/`SearchBudget` with `#[serde(default)]`
  instead of forking them; generalise `rrf_fuse` instead of rebuilding it;
  fail-open is never a quality pass.
- **Delivered (2026-11-13, implementation turn):**
  `crates/searchos-federated/` = RFC-062 kernel (`SearchSource` trait,
  `FederatedPlanner` with auditable rejection chain, `aggregate()` fail-open
  boundary) + RFC-063 (`InMemorySourceRegistry`, scope precedence) + RFC-064
  (`SearchResource`, fail-closed `PermissionRef`, egress default off) + RFC-062
  Phase 3 `FederatedExecutor` (per-source deadline, bounded `JoinSet` fan-out,
  cooperative cancellation, failure isolation) + Phase 2 adapters
  (`LocalIndexSource` over the active `SearchIndex`, `WebSearchProviderSource`
  over `SearchProvider`) — **56 tests** incl. named gates.
  `rrf_fuse_weighted` = RFC-066 slice (+9 tests, chunk-id tie-break for
  determinism); RFC-067/068 wired end to end:
  `SearchRequest.context_budget` → `build_response`/`assemble_context_block` →
  `SearchResponse.context`, HTTP `SearchDto` + MCP `apply_context_budget`
  (3 call sites) + CLI `--max-context-tokens` + OpenAPI `ContextBudget` /
  `ContextBlockSummary`; invalid budget = 400 at every edge.
  Full suite 660 passed / 0 failed. One new route: `GET /sources`.
  Specs: `rfc-063-source-registry.md`, `rfc-064-unified-resource-model.md`,
  `rfc-062-dependency-review.md` (adapter-level equivalence gate
  `legacy_pipeline_matches_federated_adapters` is delivered; pipeline-level
  `federated_default_matches_legacy_pipeline` remains the first test of the
  future `search_service.rs` wiring task).
  Design decisions to keep: planning-time early-stop belongs to `Sequential`
  only (Auto/Parallel emit runtime `StopCondition::RequirementsCovered`);
  a billable source that fills no open requirement is refused under EVERY mode
  (INV-30-04 generalised); `selected_source_ids()` preserves plan order —
  auditability needs order, use `source_set()` for membership; provider
  adapters never bill (reserve→execute→settle stays in the service), external
  candidates arrive with `permission_verified: false`, and local/external
  domain filtering is locked by `local_and_external_domain_policy_agree`.
- **Pipeline wiring (task 4, first half — delivered):** config section
  `federation.{strategy,max_sources,max_parallel_sources,source_timeout_ms}`
  consumed per INV-30-20; `legacy` default = zero behaviour change;
  `plan_only` adds the federated plan to `/search/plan` + a debug log in the
  pipeline (NEVER a warning — warnings change `derive_status` and would break
  the equivalence gate); `execute` is accepted but plans only WITH a warning
  until task 4b. Runtime registers local index + providers as sources;
  `/system/status` exposes the catalog. Pipeline-level gate
  `federated_default_matches_legacy_pipeline` (tests/integration/full_pipeline.rs)
  compares legacy vs plan_only on the assembled service; random evidence ids
  are excluded (identity, not content).
- **Task 4b delivered (2026-11-13):** when `federation.strategy = execute`,
  `execute_external` dispatches to `execute_external_federated` — the kernel
  executor runs ONLY the web sources (local index never re-purchased) with
  per-source deadline/isolation, and billing stays in the service: one
  WebSearch event per executed source, estimator-priced, spend returned
  through `AcquisitionState`. Gate:
  `federated_execute_mode_bills_once_and_matches_legacy_results` (same fake
  provider, legacy vs execute: item content equal URL-normalised — legacy
  fans out per QUERY and duplicates entries, federated per SOURCE and never
  re-buys — exactly one web_search event under execute, legacy ledger
  honestly shows 2 = the per-query re-buy this removes; estimator prices
  unknown provider ids at ZERO, so gate on event identity/count not amount).
  Pipeline gate `federated_default_matches_legacy_pipeline` also excludes
  fetched_at/retrieved_at timestamps: two runtimes seed milliseconds apart
  and a run straddling a second boundary fails spuriously (identity/runtime
  noise, not content).
- **P0 residual list CLOSED (2026-11-13, this turn):** tasks 5/7/8 were
  already delivered; this pass closed the last three residues —
  (5-tail) `SessionTranscriptSource` + `transcript_resource` (application
  layer, registered in runtime next to the memory source; workspace-scoped,
  non-billable, `ResourceType::Transcript`; only the requester's sessions are
  ever visible — 5 tests);
  (6) trust-weighted cross-source fusion WIRED: kernel-generic
  `searchos_federated::fuse_ranks` (4 tests) + service
  `order_federated_outcomes` ordering per-source outcomes by
  `ReliabilityProfile.success_rate` under `execute` — ordering ONLY (payload,
  billing, per-provider attribution untouched); single-source plans are a
  no-op; gates `reliability_weight_overrides_plan_order_in_federated_outcomes`,
  `federated_outcome_order_is_deterministic`,
  `single_source_outcomes_are_left_untouched`;
  (7) `FederationExecutionTrace` (planned/executed/failed/timed_out/
  not_executed/candidates) attached to `DecisionTrace` and re-recorded by the
  pipeline: migration **v9** `decision_traces.federation_json` (schema version
  is read from PRAGMA, no constant to bump), SQLite INSERT OR REPLACE means
  the re-record UPDATES the trace; `InMemoryTraceCollector.get` now returns
  the LAST record for an id to match; gates
  `federation_execution_summary_survives_trace_re_record` + the execute
  integration gate now asserts the summary (legacy traces stay `None`).
  Full suite **673 passed / 0 failed**; clippy `-D warnings`, fmt, route scan
  all green. OpenAPI: `DecisionTrace.federation_execution` +
  `FederationExecutionTrace` schema.
- **Lesson — the ORIGINAL warning was right for the naive attempt, wrong as a
  permanent ban:** calling FederatedExecutor AND the legacy loop is the
  double-billing bug; REPLACING the legacy loop for execute mode while
  keeping reservation/settlement in the service is the correct migration.
  Keep the dispatch inside `execute_external` so singleflight, negative
  cache, promotion and follow-up rounds all inherit federation transparently.
- **Lesson — estimator and unpriced providers:** `estimate_request` prices
  unknown provider ids at ZERO. Tests that assert billing amounts must
  register pricing or assert on event identity/count instead of dividing
  amounts (a /price arithmetic silently breaks).
- **Planner coverage semantics (keep):** `covered` = pre-covered + DECLARED
  (authoritative) claims only; generic adapters (no declared requirement ids)
  get optimistic task assignments but NEVER count as factual coverage, never
  trigger the Sequential stop, and never satisfy a REQUIRED gap. Declared
  fills dedupe against `covered`, generic fills against `covered` too — but a
  generic claim never vetoes a declared-authoritative source for the same gap.
  `generic_sources_can_answer` gates generic billable participation; Selective
  never treats a generic source as authoritative.

**Related:** [[searchos-project]], [[searchos-verification]], [[searchos-decision-red-lines]], docs/rfc/rfc-master-index.md
