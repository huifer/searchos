---
name: SearchOS decision-core quality red lines
description: Non-negotiable decision/search semantics added in the feature-completion pass
type: project-fact
---

The SearchOS decision core must preserve these invariants (source: ChatGPT share
`6af0361a-d4bc-83ec-afb2-9a12023f5602` messages #82/#87, documented in
`docs/rfc/part-23-decision-engineering-baseline.md`).

**Why:** the condensed 16-RFC baseline had left several quality-critical
semantics unimplemented; a stale cache hit could masquerade as a fresh answer,
and partial results could look successful.

**How to apply:**
- Every `SearchResponse` carries an explicit `status` (completed / completed_with_warnings / partial / insufficient_evidence / budget_exhausted / failed / cancelled), `warnings`, `task_id`, `replayed`. Never disguise partial/insufficient results as `completed`.
- `ExternalOnly`, `ExternalFirst`, strict temporal intent and strict freshness profiles/constraints MUST bypass the exact cache (`should_bypass_exact_cache`).
- Offline / provider-unavailable external paths return `Reject { ProviderUnavailable }` → `insufficient_evidence`; never downgrade to a fake local success.
- Stale local knowledge is refreshed via the `refresh` outbox event, never deleted from the index.
- The decision state machine rejects illegal transitions; every decision persists structured `stages[]`, `final_strategy`, `final_outcome`.
- External acquisition is gap-based: one targeted query per open `InformationNeed`, capped by query count and cost; covered requirements are never re-purchased.
- Budget lifecycle is reserve → execute → settle; estimated costs are never rewritten by billing reconciliation (`is_estimate` stays honest).
- `request_id` is an idempotency key: replaying it must not create a second task, external call, bill, or document version.
- Evidence merge canonicalizes URLs, retains conflicts, and counts independent sources by host; unresolved conflicts force `QualityStatus::Conflict`.
- Final quality gate uses measured relevance scores, never a constant placeholder.
- **Semantic cache reuse needs a real similarity gate (2026-11 fix).** The cache key
  is a coarse bucket (`vector_bucket`: first 8 embedding components rounded to
  one decimal). Unrelated queries can share a bucket, so `SemanticCache::get`
  now also requires `cosine_similarity(stored_query, request_query) >=
  SEMANTIC_REUSE_MIN_SIMILARITY (0.92)` before returning cached candidates. Before
  this fix a local-mode gateway (hashing embedder) served an unrelated document
  with a constant 0.5 score for arbitrary queries such as `zzzznonsense` —
  observed live, reproduced, and verified fixed. The seven `SemanticCompatibility`
  gates still apply; vector similarity is a recall condition, never the pass
  condition. Never weaken this gate back to bucket equality.

**Related:** [[searchos-project]], [[searchos-verification]], docs/gap-analysis.md, docs/rfc/README.md
