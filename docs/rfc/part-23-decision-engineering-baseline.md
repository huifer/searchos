# Part 23. Search Decision Engine — Engineering Baseline (RFC-004 v1.1)

> 搜索决策引擎工程基线

## English

This annex records the engineering-level contract from the source conversation
(message #87) that the condensed RFC-004 did not carry. It is normative for
`crates/searchos-decision/`, `crates/searchos-application/src/search_service.rs`
and the migration-v2 tables.

### 23.1 Principle

> Quality is a constraint. Cost is an optimization target.

The engine is a control system, not a router. It decides whether local evidence
can satisfy the request, which information is missing, and what is the smallest
external acquisition that can close the gap. It never generates final answers,
never treats semantic similarity as factual equivalence, and never relaxes a
quality threshold to fit a budget.

### 23.2 Requirement model

```rust
pub enum RequirementStatus { Missing, PartiallyCovered, Covered, Conflicting, NotApplicable }
pub enum GapPriority { Critical, High, Normal, Low }
pub enum GapResolutionStrategy {
    ReuseExistingEvidence, RefreshExistingSource, SearchOfficialSource,
    SearchIndependentSources, FetchExistingUrl, CrawlDomain,
    SearchAlternativeSources,
}
```

`Conflicting` must never be converted to `Covered`. Each complex query is split
into `InformationNeed`s, each evaluated independently. `GapAnalyzer` is
deterministic (token overlap over accepted local evidence); LLM-based query
understanding remains an optional Intelligence Extension.

### 23.3 Decision state machine

States: `Received → Validating → PolicyResolved → QueryUnderstood →
LocalRetrieving → LocalEvaluating → QualityChecking →` then either `Completed`
or `GapAnalyzing → PlanningExternal → SelectingProvider → ExecutingExternal →
MergingEvidence → FinalQualityChecking → PersistingKnowledge → Completed`.
Terminal branches: `Partial`, `InsufficientEvidence`, `BudgetExceeded`,
`Failed`, `Cancelled`. `transition_allowed(from, to)` rejects illegal jumps;
cancellation propagates from any active state.

### 23.4 Structured trace

`DecisionTrace` carries `stages: Vec<DecisionStageTrace>` with
`{stage, status, started_at_ms, duration_ms, input_summary, output_summary,
reasons}`, plus `final_strategy` and `final_outcome`. Unused stages are marked
`skipped`, not omitted. Traces never contain API keys, authorization headers
or private document bodies.

### 23.5 Mode semantics

| Mode | Local | External | Cache |
|---|---|---|---|
| `LocalOnly` | yes | **never** | allowed; failure → `insufficient_evidence` |
| `LocalFirst` | first | on quality failure | allowed unless strict freshness |
| `Hybrid` | yes, merged | yes | allowed unless strict freshness |
| `ExternalFirst` | supplement | primary | **bypassed** |
| `ExternalOnly` | no | always | **bypassed** |

Offline / provider-unavailable execution is reported as
`Reject { ProviderUnavailable }` → `insufficient_evidence`, never disguised as
a local success.

### 23.6 Gap-based external plan

`build_external_plan()` emits one `ExternalQueryTask` per open requirement,
each linked to its `requirement_id`. Hard caps: `max_queries`,
`max_cost_micros`; `Critical` gaps first; `RefreshExistingSource` targets
`"<need> current latest"`, `SearchOfficialSource` appends
`"official documentation"`, `FetchExistingUrl` reuses the URL. Covered
requirements are never re-purchased.

### 23.7 Evidence merge

Canonical URL dedup → version-aware keep-newest → conflict retention →
independent-source-by-host counting. Unresolved conflicts force
`QualityStatus::Conflict` and a `Partial` task status. The final quality gate
uses measured relevance scores, never a fixed placeholder.

### 23.8 Budget lifecycle

```
Estimated Cost <= Remaining Budget
  → reserve → execute → record actual → settle (release unused)
```
Budget exhaustion returns an explicit status. It never lowers quality
thresholds, never uses prohibited sources, and never marks incomplete work
successful. Estimated and actual costs are separate; provider billing is
reconciled by appending adjustments, never by rewriting the estimate.

### 23.9 Idempotency & concurrency

`request_id` is the client idempotency key. A replay returns the persisted
recoverable response (`replayed: true`) without a second external call, a
second bill, or a second document version. concurrent equivalent requests are
coalesced by Singleflight; each caller keeps its own policy evaluation.

### 23.10 Acceptance tests (SDE / Q / C / property)

`SDE-001…007` (five modes, ExternalOnly + cache hit still executes),
`Q-001…008` (stale, untrusted source, required domain, conflict, duplicate
canonical docs, provenance), `C-001…007` (targeted gap search, budget,
fallback, singleflight, persistence retry), plus property tests:
LocalOnly never produces an external plan, no hard-constraint failure can
produce a quality pass, different workspaces do not share private evidence,
and budget exhaustion never lowers a threshold.

---

## 中文

本附录补齐浓缩版 RFC-004 中缺失的工程级契约，是决策引擎、搜索编排与 v2
迁移表的规范依据。

### 23.1 核心原则

**质量是硬约束，成本是优化目标。** 决策引擎不是路由器：它判断本地证据是否
够用、缺什么、最少需要买什么。它不生成最终答案、不把语义相似当成事实等价、
也不为了省钱放宽质量阈值。

### 23.2 信息需求与缺口

复杂 Query 拆成多个 `InformationNeed`，每个维度独立评估覆盖状态
（含 `Conflicting`，不得转成 `Covered`）。缺口带优先级与最小采集策略；
`RefreshExistingSource` 优先于重新全额搜索。首版用确定性规则，不依赖 LLM。

### 23.3 状态机与轨迹

20 个状态、显式合法跳转表，非法跳转被拒绝；取消可从任意活动状态传播。
每次决策写结构化阶段轨迹（未执行的阶段标 `skipped`），并持久化
`final_strategy` / `final_outcome`；日志不写密钥和私有正文。

### 23.4 模式语义与缓存红线

`ExternalOnly` 与 `ExternalFirst` 必须绕过精确缓存；含「latest / current」
等强时效意图的查询同样绕过。离线或 Provider 不可用时显式返回
`insufficient_evidence`，绝不伪装成本地命中。

### 23.5 缺口驱动的外部计划

一个开放需求对应一个定向外部 Query，并保留 `requirement_id`；查询数与成本
都是硬上限；已覆盖的需求不再重复付费。

### 23.6 证据合并与最终质量门

URL 规范化去重、版本保留、冲突保留、独立来源按域名计数；未解决冲突强制
`Conflict` 与 `Partial`。最终质量门使用真实相关性分数，不使用占位常量。

### 23.7 预算、幂等、并发

预留→执行→结算；预算不足显式返回，绝不降阈值。估算与实际账单分离，账单
通过对账追加调整、不改写估算。`request_id` 保证重复提交不产生第二次外部
搜索/计费/文档版本；并发相同请求由 Singleflight 合并，每个调用方仍各自做
策略评估。

### 23.8 验收

SDE / Q / C / property 四组测试（见 23.10），覆盖五种模式、陈旧证据、
官方来源约束、冲突证据、重复规范化文档、预算、Singleflight 与幂等。
