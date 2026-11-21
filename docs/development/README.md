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

## Release checklist

The release workflow (`.github/workflows/release.yml`) fires on `v*` tags and
asserts that the tag matches the workspace `version`. Cutting a release is
therefore a checklist, not a ceremony:

1. **Version** — bump `version` in `Cargo.toml` (it propagates to every crate,
   `/health`, and `searchos_build_info`), and mirror it in
   `docs/openapi.yaml` (`info.version`), `sdk/typescript/package.json` and
   `sdk/python/pyproject.toml`.
2. **CHANGELOG** — move the accumulated `## [Unreleased]` entries under a
   `## [x.y.z] - YYYY-MM-DD` heading. The release body is that section.
3. **SDKs** — the clients mirror `docs/openapi.yaml` by hand. Every new path
   needs a typed method in both (`sdk/typescript/src/client.ts`,
   `sdk/python/searchos/client.py`), otherwise the SDK is lying about the
   contract. Run the automated parity gate before tagging:

   ```text
   python3 scripts/check_sdk_parity.py docs/openapi.yaml \
     sdk/typescript/src/client.ts sdk/python/searchos/client.py
   ```

4. **Gates** — all of these must be green before tagging:

   ```text
   cargo test --workspace --locked
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   SRC_DIR=crates/searchos-interface-http/src \
     bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh
   cd sdk/typescript && npm ci && npm run typecheck && npm run build
   python3 -m compileall -q sdk/python
   python3 scripts/check_sdk_parity.py docs/openapi.yaml \
     sdk/typescript/src/client.ts sdk/python/searchos/client.py
   ```

5. **Tag** — `git tag v<x.y.z>` and push. The workflow builds the binary with
   SHA-256 checksums, the multi-arch gateway/console images (published to
   GHCR when `REGISTRY_TOKEN` is set), and creates the GitHub release.

The rollback path (image tag rollback + backup restore for schema
migrations) is in [`docs/deployment.md`](../deployment.md) §10. Migrations are
append-only, so an older binary refuses a newer backup.

## Working in this repo concurrently

Two agents (or an agent and a human) editing the same checkout WILL collide:
git sees one working tree, and an overwrite is not always attributable. Two
real incidents happened in this repo:

- a foreign process replaced `crates/searchos-federated/src/fusion.rs` with a
  different implementation, breaking the committed `fuse_ranks` callers;
- a foreign process rewrote `crates/searchos-application/src/session_source.rs`
  and inserted a duplicate runtime dependency block.

Both were recovered by diffing against `HEAD` and replaying only the intended
line. Rules that follow from that:

- **Never discard** a working-tree change you did not make — stash it, patch
  it aside, or commit it with a note. `git checkout -- .` destroys someone
  else's work.
- Before a long build, check `ps` for another `cargo`/`rustc` running in this
  repo: two cargo processes fight over the target-dir lock and both stall.
- Keep each stage in its own commit with named gates, so a conflict is a
  merge of two small diffs rather than a rewrite.

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

## 发布清单

发布工作流（`.github/workflows/release.yml`）在 `v*` tag 上触发，并断言 tag 与
workspace 的 `version` 一致。因此发版是一份清单，而不是仪式：

1. **版本** —— 在 `Cargo.toml` 里改 `version`（它会传播到所有 crate、`/health` 与
   `searchos_build_info`），并同步 `docs/openapi.yaml`（`info.version`）、
   `sdk/typescript/package.json`、`sdk/python/pyproject.toml`。
2. **CHANGELOG** —— 把累积的 `## [Unreleased]` 条目移到 `## [x.y.z] - YYYY-MM-DD`
   标题下。发布正文就是该节。
3. **SDK** —— 客户端是手工镜像 `docs/openapi.yaml` 的。新增路径必须在两个客户端里
   各有一个带类型的方法（`sdk/typescript/src/client.ts`、
   `sdk/python/searchos/client.py`），否则 SDK 就在对契约撒谎。打 tag 前运行自动
   契约对齐门禁：

   ```text
   python3 scripts/check_sdk_parity.py docs/openapi.yaml \
     sdk/typescript/src/client.ts sdk/python/searchos/client.py
   ```

4. **门禁** —— 打 tag 前下列全部必须为绿：

   ```text
   cargo test --workspace --locked
   cargo clippy --workspace --all-targets -- -D warnings
   cargo fmt --all -- --check
   SRC_DIR=crates/searchos-interface-http/src \
     bash ~/.agents/skills/axum-route-conflict/scripts/scan-routes.sh
   cd sdk/typescript && npm ci && npm run typecheck && npm run build
   python3 -m compileall -q sdk/python
   python3 scripts/check_sdk_parity.py docs/openapi.yaml \
     sdk/typescript/src/client.ts sdk/python/searchos/client.py
   ```

5. **打 tag** —— `git tag v<x.y.z>` 并推送。工作流会构建带 SHA-256 校验和的二进制、
   多架构 gateway/console 镜像（配置 `REGISTRY_TOKEN` 时推送到 GHCR），并创建
   GitHub Release。

回滚路径（镜像 tag 回滚 + schema 迁移的备份恢复）见
[`docs/deployment.md`](../deployment.md) §10。迁移只追加，因此旧二进制会拒绝新备份。

## 在本仓库并发协作

两个 agent（或一个 agent 与一个人）同时改同一个 checkout 必然冲突：git 只看到一份
工作树，且覆盖往往无法归因。本仓库发生过两次真实事故：

- 外部进程把 `crates/searchos-federated/src/fusion.rs` 换成了另一份实现，
  破坏了已提交的 `fuse_ranks` 调用方；
- 外部进程重写了 `crates/searchos-application/src/session_source.rs`，
  并插入了重复的 runtime 依赖块。

两次都是通过与 `HEAD` 对比、只重放那一行意图改动恢复的。由此得到的规则：

- **绝不丢弃**不是自己改动的工作树变更 —— 存进 stash、另存补丁，或带说明提交。
  `git checkout -- .` 会毁掉别人的工作。
- 长构建之前先看 `ps` 里有没有另一个 `cargo`/`rustc` 在本仓库跑：两个 cargo 进程会
  争 target-dir 锁，双方都会卡住。
- 每个阶段单独提交并带命名门禁，这样冲突是两个小 diff 的合并，而不是重写。
