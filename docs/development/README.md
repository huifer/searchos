# Development Guide

> How to run, test, and extend SearchOS.

## Prerequisites

- Rust 1.94+ (`rust-toolchain.toml` pins `stable`; the lockfile requires >= 1.94)
- `protoc` — required by the LanceDB build script for every from-source build
  (`brew install protobuf` on macOS; `apt-get install protobuf-compiler
  libprotobuf-dev` on Debian/Ubuntu). CI installs it through
  `.github/actions/setup-protoc`; the Docker build installs both packages in
  the `rust-build` stage.

## Build & test

```text
cargo build --workspace
cargo test --workspace          # unit + integration + e2e + perf + compatibility
cargo clippy --workspace --all-targets
```

## Data layout (RFC-006 6.5)

```text
~/.searchos/
├── config/config.yaml     (copy configs/searchos.example.yaml)
├── data/searchos.db       (SQLite metadata SoT; WAL)
├── data/objects/          (content-addressed blobs: sha256/ab/abcdef...)
├── indexes/tantivy/       (rebuildable BM25)
├── indexes/lancedb/       (rebuildable vectors)
├── extensions/            (user extensions; repo checkout also scans ./extensions)
├── models/ cache/ logs/ temp/
└── data/backups/          (searchos backup create)
```

Override with `SEARCHOS_DATA_DIR`.

## Providers

API keys come from env only (`TAVILY_API_KEY`, `BRAVE_API_KEY`) — never from
config files or the database (RFC-014 14.3). Register built-in providers via
`searchos provider add tavily|brave`; custom providers implement
`searchos_provider::traits::SearchProvider` (native extension compiled in)
and ship a manifest under `extensions/providers/<id>/manifest.yaml`.

## Extension contract

- Declarative (YAML rules), native (Rust trait), WASM (later phase).
- Lifecycle: Discovered → Validation → Compatibility → Permissions →
  Init → Health → Registered → Active; failures → Degraded/Quarantined/Disabled.
- Site crawlers live under `extensions/crawlers/<domain>/` with
  `manifest.yaml` + `extractors.yaml` (+ optional `pagination.yaml`).
- Extensions can never modify the SQLite schema or bypass the cost ledger
  (RFC-011 11.5).

## Adding a migration

Migrations are append-only (`crates/searchos-storage-sqlite/src/migrations.rs`,
versioned by `PRAGMA user_version`). Never edit a released migration — add a
new versioned step.

## Evaluation

```text
python3 fixtures/evaluations/generate_dataset.py   # regenerate dataset-v1 (500)
cargo test -p searchos-integration-tests            # offline acceptance
```

Baselines A/B/C and the non-inferiority gate live in `searchos-evaluation`
(RFC-015 15.4/15.6): cost reduction only counts when quality is non-inferior.

## 中文

# 开发指南

> 如何运行、测试和扩展 SearchOS。

## 前置要求

- Rust 1.94+（`rust-toolchain.toml` 锁定 `stable`；锁文件要求 >= 1.94）
- `protoc` —— LanceDB 构建脚本在任何源码构建中都需要（macOS：`brew install protobuf`；
  Debian/Ubuntu：`apt-get install protobuf-compiler libprotobuf-dev`）。
  CI 通过 `.github/actions/setup-protoc` 安装它；Docker 构建在 `rust-build`
  阶段安装这两个包。

## 构建与测试

```text
cargo build --workspace
cargo test --workspace          # 单元 + 集成 + e2e + 性能 + 兼容性
cargo clippy --workspace --all-targets
```

## 数据布局（RFC-006 6.5）

```text
~/.searchos/
├── config/config.yaml     （复制 configs/searchos.example.yaml）
├── data/searchos.db       （SQLite 元数据 SoT；WAL）
├── data/objects/          （内容寻址 blob：sha256/ab/abcdef...）
├── indexes/tantivy/       （可重建 BM25）
├── indexes/lancedb/       （可重建向量）
├── extensions/            （用户扩展；仓库 checkout 也会扫描 ./extensions）
├── models/ cache/ logs/ temp/
└── data/backups/          （searchos backup create）
```

可用 `SEARCHOS_DATA_DIR` 覆盖。

## Provider

API key 只来自环境变量（`TAVILY_API_KEY`、`BRAVE_API_KEY`）—— 绝不来自配置文件或数据库
（RFC-014 14.3）。通过 `searchos provider add tavily|brave` 注册内置 provider；
自定义 provider 实现 `searchos_provider::traits::SearchProvider`（编译进主程序的原生扩展），
并在 `extensions/providers/<id>/manifest.yaml` 下提供 manifest。

## 扩展契约

- 声明式（YAML 规则）、原生（Rust trait）、WASM（后续阶段）。
- 生命周期：Discovered → Validation → Compatibility → Permissions → Init → Health →
  Registered → Active；失败 → Degraded/Quarantined/Disabled。
- 站点爬虫位于 `extensions/crawlers/<domain>/`，包含 `manifest.yaml` +
  `extractors.yaml`（可选 `pagination.yaml`）。
- 扩展永远不能修改 SQLite schema，也不能绕过成本账本（RFC-011 11.5）。

## 新增迁移

迁移仅追加（`crates/searchos-storage-sqlite/src/migrations.rs`，用
`PRAGMA user_version` 版本化）。绝不修改已发布的迁移 —— 请新增一个版本化步骤。

## 评估

```text
python3 fixtures/evaluations/generate_dataset.py   # 重新生成 dataset-v1（500 题）
cargo test -p searchos-integration-tests            # 离线验收
```

基线 A/B/C 与非劣性门禁在 `searchos-evaluation` 中（RFC-015 15.4/15.6）：
只有在质量非劣时，降本才算数。
