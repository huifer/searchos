# MCP Transport Contract

> MCP 传输契约

## English

### 1. Decision (ADR-031)

**SearchOS v1 speaks MCP over stdio only.** There is no HTTP listener, no
streamable-HTTP transport and no MCP route on the gateway. That is a decision,
not an unfinished feature:

| Option | Why it was not taken for v1 |
|---|---|
| Streamable HTTP MCP on the gateway | Every agent host that supports remote MCP already speaks HTTP; they can use the REST/Tavily surface today. Running a second HTTP protocol surface would add session, resumability, `Mcp-Session-Id`, SSE-per-stream and origin-validation surface for no new capability. |
| HTTP MCP as a separate listener | Duplicates the gateway's auth/workspace/rate-limit stack and would have to re-derive the same workspace binding rules. |
| WebSocket MCP | Same duplication; the MCP ecosystem's remote transport is streamable HTTP, not raw WS. |

The gateway is the remote entry point. `searchos mcp` is the local entry point.
Both call the same application services, so behaviour is identical.

### 2. The stdio contract

```bash
searchos mcp          # JSON-RPC 2.0 over stdin/stdout
```

- Framing: one JSON object per line (LSP-style `Content-Length` framing is not
  used).
- Transport identity: `searchos_interface_mcp::TRANSPORT == "stdio"`.
- No HTTP loopback: the server is in-process over the same
  `SearchOsRuntime` the CLI uses (ADR-004).
- stdout carries **only** JSON-RPC messages. Logs go to stderr, so a client can
  parse stdout safely.

#### Methods

| Method | Behaviour |
|---|---|
| `initialize` | Echoes the client's `protocolVersion` (default `2024-11-05`), advertises `{ capabilities: { tools: {} }, serverInfo }` |
| `tools/list` | Full tool catalogue with explicit JSON Schemas |
| `tools/call` | Dispatches one tool; errors use JSON-RPC error codes with a shared uppercase `data.code` (INV-30-19) |
| `ping` | Returns `{}` |
| `notifications/initialized` | Notification: no response |
| `notifications/cancelled` | Notification: no response, **but acknowledged** — see §3 |
| anything else | JSON-RPC `-32601` |

#### Tools

`search`, `fetch`, `extract`, `ingest_text`, `ingest_file`, `memory_search`,
`document_search`, `cost_summary`, `trace_inspect` (`search_trace` is a
documented compatibility alias).

`search` accepts snake_case or kebab-case mode names, rejects unknown modes with
`-32602`, supports a `request_id` idempotency key and a `max_results` cap, and
its description distinguishes `local_only` from external-capable modes.

### 3. Cancellation

The v1 server handles every request **inline**: there is no long-running,
resumable tool call, so there is no in-flight work a cancellation could stop.

A `notifications/cancelled` message therefore:

- produces **no response** (JSON-RPC notifications never get one), and
- increments `mcp.cancellation_unsupported_total` and logs a warning to stderr,

so a client asking for cancellation is told rather than silently ignored. When a
future version adds long-running tools, cancellation must become real
(progress + cancel) instead of this acknowledgement.

### 4. What a remote agent uses instead

| Need | Surface |
|---|---|
| Search / extract / ingest / documents / memory / cost / traces | Gateway REST — see [`docs/openapi.yaml`](openapi.yaml) |
| Tavily client compatibility | `POST /search`, `POST /extract` — see [tavily-compatibility.md](tavily-compatibility.md) |
| Typed clients | [TypeScript](../sdk/typescript/README.md), [Python](../sdk/python/README.md) |
| Local agent hosts (Claude Code, Codex, Pi) | `searchos mcp` over stdio |

### 5. How the decision is enforced

| Gate | Location |
|---|---|
| The declared transport is `stdio` | `transport_is_stdio` in `crates/searchos-interface-mcp` |
| The MCP crate has no HTTP server dependency | `mcp_crate_has_no_http_server_dependency` |
| The gateway exposes no MCP route | `gateway_exposes_no_mcp_route` |
| Cancellation is acknowledged, not dropped | `cancellation_is_acknowledged_not_silently_dropped` |
| The docs do not claim a remote MCP transport | `docs_do_not_claim_a_remote_mcp_transport` |
| All of the above | `tests/compatibility/tests/mcp_transport.rs` |

If remote MCP is added later, these gates fail until the contract is updated
deliberately.

---

## 中文

### 1. 决定（ADR-031）

**SearchOS v1 的 MCP 只走 stdio。** 没有 HTTP 监听、没有 streamable HTTP 传输，
网关也不暴露 MCP 路由。这是决定，不是未完成的功能：

| 备选方案 | 为何 v1 不采用 |
|---|---|
| 网关上的 streamable HTTP MCP | 支持 remote MCP 的 agent 宿主本身就能说 HTTP，今天即可使用 REST/Tavily 面。再跑一套 HTTP 协议面会引入 session、可恢复性、`Mcp-Session-Id`、每流 SSE 与 Origin 校验等成本，却换不来新能力。 |
| 独立 HTTP MCP 监听 | 与网关重复鉴权、工作区绑定、限流整套栈，还要重新推导同样的工作区规则。 |
| WebSocket MCP | 同样是重复；MCP 生态的远程传输是 streamable HTTP 而非裸 WS。 |

网关是远程入口，`searchos mcp` 是本地入口，两者调用同一套应用服务，行为一致。

### 2. stdio 契约

```bash
searchos mcp          # JSON-RPC 2.0 over stdin/stdout
```

- 分帧：每行一个 JSON 对象（不使用 LSP 的 `Content-Length` 分帧）。
- 传输标识：`searchos_interface_mcp::TRANSPORT == "stdio"`。
- 无 HTTP 回环：与 CLI 使用同一个 `SearchOsRuntime`，进程内直连（ADR-004）。
- stdout **只**承载 JSON-RPC 消息，日志走 stderr，客户端可安全解析 stdout。

#### 方法

| 方法 | 行为 |
|---|---|
| `initialize` | 回显客户端 `protocolVersion`（默认 `2024-11-05`），声明 `{ capabilities: { tools: {} }, serverInfo }` |
| `tools/list` | 返回带显式 JSON Schema 的完整工具目录 |
| `tools/call` | 分发单个工具；错误使用 JSON-RPC 错误码与统一大写 `data.code`（INV-30-19） |
| `ping` | 返回 `{}` |
| `notifications/initialized` | 通知：无响应 |
| `notifications/cancelled` | 通知：无响应，**但会确认**——见 §3 |
| 其他 | JSON-RPC `-32601` |

#### 工具

`search`、`fetch`、`extract`、`ingest_text`、`ingest_file`、`memory_search`、
`document_search`、`cost_summary`、`trace_inspect`（`search_trace` 为已文档化的
兼容别名）。

`search` 接受 snake_case 或 kebab-case 模式名，未知模式以 `-32602` 拒绝，支持
`request_id` 幂等键与 `max_results` 上限，描述中明确区分 `local_only` 与可外呼模式。

### 3. 取消

v1 服务器**内联**处理每个请求：没有长时、可恢复的工具调用，也就没有可被取消的
在途工作。

因此 `notifications/cancelled` 会：

- **不产生响应**（JSON-RPC 通知本就不该有响应），并且
- 递增 `mcp.cancellation_unsupported_total` 并向 stderr 输出警告，

让请求取消的客户端被告知，而不是被静默忽略。将来加入长时工具时，取消必须变成真正的
progress + cancel，而不是保留这个确认。

### 4. 远程 agent 应使用的面

| 需求 | 面 |
|---|---|
| 搜索 / 抓取 / 导入 / 文档 / 记忆 / 成本 / trace | 网关 REST——见 [`docs/openapi.yaml`](openapi.yaml) |
| Tavily 客户端兼容 | `POST /search`、`POST /extract`——见 [tavily-compatibility.md](tavily-compatibility.md) |
| 类型化客户端 | [TypeScript](../sdk/typescript/README.md)、[Python](../sdk/python/README.md) |
| 本地 agent 宿主（Claude Code、Codex、Pi） | `searchos mcp`（stdio） |

### 5. 决定如何被门禁守住

| 门禁 | 位置 |
|---|---|
| 声明的传输是 `stdio` | `crates/searchos-interface-mcp` 的 `transport_is_stdio` |
| MCP crate 不含 HTTP 服务端依赖 | `mcp_crate_has_no_http_server_dependency` |
| 网关不暴露任何 MCP 路由 | `gateway_exposes_no_mcp_route` |
| 取消被确认而非丢弃 | `cancellation_is_acknowledged_not_silently_dropped` |
| 文档不宣称存在 remote MCP | `docs_do_not_claim_a_remote_mcp_transport` |
| 以上全部 | `tests/compatibility/tests/mcp_transport.rs` |

若将来加入 remote MCP，这些门禁会失败，直到契约被显式更新。
