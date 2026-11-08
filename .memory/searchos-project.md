---
name: SearchOS project
description: What the searchos repo is and where its canonical design lives, plus implementation status
type: project-fact
---

The `~/git_repo/searchos` repo is the **implementation of SearchOS**, a Rust-native, extensible AI Search Infrastructure (Local Mode + Gateway Mode, Tavily-compatible, search memory, fault-isolated extension system).

**Why:** The project started from a ChatGPT share conversation that designed the system; the user asked to turn that conversation into polished, bilingual (中文/English) open-source-style docs, then to implement the RFC suite item by item.

**How to apply:**
- The canonical design is `docs/rfc/` (RFC-001..016 + parts 17-22). Treat it as source of truth; other docs reference it.
- **Implementation status (as of 2026-01): FULLY IMPLEMENTED and tested.** A complete Cargo workspace with all 25 RFC-002 crates under `crates/`, plus `extensions/`, `fixtures/` (incl. the 500-question `dataset-v1.jsonl`), `tests/{integration,e2e,performance,compatibility}`, and `.github/` CI.
- Verification: `cargo test --workspace` → 253 tests pass, 0 failures; `cargo clippy --workspace --all-targets` clean; `cargo fmt --all -- --check` clean.
- Live smoke tests verified: `searchos` CLI (init/search/trace/cost/doctor/index/backup/gateway/mcp), HTTP gateway endpoints (`/health`, `/search`, `/stats/search`, `/cost/summary`), MCP stdio (initialize/tools/call).
- Docs are bilingual (English block, then 中文 block) in every file. Keep that pattern for new docs. README stays split: `README.md` (English) + `README.zh.md` (Chinese).
- Key frozen decisions (ADRs 001-026) are in `docs/adr/index.md` and `docs/rfc/part-19-adr-decisions.md`.
- The repo now has real code: `crates/searchos-core` assembles the runtime (SQLite+Tantivy+LanceDB+FS+providers); `crates/searchos-interface-cli` is the `searchos` binary (Local Mode = in-process, never HTTP loopback); `crates/searchos-interface-http` is the Axum gateway; `crates/searchos-interface-mcp` is MCP stdio.

**Related:** [[searchos-verification]] (verification commands), docs/index.md, docs/architecture.md, docs/rfc/README.md
