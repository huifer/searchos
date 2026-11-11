# CLI Reference

> Part 18 command contract. Output: `--format json|human|markdown` (default
> human); exit codes: 0 ok, 2 validation, 3 budget, 4 policy/security,
> 5 provider/retrieval/timeout, 1 internal. Config: file
> (`SEARCHOS_CONFIG` or `~/.searchos/config/config.yaml`) then env overrides
> (`SEARCHOS_DATA_DIR`, `TAVILY_API_KEY`, `BRAVE_API_KEY`,
> `SEARCHOS_AUTH_TOKEN`). Local Mode is in-process — never HTTP loopback.

## Core

```text
searchos init              Initialize data dir + SQLite (migrations run)
searchos doctor            Diagnose config, storage, index consistency
searchos version           Print version
```

## Search / fetch / ingest

```text
searchos search "Rust Tokio" [--profile latest] [--mode local-only]
                     [--workspace default] [--max-external-queries 3]
                     [--max-cost 0.05] [--max-results 5]
                     [--request-id <idempotency-key>]
searchos fetch <url> [--workspace default]
```

## Local ingestion (RFC-029)

```text
searchos ingest --text "..." [--source-key notes/alpha] [--title "..."]
                     [--content-type text/plain] [--workspace default]
searchos ingest --file ./notes.md [--source-key notes/alpha] [--workspace default]
searchos ingest --dir ./docs [--recursive] [--ext md,txt] [--workspace default]
```

Exactly one of `--text`, `--file`, `--dir` is required. `--source-key` is the
stable logical identity; without it the source is content-addressed. `--dir`
skips dotfiles, sorts by relative path, and uses each file's relative path as
its `source_key`. Imported documents are searchable immediately under
`--mode local-only` / `local_first`.

## Documents / memory

```text
searchos document list [--limit 50] [--source-type web|local|local_text|local_file|local_directory]
                             [--workspace <id>]
searchos document inspect <id>|refresh <id>|delete <id>
searchos memory remember --title "..." --content "..." [--workspace default]
                         [--sharing private|workspace_shared|project_shared|global_public]
                         [--tags a,b]
searchos memory search "Rust web framework" [--workspace default] [--top-k 5]
searchos memory stats
```

## Providers / profiles / cost / trace

```text
searchos provider list|check-key <tavily|brave>|health   # add = env key check, never writes config
searchos profile list
searchos profile inspect <id>                 # versioned SearchProfile
searchos cost summary|agents|providers|operations|savings|reservations
searchos cost reconcile --event <cost-event-id> --actual 0.005 [--source provider_billing]
searchos cost export [--out file]
searchos trace show <trace-id>               # includes structured stage trace
```

`cost savings` / `stats savings` expose `SavingsRate`, `CPQPA`, external
avoidance and the four reuse cost views. `cost reconcile` imports real
provider billing without ever rewriting the original estimate.

## Index / stats / gateway / config

```text
searchos index status|stats|rebuild|verify
searchos stats retrieval                    # hit rates, status counts
searchos stats savings
searchos config validate                   # exit 2 on invalid config
searchos gateway start --port 8787 --host 127.0.0.1
searchos gateway status
```

## Crawler / extension / backup

```text
searchos crawler list|inspect <domain>|test <url>|preview <url>|failures|status|validate
searchos extension list [--quarantined]|validate|inspect <id>
searchos backup create|restore <path>
```

## Evaluation / export / backup

```text
searchos eval run [--dataset fixtures/evaluations/dataset-v1.jsonl]
                  [--limit N] [--out report.json]
searchos export [--out file] [--workspace default]   # secrets never exported
searchos backup create                              # directory + SHA-256 manifest
searchos backup restore <backup-dir | legacy .db>   # checksum + integrity check
```

## MCP

```text
searchos mcp    # JSON-RPC 2.0 over stdio: initialize, tools/list, tools/call
```

**Transport: stdio only (ADR-031).** There is no HTTP MCP listener and the
gateway exposes no MCP route; remote agents use the REST/Tavily surface. See
[`docs/mcp.md`](../mcp.md).

Tools: `search`, `fetch`, `extract`, `ingest_text`, `ingest_file`,
`memory_search`, `document_search`, `cost_summary`, `trace_inspect`
(`search_trace` alias) — RFC-021's tools plus the two RFC-029 ingest tools and
one documented compatibility alias. Each has an explicit parameter schema
(RFC-012 12.8, RFC-021 21.5). `notifications/cancelled` is acknowledged
(counted + logged) rather than silently dropped. `search` accepts snake_case or kebab-case
mode names, rejects unknown modes with -32602, supports a `request_id`
idempotency key and a `max_results` output cap, and describes `local_only`
vs external-capable modes.

## Search response status

Every `/search` and `searchos search` response now carries an explicit
`status` (`completed | completed_with_warnings | partial | insufficient_evidence |
budget_exhausted | failed | cancelled`), `warnings[]`, `task_id` and `replayed`.
A partial or insufficient result is never reported as a completed
quality-passing search (RFC-002 2.8). `ExternalOnly` / strict-freshness queries
bypass the exact cache (RFC-003 3.4, RFC-004 v1.1 9.5).

## 中文

# CLI 参考

> Part 18 命令契约。输出：`--format json|human|markdown`（默认 human）；
> 退出码：0 成功，2 校验失败，3 预算，4 策略/安全，5 provider/检索/超时，1 内部错误。
> 配置优先级：配置文件（`SEARCHOS_CONFIG` 或 `~/.searchos/config/config.yaml`），
> 然后是环境变量覆盖（`SEARCHOS_DATA_DIR`、`TAVILY_API_KEY`、`BRAVE_API_KEY`、
> `SEARCHOS_AUTH_TOKEN`）。Local Mode 是进程内的 —— 绝不做 HTTP 回环。

## 核心

```text
searchos init              初始化数据目录与 SQLite（跑迁移）
searchos doctor            诊断配置、存储、索引一致性
searchos version           打印版本
```

## 搜索 / 抓取 / 导入

```text
searchos search "Rust Tokio" [--profile latest] [--mode local-only]
                     [--workspace default] [--max-external-queries 3]
                     [--max-cost 0.05] [--max-results 5]
                     [--request-id <idempotency-key>]
searchos fetch <url> [--workspace default]
```

## 本地导入（RFC-029）

```text
searchos ingest --text "..." [--source-key notes/alpha] [--title "..."]
                     [--content-type text/plain] [--workspace default]
searchos ingest --file ./notes.md [--source-key notes/alpha] [--workspace default]
searchos ingest --dir ./docs [--recursive] [--ext md,txt] [--workspace default]
```

`--text`、`--file`、`--dir` 三者必须且只能指定一个。`--source-key` 是稳定的逻辑身份；
不给则按内容寻址。`--dir` 会跳过点文件、按相对路径排序，并用每个文件的相对路径作为
它的 `source_key`。导入的文档立即可在 `--mode local-only` / `local_first` 下检索到。

## 文档 / 记忆

```text
searchos document list [--limit 50] [--source-type web|local|local_text|local_file|local_directory]
                             [--workspace <id>]
searchos document inspect <id>|refresh <id>|delete <id>
searchos memory remember --title "..." --content "..." [--workspace default]
                         [--sharing private|workspace_shared|project_shared|global_public]
                         [--tags a,b]
searchos memory search "Rust web framework" [--workspace default] [--top-k 5]
searchos memory stats
```

## Provider / Profile / 成本 / Trace

```text
searchos provider list|check-key <tavily|brave>|health   # add = 检查 env key，不写配置
searchos profile list
searchos profile inspect <id>                 # 版本化的 SearchProfile
searchos cost summary|agents|providers|operations|savings|reservations
searchos cost reconcile --event <cost-event-id> --actual 0.005 [--source provider_billing]
searchos cost export [--out file]
searchos trace show <trace-id>               # 含结构化 stage trace
```

`cost savings` / `stats savings` 暴露 `SavingsRate`、`CPQPA`、外部调用规避量和四种
复用成本视图。`cost reconcile` 导入真实 provider 账单，且永不重写原始估算值。

## 索引 / 统计 / 网关 / 配置

```text
searchos index status|stats|rebuild|verify
searchos stats retrieval                    # 命中率、状态计数
searchos stats savings
searchos config validate                   # 配置非法时退出码 2
searchos gateway start --port 8787 --host 127.0.0.1
searchos gateway status
```

## 爬虫 / 扩展 / 备份

```text
searchos crawler list|inspect <domain>|test <url>|preview <url>|failures|status|validate
searchos extension list [--quarantined]|validate|inspect <id>
searchos backup create|restore <path>
```

## 评估 / 导出 / 备份

```text
searchos eval run [--dataset fixtures/evaluations/dataset-v1.jsonl]
                  [--limit N] [--out report.json]
searchos export [--out file] [--workspace default]   # 绝不导出密钥
searchos backup create                              # 目录 + SHA-256 清单
searchos backup restore <backup-dir | legacy .db>   # 校验和 + 完整性检查
```

## MCP

```text
searchos mcp    # stdio 上的 JSON-RPC 2.0：initialize、tools/list、tools/call
```

**传输：仅 stdio（ADR-031）。** 没有 HTTP MCP 监听器，网关也不暴露 MCP 路由；
远程 agent 使用 REST/Tavily 面。见 [`docs/mcp.md`](../mcp.md)。

工具：`search`、`fetch`、`extract`、`ingest_text`、`ingest_file`、
`memory_search`、`document_search`、`cost_summary`、`trace_inspect`
（别名 `search_trace`）—— RFC-021 的工具加上两个 RFC-029 导入工具和一个有文档的
兼容别名。每个工具都有显式参数 schema（RFC-012 12.8、RFC-021 21.5）。
`notifications/cancelled` 会被显式确认（计数 + 记录日志），而不是静默丢弃。
`search` 接受 snake_case 或 kebab-case 的模式名，未知模式返回 -32602，支持
`request_id` 幂等键与 `max_results` 输出上限，并能说明 `local_only` 与可外部调用模式的区别。

## 搜索响应状态

每个 `/search` 与 `searchos search` 响应都带有显式 `status`
（`completed | completed_with_warnings | partial | insufficient_evidence |
budget_exhausted | failed | cancelled`）、`warnings[]`、`task_id` 和 `replayed`。
部分或证据不足的结果绝不会被报告为“已完成且通过质量门”的搜索（RFC-002 2.8）。
`ExternalOnly` / 严格新鲜度的查询会绕过精确缓存（RFC-003 3.4、RFC-004 v1.1 9.5）。
