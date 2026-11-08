# RFC-005: Quality Gate & Evidence Evaluation

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 5.1 质量目标

SearchOS 不负责判断最终答案的所有语义正确性，但必须负责判断搜索证据是否满足任务约束。

## 5.2 Evidence Model

```
pub struct Evidence {
    pub id: EvidenceId,
    pub source_id: SourceId,
    pub document_version_id: Option<DocumentVersionId>,

    pub title: Option<String>,
    pub url: String,

    pub content_ref: Option<String>,
    pub snippet: Option<String>,

    pub published_at: Option<DateTime<Utc>>,
    pub fetched_at: DateTime<Utc>,

    pub source_reliability: ReliabilityLevel,
    pub provenance: Provenance,
}
```

## 5.3 Quality Dimensions

| 维度                 | 解释           | 是否可能是硬约束 |
| ------------------ | ------------ | -------- |
| Relevance          | 与 Query 的相关性 | 是        |
| Freshness          | 是否足够新        | 是        |
| Coverage           | 是否覆盖必要问题     | 是        |
| Source Reliability | 来源可靠程度       | 是        |
| Provenance         | 是否可追溯        | 是        |
| Diversity          | 是否存在独立来源     | 视任务而定    |
| Consistency        | 证据是否互相矛盾     | 视任务而定    |

## 5.4 质量评估方式

先检查硬约束：

```
if freshness_requirement_failed:
    reject_as_current_evidence

if required_domain_missing:
    reject

if provenance_missing:
    reject_if_provenance_required

if coverage_below_minimum:
    mark_partial
```

然后对候选证据进行评分。

初始模型：

[\
Q =\
w_r R +\
w_f F +\
w_c C +\
w_s S +\
w_p P\
]

但必须注意：

- 分数用于候选排序和评估。
- 硬性约束不能被加权分数抵消。
- 评分不代表经过校准的正确概率。
- 必须通过 Evaluation Dataset 校准阈值。

## 5.5 Freshness Policy

建议初始 TTL：

| 内容类别           | 初始 TTL     |
| -------------- | ---------- |
| 静态技术概念         | 180 天      |
| 官方技术文档         | 7–30 天     |
| GitHub Release | 1–3 天      |
| API Pricing    | 6–24 小时    |
| 新闻             | 15–60 分钟   |
| 实时信息           | 不缓存或极短 TTL |

TTL 是默认刷新策略，不等于文档在 TTL 到期前绝对正确。

支持：

- Last-Modified。
- ETag。
- Content Hash。
- Published At。
- Fetched At。
- Source-Specific Freshness Policy。

## 5.6 Evidence Coverage

复杂 Query 拆成必要子问题：

```
Compare A, B and C

  A:
    pricing
    features
    latest version

  B:
    pricing
    features
    latest version

  C:
    pricing
    features
    latest version
```

每个子问题都有独立证据覆盖状态。

当 A、B 已有有效证据，而 C 缺少价格信息时，只搜索 C 的价格。

## 5.7 Contradiction Handling

当多个来源互相矛盾：

1. 不直接覆盖历史证据。
2. 保留各自来源。
3. 记录发布时间和抓取时间。
4. 优先验证官方来源。
5. 必要时触发外部刷新。
6. 标记 Conflict。
7. 不将未解决冲突伪装成质量通过。

## 5.8 Quality Status

```
pub enum QualityStatus {
    Passed,
    Partial,
    Failed,
    Conflict,
    Unknown,
}
```

每个 Search Response 必须返回 Quality Status。
