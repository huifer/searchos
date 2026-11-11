# RFC-031: External Evidence Promotion & Knowledge Compounding

> 外部证据沉淀与知识复利

**Architecture Baseline:** v1.1
**Document Status:** Proposed / Engineering Baseline
**Related:** RFC-004 (Search Decision Engine), RFC-005 (Quality Gate & Evidence
Evaluation), RFC-006 (Knowledge Store & Document Lifecycle), RFC-007
(Retrieval & Cache Architecture), RFC-009 (Execution Planner & Provider Router),
RFC-015 (Observability & Evaluation), RFC-027 (Analytics Data Model), RFC-029
(Local Data Ingestion), ADR-021 (Evidence Provenance), ADR-022 (Local Index
Rebuildability), ADR-023 (Workspace Isolation), ADR-032, ADR-033.

---

## English

### 1. Context

SearchOS is local-first: the decision engine evaluates local knowledge before
spending on an external provider, and `mode=local_only` forbids every external
call. The suite's own framing makes the intent explicit — *every external
search must leave knowledge that later tasks can reuse*.

Today that reuse exists, but only as a **temporary index entry**. When a task
buys an external answer, `search_service.rs` writes at most two result
snippets straight into the BM25 and vector indexes
(`crates/searchos-application/src/search_service.rs:1384`). Eight concrete
defects follow from that single shortcut, and each one is verifiable in the
current tree.

**D1 — Purchased knowledge is not a document.** The external write at
`search_service.rs:1384` generates a fresh `document_id` / `version_id` and
indexes an `IndexedDocument`. It creates no `documents`, `document_versions`
or `document_chunks` row, no object-store reference and no `sources` row.
`Runtime::rebuild_index` (`crates/searchos-core/src/runtime.rs:645`)
reconstructs the index from `documents` / `document_versions` /
`document_chunks` only. A rebuild, a backup restore or an index re-creation
therefore **deletes everything the system ever bought**. The knowledge base
does not compound — it evaporates on the next rebuild.

**D2 — Unverified evidence enters the index before the quality gate runs.**
The write happens at `search_service.rs:1384`; the final quality gate runs
later, at `search_service.rs:1489`. Evidence that the gate then rejects —
unresolved conflict, failed hard constraint, insufficient coverage — is
already in the index. The next query's local retrieval finds it and treats it
as first-party knowledge. This is knowledge pollution, and it is the single
most damaging defect in the current design.

**D3 — Only snippets are captured.** `execute_external` sets
`include_raw_content: false` (`search_service.rs:1709`). The provider trait
returns `content` (a short snippet) and an optional `raw_content`
(`crates/searchos-provider/src/traits.rs:45-54`), but SearchOS never asks for
raw content and never fetches the page body. Even if D1 were fixed, the
durable artefact would be one or two paragraphs. `GapAnalyzer` judges coverage
by token overlap (`crates/searchos-decision/src/gap.rs:78-126`), so a snippet
rarely registers as `Covered` for a differently-worded later query. The
purchase does not answer the next question.

**D4 — The purchase can stay invisible behind a stale cache.**
`SearchDependencies` owns `canonical_cache`, `semantic_cache` and
`retrieval_cache` (`search_service.rs:200-202`), and `Runtime::invalidate_caches`
exists (`runtime.rs:586`). The external write path calls neither. A query that
just paid for an answer can keep seeing the cached "local miss" for the
lifetime of the retrieval cache, so the compound effect is masked exactly when
it should kick in.

**D5 — Planned and executed external queries disagree.** `build_external_plan`
computes the targeted queries (`gap.rs:135`), then query expansion appends up
to `max_expansions` more (`search_service.rs:1316-1327`) — **after** the cost
estimate and the budget reservation were sized from the planned count (the
reservation is taken at `search_service.rs:1175-1177`). The ledger records more
`WebSearch` events than the reservation was sized for, and the trace cannot
explain why one gap produced several provider calls.

**D6 — URL identity is global, ownership is not.** `DocumentRepo::find_by_url`
(`crates/searchos-storage-sqlite/src/repos.rs:507`) matches on `canonical_url`
alone. A document first captured in workspace A is owned by A. Workspace B
capturing the same public URL either binds to A's row or creates a duplicate,
and there is no visibility concept to say which.

**D7 — Refresh storms.** `enqueue_refresh` (`repos.rs:2387`) inserts a pending
outbox row unconditionally. Concurrent stale hits for the same URL enqueue
duplicate fetches of the same page.

**D8 — Savings reporting cannot see the compound effect.**
`external_avoidance_rate` (`runtime.rs:1311`) counts *any* local resolution,
including one served from a previously purchased snippet. It cannot separate
"avoided an external call because we own the knowledge" from "avoided an
external call because we cached the purchase". The value of the loop is
invisible in the metrics.

D1–D8 are the reason the system is currently a **local-first search broker**
rather than the **knowledge intermediary** it is designed to be.

### 2. Goals

| ID | Goal |
|----|------|
| G1 | Every external acquisition becomes a durable, rebuildable knowledge object — or is explicitly not promoted, with a recorded reason. |
| G2 | Promotion is quality-gated: only evidence the final quality gate accepted may enter the durable store. Evidence the gate rejected never reaches the index. |
| G3 | Promotion captures the **page body** through the existing governed fetch path (canonical URL, robots, SSRF guard, versioning, chunking, outbox) — not a snippet. |
| G4 | Promotion is non-blocking: the user response never waits for a promotion fetch. |
| G5 | Promotion is auditable end-to-end: requirement → query → provider → document → version → reuse. |
| G6 | Knowledge visibility is explicit: workspace-private by default, with a binding model that permits authorised sharing later. |
| G7 | The compound effect is measurable: acquisition, promotion, rejection, skip and reuse counters, plus a reuse ratio. |
| G8 | Savings reporting is honest: local resolution is split into first-party and external-sourced. |
| G9 | The external plan and the executed queries agree, so the reservation matches the ledger. |

### 3. Non-goals

| ID | Non-goal |
|----|----------|
| N1 | Answer synthesis and claim-level citation. The suite already assigns those to the upper-layer agent (RFC suite README, "Search Result / Evidence Set / Answer"). This RFC produces the **evidence set**. |
| N2 | Replacing the default embedder. `HashingEmbedder` (`crates/searchos-provider/src/embedding.rs:34`) is a known weakness, but changing it before the store holds verified content would only recall unverified content more accurately. Deferred to a follow-up RFC with real data. |
| N3 | Touching the semantic-cache similarity gate (`SEMANTIC_REUSE_MIN_SIMILARITY = 0.92`) or the seven compatibility gates. |
| N4 | Relaxing strict-freshness, `ExternalOnly` or `ExternalFirst` semantics (RFC-004 v1.1 9.4/9.5). |
| N5 | Changing `mode` semantics or the decision rule ordering. |
| N6 | Distributed, multi-node or shared-cache design. |
| N7 | Crawler policy, site-specific crawlers or the extension system. |
| N8 | Autonomous site crawling or bulk harvesting. Promotion happens per acquisition, never speculatively. |
| N9 | Cross-workspace sharing by default. The RFC suite's baseline principle stands: *"默认 Workspace 隔离，显式授权后才共享，不应该默认全局共享."* |

### 4. Architecture

```text
                    ┌──────────────────────────────────────────┐
   request ────────▶│ local retrieval (BM25 + vector + RRF)      │
                    └───────────────┬──────────────────────────┘
                                    │ local candidates
                    ┌───────────────▼──────────────────────────┐
                    │ Decision Engine (RFC-004 v1.1)            │
                    └───────────────┬──────────────────────────┘
                                    │ gap / external plan
                    ┌───────────────▼──────────────────────────┐
                    │ External execution (provider calls)       │
                    └───────────────┬──────────────────────────┘
                                    │ evidence set
                    ┌───────────────▼──────────────────────────┐
                    │ FINAL QUALITY GATE (RFC-005)              │
                    │   accepted ────────┐                      │
                    │   rejected ────────┼──▶ never indexed    │
                    └───────────────────┼──────────────────────┘
                                        │ accepted external items only
                    ┌───────────────────▼──────────────────────┐
                    │ acquisition + evidence_items (SQLite)     │
                    │   state = candidate → evaluated           │
                    └───────────────────┬──────────────────────┘
                                        │ outbox: capture
                    ┌───────────────────▼──────────────────────┐
                    │ capture worker: FetchService             │
                    │   robots → SSRF guard → extract →        │
                    │   Document + Version + Chunks + Source   │
                    └───────────────────┬──────────────────────┘
                                        │ outbox: index
                    ┌───────────────────▼──────────────────────┐
                    │ SearchIndex (Tantivy | ES | Meili | PG |  │
                    │   MySQL) + vector index                   │
                    └───────────────────┬──────────────────────┘
                                        │ invalidate
                    ┌───────────────────▼──────────────────────┐
                    │ canonical / semantic / retrieval cache   │
                    └──────────────────────────────────────────┘
```

The critical change is the ordering. Today the index write sits **before**
the quality gate; in this RFC it sits **after** it, and it is fed by a
durable capture worker instead of an inline snippet write.

Promotion is **outbox-driven and asynchronous**: the task records the
acquisition and the accepted items, enqueues a capture event, and returns. The
gateway's existing 1.5 s outbox tick performs the fetch, the version write and
the index write. The user never waits on a promotion fetch.

### 5. Domain Model

#### 5.1 Promotion state machine

```text
              search returns item
                     │
                     ▼
              ┌─────────────┐
              │  candidate  │  body not captured, not indexed
              └──────┬──────┘
                     │ final gate evaluated this item
                     ▼
              ┌─────────────┐   gate rejected / unresolved conflict
              │  evaluated  ├──────────────────────▶ rejected
              └──────┬──────┘
                     │ gate accepted
                     ▼
              ┌─────────────┐   robots / empty / budget / size / policy
              │  promotable ├──────────────────────▶ skipped
              └──────┬──────┘
                     │ capture worker succeeded
                     ▼
              ┌─────────────┐
              │  promoted   │  document + version + chunks + index
              └─────────────┘
```

A **set-level** `Partial` verdict does not disqualify an individually accepted
item: "part of the query was not covered" is a statement about the query, not
about that evidence. But an item involved in an **unresolved conflict** is
`rejected`, because promoting it would put contradictory content into the
durable store.

#### 5.2 External acquisition record

```rust
pub struct ExternalAcquisition {
    pub id: String,
    pub task_id: String,
    pub request_id: String,
    pub workspace_id: String,
    pub provider_id: String,
    /// The exact query issued to the provider.
    pub query: String,
    /// Where the query came from — plan and ledger must agree.
    pub query_origin: QueryOrigin,
    /// The open information requirement this purchase targeted, if any.
    pub requirement_id: Option<String>,
    pub strategy: GapResolutionStrategy,
    pub created_at: DateTime<Utc>,
}

pub enum QueryOrigin {
    /// Built by `build_external_plan` for one open requirement.
    Gap,
    /// Added by `QueryExpander` because the requirement stayed open.
    Expansion,
    /// The raw user query, used when no plan could be built.
    Fallback,
}
```

#### 5.3 Evidence item

```rust
pub struct EvidenceItem {
    pub id: String,
    pub acquisition_id: String,
    pub url: String,
    pub canonical_url: String,
    pub title: Option<String>,
    /// Provider relevance as reported by the adapter.
    pub provider_score: f32,
    /// Relevance as measured by the quality gate.
    pub relevance: f32,
    pub state: PromotionState,
    /// Set once the capture worker succeeds.
    pub document_id: Option<String>,
    pub version_id: Option<String>,
    pub chunk_count: Option<usize>,
    /// Why the item was rejected or skipped.
    pub reason: Option<String>,
    pub promoted_at: Option<DateTime<Utc>>,
}
```

#### 5.4 Evidence origin — first-party vs external-sourced

Every local candidate carries an origin:

```rust
pub enum EvidenceOrigin {
    /// The user's own material: local ingestion, workspace documents.
    FirstParty,
    /// A page this system captured after buying a search that pointed to it.
    ExternalSourced,
}
```

An `ExternalSourced` candidate is **legal evidence** for a `local_only`
decision — that is precisely the compound effect. But it must be distinguishable
in the decision trace and in savings reporting, otherwise the system reports
itself as owning knowledge it bought. G8 splits local resolution into:

```text
local_resolution.first_party      ← we owned the knowledge
local_resolution.external_sourced  ← we reused a purchase
```

#### 5.5 Visibility and bindings

```text
visibility = workspace (default)  → readable by bound workspaces only
visibility = public               → readable by every workspace (opt-in, off in v1)
```

`document_bindings` is a many-to-many table:

```sql
document_bindings(
    document_id,
    workspace_id,
    visibility,      -- 'workspace' | 'public'
    bound_at,
    last_used_at
)
```

In v1 promotion writes a binding for the acquiring workspace only
(`knowledge.public_web_knowledge = false`, default). The table exists so that
authorised cross-workspace sharing can be enabled later **without a
migration**. This is consistent with the retrieval rule that already exists in
the store — `requester OR '*'`
(`repos.rs:639`, `repos.rs:697`, `repos.rs:853`, and the same predicate in the
PostgreSQL / MySQL remote backends) — and with ADR-023's default of workspace
isolation.

#### 5.6 Reuse accounting

```sql
evidence_reuse_events(
    id, task_id, workspace_id, document_id, origin, occurred_at
)
```

A row is written whenever a local resolution serves an `ExternalSourced`
document. This is the denominator of the compound metric: how many future
tasks did one purchase actually serve?

### 6. Interface Contract

#### 6.1 Application ports

```rust
#[async_trait]
pub trait EvidencePromotionService: Send + Sync {
    /// Persist the acquisition and every item, then queue capture for the
    /// items the final gate accepted.
    async fn record_and_queue(
        &self,
        acquisition: ExternalAcquisition,
        items: Vec<EvidenceItem>,
        accepted_ids: &[String],
    ) -> Result<(), SearchOsError>;

    /// Capture worker entry point — performs the governed fetch and writes
    /// the document/version/chunks.
    async fn capture(&self, item_id: &str) -> Result<(), SearchOsError>;
}

/// Invalidates query/retrieval caches after a knowledge mutation.
pub trait CacheInvalidator: Send + Sync {
    fn invalidate_after_promotion(&self, workspace_id: &str, canonical_url: &str);
}
```

`CacheInvalidator` is implemented by `Runtime` and delegates to the existing
`invalidate_caches` (`runtime.rs:586`). The application layer owns the port,
not the concrete cache types — the same decoupling used everywhere else.

#### 6.2 HTTP (Gateway Mode, read-only observability)

| Method | Path | Notes |
|--------|------|-------|
| `GET` | `/evidence/acquisitions?workspace_id=&limit=` | Acquisition list with promotion state and counts. |
| `GET` | `/evidence/acquisitions/:id` | Acquisition + its items, with rejection / skip reasons. |
| `GET` | `/evidence/reuse?workspace_id=` | Compound report: acquisitions, promoted, rejected, skipped, reuse events, reuse ratio. |

No new write endpoints are added: promotion is an internal consequence of a
search, never a caller-driven operation.

#### 6.3 CLI (Local Mode, in-process)

```bash
searchos evidence list --workspace default [--limit 50]
searchos evidence stats --workspace default
```

#### 6.4 Configuration

```yaml
knowledge:
  promotion_enabled: true
  # How many accepted external items may be captured per task.
  max_promotions_per_task: 2
  # Hard byte bound for captured bodies per task.
  max_promoted_bytes_per_task: 10485760
  # Capture the snippet when the page body cannot be fetched.
  # Default false: a snippet is not knowledge.
  promote_snippet_fallback: false
  # Promote only items the final quality gate accepted.
  require_final_gate_pass: true
  # Cross-workspace public sharing. Off in v1 (ADR-023).
  public_web_knowledge: false
```

#### 6.5 Metrics

New Prometheus series, following the existing rule that **status counts are the
only labelled series** — therefore one series per state, not one labelled
series:

```text
searchos_external_acquisitions_total
searchos_external_promotions_promoted_total
searchos_external_promotions_rejected_total
searchos_external_promotions_skipped_total
searchos_external_promotion_fetches_total
searchos_external_evidence_reuse_total
searchos_refresh_coalesced_total
```

`searchos_external_evidence_reuse_total / searchos_external_acquisitions_total`
is the **compound ratio**: the average number of future tasks served per
purchase.

### 7. Data Model

Migration **v6** (`external_evidence_promotion`) appends. Released migrations
are immutable (RFC-006 6.7).

```sql
-- One row per external acquisition.
CREATE TABLE IF NOT EXISTS external_acquisitions (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    query TEXT NOT NULL,
    query_origin TEXT NOT NULL,       -- gap | expansion | fallback
    requirement_id TEXT,
    strategy TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_acquisitions_ws
    ON external_acquisitions(workspace_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_acquisitions_task
    ON external_acquisitions(task_id);

-- One row per result item of an acquisition.
CREATE TABLE IF NOT EXISTS evidence_items (
    id TEXT PRIMARY KEY,
    acquisition_id TEXT NOT NULL,
    url TEXT NOT NULL,
    canonical_url TEXT NOT NULL,
    title TEXT,
    provider_score REAL NOT NULL DEFAULT 0.0,
    relevance REAL NOT NULL DEFAULT 0.0,
    state TEXT NOT NULL,              -- candidate | evaluated | promoted | rejected | skipped
    document_id TEXT,
    version_id TEXT,
    chunk_count INTEGER,
    reason TEXT,
    promoted_at TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(acquisition_id) REFERENCES external_acquisitions(id)
);
CREATE INDEX IF NOT EXISTS idx_evidence_items_acq
    ON evidence_items(acquisition_id);
CREATE INDEX IF NOT EXISTS idx_evidence_items_state
    ON evidence_items(state, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_evidence_items_doc
    ON evidence_items(document_id);

-- Workspace ↔ document bindings (visibility model).
CREATE TABLE IF NOT EXISTS document_bindings (
    document_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'workspace',  -- workspace | public
    bound_at TEXT NOT NULL,
    last_used_at TEXT,
    PRIMARY KEY (document_id, workspace_id)
);
CREATE INDEX IF NOT EXISTS idx_bindings_ws
    ON document_bindings(workspace_id);

-- Reuse accounting (compound metric numerator).
CREATE TABLE IF NOT EXISTS evidence_reuse_events (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    document_id TEXT NOT NULL,
    origin TEXT NOT NULL,              -- first_party | external_sourced
    occurred_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reuse_ws
    ON evidence_reuse_events(workspace_id, occurred_at DESC);
```

Refresh coalescing (D7) is enforced by a partial unique index on the outbox.
Existing databases may already contain duplicate pending refresh rows, so the
migration de-duplicates first and keeps the oldest row:

```sql
DELETE FROM index_outbox
WHERE id NOT IN (
    SELECT MIN(id) FROM index_outbox
    WHERE operation = 'refresh' AND status = 'pending'
    GROUP BY entity_type, entity_id, operation
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_pending_refresh
    ON index_outbox(entity_type, entity_id, operation)
    WHERE status = 'pending' AND operation = 'refresh';
```

`documents` gains one column, `visibility`, defaulting to `'workspace'`.
Existing rows keep their current behaviour; promotion writes `'workspace'` in
v1.

### 8. Failure Handling

| Failure | Behavior |
|---------|----------|
| Final gate rejects the item | `state = rejected` with the failing constraint; **never indexed**. |
| Unresolved conflict in the evidence set | Conflicting items are `rejected`; the rest may still be promoted. |
| Capture fetch fails (robots / empty / blocked / network) | `state = skipped` with the reason; the response still carries the snippet for this call; no durable artefact. |
| Capture body exceeds the size bound | `state = skipped`, reason `promoted_bytes_exceeded`. |
| Budget exhausted before capture | `state = skipped`, reason `budget_exhausted`; the task's own status is unchanged — promotion never alters the response status. |
| Index write fails after the metadata commit | Existing outbox retry / DLQ path (RFC-006 6.7, RFC-014 14.4). |
| Cache invalidation fails | Logged and counted; the promotion stays valid, the next retrieval may see a stale miss until TTL. Never fails the task. |
| Duplicate capture for the same URL | The capture worker is idempotent through `save_versioned` (RFC-006 6.4): unchanged content ⇒ no new version. |
| Concurrent stale hits for one URL | Coalesced by the partial unique index; the counter `searchos_refresh_coalesced_total` increments. |
| Provider returns an item whose URL is excluded by source policy | `state = skipped`, reason `source_policy_denied`. Excluded domains are never promoted. |

### 9. Security Considerations

1. **Promotion is a fetch.** Every promotion goes through `FetchService`:
   canonical URL (one implementation, Part 30 INV-30-15), robots.txt
   enforcement (RFC-010 10.4/10.5), SSRF guard with DNS pinning
   (Part 30 INV-30-11) and Crawl-delay. No promotion bypasses these.
2. **No credentialed URLs.** Canonicalisation strips userinfo; credentials in
   URLs are never stored or replayed.
3. **Bounded work.** `max_promotions_per_task` and
   `max_promoted_bytes_per_task` cap the work per task. The capture worker is
   a separate outbox consumer with its own limits, so a burst of external
   searches cannot create an unbounded crawl.
4. **Source policy is respected.** A result from an excluded domain is never
   promoted, and an excluded domain is never re-fetched.
5. **Workspace isolation (ADR-023).** Promotion writes a binding for the
   acquiring workspace only. `public_web_knowledge` is off in v1, so a
   purchase made in workspace A is invisible to workspace B.
6. **Promoted content is data, never instructions.** Captured pages are
   sanitised with the existing HTML extractor (RFC-014 14.2) and stored as
   inert text. Nothing captured is executed, interpreted or rendered as a
   directive.
7. **Cost is charged.** The capture fetch is a `web_fetch` cost event against
   the same task and the same reservation. A purchase that triggers two
   captures costs two fetches and says so.
8. **No speculative crawling.** Promotion is triggered only by an actual
   external acquisition, and only for items the quality gate accepted.

### 10. Performance Considerations

- **No added request latency.** Promotion is enqueued, not awaited. The
  response returns immediately after the acquisition is recorded.
- **Index write amplification.** Each promoted page adds one document
  version and N chunks. The bound is `max_promotions_per_task` (default 2), so
  a single task adds at most two pages to the store.
- **Outbox cadence.** The gateway drains every 1.5 s, so a promoted page is
  searchable shortly after the task that bought it. The CLI drains on startup
  and after each command.
- **Cache invalidation cost.** Promotion invalidates the retrieval and
  canonical caches for the affected workspace. This is a correctness
  requirement (D4), not an optimisation; it is bounded by the number of
  promotions, which is bounded per task.
- **Measurement overhead.** One acquisition row, one row per item and one
  reuse event per served document. Reuse events are written in the same
  transaction as the task record, so they cannot drift from the decision.
- **Vector index cost.** Each promoted chunk is embedded once. The embedding
  cache (RFC-007 7.6) is keyed by model id and version, so an unchanged page
  reuses its embedding.

### 11. Acceptance Criteria

| ID | Criterion |
|----|-----------|
| AC1 | An external acquisition whose item passes the final quality gate produces an `evidence_items` row in state `promoted`, a `documents` row, a `document_versions` row, `document_chunks` rows and an index entry. |
| AC2 | After `rebuild_from`, every promoted document is still retrievable (D1 fixed). |
| AC3 | An external item the final gate rejects (conflict or failed hard constraint) never produces an index entry, and its state is `rejected` with a reason (D2 fixed). |
| AC4 | A promoted page is stored with its full extracted body, not the provider snippet (D3 fixed). |
| AC5 | After a promotion, the next equivalent query resolves locally and issues **zero** external provider calls. |
| AC6 | `searchos_external_evidence_reuse_total` increments for every local resolution that serves an external-sourced document, and the reuse event names the document. |
| AC7 | The dashboard's `local_resolution` is split into `first_party` and `external_sourced`, and the two never share a counter (G8). |
| AC8 | The number of `WebSearch` cost events for a task equals the number of queries in its external plan, including expansions (D5 fixed). |
| AC9 | Two concurrent stale hits for the same URL produce **one** pending refresh outbox row and increment `searchos_refresh_coalesced_total` (D7 fixed). |
| AC10 | A document promoted in workspace A is **not** retrievable from workspace B while `public_web_knowledge = false` (ADR-023). |
| AC11 | A promotion target disallowed by robots.txt is `skipped` with reason `robots_disallowed`, and no document row is created. |
| AC12 | A promotion whose URL is in `excluded_domains` is `skipped` with reason `source_policy_denied`. |
| AC13 | `GET /evidence/reuse` returns acquisitions, promoted, rejected, skipped, reuse events and the compound ratio. |
| AC14 | Every new metric appears in `GET /metrics` with the documented name. |
| AC15 | All pre-existing tests stay green; new unit + integration + compatibility tests cover AC1–AC14. |

### 12. Dependencies

| Dependency | Use |
|------------|-----|
| `searchos-domain` | `ExternalAcquisition`, `EvidenceItem`, `PromotionState`, `EvidenceOrigin`, `QueryOrigin` |
| `searchos-decision` | `GapResolutionStrategy`, requirement ids, `GapAnalyzer` |
| `searchos-application` | `EvidencePromotionService`, `CacheInvalidator` port, search pipeline changes |
| `searchos-storage-sqlite` | migration v6, acquisition / item / binding / reuse repos, outbox de-duplication |
| `searchos-document` | chunking, `VersionDiff`, hash — reused unchanged |
| `searchos-fetch` | governed fetch path (robots, SSRF, extraction) — reused unchanged |
| `searchos-core` | runtime wiring, outbox worker branch for `capture`, cache invalidation |
| `searchos-observability` | new metrics series, Prometheus renderer |
| `searchos-interface-http` | `/evidence/*` read-only routes, OpenAPI |
| `searchos-interface-cli` | `searchos evidence list|stats` |
| `searchos-policy` | source policy for promotion targets, `ContentCategory` for captured pages |

No new third-party crates are required.

### 13. Implementation Tasks

| ID | Task |
|----|------|
| T1 | Domain: `ExternalAcquisition`, `EvidenceItem`, `PromotionState`, `EvidenceOrigin`, `QueryOrigin`; `Document.visibility`. |
| T2 | Migration v6: `external_acquisitions`, `evidence_items`, `document_bindings`, `evidence_reuse_events`, `documents.visibility`, outbox de-duplication + partial unique index. |
| T3 | SQLite: repos for acquisitions, items, bindings, reuse events; `save_versioned` writes `visibility`; binding-aware document lookup. |
| T4 | Decision/pipeline: move the external index write behind the final gate; record the acquisition + items + accepted ids; enqueue capture. |
| T5 | Application: `EvidencePromotionService` (record → queue → capture) and the `CacheInvalidator` port; `Runtime` implements both. |
| T6 | Capture worker: outbox branch for `capture` — governed fetch, `save_versioned`, index write, promotion state update, cache invalidation. |
| T7 | Planner: move query expansion inside the plan; dedup queries; stamp `query_origin`; size the reservation from the final query list. |
| T8 | Refresh: coalesce pending refresh rows (D7) and count coalesced events. |
| T9 | Observability: seven new metric series; dashboard `local_resolution` split into first-party / external-sourced; reuse report. |
| T10 | HTTP: `/evidence/acquisitions`, `/evidence/acquisitions/:id`, `/evidence/reuse`; OpenAPI + route-table parity. |
| T11 | CLI: `searchos evidence list`, `searchos evidence stats`. |
| T12 | Docs: RFC-031, ADR-032, ADR-033, RFC index, docs index, API reference, feature roadmap. |
| T13 | Tests: unit (promotion state machine, conflict rejection, coalescing, visibility), integration (promote → rebuild → reuse, no pollution, zero external calls on the second query), CLI + HTTP smoke. |

---

## 中文

### 1. 背景

SearchOS 是本地优先的：决策引擎先评估本地知识，再决定是否花钱调外部 Provider；
`mode=local_only` 禁止一切外部调用。RFC 套件自己的表述已经写明了意图——
**每一次外部搜索都必须留下后续任务可以复用的知识**。

今天这种复用存在，但只是一种**临时的索引条目**。当任务买下一个外部答案时，
`search_service.rs` 会把至多两条结果摘要直接写进 BM25 和向量索引
（`crates/searchos-application/src/search_service.rs:1384`）。由这个捷径
派生出八个可在当前代码树中逐条核实的缺陷。

**D1 — 买来的知识不是文档。** `search_service.rs:1384` 的外部写入会生成新的
`document_id` / `version_id` 并索引一条 `IndexedDocument`，但**不会**创建
`documents`、`document_versions`、`document_chunks` 行，不写对象存储引用，也
不写 `sources` 行。而 `Runtime::rebuild_index`
（`crates/searchos-core/src/runtime.rs:645`）只从这三张表重建索引。因此
**一次重建、一次备份恢复或一次索引重建就会抹掉系统买过的一切**。知识库
不产生复利，只会在下次重建时蒸发。

**D2 — 未经校验的证据在质量门之前就进入了索引。** 写入发生在
`search_service.rs:1384`，最终质量门发生在之后的 `search_service.rs:1489`。
质量门随后拒绝的证据——未解决冲突、硬约束失败、覆盖不足——此时已经在索引
里。下一次查询的本地检索命中它，并把它当成第一方知识。这是知识污染，也是
当前设计里伤害最大的一处缺陷。

**D3 — 只捕获了摘要。** `execute_external` 设置 `include_raw_content: false`
（`search_service.rs:1709`）。Provider trait 返回 `content`（短摘要）和可选的
`raw_content`（`crates/searchos-provider/src/traits.rs:45-54`），但 SearchOS
既不请求 raw content，也不抓取页面正文。即使修好了 D1，沉淀下来的也只是
一两段话。`GapAnalyzer` 用 token 重叠判断覆盖度
（`crates/searchos-decision/src/gap.rs:78-126`），摘要几乎无法在换一个说法后
被判为 `Covered`。这次购买回答不了下一个问题。

**D4 — 刚买回来的知识可能被旧缓存挡住。** `SearchDependencies` 持有
`canonical_cache`、`semantic_cache`、`retrieval_cache`
（`search_service.rs:200-202`），`Runtime::invalidate_caches`
（`runtime.rs:586`）也已存在。但外部写入路径两者都没调用。刚刚付过钱的答案
可能继续看到缓存里的“本地未命中”，直到检索缓存过期——复利效应恰好在最该
生效的时候被遮住。

**D5 — 计划中的查询和实际执行的查询不一致。** `build_external_plan` 先算出
定向查询（`gap.rs:135`），随后 query expansion 又追加最多 `max_expansions`
条（`search_service.rs:1316-1327`）——**而成本估算和预算预留是按计划条数
算的**（预留发生在 `search_service.rs:1175-1177`）。账本记下的 `WebSearch`
事件多于预留的规模，trace 也解释不了一个缺口为什么产生了多次 Provider 调用。

**D6 — URL 身份是全局的，归属却不是。** `DocumentRepo::find_by_url`
（`crates/searchos-storage-sqlite/src/repos.rs:507`）只按 `canonical_url` 匹配。
一个先在工作区 A 捕获的文档归属于 A。工作区 B 捕获同一个公共 URL 时，要么
绑到 A 的行，要么产生重复，而系统里没有“可见性”这个概念来说明哪种才对。

**D7 — 刷新风暴。** `enqueue_refresh`（`repos.rs:2387`）无条件插入一条待处理
的 outbox 行。同一个 URL 被并发命中为 stale 时，会重复抓取同一个页面。

**D8 — 节省指标看不见复利。** `external_avoidance_rate`（`runtime.rs:1311`）
统计的是**任何**本地解析，包括用以前买来的摘要解析的那次。它无法区分
“因为我们自有知识而免掉一次外部调用”和“因为我们缓存了这次购买而免掉一次外部
调用”。这个闭环的价值在指标里是看不见的。

D1–D8 正是系统目前只是**本地优先的搜索代理**、而不是它被设计成的那种
**知识中间层**的原因。

### 2. 目标

| 编号 | 目标 |
|------|------|
| G1 | 每次外部获取要么成为持久、可重建的知识对象，要么被显式不沉淀并记录原因。 |
| G2 | 沉淀必须过质量门：只有最终质量门接受的证据才能进入持久存储；被拒绝的证据永不进入索引。 |
| G3 | 沉淀通过既有受控抓取路径获取**页面正文**（canonical URL、robots、SSRF 防护、版本化、切块、outbox），而不是摘要。 |
| G4 | 沉淀不阻塞：用户响应永远不等待沉淀抓取。 |
| G5 | 沉淀端到端可审计：需求 → 查询 → Provider → 文档 → 版本 → 复用。 |
| G6 | 知识可见性显式化：默认工作区私有，绑定模型为后续的授权共享预留。 |
| G7 | 复利效应可度量：获取、沉淀、拒绝、跳过、复用的计数，以及复用比。 |
| G8 | 节省口径诚实：本地解析拆分为第一方与外部来源两类。 |
| G9 | 外部计划与实际执行查询一致，预留与账本对齐。 |

### 3. 非目标

| 编号 | 非目标 |
|------|--------|
| N1 | 答案合成与声明级引用。RFC 套件已把这两件事划给上层 Agent（RFC 套件 README 的“Search Result / Evidence Set / Answer”三层划分）。本 RFC 产出**证据集**。 |
| N2 | 替换默认 embedder。`HashingEmbedder`（`crates/searchos-provider/src/embedding.rs:34`）是已知短板，但在存储层还没装下可信内容之前换它，只会更准确地召回未经验证的内容。推迟到有真实数据后的后续 RFC。 |
| N3 | 改动语义缓存的相似度门槛（`SEMANTIC_REUSE_MIN_SIMILARITY = 0.92`）或七项兼容性门槛。 |
| N4 | 放松 strict-freshness、`ExternalOnly`、`ExternalFirst` 语义（RFC-004 v1.1 9.4/9.5）。 |
| N5 | 改变 `mode` 语义或决策规则顺序。 |
| N6 | 分布式、多节点或共享缓存设计。 |
| N7 | 爬虫策略、站点专属爬虫或扩展系统。 |
| N8 | 自主站点爬取或批量采集。沉淀只按次发生，绝不投机性触发。 |
| N9 | 默认跨工作区共享。RFC 套件基线原则不变：**“默认 Workspace 隔离，显式授权后才共享，不应该默认全局共享。”** |

### 4. 架构

```text
                    ┌──────────────────────────────────────────┐
   request ────────▶│ 本地检索（BM25 + 向量 + RRF）              │
                    └───────────────┬──────────────────────────┘
                                    │ local candidates
                    ┌───────────────▼──────────────────────────┐
                    │ 决策引擎（RFC-004 v1.1）                  │
                    └───────────────┬──────────────────────────┘
                                    │ gap / 外部计划
                    ┌───────────────▼──────────────────────────┐
                    │ 外部执行（provider 调用）                 │
                    └───────────────┬──────────────────────────┘
                                    │ 证据集
                    ┌───────────────▼──────────────────────────┐
                    │ 最终质量门（RFC-005）                     │
                    │   accepted ────────┐                      │
                    │   rejected ────────┼──▶ 永不进入索引      │
                    └───────────────────┼──────────────────────┘
                                        │ 仅限质量门接受的外部条目
                    ┌───────────────────▼──────────────────────┐
                    │ acquisition + evidence_items（SQLite）    │
                    │   state = candidate → evaluated           │
                    └───────────────────┬──────────────────────┘
                                        │ outbox：capture
                    ┌───────────────────▼──────────────────────┐
                    │ 捕获 worker：FetchService                │
                    │   robots → SSRF 防护 → 抽取 →             │
                    │   Document + Version + Chunks + Source    │
                    └───────────────────┬──────────────────────┘
                                        │ outbox：index
                    ┌───────────────────▼──────────────────────┐
                    │ SearchIndex（Tantivy | ES | Meili | PG |  │
                    │   MySQL）+ 向量索引                        │
                    └───────────────────┬──────────────────────┘
                                        │ 失效
                    ┌───────────────────▼──────────────────────┐
                    │ canonical / semantic / retrieval cache    │
                    └──────────────────────────────────────────┘
```

最关键的改动是顺序。今天索引写入在**质量门之前**；本 RFC 把它移到**质量门
之后**，并且由持久化捕获 worker 供给，取代内联摘要写入。

沉淀是 **outbox 驱动且异步的**：任务记录获取与被接受的条目、投递一个捕获事件、
然后返回。网关已有的 1.5 秒 outbox tick 负责抓取、写版本和写索引。用户永远
不等待沉淀抓取。

### 5. 领域模型

#### 5.1 沉淀状态机

```text
              搜索返回条目
                   │
                   ▼
              ┌─────────────┐
              │  candidate  │  未捕获正文，未入索引
              └──────┬──────┘
                     │ 质量门已评估该条目
                     ▼
              ┌─────────────┐   质量门拒绝 / 未解决冲突
              │  evaluated  ├──────────────────────▶ rejected
              └──────┬──────┘
                     │ 质量门接受
                     ▼
              ┌─────────────┐   robots / 空内容 / 预算 / 体积 / 策略
              │  promotable ├───────────────────────▶ skipped
              └──────┬──────┘
                     │ 捕获 worker 成功
                     ▼
              ┌─────────────┐
              │  promoted   │  文档 + 版本 + 切块 + 索引
              └─────────────┘
```

**集合级**的 `Partial` 结论不会否定单个被接受的条目：“查询的一部分没被覆盖”
是对查询的陈述，不是对某条证据的陈述。但卷入**未解决冲突**的条目一律
`rejected`——把互相矛盾的内容沉淀进持久存储是有害的。

#### 5.2 外部获取记录

```rust
pub struct ExternalAcquisition {
    pub id: String,
    pub task_id: String,
    pub request_id: String,
    pub workspace_id: String,
    pub provider_id: String,
    /// 实际发给 Provider 的查询。
    pub query: String,
    /// 查询来源——计划与账本必须一致。
    pub query_origin: QueryOrigin,
    /// 本次购买针对的开放信息需求（若有）。
    pub requirement_id: Option<String>,
    pub strategy: GapResolutionStrategy,
    pub created_at: DateTime<Utc>,
}

pub enum QueryOrigin {
    /// `build_external_plan` 为某个开放需求生成。
    Gap,
    /// 需求仍未被满足，`QueryExpander` 追加。
    Expansion,
    /// 无法构建计划时使用用户原始查询。
    Fallback,
}
```

#### 5.3 证据条目

```rust
pub struct EvidenceItem {
    pub id: String,
    pub acquisition_id: String,
    pub url: String,
    pub canonical_url: String,
    pub title: Option<String>,
    /// Provider 报告的相关性。
    pub provider_score: f32,
    /// 质量门测得的相关性。
    pub relevance: f32,
    pub state: PromotionState,
    /// 捕获 worker 成功后写入。
    pub document_id: Option<String>,
    pub version_id: Option<String>,
    pub chunk_count: Option<usize>,
    /// 被拒绝或跳过的原因。
    pub reason: Option<String>,
    pub promoted_at: Option<DateTime<Utc>>,
}
```

#### 5.4 证据来源——第一方 vs 外部来源

每条本地候选都带来源：

```rust
pub enum EvidenceOrigin {
    /// 用户自有材料：本地导入、工作区文档。
    FirstParty,
    /// 本系统在买到搜索之后抓取的页面。
    ExternalSourced,
}
```

`ExternalSourced` 候选对 `local_only` 决策而言是**合法证据**——这正是复利
效应本身。但它必须在 decision trace 和节省口径里可区分，否则系统会把自己
买来的知识当成自有知识来汇报。G8 因此把本地解析拆成：

```text
local_resolution.first_party      ← 我们自有知识
local_resolution.external_sourced  ← 我们复用了购买结果
```

#### 5.5 可见性与绑定

```text
visibility = workspace（默认）→ 仅被绑定的工作区可读
visibility = public            → 所有工作区可读（可选开启，v1 关闭）
```

`document_bindings` 是多对多表：

```sql
document_bindings(
    document_id,
    workspace_id,
    visibility,      -- 'workspace' | 'public'
    bound_at,
    last_used_at
)
```

v1 中沉淀只为获取方工作区写绑定（`knowledge.public_web_knowledge = false`，
默认关闭）。这张表的存在，使得日后**无需迁移**即可开启授权的跨工作区共享。
这与存储层已有的检索规则一致——`requester OR '*'`
（`repos.rs:639`、`repos.rs:697`、`repos.rs:853`，以及 PostgreSQL / MySQL
远程后端中的同一谓词），也与 ADR-023 默认的工作区隔离一致。

#### 5.6 复用计数

```sql
evidence_reuse_events(
    id, task_id, workspace_id, document_id, origin, occurred_at
)
```

每当一次本地解析服务了 `ExternalSourced` 文档就写一行。这是复利指标的
分母：一次购买究竟服务了多少个后续任务？

### 6. 接口契约

#### 6.1 应用层端口

```rust
#[async_trait]
pub trait EvidencePromotionService: Send + Sync {
    /// 持久化获取与全部条目，并为质量门接受的条目排队捕获。
    async fn record_and_queue(
        &self,
        acquisition: ExternalAcquisition,
        items: Vec<EvidenceItem>,
        accepted_ids: &[String],
    ) -> Result<(), SearchOsError>;

    /// 捕获 worker 入口——执行受控抓取并写入文档/版本/切块。
    async fn capture(&self, item_id: &str) -> Result<(), SearchOsError>;
}

/// 知识变更后失效查询/检索缓存。
pub trait CacheInvalidator: Send + Sync {
    fn invalidate_after_promotion(&self, workspace_id: &str, canonical_url: &str);
}
```

`CacheInvalidator` 由 `Runtime` 实现并委托给既有的 `invalidate_caches`
（`runtime.rs:586`）。应用层只依赖端口，不依赖具体缓存类型——与全仓其它
位置的解耦方式一致。

#### 6.2 HTTP（Gateway Mode，只读观测）

| 方法 | 路径 | 说明 |
|------|------|------|
| `GET` | `/evidence/acquisitions?workspace_id=&limit=` | 获取列表，含沉淀状态与计数。 |
| `GET` | `/evidence/acquisitions/:id` | 单次获取及其条目，含拒绝/跳过原因。 |
| `GET` | `/evidence/reuse?workspace_id=` | 复利报告：获取、沉淀、拒绝、跳过、复用事件与复用比。 |

不新增写端点：沉淀是搜索的内部结果，不是调用方可以触发的操作。

#### 6.3 CLI（Local Mode，进程内）

```bash
searchos evidence list --workspace default [--limit 50]
searchos evidence stats --workspace default
```

#### 6.4 配置

```yaml
knowledge:
  promotion_enabled: true
  # 每个任务最多捕获多少条被接受的外部条目。
  max_promotions_per_task: 2
  # 每个任务捕获正文的硬字节上限。
  max_promoted_bytes_per_task: 10485760
  # 页面正文抓不到时是否退回摘要。
  # 默认 false：摘要不是知识。
  promote_snippet_fallback: false
  # 只沉淀最终质量门接受的条目。
  require_final_gate_pass: true
  # 跨工作区公共共享。v1 关闭（ADR-023）。
  public_web_knowledge: false
```

#### 6.5 指标

新增 Prometheus 序列，沿用既有规则——**状态计数是唯一带标签的序列**，因此
按状态拆成多条序列，而不是给一条序列加标签：

```text
searchos_external_acquisitions_total
searchos_external_promotions_promoted_total
searchos_external_promotions_rejected_total
searchos_external_promotions_skipped_total
searchos_external_promotion_fetches_total
searchos_external_evidence_reuse_total
searchos_refresh_coalesced_total
```

`searchos_external_evidence_reuse_total / searchos_external_acquisitions_total`
就是**复利比**：一次购买平均服务了多少个后续任务。

### 7. 数据模型

迁移 **v6**（`external_evidence_promotion`）为追加式。已发布的迁移不可变
（RFC-006 6.7）。

```sql
-- 每次外部获取一行。
CREATE TABLE IF NOT EXISTS external_acquisitions (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    request_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    provider_id TEXT NOT NULL,
    query TEXT NOT NULL,
    query_origin TEXT NOT NULL,       -- gap | expansion | fallback
    requirement_id TEXT,
    strategy TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_acquisitions_ws
    ON external_acquisitions(workspace_id, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_acquisitions_task
    ON external_acquisitions(task_id);

-- 每次获取的每个结果条目一行。
CREATE TABLE IF NOT EXISTS evidence_items (
    id TEXT PRIMARY KEY,
    acquisition_id TEXT NOT NULL,
    url TEXT NOT NULL,
    canonical_url TEXT NOT NULL,
    title TEXT,
    provider_score REAL NOT NULL DEFAULT 0.0,
    relevance REAL NOT NULL DEFAULT 0.0,
    state TEXT NOT NULL,              -- candidate | evaluated | promoted | rejected | skipped
    document_id TEXT,
    version_id TEXT,
    chunk_count INTEGER,
    reason TEXT,
    promoted_at TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(acquisition_id) REFERENCES external_acquisitions(id)
);
CREATE INDEX IF NOT EXISTS idx_evidence_items_acq
    ON evidence_items(acquisition_id);
CREATE INDEX IF NOT EXISTS idx_evidence_items_state
    ON evidence_items(state, created_at DESC);
CREATE INDEX IF NOT EXISTS idx_evidence_items_doc
    ON evidence_items(document_id);

-- 工作区 ↔ 文档绑定（可见性模型）。
CREATE TABLE IF NOT EXISTS document_bindings (
    document_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    visibility TEXT NOT NULL DEFAULT 'workspace',  -- workspace | public
    bound_at TEXT NOT NULL,
    last_used_at TEXT,
    PRIMARY KEY (document_id, workspace_id)
);
CREATE INDEX IF NOT EXISTS idx_bindings_ws
    ON document_bindings(workspace_id);

-- 复用计数（复利指标的分子）。
CREATE TABLE IF NOT EXISTS evidence_reuse_events (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    document_id TEXT NOT NULL,
    origin TEXT NOT NULL,              -- first_party | external_sourced
    occurred_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reuse_ws
    ON evidence_reuse_events(workspace_id, occurred_at DESC);
```

刷新合并（D7）由 outbox 上的部分唯一索引保证。既有数据库可能已经存在重复的
待处理 refresh 行，因此迁移先做去重并保留最早的一行：

```sql
DELETE FROM index_outbox
WHERE id NOT IN (
    SELECT MIN(id) FROM index_outbox
    WHERE operation = 'refresh' AND status = 'pending'
    GROUP BY entity_type, entity_id, operation
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_outbox_pending_refresh
    ON index_outbox(entity_type, entity_id, operation)
    WHERE status = 'pending' AND operation = 'refresh';
```

`documents` 新增一列 `visibility`，默认 `'workspace'`。既有行行为不变；v1
的沉淀写入 `'workspace'`。

### 8. 失败处理

| 失败 | 行为 |
|------|------|
| 质量门拒绝该条目 | `state = rejected` 并记录失败约束；**绝不进索引**。 |
| 证据集存在未解决冲突 | 冲突条目 `rejected`；其余条目仍可沉淀。 |
| 捕获抓取失败（robots / 空内容 / 被拦 / 网络） | `state = skipped` 并记录原因；本次响应仍返回摘要；不产生持久产物。 |
| 捕获正文超出体积上限 | `state = skipped`，原因 `promoted_bytes_exceeded`。 |
| 捕获前预算耗尽 | `state = skipped`，原因 `budget_exhausted`；任务自身状态不变——沉淀永不改变响应状态。 |
| 元数据提交后索引写入失败 | 走既有 outbox 重试 / DLQ 路径（RFC-006 6.7、RFC-014 14.4）。 |
| 缓存失效失败 | 记录日志并计数；沉淀仍然有效，下次检索可能因 TTL 看到陈旧的未命中。绝不使任务失败。 |
| 同一 URL 重复捕获 | 捕获 worker 通过 `save_versioned`（RFC-006 6.4）保持幂等：内容未变则不产生新版本。 |
| 同一 URL 的并发 stale 命中 | 由部分唯一索引合并，计数 `searchos_refresh_coalesced_total` 自增。 |
| Provider 返回的条目 URL 被来源策略排除 | `state = skipped`，原因 `source_policy_denied`。被排除的域名永不沉淀。 |

### 9. 安全考量

1. **沉淀就是一次抓取。** 每次沉淀都走 `FetchService`：canonical URL（唯一
   实现，Part 30 INV-30-15）、robots.txt 强制（RFC-010 10.4/10.5）、带 DNS
   pinning 的 SSRF 防护（Part 30 INV-30-11）以及 Crawl-delay。没有任何沉淀
   绕过这些。
2. **不接受带凭证的 URL。** canonical 化会去除 userinfo；URL 中的凭证不会被
   存储或重放。
3. **工作量有界。** `max_promotions_per_task` 与
   `max_promoted_bytes_per_task` 限制单任务的工作量。捕获 worker 是独立的
   outbox 消费者并自带上限，因此一批外部搜索不可能造成无界爬取。
4. **来源策略被遵守。** 来自被排除域名的结果永不沉淀，被排除域名也永不被
   重新抓取。
5. **工作区隔离（ADR-023）。** 沉淀只为获取方工作区写绑定。v1 中
   `public_web_knowledge` 关闭，因此工作区 A 的购买对工作区 B 不可见。
6. **沉淀内容是数据，不是指令。** 抓取到的页面用既有 HTML 抽取器清洗
   （RFC-014 14.2），以惰性文本存储。任何被抓取的内容都不会被执行、解释或
   渲染成指令。
7. **成本入账。** 捕获抓取是同一个任务、同一次预留下的 `web_fetch` 成本事件。
   一次购买触发两次捕获，就产生两次抓取成本并且如实记账。
8. **不投机爬取。** 沉淀只由真实的外部获取触发，且只针对质量门接受的条目。

### 10. 性能考量

- **不增加请求延迟。** 沉淀是投递而非等待。响应在记录获取后立即返回。
- **索引写入放大。** 每个被沉淀的页面增加一个文档版本和 N 个切块。上限是
  `max_promotions_per_task`（默认 2），因此单个任务最多给存储增加两个页面。
- **outbox 节奏。** 网关每 1.5 秒 drain 一次，因此被沉淀的页面在购买它的
  任务之后很快即可检索。CLI 在启动和每条命令后 drain。
- **缓存失效成本。** 沉淀会使受影响工作区的检索缓存与 canonical 缓存失效。
  这是正确性要求（D4）而非优化；其开销受沉淀次数约束，而沉淀次数受单任务
  上限约束。
- **度量开销。** 一次获取一行、每个条目一行、每个被服务的文档一个复用事件。
  复用事件与任务记录在同一事务写入，因此不会与决策漂移。
- **向量索引成本。** 每个被沉淀的切块只嵌入一次。嵌入缓存（RFC-007 7.6）
  以模型 id 和版本为键，内容未变的页面可复用嵌入。

### 11. 验收标准

| 编号 | 验收标准 |
|------|----------|
| AC1 | 一条通过最终质量门的外部条目产生 `evidence_items`（state = `promoted`）、`documents` 行、`document_versions` 行、`document_chunks` 行和一条索引条目。 |
| AC2 | `rebuild_from` 之后，每个被沉淀的文档仍可检索（D1 修复）。 |
| AC3 | 被最终质量门拒绝的外部条目（冲突或硬约束失败）绝不产生索引条目，状态为 `rejected` 并带原因（D2 修复）。 |
| AC4 | 被沉淀的页面存储的是抽取出的完整正文，而不是 Provider 摘要（D3 修复）。 |
| AC5 | 沉淀之后，下一次等价查询在本地解析，并且发出**零次**外部 Provider 调用。 |
| AC6 | 每当一次本地解析服务了外部来源文档，`searchos_external_evidence_reuse_total` 自增，且复用事件记录该文档。 |
| AC7 | Dashboard 的 `local_resolution` 拆分为 `first_party` 与 `external_sourced`，两者不共用计数器（G8）。 |
| AC8 | 一个任务的 `WebSearch` 成本事件数等于其外部计划中的查询数（含扩展查询）（D5 修复）。 |
| AC9 | 同一 URL 的两次并发 stale 命中只产生**一条**待处理 refresh outbox 行，并使 `searchos_refresh_coalesced_total` 自增（D7 修复）。 |
| AC10 | `public_web_knowledge = false` 时，在工作区 A 沉淀的文档**不能**从工作区 B 检索到（ADR-023）。 |
| AC11 | robots.txt 禁止的沉淀目标状态为 `skipped`、原因 `robots_disallowed`，且不创建文档行。 |
| AC12 | URL 位于 `excluded_domains` 的沉淀状态为 `skipped`、原因 `source_policy_denied`。 |
| AC13 | `GET /evidence/reuse` 返回获取数、沉淀数、拒绝数、跳过数、复用事件与复利比。 |
| AC14 | 每个新指标都以文档规定的名称出现在 `GET /metrics` 中。 |
| AC15 | 既有测试全部保持绿色；新增单元 + 集成 + 兼容性测试覆盖 AC1–AC14。 |

### 12. 依赖

| 依赖 | 用途 |
|------|------|
| `searchos-domain` | `ExternalAcquisition`、`EvidenceItem`、`PromotionState`、`EvidenceOrigin`、`QueryOrigin` |
| `searchos-decision` | `GapResolutionStrategy`、需求 id、`GapAnalyzer` |
| `searchos-application` | `EvidencePromotionService`、`CacheInvalidator` 端口、搜索管线改动 |
| `searchos-storage-sqlite` | 迁移 v6、获取/条目/绑定/复用仓储、outbox 去重 |
| `searchos-document` | 切块、`VersionDiff`、哈希——原样复用 |
| `searchos-fetch` | 受控抓取路径（robots、SSRF、抽取）——原样复用 |
| `searchos-core` | runtime 接线、outbox worker 的 `capture` 分支、缓存失效 |
| `searchos-observability` | 新指标序列、Prometheus 渲染器 |
| `searchos-interface-http` | `/evidence/*` 只读路由、OpenAPI |
| `searchos-interface-cli` | `searchos evidence list|stats` |
| `searchos-policy` | 沉淀目标的来源策略、被抓页面的 `ContentCategory` |

不需要新增第三方 crate。

### 13. 实施任务

| 编号 | 任务 |
|------|------|
| T1 | 领域：`ExternalAcquisition`、`EvidenceItem`、`PromotionState`、`EvidenceOrigin`、`QueryOrigin`；`Document.visibility`。 |
| T2 | 迁移 v6：`external_acquisitions`、`evidence_items`、`document_bindings`、`evidence_reuse_events`、`documents.visibility`、outbox 去重与部分唯一索引。 |
| T3 | SQLite：获取/条目/绑定/复用仓储；`save_versioned` 写入 `visibility`；绑定感知的文档查询。 |
| T4 | 决策/管线：把外部索引写入移到质量门之后；记录获取、条目与被接受的 id；投递捕获。 |
| T5 | 应用层：`EvidencePromotionService`（记录 → 排队 → 捕获）与 `CacheInvalidator` 端口；`Runtime` 实现两者。 |
| T6 | 捕获 worker：outbox 的 `capture` 分支——受控抓取、`save_versioned`、索引写入、状态更新、缓存失效。 |
| T7 | 计划器：把 query expansion 移入计划内部；查询去重；标记 `query_origin`；按最终查询列表确定预留规模。 |
| T8 | 刷新：合并待处理 refresh 行（D7）并计数合并事件。 |
| T9 | 观测：七条新指标序列；Dashboard `local_resolution` 拆分为第一方/外部来源；复用报告。 |
| T10 | HTTP：`/evidence/acquisitions`、`/evidence/acquisitions/:id`、`/evidence/reuse`；OpenAPI 与路由表一致性。 |
| T11 | CLI：`searchos evidence list`、`searchos evidence stats`。 |
| T12 | 文档：RFC-031、ADR-032、ADR-033、RFC 索引、docs 索引、API 参考、功能路线图。 |
| T13 | 测试：单元（沉淀状态机、冲突拒绝、合并、可见性）、集成（沉淀 → 重建 → 复用、无污染、第二次查询零外部调用）、CLI + HTTP 冒烟。 |

---

**RFC Suite Version:** 1.0 (+ Annexes 23/24/30; RFC-031 proposed)
**Status:** Proposed — external evidence promotion and knowledge compounding
