# RFC-003: Extension System

**Engineering Specification · Version 1.0 · 2026-11-08 · Status: Proposed**

Companion to [RFC-001](rfc-001-searchos.md). This document specifies the extension protocol,
directory layout, registry, lifecycle, permission model, crawler selection & failure
isolation, and the debugging experience.

---

## 1. Extension Types

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

## 2. Manifest Schema

Every extension ships a `manifest.yaml`:

```yaml
id: crawler.github
name: GitHub Crawler
version: 1.0.0
type: site_crawler

match:
  domains:
    - github.com
    - raw.githubusercontent.com

capabilities:
  - page_fetch
  - markdown_extract
  - pagination
  - code_content

runtime:
  type: declarative        # declarative | native | wasm
  timeout_ms: 15000

permissions:
  network:
    allowed_domains:
      - github.com
      - raw.githubusercontent.com

healthcheck:
  enabled: true
```

Required fields: `id`, `name`, `version`, `type`, `match`, `capabilities`, `runtime`,
`permissions`, `healthcheck`, `dependencies`, `compat_version`, `failure_policy`.

A declarative crawler additionally carries `crawler.yaml` (fetch strategy, URL
include/exclude, content selectors, limits) and `extractors.yaml` (per page-type field
extraction):

```yaml
extractors:
  - id: repository
    match:
      path: "/{owner}/{repo}"
    fields:
      title:
        selector: "h1"
      description:
        selector: "[itemprop=about]"
      stars:
        selector: "[aria-label*=stars]"
```

**All rules must be validated against real HTML fixtures** under `fixtures/`; a selector is
never assumed to stay valid.

## 3. Directory Layout

```text
extensions/
└── crawlers/
    ├── generic/          manifest.yaml · rules.yaml · tests/
    ├── github.com/       manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
    ├── docs.rs/          manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
    └── custom/
```

Directory names use a normalized Domain ID; multiple domain aliases may point to one
extension.

## 4. Registry

A single `Extension Registry` owns typed sub-registries (Provider, Crawler, Extractor,
Embedding, Reranker, Memory, Intelligence). On startup it: discovers manifests → validates →
resolves dependencies → initializes → health-checks → registers. Registration is idempotent
per `(type, id, version)`.

## 5. Implementation Levels

- **Level 1 — Declarative (YAML/JSON).** Highest priority. Covers URL matching, page-type
  detection, CSS/XPath selectors, pagination, fields, headers, cleaning.
- **Level 2 — Native (Rust Trait).** For complex logic; official extensions compile with the
  binary to avoid ABI issues.
- **Level 3 — WASM/WASI.** For third-party contributions; explicit network/file/resource
  permissions, never full host access by default.

## 6. Lifecycle

```text
Discovered → Manifest Validation → Compatibility Check → Permission Validation
  → Dependency Resolution → Initialization → Health Check → Registered → Active
  → (Failure → Degraded → Quarantined) → Disabled / Unloaded
```

- Supports versioning, dependency resolution, config validation, permission enforcement,
  isolation, hot-reload (safe types only), rollback, health, logs, metrics.
- **Hot updates use version snapshots**: new tasks bind to the new version; in-flight tasks
  finish or time out on the old one; mid-task rule switching is forbidden.

## 7. Crawler Selection

```text
Incoming URL → Normalize URL → Extract Domain → Crawler Registry
  → Exact Domain Match → Domain Alias Match → Generic Crawler → Fallback
  → Capability Check → Execution → Validation → Content Extraction
```

Priority is strictly ordered: exact extension → alias extension → generic → fallback. Two
extensions must never race on the same URL.

## 8. Failure Management

Each site crawler owns an independent health state machine:

| State | Meaning |
|---|---|
| Healthy | operating normally |
| Degraded | success rate dropping |
| Unstable | intermittent failures |
| Quarantined | temporarily isolated |
| Disabled | manually disabled |
| Updating | rules being updated |

Mechanisms: consecutive-failure threshold, success-rate threshold, circuit-break window,
automatic retry, fallback crawler, generic-crawler degradation, manual recovery, and failure
cause logging.

Failure classification (each with its own handling policy):

```text
NETWORK_ERROR  DNS_ERROR  TIMEOUT  HTTP_403  HTTP_404  HTTP_429  HTTP_5XX
ROBOTS_DISALLOWED  SELECTOR_NOT_FOUND  CONTENT_EMPTY  PARSER_ERROR
BROWSER_RENDER_ERROR  RATE_LIMITED
```

Examples: `HTTP_429` honors `Retry-After` with delayed retry; `HTTP_403` must not retry
infinitely (check access policy); `SELECTOR_NOT_FOUND` triggers rule-anomaly detection;
`CONTENT_EMPTY` must not be written into the normal index.

## 9. Permissions & Isolation

- Per-extension permission set (network domains, filesystem paths, resource budgets).
- Extension failure must never crash Core.
- WASM extensions get explicit resource limits (memory, CPU, wall clock).
- Native extensions are trusted only if locally installed or signature-verified.
- Extensions must not open their own connections to other modules' databases.

## 10. Debugging

```bash
searchos crawler inspect github.com
searchos crawler test https://github.com/owner/repo
searchos crawler status
searchos crawler failures
searchos crawler preview https://example.com
searchos extension validate
searchos extension install ./my-extension
searchos extension reload crawler.github
```

A test run records: raw HTML, final DOM, extracted Markdown, matched rules, failure reason,
latency, and rule version — so site-specific crawlers stay maintainable instead of becoming
an uncontrolled rule pile.
