# Architecture Decision Records (ADR)

> Architecture Decision Records

These decisions are the first-stage default baseline defined by the SearchOS RFC Suite v1.0
(see [docs/rfc/part-19-adr-decisions.md](../rfc/part-19-adr-decisions.md)). Each ADR is a
single, reversible-with-effort commitment.

| ADR | Decision |
|----:|----------|
| ADR-001 | Rust Workspace |
| ADR-002 | Decouple Core / Application / Interface |
| ADR-003 | Dual Runtime: Local Mode + Gateway Mode |
| ADR-004 | Local In-Process Execution (no HTTP loopback) |
| ADR-005 | Axum HTTP Gateway |
| ADR-006 | Tavily-compatible API |
| ADR-007 | SQLite Metadata Store |
| ADR-008 | Tantivy Full-Text Index |
| ADR-009 | LanceDB Vector Index |
| ADR-010 | Filesystem Content Store |
| ADR-011 | Transactional Outbox |
| ADR-012 | Cache / Document / Memory Separation |
| ADR-013 | Search IR (internal unified protocol) |
| ADR-014 | Quality-Gated Search Decision |
| ADR-015 | Cost Ledger Append-Only |
| ADR-016 | Provider Registry |
| ADR-017 | Site-Specific Crawler Directory |
| ADR-018 | Declarative + Native + WASM Extensions |
| ADR-019 | Crawler Failure Isolation |
| ADR-020 | Search Profile Versioning |
| ADR-021 | Evidence Provenance |
| ADR-022 | Local Index Rebuildability |
| ADR-023 | Workspace Isolation |
| ADR-024 | Cost-Aware Execution Planner |
| ADR-025 | Evaluation-Driven Quality |
| ADR-026 | Open Core Distribution |
| ADR-027 | User-Selectable Remote Full-Text Backends |
| ADR-029 | Multi-Key Rotation for Upstream Providers |
| ADR-028 | Local Data Ingestion |
| ADR-030 | Execution Correctness Baseline (Part 30) |
| ADR-031 | MCP Transport: stdio-only in v1 |

## 中文

# 架构决策记录（ADR）

这些决策是 SearchOS RFC 套件 v1.0 定义的第一阶段默认基线
（见 [docs/rfc/part-19-adr-decisions.md](../rfc/part-19-adr-decisions.md)）。
每条 ADR 都是一个需要付出成本才能回退的单点承诺。

| ADR | 决策 |
|----:|----------|
| ADR-001 | Rust Workspace |
| ADR-002 | 解耦 Core / Application / Interface |
| ADR-003 | 双运行模式：Local Mode + Gateway Mode |
| ADR-004 | 本地进程内执行（无 HTTP 回环） |
| ADR-005 | Axum HTTP 网关 |
| ADR-006 | Tavily 兼容 API |
| ADR-007 | SQLite 元数据存储 |
| ADR-008 | Tantivy 全文索引 |
| ADR-009 | LanceDB 向量索引 |
| ADR-010 | 文件系统内容存储 |
| ADR-011 | 事务性 Outbox |
| ADR-012 | Cache / Document / Memory 分离 |
| ADR-013 | Search IR（内部统一协议） |
| ADR-014 | 质量门控的搜索决策 |
| ADR-015 | 成本账本仅追加 |
| ADR-016 | Provider 注册表 |
| ADR-017 | 按站点爬虫目录 |
| ADR-018 | 声明式 + 原生 + WASM 扩展 |
| ADR-019 | 爬虫故障隔离 |
| ADR-020 | Search Profile 版本化 |
| ADR-021 | 证据溯源 |
| ADR-022 | 本地索引可重建 |
| ADR-023 | Workspace 隔离 |
| ADR-024 | 成本感知执行规划器 |
| ADR-025 | 评估驱动的质量 |
| ADR-026 | Open Core 分发 |
| ADR-027 | 用户可选的远程全文后端 |
| ADR-029 | 上游 Provider 多 key 轮转 |
| ADR-028 | 本地数据导入 |
| ADR-030 | 执行正确性基线（Part 30） |
| ADR-031 | MCP 传输：v1 仅 stdio |
