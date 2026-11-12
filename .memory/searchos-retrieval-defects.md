---
name: SearchOS retrieval & search-agent defect register
description: Verified (reproduced) BM25/vector/search-agent defects found during the 2026-11 feature-gap audit
type: project-fact
---

A code + live-binary audit (2026-11, on commit 799a2c4) found the following
**reproduced** defects. Each was verified with a probe test, tokenizer dump, or
a real `searchos` binary run — treat them as facts until a fix lands.

**Why:** the v1.0-GA surface is contract-complete, but the retrieval quality
layer behind it has holes that unit tests do not cover (they index only clean
ASCII and call adapters directly, bypassing the runtime write path).

**How to apply:**
- **CJK is effectively unsearchable on the default Tantivy backend.** The schema
  uses the `en_stem` tokenizer (`crates/searchos-index-tantivy/src/lib.rs:107`),
  i.e. SimpleTokenizer → RemoveLongFilter(40) → LowerCaser → English stemmer.
  CJK has no spaces, so a Chinese sentence is ONE token; and because the
  RemoveLongFilter limit counts **bytes** (CJK = 3 bytes/char), any Chinese run
  longer than 13 chars is silently dropped. Verified: the sentence
  `这是关于本地优先检索引擎的详细说明。系统会先在本地知识库解析问题。` tokenizes
  to `[]` (empty), and an end-to-end ingest→search of Chinese text returns
  `insufficient_evidence`, 0 results, while the same content in English works.
  The MySQL remote backend has an ngram parser; Tantivy has no CJK path.
- **Query parse errors fail the whole search.** `TantivyIndex::build_query`
  parses the raw user text with `QueryParser::parse_query`
  (`crates/searchos-index-tantivy/src/lib.rs:137`). A query containing a URL
  (`https://tokio.rs/...`) → `Field does not exist: 'https'`; also unclosed
  quotes / trailing `AND` / `OR` → syntax error. Verified via HTTP:
  `POST /search {"query":"see https://tokio.rs/tokio for rust"}` → **HTTP 500**
  (`retrieval error: query parse: Field does not exist: 'https'`). There is no
  sanitize/fallback path.
- **The vector index is never populated at runtime.** `VectorIndex::upsert` has
  exactly one call site in the whole repo: a unit test. `process_outbox` writes
  only to `self.fulltext` (`crates/searchos-core/src/runtime.rs:886`). Verified:
  after ingest+search the `embeddings.lance` directory contains only
  manifest/version files, no data. So "hybrid BM25+vector RRF" is BM25-only in
  production; the G-02 acceptance test passes because it hand-upserts vectors.
- **No real embedding provider.** Only `HashingEmbedder` (feature-hashing
  bag-of-words) is implemented; `search.embedding.provider: remote` is not wired
  ("remote memory embeddings not wired" warning) and
  `searchos-vector-lancedb` constructs its own internal `HashingEmbedder(256)`
  independently of the runtime's instance.
- **Reranker is never wired.** `HybridRetriever::with_reranker` has no
  non-test caller; only `NeverReranker` exists. `intelligence.reranking` only
  gates the RRF result cache (misleading name).
- **Freshness is filtered after top-k truncation.** `search_service.rs:766`
  sets `query.include_stale = true` unconditionally, so `candidate_allowed`
  never applies `fetched_after`; stale docs occupy the top_k slots and can
  crowd fresh docs out. Verified at the index layer (12 stale + 1 fresh →
  strict-freshness query returns only stale, fresh absent) and by the code path.
- **No snippet/highlight.** Snippet = first 400 chars of body
  (`crates/searchos-index-tantivy/src/lib.rs:327`), not the matched region.
  Verified: a needle at position ~700 is absent from the snippet.
- **No title boost in Tantivy.** `QueryParser::for_index(index, [title, body])`
  gives equal weight; ES uses `title^2` and Postgres uses setweight A → the
  local default backend ranks differently from the remote ones.
- **No typo tolerance** (no FuzzyQuery anywhere) and **no BM25 k1/b tuning**.
- **The final gate's `verdict.coverage_gaps` is computed but never consumed** by
  the application layer; the response exposes only
  `quality.coverage: insufficient` with no missing-requirement detail or
  next-query suggestion. `SearchDecision::Hybrid.missing_requirements` is used
  only to build the one-shot external plan.
- **The pipeline is single-pass** — no iterative re-plan when coverage is still
  insufficient after acquisition; no query decomposition beyond
  comparison-with-≥2-entities; no session/follow-up; MCP has no
  research/plan/resume tool (10 single-shot tools).
- **Evaluation measures keyword coverage, not ranking quality** (no nDCG/MRR/
  recall@k, no relevance judgments); the CI gate runs 20 questions over 40
  English seed docs, so none of the above can regress the gate.

- **Retrieval fixes implemented in the current working tree (2026-11):** SearchOS analyzer with CJK bigrams + legacy analyzer override, literal-term query building with URL/syntax-error tolerance, oversized freshness candidate pool, `SearchResponse.coverage_gaps`, vector outbox upsert/delete/rebuild wiring using the shared runtime embedder, and runtime promotion/reindex/delete vector coverage. New/updated acceptance tests include CJK and malformed-query regressions, unrelated-query isolation, coverage-gap exposure, and runtime vector population/deletion. Full verification after this pass: `cargo test --workspace --locked` → 527 passed / 0 failed; `cargo clippy --workspace --all-targets -- -D warnings` clean; `cargo fmt --all -- --check` clean.

**Second pass (2026-11, on top of 799a2c4) — the remaining audit items are now implemented:**
- **Remote embedding provider**: `RemoteEmbeddingProvider` (OpenAI-compatible
  `/v1/embeddings`, batching, index-order restore, dimension validation, key from
  env only). The runtime's single embedder is shared by the vector index, memory
  and the semantic cache; LanceDB's Arrow schema now follows the model dimension
  (was hard-coded 256) and `rebuild_from` recreates the table so a model change
  is safe.
- **Reranker**: `FeatureReranker` (offline, title/body/phrase/recency) is wired
  by default and billed as a `Reranking` cost event; `RemoteReranker` targets
  cross-encoder endpoints. Rerankers only REORDER — they must never rescale
  `score`, because the quality gate normalizes relevance from it
  (`normalize_candidate_score`: `Hybrid | Reranked` share the RRF scale).
- **Highlight/snippet**: tantivy `SnippetGenerator` — the snippet is the
  matched region with `<b>…</b>` markers (`RetrievalCandidate.highlight`).
- **BM25F-lite**: title terms get an explicit boost (aligned with ES `title^2`);
  an edit-distance-1 fuzzy pass recovers misspelled ASCII queries.
- **Iterative search-agent loop**: `SearchBudget.max_acquisition_rounds`
  (default 2) re-plans reformulated queries while the final gate still has open
  requirements. Query/cost caps are cumulative; no query is re-bought; a
  provider failure never triggers re-buying; conflicts never iterate.
- **Ranking metrics + Chinese corpus**: MRR / nDCG@5 / Recall@5 / Precision@5
  against per-question qrels (`relevant_urls`), a 12-doc Chinese seed corpus,
  `dataset-zh.jsonl`, and a CI gate for CJK ranking quality.
- **Agent surface**: `POST /search/plan`, `POST /search/follow-up`, MCP
  `research` + `plan`, a bounded in-process session store, and
  `SearchResponse.coverage_gaps / session_id / focus_requirements`.

**Lessons from the second pass (do not repeat):**
- The retrieval pool was enlarged 4x for freshness recall. That DILUTED the
  quality gate (the engine judged 40 candidates instead of 10) and escalated
  covered local queries to external acquisition. The fix: keep the large pool
  for freshness, but cap the DECISION input at the original `top_k`.
- `searchos-local://` documents have no web host; counting them as one source
  made source-diversity checks fail. `host_of` now returns the canonical URI
  for local documents.

**Still open:** cross-encoder quality needs real relevance labels (the qrels
are URL-level and binary); the remote reranker/embedding paths have no live
provider test (only mock HTTP servers); sessions are process-local (no
persistence across restarts).
