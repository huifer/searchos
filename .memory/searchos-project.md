---
name: SearchOS project
description: What the searchos repo is and where its canonical design lives, plus implementation status
type: project-fact
---

The `~/git_repo/searchos` repo is the **implementation of SearchOS**, a Rust-native, extensible AI Search Infrastructure (Local Mode + Gateway Mode, Tavily-compatible, search memory, fault-isolated extension system).

**Why:** The project started from a ChatGPT share conversation that designed the system; the user asked to turn that conversation into polished, bilingual (中文/English) open-source-style docs, then to implement the RFC suite item by item.

**How to apply:**
- The canonical design is `docs/rfc/` (RFC-001..016 + parts 17-24 + RFC-026/027/028). The original design source is the ChatGPT share `https://chatgpt.com/share/6af0361a-d4bc-83ec-afb2-9a12023f5602`; the UI design source is `https://chatgpt.com/share/6af054b8-a43c-83ee-9218-d443cbac9f47`. Its 25-RFC suite + RFC-004 v1.1 + Visualization/Analytics RFCs were reconciled in `docs/gap-analysis.md`, annexes 23/24 and RFC-026..028. Treat `docs/rfc/` as source of truth.
- **Implementation status (as of 2026-01): FULLY IMPLEMENTED and tested.** A complete Cargo workspace with all 25 RFC-002 crates under `crates/`, plus `extensions/`, `fixtures/` (incl. the 500-question `dataset-v1.jsonl`), `tests/{integration,e2e,performance,compatibility}`, and `.github/` CI.
- Verification (after the Analytics/UI pass): `cargo test --workspace` → 295 tests pass, 0 failures (57 suites); `cargo clippy --workspace --all-targets` clean; `cargo fmt --all -- --check` clean. Schema is migration v3 (append-only; never edit released migrations). Frontend: `cd web && npm ci && npm run typecheck && npm run build`.
- Live smoke tests verified: `searchos` CLI (init/search/trace/cost/doctor/index/backup/gateway/mcp), HTTP gateway endpoints (`/health`, `/search`, `/stats/search`, `/cost/summary`), MCP stdio (initialize/tools/call).
- Docs are bilingual (English block, then 中文 block) in every file. Keep that pattern for new docs. README stays split: `README.md` (English) + `README.zh.md` (Chinese).
- Key frozen decisions (ADRs 001-026) are in `docs/adr/index.md` and `docs/rfc/part-19-adr-decisions.md`.
- The repo now has real code: `crates/searchos-core` assembles the runtime (SQLite+Tantivy+LanceDB+FS+providers); `crates/searchos-interface-cli` is the `searchos` binary (Local Mode = in-process, never HTTP loopback); `crates/searchos-interface-http` is the Axum gateway; `crates/searchos-interface-mcp` is MCP stdio.

**Related:** [[searchos-verification]] (verification commands), docs/index.md, docs/architecture.md, docs/rfc/README.md
