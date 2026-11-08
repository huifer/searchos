# Extension & Crawler System

> Extension & Crawler System

SearchOS is **extension-first**. All non-generic capabilities — providers, site-specific
crawlers, extractors, embeddings, rerankers, memory, and intelligence — enter through a
unified, fault-isolated Extension System. This keeps the core stable while the number of
supported websites grows.

## Extension Types

| Type | Purpose |
|---|---|
| Provider Extension | external search provider |
| Site Crawler Extension | site-specific crawler |
| Extractor Extension | content extraction |
| Search Extension | recall strategy |
| Embedding Extension | vector generation |
| Reranker Extension | result reranking |
| Memory Extension | memory storage & retrieval |
| Intelligence Extension | AI decision & planning |

## Implementation Levels

- **Level 1 — Declarative (YAML/JSON)**: URL match, page-type detection, selectors, XPath,
  pagination, fields, headers, cleaning. Highest priority to support.
- **Level 2 — Native (Rust Trait)**: complex logic; ships compiled to avoid ABI issues.
- **Level 3 — WASM/WASI**: third-party extensions with explicit network/file/resource
  permissions. No default full-host access.

## Site Crawler Directory

```text
extensions/crawlers/
├── generic/        manifest.yaml · rules.yaml · tests/
├── github.com/     manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
├── docs.rs/        manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
└── custom/
```

Each site crawler carries its **own rules, tests, version, health state, and degradation
policy**. A changed page structure on one site must not break the whole system.

## Crawler Selection

```text
Incoming URL → Normalize → Domain → Registry
  → Exact Match → Alias Match → Generic Crawler → Fallback
```

## Failure Management

Health states: Healthy → Degraded → Unstable → Quarantined → Disabled → Updating. Classified
errors (TIMEOUT, HTTP_4xx/5xx, SELECTOR_NOT_FOUND, CONTENT_EMPTY, RATE_LIMITED…) each get their
own handling. A failed site auto-degrades to the generic crawler and is quarantined until
recovered.

## Lifecycle

Discovered → Validate → Compatibility → Permissions → Dependencies → Init → Health → Registered
→ Active → (Failure → Degraded → Quarantined) → Disabled. Supports rollback and version-snapshot
hot updates.

## Debugging

```bash
searchos crawler inspect github.com
searchos crawler test https://github.com/owner/repo
searchos crawler preview https://example.com
searchos extension validate
searchos extension reload crawler.github
```
