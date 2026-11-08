---
name: SearchOS verification
description: Exact commands that verify the SearchOS implementation end-to-end
type: project-fact
---

The SearchOS implementation (`~/git_repo/searchos`) is verified by these commands (all green as of the implementation turn):

**Why:** goal-mode completion audits require reproducible, authoritative evidence — not conversation summaries.

**How to apply:**
- Unit + integration + suites: `cargo test --workspace` → 253 passed / 0 failed (57 suites).
- Lint/format gates: `cargo clippy --workspace --all-targets` (0 warnings), `cargo fmt --all -- --check`.
- CLI smoke (real binary): `cargo build --bin searchos`, then with `SEARCHOS_DATA_DIR=$(mktemp -d)`:
  - `searchos init --format json`, `searchos doctor`, `searchos search "q" --mode local-only --format json`, `searchos trace show <trace_id>`, `searchos cost summary`, `searchos index status`, `searchos crawler list`, `searchos backup create`.
- Gateway smoke: `searchos gateway start --port 18787 &` then `curl /health`, `POST /search`, `GET /stats/search`, `GET /cost/summary`.
- MCP smoke: pipe JSON-RPC lines (`initialize`, `tools/list`, `tools/call cost_summary`) into `searchos mcp`.
- Evaluation dataset: `python3 fixtures/evaluations/generate_dataset.py` (regenerates the committed 500-question dataset; category counts must equal RFC-015 15.3: 100/100/75/75/50/50/50).

- Release build: `cargo build --release --bin searchos` (15m09s, 0 errors/warnings, 273M binary); the release binary passes init/search/doctor/gateway smoke tests.
- Full clean rebuild verified: `cargo clean` (removed 59.1GiB) → `cargo build --workspace --all-targets` (575 crates, 5m55s, 0 errors/warnings) → `cargo test --workspace` (253 passed) → `cargo clippy --workspace --all-targets` (0 warnings) → `cargo fmt --all -- --check` clean.

**Known environment notes:**
- LanceDB requires `protoc` at build time (`brew install protobuf`).
- Live network tests (fetch of https://example.com) tolerate sandbox timeouts by design.

**Related:** [[searchos-project]], tests/README.md
