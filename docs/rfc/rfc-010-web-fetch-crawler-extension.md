# RFC-010: Web Fetch & Crawler Extension

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 10.1 目标

提供可持续维护的网页采集系统。

SearchOS 不承诺所有网站都能稳定抓取。

不同网站可以使用不同的专属 Crawler Extension。

## 10.2 Crawler Directory

```
extensions/crawlers/
├── generic/
│   ├── manifest.yaml
│   ├── crawler.yaml
│   ├── extractors.yaml
│   └── fixtures/
│
├── github.com/
│   ├── manifest.yaml
│   ├── crawler.yaml
│   ├── extractors.yaml
│   ├── pagination.yaml
│   ├── fixtures/
│   └── tests/
│
├── docs.rs/
│   ├── manifest.yaml
│   ├── crawler.yaml
│   ├── extractors.yaml
│   └── tests/
│
└── custom/
```

每个网站独立维护。

## 10.3 Crawler Manifest

```
id: crawler.github.com
name: GitHub Crawler
version: 1.0.0

type: site_crawler

match:
  domains:
    - github.com

capabilities:
  - fetch
  - extract
  - pagination

runtime:
  type: native
  timeout_ms: 15000

permissions:
  network:
    - github.com

healthcheck:
  enabled: true
```

## 10.4 Crawler Selection

优先级：

1. 精确域名匹配。
2. 显式注册的域名匹配。
3. Generic Crawler。
4. Fallback Fetch。

同一 URL 不允许多个专属 Crawler 无序竞争。

## 10.5 抓取执行流程

```
URL
 |
 v
URL Validation
 |
 v
Robots / Access Policy
 |
 v
Domain Crawler Selection
 |
 v
Fetch Strategy
 |
 v
HTML / JSON / PDF
 |
 v
Content Extraction
 |
 v
Normalization
 |
 v
Content Hash
 |
 v
Document Persistence
```

## 10.6 Failure States

```
pub enum CrawlerStatus {
    Healthy,
    Degraded,
    Unstable,
    Quarantined,
    Disabled,
    Updating,
}
```

错误类型：

- DNS_ERROR。
- NETWORK_ERROR。
- TIMEOUT。
- HTTP_403。
- HTTP_404。
- HTTP_429。
- HTTP_5XX。
- ROBOTS_DISALLOWED。
- SELECTOR_NOT_FOUND。
- CONTENT_EMPTY。
- PARSER_ERROR。
- BROWSER_RENDER_ERROR。

## 10.7 失败隔离

- 429 遵循 Retry-After。
- 403 不无限重试。
- Selector Not Found 标记规则异常。
- Timeout 使用有界重试。
- 空内容不得写入正常索引。
- 单站点失败不能影响其他站点。
- 连续失败达到阈值进入 Quarantine。

不提供无限制绕过反爬的能力。

## 10.8 调试 CLI

```
searchos crawler list

searchos crawler inspect github.com

searchos crawler test https://github.com/example/project

searchos crawler preview https://github.com/example/project

searchos crawler failures

searchos crawler status

searchos crawler validate
```

## 10.9 Fetch Cache

Fetch 复用条件：

- Canonical URL 相同。
- 请求方法和必要请求头兼容。
- 内容未发生变化。
- 文档仍满足当前 Freshness Policy。

支持：

- ETag。
- If-Modified-Since。
- Content Hash。
- Last Fetch。
- Next Refresh。
