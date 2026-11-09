# RFC Implementation Gap Register

> **RESOLUTION (2026-11-09):** G-01…G-13 are implemented end-to-end. The named
> Part 30 acceptance tests live in `tests/integration/tests/rfc_gap_acceptance.rs`
> plus the unit tests referenced from each gap; `cargo test --workspace`,
> `cargo clippy --workspace --all-targets -- -D warnings`, and
> `cargo fmt --all -- --check` are green. This file is retained as the historical
> gap register and design rationale.


> RFC 实现缺口登记（对照 `docs/rfc/` 当前代码状态）
>
> Scope: RFC-001…029、Part 17…24、Part 30。本文只登记**尚未完整实现**的条目；
> 已落地且通过测试的主链路不在本文范围。
>
> Snapshot: 2026-11-08 working tree. 行号仅用于定位，函数名优先。
>
> Verification baseline: `cargo test --workspace` → 428 passed / 62 suites；
> `web` `typecheck + build`、`clippy -D warnings`、`fmt --check`、路由扫描均通过。
> 因此下文所有“未实现”均指**功能契约未闭环**，不是“编译失败”。

---

## English

### 1. 结论摘要

- **RFC-001…029 的 MVP 主链路已存在**：双运行模式、决策引擎、质量门、SQLite/Tantivy、
  成本账本、网关、CLI、MCP、Dashboard、本地导入。
- **但不能表述为“全部实现”**。以下三类缺口仍然存在：
  1. **Part 30 整份 Proposed 基线未落地**（21 条执行正确性不变量）。
  2. **若干 RFC 只有类型/单测，未接入运行时主链路**（hybrid retrieval、
     semantic/canonical cache、crawler execution、robots enforcement、扩展发现隔离）。
  3. **RFC-029 部分验收项与实现不一致**（HTTP 状态码、AC7、批次语义）。
- 文档本身也有漂移：`docs/gap-analysis.md` 写“20 项缺口均已补强”，但 Part 30
  仍标 `Proposed — not yet implemented`；README 的 “Hybrid retrieval /
  Extension-first / semantic memory” 超出当前运行时接线。

### 2. 缺口登记

#### G-01 · Part 30 执行正确性基线（整份未实现） (RESOLVED)

**规范**：`docs/rfc/part-30-execution-correctness-baseline.md` §30.1 明确
`Status: Proposed — not yet implemented`；§30.4 定义 INV-30-01…21。

**当前证据**：代码中不存在 `INV-30-*` 修复，也不存在 §30.11 列出的命名验收测试
（`local_only_never_returns_stale_or_excluded_candidates`、
`concurrent_same_request_id_executes_once_and_bills_once`、
`reject_path_settles_reservation`、`local_only_does_not_hit_hybrid_cache_entry`、
`follower_arriving_after_publish_terminates`、
`partial_external_failure_reports_spent_and_settles_spent`、
`rebuild_preserves_workspace_language_age_and_title`、
`deleted_document_is_not_retrieved`、
`failed_migration_leaves_previous_version_usable`、
`workspace_token_cannot_search_other_workspace_documents`、
`fetch_connects_to_validated_address`、`bad_manifest_quarantines_only_itself`、
`per_thousand_units_pricing_uses_unit_counts`、
`stage_trace_records_real_timestamps_and_duration`、
`canonicalization_is_identical_across_paths`、`no_unconsumed_config_fields`、
`oversized_response_rejected_before_full_buffer`、
`fetch_respects_robots_disallow`、
`transports_reject_unknown_mode_identically`、
`two_pages_on_same_official_host_are_not_a_conflict`、
`latency_samples_are_bounded`）。

**未闭环条目**（与 §30.4 一一对应）：

| ID | 状态 | 缺口 |
|---|---|---|
| INV-30-01 | OPEN | 本地候选未按 freshness/source 过滤即返回 |
| INV-30-02 | OPEN | `idempotency_begin` 丢弃 `n == 1` claim，并发重放可双执行 |
| INV-30-03 | OPEN | Reject/early return 路径不结算预算预留 |
| INV-30-04 | OPEN | 缓存键缺 mode/freshness；缓存命中先于幂等 claim |
| INV-30-05 | OPEN | Singleflight 无超时、先 remove 后 publish、cancel 永久占 key |
| INV-30-06 | OPEN | 外部部分失败时 `cost_spent` 被清零 |
| INV-30-07 | PARTIAL | rebuild 仍硬编码 `language: "en"` 与 `fetched_at: now` |
| INV-30-08 | OPEN | delete outbox 传 chunk_id，消费端当 document_id 用（删除 no-op） |
| INV-30-09 | OPEN | 迁移无事务；v2 `ALTER TABLE` 不可重入 |
| INV-30-10 | PARTIAL | `/documents/search` 无 workspace SQL 谓词 |
| INV-30-11 | OPEN | 校验后的 DNS 地址未固定到连接 |
| INV-30-12 | OPEN | 扩展发现失败被 `let _ =` 吞掉，不隔离、不计数 |
| INV-30-13 | OPEN | `PerThousandUnits` 当 per-request；解析失败静默归零；singleflight 节省恒 0 |
| INV-30-14 | OPEN | trace `started_at_ms` 不真；`coverage_status` 是数字；成功也填 rejection_reasons |
| INV-30-15 | OPEN | canonical URL 两套实现不一致 |
| INV-30-16 | OPEN | 多个 validate 字段无消费者 |
| INV-30-17 | OPEN | body 先完整缓冲再判上限；配置上限未传入 `FetchLimits` |
| INV-30-18 | OPEN | `robots_allows` 无调用者；`respect_robots` 无消费者；Crawl-delay 未解析 |
| INV-30-19 | PARTIAL | `max_results` 未在 CLI/MCP 对齐；错误码大小写不一致 |
| INV-30-20 | OPEN | 同 host 两个官方页面被误判为冲突；排除域名报成 `required_domain_missing` |
| INV-30-21 | OPEN | 延迟样本无界、缓存无 LRU 上限、限流窗口无过期清理 |

**验收**：`cargo test --workspace` 必须包含 §30.11 全部命名测试，且每条在修复前
先失败、修复后通过。当前不满足。

---

#### G-02 · RFC-007 Hybrid Retrieval 未接入运行时 (RESOLVED)

**规范**：RFC-007 §7.1/§7.5 要求 `BM25 → Vector → Metadata Filtering → RRF →
Optional Rerank → Evidence`。

**当前证据**：
- `crates/searchos-retrieval/src/fusion.rs` 的 `rrf_fuse` 与
  `crates/searchos-retrieval/src/lib.rs` 的 `HybridRetriever` 存在，并有单测。
- `crates/searchos-core/src/runtime.rs` 的 `SearchDependencies` 只有 `bm25`，
  没有 vector/rerank/canonical/semantic cache 字段。
- `crates/searchos-vector-lancedb` 有 adapter 与 `HashingEmbedder`，但运行时从未
  `upsert` 向量，也从未把 `LanceVectorIndex` 接进检索。
- `crates/searchos-cache` 的 `CanonicalQueryCache` / `SemanticCache` /
  `RetrievalCache` / `EmbeddingCache` / `RerankCache` 仅有类型与单测，未被
  `SearchService` 使用。

**影响**：搜索主链路实际是 **Tantivy + ExactQueryCache**；README 的
“Hybrid retrieval” 声明超出实现。

**验收**：`mode=local_only` 在 2k 语料上同时召回 BM25 与向量结果，RRF 融合后
排序可由测试断言；semantic reuse 必须过 intent/freshness/source/language/
workspace/coverage/validity 七项兼容检查。

---

#### G-03 · RFC-010 Crawler 执行链未接入 FetchService (RESOLVED)

**规范**：RFC-010 §10.4/§10.5/§10.7 要求 crawler 选择、robots、失败隔离、
Crawl-delay。

**当前证据**：
- `crates/searchos-crawler/src/registry.rs` 的 `select` 与 quarantine 存在。
- `crates/searchos-application/src/fetch_service.rs` 直接调用 `HttpFetcher`，
  不查询 `CrawlerRegistry`；`runtime.rs` 构造了 `crawlers` 但从未注入
  `FetchServiceImpl`。
- `crates/searchos-fetch/src/http.rs` 的 `robots_allows` 无生产调用者；
  `HttpFetcher::fetch` 不调用它。
- `CrawlerConfig` 的 `default_timeout_ms/max_concurrent/respect_robots` 无消费者。

**影响**：站点专属 crawler、robots 访问策略、Crawl-delay 实际未生效；
`FetchOutcome::RobotsDisallowed` 永不由生产路径构造。

**验收**：`fetch_respects_robots_disallow`、
`unreachable_robots_disallowed_by_default`、`crawl_delay_is_honoured`。

---

#### G-04 · RFC-011 扩展发现未隔离、不可观测 (RESOLVED)

**规范**：RFC-011 §11.3 失败进入 Degraded/Quarantined/Disabled；Part 24 §24.6
要求 failed index/refresh 可重试、可死信；Part 30 INV-30-12 要求发现失败
隔离并计数。

**当前证据**：
- `runtime.rs` 中 `let _ = extensions.discover(...)` 与
  `let _ = crawlers.discover(...)` 丢弃全部错误。
- `CrawlerRegistry::discover` 在 `errors` 非空时整体 `return Err`，后续
  manifest 不再加载；没有 per-manifest 继续加载。
- 没有 `extensions.load_failed_total` / `extensions.quarantined` 指标，
  也没有 `searchos extension list` 的 quarantined 视图。

**影响**：一个坏 manifest 可静默禁用后续扩展；用户以为已安装。

**验收**：`bad_manifest_quarantines_only_itself`、
`discovery_failure_is_logged_and_counted`。

---

#### G-05 · RFC-013 Memory 语义检索未接线 (RESOLVED)

**规范**：RFC-013 §13.3 要求 `Metadata Filter → BM25 + Vector → RRF →
Optional Reranker → Freshness Check`。

**当前证据**：
- `crates/searchos-core/src/runtime.rs` 的 `SqliteMemoryStore::search` 是
  词项重叠评分，注释明确写 “memory search MVP; vector path arrives with
  semantic reuse phase”。
- 所有 `MemoryEntry.embedding_status` 写死 `"none"`。
- CLI 只有 `memory search|stats`，没有 `memory remember`；普通用户无法从
  CLI 写入 memory。

**影响**：跨 Agent 复用只有 namespace 过滤 + 词重叠；README 的
“semantic memory” 超出实现。

**验收**：memory 条目可写入；相同语义不同措辞可召回；跨 workspace 非
GlobalPublic 不可见。

---

#### G-06 · RFC-029 本地导入验收未完全闭环 (RESOLVED)

**规范**：RFC-029 §6.1/§11。

**当前缺口**：

1. **AC1 状态码不一致**：规范要求 `POST /ingest` 返回 **201**；
   `crates/searchos-interface-http/src/routes.rs` 的 `ingest` 返回
   `Ok(Json(...))`，即 **200**。
2. **AC7 无端到端测试**：size-bound 只在 `IngestServiceImpl` 单测中验证；
   HTTP `/ingest/file` 与 CLI `--file/--dir` 没有“超限后无部分文档、无对象
   写入”的集成测试。
3. **批次语义不完整**：
   - `IngestBatchDto` 只有 `items: Vec<IngestDto>`，而
     `IngestService::ingest_batch` 只处理 `Text/File(PathBuf)`；目录批导入
     只能走 CLI。
   - 200 MB 总量上限在循环中累加判断，未在导入前预检。
   - 混合批次 `source_type` 用 `all(File)` 推断，文本批次与混合批次语义不清。
   - 批项失败路径中 `continue` 前可能已写 job item，失败项与 job 汇总需收口。
4. **MCP 未暴露 ingest**：RFC-029 §6.3 已写明这是 follow-up，当前仍只有
   RFC-021 的 7 个工具 + `search_trace` alias。

**验收**：AC1–AC11 全部有对应自动化测试；HTTP 状态码、批次数、失败项、
rebuild 一致性可断言。

---

#### G-07 · RFC-008 / Part 30 INV-30-13 计价语义未对齐 (RESOLVED)

**规范**：RFC-008 §8.2/§8.4；Part 30 §30.4 INV-30-13。

**当前证据**：
- `crates/searchos-cost/src/estimator.rs` 把 `PerThousandUnits.unit_price_per_k`
  直接存成 `provider_request_micros`，`estimate_request` 不读 unit counts。
- `Money::parse(...).unwrap_or(0)` 静默吞掉非法价格。
- `search_service.rs` 调 `record_singleflight(&fp, "", &[task_id], 0)`，
  `total_saved()` 结构性为 0。

**验收**：`per_thousand_units_pricing_uses_unit_counts`、
`cost_parse_failure_is_reported`、
`singleflight_savings_positive_when_followers_join`。

---

#### G-08 · Part 24 / Part 30 安全与可靠性未闭环 (RESOLVED)

**规范**：Part 24 §24.1/§24.2；Part 30 INV-30-10/11/17/19。

**当前缺口**：
- `DocumentRepo::search` 无 workspace 参数；`/documents/search` 先
  `authorize_workspace` 再忽略 workspace 做 SQL 过滤。
- `http.rs` 在 `resp.text()` 之后才检查 `body.len()`，超大响应已完整缓冲。
- `FetchLimits::default()` 固定 5 MB，`security.max_response_bytes=10MB` 未传入；
  `max_redirects` 未从配置传入。
- DNS 校验返回的 `Vec<SocketAddr>` 被丢弃，连接阶段重新解析。
- `max_results` 未在 CLI/MCP 对齐；HTTP/CLI/MCP 错误码大小写不一致。

**验收**：`workspace_token_cannot_search_other_workspace_documents`、
`fetch_connects_to_validated_address`、
`oversized_response_rejected_before_full_buffer`、
`configured_limits_are_applied`、
`transports_reject_unknown_mode_identically`、
`transports_agree_on_max_results`、`error_code_table_is_consistent`。

---

#### G-09 · RFC-006 / Part 30 INV-30-08 文档删除是 no-op (RESOLVED)

**规范**：RFC-006 §6.7；Part 30 §30.4 INV-30-08。

**当前证据**：
- `DocumentRepo::delete` 为每个 chunk 写 `entity_type='document_chunk'`、
  `entity_id=chunk_id` 的 outbox 行。
- `runtime.rs::process_outbox` 把 `row.entity_id` 构造成 `DocumentId` 后调用
  `fulltext.delete`。
- `TantivyIndex::delete` 按 document_id term 删除；chunk_id 不匹配，删除无效。
- `SearchIndex` trait 没有 `delete_by_chunk_ids`。

**影响**：`searchos document delete` 后文档仍可检索；`index_consistency`
 的 chunk 计数与 SQLite 漂移。

**验收**：`deleted_document_is_not_retrieved`、
`delete_outbox_retry_after_index_failure`。

---

#### G-10 · RFC-004 / Part 23 trace 字段语义未对齐 (RESOLVED)

**规范**：Part 23 §23.4；Part 30 INV-30-14。

**当前证据**：
- `engine.rs` 写 `coverage_status: format!("{:?}", required_subquestions.len())`，
  是数字字符串而非 `CoverageStatus`。
- `rejection_reasons` 对包括 `LocalOnly` 在内的所有决策都填
  `decision.reason().code()`。
- `state.rs::record` 用 `self.started_ms` 作为每个 stage 的 `started_at_ms`，
  且 engine 传入的 `duration_ms` 为常量/零。

**验收**：`stage_trace_records_real_timestamps_and_duration`、
`coverage_status_is_a_status_not_a_number`、
`successful_decision_has_no_rejection_reasons`。

---

#### G-11 · RFC-004 / Part 30 INV-30-15 canonical URL 双实现 (RESOLVED)

**规范**：Part 30 §30.4 INV-30-15。

**当前证据**：
- `fetch_service.rs::canonicalize` 只去 fragment/尾斜杠。
- `merge.rs::canonical_url` 小写化整个 URL、去 scheme、去 tracking 参数。
- `merge.rs::host_of` 把 `searchos-local://` 全部当成一个 host。

**影响**：同一 URL 在 fetch 与 merge 中产生不同 key；`find_by_url` 可能返回
任意行；去重失效。

**验收**：`canonicalization_is_identical_across_paths`、
`canonicalization_preserves_case_sensitive_paths`。

---

#### G-12 · Part 24 §24.3 配置字段未消费 (RESOLVED)

**规范**：Part 24 §24.3；Part 30 INV-30-16。

**无消费者字段**（在 `config.rs` 中定义，runtime 不读取）：
- `security.ssrf_protection`
- `crawler.default_timeout_ms/max_concurrent/respect_robots`
- `observability.log_level/metrics_enabled/trace_retention_days`
- `indexes.rebuild_on_start/verify_on_start`
- `extensions.allow_native/wasm_enabled`
- `search.semantic_dedup/incremental`
- `memory.embedding.*`

**验收**：`no_unconsumed_config_fields`；或显式 deprecated 并在
`config validate` 报警。

---

#### G-13 · 文档与实现漂移 (RESOLVED)

| 位置 | 声明 | 实际 |
|---|---|---|
| `docs/gap-analysis.md` §2 | 20 项缺口“均已补强” | Part 30 仍 `Proposed — not yet implemented` |
| `docs/rfc/README.md` 末尾 | `Implemented Engineering Baseline (Annex 30: Proposed)` | ADR-030 已挂名，但 21 条不变量未落地 |
| `README.md` Highlights | Hybrid retrieval / Extension-first / semantic memory | 主链路未接 vector/RRF、crawler、memory vector |
| `README.zh.md` Highlights | 同上 | 同上 |

**验收**：README 的 “implemented” 口径与本登记一致；Part 30 在落地前不得
标为 Implemented。

### 3. 建议修复顺序（ROI）

按 Part 30 §30.13 的 ROI 顺序，先修执行正确性，再补 RFC 主链路：

1. **任务 1–4**（行为修复）：本地候选过滤、幂等 claim、预算 RAII、缓存键/幂等前置。
2. **任务 5–9**：singleflight、外部失败成本、rebuild provenance、chunk 删除、
   事务化迁移。
3. **任务 10–12**：workspace SQL 过滤、DNS 地址固定、扩展发现隔离。
4. **任务 13–21**：计价、trace、canonical URL、配置消费、资源上限、robots、
   传输对齐、冲突检测、热路径有界。
5. **并行补 RFC 主链路**：hybrid retrieval 接线、crawler execution、memory
   语义检索、RFC-029 HTTP 状态码与批次语义。
6. **文档收口**：更新 `docs/gap-analysis.md`、`docs/rfc/README.md`、README
   与 README.zh 的 overclaim。

### 4. 完成定义

- `docs/rfc/` 中所有标 `Implemented` 的 RFC，其验收测试全部存在且通过。
- Part 30 §30.11 的 21 条命名测试全部存在且通过。
- README 不再声明未接入运行时的能力。
- `cargo test --workspace`、`clippy -D warnings`、`fmt --check`、前端
  `typecheck + build`、路由扫描保持绿色。

---

## 中文

### 1. 结论摘要

- **RFC-001…029 的 MVP 主链路已存在**：双运行模式、决策引擎、质量门、
  SQLite/Tantivy、成本账本、网关、CLI、MCP、Dashboard、本地导入。
- **但不能表述为“全部实现”**。仍然存在三类缺口：
  1. **Part 30 整份 Proposed 基线未落地**（21 条执行正确性不变量）。
  2. **若干 RFC 只有类型/单测，未接入运行时主链路**（hybrid retrieval、
     semantic/canonical cache、crawler execution、robots enforcement、扩展发现隔离）。
  3. **RFC-029 部分验收项与实现不一致**（HTTP 状态码、AC7、批次语义）。
- 文档漂移：`docs/gap-analysis.md` 写“20 项缺口均已补强”，但 Part 30 仍标
  `Proposed — not yet implemented`；README 的 “Hybrid retrieval /
  Extension-first / semantic memory” 超出当前运行时接线。

### 2. 缺口登记

#### G-01 · Part 30 执行正确性基线（整份未实现） (RESOLVED)

**规范**：`docs/rfc/part-30-execution-correctness-baseline.md` §30.1 明确
`Status: Proposed — not yet implemented`；§30.4 定义 INV-30-01…21。

**当前证据**：代码中不存在 `INV-30-*` 修复，也不存在 §30.11 列出的命名验收测试
（`local_only_never_returns_stale_or_excluded_candidates`、
`concurrent_same_request_id_executes_once_and_bills_once`、
`reject_path_settles_reservation`、`local_only_does_not_hit_hybrid_cache_entry`、
`follower_arriving_after_publish_terminates`、
`partial_external_failure_reports_spent_and_settles_spent`、
`rebuild_preserves_workspace_language_age_and_title`、
`deleted_document_is_not_retrieved`、
`failed_migration_leaves_previous_version_usable`、
`workspace_token_cannot_search_other_workspace_documents`、
`fetch_connects_to_validated_address`、`bad_manifest_quarantines_only_itself`、
`per_thousand_units_pricing_uses_unit_counts`、
`stage_trace_records_real_timestamps_and_duration`、
`canonicalization_is_identical_across_paths`、`no_unconsumed_config_fields`、
`oversized_response_rejected_before_full_buffer`、
`fetch_respects_robots_disallow`、
`transports_reject_unknown_mode_identically`、
`two_pages_on_same_official_host_are_not_a_conflict`、
`latency_samples_are_bounded`）。

**未闭环条目**（与 §30.4 一一对应）：

| ID | 状态 | 缺口 |
|---|---|---|
| INV-30-01 | OPEN | 本地候选未按 freshness/source 过滤即返回 |
| INV-30-02 | OPEN | `idempotency_begin` 丢弃 `n == 1` claim，并发重放可双执行 |
| INV-30-03 | OPEN | Reject/early return 路径不结算预算预留 |
| INV-30-04 | OPEN | 缓存键缺 mode/freshness；缓存命中先于幂等 claim |
| INV-30-05 | OPEN | Singleflight 无超时、先 remove 后 publish、cancel 永久占 key |
| INV-30-06 | OPEN | 外部部分失败时 `cost_spent` 被清零 |
| INV-30-07 | PARTIAL | rebuild 仍硬编码 `language: "en"` 与 `fetched_at: now` |
| INV-30-08 | OPEN | delete outbox 传 chunk_id，消费端当 document_id 用（删除 no-op） |
| INV-30-09 | OPEN | 迁移无事务；v2 `ALTER TABLE` 不可重入 |
| INV-30-10 | PARTIAL | `/documents/search` 无 workspace SQL 谓词 |
| INV-30-11 | OPEN | 校验后的 DNS 地址未固定到连接 |
| INV-30-12 | OPEN | 扩展发现失败被 `let _ =` 吞掉，不隔离、不计数 |
| INV-30-13 | OPEN | `PerThousandUnits` 当 per-request；解析失败静默归零；singleflight 节省恒 0 |
| INV-30-14 | OPEN | trace `started_at_ms` 不真；`coverage_status` 是数字；成功也填 rejection_reasons |
| INV-30-15 | OPEN | canonical URL 两套实现不一致 |
| INV-30-16 | OPEN | 多个 validate 字段无消费者 |
| INV-30-17 | OPEN | body 先完整缓冲再判上限；配置上限未传入 `FetchLimits` |
| INV-30-18 | OPEN | `robots_allows` 无调用者；`respect_robots` 无消费者；Crawl-delay 未解析 |
| INV-30-19 | PARTIAL | `max_results` 未在 CLI/MCP 对齐；错误码大小写不一致 |
| INV-30-20 | OPEN | 同 host 两个官方页面被误判为冲突；排除域名报成 `required_domain_missing` |
| INV-30-21 | OPEN | 延迟样本无界、缓存无 LRU 上限、限流窗口无过期清理 |

**验收**：`cargo test --workspace` 必须包含 §30.11 全部命名测试，且每条在修复前
先失败、修复后通过。当前不满足。

---

#### G-02 · RFC-007 Hybrid Retrieval 未接入运行时 (RESOLVED)

**规范**：RFC-007 §7.1/§7.5 要求 `BM25 → Vector → Metadata Filtering → RRF →
Optional Rerank → Evidence`。

**当前证据**：
- `crates/searchos-retrieval/src/fusion.rs` 的 `rrf_fuse` 与
  `crates/searchos-retrieval/src/lib.rs` 的 `HybridRetriever` 存在，并有单测。
- `crates/searchos-core/src/runtime.rs` 的 `SearchDependencies` 只有 `bm25`，
  没有 vector/rerank/canonical/semantic cache 字段。
- `crates/searchos-vector-lancedb` 有 adapter 与 `HashingEmbedder`，但运行时从未
  `upsert` 向量，也从未把 `LanceVectorIndex` 接进检索。
- `crates/searchos-cache` 的 `CanonicalQueryCache` / `SemanticCache` /
  `RetrievalCache` / `EmbeddingCache` / `RerankCache` 仅有类型与单测，未被
  `SearchService` 使用。

**影响**：搜索主链路实际是 **Tantivy + ExactQueryCache**；README 的
“Hybrid retrieval” 声明超出实现。

**验收**：`mode=local_only` 在 2k 语料上同时召回 BM25 与向量结果，RRF 融合后
排序可由测试断言；semantic reuse 必须过 intent/freshness/source/language/
workspace/coverage/validity 七项兼容检查。

---

#### G-03 · RFC-010 Crawler 执行链未接入 FetchService (RESOLVED)

**规范**：RFC-010 §10.4/§10.5/§10.7 要求 crawler 选择、robots、失败隔离、
Crawl-delay。

**当前证据**：
- `crates/searchos-crawler/src/registry.rs` 的 `select` 与 quarantine 存在。
- `crates/searchos-application/src/fetch_service.rs` 直接调用 `HttpFetcher`，
  不查询 `CrawlerRegistry`；`runtime.rs` 构造了 `crawlers` 但从未注入
  `FetchServiceImpl`。
- `crates/searchos-fetch/src/http.rs` 的 `robots_allows` 无生产调用者；
  `HttpFetcher::fetch` 不调用它。
- `CrawlerConfig` 的 `default_timeout_ms/max_concurrent/respect_robots` 无消费者。

**影响**：站点专属 crawler、robots 访问策略、Crawl-delay 实际未生效；
`FetchOutcome::RobotsDisallowed` 永不由生产路径构造。

**验收**：`fetch_respects_robots_disallow`、
`unreachable_robots_disallowed_by_default`、`crawl_delay_is_honoured`。

---

#### G-04 · RFC-011 扩展发现未隔离、不可观测 (RESOLVED)

**规范**：RFC-011 §11.3 失败进入 Degraded/Quarantined/Disabled；Part 24 §24.6
要求 failed index/refresh 可重试、可死信；Part 30 INV-30-12 要求发现失败
隔离并计数。

**当前证据**：
- `runtime.rs` 中 `let _ = extensions.discover(...)` 与
  `let _ = crawlers.discover(...)` 丢弃全部错误。
- `CrawlerRegistry::discover` 在 `errors` 非空时整体 `return Err`，后续
  manifest 不再加载；没有 per-manifest 继续加载。
- 没有 `extensions.load_failed_total` / `extensions.quarantined` 指标，
  也没有 `searchos extension list` 的 quarantined 视图。

**影响**：一个坏 manifest 可静默禁用后续扩展；用户以为已安装。

**验收**：`bad_manifest_quarantines_only_itself`、
`discovery_failure_is_logged_and_counted`。

---

#### G-05 · RFC-013 Memory 语义检索未接线 (RESOLVED)

**规范**：RFC-013 §13.3 要求 `Metadata Filter → BM25 + Vector → RRF →
Optional Reranker → Freshness Check`。

**当前证据**：
- `crates/searchos-core/src/runtime.rs` 的 `SqliteMemoryStore::search` 是
  词项重叠评分，注释明确写 “memory search MVP; vector path arrives with
  semantic reuse phase”。
- 所有 `MemoryEntry.embedding_status` 写死 `"none"`。
- CLI 只有 `memory search|stats`，没有 `memory remember`；普通用户无法从
  CLI 写入 memory。

**影响**：跨 Agent 复用只有 namespace 过滤 + 词重叠；README 的
“semantic memory” 超出实现。

**验收**：memory 条目可写入；相同语义不同措辞可召回；跨 workspace 非
GlobalPublic 不可见。

---

#### G-06 · RFC-029 本地导入验收未完全闭环 (RESOLVED)

**规范**：RFC-029 §6.1/§11。

**当前缺口**：

1. **AC1 状态码不一致**：规范要求 `POST /ingest` 返回 **201**；
   `crates/searchos-interface-http/src/routes.rs` 的 `ingest` 返回
   `Ok(Json(...))`，即 **200**。
2. **AC7 无端到端测试**：size-bound 只在 `IngestServiceImpl` 单测中验证；
   HTTP `/ingest/file` 与 CLI `--file/--dir` 没有“超限后无部分文档、无对象
   写入”的集成测试。
3. **批次语义不完整**：
   - `IngestBatchDto` 只有 `items: Vec<IngestDto>`，而
     `IngestService::ingest_batch` 只处理 `Text/File(PathBuf)`；目录批导入
     只能走 CLI。
   - 200 MB 总量上限在循环中累加判断，未在导入前预检。
   - 混合批次 `source_type` 用 `all(File)` 推断，文本批次与混合批次语义不清。
   - 批项失败路径中 `continue` 前可能已写 job item，失败项与 job 汇总需收口。
4. **MCP 未暴露 ingest**：RFC-029 §6.3 已写明这是 follow-up，当前仍只有
   RFC-021 的 7 个工具 + `search_trace` alias。

**验收**：AC1–AC11 全部有对应自动化测试；HTTP 状态码、批次数、失败项、
rebuild 一致性可断言。

---

#### G-07 · RFC-008 / Part 30 INV-30-13 计价语义未对齐 (RESOLVED)

**规范**：RFC-008 §8.2/§8.4；Part 30 §30.4 INV-30-13。

**当前证据**：
- `crates/searchos-cost/src/estimator.rs` 把 `PerThousandUnits.unit_price_per_k`
  直接存成 `provider_request_micros`，`estimate_request` 不读 unit counts。
- `Money::parse(...).unwrap_or(0)` 静默吞掉非法价格。
- `search_service.rs` 调 `record_singleflight(&fp, "", &[task_id], 0)`，
  `total_saved()` 结构性为 0。

**验收**：`per_thousand_units_pricing_uses_unit_counts`、
`cost_parse_failure_is_reported`、
`singleflight_savings_positive_when_followers_join`。

---

#### G-08 · Part 24 / Part 30 安全与可靠性未闭环 (RESOLVED)

**规范**：Part 24 §24.1/§24.2；Part 30 INV-30-10/11/17/19。

**当前缺口**：
- `DocumentRepo::search` 无 workspace 参数；`/documents/search` 先
  `authorize_workspace` 再忽略 workspace 做 SQL 过滤。
- `http.rs` 在 `resp.text()` 之后才检查 `body.len()`，超大响应已完整缓冲。
- `FetchLimits::default()` 固定 5 MB，`security.max_response_bytes=10MB` 未传入；
  `max_redirects` 未从配置传入。
- DNS 校验返回的 `Vec<SocketAddr>` 被丢弃，连接阶段重新解析。
- `max_results` 未在 CLI/MCP 对齐；HTTP/CLI/MCP 错误码大小写不一致。

**验收**：`workspace_token_cannot_search_other_workspace_documents`、
`fetch_connects_to_validated_address`、
`oversized_response_rejected_before_full_buffer`、
`configured_limits_are_applied`、
`transports_reject_unknown_mode_identically`、
`transports_agree_on_max_results`、`error_code_table_is_consistent`。

---

#### G-09 · RFC-006 / Part 30 INV-30-08 文档删除是 no-op (RESOLVED)

**规范**：RFC-006 §6.7；Part 30 §30.4 INV-30-08。

**当前证据**：
- `DocumentRepo::delete` 为每个 chunk 写 `entity_type='document_chunk'`、
  `entity_id=chunk_id` 的 outbox 行。
- `runtime.rs::process_outbox` 把 `row.entity_id` 构造成 `DocumentId` 后调用
  `fulltext.delete`。
- `TantivyIndex::delete` 按 document_id term 删除；chunk_id 不匹配，删除无效。
- `SearchIndex` trait 没有 `delete_by_chunk_ids`。

**影响**：`searchos document delete` 后文档仍可检索；`index_consistency`
  的 chunk 计数与 SQLite 漂移。

**验收**：`deleted_document_is_not_retrieved`、
`delete_outbox_retry_after_index_failure`。

---

#### G-10 · RFC-004 / Part 23 trace 字段语义未对齐 (RESOLVED)

**规范**：Part 23 §23.4；Part 30 INV-30-14。

**当前证据**：
- `engine.rs` 写 `coverage_status: format!("{:?}", required_subquestions.len())`，
  是数字字符串而非 `CoverageStatus`。
- `rejection_reasons` 对包括 `LocalOnly` 在内的所有决策都填
  `decision.reason().code()`。
- `state.rs::record` 用 `self.started_ms` 作为每个 stage 的 `started_at_ms`，
  且 engine 传入的 `duration_ms` 为常量/零。

**验收**：`stage_trace_records_real_timestamps_and_duration`、
`coverage_status_is_a_status_not_a_number`、
`successful_decision_has_no_rejection_reasons`。

---

#### G-11 · RFC-004 / Part 30 INV-30-15 canonical URL 双实现 (RESOLVED)

**规范**：Part 30 §30.4 INV-30-15。

**当前证据**：
- `fetch_service.rs::canonicalize` 只去 fragment/尾斜杠。
- `merge.rs::canonical_url` 小写化整个 URL、去 scheme、去 tracking 参数。
- `merge.rs::host_of` 把 `searchos-local://` 全部当成一个 host。

**影响**：同一 URL 在 fetch 与 merge 中产生不同 key；`find_by_url` 可能返回
任意行；去重失效。

**验收**：`canonicalization_is_identical_across_paths`、
`canonicalization_preserves_case_sensitive_paths`。

---

#### G-12 · Part 24 §24.3 配置字段未消费 (RESOLVED)

**规范**：Part 24 §24.3；Part 30 INV-30-16。

**无消费者字段**（在 `config.rs` 中定义，runtime 不读取）：
- `security.ssrf_protection`
- `crawler.default_timeout_ms/max_concurrent/respect_robots`
- `observability.log_level/metrics_enabled/trace_retention_days`
- `indexes.rebuild_on_start/verify_on_start`
- `extensions.allow_native/wasm_enabled`
- `search.semantic_dedup/incremental`
- `memory.embedding.*`

**验收**：`no_unconsumed_config_fields`；或显式 deprecated 并在
`config validate` 报警。

---

#### G-13 · 文档与实现漂移 (RESOLVED)

| 位置 | 声明 | 实际 |
|---|---|---|
| `docs/gap-analysis.md` §2 | 20 项缺口“均已补强” | Part 30 仍 `Proposed — not yet implemented` |
| `docs/rfc/README.md` 末尾 | `Implemented Engineering Baseline (Annex 30: Proposed)` | ADR-030 已挂名，但 21 条不变量未落地 |
| `README.md` Highlights | Hybrid retrieval / Extension-first / semantic memory | 主链路未接 vector/RRF、crawler、memory vector |
| `README.zh.md` Highlights | 同上 | 同上 |

**验收**：README 的 “implemented” 口径与本登记一致；Part 30 在落地前不得
标为 Implemented。

### 3. 建议修复顺序（ROI）

按 Part 30 §30.13 的 ROI 顺序，先修执行正确性，再补 RFC 主链路：

1. **任务 1–4**（行为修复）：本地候选过滤、幂等 claim、预算 RAII、缓存键/幂等前置。
2. **任务 5–9**：singleflight、外部失败成本、rebuild provenance、chunk 删除、
   事务化迁移。
3. **任务 10–12**：workspace SQL 过滤、DNS 地址固定、扩展发现隔离。
4. **任务 13–21**：计价、trace、canonical URL、配置消费、资源上限、robots、
   传输对齐、冲突检测、热路径有界。
5. **并行补 RFC 主链路**：hybrid retrieval 接线、crawler execution、memory
   语义检索、RFC-029 HTTP 状态码与批次语义。
6. **文档收口**：更新 `docs/gap-analysis.md`、`docs/rfc/README.md`、README
   与 README.zh 的 overclaim。

### 4. 完成定义

- `docs/rfc/` 中所有标 `Implemented` 的 RFC，其验收测试全部存在且通过。
- Part 30 §30.11 的 21 条命名测试全部存在且通过。
- README 不再声明未接入运行时的能力。
- `cargo test --workspace`、`clippy -D warnings`、`fmt --check`、前端
  `typecheck + build`、路由扫描保持绿色。
