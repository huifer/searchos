# SearchOS

> 面向 AI Agent 的搜索与记忆基础设施。
> 一个搜索引擎。两种运行模式。共享的智能。

## 为什么要做 SearchOS

AI Agent 工作流高度依赖 Web Search，但今天每个 Agent、每个工具、每个任务往往各用各的搜索服务
（Tavily、Brave、Exa……），由此带来：

- 相同或高度相似的搜索被反复执行。
- 不同搜索服务的额度无法统一管理、共享。
- 已经抓取的网页内容无法充分复用。
- 搜索结果缺乏持久化与知识积累。
- 不同网站的结构差异，通用爬虫无法稳定处理。
- 本地使用与服务器部署需要两套不同的使用方式。

SearchOS 用一套统一的基础设施解决这些问题。

## 核心特性

- **双运行模式（Dual Runtime）**——同一套 Rust Core 既可作为进程内 **Local Mode**（CLI / Rust SDK /
  MCP stdio）运行，也可作为 HTTP **Gateway Mode**（兼容 Tavily 的 REST + MCP）运行。
- **Tavily 兼容 API**——把现有客户端指向 `http://127.0.0.1:8787` 即可，工作流无需改动。
- **本地优先搜索**——优先从本地索引与记忆中返回答案，再决定是否访问外部搜索。
- **搜索记忆（Search Memory）**——搜索历史、文档版本、语义记忆、研究记忆都是一等公民级、可持续积累的资产。
- **混合检索（Hybrid Retrieval）**——BM25 + 向量 + 元数据，经 Reciprocal Rank Fusion（RRF）融合，并支持可插拔重排序。
- **扩展优先（Extension-first）**——Provider、网站专属爬虫、Extractor、Embedding、Reranker 全部通过统一、故障隔离的扩展系统接入。
- **成本感知路由**——综合质量、新鲜度、延迟、成本、可用性进行评分选择；并提供 Singleflight 请求合并。
- **分布式就绪（Distributed-ready）**——从数据模型与接口层面第一天就为多节点设计，但不在 v1 强制引入分布式复杂度。

## 架构总览

```text
┌──────────────────────────────────────────┐
│              接口层 Interface Layer       │
│  CLI   ·   Rust SDK   ·   MCP   ·   HTTP API │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│              应用层 Application Layer      │
│  搜索 · 抓取 · 爬虫 · 记忆 · 研究          │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│              核心运行时 Core Runtime       │
│  查询 · 规划 · 路由 · 缓存 · 检索 ·         │
│  记忆 · 爬虫 · 扩展                       │
└──────────────────┬───────────────────────┘
                   │
┌──────────────────▼───────────────────────┐
│              基础设施 Infrastructure      │
│  SQLite · Tantivy · LanceDB · 文件/对象    │
│  队列 · Provider · Embedding · 浏览器      │
└──────────────────────────────────────────┘
```

详见 [`docs/architecture.md`](docs/architecture.md) 与正式设计文档
[`docs/rfc/rfc-001-searchos.md`](docs/rfc/rfc-001-searchos.md)。

## 开发路线图

以「可交付能力」推进的分阶段计划（约 16 周到达 v1.0）。完整内容见
[`docs/roadmap.md`](docs/roadmap.md)。

| 阶段 | 范围 |
|-----:|------|
| P0 | 架构基础：Workspace、核心 Trait、双运行模式 |
| P1 | 双运行模式 + Tavily 兼容搜索网关 |
| P2 | 文档与爬虫基础 |
| P3 | 本地检索与记忆 |
| P4 | 扩展系统 |
| P5 | AI 搜索智能 |
| P6 | Agent 集成（MCP、SDK） |
| P7 | 生产发布（v1.0） |

## 文档

- [架构说明](docs/architecture.md)
- RFC 系列（英文）：
  - [RFC-001：SearchOS](docs/rfc/rfc-001-searchos.md)
  - [RFC-002：Core Crate Specification](docs/rfc/rfc-002-core-crate-specification.md)
  - [RFC-003：Extension System](docs/rfc/rfc-003-extension-system.md)
  - [RFC-004：Storage & Memory](docs/rfc/rfc-004-storage-and-memory.md)
  - [RFC-005：Execution Plan](docs/rfc/rfc-005-execution-plan.md)
- [扩展与爬虫系统](docs/extensions.md)
- [路线图与 ADR](docs/roadmap.md)
- [文档中心](docs/index.md)
- [English README](README.md)

## 许可证

见 [LICENSE](LICENSE)。（Open Core：社区版开源；托管与企业能力另行提供。）
