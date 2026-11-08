# RFC-015: Observability & Evaluation

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 15.1 Trace

每次 Search Request 生成一个 Trace。

```
Search Request
   |
   +---- Query Normalize
   |
   +---- Intent Classification
   |
   +---- Cache Lookup
   |
   +---- Local Retrieval
   |
   +---- Quality Evaluation
   |
   +---- Provider Routing
   |
   +---- External Search
   |
   +---- Fetch
   |
   +---- Persistence
   |
   +---- Cost Accounting
   |
   +---- Response
```

## 15.2 Metrics

### Search Metrics

- Search Requests。
- Local Resolution Rate。
- External Search Rate。
- Cache Hit Rate。
- Semantic Reuse Rate。
- Singleflight Reuse Rate。
- Search P50 / P95 / P99。

### Quality Metrics

- Evidence Recall。
- Coverage Pass Rate。
- Freshness Violation Rate。
- Source Reliability Pass Rate。
- Conflict Rate。
- Quality Pass Rate。

### Cost Metrics

- Cost Per Query。
- Cost Per Successful Query。
- External API Cost。
- Local Compute Cost。
- Fetch Cost。
- Embedding Cost。
- Reranker Cost。
- Storage Cost。
- Cost Avoidance。

### Reliability Metrics

- Provider Failure Rate。
- Crawler Success Rate。
- Extension Health。
- Index Consistency。
- Job Recovery Rate。

## 15.3 Evaluation Dataset

第一阶段准备至少 500 个问题。

| 类型         | 数量  |
| ---------- | --- |
| 静态技术知识     | 100 |
| 技术文档       | 100 |
| GitHub     | 75  |
| 新闻与最新信息    | 75  |
| 产品及 API 价格 | 50  |
| 复杂对比研究     | 50  |
| 中英文混合      | 50  |

## 15.4 Baselines

必须比较三种系统：

A. External Only。

B. Traditional Exact Cache + TTL。

C. SearchOS Local Retrieval + Quality Gate + Cost Optimizer。

必须在同等质量要求下比较成本。

## 15.5 Acceptance Targets

初始工程目标：

| 指标                 | 目标         |
| ------------------ | ---------- |
| Decision Trace 覆盖率 | 100%       |
| Cost Event 记录覆盖率   | 100%       |
| 关键证据覆盖率            | ≥ 95%      |
| 强时效数据错误复用          | 0 容忍       |
| 相同并发请求合并           | 100%      |
| 静态知识本地解决率          | ≥ 60%      |
| 质量相对基线             | 不低于预设非劣效界限 |

所有指标必须通过可重复测试验证。

## 15.6 非劣效评估

不能简单认为 SearchOS 的结果和 External Only 一样。

应当：

1. 固定测试集。
2. 固定时间窗口。
3. 固定 Provider 配置。
4. 固定质量标准。
5. 比较 Evidence Recall。
6. 比较 Freshness。
7. 比较人工标注质量。
8. 比较全链路成本。

只有质量达到非劣效标准，成本下降才算有效。
