# RFC-062 P0 Dependency Review — contracts, gaps, and the task list

> RFC-062 P0 依赖审查 —— 契约、缺口与任务清单
>
> Scope: the final RFC Master Index puts **046 Result Normalization, 054 Task
> Orchestration, 055 Cancellation & Deadline, 063 Search Source Registry,
> 064 Unified Resource Model** ahead of **062 Federated Search Architecture**
> in P0, because 062 can only be as good as the contracts it coordinates.
> This review maps every P0 contract to the code that exists today, states
> what is already satisfied, what is partially satisfied, and what is missing,
> then turns the residue into an ordered task list with named acceptance tests.
>
> Status: **review complete; P0 contracts 063 + 064 + 062-kernel delivered this
> iteration** (see §5). Review basis: commit tree after the
> `searchos-context` / `searchos-federated` additions.

---

## English

### 1. Verdict per P0 contract

| Contract | Owning RFC | Status in tree | Evidence | Residue for 062 |
|---|---|---|---|---|
| Result Normalization (046) | planned | **Partial → adapters deliver it** | `merge.rs` canonicalisation + conflict retention; `searchos-federated::source::SourceCandidate` is the unified cross-source shape, and both Phase 2 adapters emit it (local index + provider) | remaining: make `search_service.rs` consume adapter output instead of its own inline mapping (task 4) |
| Task Orchestration (054) | planned | **Partial → stage queue delivered** | `search_task` + outbox + idempotency + budget reservations; `AcquisitionState` caps rounds/queries/cost; `FederatedSearchPlan.stages` + `FederatedExecutor` provide the per-source queue and `ExecutionReport` | delivered: durable per-source execution records (`FederationExecutionTrace` on the trace, migration v9) |
| Cancellation & Deadline (055) | planned | **Partial → delivered** | singleflight timeout path, `max_execution_time_ms` budget, graceful-shutdown drain; plus `ExecutionControls` (flag + deadline) and per-source `tokio::time::timeout` | remaining: none for the kernel |
| Source Registry (063) | planned | **Delivered (kernel)** | `crates/searchos-federated/src/registry.rs`: scoped registration, precedence resolution, capability filtering, enable/disable, health; 25 invariant tests | Persistence (SQLite projection) when the gateway needs a durable catalog; in-process registry satisfies Local Mode now |
| Unified Resource Model (064) | planned | **Delivered (kernel)** | `crates/searchos-federated/src/resource.rs`: `SearchResource`, `ResourceType` (11 kinds), `ContentRef`, fail-closed `PermissionRef`, stable identity + dedup keys | Document/session ingestion mapping their rows into `SearchResource` when sources are wired |
| Federated Search Architecture (062) | **this RFC** | **Phases 1–5 delivered; P0 residual list closed** | `searchos-federated` kernel + executor + adapters; `search_service.rs` consults the planner behind `federation.strategy` (`legacy` default / `plan_only` / `execute`); `/search/plan` reports the source plan; `/system/status` and `GET /sources` expose the catalog; both equivalence gates pass | none for P0 (per-source trace fields, session/transcript source and weighted fusion wired — see §4/§5) |

**Overall: P0 is structurally unblocked, the runtime pieces exist, and all
residual tasks in §4 are closed.** The contracts 062 depends on (063 registry,
064 resource model) are tested, dependency-light code; the Phase 2 adapters,
Phase 3 executor and Phase 5 telemetry/trace pieces are landed on top. Task 4
wired the planner and executor into the pipeline behind the equivalence gate
in §6 — a behaviour-preserving change because `legacy` remains the default.

### 2. Contract-by-contract detail

#### 046 Result Normalization → 062 §9

| 062 requires | Today | Gap action |
|---|---|---|
| Unified candidate with `resource_id` ≠ `candidate_id` | `SourceCandidate` (federated) has source-scoped identity; domain `SearchResultItem` is response-shaped | Phase 2: provider adapter emits `SourceCandidate` with a derived stable id (`derive_resource_id`) |
| URL canonicalisation in one place | `merge.rs` canonicaliser is authoritative | Keep; `resource::canonicalize_reference` is dedup-key-only and documented as such |
| Raw scores preserved, never compared across sources | RRF is rank-based (`rrf_fuse_weighted`); raw scores kept on the candidate | none |
| Unknown metadata stays unknown | Candidates use `Option` everywhere | none |

#### 054/055 Orchestration, Cancellation & Deadline → 062 §7–8

| 062 requires | Today | Gap action |
|---|---|---|
| Plan with stages, per-stage timeout | `FederatedSearchPlan { stages, max_deadline_ms }`; the executor reads each source's declared `latency.timeout_ms`, falling back to the stage timeout | **delivered** |
| Independent per-source deadline | `tokio::time::timeout` per execution; a slow source is cut off and never awaited (the test asserts elapsed < 300 ms while the source sleeps 400 ms) | **delivered** |
| Cancellation token per execution | `ExecutionControls`: atomic flag + optional absolute deadline, checked before every stage and every sequential task | **delivered** — v1 handles requests inline, so "do not start the next thing" is the whole requirement; a plain atomic avoids a runtime dependency |
| Budget exhaustion keeps results + status | `SearchStatus::BudgetExhausted` + reserve→settle; `aggregate()` keeps candidates and reports failures separately; `ExecutionReport.not_executed` names what never ran | **delivered** |
| One failure never fails the others | Each source runs as its own task in a bounded `JoinSet`; a task error (including a panic) is back-filled as a failed source, never a failed request (ADR-062-007) | **delivered** |

#### 063 Source Registry → 062 §5

| 062 requires | Today | Gap action |
|---|---|---|
| Register / unregister / get / list / health | `InMemorySourceRegistry` — all present, scope-resolved | remaining: SQLite projection if the console must list sources (not needed for Local Mode) |
| Scope precedence Agent > Project > Workspace > Global | `SourceScope::precedence` + `pick()` resolution | none |
| Override never widens access | Workspace-bound registration invisible to other workspaces (`workspace_scoped_source_is_invisible_to_other_workspaces`) | none |
| Disabled hides every scope copy | `set_enabled` applies to all registrations of the id | none |
| One id = one resolved descriptor | `catalog_resolves_one_descriptor_per_id` | none |

#### 064 Unified Resource Model → 062 §6.4, §9.3

| 062 requires | Today | Gap action |
|---|---|---|
| `ResourceType` beyond Document (transcripts, tool executions, API records…) | 11 variants with category cross-checks (`category_matches_type`) | adapters map rows → resources |
| Stable identity across sources | `derive_resource_id` = FNV over (type, canonical ref) | swap to storage-layer SHA-256 if identity ever becomes security-relevant (documented) |
| Dedup key prefers content hash | `SearchResource::dedup_key` | none |
| Permission fail-closed, agent allow-list, egress default off | `PermissionRef` + tests | RFC-087 builds the ingestion-time redaction on top |

### 3. Named acceptance tests (added this iteration)

| Test | Gate |
|---|---|
| `federated_auto_never_escapes_local_only` | INV-FED-02 across all four strategies |
| `external_only_never_selects_a_local_source` | INV-FED-02 (priority cannot beat permission) |
| `unknown_mode_is_impossible_by_construction` | planner is total over the five modes |
| `planner_never_fans_out_to_every_source` | ADR-062-002 |
| `selective_strategy_skips_sources_that_fill_no_open_requirement` | strategy semantics |
| `local_first_is_a_property_of_the_plan_not_a_convention` | P-002 survives a 100× priority external source |
| `sequential_strategy_emits_one_stage_per_source` + `sequential_stops_as_soon_as_coverage_is_complete` | §7.3 |
| `covered_requirements_are_never_repurchased` | INV-30-04 generalised: no billable filler under any mode |
| `budget_exhaustion_drops_the_pricier_source_and_records_it` | §13 |
| `plan_rejections_are_auditable` | INV-FED-06 |
| `scope_precedence_cannot_relax_permissions` | RFC-063 |
| `workspace_scoped_source_is_invisible_to_other_workspaces` | RFC-013 8.7 |
| `agent_scope_overrides_workspace_scope`, `duplicate_same_scope_registration_is_rejected_not_silently_replaced`, `disabling_a_source_hides_it_from_every_scope_registration`, `catalog_resolves_one_descriptor_per_id` | RFC-063 |
| `source_timeout_does_not_fail_the_request` | INV-FED-01 |
| `required_source_failure_forces_partial` | ADR-062-004 |
| `failed_sources_are_reported_separately_from_candidates` | §14.3 |
| `context_assembly_never_drops_citations` (application) | RFC-067/068 |
| `context_budget_round_trips_through_http_search` (HTTP) | transport parity |
| resource-model tests (identity, dedup, permission, egress, serde) | RFC-064 |

### 4. Residual task list, in order

| # | Task | RFC | Effort | Blocks |
|---|---|---|---|---|
| 1 | Local retrieval adapter: wrap the active `SearchIndex` as a `SearchSource` emitting `SourceCandidate`s | 062 Phase 2 | **Done** (`LocalIndexSource`) | — |
| 2 | Provider adapter: wrap `SearchProvider` the same way, with per-source cost profile | 062 Phase 2 | **Done** (`WebSearchProviderSource`) | — |
| 3 | Executor: stage loop with per-source deadline, cancellation flag, fail-open join, partial aggregation | 054/055/062 Phase 3 | **Done** (`FederatedExecutor` + `ExecutionControls`) | — |
| 4 | Wire the planner/executor into `search_service.rs` behind `federation.strategy` (consumed per INV-30-20), decision engine still owning the final gate | 062 Phase 3 | **Done** (config + registry + plan report + both equivalence gates + provider execution migrated through the executor with service-side billing; see §6) | — |
| 5 | Memory as a federated source + `MemoryEntry` → `SearchResource` mapping | 064/069 | **Done** (`AgentMemorySource`, `memory_resource`, plus `SessionTranscriptSource` / `transcript_resource` for session transcripts) | — |
| 6 | Weighted fusion fed by `ReliabilityProfile` (weights from profile, not scores) | 066 | S | **Done** (`rrf_fuse_weighted` + generic `fuse_ranks` wired into `execute` outcome ordering; 3 named service gates) |
| 7 | Per-source metrics + plan/execution telemetry | 062 §16 | **Done** (13 `federation.*` invariant counters, exported via `/metrics`; `FederationExecutionTrace` on the decision trace via migration v9) | — |
| 8 | SQLite projection `search_sources` + gateway `GET /sources` (route table + OpenAPI in the same change) | 063 | **Done** (migration v8, `SourceCatalogRepo`, `GET /sources?persisted=`) | — |

### 5. What this iteration delivered (so the review is falsifiable)

* `crates/searchos-federated` — source contract, registry, resource model, planner kernel, executor, Phase 2 adapters; **56 tests** (10 unit + 25 invariants + 9 execution + 12 adapters/equivalence).
* `crates/searchos-retrieval` — `rrf_fuse_weighted` (N weighted ranked lists) with the two-list wrapper kept; deterministic tie-break; **+9 tests**.
* `crates/searchos-domain` — `ContextBudget` / `ContextBlockSummary` wire contract.
* `crates/searchos-application` — `context` block assembly behind an explicit request budget; **+1 named test**.
* HTTP / MCP / CLI accept `context_budget` identically and validate at the edge; OpenAPI documents `ContextBudget` + `ContextBlockSummary`.
* `searchos-application` / `searchos-core` — planner wiring behind
  `federation.strategy`: `legacy` (default, zero behaviour change),
  `plan_only` (federated plan on `/search/plan`, logged in the pipeline),
  `execute` (provider execution migrated through the executor — see §6).
  Config fields are consumed per INV-30-20; `/system/status` exposes the
  source catalog.
* **Pipeline-level equivalence gate delivered**:
  `federated_default_matches_legacy_pipeline` (integration) seeds the same
  knowledge under `legacy` and `plan_only` and asserts decision, status,
  quality, results, provenance content, cost and coverage are identical.
* **RFC-069**: `AgentMemorySource` (application layer, so the kernel keeps no
  storage dependency) + `memory_resource` mapping memory onto RFC-064 with
  permission derived from the SHARING POLICY. 4 acceptance tests, the central
  one proving a workspace-level read never surfaces an agent-private entry and
  never leaks across workspaces.
* **RFC-062 §16 telemetry**: 11 zero-seeded `federation.*` invariant counters
  (plan totals, selected/rejected sources, executed/failed/timeout/candidates/
  not-executed, memory searched/hits), emitted at the planner and executor call
  sites so `/search/plan` and the live pipeline cannot drift.
* **RFC-063 durable catalog**: append-only migration **v8** `search_sources`,
  `SourceCatalogRepo` (upsert/health/remove/list), runtime projection at
  startup, and `GET /sources` (live registry or `?persisted=true`), documented
  in OpenAPI in the same change.
* **Federated residuals closed (tasks 5/6/7 tail):**
  * `SessionTranscriptSource` + `transcript_resource` (application layer,
    registered in the runtime next to the memory source): past research turns
    as a workspace-scoped, non-billable `Conversation` source mapped onto
    `ResourceType::Transcript`; 5 acceptance tests, the central one proving
    only the requester's sessions are ever visible.
  * Cross-source ordering under `federation.strategy = execute` follows
    trust-weighted rank fusion (`searchos_federated::fuse_ranks`, weights =
    `ReliabilityProfile.success_rate`): `order_federated_outcomes` only
    reorders — payload, billing and per-provider attribution are untouched;
    single-source plans unchanged. 3 named service gates + 4 kernel fusion
    tests.
  * `FederationExecutionTrace` (planned / executed / failed / timed-out /
    not-executed source ids + candidate count) attached to `DecisionTrace`,
    re-recorded by the pipeline (SQLite INSERT OR REPLACE via migration **v9**
    `decision_traces.federation_json`; in-memory collector serves the last
    record for an id). Gate:
    `federation_execution_summary_survives_trace_re_record`, plus the execute
    integration gate now asserts the summary (`legacy` traces stay `None`).
* **RFC-080 slice — outcome-driven per-source circuit breaker:** every
  federated execution feeds its outcome back into the source's LIVE
  `ReliabilityProfile` (registry `record_outcome`). Three consecutive
  failures open the circuit, so the next plan rejects the source with
  `RejectionReason::Reliability` instead of paying for a dead provider; a
  30s cooldown allows exactly one half-open probe (the tested
  `CircuitBreaker` is reused, with a new additive `reset()` for an explicit
  verified success), and a success resets the streak. All scope copies of an
  id trip together. The transition increments
  `federation.source.circuit_opened_total`.
  Gates: `consecutive_failures_open_the_source_circuit_and_the_planner_rejects_it`,
  `one_success_closes_the_circuit_and_resets_the_failure_streak`,
  `outcome_updates_every_scope_copy_of_a_source`,
  `explicit_reset_closes_a_fully_open_breaker`, and the live-pipeline gate
  `federated_failures_trip_the_source_circuit_in_the_live_pipeline`.
* **Defect found by that gate (fixed):** under `execute`, `ExternalOnly` /
  `ExternalFirst` carry no gap-derived requirement ids, so the federated
  planner rejected every billable source as "fills nothing" and acquired
  NOTHING — the legacy loop buys one fallback query there. The fallback
  purchase now gets one namespaced synthetic open requirement
  (`fallback:<query>`), which keeps INV-30-04's no-re-buy rule and restores
  parity with the legacy path.
* Full suite: **678 tests, 0 failures**; `cargo fmt --check`, `cargo clippy --workspace --all-targets -- -D warnings`, route-conflict scan all clean.
* Route table unchanged by THIS residue pass; the earlier iteration added
  `GET /sources` with the route table + OpenAPI in the same change.

### 6. Risk note (why executor work is a separate change)

`search_service.rs` is the hot path shared by the two most recent commits
(`c54ccbe` retrieval quality, `ff8b2be` graded qrels / durable sessions). The
contracts above were landed additively so that wiring the planner into the
pipeline stays a *behaviour-preserving* change.

**Two gate levels, both delivered and passing.**

1. **Adapter level** — `legacy_pipeline_matches_federated_adapters` (in
   `crates/searchos-federated/tests/federated_adapters.rs`) asserts that the
   query the adapter hands the index equals the legacy query on every
   policy-bearing field (text, top_k, workspace, required/excluded domains,
   `fetched_after`, and `include_stale` per INV-30-01), that the adapter
   invents no language policy of its own, and that mapped candidates are
   one-for-one identical to the legacy results.
2. **Pipeline level** — `federated_default_matches_legacy_pipeline` (in
   `tests/integration/tests/full_pipeline.rs`) runs the whole assembled
   service twice over identical seeded knowledge, once with `legacy` and
   once with `plan_only`, and asserts decision path/reason, status, quality,
   result items, provenance content, cost and coverage gaps are identical
   (random evidence ids are identity, not content, and are excluded).

Two further gates keep the adapters honest about what they claim:

* `adapter_preserves_workspace_scoping` — the requester's workspace, not the
  registration scope, is what reaches the index.
* `local_and_external_domain_policy_agree` — the local (`candidate_allowed`)
  and external (provider) paths share **one** host-extraction rule and one
  domain-match predicate, verified over a corpus rather than asserted in a
  comment.

Deliberate adapter constraints, each with a test:

* **No second billing path.** `WebSearchProviderSource` reports an
  *estimate* for planning and never settles cost; billing stays in the
  service's reserve → execute → settle lifecycle (INV-30-12). A source that
  also billed would create two ledger entries per provider call.
* **Permission is reported, never assumed.** External candidates arrive with
  `permission_verified: false`, because providers do not know SearchOS
  workspaces; the assembler must not trust them implicitly.
* **Health is bookkeeping.** A failed `health_check` returns `Down`; it never
  becomes a request error.
* **Requirement authority is wiring-supplied.** `with_satisfies_requirements`
  exists because the kernel must not read a provider's private state to guess
  what it can answer; ordering keeps locality (P-002) as the dominant key so a
  billable source can never be planned ahead of free local knowledge.

**Task 4b (provider execution migration) — DELIVERED.** When
`federation.strategy = execute`, `execute_external` dispatches to a new
`execute_external_federated` path: the federated planner selects the web
sources (local index excluded — local retrieval is the pipeline's own path
and is never re-purchased), `FederatedExecutor` runs them with per-source
deadline and failure isolation, and the SERVICE settles billing — one
WebSearch cost event per executed source, priced by the same estimator as
the legacy loop, spend flowing back through `AcquisitionState` so the
reservation guard and the follow-up loop stay authoritative.

**The execution equivalence gate** (`federated_execute_mode_bills_once_and_matches_legacy_results`,
integration): the same fake provider, same query, `legacy` vs `execute` —
identical external item content (URL-normalised; legacy fans out per QUERY
and duplicates the entry, federated fans out per SOURCE and never re-buys),
decision path identical, and the double-billing veto asserts EXACTLY ONE
web_search cost event under `execute` (the legacy ledger honestly shows two
— one per planned query — which is the re-buy this mode removes). Unknown
provider ids price at zero: the test asserts the event's identity and count,
not an invented amount.

`federation.strategy=execute` now genuinely executes; the warning was
removed with the migration.

---

## 中文

### 1. P0 契约逐项结论

| 契约 | 归属 RFC | 现状 | 证据 | 062 的剩余工作 |
|---|---|---|---|---|
| 结果标准化（046） | 规划 | **部分 → 由适配器交付** | `merge.rs` 规范化与冲突保留；`searchos-federated::source::SourceCandidate` 是统一跨源形状，两个 Phase 2 适配器都产出它 | 无（`execute` 路径直接消费适配器输出，等价性门禁锁定） |
| 任务编排（054） | 规划 | **部分 → stage 队列已交付** | `search_task` + outbox + 幂等 + 预算预留；`AcquisitionState` 上限；`FederatedSearchPlan.stages` + `FederatedExecutor` 提供逐源队列与 `ExecutionReport` | 已交付：持久化逐源执行记录（`FederationExecutionTrace`，migration v9） |
| 取消与截止时间（055） | 规划 | **部分 → 已交付** | singleflight 超时路径、`max_execution_time_ms`、优雅停机排水；另有 `ExecutionControls`（标记 + deadline）与逐源 `tokio::time::timeout` | 剩余：内核无 |
| 来源注册（063） | 规划 | **内核已交付** | `registry.rs`：作用域注册、优先级解析、能力过滤、启停、健康；25 个不变量测试 | 已交付：migration v8 `search_sources` 投影 + `GET /sources` |
| 统一资源模型（064） | 规划 | **内核已交付** | `resource.rs`：`SearchResource`、11 种 `ResourceType`、`ContentRef`、fail-closed `PermissionRef`、稳定身份 + 去重键 | 接入来源时把文档/会话行映射为 `SearchResource` |
| 联邦检索架构（062） | 本 RFC | **Phase 1–5 已交付；P0 残留清单已关闭** | `searchos-federated` 内核 + 执行器 + 适配器；`search_service.rs` 在 `federation.strategy` 背后咨询 planner（默认 `legacy` / `plan_only` / `execute` 真实执行）；`/search/plan` 报告 source 计划；`/system/status` 与 `GET /sources` 暴露目录；两级等价性门禁通过 | 无 P0 残留（provider 执行迁移 4b、逐源指标 7、持久目录 8、会话逐字稿 5、融合接线 6 均已交付） |

**结论：P0 结构上已解锁，运行时部件已就位，§4 残留任务全部关闭。** 062 依赖的契约（063 注册表、064 资源模型）已是经过测试的轻依赖代码，Phase 2 适配器、Phase 3 执行器与 Phase 5 遥测/trace 就建在其上。任务 4 已把 planner 与执行器接入管线 —— 在 §6 等价性门禁保护下是行为保持型改动，`legacy` 仍是默认。

### 2. 逐契约明细

（英文表已逐条列出 046 / 054+055 / 063 / 064 的「062 要求 / 现状 / 缺口动作」，此处同义，不再重复。要点：）
- 046：跨源候选类型已补齐；规范化继续以 `merge.rs` 为唯一权威实现。
- 054/055：计划与逐源上下文已建模；执行器负责 `stage.timeout_ms` 与取消标记（加法字段，不破坏契约）。
- 063：作用域优先级、禁用全作用域生效、一 id 一描述符，全部已测试。
- 064：身份稳定、内容哈希优先去重、权限 fail-closed、egress 默认禁止。

### 3. 本轮新增的具名验收测试

见英文 §3 表格（20 项，覆盖 RFC-062 §12 清单与 RFC-063/064 的关键规则）。

### 4. 剩余任务清单（按顺序）

1. 本地检索适配器（包装 `LocalKnowledgePort`/`HybridRetriever` → `SearchSource`）。
2. Provider 适配器（同上，带逐源成本画像）。
3. 执行器：stage 循环 + 逐源 deadline + 取消 + fail-open 汇聚。
4. **已完成**：`federation.strategy` 配置（INV-30-20 消费）、registry 接线、`/search/plan` 报告、管线级等价性门禁 `federated_default_matches_legacy_pipeline`，以及 **provider 执行迁移（4b）**——`execute` 模式下来源选择与扇出走 executor（逐源 deadline、故障隔离），计费保持在服务侧逐调用落账，防双计费门禁 `federated_execute_mode_bills_once_and_matches_legacy_results` 通过。
5. **已完成**：摄取/会话适配器产出 `SearchResource`（记忆成为 Source），会话逐字稿也已接入 —— `SessionTranscriptSource`（workspace 隔离、`ResourceType::Transcript`、非计费）+ `transcript_resource`，5 个验收测试。
6. **已完成**：融合权重改由 `ReliabilityProfile` 提供 —— `rrf_fuse_weighted`（N 列表带权）+ 内核通用 `fuse_ranks`，`execute` 路径的跨源排序走 `order_federated_outcomes`（只排序，不改载荷/计费/归属）；3 个服务级具名门禁 + 4 个内核融合测试。
7. **已完成**：逐源指标（13 个 `federation.*` invariant 计数器，经 `/metrics` 导出）+ `DecisionTrace` 逐源字段 —— `FederationExecutionTrace`（planned/executed/failed/timed_out/not_executed/candidates），管线回写（migration v9 `decision_traces.federation_json`，INSERT OR REPLACE）；门禁 `federation_execution_summary_survives_trace_re_record`，execute 集成门禁同时断言摘要、legacy 保持 `None`。
8. **已交付**：migration v8 `search_sources` 投影 + `SourceCatalogRepo` + 网关 `GET /sources`（支持 `persisted=true`，路由表与 OpenAPI 同一提交）。

### 5. 本轮交付（可证伪清单）

见英文 §5。本轮累计：`searchos-federated` 内核（source/registry/resource/planner/executor/fusion，含内核融合与熔断反馈测试）、带权 RRF +9 测试，domain 契约（`ContextBudget`/`FederationExecutionTrace`），application 装配 + 具名测试，三传输层接入与 OpenAPI，planner/executor 管线接线（legacy / plan_only / execute 真实执行）与两级等价性门禁；全套 **678 测试 0 失败**，fmt / clippy / 路由扫描全绿。`GET /sources` 由前一轮随路由表 + OpenAPI 同步交付；本轮残留收尾未新增 HTTP 路由。逐源熔断（RFC-080 切片）由执行结果反哺 `ReliabilityProfile`，三次连续失败开熔断、cooldown 后半开探针、显式健康成功复位；门禁同时发现并修复了 `ExternalOnly` 在 `execute` 下无 requirement id 导致零采集的缺陷。

### 6. 风险说明（为什么执行器单独交付）

`search_service.rs` 是最近两个提交共用的热路径。以上契约均以加法式落地，使「把 planner 接入管线」保持为**行为保持**的改动。

**两级等价性门禁都已交付并通过**：

1. **适配器级** `legacy_pipeline_matches_federated_adapters`：适配器交给索引的查询在每一个带策略的字段上（text、top_k、workspace、必选/排除域名、`fetched_after`，以及按 INV-30-01 的 `include_stale`）都与既有本地路径一致；适配器不得自己发明 language 策略；映射出的候选与本地结果一一对应。
2. **管线级** `federated_default_matches_legacy_pipeline`：在装配好的真实服务上，用同一份种子知识分别以 `legacy` 与 `plan_only` 各跑一次，断言决策路径/原因、status、质量、结果条目、provenance 内容、成本与覆盖缺口完全一致（随机 evidence id 是身份不是内容，排除比较）。

另有 `adapter_preserves_workspace_scoping`（进索引的是请求者的 workspace，而非注册作用域）与 `local_and_external_domain_policy_agree`（本地与外部路径共享**同一套** host 提取与域名匹配规则，并用语料验证，而不只是写在注释里）。

**任务 4b（provider 执行迁移）已交付。** `federation.strategy = execute` 时，外部路径分派到 `execute_external_federated`：executor 只跑 Web 来源（本地索引是管线自有路径，永不重复购买），逐源 deadline/故障隔离由内核负责，**计费保持在服务侧** —— 每个执行来源一条 WebSearch 成本事件、按同一估算器定价，花费经 `AcquisitionState` 回流，预留护栏与 follow-up 循环保持权威。门禁 `federated_execute_mode_bills_once_and_matches_legacy_results`：同一 fake provider、同一查询，legacy 与 execute 结果内容（URL 归一化）一致，execute 恰好一条 web_search 事件（legacy 诚实显示两条按查询的重复购买，即本模式消除的问题）。

适配器的刻意约束（每条都有测试）：

* **不存在第二条计费路径。** `WebSearchProviderSource` 只报告用于规划的
  *估算*，绝不结算；计费留在服务的 reserve → execute → settle 生命周期
  （INV-30-12）。来源自己计费会让每次 provider 调用产生两条账。
* **权限只上报，不假设。** 外部候选带 `permission_verified: false`，因为
  provider 不认识 SearchOS workspace；组装器不得隐式信任。
* **健康是记账。** `health_check` 失败返回 `Down`，不会变成请求错误。
* **需求归属由接线方声明。** 提供 `with_satisfies_requirements`，因为内核
  不该去读 provider 私有状态猜测它能答什么；排序以本地优先（P-002）为主键，
  因此计费来源永不会被排在免费本地知识之前。
