---
name: SearchOS project
description: What the searchos repo is and where its canonical design lives
type: project-fact
---

The `~/git_repo/searchos` repo is the **documentation/home of SearchOS**, a Rust-native, extensible AI Search Infrastructure (Local Mode + Gateway Mode, Tavily-compatible, search memory, fault-isolated extension system).

**Why:** The project started from a ChatGPT share conversation that designed the system; the user asked to turn that conversation into polished, bilingual (中文/English) open-source-style docs.

**How to apply:**
- The canonical design is `docs/rfc/rfc-001-searchos.md` (RFC-001 v2.0). Treat it as source of truth; other docs reference it.
- Docs are bilingual (English block, then 中文 block) in every file. Keep that pattern for new docs.
- There is no implementation yet — the repo is design/docs only (no Cargo/code). README + docs/ + configs/example + LICENSE exist.
- RFC set (English-only): RFC-001 (architecture v2.0), RFC-002 (core crate spec), RFC-003 (extension system), RFC-004 (storage & memory), RFC-005 (execution plan) in `docs/rfc/`.
- README is split: `README.md` (English) + `README.zh.md` (Chinese); no Quick-start section in either.
- docs/* are English-only; only the two READMEs are bilingual.
- Key frozen decisions (ADRs 001–020) are in `docs/adr/index.md`.

**Related:** README.md, docs/index.md, docs/architecture.md, docs/extensions.md, docs/roadmap.md
