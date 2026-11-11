# Roadmap & Architecture Decisions

> Roadmap & ADRs

> **STATUS (2026-11): HISTORICAL PLANNING ARTIFACT.** Everything below shipped
> as **v0.1.0**; all Wave 0–4 items are closed and verified in
> [`docs/feature-roadmap.md`](feature-roadmap.md), and the release is recorded in
> [`CHANGELOG.md`](../CHANGELOG.md). These tables are retained for rationale and
> phasing, **not** as a current status list. The authoritative design source
> remains [`docs/rfc/`](rfc/README.md).

## Phased Roadmap (~16 weeks to v1.0)

| Phase | Scope | Acceptance |
|------:|-------|------------|
| P0 | Architecture foundation: workspace, core traits, dual runtime | CLI & Gateway call the same core; no HTTP loopback |
| P1 | Dual runtime + Tavily-compatible gateway | Local Mode HTTP-free; Tavily clients work; shared data model |
| P2 | Document & crawler foundation | Persistent content; per-site extractors; isolated crawler failures |
| P3 | Local search & memory | Offline retrieval; memory survives restart; model-version rebuild |
| P4 | Extension system | Add site crawler without touching core; disable/rollback; quarantine |
| P5 | AI search intelligence | Similar-query reuse; no stale reuse; explainable routing; budget caps |
| P6 | Agent integration | MCP stdio, Rust workspace crates, Codex/Claude Code/Pi integration |
| P7 | Production & release | Security/perf tests, backup/restore, Docker, v1.0 GA |

## Version Strategy

| Version | Capability |
|--------|-----------|
| v0.1 | Rust Core + CLI + Gateway |
| v0.2 | Tavily API + Provider Router |
| v0.3 | Generic Crawler + Site Extensions |
| v0.4 | Tantivy + LanceDB + Memory |
| v0.5 | Semantic Search + Search Planner |
| v0.6 | Extension Lifecycle + MCP |
| v0.8 | Reliability + Observability |
| v1.0 | Dual-mode GA |
| v1.1 | Advanced crawler extensions |
| v1.2 | Multi-process workers |
| v1.5 | Distributed cluster |
| v2.0 | Multi-tenant + hosted search |

## Non-goals (v1.0)

No self-built global web index, LLM, vector DB, or browser engine; no general autonomous-agent
platform; no full distributed database; no geo-strong-consistent vector retrieval; no universal
crawler for all sites; no unlimited anti-bot bypass; no comprehensive multi-language FFI.

## Architecture Decision Records

All ADRs are tracked in [adr/index.md](adr/index.md). Highlights: dual runtime (ADR-003),
in-process Local Mode (ADR-004), Tavily-compatible API (ADR-006), extension-first (ADR-010),
site-specific crawler directory (ADR-011), crawler failure isolation (ADR-013), Memory/Cache
separation (ADR-014), and Open Core (ADR-020).

## Related RFCs

- [RFC-002: Core Crate Specification](rfc/archive/rfc-002-core-crate-specification.md) — workspace, crates, traits.
- [RFC-003: Extension System](rfc/archive/rfc-003-extension-system.md) — extension protocol & crawler isolation.
- [RFC-004: Storage & Memory](rfc/archive/rfc-004-storage-and-memory.md) — schema, indexing, memory lifecycle.
- [RFC-005: Execution Plan](rfc/archive/rfc-005-execution-plan.md) — issues, dependency order, acceptance gates.

## 中文

# 路线图与架构决策

> **状态（2026-11）：历史规划资料。** 以下所有内容已随 **v0.1.0** 发布；Wave 0–4 的全部
> 条目已在 [`docs/feature-roadmap.md`](feature-roadmap.md) 中关闭并验证，发布记录见
> [`CHANGELOG.md`](../CHANGELOG.md)。这些表格仅保留规划依据，**不是**当前状态清单。
> 权威设计源仍是 [`docs/rfc/`](rfc/README.md)。

### 分阶段路线图（约 16 周到 v1.0）

| 阶段 | 范围 | 验收 |
|------:|-------|------------|
| P0 | 架构基础：workspace、核心 Trait、双运行模式 | CLI 与网关调用同一个核心；无 HTTP 回环 |
| P1 | 双运行模式 + Tavily 兼容网关 | Local Mode 不依赖 HTTP；Tavily 客户端可用；共享数据模型 |
| P2 | 文档与爬虫基础 | 内容持久化；按站点抽取器；爬虫故障隔离 |
| P3 | 本地搜索与记忆 | 离线检索；记忆重启后仍在；模型版本化重建 |
| P4 | 扩展系统 | 不改核心即可新增站点爬虫；禁用/回滚；隔离 |
| P5 | AI 搜索智能 | 相似查询复用；不误用过期缓存；可解释路由；预算上限 |
| P6 | Agent 集成 | MCP stdio、Rust workspace crates、Codex/Claude Code/Pi 集成 |
| P7 | 生产发布 | 安全/性能测试、备份/恢复、Docker、v1.0 GA |

### 版本策略

| 版本 | 能力 |
|--------|-----------|
| v0.1 | Rust Core + CLI + Gateway |
| v0.2 | Tavily API + Provider Router |
| v0.3 | 通用爬虫 + 站点扩展 |
| v0.4 | Tantivy + LanceDB + Memory |
| v0.5 | 语义搜索 + Search Planner |
| v0.6 | 扩展生命周期 + MCP |
| v0.8 | 可靠性 + 可观测性 |
| v1.0 | 双模式 GA |
| v1.1 | 高级爬虫扩展 |
| v1.2 | 多进程 worker |
| v1.5 | 分布式集群 |
| v2.0 | 多租户 + 托管搜索 |

### Non-goals（v1.0）

不自建全球网页索引、自研 LLM、向量数据库或浏览器引擎；不做通用自治 agent 平台；
不做完整分布式数据库；不做地理强一致向量检索；不做适配所有站点的通用爬虫；
不做无上限反反爬；不做全语言 FFI。

### 架构决策记录（ADR）

所有 ADR 在 [adr/index.md](adr/index.md) 跟踪。重点：双运行模式（ADR-003）、
进程内 Local Mode（ADR-004）、Tavily 兼容 API（ADR-006）、扩展优先（ADR-010）、
按站点爬虫目录（ADR-011）、爬虫故障隔离（ADR-013）、Memory/Cache 分离（ADR-014）、
Open Core（ADR-020）。

### 相关 RFC

- [RFC-002: Core Crate Specification](rfc/archive/rfc-002-core-crate-specification.md) —— workspace、crates、traits。
- [RFC-003: Extension System](rfc/archive/rfc-003-extension-system.md) —— 扩展协议与爬虫隔离。
- [RFC-004: Storage & Memory](rfc/archive/rfc-004-storage-and-memory.md) —— schema、索引、记忆生命周期。
- [RFC-005: Execution Plan](rfc/archive/rfc-005-execution-plan.md) —— issue、依赖顺序、验收门禁。
