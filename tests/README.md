# SearchOS Test Suites (RFC-002 2.1, RFC-016 Phase 8)

| Suite | Path | Purpose |
|---|---|---|
| Integration | `tests/integration/` | Cross-crate full-pipeline acceptance (offline, deterministic) |
| E2E | `tests/e2e/` | Real `searchos` binary smoke tests (Part-18 contract) |
| Performance | `tests/performance/` | Latency regression guards (Estimated machine-relative figures) |
| Compatibility | `tests/compatibility/` | Schema versioning, migration idempotence, restart preservation |

Each suite is a workspace member crate so `cargo test --workspace` runs all of
them. Unit tests live inside the crates they test.

## 中文

# SearchOS 测试套件（RFC-002 2.1、RFC-016 Phase 8）

| 套件 | 路径 | 用途 |
|---|---|---|
| 集成测试 | `tests/integration/` | 跨 crate 全链路验收（离线、确定性） |
| 端到端 | `tests/e2e/` | 真实 `searchos` 二进制冒烟测试（Part-18 契约） |
| 性能 | `tests/performance/` | 延迟回归护栏（与机器相关的 Estimated 数值） |
| 兼容性 | `tests/compatibility/` | schema 版本、迁移幂等、重启后保留 |

每个套件都是 workspace 成员 crate，因此 `cargo test --workspace` 会运行全部套件。
单元测试放在各自被测的 crate 内。
