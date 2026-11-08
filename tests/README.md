# SearchOS Test Suites (RFC-002 2.1, RFC-016 Phase 8)

| Suite | Path | Purpose |
|---|---|---|
| Integration | `tests/integration/` | Cross-crate full-pipeline acceptance (offline, deterministic) |
| E2E | `tests/e2e/` | Real `searchos` binary smoke tests (Part-18 contract) |
| Performance | `tests/performance/` | Latency regression guards (Estimated machine-relative figures) |
| Compatibility | `tests/compatibility/` | Schema versioning, migration idempotence, restart preservation |

Each suite is a workspace member crate so `cargo test --workspace` runs all of
them. Unit tests live inside the crates they test.
