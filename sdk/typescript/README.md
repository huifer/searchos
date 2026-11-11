# @searchos/typescript-sdk

Typed TypeScript client for the SearchOS Gateway API.

The client is generated from [`docs/openapi.yaml`](../../docs/openapi.yaml).
That document is the frozen external contract and is verified against the real
Axum route table by the Rust test
`openapi_spec_matches_the_route_table`, so a method here cannot drift from the
gateway without CI failing.

## Install

```bash
npm install @searchos/typescript-sdk
```

## Use

```ts
import { SearchOSClient, SearchOSError } from "@searchos/typescript-sdk";

const searchos = new SearchOSClient({
  baseUrl: "http://127.0.0.1:8787",
  basePath: "/v1",            // optional; the same paths are served unversioned
  token: process.env.SEARCHOS_AUTH_TOKEN,
});

const response = await searchos.search({
  query: "hybrid retrieval RRF fusion",
  mode: "local-only",
  max_results: 5,
});

// Status is explicit: partial / insufficient_evidence are never "completed".
console.log(response.status, response.results.length, response.trace_id);

// Import local text into the same knowledge store as URL fetches.
const doc = await searchos.ingestText({
  content: "SearchOS serves evidence, not generated answers.",
  source_key: "notes/searchos",
  title: "SearchOS note",
});
console.log(doc.document_id, doc.changed);

// Inspect, re-index, then delete — the document lifecycle.
const detail = await searchos.getDocument(doc.document_id);
console.log(detail.summary?.status, detail.versions?.length);
await searchos.reindexDocument(doc.document_id);
await searchos.deleteDocument(doc.document_id);

// Analytics Read Model: filters are pushed into SQL, and the response declares
// which filters were applied and which the dataset cannot apply.
const overview = await searchos.dashboardOverview({ interval: "week" });
console.log(overview.meta?.query_mode, overview.meta?.applied_filters, overview.meta?.ignored_filters);

// Errors carry the gateway status and error code.
try {
  await searchos.getDocument("doc_missing");
} catch (error) {
  if (error instanceof SearchOSError) console.log(error.status, error.code);
}
```

## Coverage

Every operation in `docs/openapi.yaml` has a typed method:

| Group | Methods |
|---|---|
| Search | `search` |
| Knowledge | `extract`, `extractTavily`, `ingestText`, `ingestFile`, `ingestBatch`, `getIngestJob`, `listDocuments`, `getDocument`, `getDocumentVersions`, `reindexDocument`, `deleteDocument`, `searchDocuments` |
| Memory | `searchMemory` |
| Cost | `costSummary`, `costProviders`, `costSavings`, `costReservations`, `reconcileCost` |
| Observability | `statsSearch`, `statsSavings`, `statsRetrieval`, `getDecision`, `listTraces`, `systemStatus`, `listProfiles`, `getProfile`, `health`, `metrics` |
| Analytics | `dashboardOverview`, `dashboardDataset`, `dashboardExport`, `dashboardEvents` |

## Notes

- `dashboardEvents` returns a browser `EventSource`. The SSE stream is a
  refetch hint only; the Read Model remains the historical truth.
- `dashboardExport("csv")` returns raw text, `"json"` returns the envelope.
- `ingestFile` accepts a `Blob`; the runtime sets the multipart boundary.

## Build

```bash
npm install
npm run build
```

## 中文

# @searchos/typescript-sdk

SearchOS Gateway API 的类型化 TypeScript 客户端。

客户端由 [`docs/openapi.yaml`](../../docs/openapi.yaml) 生成。该文档是冻结的对外契约，
并由 Rust 测试 `openapi_spec_matches_the_route_table` 与真实 Axum 路由表比对，
所以这里的方法不可能与网关脱节而不被 CI 发现。

## 安装

```bash
npm install @searchos/typescript-sdk
```

## 用法

```ts
import { SearchOSClient, SearchOSError } from "@searchos/typescript-sdk";

const searchos = new SearchOSClient({
  baseUrl: "http://127.0.0.1:8787",
  basePath: "/v1",            // 可选；同一批路径也挂在未加版本前缀的位置
  token: process.env.SEARCHOS_AUTH_TOKEN,
});

const response = await searchos.search({
  query: "hybrid retrieval RRF fusion",
  mode: "local-only",
  max_results: 5,
});

// status 是显式的：partial / insufficient_evidence 绝不会被写成 completed。
console.log(response.status, response.results.length, response.trace_id);

// 把本地文本导入与 URL 抓取完全相同的知识库。
const doc = await searchos.ingestText({
  content: "SearchOS serves evidence, not generated answers.",
  source_key: "notes/searchos",
  title: "SearchOS note",
});
console.log(doc.document_id, doc.changed);

// 查看、重新索引、删除——文档生命周期。
const detail = await searchos.getDocument(doc.document_id);
console.log(detail.summary?.status, detail.versions?.length);
await searchos.reindexDocument(doc.document_id);
await searchos.deleteDocument(doc.document_id);

// Analytics Read Model：过滤条件下推到 SQL，响应会声明哪些过滤被应用、
// 哪些是该数据集无法应用的。
const overview = await searchos.dashboardOverview({ interval: "week" });
console.log(overview.meta?.query_mode, overview.meta?.applied_filters, overview.meta?.ignored_filters);

// 错误对象携带网关状态与错误码。
try {
  await searchos.getDocument("doc_missing");
} catch (error) {
  if (error instanceof SearchOSError) console.log(error.status, error.code);
}
```

## 覆盖范围

`docs/openapi.yaml` 中的每个操作都有对应的类型化方法：

| 分组 | 方法 |
|---|---|
| 搜索 | `search` |
| 知识 | `extract`、`extractTavily`、`ingestText`、`ingestFile`、`ingestBatch`、`getIngestJob`、`listDocuments`、`getDocument`、`getDocumentVersions`、`reindexDocument`、`deleteDocument`、`searchDocuments` |
| 记忆 | `searchMemory` |
| 成本 | `costSummary`、`costProviders`、`costSavings`、`costReservations`、`reconcileCost` |
| 可观测性 | `statsSearch`、`statsSavings`、`statsRetrieval`、`getDecision`、`listTraces`、`systemStatus`、`listProfiles`、`getProfile`、`health`、`metrics` |
| 分析 | `dashboardOverview`、`dashboardDataset`、`dashboardExport`、`dashboardEvents` |

## 说明

- `dashboardEvents` 返回浏览器 `EventSource`。SSE 流只是重取提示；
  Read Model 仍是历史的真实来源。
- `dashboardExport("csv")` 返回原始文本，`"json"` 返回标准 envelope。
- `ingestFile` 接受 `Blob`；运行时会自动设置 multipart boundary。

## 构建

```bash
npm install
npm run build
```
