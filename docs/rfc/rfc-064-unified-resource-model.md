# RFC-064: Unified Resource Model

> 统一资源模型

**Architecture Baseline:** SearchOS RFC Master Index (final, RFC-001…087)
**Document Status:** Implemented (kernel) / Engineering Baseline
**Category:** Core Architecture
**Priority:** P0
**Series:** RFC Master Index, group D (062–068)
**Source:** `6af69118-d0f4-83ee-bbbb-948eeeb623c8` turn #81 (final catalogue) · implementation in `crates/searchos-federated/src/resource.rs`
**Depends on:** none (leaf model consumed by RFC-062/063/066/067)
**Related:** [`rfc-master-index.md`](rfc-master-index.md) §5 (Search Resource is owned **only** here), [`rfc-062-federated-search-architecture.md`](rfc-062-federated-search-architecture.md) §9.3, [`../rfc-062-dependency-review.md`](../rfc-062-dependency-review.md)

---

## English

### 1. Context

Not every retrievable thing is a `Document`. A session message, a tool
execution, an API record, a knowledge-graph entity and a historical search
result all need to be retrievable, deduplicable, permission-checked and
citable. v1.x forced all of them through `Document` + `DocumentVersion` +
`DocumentChunk`, which is why adding a source meant editing the pipeline.

`SearchResource` is the shared identity layer. It is **not** a storage schema
and does not replace `Document` — RFC-006/RFC-029 keep the document lifecycle,
versioning and rebuild. It is the common shape that sources return and that
fusion, dedup, privacy and context assembly reason over.

### 2. Goals

| ID | Goal |
|----|------|
| G1 | Eleven resource kinds (`WebPage … SearchResult`) with source-category cross-checks. |
| G2 | Identity stable across sources: same page seen by two sources is one resource, two discoveries. |
| G3 | Dedup identity prefers content hash over reference; two versions of one URL are two resources. |
| G4 | Permission carried on the resource, fail-closed until verified; agent allow-list; egress forbidden by default. |
| G5 | User-owned material is not age-stale (RFC-029 G4, generalised). |

### 3. Non-goals

| ID | Non-goal |
|----|----------|
| N1 | Storage schema / migrations — resources are in-memory contracts until adapters map real rows. |
| N2 | Cryptographic identity — resource id is a dedup/identity tag (FNV-1a), explicitly documented as non-security; content integrity stays on the storage layer's SHA-256. |
| N3 | Answer generation. |

### 4. Domain model

```rust
pub enum ResourceType { WebPage, Document, File, Transcript, Message,
                        ToolExecution, ApiRecord, DatabaseRecord,
                        KnowledgeEntity, KnowledgeFact, SearchResult }

pub struct SearchResource {
    pub resource_id: String,            // stable: digest(type | canonical_ref)
    pub resource_type: ResourceType,
    pub source_category: SourceCategory,// cross-checked via category_matches_type()
    pub workspace_id: String,
    pub project_id: Option<String>,
    pub canonical_ref: Option<String>,  // URL / URI / searchos-local://…
    pub content_hash: Option<String>,
    pub title: Option<String>,
    pub content_ref: ContentRef,        // Inline | ObjectStore | Remote | ReferenceOnly
    pub observed_at: DateTime<Utc>,
    pub published_at: Option<DateTime<Utc>>,
    pub updated_at: Option<DateTime<Utc>>,
    pub permission: PermissionRef,
    pub provenance_ref: Option<String>,
    pub language: Option<String>,
}

pub struct PermissionRef {
    pub workspace_id: String,
    pub allowed_agents: Vec<String>,    // empty = every agent in the workspace
    pub egress_allowed: bool,           // RFC-087 boundary
    pub verified: bool,                 // false must block context assembly
}
```

Key operations (all tested):

| Operation | Rule | Test |
|---|---|---|
| `SearchResource::new` | id derived from (type, canonical ref) → identical across sources | `resource_identity_is_stable_across_sources` |
| `dedup_key()` | `hash:<sha>` when a content hash exists, else `ref:<canonicalized>` | `dedup_prefers_content_hash_over_reference` |
| `canonicalize_reference` | drop fragment, strip trailing slash, lowercase scheme+host — dedup-key only; the authoritative canonicaliser stays in `merge.rs` | `canonical_reference_drops_fragment_and_trailing_slash` |
| `context_eligible(agent)` | `permission.verified && workspace matches && agent allow-list` | `permission_is_fail_closed_until_verified`, `agent_allow_list_restricts_across_workspace`, `cross_workspace_permission_is_denied` |
| `PermissionRef::egress_allowed` | default `false`: sending local content outward is an explicit grant | `egress_default_is_forbidden` |
| `category_matches_type()` | adapter fails fast on a mislabelled mapping | `category_and_type_are_consistent` |
| `ResourceType::is_user_owned` | user material is not stale because it is old | `user_owned_types_are_not_age_stale` |
| serde round-trip | additive evolution only | `serde_round_trip_preserves_identity` |

### 5. Interface contract

* Sources return `SearchResource`-shaped identity alongside candidates; the
  response-level `SearchResultItem` stays unchanged in v1 (additive rule).
* `occurrences` (count of source observations) is **provenance, not
  independence** — three reposts are one resource (RFC-074 builds on this).
* Permission is inherited when knowledge is shared: one agent's access never
  widens the workspace's.

### 6. Failure handling

An unverified permission or a cross-workspace reference must never enter an
Agent context; the assembler drops it and records the drop (RFC-067's
`permission_denied` reason). There is no "unknown, probably fine".

### 7. Security

* `verified: false` blocks rendering; only the transport of the reference is
  allowed.
* `egress_allowed: false` is the RFC-087 boundary: ingestion-time redaction
  and egress policy will consume this field, not redefine it.

### 8. Acceptance criteria

* All ten model tests above pass.
* No I/O, no async, serde-only dependencies — the model is usable from every
  crate in the workspace without layering violations.

### 9. Implementation tasks

| Phase | Task | Status |
|---|---|---|
| 1 | Model + identity + permission + tests | **Done** |
| 2 | Adapters: document/session rows → `SearchResource` | Planned (with RFC-062 Phase 2) |
| 3 | Dedup fusion across sources keyed by `dedup_key()` with occurrences | Planned (RFC-066/074) |

---

## 中文

### 1. 背景

并非一切可检索之物都是 `Document`。会话消息、工具执行、API 记录、知识图谱
实体、历史搜索结果都需要可检索、可去重、可鉴权、可引用。v1.x 把它们全部
塞进 `Document` + `DocumentVersion` + `DocumentChunk`，导致新增来源就要改管线。

`SearchResource` 是共享身份层。它**不是**存储结构，也不取代 `Document` ——
文档生命周期、版本与重建仍归 RFC-006/RFC-029。它是来源返回、融合/去重/隐私/
上下文共用的公共形状。

### 2. 目标

| 编号 | 目标 |
|----|------|
| G1 | 11 种资源类型，并与来源类别做交叉校验。 |
| G2 | 身份跨来源稳定：同一页面被两个来源发现，是一个资源的两次观察。 |
| G3 | 去重身份优先内容哈希；同一 URL 的两个版本是两个资源。 |
| G4 | 权限随资源携带，未验证前 fail-closed；agent 白名单；egress 默认禁止。 |
| G5 | 用户自有材料不因年龄而「过期」（RFC-029 G4 的推广）。 |

### 3. 非目标

| 编号 | 非目标 |
|----|----------|
| N1 | 存储结构 / 迁移 —— 在适配器映射真实行之前，资源只是内存契约。 |
| N2 | 加密身份 —— resource id 是去重/身份标记（FNV-1a），文档明确其非安全属性；内容完整性仍在存储层 SHA-256。 |
| N3 | 答案生成。 |

### 4. 领域模型

见英文 §4。关键操作与规则（全部有测试）：

| 操作 | 规则 | 测试 |
|---|---|---|
| `SearchResource::new` | id 由 (type, canonical ref) 派生 → 跨来源一致 | `resource_identity_is_stable_across_sources` |
| `dedup_key()` | 有内容哈希用 `hash:<sha>`，否则 `ref:<canonicalized>` | `dedup_prefers_content_hash_over_reference` |
| `canonicalize_reference` | 去片段、去尾斜杠、scheme+host 小写 —— 仅用于去重键；权威规范化仍在 `merge.rs` | `canonical_reference_drops_fragment_and_trailing_slash` |
| `context_eligible(agent)` | `verified && workspace 匹配 && agent 白名单` | `permission_is_fail_closed_until_verified` 等 |
| `egress_allowed` | 默认 `false`：外发本地内容必须是显式授权 | `egress_default_is_forbidden` |
| `category_matches_type()` | 标注错误的映射立即失败 | `category_and_type_are_consistent` |
| `is_user_owned` | 用户材料不因年龄变「陈旧」 | `user_owned_types_are_not_age_stale` |
| serde round-trip | 只做加法演进 | `serde_round_trip_preserves_identity` |

### 5. 接口契约

* 来源在候选之外返回 `SearchResource` 形状的身份；v1 的响应层
  `SearchResultItem` 保持不变（加法规则）。
* `occurrences`（来源观察次数）是 **provenance，不是独立性** —— 三个转载是
  一个资源（RFC-074 在此之上）。
* 知识共享时权限继承：一个 agent 的访问权限永不扩大整个 workspace 的。

### 6. 失败处理

未验证的权限或跨 workspace 引用绝不进入 Agent 上下文；组装器丢弃并记录
（RFC-067 的 `permission_denied` 原因）。不存在「未知，但应该没事」。

### 7. 安全

* `verified: false` 阻止渲染；只允许传输引用本身。
* `egress_allowed: false` 是 RFC-087 的边界：摄取期脱敏与外发策略消费该
  字段，而不是重新定义它。

### 8. 验收条件

* 上述十项模型测试全部通过。
* 无 I/O、无 async、仅 serde 依赖 —— workspace 内任何 crate 都能在不违反
  分层规则的前提下使用该模型。

### 9. 实施任务

| 阶段 | 任务 | 状态 |
|---|---|---|
| 1 | 模型 + 身份 + 权限 + 测试 | **已完成** |
| 2 | 适配器：文档/会话行 → `SearchResource` | 计划中（随 RFC-062 Phase 2） |
| 3 | 以 `dedup_key()` 为键的跨源去重融合（含 occurrences） | 计划中（RFC-066/074） |
