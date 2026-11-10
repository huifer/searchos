# Tavily Compatibility Contract (frozen subset)

> Tavily 兼容契约（冻结子集）

## English

### 1. Scope

SearchOS exposes a **frozen subset** of the Tavily surface on the same paths
Tavily clients already use. The contract is executable: `fixtures/tavily/`
holds the request cases and `tests/compatibility/tests/tavily_contract.rs`
asserts the wire shape against a real in-process gateway.

Three promises are locked:

1. **Stable response shape.** A Tavily client always sees the same fields.
2. **Nothing is silently ignored.** Every documented Tavily knob SearchOS does
   not honor — and every unknown field — is reported back in
   `unsupported_features`.
3. **The native contract is unaffected.** A body without Tavily keys keeps the
   SearchOS envelope and never grows Tavily-only fields.

### 2. Detection

A `/search` body is routed to the compatibility layer when it contains any of:
`search_depth`, `topic`, `include_answer`, `include_raw_content`,
`include_domains`, `exclude_domains`, `include_images`,
`include_image_descriptions`, `chunks_per_source`, `time_range`, `country`,
`include_favicon`, `days`.

An `/extract` body is routed there when it contains a `urls` array.

A body with only `query` (and native keys) stays native. The heuristic is
deliberately conservative: a false positive would change the response shape for
a native client.

### 3. `POST /search`

#### Supported knobs

| Knob | Mapping |
|---|---|
| `query` | Search query (required) |
| `topic` | `general` → general profile, `news` → latest profile, `finance` → research profile; any other value falls back to general **and is reported** |
| `max_results` | Shared result cap (clamped by the same limit as CLI/HTTP/MCP) |
| `days` | Numeric freshness window → `MaximumAgeSeconds(days * 86400)` |
| `include_domains` | `required_domains` |
| `exclude_domains` | `excluded_domains` |
| `workspace_id` | Workspace override; defaults to the dedicated `tavily` workspace |

#### Reported as unsupported

`search_depth`, `include_answer`, `include_raw_content`, `include_images`,
`include_image_descriptions`, `chunks_per_source`, `time_range`, `country`,
`include_favicon`, `auto_parameters`, `model`, `include_usage`, plus any field
SearchOS does not know.

Each entry names the knob and the reason, for example:

```json
"unsupported_features": [
  "include_answer (SearchOS does not generate answers)",
  "search_depth=advanced (SearchOS selects a profile, not a depth preset)",
  "some_future_knob (not part of the v1 Tavily subset)"
]
```

#### Response

```json
{
  "query": "latest ai news",
  "results": [
    { "title": "...", "url": "https://...", "content": "...", "score": 0.5 }
  ],
  "unsupported_features": [],
  "answer": null,
  "response_time": 0.123
}
```

- `results[].title` / `url` / `content` / `score` are always present.
- `answer` is **always `null`**: SearchOS returns evidence, never a generated
  answer (RFC-001 1.2).
- `unsupported_features` is **always present**, possibly empty, so a client can
  tell the request was fully honored instead of guessing.
- `raw_content` is `null` when present: SearchOS returns extracted text, not
  raw HTML.

### 4. `POST /extract`

Tavily sends `{ "urls": [...] }`; SearchOS extracts **one URL per call**.

| Knob | Behavior |
|---|---|
| `urls[0]` | Fetched, extracted and persisted |
| `urls[1..]` | Returned in `failed_results` with an explicit reason |
| `extract_depth` | Reported as unsupported (one extractor per content type) |
| `format` | Reported as unsupported (markdown and raw text only) |
| `include_images`, `include_favicon` | Reported as unsupported |
| `workspace_id` | Defaults to `default` |

```json
{
  "results": [{ "url": "https://...", "title": "...", "raw_content": null }],
  "failed_results": [{ "url": "https://...", "error": "not processed in this call: ..." }],
  "unsupported_features": ["extract_depth=advanced (...)", "urls[1..] (1 URL(s) skipped; ...)"],
  "response_time": 0.456
}
```

### 5. Errors, auth, rate limit

The compatibility layer uses the **standard gateway envelope** — there is no
Tavily-specific error shape:

```json
{ "error": { "code": "VALIDATION_FAILED", "message": "query must not be empty" } }
```

| Condition | Status |
|---|---|
| Missing/invalid bearer token | `401` |
| Token bound to another workspace | `403` |
| Inverted time range, unknown mode, empty query | `400` |
| Document too large / bad encoding | `413` / `400` |
| Rate limit exceeded | `429` |

### 6. Out of scope for v1

| Tavily surface | Decision |
|---|---|
| `POST /crawl` | Not part of v1; returns `404` and is not in `docs/openapi.yaml` |
| `POST /map` | Not part of v1; same |
| `usage` / quota reporting | SearchOS has no request quota to report |
| Raw HTML / images / favicons | Not extracted |

These are asserted by `crawl_and_map_are_out_of_scope`, so a later addition
must update the contract deliberately.

### 7. Verification

```bash
cargo test -p searchos-compatibility-tests     # fixture-driven contract
cargo test -p searchos-interface-http tavily   # mapping unit tests
```

---

## 中文

### 1. 范围

SearchOS 在 Tavily 客户端已经在用的路径上暴露一个**冻结的 Tavily 子集**。该契约
是可执行的：`fixtures/tavily/` 存放请求用例，
`tests/compatibility/tests/tavily_contract.rs` 用真实的进程内网关断言线上报文形状。

锁定三条承诺：

1. **响应形状稳定**：Tavily 客户端永远看到同一组字段。
2. **不静默忽略**：SearchOS 不支持的每个 Tavily 已文档化旋钮——以及每个未知字段——
   都会通过 `unsupported_features` 回报。
3. **不影响原生契约**：不含 Tavily 键的请求体保持 SearchOS envelope，绝不会长出
   Tavily 专属字段。

### 2. 判定

`/search` 请求体包含以下任一键时进入兼容层：`search_depth`、`topic`、
`include_answer`、`include_raw_content`、`include_domains`、`exclude_domains`、
`include_images`、`include_image_descriptions`、`chunks_per_source`、
`time_range`、`country`、`include_favicon`、`days`。

`/extract` 请求体包含 `urls` 数组时进入兼容层。

只带 `query`（以及原生键）的请求体保持原生契约。这个启发式刻意保守：误判会改变
原生客户端的响应形状。

### 3. `POST /search`

#### 支持的旋钮

| 旋钮 | 映射 |
|---|---|
| `query` | 搜索词（必填） |
| `topic` | `general` → general profile，`news` → latest profile，`finance` → research profile；其他值回退到 general **并回报** |
| `max_results` | 统一结果上限（与 CLI/HTTP/MCP 同一限制） |
| `days` | 数字新鲜度窗口 → `MaximumAgeSeconds(days * 86400)` |
| `include_domains` | `required_domains` |
| `exclude_domains` | `excluded_domains` |
| `workspace_id` | 工作区覆盖；默认使用专用 `tavily` 工作区 |

#### 回报为不支持

`search_depth`、`include_answer`、`include_raw_content`、`include_images`、
`include_image_descriptions`、`chunks_per_source`、`time_range`、`country`、
`include_favicon`、`auto_parameters`、`model`、`include_usage`，以及 SearchOS
不认识的任何字段。

每条都带旋钮名和原因，例如：

```json
"unsupported_features": [
  "include_answer (SearchOS does not generate answers)",
  "search_depth=advanced (SearchOS selects a profile, not a depth preset)",
  "some_future_knob (not part of the v1 Tavily subset)"
]
```

#### 响应

```json
{
  "query": "latest ai news",
  "results": [
    { "title": "...", "url": "https://...", "content": "...", "score": 0.5 }
  ],
  "unsupported_features": [],
  "answer": null,
  "response_time": 0.123
}
```

- `results[].title` / `url` / `content` / `score` 始终存在。
- `answer` **永远为 `null`**：SearchOS 返回证据，不生成答案（RFC-001 1.2）。
- `unsupported_features` **始终存在**（可能为空），客户端可以确定请求被完整执行，
  而不是靠猜。
- `raw_content` 存在时为 `null`：SearchOS 返回抽取后的文本，不返回原始 HTML。

### 4. `POST /extract`

Tavily 发送 `{ "urls": [...] }`；SearchOS **每次调用抽取一个 URL**。

| 旋钮 | 行为 |
|---|---|
| `urls[0]` | 抓取、抽取并持久化 |
| `urls[1..]` | 以显式原因放入 `failed_results` |
| `extract_depth` | 回报为不支持（每种内容类型一个抽取器） |
| `format` | 回报为不支持（仅 markdown 与原始文本） |
| `include_images`、`include_favicon` | 回报为不支持 |
| `workspace_id` | 默认 `default` |

```json
{
  "results": [{ "url": "https://...", "title": "...", "raw_content": null }],
  "failed_results": [{ "url": "https://...", "error": "not processed in this call: ..." }],
  "unsupported_features": ["extract_depth=advanced (...)", "urls[1..] (1 URL(s) skipped; ...)"],
  "response_time": 0.456
}
```

### 5. 错误、鉴权与限流

兼容层使用**标准网关错误 envelope**，没有 Tavily 专属错误形状：

```json
{ "error": { "code": "VALIDATION_FAILED", "message": "query must not be empty" } }
```

| 情况 | 状态码 |
|---|---|
| 缺少/无效 bearer token | `401` |
| token 绑定到其他工作区 | `403` |
| 时间范围倒置、未知 mode、空 query | `400` |
| 文档过大 / 编码非法 | `413` / `400` |
| 超过限流 | `429` |

### 6. v1 不做的事

| Tavily 能力 | 决定 |
|---|---|
| `POST /crawl` | 不进 v1；返回 `404`，且不在 `docs/openapi.yaml` 中 |
| `POST /map` | 不进 v1；同上 |
| `usage` / 配额上报 | SearchOS 没有可上报的请求配额 |
| 原始 HTML / 图片 / favicon | 不抽取 |

这些由 `crawl_and_map_are_out_of_scope` 断言，后续若要加入必须显式更新契约。

### 7. 验证

```bash
cargo test -p searchos-compatibility-tests     # fixture 驱动的契约测试
cargo test -p searchos-interface-http tavily   # 映射单元测试
```
