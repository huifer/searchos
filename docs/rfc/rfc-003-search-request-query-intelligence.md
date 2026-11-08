# RFC-003: Search Request & Query Intelligence

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 3.1 目标

将不同 Agent 提交的自然语言 Query 转换成标准化搜索任务。

## 3.2 Query Processing Pipeline

```
Raw Query
   |
   v
Language Detection
   |
   v
Normalization
   |
   v
Entity Extraction
   |
   v
Intent Classification
   |
   v
Temporal Sensitivity
   |
   v
Source Requirement
   |
   v
Query Fingerprint
   |
   v
Search Profile Resolution
```

## 3.3 Query Intent

```
pub enum SearchIntent {
    Concept,
    Technical,
    ExactUrl,
    Latest,
    News,
    Price,
    Comparison,
    Research,
    Troubleshooting,
    Documentation,
    Unknown,
}
```

Intent 必须可以多标签表达。

例如：

`Compare latest Tokio and Axum versions`

可能包含：

- Comparison。
- Latest。
- Technical。
- Documentation。

不要强行限制为一个 Intent。

## 3.4 Query Fingerprint

Fingerprint 需要区分：

- 归一化后的 Query。
- 语言。
- 搜索 Profile。
- 时间约束。
- 来源约束。
- Workspace 范围。

建议：

```
fingerprint =
SHA256(
  normalized_query
  + language
  + profile_version
  + freshness_policy
  + source_policy
  + workspace_scope
)
```

不能把不同质量约束的请求合并为同一缓存。

## 3.5 Search Profiles

内置配置：

| Profile   | Freshness      | Source             | Coverage       | 默认策略              |
| --------- | -------------- | ------------------ | -------------- | ----------------- |
| general   | Recent         | Trusted            | Normal         | Local First       |
| technical | Recent         | Official Preferred | High           | Local First       |
| latest    | Strict         | Official Preferred | High           | External First    |
| research  | Strict         | Multiple Sources   | Very High      | Hybrid            |
| exact     | Policy-defined | Exact Source       | Exact          | Exact Local First |
| offline   | Any            | Local Only         | Policy-defined | Local Only        |

Profile 配置必须支持版本管理。

历史 Search Task 必须保存当时使用的 Profile Version。

## 3.6 Query Expansion

Query Expansion 不是默认开启。

触发条件：

- 当前 Query 无法覆盖必要信息。
- 外部结果数量不足。
- 结果来源高度重复。
- 查询本身是复杂研究任务。

初始限制：

```
query_expansion:
  enabled: true
  max_expansions: 3
  max_external_queries: 5
  require_coverage_gap: true
```
