# Deployment (Docker)

> 部署（Docker）

## English

### 1. What is in the box

`Dockerfile` is a multi-stage build that produces two images:

| Target | Base | Contents |
|---|---|---|
| `gateway` | `debian:bookworm-slim` | The release `searchos` binary, `tini` as PID 1, `curl` for the healthcheck |
| `console` | `node:22-alpine` | The built Next.js console with production-only `node_modules` |

Both images are built from the same context (`.dockerignore` keeps `target/`,
`node_modules/`, `.git/` and docs out of it), so one `docker compose up --build`
produces the whole local deployment.

**MCP is stdio-only (ADR-031), so there is deliberately no MCP container.**
Remote agents use the gateway REST/Tavily surface; local agent hosts run
`searchos mcp` on the host.

### 2. Quick start

```bash
docker compose up --build          # gateway on 127.0.0.1:8787, console on 127.0.0.1:3100
```

Then:

- Console: <http://127.0.0.1:3100>
- Gateway health: <http://127.0.0.1:8787/health>
- Metrics: <http://127.0.0.1:8787/metrics>

Both ports are published on `127.0.0.1` only. To expose them on all interfaces,
edit the `ports:` entries — do not rely on this file for a public deployment.

### 3. With authentication

```bash
SEARCHOS_AUTH_TOKEN="$(openssl rand -hex 32)" \
  env SEARCHOS_AUTH_TOKEN="$(openssl rand -hex 32)" docker compose up --build -d
```

The gateway requires `Authorization: Bearer …` on every endpoint (including
`/metrics`), and the console reads the same value server-side through its
proxy, so the token never reaches the browser.

### 4. Persistence and backup

All state — SQLite, Tantivy, LanceDB, object store, extension manifests —
lives under `SEARCHOS_DATA_DIR=/data`, which is a named volume
(`searchos_gateway-data`). One volume, one backup unit.

```bash
# Consistent backup (SQLite + config + manifest checksums)
docker compose exec gateway searchos backup create
docker compose cp gateway:/data/backups ./backups       # copy it off the container

# Restore into a fresh volume
docker compose down
docker volume rm searchos_gateway-data
docker compose up -d
docker compose cp ./backups gateway:/data/backups
docker compose exec gateway searchos backup restore /data/backups/<dir>
```

`searchos doctor` inside the container reports schema version, index
consistency and provider state.

### 5. Observability

```bash
curl -s http://127.0.0.1:8787/metrics | head      # Prometheus text 0.0.4
docker compose --profile observability up -d      # also starts Prometheus
open http://127.0.0.1:9090                        # Prometheus UI, target "searchos-gateway"
```

`deploy/prometheus.yml` scrapes `gateway:8787/metrics` every 15 s. When
`SEARCHOS_AUTH_TOKEN` is set, uncomment the `authorization` block and mount the
token file into the Prometheus container.

`/stats/*` stays as the local diagnostic surface; `/metrics` is the scrape
surface. They expose the same in-process counters.

For push-based collection, `docker compose --profile otel up -d` starts an
OpenTelemetry Collector (`deploy/otel-collector.yml`) and
`SEARCHOS_OTLP_ENABLED=true` makes the gateway push traces and the *same*
metric snapshot over OTLP/HTTP. It is off by default and is flushed on
graceful shutdown. See [`docs/observability.md`](observability.md).

### 6. Shutdown semantics

`docker stop` sends `SIGTERM`; the gateway drains in-flight requests,
terminates SSE streams, flushes the outbox once and exits `0` (see
[`docs/api/http.md`](api/http.md#graceful-shutdown)). `stop_grace_period: 30s`
covers the drain; a second signal still force-exits, so a wedged container is
always recoverable. `tini` is PID 1 so signals reach the process and zombies are
reaped.

### 7. Configuration reference

| Variable | Used by | Meaning |
|---|---|---|
| `SEARCHOS_DATA_DIR` | gateway | State root (mounted at `/data`) |
| `SEARCHOS_AUTH_TOKEN` | gateway + console | Bearer token; empty = local dev |
| `SEARCHOS_CONFIG` | gateway | Path to a mounted `config.yaml` (secrets stay in env) |
| `TAVILY_API_KEYS` / `BRAVE_API_KEYS` | gateway | Comma-separated provider keys (multi-key rotation, ADR-029) |
| `RUST_LOG` | gateway | `info` default; `debug`/`trace` for diagnostics |
| `SEARCHOS_OTLP_ENABLED` | gateway | `true` enables OTLP/HTTP export (default off) |
| `SEARCHOS_OTLP_ENDPOINT` | gateway | Collector base, e.g. `http://otel-collector:4318` |
| `SEARCHOS_OTLP_SERVICE_NAME` | gateway | `service.name` resource attribute |
| `SEARCHOS_OTLP_HEADERS` | gateway | `k=v,k2=v2` extra headers; secrets never logged |
| `SEARCHOS_GATEWAY_URL` | console | Server-side proxy target (compose: `http://gateway:8787`) |
| `NEXT_PUBLIC_SITE_URL` | console | Absolute OG-image URLs in metadata |

Config file values never carry secrets: keys come from the environment.

### 8. Health model

| Check | What it proves |
|---|---|
| gateway `/health` | Process is up **and** the index is consistent (Tantivy vs SQLite chunk counts) |
| gateway `/metrics` | Scrape surface; same auth as the rest of the API |
| console `/` | Next.js server is serving |

Compose gates `console` on `gateway` being healthy, so the console never starts
against a gateway that cannot serve its Read Model.

### 9. Building images without compose

```bash
docker build --target gateway -t searchos/gateway:0.1.0 .
docker build --target console -t searchos/console:0.1.0 .
```

The Rust build uses BuildKit cache mounts for the cargo registry and
`target/`, so rebuilds after a source change re-compile only what changed. The
first build compiles the whole dependency graph (~10 min on a laptop).

Two build args are available:

| Arg | Default | Purpose |
|---|---|---|
| `RUST_VERSION` | `1.97` | Must be ≥ 1.94 — `sqlx 0.9`, `rusqlite 0.41`, `uuid 1.29` and `tantivy 0.27` declare newer `rust-version`s. Keep it in step with `rust-toolchain.toml`. |
| `CARGO_REGISTRY` | *(empty = crates.io)* | A full sparse registry URL for regions where the default registry is slow or unreachable, e.g. `--build-arg CARGO_REGISTRY=sparse+https://mirrors.ustc.edu.cn/crates.io-index/`. Empty keeps upstream, so the file stays portable. |

`libprotobuf-dev` is installed explicitly because it ships the well-known
`google/protobuf/*.proto` files that LanceDB's build script imports; it is only
a `Recommends` of `protobuf-compiler`, and `--no-install-recommends` would drop
it.

### 10. Release and rollback

A release is a tag that matches the workspace version. The `release` workflow
runs the CI gates, builds the release binary with SHA-256 checksums, builds
multi-arch images, and publishes a GitHub release with the matching
[`CHANGELOG.md`](../CHANGELOG.md) section.

```bash
grep '^version' Cargo.toml      # the tag must match this
git tag v0.1.0 && git push origin v0.1.0
```

**Rollback** is a two-step operation because the knowledge base is data, not
just an image:

1. Roll the image back: `docker compose down`, set the image tag to the previous
   version in `docker-compose.yml`, `docker compose up -d`.
2. Roll the data back if the new version migrated the schema: restore the backup
   taken before the upgrade (`searchos backup create` → copy the directory off
   the container → `docker compose cp` it back → `searchos backup restore`).
   Migrations are append-only, so an older binary refuses a newer backup via the
   schema check.

Always take a backup **before** upgrading, not after.

---

## 中文

### 1. 内容

`Dockerfile` 是多阶段构建，产出两个镜像：

| 目标 | 基础镜像 | 内容 |
|---|---|---|
| `gateway` | `debian:bookworm-slim` | release 版 `searchos` 二进制、PID 1 为 `tini`、healthcheck 用的 `curl` |
| `console` | `node:22-alpine` | 构建好的 Next.js 控制台（仅生产依赖） |

**MCP 仅 stdio（ADR-031），因此刻意没有 MCP 容器。** 远程 agent 走网关
REST/Tavily 面；本地 agent 宿主在宿主机上运行 `searchos mcp`。

### 2. 快速开始

```bash
docker compose up --build          # 网关 127.0.0.1:8787，控制台 127.0.0.1:3100
```

- 控制台：<http://127.0.0.1:3100>
- 网关健康：<http://127.0.0.1:8787/health>
- 指标：<http://127.0.0.1:8787/metrics>

两个端口默认只发布在 `127.0.0.1`。要对公网暴露请自行修改 `ports:`，不要把这份
文件直接当成公网部署配置。

### 3. 开启鉴权

```bash
export SEARCHOS_AUTH_TOKEN="$(openssl rand -hex 32)"
docker compose up --build -d
```

网关的所有端点（含 `/metrics`）都要求 `Authorization: Bearer …`；控制台在同一
值的服务端代理里读取，token 不会进入浏览器。

### 4. 持久化与备份

所有状态——SQLite、Tantivy、LanceDB、对象存储、扩展清单——都在
`SEARCHOS_DATA_DIR=/data` 下，对应命名卷 `searchos_gateway-data`。一个卷即一个
备份单元。

```bash
docker compose exec gateway searchos backup create
docker compose cp gateway:/data/backups ./backups

docker compose down
docker volume rm searchos_gateway-data
docker compose up -d
docker compose cp ./backups gateway:/data/backups
docker compose exec gateway searchos backup restore /data/backups/<dir>
```

容器内 `searchos doctor` 可查看 schema 版本、索引一致性与 provider 状态。

### 5. 可观测性

```bash
curl -s http://127.0.0.1:8787/metrics | head      # Prometheus 文本 0.0.4
docker compose --profile observability up -d      # 同时启动 Prometheus
open http://127.0.0.1:9090                        # 抓取目标 searchos-gateway
```

`deploy/prometheus.yml` 每 15s 抓取 `gateway:8787/metrics`。设置了
`SEARCHOS_AUTH_TOKEN` 时，请取消注释 `authorization` 块，并把 token 文件挂载进
Prometheus 容器。

`/stats/*` 仍是本地诊断面；`/metrics` 是抓取面。两者是同一套进程内计数器。

如需推送式采集，`docker compose --profile otel up -d` 会启动 OpenTelemetry
Collector（`deploy/otel-collector.yml`），设置 `SEARCHOS_OTLP_ENABLED=true`
后网关会通过 OTLP/HTTP 推送 trace 与**同一套**指标快照。默认关闭，
优雅停机时会冲刷。见 [`docs/observability.md`](observability.md)。

### 6. 停机语义

`docker stop` 发送 `SIGTERM`；网关排空在途请求、终止 SSE 流、做最后一次
outbox 冲刷并以 `0` 退出（见
[`docs/api/http.md`](api/http.md#graceful-shutdown)）。`stop_grace_period: 30s`
覆盖排空时间；第二次信号仍会强制退出，卡死的容器始终可恢复。`tini` 作为 PID 1
负责转发信号并回收僵尸进程。

### 7. 配置参考

| 变量 | 使用方 | 含义 |
|---|---|---|
| `SEARCHOS_DATA_DIR` | 网关 | 状态根目录（挂载在 `/data`） |
| `SEARCHOS_AUTH_TOKEN` | 网关 + 控制台 | Bearer token；为空即本地开发 |
| `SEARCHOS_CONFIG` | 网关 | 挂载进来的 `config.yaml` 路径（密钥仍走环境变量） |
| `TAVILY_API_KEYS` / `BRAVE_API_KEYS` | 网关 | 逗号分隔的 provider 密钥（多密钥轮换，ADR-029） |
| `RUST_LOG` | 网关 | 默认 `info`；诊断用 `debug`/`trace` |
| `SEARCHOS_OTLP_ENABLED` | 网关 | `true` 开启 OTLP/HTTP 导出（默认关闭） |
| `SEARCHOS_OTLP_ENDPOINT` | 网关 | Collector 基地址，例如 `http://otel-collector:4318` |
| `SEARCHOS_OTLP_SERVICE_NAME` | 网关 | `service.name` 资源属性 |
| `SEARCHOS_OTLP_HEADERS` | 网关 | `k=v,k2=v2` 附加 header；密钥不进日志 |
| `SEARCHOS_GATEWAY_URL` | 控制台 | 服务端代理目标（compose 内为 `http://gateway:8787`） |
| `NEXT_PUBLIC_SITE_URL` | 控制台 | 元数据里的 OG 绝对图片地址 |

配置文件不承载密钥：密钥一律来自环境变量。

### 8. 健康模型

| 检查 | 证明 |
|---|---|
| 网关 `/health` | 进程存活 **且** 索引一致（Tantivy 与 SQLite 块数比对） |
| 网关 `/metrics` | 抓取面；与其余 API 同一鉴权 |
| 控制台 `/` | Next.js 服务端在出页面 |

compose 让 `console` 等 `gateway` 健康后才启动，控制台不会对着一个无法提供
Read Model 的网关启动。

### 9. 不用 compose 单独构建镜像

```bash
docker build --target gateway -t searchos/gateway:0.1.0 .
docker build --target console -t searchos/console:0.1.0 .
```

Rust 构建对 cargo registry 与 `target/` 使用 BuildKit cache mount，源码变更后
只重编译改动部分。首次构建需要编译完整依赖图（笔记本上约 10 分钟）。

两个构建参数：

| 参数 | 默认 | 用途 |
|---|---|---|
| `RUST_VERSION` | `1.97` | 必须 ≥ 1.94——`sqlx 0.9`、`rusqlite 0.41`、`uuid 1.29`、`tantivy 0.27` 都声明了更高的 `rust-version`。请与 `rust-toolchain.toml` 保持同步。 |
| `CARGO_REGISTRY` | （空 = crates.io） | 在默认 registry 慢或不可用的地区，传入完整的 sparse registry URL，例如 `--build-arg CARGO_REGISTRY=sparse+https://mirrors.ustc.edu.cn/crates.io-index/`。留空则保持上游，保证文件可移植。 |

`libprotobuf-dev` 是显式安装的：它提供 LanceDB 构建脚本要 import 的
`google/protobuf/*.proto`。它只是 `protobuf-compiler` 的 `Recommends`，
`--no-install-recommends` 会把它丢掉。

### 10. 发版与回滚

一次发版就是一个与 workspace 版本一致的 tag。`release` 工作流会跑 CI 门禁、
构建带 SHA-256 校验和的 release 二进制、构建多架构镜像，并发布携带对应
[`CHANGELOG.md`](../CHANGELOG.md) 小节的 GitHub Release。

```bash
grep '^version' Cargo.toml      # tag 必须与此一致
git tag v0.1.0 && git push origin v0.1.0
```

**回滚**分两步，因为知识库是数据而不只是镜像：

1. 回滚镜像：`docker compose down`，把 `docker-compose.yml` 里的镜像 tag 改成
   上一个版本，`docker compose up -d`。
2. 如果新版本迁移过 schema，还要回滚数据：恢复升级前的备份
   （`searchos backup create` → 把目录拷出容器 → `docker compose cp` 拷回 →
   `searchos backup restore`）。迁移是只增不改的，所以旧版本二进制会通过
   schema 检查拒绝更新版本的备份。

务必在升级**之前**做备份，而不是之后。
