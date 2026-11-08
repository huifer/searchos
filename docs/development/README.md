# Development Guide

> How to run, test, and extend SearchOS.

## Prerequisites

- Rust stable (see `rust-toolchain.toml`)
- `protoc` (only needed when rebuilding LanceDB from source: `brew install protobuf`)

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
