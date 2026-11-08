# RFC-012: Local Runtime & Gateway Runtime

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 12.1 Application Interface

```
pub struct SearchOsApplication {
    search_service: Arc<dyn SearchService>,
    fetch_service: Arc<dyn FetchService>,
    memory_service: Arc<dyn MemoryService>,
    cost_service: Arc<dyn CostService>,
}
```

Application 通过构造函数注入 Adapter。

## 12.2 Local Runtime

```
CLI
 |
 v
Application
 |
 v
Search Core
 |
 +---- SQLite
 +---- Tantivy
 +---- LanceDB
 +---- Filesystem
 +---- External Providers
```

CLI 不通过 HTTP 调用自身。

## 12.3 Gateway Runtime

```
HTTP Request
    |
    v
Axum Handler
    |
    v
Authentication
    |
    v
DTO Validation
    |
    v
Application
    |
    v
Search Core
```

HTTP Handler 只负责：

- Authentication。
- Authorization。
- DTO Validation。
- Application 调用。
- Response Mapping。

## 12.4 API

核心接口：

```
POST /search
POST /extract
POST /documents
GET  /documents/:id
POST /memory/search
GET  /cost/summary
GET  /cost/providers
GET  /stats/search
GET  /decisions/:trace_id
GET  /health
```

## 12.5 Search Request

```
{
  "query": "What is the latest Tokio version?",
  "profile": "latest",
  "mode": "local_first",
  "budget": {
    "max_external_queries": 3,
    "max_total_cost": 0.05
  }
}
```

## 12.6 Search Response

```
{
  "query": "What is the latest Tokio version?",
  "decision": {
    "path": "external",
    "reason": "local_evidence_stale"
  },
  "results": [],
  "quality": {
    "status": "passed",
    "freshness": "verified",
    "coverage": "sufficient"
  },
  "cost": {
    "estimated": 0.0032,
    "actual": null,
    "currency": "USD"
  },
  "provenance": [],
  "trace_id": "trace_xxx"
}
```

这里的 `actual: null` 表示尚未获得真实账单数据，而不是实际成本为零。

## 12.7 Tavily Compatibility

兼容核心 Search / Extract 请求与响应。

内部使用 SearchOS Domain Model。

兼容层负责：

- Request Mapping。
- Response Mapping。
- Error Mapping。
- Parameter Defaults。
- Unsupported Feature Reporting。

不得让 Tavily DTO 渗透到 Core。

## 12.8 MCP

初期使用 MCP stdio。

工具：

- search。
- extract。
- memory_search。
- cost_summary。
- search_trace。

MCP 工具必须暴露明确的参数 Schema。
