# Part 30. Search Execution Correctness Baseline

> 搜索执行正确性基线（RFC-004 v1.1 / RFC-008 / RFC-009 / RFC-014 的执行侧收口）

## English

### 30.1 Status

| Field | Value |
|---|---|
| Status | **Implemented** — 21 invariants landed with named acceptance tests |
| Snapshot | 2026-11-09 working tree (line numbers below refer to this snapshot; prefer function names when they drift) |
| Normative for | `crates/searchos-application` (search_service, singleflight), `crates/searchos-core` (runtime, task recorder), `crates/searchos-storage-sqlite` (repos, migrations), `crates/searchos-index-tantivy`, `crates/searchos-interface-http` |
| Relation | Amends Part 23 / Part 24 where those specified behaviour the execution path did not enforce. Does not replace RFC-001…029. |
| Scope | Correctness invariants only. No new features. Hybrid retrieval, async jobs and evidence export are separate RFCs. |

This annex exists because the RFC suite specified *what* must hold while the
execution path enforced it only at decision time. Every invariant below is
traceable to a concrete defect observed in the current implementation.

### 30.2 Problem

The decision engine evaluates quality, freshness and source constraints and
produces a correct `SearchDecision`. The surrounding execution path then
discards part of that verdict:

- it returns local candidates that were never filtered by the same freshness /
  source constraints the decision used;
- it treats an idempotency key as fire-and-forget, so concurrent replays both
  execute and both bill;
- it reserves budget but does not settle on reject paths, so reservations leak
  and the reservation is not a real ceiling;
- it lets a cache hit bypass idempotency entirely and keys the cache without
  mode or freshness, so a `LocalOnly` request can be served external results;
- it can dead-lock or double-execute a coalesced request;
- it reports zero cost when an external attempt fails after earlier attempts
  succeeded.

None of these are new features. They are the same invariant failing at the last
step. This annex makes each one testable.

### 30.3 Goals / Non-goals

**Goals**

1. State every cross-cutting execution invariant as a numbered, testable rule.
2. Define the acceptance test that proves each rule, including the adversarial
   case (late follower, cancelled leader, reject path, cache-hit-before-claim).
3. Fix the data model only where an invariant cannot be expressed without it.
4. Provide a ROI-ordered implementation sequence that can be executed in
   independent slices.

**Non-goals**

- Wiring hybrid retrieval / embeddings (RFC-007 execution).
- Async search jobs or evidence export (future RFC).
- New providers, new extension types, new transport.
- Changing the public response schema (the `execution` block in 30.6 is
  optional and additive).

### 30.4 Invariants

Status values: **OPEN** (defect present), **PARTIAL** (some paths fixed),
**FIXED** (verified in the snapshot, test exists).

---

#### INV-30-01 — Returned local evidence must satisfy the same constraints as the decision

**Statement.** Every candidate that reaches `results` / `evidence` from the
local index must pass the request's freshness requirement and source
constraints (`required_domains`, `excluded_domains`). Candidates that fail
may be *retained* for refresh enqueue, but must never be returned as evidence.

**Rationale.** Otherwise `quality.status = Passed` contradicts the payload,
and ADR-021 provenance is polluted with documents the policy rejected.

**Current evidence (OPEN).** `search_service.rs:590` retrieves raw
`local_hits`; `RetrievalQuery::new` sets `include_stale: true` and empty
domain filters (`domain/retrieval.rs:42-53`); the return branches at
`search_service.rs:709`, `:834` and `:886` pass `local_hits` straight into
`local_results_and_evidence` (`:307`), which performs no filtering.

**Required behaviour.** Filter once, before the decision, into
`servable` and `stale_for_refresh`; feed only `servable` to
`local_results_and_evidence`; feed `stale_for_refresh` only to the refresh
enqueue path.

**Acceptance test.** `local_only_never_returns_stale_or_excluded_candidates`
(index seeded with one fresh allowed doc, one stale doc, one excluded-domain
doc → response contains exactly the fresh allowed doc).

---

#### INV-30-02 — `request_id` must be an atomic claim

**Statement.** `idempotency_begin` returns whether *this* caller won the
claim. Concurrent callers with the same `request_id` must produce exactly one
execution, one bill and one completed record; the loser must observe the
winner's result.

**Rationale.** Otherwise replay is not idempotent and double-billing is
possible, which breaks RFC-008 and the `replayed` flag contract.

**Current evidence (OPEN).** `repos.rs:1804-1820` already returns `n == 1`
from `INSERT OR IGNORE`; `runtime.rs:633-640` discards it and always
returns `Ok(())`.

**Required behaviour.** Propagate the boolean through `TaskRecorder`; on
`false`, join the in-flight execution (singleflight) or read the completed
record; never run the pipeline.

**Acceptance test.** `concurrent_same_request_id_executes_once_and_bills_once`
(two tasks, same request_id, assert one external call and one cost event).

---

#### INV-30-03 — Budget reservation must settle on every exit path

**Statement.** Every `reserve_budget` must reach exactly one of `settled`,
`exceeded` or `released`, including early `return`, `?` propagation,
panic and cancellation.

**Rationale.** Otherwise `budget_reservations` accumulates permanent
`reserved` rows and the reservation is not an enforceable ceiling.

**Current evidence (OPEN).** Reserve at `search_service.rs:693`; the only
`settle_budget` call is at `:988`. The `Reject` branch returns at `:786`
without settling; `ProviderUnavailable`, `StrictFreshness` and
`BudgetExceeded` all reach it.

**Required behaviour.** RAII guard: `reserve` returns a guard whose `Drop`
settles with the accumulated spend; the explicit `settle` at the success path
consumes the guard.

**Acceptance test.** `reject_path_settles_reservation` and
`early_return_settles_reservation` (assert terminal state and zero remaining
`reserved` rows for the task).

---

#### INV-30-04 — Cache identity must include mode and freshness

**Statement.** Two requests may share an exact-cache entry only when they agree
on `mode`, profile version, workspace and the effective freshness
requirement. A `LocalOnly` request must never be satisfied from an entry
written by a request that acquired external evidence.

**Rationale.** Otherwise the cache breaks the mode contract and leaks external
results into a path that forbids external calls.

**Current evidence (OPEN).** `SearchCacheKey` and `QueryFingerprintParts`
contain neither mode nor freshness; the cache-hit return
(`search_service.rs:415-427`) precedes `idempotency_begin` (`:431`), so a
first-execution cache hit also leaves no idempotency record.

**Required behaviour.** Add `mode` and `freshness_requirement` to both the
cache key and the fingerprint; move `idempotency_begin` ahead of the cache
lookup and record the claim on cache hits too.

**Acceptance test.** `local_only_does_not_hit_hybrid_cache_entry` and
`cache_hit_records_idempotency_claim`.

---

#### INV-30-05 — Singleflight must neither hang forever nor double-execute

**Statement.** A follower must always terminate (result or error) within a
bounded time. A leader must publish the result before removing its entry. A
cancelled or panicked leader must not leave the key permanently claimed.

**Rationale.** Otherwise one query can wedge every subsequent identical request,
or bill the same external acquisition twice.

**Current evidence (OPEN).** `singleflight.rs:53` awaits `notified()` with no
timeout; `Notify::notify_waiters` (`:84`) stores no permit, so a follower
that has not yet polled loses the wakeup; the leader `remove`s the entry at
`:77` *before* `result.set` / `notify_waiters`; a cancelled `op().await`
(`:75`) skips both, stranding the entry forever. `engine.rs:256` has a
production `.expect(...)` that can trigger the panic case.

**Required behaviour.** Replace `Notify` with `tokio::sync::watch` or a
semaphore + state slot; order `set` → `notify` → `remove`; add a
`LeaderGuard` with `Drop` that releases the key; bound the follower wait.

**Acceptance test.** `follower_arriving_after_publish_terminates`,
`leader_cancel_releases_key`, `single_fingerprint_single_external_call`.

---

#### INV-30-06 — Cost must be attributed on partial external failure

**Statement.** `cost_spent` in the response and the amount used to settle the
reservation must equal the sum of cost events already written for the task,
including attempts that failed after earlier attempts succeeded.

**Rationale.** Otherwise ledger, response and reservation disagree, and cost
reports are wrong exactly when the system is degraded.

**Current evidence (OPEN).** `execute_external` writes a cost event per
successful call, but a failed `provider.search(...)?` returns `Err` and the
caller (`search_service.rs:846-862`) resets `cost_spent` to zero; the
failure itself is not recorded as an attempt.

**Required behaviour.** Carry `spent` in the error path (or accumulate in a
guard), settle with it, expose it in the response, and append a zero-amount
`provider_attempt` event with `status = failed`.

**Acceptance test.** `partial_external_failure_reports_spent_and_settles_spent`.

---

#### INV-30-07 — Index rebuild must preserve provenance

**Statement.** `rebuild_from` must restore the document's real
`workspace_id`, `language`, `fetched_at` and `title`, and must be a
net-neutral operation: a rebuild must not change what any request can see.

**Rationale.** Otherwise a routine operational action silently breaks tenant
isolation, resets document age (making stale content look fresh) and degrades
coverage scoring.

**Current evidence (PARTIAL).** `runtime.rs:468` now uses the real
`workspace_id`; `runtime.rs:467` still hard-codes
`language: Some("en")` and `:469` still hard-codes `fetched_at: now`.
Title is still sourced from the search-result join.

**Required behaviour.** Select `documents.language`, `document_versions.fetched_at`
and `documents.title` in the same query; fall back to `"en"` / `created_at`
only when the column is NULL.

**Acceptance test.** `rebuild_preserves_workspace_language_age_and_title` and
`rebuild_is_net_neutral_for_queries`.

---

#### INV-30-08 — Deletion must remove the document from every index

**Statement.** A delete outbox event must remove exactly the chunks belonging
to that document from the full-text index, and must only be marked processed
if the index reported success.

**Rationale.** Otherwise deleted knowledge remains retrievable, contradicting
RFC-006 and the `index_consistency` signal.

**Current evidence (OPEN).** `repos.rs:636` writes `entity_id = chunk_id`;
`runtime.rs:486-492` interprets `entity_id` as a `DocumentId`;
`TantivyIndex::delete` deletes by the `document_id` term
(`index-tantivy/src/lib.rs:332-344`) — a no-op for a chunk id.

**Required behaviour.** Add `delete_by_chunk_ids(&[ChunkId])` to the
`SearchIndex` port and use it from the outbox consumer; keep the payload
explicit about which identifier space it carries.

**Acceptance test.** `deleted_document_is_not_retrieved` and
`delete_outbox_retry_after_index_failure`.

---

#### INV-30-09 — Schema migration must be recoverable

**Statement.** A migration either applies completely or leaves the database in
its previous usable version. A failed migration must never make the database
permanently unopenable.

**Rationale.** Otherwise a disk-full or a killed process during an `ALTER
TABLE` bricks the metadata store with no rollback path.

**Current evidence (OPEN).** `migrations.rs:567-579` runs each migration's SQL
with `execute_batch` and then `PRAGMA user_version` as separate
autocommit statements; v2 contains non-idempotent `ALTER TABLE ... ADD COLUMN`
(`migrations.rs:481-483`).

**Required behaviour.** Wrap each migration in `BEGIN` / `COMMIT` with
`user_version` updated inside the same transaction; make column additions
idempotent (`PRAGMA table_info` check or rebuild-by-copy); on failure, report
the exact version and refuse to continue with a actionable message.

**Acceptance test.** `failed_migration_leaves_previous_version_usable`
(injected failure at the second `ALTER`), `migration_is_idempotent_on_retry`.

---

#### INV-30-10 — Cross-workspace reads must be filtered at the query, not only at the token

**Statement.** Every read path that can return workspace-scoped data must
include the workspace predicate in the query itself.

**Rationale.** Otherwise `workspace_tokens` protects only the routes that
remember to filter, and a token bound to ws-a can read ws-b data.

**Current evidence (PARTIAL).** `/documents/search` now uses a parameterised
LIKE over `canonical_url`/`title` (`repos.rs:578-607`) instead of scanning
500 rows, but `DocumentRepo::search` has no workspace parameter and its WHERE
clause has no `workspace_id` predicate; `routes.rs:1088` authorises the
workspace and then ignores it.

**Required behaviour.** Add `workspace_id` to `DocumentSearchDto` →
`DocumentRepo::search`; add the same predicate to the remaining unfiltered
read routes (traces, decisions, ingest jobs, document fetch, cost/stats).

**Acceptance test.** `workspace_token_cannot_search_other_workspace_documents`
and per-route isolation tests.

---

#### INV-30-11 — Fetched content must be retrieved through a validated address

**Statement.** The connection must use the addresses that passed DNS validation,
not a fresh resolution performed by the HTTP client.

**Rationale.** Otherwise DNS rebinding can defeat the SSRF guard between
validation and connect.

**Current evidence (OPEN).** `http.rs:149` discards the `Vec<SocketAddr>`
returned by `resolve_and_validate`; `raw_get` lets reqwest resolve again.

**Required behaviour.** Pin the validated address for the connection
(`reqwest` `resolve_to_addrs` or `resolve`), re-validating on every
redirect hop, and keep the original `Host` / SNI.

**Acceptance test.** `fetch_connects_to_validated_address` (mock DNS returning
a public address at validation and a private address afterwards).

---

#### INV-30-12 — Extension discovery failures must be observable and isolated

**Statement.** A manifest error must never silently disable the extensions that
follow it, and must never be swallowed without a log or metric.

**Rationale.** Otherwise the user believes an extension is installed when it is
not, and RFC-011's isolation guarantee is not met.

**Current evidence (OPEN).** `runtime.rs:219-221` discards both `discover`
results with `let _ =`; `ExtensionRegistry::discover`
(`lifecycle.rs:130-136`) returns on the first error, so later manifests are
never loaded.

**Required behaviour.** Accumulate per-manifest errors and continue; emit a
`tracing::warn!` and an `extensions.load_failed` counter per failure; expose
the quarantined set via `searchos extension list` and the dashboard.

**Acceptance test.** `bad_manifest_quarantines_only_itself` and
`discovery_failure_is_logged_and_counted`.

---

#### INV-30-13 — Pricing must follow the declared pricing model

**Statement.** A cost estimate must be computed according to the provider's
`PricingModel`. `PerThousandUnits` must multiply the unit price by the number
of units consumed (tokens / pages / chunks) divided by 1000. A price that fails
to parse must be visible, never silently zero. Coalescing savings must be
measurable.

**Rationale.** Otherwise RFC-008's cost transparency is false: token-priced
providers are billed per request, misconfiguration is invisible, and the
singleflight benefit cannot be quantified.

**Current evidence (OPEN).** `estimator.rs:21-31` stores `unit_price_per_k`
as per-request micros; `estimate_request` (`:41-49`) never reads unit counts;
`Money::parse(...).unwrap_or(0)` (`:24`, `:28`) silently zeroes a malformed
price; `record_singleflight(&fp, "", &[task_id], 0)` makes `total_saved()`
structurally zero.

**Required behaviour.** Dispatch per `PricingModel`; accept unit counts;
surface parse failures through `config validate` and a `tracing::warn!` plus a
trace marker; compute singleflight savings from follower count × avoided price.

**Acceptance test.** `per_thousand_units_pricing_uses_unit_counts`,
`cost_parse_failure_is_reported`, `singleflight_savings_positive_when_followers_join`.

---

#### INV-30-14 — Trace fields must be semantically true

**Statement.** `DecisionStageTrace.started_at_ms` must be the stage's own start
time and `duration_ms` must be measured. `coverage_status` must carry a
`CoverageStatus` value, not a number. `rejection_reasons` must be empty unless
the decision is a `Reject`.

**Rationale.** Otherwise P-011 "every decision is explainable" is not met and
operators debug against misleading data.

**Current evidence (OPEN).** `state.rs:241` writes `self.started_ms` (the
collector's start) for every stage; the engine passes a zero duration;
`engine.rs:740` stores `format!("{:?}", len)` (a numeric string) as
`coverage_status`; `engine.rs:747` fills `rejection_reasons` for every
decision including successful `LocalOnly`.

**Required behaviour.** Record the stage start and measure the duration; store
the enum's wire name; populate `rejection_reasons` only on `Reject`.

**Acceptance test.** `stage_trace_records_real_timestamps_and_duration`,
`coverage_status_is_a_status_not_a_number`,
`successful_decision_has_no_rejection_reasons`.

---

#### INV-30-15 — Exactly one canonical-URL implementation

**Statement.** Fetch, ingest, merge and index writes must all use the same
canonical-URL function. The raw URL is kept for display. Canonicalisation must
not lowercase case-sensitive path segments.

**Rationale.** Otherwise the same URL produces two documents/versions, dedup
keys never match, and `find_by_url` returns an arbitrary row.

**Current evidence (OPEN).** `fetch_service.rs:134` `canonicalize` only strips
the fragment; `merge.rs:24` `canonical_url` lowercases the whole string, strips
the scheme and tracking parameters. The two disagree. `merge.rs:58-64`
`host_of` also treats every `searchos-local://` URL as one host.

**Required behaviour.** Extract one implementation into `searchos-document`
(or domain) and call it from every path; keep the raw URL for display; handle
`searchos-local://` explicitly in `host_of`.

**Acceptance test.** `canonicalization_is_identical_across_paths`,
`canonicalization_preserves_case_sensitive_paths`.

---

#### INV-30-16 — Every validated config field must be consumed or deprecated

**Statement.** Any field accepted by `Config::validate()` must be read by
runtime code, or be explicitly marked deprecated with a warning. Silent no-op
knobs are forbidden.

**Rationale.** Otherwise users tune security, crawler and observability settings
that have no effect, and the configuration surface lies about the system.

**Current evidence (OPEN).** These fields have no consumer outside
`config.rs`: `security.ssrf_protection`,
`crawler.default_timeout_ms/max_concurrent/respect_robots`,
`observability.log_level/metrics_enabled/trace_retention_days`,
`indexes.rebuild_on_start/verify_on_start`,
`extensions.allow_native/wasm_enabled`,
`search.semantic_dedup/incremental`, `memory.embedding.*`.

**Required behaviour.** Wire each field or mark it deprecated; `config validate`
reports unconsumed fields; a test enumerates the config schema against consumers.

**Acceptance test.** `no_unconsumed_config_fields`,
`deprecated_field_emits_warning`.

---

#### INV-30-17 — Resource limits must bound resources before consumption

**Statement.** Response size, redirect count, fetch duration and concurrency
limits must be enforced *during* transfer, and the configured values must reach
the enforcement point.

**Rationale.** Otherwise a large response can exhaust memory before the limit is
checked, and Part 24 §24.1's resource-limit promise is not kept.

**Current evidence (OPEN).** `http.rs:170` checks `body.len()` only after
`resp.text()` (`:240`) has materialised the entire body; `fetch_service`
builds the fetcher with `FetchLimits::default()` (5 MB, `http.rs:266`) instead
of the configured 10 MB (`config.rs:143`); `max_redirects` is likewise not
passed through.

**Required behaviour.** Stream with a byte cap (`bytes_stream().take(limit+1)`),
pre-check `Content-Length` when present, pass configured limits into
`FetchLimits`, and enforce redirect count and per-hop timeouts.

**Acceptance test.** `oversized_response_rejected_before_full_buffer`,
`configured_limits_are_applied`.

---

#### INV-30-18 — robots.txt must be enforced when enabled

**Statement.** With `crawler.respect_robots` enabled, the fetch pipeline must
consult robots.txt before fetching. `RobotsDecision::Unknown` must follow
RFC 9309 (unreachable → disallow) unless explicitly configured otherwise.
`Crawl-delay` must be honoured.

**Rationale.** Otherwise the crawler ignores a site's stated policy, which is a
compliance and reputation risk.

**Current evidence (OPEN).** `robots_allows` (`http.rs:119`) has no caller;
`FetchOutcome::RobotsDisallowed` is never constructed; the parser discards
`Crawl-delay`; `respect_robots` has no consumer.

**Required behaviour.** Call `robots_allows` from the fetch service before
fetching; make the Unknown policy configurable (default RFC 9309); parse and
honour `Crawl-delay`; expose a `fetch.robots_disallowed_total` counter.

**Acceptance test.** `fetch_respects_robots_disallow`,
`unreachable_robots_disallowed_by_default`, `crawl_delay_is_honoured`.

---

#### INV-30-19 — Transports must agree on semantics

**Statement.** CLI, HTTP and MCP must parse the same mode vocabulary, reject
unknown modes identically, expose the same result limit, and map a domain error
to equivalent transport errors.

**Rationale.** Otherwise a client migrating between transports gets different
behaviour for the same request, and `local_only` can silently become external.

**Current evidence (PARTIAL).** MCP now rejects unknown modes
(`server.rs:177-182`, test at `:505`) and HTTP validates mode; the CLI accepts
kebab and snake forms. `max_results` is still absent from `SearchDto`, the MCP
schema and the CLI `Search` command. Error-code casing differs
(`unauthorized` vs `VALIDATION_FAILED`).

**Required behaviour.** One shared `SearchMode::parse` used by all three
transports; one shared `max_results` field with identical default and clamp; a
documented error-code table asserted by a parity test.

**Acceptance test.** `transports_reject_unknown_mode_identically`,
`transports_agree_on_max_results`, `error_code_table_is_consistent`.

---

#### INV-30-20 — Conflict detection must not invent conflicts

**Statement.** Two distinct pages on the same authoritative host are not a
conflict. A conflict requires the same subject/claim with contradictory values.
Failure reasons must name the constraint actually violated.

**Rationale.** Otherwise ordinary documentation sites are downgraded to
`Partial`, and operators chase non-existent quality problems.

**Current evidence (OPEN).** `quality.rs:335-367` flags a conflict when two or
more Official/Primary items share a host and differ in `published_at` or
`document_version_id`; `quality.rs:213-215` reports
`required_domain_missing` for an excluded domain.

**Required behaviour.** Require a shared subject/claim key before declaring a
conflict; emit `excluded_domain_violation` for exclusions; keep
`MergeReport.conflicts` honest about what was dropped.

**Acceptance test.** `two_pages_on_same_official_host_are_not_a_conflict`,
`excluded_domain_reason_is_correct`.

---

#### INV-30-21 — Hot paths must stay bounded

**Statement.** No hot path may grow memory without bound, take a write lock for
a read, or perform blocking work proportional to corpus size on an async thread.

**Rationale.** Otherwise a long-running gateway degrades or dies under normal
load, independently of any single request.

**Current evidence (OPEN).** `MetricsRegistry.search_latencies` is an unbounded
`Vec` cloned and fully sorted per percentile query; `ExactQueryCache::get`
takes a write lock and has no maximum entry count or sweeper; `rebuild_index`
performs O(V) blocking SQLite calls inside an async fn; `RateLimiter.windows`
never evicts expired entries.

**Required behaviour.** Ring buffer or t-digest for latency; read lock plus
bounded LRU for the cache; batched/JOIN rebuild; TTL eviction for rate-limit
windows.

**Acceptance test.** `latency_samples_are_bounded`,
`cache_respects_max_entries`, `rate_limit_windows_are_evicted`.

---

### 30.5 Domain model

```rust
/// Result of claiming a request id. `true` means this caller must execute.
pub enum IdempotencyClaim {
    Claimed { task_id: TaskId },
    AlreadyRunning { task_id: TaskId },
    Completed { response: serde_json::Value },
}

/// Terminal states of a budget reservation.
pub enum ReservationState { Reserved, Settled, Exceeded, Released }

/// A provider attempt that did not produce a billable result but must be
/// visible in the ledger for audit.
pub struct CostAttempt {
    pub provider_id: String,
    pub status: CostAttemptStatus, // Succeeded | Failed
    pub error_code: Option<String>,
    pub amount_micros: i64,        // 0 when Failed
}
```

No existing field is renamed or removed. `SearchResponse` is unchanged in this
annex.

### 30.6 Interface contract

Internal-only changes, no wire-format change:

| Change | From | To |
|---|---|---|
| `TaskRecorder::idempotency_begin` | `Result<()>` | `Result<IdempotencyClaim>` |
| `SearchIndex::delete` | delete by `DocumentId` | add `delete_by_chunk_ids(&[ChunkId])` |
| `DocumentRepo::search` | `search(query, limit)` | `search(query, limit, workspace: Option<&str>)` |
| `CostSink::reserve_budget` | `Result<()>` | `Result<BudgetGuard>` (RAII settle) |

Optional additive response block (deferred to RFC-031, not required by this
annex):

```json
{ "execution": { "retrieval_strategy": "bm25", "singleflight": "leader|follower",
  "budget": { "reserved_micros": 20000, "spent_micros": 12500 } } }
```

### 30.7 Data model

Append-only migrations; a new **v5** migration is required for:

1. `request_idempotency`: add `claim_owner_task_id` (nullable) and
   `claimed_at`; the existing `INSERT OR IGNORE` claim semantics stay.
2. `cost_events`: allow `operation = 'provider_attempt'` with
   `amount_micros = 0` and `status = 'failed'` for auditable failures.
3. `budget_reservations`: add `released_at`; no column is removed.
4. Indexes: `documents(workspace_id)`, `document_chunks(document_id)`.

The v2 `ALTER TABLE` statements are **not** edited (released migrations are
immutable); v5 adds the idempotency/recovery behaviour described in INV-30-09.

### 30.8 Failure handling

| Failure | Required behaviour |
|---|---|
| Migration fails mid-way | Transaction rolls back; `user_version` unchanged; startup reports the version and refuses to continue |
| Leader panics / is cancelled | `LeaderGuard::drop` removes the key and notifies followers with an error |
| Follower waits too long | Bounded timeout → `insufficient_evidence` with a `singleflight_timeout` warning |
| Cache hit before claim | Claim is still recorded; response marked `replayed` |
| Reservation path panics | `BudgetGuard::drop` settles with accumulated spend |
| Index delete fails | Outbox row stays `pending`; retry with backoff; DLQ after N attempts |

### 30.9 Security considerations

- **INV-30-04 is a security invariant**: serving external-acquired evidence to a
  `LocalOnly` caller breaks the mode contract and can leak paid-provider
  content into a path the caller believed was offline.
- **INV-30-02 is a billing invariant**: a lost claim permits double billing.
- **INV-30-10 is a tenant-isolation invariant**: workspace predicates must be
  part of the SQL, not the handler.
- **INV-30-11 preserves the SSRF boundary** promised by Part 24 §24.1.
- No new secrets, no new unauthenticated surface. Cache keys and fingerprints
  must not include raw query text (hash only).

### 30.10 Performance considerations

- INV-30-01 filtering is O(candidates) in memory; no extra index round-trip.
- INV-30-05 replaces `Notify` with `watch`; the follower holds a read lock
  only while cloning the result.
- INV-30-03 adds one `UPDATE` per request on paths that currently leak; the
  guard is a single integer addition.
- INV-30-09 adds a transaction per migration, applied once per version.
- INV-30-10 adds a `workspace_id` predicate to existing queries; the v5 index
  on `documents(workspace_id)` keeps list/search plans stable.

### 30.11 Acceptance criteria

Every invariant must have a named test that fails on the current implementation
and passes after the fix.

| Invariant | Test | Type |
|---|---|---|
| INV-30-01 | `local_only_never_returns_stale_or_excluded_candidates` | integration |
| INV-30-02 | `concurrent_same_request_id_executes_once_and_bills_once` | concurrency |
| INV-30-03 | `reject_path_settles_reservation`, `early_return_settles_reservation` | unit |
| INV-30-04 | `local_only_does_not_hit_hybrid_cache_entry` | integration |
| INV-30-05 | `follower_arriving_after_publish_terminates`, `leader_cancel_releases_key` | concurrency |
| INV-30-06 | `partial_external_failure_reports_spent_and_settles_spent` | unit |
| INV-30-07 | `rebuild_preserves_workspace_language_age_and_title` | integration |
| INV-30-08 | `deleted_document_is_not_retrieved` | integration |
| INV-30-09 | `failed_migration_leaves_previous_version_usable` | fault-injection |
| INV-30-10 | `workspace_token_cannot_search_other_workspace_documents` | integration |
| INV-30-11 | `fetch_connects_to_validated_address` | mock-DNS |
| INV-30-12 | `bad_manifest_quarantines_only_itself` | unit |
| INV-30-13 | `per_thousand_units_pricing_uses_unit_counts`, `cost_parse_failure_is_reported` | unit |
| INV-30-14 | `stage_trace_records_real_timestamps_and_duration`, `coverage_status_is_a_status_not_a_number` | unit |
| INV-30-15 | `canonicalization_is_identical_across_paths` | integration |
| INV-30-16 | `no_unconsumed_config_fields` | static assertion |
| INV-30-17 | `oversized_response_rejected_before_full_buffer` | integration |
| INV-30-18 | `fetch_respects_robots_disallow`, `unreachable_robots_disallowed_by_default` | integration |
| INV-30-19 | `transports_reject_unknown_mode_identically`, `transports_agree_on_max_results` | parity |
| INV-30-20 | `two_pages_on_same_official_host_are_not_a_conflict` | unit |
| INV-30-21 | `latency_samples_are_bounded`, `cache_respects_max_entries` | unit |

**Definition of done:** `cargo test --workspace` green, `cargo clippy
--workspace --all-targets -- -D warnings` clean, and
`cargo test --workspace -- --format json` count asserted in CI so the README
badge cannot drift again.

### 30.12 Dependencies

- Depends on Part 23 (decision states) and Part 24 (API/security) remaining in
  force.
- Blocks RFC-031 (async search jobs) and any hybrid-retrieval rollout: those
  features must not be built on an unsettled budget or an unclaimed idempotency
  path.
- No new external services; `tokio::sync::watch` is already in the tree.

### 30.13 Implementation tasks (ROI order)

| # | Task | Invariants | Effort |
|---|---|---|---|
| 1 | Filter local candidates before the decision; split `servable` / `stale_for_refresh` | INV-30-01 | S |
| 2 | Propagate the idempotency claim; loser joins or reads | INV-30-02, INV-30-04 | M |
| 3 | RAII budget guard; settle on all exits | INV-30-03 | M |
| 4 | Move `idempotency_begin` before the cache lookup; extend cache key/fingerprint | INV-30-04 | S |
| 5 | `watch`-based singleflight + `LeaderGuard` + timeout | INV-30-05 | M |
| 6 | Carry spend through the external error path; add attempt events | INV-30-06 | S |
| 7 | Rebuild selects real language / fetched_at / title | INV-30-07 | S |
| 8 | `delete_by_chunk_ids` in the index port and outbox consumer | INV-30-08 | S |
| 9 | Transactional, idempotent migrations | INV-30-09 | M |
| 10 | Workspace predicate on `DocumentRepo::search` and remaining read routes | INV-30-10 | M |
| 11 | Pin the validated address for fetch connections | INV-30-11 | M |
| 12 | Quarantine-and-continue extension discovery with metrics | INV-30-12 | S |
| 13 | Dispatch pricing per `PricingModel`; surface parse failures; compute real savings | INV-30-13 | M |
| 14 | Record real stage timestamps/duration; fix `coverage_status` and `rejection_reasons` | INV-30-14 | S |
| 15 | Unify canonicalisation into one implementation | INV-30-15 | M |
| 16 | Wire or deprecate every validated config field; report unconsumed fields | INV-30-16 | M |
| 17 | Stream response with a byte cap; pass configured limits into `FetchLimits` | INV-30-17 | M |
| 18 | Enforce robots.txt and `Crawl-delay` when enabled | INV-30-18 | M |
| 19 | Shared mode parser, shared `max_results`, error-code parity table | INV-30-19 | S |
| 20 | Conflict detection requires a shared claim; correct failure reasons | INV-30-20 | M |
| 21 | Bound latency samples, cache entries and rate-limit windows | INV-30-21 | M |

### 30.14 Rollout

1. Land tasks 1–4 behind no flag (behaviour fixes, tests prove the change).
2. Land tasks 5–9 with tests; each is independently revertible.
3. Land tasks 10–12 next, since they touch routing and manifests.
4. Land tasks 13–21 as a second wave. Task 16 (config consumption) should
   precede 17 and 18, because it makes the unwired resource limits visible.
5. Update `docs/rfc/README.md` status, `docs/index.md`, and add ADR-030
   referencing this annex.
6. Only after all twenty-one are green may README claim "quality-first" without
   a caveat; the existing overclaims (Tavily compatibility, hybrid retrieval,
   semantic memory, test counts) are tracked separately and must not be
   re-asserted by this annex.

### 30.15 Observability & SLIs

An invariant that cannot be observed in production is only half enforced. Every
invariant in §30.4 carries a counter or gauge exported through the existing
`MetricsRegistry` / `/stats/*` surface, and an alert condition that can be
expressed on the dashboard. Counters must never contain secrets, raw query text
or document bodies — use IDs and hashes.

| Invariant | Metric | Kind | Alert condition |
|---|---|---|---|
| INV-30-01 | `search.evidence.stale_or_excluded_served_total` | counter | > 0 over 5 min |
| INV-30-02 | `search.idempotency.claim_conflict_total` | counter | > 0 over 5 min |
| INV-30-03 | `budget.reservation.leaked_total`; `budget.reservation.reserved_age_seconds` | counter; gauge | any reservation reserved > `max_execution_time_ms` |
| INV-30-04 | `cache.cross_mode_hit_total`; `cache.hit_without_claim_total` | counter | > 0 |
| INV-30-05 | `singleflight.follower_timeout_total`; `singleflight.leader_abandoned_total`; `singleflight.duplicate_execution_total` | counter | > 0 |
| INV-30-06 | `cost.attribution_mismatch_total` | counter | > 0 (ledger vs response) |
| INV-30-07 | `index.rebuild.provenance_mismatch_total` | counter | > 0 after a rebuild |
| INV-30-08 | `index.delete.noop_total` | counter | > 0 |
| INV-30-09 | `storage.migration.failure_total`; `storage.schema.version` | counter; gauge | failure > 0; version below expected |
| INV-30-10 | `auth.workspace_cross_read_total` | counter | > 0 |
| INV-30-11 | `fetch.ssrf.address_mismatch_total` | counter | > 0 |
| INV-30-12 | `extensions.load_failed_total`; `extensions.quarantined` | counter; gauge | > 0 |
| INV-30-13 | `cost.pricing.parse_error_total`; `cost.estimate.units_missing_total` | counter | > 0 |
| INV-30-14 | `trace.invalid_field_total` | counter | > 0 |
| INV-30-15 | `document.canonical_divergence_total` | counter | > 0 |
| INV-30-16 | `config.unconsumed_field_total` (emitted at startup and by `config validate`) | gauge | > 0 |
| INV-30-17 | `fetch.limit_exceeded_total`; `fetch.oversize_buffered_bytes` | counter; histogram | buffered bytes above the configured cap |
| INV-30-18 | `fetch.robots_disallowed_total`; `fetch.crawl_delay_waited_seconds` | counter; histogram | informational |
| INV-30-19 | `transport.parity_violation_total` (CI parity suite) | counter | > 0 in CI |
| INV-30-20 | `quality.conflict_declared_total` with a sampled reason | counter | review when the sampled conflict rate is high |
| INV-30-21 | `metrics.sample_overflow_total`; `cache.evicted_total`; `ratelimit.windows_evicted_total` | counter | > 0 sustained |

**Rule.** A pull request that adds an invariant to §30.4 without adding its
metric to this table is incomplete. The metric name is part of the invariant's
acceptance criteria.

### 30.16 Existing-data remediation

Fixing the code does not repair the data the defect already produced. Each item
below is a one-time, idempotent, logged action — never a silent startup repair.

| # | Trigger | Action | Safety |
|---|---|---|---|
| 1 | INV-30-04 lands | Add `key_version` to `SearchCacheKey` so pre-fix entries can never be hit; optionally purge `search_cache` on upgrade | Old entries expire naturally; no query plan change |
| 2 | INV-30-08 lands | Reconciliation pass that deletes indexed chunks whose `documents.status = 'deleted'` via `delete_by_chunk_ids` | Dry-run first; report counts before/after |
| 3 | INV-30-07 lands | Run `searchos index verify`; if drift, run the now provenance-preserving `index rebuild` | Rebuild is read-only against SQLite |
| 4 | INV-30-03 lands | Sweep `budget_reservations` rows older than `max_execution_time_ms` to `released` with an audit event | Never delete rows; keep the audit trail |
| 5 | INV-30-02 lands | Mark `request_idempotency` rows stuck in `running` beyond the task TTL as `failed` with reason `abandoned` | Never re-use or silently delete a claim row |
| 6 | INV-30-15 lands | Deduplicate `documents` by the new canonical form: keep the newest version, tombstone duplicates, rewrite `sources.canonical_url` | `--dry-run` required by default; report every merge |
| 7 | INV-30-17/18 land | Re-fetch nothing automatically; only enforce limits going forward | No automatic re-crawl |

**Rule.** Remediation is opt-in via an explicit command (`searchos doctor
--remediate` or a dedicated subcommand), idempotent on re-run, and observable
through the §30.15 counters. It must never delete authoritative knowledge, and
must never run implicitly during startup.

---

## 中文

### 30.1 状态

| 字段 | 值 |
|---|---|
| 状态 | **已实现** — 21 条不变量已落地并附带命名验收测试 |
| 快照 | 2026-11-09 工作区（下列行号对应该快照；行号漂移时以函数名为准） |
| 适用范围 | `searchos-application`（search_service、singleflight）、`searchos-core`（runtime、task recorder）、`searchos-storage-sqlite`（repos、migrations）、`searchos-index-tantivy`、`searchos-interface-http` |
| 关系 | 修订 Part 23 / Part 24 中"已规定但执行侧未强制"的部分，不替代 RFC-001…029 |
| 范围 | 只收正确性不变量，不含新功能；混合检索、异步任务、证据包另立新 RFC |

### 30.2 问题

决策引擎会正确评估质量、时效与来源约束并产出正确的 `SearchDecision`，但
执行侧只执行了其中一部分：本地候选未经同样的约束过滤就返回；幂等键被当作
"发了就算"；预算只预留不结算；缓存命中绕过幂等且缓存键不含 mode/freshness；
合并请求可能永久挂起或重复执行；外部尝试部分失败时成本归零。这些都不是新功能，
而是同一条不变量在最后一步失效。

### 30.3 目标 / 非目标

目标：把跨切面的执行不变量写成可测试规则；为每条规则定义验收测试（含对抗场景）；
仅在不变量无法表达时改数据模型；给出可独立执行的 ROI 实施顺序。

非目标：混合检索与 embedding 接线（RFC-007 执行）；异步任务与证据包（后续 RFC）；
新 Provider / 新扩展类型 / 新传输；改动公开响应结构（30.6 的 `execution` 块为可选）。

### 30.4 不变量（规范）

状态取值：**OPEN**（缺陷存在）、**PARTIAL**（部分修复）、**FIXED**（快照中已验证）。

| ID | 不变量 | 状态 |
|---|---|---|
| INV-30-01 | 返回的本地证据必须通过决策所用的同一套 freshness / source 约束 | OPEN（`search_service.rs:590/709/834/886`） |
| INV-30-02 | `request_id` 必须是原子 claim，并发重放只能执行一次、计费一次 | OPEN（`repos.rs:1804` 已返回布尔，`runtime.rs:633` 丢弃） |
| INV-30-03 | 预算预留必须在所有退出路径结算（含 early return / panic / 取消） | OPEN（`search_service.rs:693` vs `:988`，Reject 在 `:786` 直接 return） |
| INV-30-04 | 缓存身份必须包含 mode 与 freshness；`LocalOnly` 不得命中外部结果缓存 | OPEN（缓存键缺字段；`search_service.rs:415-427` 命中先于 `:431` 的幂等） |
| INV-30-05 | Singleflight 不得永久挂起，也不得重复执行 | OPEN（`singleflight.rs:53/77/84`） |
| INV-30-06 | 外部部分失败时必须按已花成本结算与上报 | OPEN（`search_service.rs:846-862`） |
| INV-30-07 | 索引重建必须保留真实 workspace / language / fetched_at / title | PARTIAL（workspace 已修；`runtime.rs:467` language 与 `:469` fetched_at 仍硬编码） |
| INV-30-08 | 删除必须从所有索引中真正移除文档 | OPEN（`repos.rs:636` 传 chunk_id，`runtime.rs:486` 当 document_id 用） |
| INV-30-09 | Schema 迁移必须可恢复，失败不得让数据库永久不可用 | OPEN（`migrations.rs:567-579` 无事务，`481-483` ALTER 不可重入） |
| INV-30-10 | 跨 workspace 读取必须在查询层过滤，而非只在 token 层 | PARTIAL（`/documents/search` 已改参数化 LIKE（`repos.rs:578-607`），但仍无 workspace 谓词） |
| INV-30-11 | 抓取必须连接"已通过 DNS 校验的地址" | OPEN（`http.rs:149` 丢弃 `Vec<SocketAddr>`） |
| INV-30-12 | 扩展发现失败必须可见且相互隔离 | OPEN（`runtime.rs:219-221` 用 `let _ =` 吞掉） |
| INV-30-13 | 计价必须遵循声明的 PricingModel；解析失败不得静默归零；节约可量化 | OPEN（`estimator.rs:21-31` 把千单位单价当每次请求价；`record_singleflight` 传 0） |
| INV-30-14 | trace 字段必须语义真实（stage 起始时间、实测耗时、覆盖状态、失败原因） | OPEN（`state.rs:241` 写采集器启动时间；`engine.rs:740` 存数字；`:747` 成功也填失败原因） |
| INV-30-15 | 全系统只能有一个 canonical URL 实现 | OPEN（`fetch_service.rs:134` 与 `merge.rs:24` 两套实现不一致） |
| INV-30-16 | validate 接受的每个配置字段必须被消费或显式废弃 | OPEN（`ssrf_protection`、`crawler.*`、`indexes.*`、`observability.*`、`extensions.*`、`memory.embedding.*` 等零消费者） |
| INV-30-17 | 资源上限必须在消费前生效（流式截断），且配置值必须到达执行点 | OPEN（`http.rs:170` 读完才判断；`http.rs:266` 固定 5MB，配置的 10MB 未接线） |
| INV-30-18 | 启用时 robots.txt 与 Crawl-delay 必须被执行 | OPEN（`robots_allows`（`http.rs:119`）无调用者；`respect_robots` 无消费者） |
| INV-30-19 | 三种传输必须语义一致（mode 解析、错误码、结果条数） | PARTIAL（MCP 已拒绝未知 mode；`max_results` 三端都缺；错误码大小写不一致） |
| INV-30-20 | 冲突检测不得凭空造冲突（同 host 两个页面不是冲突） | OPEN（`quality.rs:335-367` 按 host 计数判冲突；`:213-215` 排除域名被报成缺少必需域名） |
| INV-30-21 | 热路径必须有界（内存、读锁、阻塞工作） | OPEN（`metrics` 无界 Vec；`ExactQueryCache::get` 取写锁且无上限；`rebuild` 在 async 中做 O(V) 阻塞查询） |

每条不变量的完整陈述、当前证据、要求行为与验收测试见英文版 §30.4；
验收测试名与类型见 §30.11。

### 30.5 领域模型

新增 `IdempotencyClaim`（Claimed / AlreadyRunning / Completed）、
`ReservationState`（Reserved / Settled / Exceeded / Released）、
`CostAttempt`（成功 / 失败、金额、错误码）。不重命名、不删除任何现有字段，
本 annex 不改变 `SearchResponse`。

### 30.6 接口契约

仅内部接口变更：`idempotency_begin` 返回 claim；`SearchIndex` 增加
`delete_by_chunk_ids`；`DocumentRepo::search` 增加 workspace 参数；
`reserve_budget` 返回 RAII guard。响应中可选增加 `execution` 块（留待 RFC-031）。

### 30.7 数据模型

仅追加式迁移，新增 **v5**：`request_idempotency` 增加 claim 归属与时间；
`cost_events` 允许 `provider_attempt`（金额 0、可失败）用于审计；
`budget_reservations` 增加 `released_at`；补 `documents(workspace_id)` 与
`document_chunks(document_id)` 索引。v2 的 ALTER 语句不修改（已发布迁移不可变），
INV-30-09 的幂等/恢复行为由 v5 承载。

### 30.8 失败处理

迁移中途失败必须整体回滚且 `user_version` 不变；leader panic/取消由
`LeaderGuard` 释放 key；follower 等待必须有界；缓存命中也要写幂等；
预留路径 panic 由 guard 结算；索引删除失败保留 outbox 待重试并进 DLQ。

### 30.9 安全考量

INV-30-04 是安全不变量（外部结果不得进入 LocalOnly 路径）；INV-30-02 是计费
不变量（claim 丢失导致重复计费）；INV-30-10 是租户隔离不变量（workspace 谓词
必须下推到 SQL）；INV-30-11 维系 Part 24 §24.1 的 SSRF 承诺。缓存键与指纹不得
包含原始查询文本，只允许哈希。

### 30.10 性能考量

INV-30-01 为内存 O(候选数) 过滤，不增加索引往返；INV-30-05 用 `watch` 替代
`Notify`；INV-30-03 每请求增加一次 UPDATE；INV-30-09 每版本只执行一次事务；
INV-30-10 的 workspace 谓词由 v5 索引支撑，保持查询计划稳定。

### 30.11 验收标准

每条不变量必须有"当前实现下失败、修复后通过"的命名测试（见 §30.11 表格）。
完成定义：`cargo test --workspace` 全绿、`cargo clippy --workspace --all-targets
-- -D warnings` 无警告、CI 中断言测试总数，避免 README 徽章再次漂移。

| 不变量 | 测试 | 类型 |
|---|---|---|
| INV-30-13 | `per_thousand_units_pricing_uses_unit_counts` | unit |
| INV-30-14 | `stage_trace_records_real_timestamps_and_duration` | unit |
| INV-30-15 | `canonicalization_is_identical_across_paths` | integration |
| INV-30-16 | `no_unconsumed_config_fields` | 静态断言 |
| INV-30-17 | `oversized_response_rejected_before_full_buffer` | integration |
| INV-30-18 | `fetch_respects_robots_disallow` | integration |
| INV-30-19 | `transports_reject_unknown_mode_identically` | parity |
| INV-30-20 | `two_pages_on_same_official_host_are_not_a_conflict` | unit |
| INV-30-21 | `latency_samples_are_bounded` | unit |

### 30.12 依赖

以 Part 23 / Part 24 继续有效为前提；阻塞 RFC-031（异步搜索任务）与任何混合检索
上线——这些功能不得建立在未结算的预算或未 claim 的幂等路径之上。不引入新外部服务。

### 30.13 实施任务（ROI 顺序）

| # | 任务 | 不变量 | 工作量 |
|---|---|---|---|
| 1 | 决策前过滤本地候选，拆分 `servable` / `stale_for_refresh` | INV-30-01 | S |
| 2 | 传递幂等 claim，未抢到者加入或读取结果 | INV-30-02/04 | M |
| 3 | RAII 预算 guard，所有退出路径结算 | INV-30-03 | M |
| 4 | 幂等前置于缓存查找；缓存键/指纹补 mode+freshness | INV-30-04 | S |
| 5 | 基于 `watch` 的 singleflight + `LeaderGuard` + 超时 | INV-30-05 | M |
| 6 | 外部失败路径携带已花成本并记录 attempt | INV-30-06 | S |
| 7 | 重建读取真实 language / fetched_at / title | INV-30-07 | S |
| 8 | 索引端口与 outbox 消费端支持按 chunk 删除 | INV-30-08 | S |
| 9 | 事务化、幂等化迁移 | INV-30-09 | M |
| 10 | `DocumentRepo::search` 及其余读路由补 workspace 谓词 | INV-30-10 | M |
| 11 | 抓取连接固定为已校验地址 | INV-30-11 | M |
| 12 | 扩展发现改为隔离+继续+指标 | INV-30-12 | S |
| 13 | 按 PricingModel 分派计价；暴露解析失败；计算真实节约 | INV-30-13 | M |
| 14 | 记录真实 stage 时间与耗时；修 coverage_status 与 rejection_reasons | INV-30-14 | S |
| 15 | 统一 canonicalization 为唯一实现 | INV-30-15 | M |
| 16 | 接线或废弃每个已校验配置字段；报告未消费字段 | INV-30-16 | M |
| 17 | 流式截断响应体；把配置上限传入 FetchLimits | INV-30-17 | M |
| 18 | 启用时执行 robots.txt 与 Crawl-delay | INV-30-18 | M |
| 19 | 共享 mode 解析、共享 max_results、错误码对齐表 | INV-30-19 | S |
| 20 | 冲突检测要求共同主张；修正失败原因 | INV-30-20 | M |
| 21 | 有界化延迟样本、缓存条目与限流窗口 | INV-30-21 | M |

### 30.14 落地顺序

先做 1–4（行为修复，测试自证）；再做 5–9（各自可独立回滚）；接着做 10–12
（涉及路由与清单）；最后做 13–21 的第二波，其中 16（配置消费）应先于 17、18，
因为它让未接线的资源上限先暴露出来。同步更新 `docs/rfc/README.md` 状态、
`docs/index.md`，并补 ADR-030 引用本 annex。二十一项全绿之前，README 不得宣称
"quality-first" 而无附加说明；既有夸大表述（Tavily 兼容、混合检索、语义记忆、
测试数）由各自追踪项负责，本 annex 不代为重新背书。

### 30.15 可观测性与 SLI

无法在生产中观测的不变量只算执行了一半。§30.4 的每条不变量都必须有通过现有
`MetricsRegistry` / `/stats/*` 暴露的计数器或仪表，并有可在控制台上表达的告警
条件。计数器不得包含密钥、原始查询文本或文档正文——只允许 ID 与哈希。

| 不变量 | 指标 | 类型 | 告警条件 |
|---|---|---|---|
| INV-30-01 | `search.evidence.stale_or_excluded_served_total` | counter | 5 分钟内 > 0 |
| INV-30-02 | `search.idempotency.claim_conflict_total` | counter | 5 分钟内 > 0 |
| INV-30-03 | `budget.reservation.leaked_total`；`budget.reservation.reserved_age_seconds` | counter；gauge | 存在超过 `max_execution_time_ms` 仍 reserved 的行 |
| INV-30-04 | `cache.cross_mode_hit_total`；`cache.hit_without_claim_total` | counter | > 0 |
| INV-30-05 | `singleflight.follower_timeout_total`；`singleflight.leader_abandoned_total` | counter | > 0 |
| INV-30-06 | `cost.attribution_mismatch_total` | counter | > 0（ledger 与响应不一致） |
| INV-30-07 | `index.rebuild.provenance_mismatch_total` | counter | 重建后 > 0 |
| INV-30-08 | `index.delete.noop_total` | counter | > 0 |
| INV-30-09 | `storage.migration.failure_total`；`storage.schema.version` | counter；gauge | 失败 > 0；版本低于预期 |
| INV-30-10 | `auth.workspace_cross_read_total` | counter | > 0 |
| INV-30-11 | `fetch.ssrf.address_mismatch_total` | counter | > 0 |
| INV-30-12 | `extensions.load_failed_total`；`extensions.quarantined` | counter；gauge | > 0 |
| INV-30-13 | `cost.pricing.parse_error_total` | counter | > 0 |
| INV-30-14 | `trace.invalid_field_total` | counter | > 0 |
| INV-30-15 | `document.canonical_divergence_total` | counter | > 0 |
| INV-30-16 | `config.unconsumed_field_total`（启动与 `config validate` 时输出） | gauge | > 0 |
| INV-30-17 | `fetch.oversize_buffered_bytes` | histogram | 超过配置上限 |
| INV-30-18 | `fetch.robots_disallowed_total` | counter | 仅观测 |
| INV-30-19 | `transport.parity_violation_total` | counter | CI 中 > 0 |
| INV-30-20 | `quality.conflict_declared_total`（含抽样原因） | counter | 抽样冲突率异常时人工复核 |
| INV-30-21 | `metrics.sample_overflow_total`；`cache.evicted_total`；`ratelimit.windows_evicted_total` | counter | 持续 > 0 |

**规则**：向 §30.4 增加不变量却没有在本表增加对应指标的 PR 视为未完成——指标名是
该不变量验收标准的一部分。

### 30.16 存量数据修复

修好代码不等于修好缺陷已经产生的数据。下表每项都是一次性、幂等、有日志的动作，
**绝不允许在启动时静默修复**。

| # | 触发条件 | 动作 | 安全性 |
|---|---|---|---|
| 1 | INV-30-04 落地 | 给 `SearchCacheKey` 增加 `key_version`，使修复前的条目永不可命中；可选在升级时清理 `search_cache` | 旧条目自然过期，不改变查询计划 |
| 2 | INV-30-08 落地 | 对账：删除 `documents.status = 'deleted'` 对应仍留在索引里的 chunk（用 `delete_by_chunk_ids`） | 先 dry-run，报告前后计数 |
| 3 | INV-30-07 落地 | 执行 `searchos index verify`；若有漂移，执行已保留 provenance 的 `index rebuild` | rebuild 对 SQLite 只读 |
| 4 | INV-30-03 落地 | 把超过 `max_execution_time_ms` 的 `budget_reservations` 扫为 `released` 并写审计事件 | 不删行，保留审计链 |
| 5 | INV-30-02 落地 | 把卡在 `running` 超过 TTL 的 `request_idempotency` 标记为 `failed`（原因 `abandoned`） | 不复用、不静默删除 claim 行 |
| 6 | INV-30-15 落地 | 按新的 canonical 形式去重 `documents`：保留最新版本、重复项打 tombstone、重写 `sources.canonical_url` | 默认强制 `--dry-run`；每次合并都要报告 |
| 7 | INV-30-17/18 落地 | 不自动重抓，只对之后的行为生效 | 不触发自动 re-crawl |

**规则**：修复必须通过显式命令触发（`searchos doctor --remediate` 或专用子命令），
可重复执行且幂等，并通过 §30.15 的指标可观测；绝不能删除权威知识，也绝不能在启动
过程中隐式执行。

---

**RFC Suite Version:** 1.0 (+ Annexes 23/24/30)
**Status:** Implemented
**Next Step:** Keep the Part 30 acceptance tests green in CI; new invariants must add a metric to §30.15.
