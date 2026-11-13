# RFC-063: Search Source Registry

> 来源注册表

**Architecture Baseline:** SearchOS RFC Master Index (final, RFC-001…087)
**Document Status:** Implemented (in-process kernel) / Engineering Baseline
**Category:** Core Architecture
**Priority:** P0
**Series:** RFC Master Index, group D (062–068)
**Source:** `6af69118-d0f4-83ee-bbbb-948eeeb623c8` turn #81 (final catalogue) · implementation in `crates/searchos-federated/src/registry.rs`
**Depends on:** RFC-064 (Unified Resource Model, same crate), RFC-062 (the planner is the registry's only consumer)
**Related:** [`rfc-master-index.md`](rfc-master-index.md) §5 (ownership: Search Source is owned **only** here), [`../rfc-062-dependency-review.md`](../rfc-062-dependency-review.md), [`../gap-analysis-master-index.md`](../gap-analysis-master-index.md)

---

## English

### 1. Context

v1.x had two ways into retrieval: `LocalKnowledgePort` for local knowledge and
`ProviderRouter` for everything external, both hard-wired branches in the
search service. A Source Registry is the single place a source enters the
system, so adding the fifth source becomes *registration*, not a pipeline edit
— and so an unregistered or unauthorized source cannot exist downstream.

### 2. Goals

| ID | Goal |
|----|------|
| G1 | One registry: register / unregister / resolve / list / enable / disable / health. |
| G2 | Scope precedence `Agent > Project > Workspace > Global` for overrides. |
| G3 | Capability-declared discovery: the planner only asks for declared operations. |
| G4 | Deterministic, auditable listing (priority, then id). |
| G5 | No duplicate or silently-replaced registrations; no scope escape. |

### 3. Non-goals

| ID | Non-goal |
|----|----------|
| N1 | Ranking or fusion (RFC-066). |
| N2 | Storage schema — the in-process registry satisfies Local Mode; a SQLite projection comes with the Gateway surface (route + OpenAPI in the same change). |
| N3 | Remote/distributed registries (RFC-084/085, Staged). |

### 4. Architecture

```rust
pub struct InMemorySourceRegistry { /* RwLock<HashMap<id, Vec<Registration>>> */ }

impl InMemorySourceRegistry {
    pub fn register(&self, source: Arc<dyn SearchSource>) -> Result<(), RegistryError>;
    pub fn unregister(&self, source_id: &SourceId) -> Result<(), RegistryError>;
    pub fn get(&self, source_id: &SourceId, requester: &WorkspaceId)
        -> Option<Arc<dyn SearchSource>>;
    pub fn descriptor_for(&self, source_id: &SourceId, requester: &WorkspaceId)
        -> Option<SearchSourceDescriptor>;
    pub fn set_enabled(&self, source_id: &SourceId, enabled: bool) -> Result<(), RegistryError>;
    pub fn set_health(&self, source_id: &SourceId, health: SourceHealth);
    pub fn list(&self, filter: &SourceFilter, requester: &WorkspaceId)
        -> Vec<SearchSourceDescriptor>;
    pub fn all_references(&self) -> Vec<SourceDescriptorRef>; // diagnostics only
}
```

Rules the implementation enforces (each one has a named test):

| Rule | Test |
|---|---|
| One id may be registered at several scopes; resolution picks the highest precedence visible to the requester | `scope_precedence_cannot_relax_permissions`, `agent_scope_overrides_workspace_scope` |
| A workspace-bound registration is invisible to other workspaces | `workspace_scoped_source_is_invisible_to_other_workspaces` |
| Same-scope re-registration fails loudly instead of silently replacing a live implementation | `duplicate_same_scope_registration_is_rejected_not_silently_replaced` |
| Disable applies to **every** scope registration of the id | `disabling_a_source_hides_it_from_every_scope_registration` |
| One id contributes exactly one descriptor to `list` (overrides never count as a second, independent source) | `catalog_resolves_one_descriptor_per_id` |
| Listing is deterministic (priority desc, then id) | enforced in `list` |

### 5. Interface contract

* The planner receives descriptors **only** from `list` / `descriptor_for`; an
  ad-hoc source list is a contract violation (the executor must refuse it).
* Health is bookkeeping, not admission control: a `Down` source is rejected by
  the planner's reliability stage, not hidden by the registry.
* `SourceFilter` capability flags (`require_lexical` / `require_semantic` /
  `require_provenance`) filter on the *declared* capabilities; nothing is
  assumed.

### 6. Failure handling

`RegistryError::AlreadyRegistered` / `NotFound` are programming errors at the
call site: they are returned, never swallowed. A poisoned lock degrades to an
empty catalog for `list` (read path) and errors for writes.

### 7. Security

* Visibility is workspace-scoped by construction (`SourceScope::serves`), so
  cross-workspace leakage requires mislabelling a scope, which the visibility
  tests catch.
* Health/diagnostics (`all_references`) expose descriptors to operators only.

### 8. Acceptance criteria

* All six rule-table tests pass.
* `list` output is deterministic across runs.
* Registry has no I/O and no async requirement beyond the source trait.

### 9. Implementation tasks

| Phase | Task | Status |
|---|---|---|
| 1 | In-process registry + scope precedence + filters | **Done** |
| 2 | SQLite projection `search_sources` + `GET /sources` + OpenAPI | Planned (Gateway surface) |
| 3 | Health probe loop wiring `SearchSource::health_check` | Planned |

---

## 中文

### 1. 背景

v1.x 有两条进入检索的路径：本地知识走 `LocalKnowledgePort`，外部走
`ProviderRouter`，都是检索服务里的硬连线分支。来源注册表是来源进入系统的
**唯一**入口：新增第五个来源变成「注册」，而不是改管线；未注册或未授权的
来源在下游根本不存在。

### 2. 目标

| 编号 | 目标 |
|----|------|
| G1 | 一个注册表：注册 / 注销 / 解析 / 列举 / 启停 / 健康。 |
| G2 | 作用域优先级 `Agent > Project > Workspace > Global`。 |
| G3 | 按声明能力发现：planner 只能要求来源声明过的操作。 |
| G4 | 列举确定性（先 priority 后 id）。 |
| G5 | 不允许重复注册或静默替换实现；不允许作用域逃逸。 |

### 3. 非目标

| 编号 | 非目标 |
|----|----------|
| N1 | 排序与融合（RFC-066）。 |
| N2 | 存储结构 —— 进程内注册表满足 Local Mode；SQLite 投影随 Gateway 一起交付（路由 + OpenAPI 同一提交）。 |
| N3 | 远程 / 分布式注册表（RFC-084/085，Staged）。 |

### 4. 架构

接口见英文 §4。实现强制以下规则（每条都有具名测试）：

| 规则 | 测试 |
|---|---|
| 同一 id 可注册在多个作用域；解析取对请求者可见的最高优先级 | `scope_precedence_cannot_relax_permissions`、`agent_scope_overrides_workspace_scope` |
| workspace 级注册对其它 workspace 不可见 | `workspace_scoped_source_is_invisible_to_other_workspaces` |
| 同作用域重复注册报错，而不是静默替换线上实现 | `duplicate_same_scope_registration_is_rejected_not_silently_replaced` |
| 禁用对该 id 的**所有**作用域注册生效 | `disabling_a_source_hides_it_from_every_scope_registration` |
| 一个 id 在 `list` 中只贡献一个描述符（覆盖不等于第二个独立来源） | `catalog_resolves_one_descriptor_per_id` |
| 列举确定性（priority 降序，再按 id） | 在 `list` 内强制 |

### 5. 接口契约

* planner 只能从 `list` / `descriptor_for` 获取描述符；把临时来源列表递给
  执行器属于契约违规，执行器必须拒绝。
* 健康状态是记账，不是准入控制：`Down` 来源由 planner 的可靠性阶段拒绝，
  注册表不隐藏它。
* `SourceFilter` 的能力开关只过滤**已声明**能力，不做任何假设。

### 6. 失败处理

`RegistryError::AlreadyRegistered` / `NotFound` 是调用方的编程错误：返回错误，
绝不吞掉。锁中毒时读路径降级为空目录，写路径报错。

### 7. 安全

* 可见性由 `SourceScope::serves` 按构造保证 workspace 隔离；跨 workspace 泄漏
  需要错误标注作用域，而可见性测试会抓住它。
* 健康与诊断（`all_references`）只暴露给运维。

### 8. 验收条件

* 规则表中的六项测试全部通过。
* `list` 输出跨次运行确定。
* 注册表无 I/O；除来源 trait 外无异步要求。

### 9. 实施任务

| 阶段 | 任务 | 状态 |
|---|---|---|
| 1 | 进程内注册表 + 作用域优先级 + 过滤器 | **已完成** |
| 2 | SQLite 投影 `search_sources` + `GET /sources` + OpenAPI | 计划中（Gateway 交付面） |
| 3 | 健康探测循环接入 `SearchSource::health_check` | 计划中 |
