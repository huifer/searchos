# Observability

> 可观测性

## English

SearchOS exposes three views of the same process. They are independent
readers of one in-process registry, so they cannot disagree.

| Surface | Format | Purpose |
|---|---|---|
| `GET /stats/search` | JSON | Local diagnostics, the console's `/cost` page |
| `GET /metrics` | Prometheus text 0.0.4 | Pull-based scraping, alerting |
| OTLP/HTTP (opt-in) | Protobuf | Push traces + metrics to an OpenTelemetry collector |

The first two are always on. The third is **off by default**: with no
configuration there is no exporter, no background thread and no network call.

### 1. What OTLP exports

- **Traces**: `tracing` spans bridged through `tracing-opentelemetry` to an
  `SdkTracerProvider` with a batch span processor.
- **Metrics**: the exact `MetricsSnapshot` behind `GET /metrics` (search,
  quality, cost, outbox, latency, Part 30 invariants, status counts) exposed
  as observable gauges. An observable gauge reads the current value at
  collection time, so a counter is never double-counted or reconstructed from
  a rate.

Metric names match the Prometheus series (e.g.
`searchos.search.requests`, `searchos.quality.pass_rate`), which lets a scrape
and an OTLP export be correlated directly. Both signals carry
`service.name = <otlp_service_name>`.

### 2. Enabling it

Environment variables win over the config file — collector tokens must never
be written into YAML:

```bash
export SEARCHOS_OTLP_ENABLED=true
export SEARCHOS_OTLP_ENDPOINT=http://127.0.0.1:4318
export SEARCHOS_OTLP_SERVICE_NAME=searchos-gateway
# Optional: "k=v,k2=v2" extra HTTP headers, e.g. an auth token
export SEARCHOS_OTLP_HEADERS='authorization=Bearer …'

searchos gateway start --host 0.0.0.0
```

`OTEL_EXPORTER_OTLP_ENDPOINT` (the standard OTEL variable) also enables export
when `SEARCHOS_OTLP_ENDPOINT` is unset; an explicit
`SEARCHOS_OTLP_ENABLED=false` always wins.

Config-file equivalents live under `observability:` in
`configs/searchos.example.yaml`:

```yaml
observability:
  otlp_enabled: false
  otlp_endpoint: http://127.0.0.1:4318
  otlp_service_name: searchos
  otlp_metrics_interval_seconds: 15
  otlp_export_traces: true
  otlp_export_metrics: true
```

Signal paths are appended once: the endpoint may be a base
(`http://host:4318` → `/v1/traces`, `/v1/metrics`) or a full signal URL.

`searchos config validate` checks the endpoint scheme, the service name, the
interval and that at least one signal is enabled. Header values never appear
in validation errors, logs, `config.json` backups or the redacted export.

### 3. Local collector

```bash
SEARCHOS_OTLP_ENABLED=true docker compose --profile otel up -d
docker compose logs -f otel-collector
```

`deploy/otel-collector.yml` receives OTLP/HTTP, logs both signals with the
`debug` exporter and exposes the collector's own metrics on port 8889. The
gateway service defaults `SEARCHOS_OTLP_ENDPOINT` to `http://otel-collector:4318`.

### 4. Shutdown and failure behaviour

- The OTLP guard is dropped explicitly **before** `process::exit` (which skips
  destructors), after the gateway's graceful drain. `shutdown()` performs a
  final export of both signals, so the last spans are not lost on `SIGTERM`.
- A collector that is unreachable at startup is reported as a warning and
  export is disabled; it never crashes the process.
- Export failures are logged by the SDK's batch/periodic processors and never
  block a search request.

### 5. Verification gates

| Test | What it proves |
|---|---|
| `cargo test -p searchos-observability` | Config validation, endpoint joining, header parsing (no secret leakage), disabled-by-default, Prometheus renderer |
| `cargo test -p searchos-observability --test otlp_export` | A real local TCP collector receives `POST /v1/traces` and `POST /v1/metrics` with non-empty protobuf payloads |
| `cargo test -p searchos-core --test env_overrides` | Env enables OTLP, applies the bearer token, parses header secrets without logging them, and explicit disable wins |

---

## 中文

SearchOS 对同一套进程内注册表提供三种视图，三者是独立的读取者，不会互相矛盾。

| 面 | 格式 | 用途 |
|---|---|---|
| `GET /stats/search` | JSON | 本地诊断、控制台 `/cost` 页 |
| `GET /metrics` | Prometheus 文本 0.0.4 | 拉取式抓取与告警 |
| OTLP/HTTP（可选） | Protobuf | 向 OpenTelemetry Collector 推送 trace 与指标 |

前两者始终开启；第三者**默认关闭**：未配置时不会创建 exporter、不会启动后台线程、
不会发起任何网络请求。

### 1. OTLP 导出什么

- **Trace**：`tracing` span 经 `tracing-opentelemetry` 桥接到带批处理器的
  `SdkTracerProvider`。
- **指标**：与 `GET /metrics` 完全相同的 `MetricsSnapshot`（搜索、质量、成本、
  outbox、延迟、Part 30 不变量、状态计数），以 observable gauge 导出。
  observable gauge 在采集时读取当前值，因此计数器不会被重复累加，也不需要由
  比率反推。

指标名与 Prometheus series 一致（如 `searchos.search.requests`、
`searchos.quality.pass_rate`），便于把抓取结果与 OTLP 导出直接对照。
两种信号都带 `service.name = <otlp_service_name>`。

### 2. 开启方式

环境变量优先于配置文件——collector token 绝不写进 YAML：

```bash
export SEARCHOS_OTLP_ENABLED=true
export SEARCHOS_OTLP_ENDPOINT=http://127.0.0.1:4318
export SEARCHOS_OTLP_SERVICE_NAME=searchos-gateway
export SEARCHOS_OTLP_HEADERS='authorization=Bearer …'

searchos gateway start --host 0.0.0.0
```

未设置 `SEARCHOS_OTLP_ENDPOINT` 时，标准变量 `OTEL_EXPORTER_OTLP_ENDPOINT`
也会开启导出；显式的 `SEARCHOS_OTLP_ENABLED=false` 始终优先。

配置文件对应 `configs/searchos.example.yaml` 的 `observability:` 段，见上方
英文示例。信号路径只追加一次：endpoint 可以是基地址
（`http://host:4318` → `/v1/traces`、`/v1/metrics`），也可以是完整信号 URL。

`searchos config validate` 会检查 endpoint scheme、service name、采集间隔，
以及至少启用一种信号。Header 值不会出现在校验错误、日志、`config.json`
备份或 redacted 导出里。

### 3. 本地 Collector

```bash
SEARCHOS_OTLP_ENABLED=true docker compose --profile otel up -d
docker compose logs -f otel-collector
```

`deploy/otel-collector.yml` 接收 OTLP/HTTP，用 `debug` exporter 打印两种信号，
并在 8889 端口暴露 collector 自身指标。网关服务的
`SEARCHOS_OTLP_ENDPOINT` 默认指向 `http://otel-collector:4318`。

### 4. 停机与失败行为

- OTLP guard 在 `process::exit`（会跳过析构）之前、网关优雅排空之后被显式
  drop；`shutdown()` 会做最后一次双信号导出，因此 `SIGTERM` 不会丢最后一批 span。
- 启动时 collector 不可达只报告 warning 并关闭导出，不会让进程崩溃。
- 导出失败由 SDK 的批处理器/周期处理器记录日志，绝不阻塞搜索请求。

### 5. 验证门禁

| 测试 | 证明 |
|---|---|
| `cargo test -p searchos-observability` | 配置校验、endpoint 拼接、header 解析（不泄漏密钥）、默认关闭、Prometheus 渲染 |
| `cargo test -p searchos-observability --test otlp_export` | 真实本地 TCP collector 收到带非空 protobuf 载荷的 `POST /v1/traces` 与 `POST /v1/metrics` |
| `cargo test -p searchos-core --test env_overrides` | 环境变量开启 OTLP、应用 bearer token、解析 header 密钥且不记录、显式关闭优先 |
