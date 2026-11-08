# Part 24. Product API, Security, Reliability & Governance Baseline

> 产品接口、安全、可靠性与治理基线（RFC-017…025 的工程化归并）

## English

The source conversation's complete RFC suite (#82) contains 25 RFCs. This repo's
condensed baseline stopped at RFC-016 + annexes 17–22. This annex restores the
*operational* content of RFC-017…025 as implemented, without duplicating the
existing 16 RFCs.

### 24.1 Security & trust (RFC-017)

- SSRF: loopback / private / link-local / cloud-metadata blocking, DNS validation
  and redirect re-validation (`searchos-fetch::UrlGuard`).
- All fetched web content is **untrusted evidence**, never instructions. It
  cannot change policy, workspace, budget, provider config or extension permissions.
- API keys: env-only (`TAVILY_API_KEY`, `BRAVE_API_KEY`), never in config, DB,
  backup, export or logs. `config.redacted()` is used for backup/export.
- Gateway workspace isolation: `server.workspace_tokens` binds a bearer token
  to exactly one workspace; cross-workspace reads/writes return 403.
- Rate limiting: fixed-window in-process limiter → HTTP 429. Configurable via
  `server.rate_limit_per_minute` (0 disables).
- Resource limits: max response size, redirects, fetch duration, document size,
  chunk/embedding batch, concurrency (`security.*`, `crawler.*`).
- Extension permissions are declared in manifests; WASM is future (ADR-024).

### 24.2 Reliability, recovery & data integrity (RFC-018)

- Bounded retries + exponential backoff, circuit breakers, provider health,
  transactional outbox with dead-lettering, graceful task status.
- On restart: schema check → task recovery → pending outbox → index health →
  rebuild derived indexes (`searchos index rebuild|verify`).
- **Backup**: a consistent directory with `metadata.sqlite`, redacted
  `config.json`, declarative extension manifests and `manifest.json` carrying
  SHA-256 hashes. Derived indexes are rebuildable (ADR-022).
- **Restore** refuses to overwrite on hash mismatch or failed
  `PRAGMA integrity_check`, then instructs `index verify` / `index rebuild`.
- Refresh of stale documents is an outbox event, never a deletion of
  authoritative knowledge.

### 24.3 Configuration & operations (RFC-019)

Priority: CLI overrides → environment → config file → built-in defaults.
Sections: `app, server, storage, search, memory, intelligence, indexes,
policies, budgets, crawler, extensions, security, observability, providers`.
`config_version` is versioned; `validate()` returns actionable errors and
`searchos config validate` fails with exit code 2 on invalid config. Unknown
fields are tolerated; secrets are never part of the file.

Operational commands added: `profile list|inspect`, `stats retrieval|savings`,
`cost savings|reservations|reconcile`, `index stats`, `export`, `eval run`,
`config validate`.

### 24.4 API & agent integration (RFC-021)

| Method | Path | Purpose |
|---|---|---|
| POST | `/search` | quality-gated search (SearchOS DTO or Tavily-shaped) |
| POST | `/extract` | fetch + extract + optional persist |
| POST | `/documents` | persist a document |
| GET | `/documents/{id}` | document + current version |
| POST | `/documents/search` | persisted document lookup |
| POST | `/memory/search` | namespaced memory search |
| GET | `/cost/summary`, `/cost/providers`, `/cost/savings`, `/cost/reservations` | cost views |
| POST | `/cost/reconcile` | provider billing import |
| GET | `/stats/search`, `/stats/retrieval`, `/stats/savings` | metrics |
| GET | `/decisions/{trace_id}`, `/trace/{trace_id}` | explainable decision trace |
| GET | `/profiles`, `/profiles/{id}` | versioned search profiles |
| GET | `/health` | liveness + index consistency |

All routes are also mounted under `/v1` and responses carry
`x-searchos-api-version` (RFC-013 13.7). Tavily compatibility stays a
transport adapter; Tavily DTOs never leak into the domain. `answer` is always
null — SearchOS returns evidence.

MCP tools (RFC-021 21.5): `search`, `fetch`, `extract`, `memory_search`,
`document_search`, `cost_summary`, `trace_inspect` (+ `search_trace` alias).
Tool descriptions explicitly explain `local_only` vs external-capable modes.

### 24.5 Cost views, savings & evaluation (RFC-006 6.9/6.10, RFC-015)

- Four reuse views: original acquisition, reuse execution, allocated
  amortized, marginal cost.
- `SavingsRate = 1 − C_SearchOS / C_Baseline`, with an equivalent-quality
  external-only baseline. `Cost Per Quality-Passing Answer` is the primary
  metric; "external avoidance" alone is never a success metric.
- `searchos eval run` executes the committed 500-question dataset through the
  real search service and compares baselines A (External Only), B (Exact
  Cache+TTL, analytic) and C (SearchOS, live). A cost gain does not compensate
  for a quality non-inferiority failure.

### 24.6 Extension governance & resource management (RFC-022, RFC-023)

Extensions declare id/version/type/compatibility/capabilities/permissions/
dependencies/config schema/healthcheck. Incompatible extensions are not
activated; running tasks keep the version snapshot they started with. Failed
index/refresh work is retried through the outbox and dead-lettered. Background
work (refresh, embedding, rebuild, retention) does not block interactive
search. Admission control honours provider/domain/workspace concurrency caps.

### 24.7 Privacy & retention (RFC-024)

Data classes: public external content, workspace content, private agent
content, credentials, sensitive data. `searchos export` emits documents,
cost summary, redacted config and index state with `secrets: [never exported]`.
Deletion is workspace-aware, auditable, and propagates through derived indexes
and caches (`document delete` + outbox). Robots/terms/copyright constraints are
respected; persistent storage does not grant redistribution rights.

### 24.8 Release acceptance (RFC-025)

A feature is complete only when the implementation, contract doc, tests,
failure modes, metrics and quality behaviour all exist. Definition of Done
remains Part 20; this pass adds the missing *functional* checks: explicit
status, cache-bypass red line, stage trace, idempotent replay, reserve/settle,
reconciliation, savings/CPQPA, workspace auth, rate limit, API version,
consistent backup/restore and a runnable evaluation command.

### 24.9 Deferred (not gaps)

Distributed Gateway (PostgreSQL/broker/object storage/workers, RFC-020),
WASM extension sandbox, multi-tenant RBAC/SSO (v2.0), cross-region vector
replication, and managed provider credentials remain future-stage by design.

---

## 中文

完整 RFC 体系共 25 份；本仓库原先的浓缩基线止于 RFC-016 + 附录 17–22。
本附录把 RFC-017…025 中「可落地的运维内容」补回，不重复已有 16 份 RFC。

### 24.1 安全

SSRF 防护、网页内容一律视为不可信证据、API Key 仅从环境变量读取、
备份/导出/日志均脱敏；网关支持 token→workspace 绑定、限流（429）、
响应大小/重定向/并发上限。扩展权限由 manifest 声明，WASM 留待后续。

### 24.2 可靠性

有界重试、熔断、事务性 Outbox 与死信；重启后校验 schema、恢复任务、
处理 Outbox、检查并重建派生索引。备份是「SQLite + 脱敏配置 + 扩展 manifest
+ SHA-256 清单」的一致性目录；恢复先校验哈希与 `PRAGMA integrity_check`。
过期文档刷新走 Outbox，绝不删除权威知识。

### 24.3 配置与接口

配置优先级 CLI > 环境变量 > 文件 > 默认值，共 14 个 section；
`config_version` 版本化，非法配置由 `searchos config validate` 明确报错。
HTTP 全部端点同时挂载 `/v1`，返回 `x-searchos-api-version`；
MCP 工具补齐为 7 个，`search` 描述明确区分 `local_only` 与外部可用模式。

### 24.4 成本与评估

四种复用成本视图、等效质量基线下的节省率、CPQPA 为一等指标；
`searchos eval run` 用真实搜索服务跑 500 题数据集并对照 A/B/C 基线。
成本下降不能抵消质量非劣效失败。

### 24.5 治理与发布

扩展版本快照、刷新/嵌入/重建等后台工作不阻塞交互搜索；
`searchos export` 支持数据导出且永不导出密钥；删除可审计并传播到派生
索引与缓存。功能完成的定义仍是 Part 20，本次补齐的是其中的功能红线与
可验证项。

### 24.6 有意延后

分布式 Gateway、WASM 沙箱、多租户 RBAC/SSO、跨地域向量复制与托管密钥，
在源对话里就是后续阶段，不属于本次遗漏。
