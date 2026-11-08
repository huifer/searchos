# SearchOS 完整技术 RFC 套件

> Architecture Baseline v1.0
> Rust Native · Local-First · Quality-First

> SearchOS 的任务不是尽可能少搜索，而是在满足质量约束的前提下，选择总成本最低的搜索执行路径，并让每次外部搜索产生的知识能够被后续任务复用。

这里需要区分三个结果：

1. Search Result：搜索引擎返回的结果。
2. Evidence Set：经过质量、时效性和来源验证的证据集合。
3. Answer：由上层 Agent 根据证据生成的最终答案。

SearchOS 负责前两者及其成本优化，不在核心产品中重新实现一个通用 LLM Agent。

本套 RFC 以你已经确定的条件为约束：

- Local Mode 必须是真正的 Rust In-Process Runtime，不允许 CLI 通过 localhost HTTP 回环。
- Gateway Mode 使用 HTTP，兼容 Tavily 的核心 Search / Extract 接口。
- Local 与 Gateway 共用同一套 Core 和 Application Use Cases。
- 本地知识优先，但质量硬约束高于成本。
- 成本统计覆盖 Search、Fetch、Crawl、Embedding、Reranking、CPU/GPU、存储和网络。
- 缓存、文档、Memory、搜索结果、成本账本必须分离。
- 网站专属爬虫按独立 Extension 维护，必须支持故障隔离和降级。
- SQLite、Tantivy、LanceDB 和文件系统构成第一阶段单机存储。
- 不以牺牲答案质量换取缓存命中率或成本下降。

---

# Part 0. RFC Governance

## 0.1 RFC Index

整个工程规范分为 16 份 RFC。

| RFC     | 名称                                        | 主要解决的问题                  | 优先级 |
| ------- | ----------------------------------------- | ------------------------ | --- |
| RFC-001 | Product Architecture & System Boundaries  | 产品定位、总体架构、边界             | P0  |
| RFC-002 | Core Domain & Rust Workspace              | Crate 划分、领域模型、Trait      | P0  |
| RFC-003 | Search Request & Query Intelligence       | 查询标准化、意图识别、Profile       | P0  |
| RFC-004 | Search Decision Engine                    | 本地与外部搜索决策                | P0  |
| RFC-005 | Quality Gate & Evidence Evaluation        | 质量、时效性、来源、覆盖度            | P0  |
| RFC-006 | Knowledge Store & Document Lifecycle      | 文档、版本、内容持久化              | P0  |
| RFC-007 | Retrieval & Cache Architecture            | Cache、BM25、Vector、Hybrid | P0  |
| RFC-008 | Cost Accounting & Cost Model              | 全链路成本核算                  | P0  |
| RFC-009 | Execution Planner & Provider Router       | 执行计划、Provider 选择         | P0  |
| RFC-010 | Web Fetch & Crawler Extension             | 抓取、网站专属爬虫                | P1  |
| RFC-011 | Extension System                          | 扩展协议、生命周期、权限             | P1  |
| RFC-012 | Local Runtime & Gateway Runtime           | 双运行模式、API、MCP            | P0  |
| RFC-013 | Memory & Cross-Agent Reuse                | 长期知识和跨 Agent 复用          | P1  |
| RFC-014 | Security & Reliability                    | 安全、故障隔离、恢复               | P0  |
| RFC-015 | Observability & Evaluation                | 质量评估、成本指标、实验             | P0  |
| RFC-016 | Deployment, Release & Engineering Roadmap | 构建、发布、部署、实施计划            | P0  |

> 源对话包含 25 份基线 RFC、RFC-004 v1.1 工程细化，以及后续 UI 专项的
> RFC-026/027/028。上述 16 份是浓缩基线；附录 17–24 补齐数据库、CLI、ADR、
> DoD、执行流程、最终决策、决策引擎工程基线、API / 安全 / 可靠性 / 治理基线；
> 新增 RFC-026–028 定义可视化与分析层、Analytics Read Model、Dashboard API
> 与前端集成。功能遗漏分析见 [`docs/gap-analysis.md`](../gap-analysis.md)。

| 附录 | 名称 |
| --- | --- |
| Part 17 | Database Schema Baseline |
| Part 18 | CLI Command Contract |
| Part 19 | ADR Decisions |
| Part 20 | Definition of Done |
| Part 21 | GitHub Engineering Execution |
| Part 22 | Final Architecture Decisions |
| Part 23 | Search Decision Engine — Engineering Baseline (RFC-004 v1.1) |
| Part 24 | Product API, Security, Reliability & Governance Baseline (RFC-017…025) |
| RFC-026 | Visualization & Analytics Architecture |
| RFC-027 | Analytics Data Model & Aggregation |
| RFC-028 | Dashboard API & Frontend Integration |

## 0.2 RFC 规范

每份 RFC 必须包含：

1. Status
2. Context
3. Goals
4. Non-goals
5. Architecture
6. Domain Model
7. Interface Contract
8. Data Model
9. Failure Handling
10. Security Considerations
11. Performance Considerations
12. Acceptance Criteria
13. Dependencies
14. Implementation Tasks

所有重大架构修改必须提交 ADR。

RFC 不允许仅描述功能目标而缺少可验证的接口、数据结构和验收条件。

## 0.3 核心设计原则

| 编号    | 原则                    | 约束                   |
| ----- | --------------------- | -------------------- |
| P-001 | Quality First         | 质量约束不能被成本策略覆盖        |
| P-002 | Local First           | 默认优先评估本地知识           |
| P-003 | In-Process Local      | Local Mode 不依赖 HTTP  |
| P-004 | Shared Core           | 两种运行模式共用 Application |
| P-005 | Cost Transparency     | 每个操作必须有成本归属          |
| P-006 | Evidence Preservation | 结果必须保留来源和证据          |
| P-007 | Incremental Reuse     | 优先复用未变化的内容           |
| P-008 | Pluggable Provider    | 外部服务可替换              |
| P-009 | Extension Isolation   | 扩展不能破坏核心 Runtime     |
| P-010 | Rebuildable Index     | 检索索引必须可以重建           |
| P-011 | Explainable Decision  | 每次搜索决策可追踪            |
| P-012 | Bounded Execution     | 所有任务都有资源和预算边界        |

---

# 交付后的工程判断

这套规范已经覆盖产品边界、Rust Workspace、决策算法、质量门控、知识库、成本模型、Provider、爬虫、扩展、双运行模式、安全、评估和开发流程。

但在真正开始编码前，需要明确三个容易影响后续返工的架构问题：

1. **本地计算成本计量精度**：MVP 先统计 CPU/GPU 时间、内存、I/O 和电力估算，不必一开始做精确硬件折旧。
2. **跨 Agent 知识隔离**：默认 Workspace 隔离，显式授权后才共享，不应该默认全局共享。
3. **质量评估边界**：SearchOS 验证证据质量，不保证上层 LLM 最终答案绝对正确。

**RFC Suite Version:** 1.0 (+ Annexes 23/24, feature-completion pass)
**Status:** Implemented Engineering Baseline
**Next Step:** Keep RFC-004 v1.1 acceptance tests (Part 23) green on every change; revisit deferred distributed/WASM capabilities in v1.5+.
