# searchos-sdk (Python)

Typed Python client for the SearchOS Gateway API.

Generated from [`docs/openapi.yaml`](../../docs/openapi.yaml). That document is
the frozen external contract and is verified against the real Axum route table
by the Rust test `openapi_spec_matches_the_route_table`, so a method here
cannot drift from the gateway without CI failing.

## Install

```bash
pip install searchos-sdk
```

## Use

```python
from searchos import SearchOSClient, SearchOSError

searchos = SearchOSClient(
    base_url="http://127.0.0.1:8787",
    base_path="/v1",                      # optional; the same paths are served unversioned
    token=None,                           # set when server.auth_token is configured
)

response = searchos.search("hybrid retrieval RRF fusion", mode="local-only", max_results=5)
# Status is explicit: partial / insufficient_evidence are never "completed".
print(response["status"], len(response["results"]), response.get("trace_id"))

# Import local text into the same knowledge store as URL fetches.
doc = searchos.ingest_text(
    "SearchOS serves evidence, not generated answers.",
    source_key="notes/searchos",
    title="SearchOS note",
)
print(doc["document_id"], doc["changed"])

# Inspect, re-index, then delete — the document lifecycle.
detail = searchos.get_document(doc["document_id"])
print(detail["summary"]["status"], len(detail["versions"]))
searchos.reindex_document(doc["document_id"])
searchos.delete_document(doc["document_id"])

# Analytics Read Model: filters are pushed into SQL and the meta declares
# which were applied and which the dataset cannot apply.
overview = searchos.dashboard_overview(interval="week")
print(overview["meta"]["query_mode"], overview["meta"]["applied_filters"])

# Errors carry the gateway status and error code.
try:
    searchos.get_document("doc_missing")
except SearchOSError as error:
    print(error.status, error.code)
```

## Coverage

Every operation in `docs/openapi.yaml` has a typed method:

| Group | Methods |
|---|---|
| Search | `search` |
| Knowledge | `extract`, `ingest_text`, `ingest_file`, `ingest_batch`, `get_ingest_job`, `list_documents`, `get_document`, `get_document_versions`, `reindex_document`, `delete_document`, `search_documents` |
| Memory | `search_memory` |
| Cost | `cost_summary`, `cost_providers`, `cost_savings`, `cost_reservations`, `reconcile_cost` |
| Observability | `stats_search`, `stats_savings`, `stats_retrieval`, `get_decision`, `list_traces`, `system_status`, `list_profiles`, `get_profile`, `health`, `metrics` |
| Analytics | `dashboard_overview`, `dashboard_dataset`, `dashboard_export` |

## Notes

- The client uses only the standard library (`urllib.request`); there are no
  runtime dependencies.
- `ingest_file` streams the file as multipart/form-data with the filename as a
  label only — the server never writes to its own filesystem.
- SSE (`/api/v1/dashboard/events`) is intentionally not wrapped: it is a
  refetch hint, and the Read Model remains the historical truth.

## 中文

# searchos-sdk（Python）

SearchOS Gateway API 的类型化 Python 客户端。

由 [`docs/openapi.yaml`](../../docs/openapi.yaml) 生成。该文档是冻结的对外契约，
并由 Rust 测试 `openapi_spec_matches_the_route_table` 与真实 Axum 路由表比对，
所以这里的方法不可能与网关脱节而不被 CI 发现。

## 安装

```bash
pip install searchos-sdk
```

## 用法

```python
from searchos import SearchOSClient, SearchOSError

searchos = SearchOSClient(
    base_url="http://127.0.0.1:8787",
    base_path="/v1",                      # 可选；同一批路径也挂在未加版本前缀的位置
    token=None,                           # 配置了 server.auth_token 时传入
)

response = searchos.search("hybrid retrieval RRF fusion", mode="local-only", max_results=5)
# status 是显式的：partial / insufficient_evidence 绝不会被写成 completed。
print(response["status"], len(response["results"]), response.get("trace_id"))

# 把本地文本导入与 URL 抓取完全相同的知识库。
doc = searchos.ingest_text(
    "SearchOS serves evidence, not generated answers.",
    source_key="notes/searchos",
    title="SearchOS note",
)
print(doc["document_id"], doc["changed"])

# 查看、重新索引、删除——文档生命周期。
detail = searchos.get_document(doc["document_id"])
print(detail["summary"]["status"], len(detail["versions"]))
searchos.reindex_document(doc["document_id"])
searchos.delete_document(doc["document_id"])

# Analytics Read Model：过滤条件下推到 SQL，meta 会声明哪些被应用、
# 哪些是该数据集无法应用的。
overview = searchos.dashboard_overview(interval="week")
print(overview["meta"]["query_mode"], overview["meta"]["applied_filters"])

# 错误对象携带网关状态与错误码。
try:
    searchos.get_document("doc_missing")
except SearchOSError as error:
    print(error.status, error.code)
```

## 覆盖范围

`docs/openapi.yaml` 中的每个操作都有对应的类型化方法：

| 分组 | 方法 |
|---|---|
| 搜索 | `search` |
| 知识 | `extract`、`ingest_text`、`ingest_file`、`ingest_batch`、`get_ingest_job`、`list_documents`、`get_document`、`get_document_versions`、`reindex_document`、`delete_document`、`search_documents` |
| 记忆 | `search_memory` |
| 成本 | `cost_summary`、`cost_providers`、`cost_savings`、`cost_reservations`、`reconcile_cost` |
| 可观测性 | `stats_search`、`stats_savings`、`stats_retrieval`、`get_decision`、`list_traces`、`system_status`、`list_profiles`、`get_profile`、`health`、`metrics` |
| 分析 | `dashboard_overview`、`dashboard_dataset`、`dashboard_export` |

## 说明

- 客户端只使用标准库（`urllib.request`），没有运行时依赖。
- `ingest_file` 以 multipart/form-data 流式上传，文件名仅作为标签——
  服务端永远不会写入自己的文件系统。
- SSE（`/api/v1/dashboard/events`）有意不做封装：它只是重取提示，
  Read Model 仍是历史的真实来源。
