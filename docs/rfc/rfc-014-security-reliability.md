# RFC-014: Security & Reliability

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 14.1 SSRF Protection

所有 URL Fetch 必须经过统一安全检查。

要求：

- 禁止 localhost。
- 禁止私有 IPv4 / IPv6。
- 禁止 Cloud Metadata 地址。
- DNS 解析结果校验。
- 每次 Redirect 重新校验。
- 限制跳转次数。
- 限制响应大小。
- 限制请求时间。
- 限制每域名并发。

DNS Rebinding 必须纳入测试。

## 14.2 Prompt Injection

网页内容是 Untrusted Data。

要求：

- 不执行网页中的 Agent 指令。
- 不把网页内容作为 System Prompt。
- 内容和控制指令分离。
- 保留来源和原始内容。
- 在传递给上层 Agent 时标注 Untrusted Evidence。

## 14.3 API Key

- 不写入普通日志。
- 本地使用系统安全存储或受权限保护的配置。
- Gateway 使用 Secret Provider。
- 数据库仅保存 Secret Reference。
- 支持轮换。
- 支持撤销。

## 14.4 Reliability

必须实现：

- Timeout。
- Bounded Retry。
- Exponential Backoff。
- Circuit Breaker。
- Provider Health Check。
- Graceful Shutdown。
- Job Recovery。
- Dead Letter Queue。
- Index Rebuild。
- Backup & Restore。

## 14.5 Backup

SQLite、Filesystem 和索引之间需要明确一致性策略。

备份至少包括：

- SQLite Snapshot。
- Object Files。
- Config。
- Extension Manifest。
- Index Metadata。

Tantivy 和 LanceDB 可以选择备份，也可以从权威文档数据重建。

必须提供恢复验证。
