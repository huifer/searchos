# Feature Gap Analysis: Source Conversation vs. Implementation

> 功能遗漏分析：源对话 vs. 当前实现
>
> Source: `https://chatgpt.com/share/6af0361a-d4bc-83ec-afb2-9a12023f5602`
> (`本地搜索基础设施`, 88 nodes / 9 user turns, fetched via the share backend API).

## English

### 1. Method

The shared conversation is the *original design source*. Its authoritative parts are:

| Message | Content |
|---|---|
| #29-#35, #42-#45 | RFC-001 v1.0 / v2.0 (dual runtime, crawler extension system) |
| #63, #68, #70, #71, #73, #74 | Quality-First Local-First architecture, decision algorithm, data model, MVP |
| #82 | **Complete RFC suite, 25 RFCs + Appendix A-D** (product → security → API → release) |
| #87 | **RFC-004 v1.1 engineering specification** (state machine, Rust types, gap analysis, error matrix, test spec) |

The repo's `docs/rfc/` held a condensed **16-RFC** baseline plus annexes 17-22. The
implementation covered most of that baseline, but a comparison against #82/#87/#74
found concrete functional gaps. This document records them and what was added.

### 2. Gaps found and fixed

| # | Spec reference | Gap before | Enhancement in this pass |
|---|---|---|---|
| 1 | RFC-002 2.8; v1.1 6.12 | `SearchResponse` had **no explicit task status**; partial/insufficient results could look successful | `SearchStatus` (`completed`, `completed_with_warnings`, `partial`, `insufficient_evidence`, `budget_exhausted`, `failed`, `cancelled`) + `warnings[]`, `task_id`, `replayed`. Derived deterministically by `derive_status`. |
| 2 | v1.1 9.5 / SDE-007 / RFC-003 3.4 | **Exact cache intercepted `ExternalOnly`** and strict-freshness queries: the single most serious quality violation ("latest" could be served from history) | `should_bypass_exact_cache()` bypasses cache for `ExternalOnly`, `ExternalFirst`, temporal intent, strict profile/constraint freshness; cache hits keep the original persisted trace id. |
| 3 | v1.1 7 / 11 | No decision **state machine**, no structured **stage trace** | `searchos-decision/src/state.rs`: 20 `DecisionState`s, validated transition table, `DecisionStageTrace`; `DecisionTrace.stages/final_strategy/final_outcome` persisted in migration v2. |
| 4 | v1.1 5.3 / 6.7 / 6.8 / 10 | Only a flat `CoverageGap {question, kind}`; no requirement status, priority, resolution strategy or minimal external plan | `RequirementStatus`, `GapPriority`, `GapResolutionStrategy`, `requirement_id` on `CoverageGap`; `gap.rs` with `GapAnalyzer` + `build_external_plan()` producing targeted, budget-capped queries linked to each open requirement. |
| 5 | v1.1 10.1; RFC-018 18.2 | `LocalWithRefresh` was **unreachable dead code**, and its branch *deleted* documents from the index | Stale ids now flow from the local port into the engine; the branch serves valid local evidence and enqueues auditable `refresh` outbox events; `process_outbox` re-fetches the existing source. Authoritative knowledge is never deleted. |
| 6 | v1.1 9.4 | `ExternalFirst` was silently treated as `ExternalOnly` (no local supplement) | Distinct `ExternalFirstRequested` reason and `external_then_local` strategy; the pipeline merges local evidence for `Hybrid` / `ExternalFirst`. |
| 7 | RFC-014 14.7; v1.1 14.2 | No **request idempotency**: replaying a `request_id` re-ran and re-billed external search | `request_idempotency` table + `IdempotencyRepo`; `SearchRequest::with_request_id`; replay returns the recoverable response with `replayed: true` and creates no second task/bill. |
| 8 | v1.1 6.11 | No **evidence merge**: duplicate URLs counted as independent sources, conflicts could be dropped | `searchos-application/src/merge.rs`: canonical URL dedup (tracking params/fragment/trailing slash), freshest-version preference, conflict retention, independent-source by-host counting; unresolved conflicts force `QualityStatus::Conflict`. |
| 9 | RFC-005 5.4 | Final quality gate used a **constant 0.8 relevance** for every evidence item | Relevance is now derived from actual retrieval / provider scores. |
| 10 | v1.1 15; RFC-006 6.11 | Budget was only checked once before execution; no reserve/settle | `budget_reservations` table + `reserve → execute → settle/exceeded` lifecycle, exposed at `/cost/reservations` and `searchos cost reservations`. |
| 11 | RFC-006 6.7 | No way to import provider billing; estimate could be presented as an invoice | `cost_reconciliations` + `POST /cost/reconcile` / `searchos cost reconcile`; estimates are never rewritten, `is_estimate` stays honest. |
| 12 | RFC-006 6.9/6.10; App. C | No savings / CPQPA / four reuse cost views | `SavingsReport` + `ReuseCostViews`; `searchos cost savings`, `stats savings`, `/stats/savings`, `/cost/savings`; new metrics (`external_avoidance_rate`, `valid_local_resolution_rate`, `cost_per_quality_passing_answer_micros`). |
| 13 | RFC-013 13.3; RFC-017 17.6 | Gateway had a single bearer token, **no workspace authorization**, no rate limit, no API version | `server.workspace_tokens` (token→workspace binding), in-process `RateLimiter` (429), `/v1/*` mount + `x-searchos-api-version` header. |
| 14 | RFC-021 21.2/21.5 | Missing `GET /trace/{id}`; MCP missing `fetch`, `document_search`, `trace_inspect` | All added; MCP now also exposes `ingest_text` / `ingest_file`; tool descriptions distinguish `local_only` from external-capable modes. |
| 15 | RFC-019 19.3/19.4/19.5 | Config had 6 of 13 declared sections; invalid YAML silently fell back to defaults; no `profile` commands | All governance sections added with defaults, `validate()` + `load_config_report()`, `searchos config validate`, `profile list|inspect`, `stats retrieval|savings`, `index stats`. |
| 16 | RFC-024 24.5 | No data export | `searchos export` (documents, cost summary, redacted config, index state; secrets never exported). |
| 17 | RFC-015 15.4 | Evaluation framework existed only as a library; no way to run the 500-question dataset | `searchos eval run --dataset ... --limit N --out ...` wiring the real search service into `run_dataset` and comparing baselines A/B/C. |
| 18 | RFC-018 18.5/18.6 | Backup copied a single SQLite file; restore did no verification | Consistent backup directory (`metadata.sqlite`, redacted `config.json`, extension manifests, `manifest.json` with SHA-256); restore verifies checksum + `PRAGMA integrity_check` before overwriting. |
| 19 | RFC-002 2.8; v1.1 13.2 | Offline mode silently downgraded an external request into a "local" decision | Explicit `Reject { ProviderUnavailable }` → `insufficient_evidence`, with a warning. Never disguised as success. |
| 20 | #82 RFC-017-025; #87 v1.1 | Annexes 17-22 did not carry the full security/reliability/API/governance/engineering spec | New annexes [Part 23](rfc/part-23-decision-engineering-baseline.md) and [Part 24](rfc/part-24-api-governance-baseline.md) (bilingual), plus this gap analysis. |

### 3. Visual interface

The original architecture conversation never specified a browser UI. A later
UI design share (`6af054b8-a43c-83ee-9218-d443cbac9f47`) added RFC-026/027/028:
a first-class Visualization & Analytics Layer, an Analytics Read Model, a
Dashboard API and a concrete frontend stack. Before that work the repo had
**no visual interface** (only CLI text, MCP JSON-RPC and gateway JSON). The
console under `web/` initially used Next.js + native CSS, then was upgraded to
Next.js 16 + Tailwind v4 + shadcn/ui + Radix + Recharts + TanStack Query +
next-themes, with the Rust Dashboard API behind it. See [`docs/ui.md`](ui.md)
and RFC-026/027/028. This is a new architecture layer, not a missing item in
the original 25 RFCs.

### 4. Deliberately not implemented (per the source spec)

These are *future-stage* in the conversation itself, not omissions:
WASM extension runtime (ADR-024), multi-node Gateway / PostgreSQL / message broker
(RFC-020, v1.5), managed multi-tenant RBAC/SSO (v2.0), cross-region vector
replication, proprietary global web index, self-trained embedding/LLM, automatic
LLM-driven decisions, and unlimited query expansion. `answer_generation` is
actively rejected by config validation because SearchOS returns evidence, not answers.

### 5. Verification

```bash
cargo test --workspace                 # all suites green (CI tracks the count)
cargo clippy --workspace --all-targets # 0 warnings
cargo fmt --all -- --check
cargo build --bin searchos
SEARCHOS_DATA_DIR=$(mktemp -d) searchos --format json config validate
SEARCHOS_DATA_DIR=$(mktemp -d) searchos --format json eval run --limit 2
SRC_DIR=crates/searchos-interface-http/src \
  bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh  # no conflicts
```

---

## 中文

### 1. 分析方法

分享链接里的对话是项目的**原始设计源**。其中 #82 是完整的 25 份 RFC 基线，
#87 是 RFC-004 v1.1 的工程化细化，#74 是 MVP 验收口径。仓库 `docs/rfc/` 原先
是浓缩后的 16 份 RFC + 附录 17-22。对照后发现上表 20 项功能缺口，均已补强；Part 30
的 21 条执行正确性不变量也已落地并通过命名验收测试。

### 2. 可视化界面

原始架构对话从未定义浏览器 UI；后续 UI 专项分享新增 RFC-026/027/028，要求
独立的 Visualization & Analytics Layer、Analytics Read Model、Dashboard API
与明确前端栈。本次之前仓库**没有任何视觉界面**，只有 CLI 文本、MCP JSON-RPC
与网关 JSON。`web/` 控制台先以 Next.js + 原生 CSS 落地，再升级为 Next.js 16 +
Tailwind v4 + shadcn/ui + Radix + Recharts + TanStack Query + next-themes，
并在 Rust 侧补齐 Dashboard API（见 [`docs/ui.md`](ui.md)）。这是新增架构层，
不是原始 25 份 RFC 的遗漏项。

### 3. 最值得关注的三个修护

1. **质量红线**：`ExternalOnly` 与「latest / current」类查询过去会被精确缓存
   静默拦截，可能拿旧结果冒充新结果。现在强制绕过缓存，且所有响应带显式
   `status`，部分成功绝不会被标成 `completed`。
2. **成本闭环**：补齐了预算预留→执行→结算、Provider 账单对账、节省率、
   CPQPA 与四种复用成本视图；本地计算成本不再被当成零。
3. **决策可解释**：状态机拒绝非法跳转；每次决策产生结构化阶段轨迹；
   复杂查询拆成 InformationNeed，外部搜索只买缺失/过期的那部分。

### 4. 有意不做的部分

WASM 扩展、Gateway 集群、多租户 RBAC/SSO、自研向量库/Embedding/LLM、
全自动 LLM 决策与无限查询扩展，在源对话里本来就是后续阶段或明确排除项，
不属于本次遗漏。配置校验会主动拒绝 `answer_generation`。

### 5. 验收

见上方命令；`cargo test --workspace` 全部套件通过，网关路由扫描无冲突。
