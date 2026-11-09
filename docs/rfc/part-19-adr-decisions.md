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
