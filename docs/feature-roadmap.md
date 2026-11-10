# SearchOS Feature 补充行程文档（历史规划资料 / Historical planning artifact）

> **STATUS (2026-11): CLOSED.** Every Wave 0–4 item in this document has been
> implemented and verified. It is retained for the rationale, phasing and
> non-goals, **not** as a current status list. The authoritative status is the
> completion audit in `.memory/searchos-project.md`, the CI gates in
> `.github/workflows/ci.yml` / `tests/README.md`, and `docs/deployment.md`.
> Wave 1–2 descriptions below
> are stale (they predate the Part 30 execution-correctness pass).
>
> 目标：把当前 SearchOS 从主链路可运行，推进到执行正确、契约可信、可发布、可持续扩展。
> 评估时间：2026-11-09（Asia/Shanghai）。评估期间同一工作树存在并行修改，以下状态以写入前的最新代码为准。

## 一、当前判断

### 已具备基础

- Rust workspace、Local Mode 与 Gateway Mode 共用核心运行时。
- CLI、HTTP、MCP stdio、Tavily 形状的基础 /search、/extract、本地数据导入主链路。
- SQLite 元数据、Tantivy 全文索引、LanceDB/remote index 接口基础。
- 决策状态机、质量门、成本 ledger、预算 reservation/settlement、trace、500 题评测数据集。
- Dashboard 基础页面：Overview、Search、Traces、Analytics、Cost、Knowledge、SEO。
- CI 已覆盖 fmt、clippy、Rust tests、cargo audit、console typecheck/build。

### 目前不能继续宣称全部实现的能力

- Hybrid retrieval / Vector / RRF 仍需端到端验收，不能只凭 trait 和单测宣称完成。
- Crawler Registry、站点 crawler、robots 与 Crawl-delay 尚未闭合到真实 fetch 路径。
- 幂等、缓存身份、预算早退结算、workspace 隔离、删除 outbox 仍需命名验收测试关门。
- Analytics 的时间范围、interval、维度筛选、workspace scope 仍需验证是否真正下推 SQL。
- Tavily 兼容面目前主要是 /search 与 /extract，完整协议契约仍缺。
- 发布、部署、外部 metrics、SDK/OpenAPI、前端测试尚未形成完整交付链。

## 二、执行原则

1. 正确性优先于新功能：错误答案、重复计费、预算泄漏、越权高于新 provider、新图表。
2. 先接线再宣传：必须有真实调用路径、错误处理、指标、测试和文档，才算 Implemented。
3. 契约先行：先固定 schema、错误码、鉴权、分页、版本策略，再生成 SDK 或扩展。
4. 每个 feature 都要有验收证据；关键能力必须有 integration/e2e test。
5. 不扩大 v1 边界：不重新纳入已明确 deferred 的 WASM、分布式、多租户 SSO、浏览器引擎、PDF/OCR。

## 三、执行路线

## Wave 0：冻结基线与写入门禁（1–2 天）

- 冻结当前分支；为并行任务声明独立 write scope。
- 将 Part 30 的 21 条 invariant 拆成测试清单。
- 统一状态口径：README、gap-analysis、Part 30 和 project memory 不得互相矛盾。
- 将 FULLY IMPLEMENTED 改为 core path implemented; production hardening in progress，直到验收门全部通过。
- 统一 LICENSE、Cargo.toml、README badge 的许可证声明。

完成标准：有唯一状态文档；所有 P0/P1 缺口有 owner、依赖、验收测试和目标版本。

## Wave 1：执行正确性与安全边界（1–2 周，v0.2 hardening）

### 1. 幂等与缓存身份
- 正确消费 IdempotencyRepo.begin 的新建/重放结果。
- 幂等 claim 必须发生在 cache hit 之前。
- cache key 包含 mode、freshness、source policy、workspace、language、profile version、index/model version。
- replay 返回原 task/trace/response，不重复执行、不重复计费。
- 验收：concurrent_same_request_id_executes_once_and_bills_once；local_only 不得命中外部结果缓存。

### 2. 预算 RAII 与所有退出路径结算
- reserve 成功后用 guard 管理 settle/release。
- Reject、ProviderUnavailable、timeout、cancel、partial failure 都必须结算。
- 区分 reserved、spent、released、exceeded。
- 验收：reject_path_settles_reservation；部分外部失败保留已发生 cost_spent。

### 3. Workspace 隔离与网络资源边界
- DocumentRepo、memory、analytics read model 的查询都带 workspace predicate。
- token 绑定 workspace 不能只停留在 handler 层。
- DNS 校验结果固定到连接；response body 采用流式上限。
- timeout、redirect、max response bytes、concurrency 必须从 config 传入执行器。
- 验收：workspace_token_cannot_search_other_workspace_documents、fetch_connects_to_validated_address、oversized_response_rejected_before_full_buffer。

### 4. 删除、迁移、canonicalization
- 删除 outbox 统一按 document_id，或提供 delete_by_chunk_ids。
- migration 使用事务且可重入；失败后旧版本仍可用。
- fetch、merge、find_by_url 共享 canonical URL 实现。
- rebuild 保留 language、workspace、title、fetched_at 等 provenance。
- 验收：deleted_document_is_not_retrieved、failed_migration_leaves_previous_version_usable、canonicalization_is_identical_across_paths。

### 5. Trace 与配置消费
- stage trace 记录真实 started_at/duration。
- coverage_status 使用枚举语义；成功结果不写 rejection_reasons。
- 无消费者配置必须接入、deprecated，或在 validate 中显式告警。
- 验收：stage_trace_records_real_timestamps_and_duration、no_unconsumed_config_fields。

## Wave 2：核心能力真正接线（2–4 周，v0.3/v0.4）

### 6. Hybrid retrieval
- 文档写入生成 embedding，并写入 LanceDB/vector backend。
- SearchDependencies 注入 BM25、Vector、RRF、reranker。
- 统一 workspace、source、freshness、language、domain、coverage filters。
- semantic/canonical/retrieval/embedding/rerank cache 逐层接线并定义失效策略。
- HashingEmbedder 仅作测试 fallback；生产配置必须明确模型与版本。
- 验收：local_only 同时获得 BM25 与 vector candidates；RRF 排序有可重复测试。

### 7. Crawler / robots / refresh
- FetchService 注入 CrawlerRegistry，真实选择 site crawler → generic → fallback。
- robots 在每个 fetch hop 生效；解析 Crawl-delay；执行 per-domain concurrency。
- extension discovery 按 manifest 隔离失败，并记录 quarantine 指标。
- ConditionalHeaders、sitemap、next_refresh_at、refresh outbox 接成 refresh job。
- 验收：fetch_respects_robots_disallow、crawl_delay_is_honoured、bad_manifest_quarantines_only_itself。

### 8. Semantic memory
- 增加明确 remember/write API，CLI、HTTP、MCP 至少选择两种。
- embedding、BM25、RRF、metadata filter、freshness 组成完整 memory search。
- 支持 namespace/workspace 可见性、去重、冲突、retention sweep、re-embedding。

## Wave 3：对外契约与控制台闭环（2–3 周，v0.5）

### 9. Tavily compatibility contract
- 固定 SearchOS 支持子集，并显式返回 unsupported；禁止静默忽略。
- 补齐或删除 search_depth、time_range、include_raw_content、include_images、chunks_per_source 等字段。
- 定义 /search、/extract 的兼容响应、错误、鉴权、usage；明确 /crawl、/map 是否进 v1。
- 增加真实 fixture/client compatibility tests。

### 10. OpenAPI + SDK
- 生成 OpenAPI 3.1，覆盖 /v1、错误 envelope、鉴权、分页、rate limit headers。
- 生成 TypeScript/Python client；Rust SDK 封装 core 或 HTTP client。
- CI 对 OpenAPI 与实际路由做 drift check。

### 11. MCP 口径与长任务 — DONE (ADR-031)
- 裁决结果：**v1 只做 stdio**。`docs/architecture.md` 的 "HTTP API / remote MCP"
  与 README 的 "REST + MCP" 已修正；契约落在 `docs/mcp.md`，理由与代价见 ADR-031。
- 远程入口是 REST/Tavily 面，本地入口是 `searchos mcp`；两者调用同一套应用服务。
- `notifications/cancelled` 不再静默丢弃：计数 `mcp.cancellation_unsupported_total`
  并向 stderr 记录（v1 内联处理请求，无在途工作可取消）。
- 门禁：`tests/compatibility/tests/mcp_transport.rs`（传输声明、MCP crate 无 HTTP
  服务端依赖、网关无 MCP 路由、取消被确认、文档不得宣称远程 MCP、工具目录稳定）。

### 12. Console knowledge/ingest
- Knowledge 页面加入 text/file/dir ingest、job status、失败项、重试。
- 增加文档详情、版本、软删除、reindex、workspace selector。
- 修复 documents/search workspace predicate；让 KPI drilldown 真正透传 filter。
- 验收：浏览器完成导入 → 等待 job → 搜索 → 查看版本 → 删除 → 验证不可检索。

### 13. Analytics read model
- from/to/interval/timezone/workspace/provider/agent/mode/quality 必须下推 SQL。
- 不要每次请求全量 rebuild；改为增量更新或后台刷新并声明 freshness。
- cursor/limit、CSV/JSON export、drilldown 统一契约。
- 每个 KPI 定义 numerator、denominator、time window。

## Wave 4：发布与运维（2–3 周，v1.0 GA）

### 14. 可复现部署
- Dockerfile + docker-compose，包含 gateway、console、持久化 volume、healthcheck。
- graceful shutdown、migration/backup/restore runbook、TLS termination 说明。
- release workflow、版本号、CHANGELOG、artifact checksum、rollback 说明。

### 15. Production observability
- Prometheus /metrics 或 OpenTelemetry exporter。
- request/trace/task/provider/cost 使用同一关联 ID。
- SLO/alert：错误率、P95 latency、provider failure、budget exceeded、dead-letter、auth failure。
- 保留 /stats/* 作为本地诊断，不冒充外部监控。

### 16. 评测门禁
- CI 定期运行 500 题 dataset，固定 baseline、质量阈值、成本阈值和非劣效规则。
- search/retrieval/provider 改动输出 A/B/C diff；失败阻止合入或留下人工 override 审计。

### 17. 备份与恢复
- 备份 SQLite、对象存储、索引元数据、extension manifests、配置引用。
- restore 校验 checksum、PRAGMA integrity_check、对象覆盖和可重建索引一致性。
- 增加定时备份与恢复演练。

### 18. 前端质量与发布
- Vitest/Playwright 覆盖搜索、导入、workspace 隔离、错误/空态。
- 提供 console Docker image 或明确静态/Node 部署方式。
- 增加中文 locale，或明确 v1 只支持英文并同步文档。

## 四、版本与验收门

| 版本 | 目标 | 必须通过 |
|---|---|---|
| v0.2 | 正确性 hardening | 幂等、预算、workspace、SSRF、删除、迁移关键测试 |
| v0.3 | Hybrid + crawler + memory 接线 | vector/RRF、robots、quarantine、semantic memory 集成测试 |
| v0.4 | Tavily/API 契约 | schema、鉴权、错误、兼容 fixture、OpenAPI 初版 |
| v0.5 | Console/analytics 闭环 | ingest/delete/reindex、workspace dashboard、filters、前端 smoke tests |
| v1.0 | 可发布 GA | Docker、release、metrics、eval CI、backup drill、security review |

## 五、任务拆分

### P0-A：正确性
A1 幂等 claim + replay response；A2 budget guard + all exit settlement；A3 workspace predicates + read model scope；A4 delete outbox + migration transaction；A5 SSRF DNS pinning + streaming limits；A6 trace semantics + canonical URL。

### P0-B：主链路
B1 hybrid retrieval runtime wiring；B2 crawler registry / robots / Crawl-delay；B3 extension discovery quarantine；B4 semantic memory write/search。

### P1：契约与体验
C1 Tavily compatibility tests；C2 OpenAPI + SDK；C3 MCP transport decision；C4 console ingest/document lifecycle；C5 analytics query pushdown and incremental refresh。

### P1：GA 运维
D1 Docker/release/TLS/shutdown；D2 Prometheus/OTel/SLO；D3 evaluation CI gate；D4 backup restore drill；D5 frontend tests and deployment artifact。

## 六、不纳入本行程

- WASM/WASI 扩展、多节点 Gateway、PostgreSQL/消息队列、跨地域向量复制。
- Managed multi-tenant、RBAC/SSO、跨组织控制面。
- 自建全球网页索引、自研 LLM、自动答案生成、无限 query expansion。
- 浏览器引擎、无限反爬绕过。
- RFC-029 v1 排除的 PDF/DOCX/PPTX 解析、OCR、文件监听、音视频。
- 更多 provider（Exa/SearXNG 等），除非先有明确需求和兼容契约。

## 七、Definition of Done

1. 代码进入真实调用路径，不是只有 trait、struct 或单测。
2. 有单元测试；跨边界能力有 integration/e2e test。
3. 失败、超时、取消、重试和权限边界有明确行为。
4. 有指标或 trace 说明执行结果、成本和失败原因。
5. CLI/HTTP/MCP（适用时）行为一致。
6. 配置字段要么有消费者，要么显式 deprecated 并在 validate 报告。
7. RFC、API 文档、README 与实现状态一致。
8. 通过 fmt、clippy、cargo test、console typecheck/build/test。
9. 不声称已验证/已生效，除非有对应命令或 CI 证据。

## 八、推荐第一周排期

| 天 | 工作重点 | 交付物 |
|---|---|---|
| Day 1 | 冻结基线、任务拆分、统一文档口径 | branch/owner matrix、P0 test list |
| Day 2 | 幂等与 cache identity | replay tests、cache isolation tests |
| Day 3 | budget guard + workspace predicates | settlement tests、cross-workspace tests |
| Day 4 | delete/rebuild/migration/canonicalization | index deletion + migration tests |
| Day 5 | SSRF pinning + streaming limits | security regression tests |
| Day 6 | review + full Rust gates | fmt/clippy/test、route scan |
| Day 7 | 文档收口与版本决策 | updated RFC status、release decision |

## 结论

**先补可信度底座，再补能力宽度。** 第一阶段不要继续堆新 provider、新图表或新扩展类型；先让同一个 request 在并发、缓存、预算、权限、索引删除和网络安全上有确定行为。随后再把 Vector、Crawler、Memory、Analytics 真实接线，最后补齐 Tavily/OpenAPI/SDK 和 GA 运维交付。
