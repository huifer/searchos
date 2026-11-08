# RFC-016: Deployment, Release & Engineering Roadmap

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 16.1 MVP 范围

第一版不做完整分布式架构。

必须实现：

- Rust Core。
- Local Runtime。
- Gateway Runtime。
- SQLite。
- Tantivy。
- Document Store。
- Exact Cache。
- Search Decision。
- Quality Gate。
- Tavily Provider。
- Brave Provider。
- Cost Ledger。
- Singleflight。
- Freshness Policy。
- CLI。
- MCP。
- Evaluation。

Vector Search、Semantic Reuse 和 Crawler Extension 在 MVP 后半段加入。

## 16.2 Roadmap

### Phase 0 — Architecture Foundation

目标：建立稳定的工程边界。

- Workspace。
- Domain。
- Application。
- Storage Traits。
- Local Runtime。
- Gateway Runtime。
- Search IR。
- ADR。

验收：

- 两种模式共用同一 Use Case。
- Local Mode 不依赖 HTTP。
- 核心测试通过。

### Phase 1 — External Search & Cost

目标：完成外部搜索调用和成本记录。

- Tavily。
- Brave。
- Provider Registry。
- Cost Model。
- Cost Event。
- Budget。
- Trace。

验收：

每次外部搜索都能追踪成本、Provider、Agent 和任务。

### Phase 2 — Local Knowledge

目标：实现文档和本地搜索。

- SQLite。
- Filesystem。
- Document Versioning。
- Content Hash。
- Tantivy。
- Exact Cache。
- Freshness。

验收：

重复搜索可以本地解决，过期信息不会被错误标记为当前事实。

### Phase 3 — Decision Engine

目标：实现质量优先决策。

- Intent。
- Profile。
- Local Retrieval。
- Quality Gate。
- Coverage Gap。
- External Fallback。
- Decision Trace。

验收：

每个搜索任务都能解释决策原因。

### Phase 4 — Cost Optimizer

目标：完成全链路成本优化。

- Execution Planner。
- Provider Router。
- Cost Estimator。
- Singleflight。
- Budget Enforcement。
- Cost Attribution。

验收：

能够在满足质量要求的路径中选择成本最低的执行路径。

### Phase 5 — Semantic Reuse

目标：降低相似查询重复搜索成本。

- LanceDB。
- Embedding。
- Hybrid Retrieval。
- Semantic Cache。
- RRF。
- Optional Reranker。

验收：

语义复用不绕过质量门控。

### Phase 6 — Agent Integration

目标：多 Agent 使用。

- MCP stdio。
- Tavily Compatibility。
- Rust SDK。
- Codex。
- Claude Code。
- Pi。
- OpenClaw。

验收：

多个 Agent 可以共享一个 Gateway 的 Workspace Knowledge。

### Phase 7 — Crawler Extensions

目标：持续获取高价值知识。

- Generic Crawler。
- GitHub Crawler。
- Docs Crawler。
- Site-Specific Extension。
- Failure Isolation。
- Crawler Debug CLI。

验收：

单站点爬虫失败不会影响其他站点。

### Phase 8 — Production

目标：正式发布。

- Security Audit。
- Performance。
- Backup & Restore。
- Documentation。
- Installer。
- Release Pipeline。
- Benchmark。
- Cost Report。

验收：

能够稳定运行并证明成本优化有效。

## 16.3 版本规划

| 版本   | 主要能力                           |
| ---- | ------------------------------ |
| v0.1 | Core + Local CLI               |
| v0.2 | Gateway + Tavily + Cost Ledger |
| v0.3 | Document Store + BM25          |
| v0.4 | Decision Engine + Quality Gate |
| v0.5 | Cost Optimizer + Budget        |
| v0.6 | Vector + Semantic Reuse        |
| v0.7 | MCP + Agent Sharing            |
| v0.8 | Crawler Extensions             |
| v1.0 | Production Release             |
| v1.5 | Distributed Gateway            |
| v2.0 | Hosted Multi-Tenant            |
