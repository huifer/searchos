# RFC-013: Memory & Cross-Agent Reuse

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 13.1 Memory 与 Cache 的区别

| 类型              | 生命周期 | 作用        |
| --------------- | ---- | --------- |
| Query Cache     | 短期   | 搜索结果复用    |
| Document Store  | 长期   | 保存网页内容    |
| Search Memory   | 中长期  | 搜索任务历史和证据 |
| Research Memory | 项目周期 | 持续研究资料    |
| Semantic Memory | 长期   | 语义知识检索    |

Cache 不是 Memory。

Memory 不是最终答案缓存。

## 13.2 Namespace

```
Global
  |
  +---- Workspace
          |
          +---- Project
                  |
                  +---- Agent
```

支持共享策略：

- Private。
- Workspace Shared。
- Project Shared。
- Global Public Knowledge。

默认不跨 Workspace 泄露私人知识。

## 13.3 Memory Search

检索流程：

```
Query
  |
  v
Metadata Filter
  |
  v
BM25 + Vector
  |
  v
RRF
  |
  v
Optional Reranker
  |
  v
Freshness Check
  |
  v
Evidence Return
```

## 13.4 Cross-Agent Reuse

例如：

```
Agent A:
  Search Rust Frameworks
      |
      v
  Persist Documents
      |
      v
  Build Evidence Set

Agent B:
  Search Rust Framework Comparison
      |
      v
  Retrieve Agent A Evidence
      |
      v
  Identify Coverage Gap
      |
      v
  Search Missing Information Only
```

跨 Agent 复用必须满足：

- Workspace 授权。
- Profile 兼容。
- Freshness 合格。
- Source Policy 合格。
- Evidence Coverage 合格。

## 13.5 Memory Governance

支持：

- Namespace Isolation。
- Retention Policy。
- Delete。
- Export。
- Backup。
- Restore。
- Version History。
- Re-Embedding。
