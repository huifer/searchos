# Part 19. ADR Decisions

以下架构决策作为第一阶段默认基线。

| ADR     | 决策                                     |
| ------- | -------------------------------------- |
| ADR-001 | Rust Workspace                         |
| ADR-002 | Core / Application / Interface 解耦      |
| ADR-003 | Local + Gateway 双运行模式                  |
| ADR-004 | Local In-Process Execution             |
| ADR-005 | Axum HTTP Gateway                      |
| ADR-006 | Tavily-compatible API                  |
| ADR-007 | SQLite Metadata Store                  |
| ADR-008 | Tantivy Full-Text Index                |
| ADR-009 | LanceDB Vector Index                   |
| ADR-010 | Filesystem Content Store               |
| ADR-011 | Transactional Outbox                   |
| ADR-012 | Cache / Document / Memory 分离           |
| ADR-013 | Search IR                              |
| ADR-014 | Quality-Gated Search Decision          |
| ADR-015 | Cost Ledger Append-Only                |
| ADR-016 | Provider Registry                      |
| ADR-017 | Site-Specific Crawler Directory        |
| ADR-018 | Declarative + Native + WASM Extensions |
| ADR-019 | Crawler Failure Isolation              |
| ADR-020 | Search Profile Versioning              |
| ADR-021 | Evidence Provenance                    |
| ADR-022 | Local Index Rebuildability             |
| ADR-023 | Workspace Isolation                    |
| ADR-024 | Cost-Aware Execution Planner           |
| ADR-025 | Evaluation-Driven Quality              |
| ADR-026 | Open Core Distribution                 |
| ADR-027 | User-Selectable Remote Full-Text       |
| ADR-029 | Multi-Key Rotation for Upstream          |
| ADR-028 | Local Data Ingestion                   |

## 19.1 ADR-026: Open Core

开源核心建议包括：

- Local Runtime。
- Gateway Runtime。
- CLI。
- MCP。
- Core Search。
- Exact Cache。
- BM25。
- Vector Retrieval。
- Search Decision。
- Cost Ledger。
- Provider Interface。
- Generic Crawler。
- Extension Interface。

商业化功能可以包括：

- Hosted Gateway。
- Multi-Tenant。
- SSO。
- RBAC。
- Distributed Search。
- Centralized Cost Governance。
- Managed Crawler。
- Team Analytics。
- SLA。

不要故意削弱本地搜索质量来制造付费门槛。

## 19.2 ADR-027: User-Selectable Remote Full-Text

用户可以通过 `storage.fulltext` 选择检索引擎：`tantivy`（默认，嵌入式）、`elasticsearch`、
`meilisearch`、`postgres`、`mysql`。远程后端通过 `storage.remote.<backend>` 配置连接信息，
密钥从环境变量注入。

- SQLite 始终是元数据真相源；远程索引保持可重建（ADR-022），重建路径与本地一致。
- 所有后端实现同一个 `SearchIndex` 端口，outbox 写入、本地知识端口、rebuild 逻辑无需分支。
- 工作区隔离（`requester OR *`）在引擎查询层执行，无法下推时客户端二次校验。
- 默认路径不变：不配置远程后端时，行为与之前完全一致（零依赖、离线可用）。

## 19.3 ADR-028: Local Data Ingestion

本地内容（纯文本、文件、目录）可以导入知识库，并与 URL 抓取内容走**同一条**
文档生命周期（RFC-006）、同一个事务 outbox、同一个 `SearchIndex` 端口和同一个
`LocalKnowledgePort`。

- 本地来源使用规范化 URI `searchos-local://<workspace>/<source_key>`；
  `source_key` 由调用方指定，未指定时按内容寻址派生。
- `sources.source_type` 不再硬编码为 `web`，新增 `local_text` / `local_file` /
  `local_directory`；`Document` 增加 `source_type` 与 `title`。
- 本地内容属于用户自有知识，`ContentCategory::Local` 默认不因年龄判定 stale。
- 导入安全边界：显式大小上限、内容类型嗅探、不信任 multipart 文件名、
  工作区隔离、导入内容按数据处理（复用 HTML 清洗器）。
- 两种运行模式都可用：CLI `searchos ingest`（Local Mode，进程内）与
  HTTP `POST /ingest*`（Gateway Mode）。
- 规范见 [RFC-029](../rfc/rfc-029-local-data-ingestion.md)。

## 19.4 ADR-029: Multi-Key Rotation for Upstream Providers

上游 Provider（Tavily、Brave 等）支持多 API Key。SearchOS 在密钥池内轮询，单个密钥被限流或
耗尽不会导致整个搜索失败（RFC-009 9.7）。

- 密钥来源：`TAVILY_API_KEYS` / `BRAVE_API_KEYS`（逗号/空格分隔）> 配置 `providers.<id>.api_keys`
  > 单密钥环境变量。密钥永不入日志，状态输出只显示掩码（`tvly-****-abcd`）。
- 轮询策略：`round_robin`（默认）或 `failover`（粘性，失败后才切换）。
- 每密钥独立健康状态：429 → 限流至 Retry-After；401/403/配额 → 封禁（1800s/3600s）；
  超时/网络错误 → 连续失败计数。全部密钥不可用时返回 429 并附带最早恢复时间。
- 路由、熔断器、成本账本不变：一个 Provider ID，内部多密钥；registry 与执行路径共享同一实例。

## 19.5 ADR-031: MCP Transport — stdio-only in v1

**Decision.** SearchOS v1 speaks MCP **only** over stdio. The gateway exposes
**no** MCP route, no streamable HTTP transport and no MCP session state.

**Why.**

- Every agent host that supports remote MCP already speaks HTTP, so it can use
  the REST / Tavily surface today. A second HTTP protocol surface would add
  session, resumability, `Mcp-Session-Id`, per-stream SSE and Origin-validation
  cost without adding capability.
- A separate HTTP MCP listener would duplicate the gateway's auth, workspace
  binding and rate-limit stack, and would have to re-derive the same rules.
- The stdio server is in-process over the same `SearchOsRuntime` as the CLI
  (ADR-004), so Local Mode keeps its "no HTTP loopback" guarantee.

**Consequences.**

- `searchos mcp` is the local agent entry point; the gateway is the remote one.
  Both call the same application services, so behaviour is identical.
- `notifications/cancelled` is acknowledged (counted + logged to stderr) rather
  than silently dropped; the v1 server handles every request inline, so there is
  no in-flight work to cancel. If long-running tools are added later,
  cancellation must become real (progress + cancel).
- The decision is enforced by `tests/compatibility/tests/mcp_transport.rs`:
  declared transport, no HTTP server dependency in the MCP crate, no MCP route
  on the gateway, cancellation counted, and no doc claiming a remote transport.

**规范**：[`docs/mcp.md`](../mcp.md)。

---

## 19.5 ADR-031：MCP 传输——v1 仅 stdio

**决定。** SearchOS v1 的 MCP **只**走 stdio。网关**不**暴露 MCP 路由、不提供
streamable HTTP 传输、不保存 MCP session 状态。

**理由。**

- 支持 remote MCP 的 agent 宿主本身就能说 HTTP，今天即可使用 REST / Tavily 面。
  再跑一套 HTTP 协议面会带来 session、可恢复性、`Mcp-Session-Id`、每流 SSE 与
  Origin 校验的成本，却换不来新能力。
- 独立的 HTTP MCP 监听会与网关的鉴权、工作区绑定、限流栈重复，并需要重新推导
  同样的规则。
- stdio 服务器与 CLI 共用同一个 `SearchOsRuntime`（ADR-004），Local Mode 的
  “无 HTTP 回环”约束得以保持。

**影响。**

- `searchos mcp` 是本地 agent 入口，网关是远程入口；两者调用同一套应用服务，
  行为一致。
- `notifications/cancelled` 会被确认（计数 + stderr 日志）而不是静默丢弃；v1
  服务器内联处理每个请求，没有可取消的在途工作。将来加入长时工具时，取消必须变成
  真正的 progress + cancel。
- 该决定由 `tests/compatibility/tests/mcp_transport.rs` 守住：传输声明、MCP crate
  无 HTTP 服务端依赖、网关无 MCP 路由、取消被计数、文档不得宣称远程传输。

**规范**：[`docs/mcp.md`](../mcp.md)。

---

## 19.6 ADR-032: Quality-Gated Evidence Promotion

**Decision.** External results enter the durable knowledge store **only** after
the task's final quality gate accepts them. Promotion is asynchronous and
outbox-driven: the task records the acquisition, the capture worker performs
the governed fetch and the index write, and the user response never waits for
it. Unverified provider snippets are never written to the index.

**Why.**

- The current inline write at `search_service.rs:1384` happens **before** the
  final gate at `search_service.rs:1489`, so rejected or conflicting evidence
  already sits in the index and is later served as first-party knowledge.
- That write stores only a snippet and no document row, so
  `rebuild_index` (`runtime.rs:645`) erases every purchase: the knowledge base
  cannot compound.
- A purchase that is written but hidden behind a stale retrieval cache
  (the external write path never calls `invalidate_caches`) is paid for and
  then invisible — the compound effect is masked exactly when it should fire.
- Synchronous capture would add fetch latency to every response that bought
  something. The outbox already exists and is drained every 1.5 s.

**Consequences.**

- The external result path changes from "write snippets inline" to "record →
  queue capture → persist version → index → invalidate". The old behaviour is
  removed rather than kept behind a flag, because indexing unverified
  snippets is the defect.
- A set-level `Partial` verdict does not disqualify an individually accepted
  item; an unresolved conflict does.
- Promotion never changes the response status. A promotion failure is
  recorded as `skipped` with a reason and is invisible to the caller.
- Cost: every capture is a `web_fetch` cost event against the same task and
  reservation.
- Enforced by `tests/integration/tests/rfc_031_evidence_promotion.rs`:
  promote → rebuild → reuse, no pollution from rejected evidence, zero
  external calls on the second equivalent query.

**规范**：[`docs/rfc/rfc-031-external-evidence-promotion.md`](rfc-031-external-evidence-promotion.md)。

#### 中文

**决定。** 外部结果**只有**在任务最终质量门接受之后，才允许进入持久知识库。
沉淀是异步且 outbox 驱动的：任务记录本次获取，捕获 worker 执行受控抓取与索引
写入，用户响应不等待。未经验证的 Provider 摘要永不写入索引。

**理由。**

- 当前的内联写入（`search_service.rs:1384`）发生在最终质量门
  （`search_service.rs:1489`）**之前**，因此被拒绝或存在冲突的证据已经躺在索引
  里，日后会被当成第一方知识服务。
- 那次写入只存摘要、不存文档行，于是 `rebuild_index`（`runtime.rs:645`）会把
  买过的一切抹掉——知识库无法产生复利。
- 写入后却被陈旧检索缓存挡住（外部写入路径从不调用 `invalidate_caches`）的
  购买，等于付了钱却看不见——复利效应恰好在最该生效时被遮住。
- 同步捕获会给每一次付费的响应加上抓取延迟。而 outbox 已经存在，每 1.5 秒
  drain 一次。

**影响。**

- 外部结果路径从“内联写入摘要”改为“记录 → 排队捕获 → 写版本 → 写索引 →
  失效缓存”。旧行为被移除而不是藏在开关后面，因为“索引未验证摘要”本身就是
  那个缺陷。
- 集合级的 `Partial` 结论不否定单个被接受的条目；未解决冲突则否定。
- 沉淀永不改变响应状态。沉淀失败记为 `skipped` 并带原因，对调用方不可见。
- 成本：每次捕获都是同一个任务、同一次预留下的一条 `web_fetch` 成本事件。
- 由 `tests/integration/tests/rfc_031_evidence_promotion.rs` 守住：沉淀 → 重建
  → 复用、被拒证据不污染、等价查询第二次零外部调用。

---

## 19.7 ADR-033: Workspace-Scoped Knowledge with an Explicit Binding Model

**Decision.** Promoted knowledge is **workspace-scoped by default**.
`document_bindings` records which workspaces may read a document and with what
visibility (`workspace` or `public`). Cross-workspace public sharing is
opt-in via `knowledge.public_web_knowledge` and is **off in v1**.

**Why.**

- The suite's baseline principle is explicit: *"默认 Workspace 隔离，显式授权后
  才共享，不应该默认全局共享."* A purchase made in workspace A is not
  automatically knowledge for workspace B.
- `DocumentRepo::find_by_url` (`repos.rs:507`) matches on `canonical_url`
  alone, so URL identity is global while ownership is not — a workspace
  boundary cannot be expressed at all until a binding exists.
- The store already has the right retrieval rule (`requester OR '*'`,
  `repos.rs:639` / `697` / `853`, and the same predicate in the PostgreSQL and
  MySQL backends). The binding model reuses it instead of inventing a second
  visibility mechanism.
- Introducing the binding table now means authorised sharing can be enabled
  later **without a schema migration**.

**Consequences.**

- Promotion writes one binding for the acquiring workspace. A document
  promoted in workspace A is invisible in workspace B.
- `documents.visibility` is added with a `'workspace'` default; existing rows
  keep their behaviour.
- The RFC suite's global `*` namespace remains reserved for explicitly shared
  knowledge, not for accidental cross-workspace leakage.

#### 中文

**决定。** 被沉淀的知识**默认按工作区隔离**。`document_bindings` 记录哪些工作
区可以读某个文档，以及以什么可见性读（`workspace` 或 `public`）。跨工作区的
公共共享由 `knowledge.public_web_knowledge` 显式开启，并且 **v1 关闭**。

**理由。**

- RFC 套件的基线原则写得很清楚：**“默认 Workspace 隔离，显式授权后才共享，
  不应该默认全局共享。”** 工作区 A 的一次购买不会自动变成工作区 B 的知识。
- `DocumentRepo::find_by_url`（`repos.rs:507`）只按 `canonical_url` 匹配，于是
  URL 身份是全局的、归属却不是——在存在绑定之前，工作区边界根本无法表达。
- 存储层已经有正确的检索规则（`requester OR '*'`，`repos.rs:639` / `697` /
  `853`，以及 PostgreSQL / MySQL 后端中的同一谓词）。绑定模型复用它，而不是
  再发明一套可见性机制。
- 现在就引入绑定表，意味着日后开启授权共享**不需要 schema 迁移**。

**影响。**

- 沉淀为获取方工作区写一条绑定。在工作区 A 沉淀的文档在工作区 B 不可见。
- `documents.visibility` 新增列，默认 `'workspace'`；既有行行为不变。
- RFC 套件的全局 `*` 命名空间保留给显式共享的知识，而不是意外泄露。

**规范**：[`docs/rfc/rfc-031-external-evidence-promotion.md`](rfc-031-external-evidence-promotion.md)。
