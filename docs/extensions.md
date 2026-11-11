# Extension & Crawler System

> Extension & Crawler System

SearchOS has a versioned, fault-isolated **extension registry**. Native providers and
site crawlers ship compiled into the binary today; manifest discovery, lifecycle,
permissions, and quarantine are implemented, and declarative/WASM runtimes are
architecture targets for later phases. This keeps the core stable while the number of
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

## 中文

# 扩展与爬虫系统

SearchOS 拥有带版本、故障隔离的**扩展注册表**。原生 provider 和站点爬虫目前随主程序编译；
manifest 发现、生命周期、权限与隔离已实现，声明式/WASM 运行时是后续阶段的架构目标。
这样在支持的站点数量增长的同时，核心保持稳定。

### 扩展类型

| 类型 | 用途 |
|---|---|
| Provider 扩展 | 外部搜索 provider |
| 站点爬虫扩展 | 特定站点的爬虫 |
| Extractor 扩展 | 内容抽取 |
| Search 扩展 | 召回策略 |
| Embedding 扩展 | 向量生成 |
| Reranker 扩展 | 结果重排 |
| Memory 扩展 | 记忆存取与检索 |
| Intelligence 扩展 | AI 决策与规划 |

### 实现层级

- **Level 1 —— 声明式（YAML/JSON）**：URL 匹配、页面类型识别、选择器、XPath、
  分页、字段、header、清洗。优先级最高的支持目标。
- **Level 2 —— 原生（Rust Trait）**：复杂逻辑；随主程序编译以规避 ABI 问题。
- **Level 3 —— WASM/WASI**：第三方扩展，需显式声明网络/文件/资源权限，
  默认不给全主机访问。

### 站点爬虫目录

```text
extensions/crawlers/
├── generic/        manifest.yaml · rules.yaml · tests/
├── github.com/     manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
├── docs.rs/        manifest.yaml · crawler.yaml · extractors.yaml · fixtures/ · tests/
└── custom/
```

每个站点爬虫都有**自己的规则、测试、版本、健康状态和降级策略**。单个站点页面结构变化
不允许破坏整个系统。

### 爬虫选择

```text
Incoming URL → Normalize → Domain → Registry
  → Exact Match → Alias Match → Generic Crawler → Fallback
```

### 故障管理

健康状态：Healthy → Degraded → Unstable → Quarantined → Disabled → Updating。错误分类
（TIMEOUT、HTTP_4xx/5xx、SELECTOR_NOT_FOUND、CONTENT_EMPTY、RATE_LIMITED…）各自有独立的
处理策略。失败的站点自动降级到通用爬虫，并被隔离直到恢复。

### 生命周期

Discovered → Validate → Compatibility → Permissions → Dependencies → Init → Health →
Registered → Active →（失败 → Degraded → Quarantined）→ Disabled。支持回滚和基于
版本快照的热更新。

### 调试

```bash
searchos crawler inspect github.com
searchos crawler test https://github.com/owner/repo
searchos crawler preview https://example.com
searchos extension validate
searchos extension reload crawler.github
```
