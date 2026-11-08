# RFC-001: Product Architecture & System Boundaries

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 1.1 产品定位

SearchOS 是一个面向 AI Agent 的搜索执行与成本优化基础设施。

核心能力：

- 本地知识检索。
- 外部 Web Search。
- 网页内容获取。
- 搜索结果和文档持久化。
- 语义级知识复用。
- 搜索质量评估。
- 搜索路径自动决策。
- Provider 和本地计算资源调度。
- 全链路成本核算。
- 多 Agent 共享知识。

产品目标：

在满足指定搜索质量标准的前提下，降低每次有效搜索任务的全链路成本。

## 1.2 产品边界

SearchOS 负责：

- Search Request。
- Query Understanding。
- Local Retrieval。
- External Search。
- Web Fetch。
- Evidence Evaluation。
- Search Decision。
- Cost Accounting。
- Knowledge Persistence。
- Agent Search Interface。

SearchOS 不负责：

- 通用 Agent 执行框架。
- 最终自然语言答案生成。
- 通用 LLM 推理引擎。
- 全球互联网搜索索引建设。
- 自研向量数据库。
- 自研浏览器引擎。
- 通用代码执行环境。

## 1.3 系统架构

```
                    AI Agents
       Codex / Claude Code / Pi / OpenClaw
                       |
        +--------------+--------------+
        |              |              |
       CLI            MCP          HTTP API
        |              |              |
        +--------------+--------------+
                       |
               Application Layer
                       |
        +--------------+--------------+
        |              |              |
    Search Service  Fetch Service  Cost Service
        |              |              |
        +--------------+--------------+
                       |
               Decision Engine
                       |
               Execution Planner
                       |
        +--------------+--------------+
        |                             |
    Local Execution             External Execution
        |                             |
    Cache / BM25                 Search Providers
    Vector Search                Fetch Providers
    Documents                    Crawl Providers
    Memory                       Embedding Providers
        |                             |
        +--------------+--------------+
                       |
                Evidence Store
                       |
                Cost Ledger
                       |
              Evaluation Engine
```

## 1.4 双运行模式

### Local Mode

运行方式：

- Rust CLI。
- Rust Library。
- Rust SDK。
- MCP stdio。

要求：

- 直接调用 Application Service。
- 不启动 HTTP Server。
- 不使用 localhost HTTP。
- 不依赖 Redis。
- 不依赖 PostgreSQL。
- 不依赖 Kafka、NATS。
- 可以完全离线运行本地检索。

### Gateway Mode

运行方式：

- Axum HTTP Server。
- 多 Agent 请求接入。
- HTTP API。
- Tavily-compatible API。
- 后续支持远程 Workspace。

两种模式共享：

- Domain。
- Application。
- Decision Engine。
- Execution Planner。
- Retrieval。
- Cost Accounting。
- Storage Interfaces。

## 1.5 核心质量原则

所有搜索执行路径必须先满足硬性质量条件，再参与成本比较。

```
Candidate Execution Paths
          |
          v
Quality Constraints
          |
          v
Remove Invalid Paths
          |
          v
Estimate Total Cost
          |
          v
Choose Lowest-Cost Valid Path
```

优化目标：

[\
\min C(p)\
]

约束：

[\
Q(p) \geq Q\_{min}\
]

[\
F(p) \geq F\_{min}\
]

[\
Coverage(p) \geq Coverage\_{min}\
]

并满足用户设定的预算、时间和来源要求。
