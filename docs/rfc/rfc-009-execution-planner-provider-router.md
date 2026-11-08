# RFC-009: Execution Planner & Provider Router

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 9.1 目标

把搜索任务拆成一组可执行操作，并选择满足质量要求的低成本执行路径。

## 9.2 Execution Plan

```
pub struct ExecutionPlan {
    pub id: PlanId,
    pub task_id: TaskId,

    pub nodes: Vec<ExecutionNode>,
    pub dependencies: Vec<ExecutionDependency>,

    pub estimated_cost: Money,
    pub estimated_duration_ms: u64,

    pub budget: SearchBudget,
}
```

节点示例：

```
Query Normalize
       |
       v
Local Retrieval
       |
       v
Quality Evaluation
       |
       +------ Pass ------> Return
       |
       v
External Search
       |
       v
Fetch Documents
       |
       v
Extract
       |
       v
Persist
       |
       v
Return
```

## 9.3 Provider Capabilities

```
pub struct ProviderCapabilities {
    pub search: bool,
    pub extract: bool,
    pub crawl: bool,
    pub map: bool,
    pub semantic_search: bool,
    pub supports_domain_filter: bool,
    pub supports_date_filter: bool,
}
```

Provider 的能力必须由实际 Adapter 声明，不得仅凭名称推断。

## 9.4 Provider Selection

第一阶段过滤：

- 不支持当前任务。
- 不满足来源要求。
- 质量评估不合格。
- 已达到限流。
- Provider 不健康。
- 超出允许预算。

第二阶段排序：

[\
ProviderScore =\
f(Quality, Coverage, Freshness, Cost, Latency, Reliability)\
]

质量和硬性约束先于排序。

## 9.5 Provider Registry

初始 Provider：

- Tavily。
- Brave Search。
- Exa。
- SearXNG。
- Custom HTTP Provider。

初期只需要实现 Tavily 和 Brave 两个 Provider，其他通过接口扩展。

## 9.6 Budget

```
budget:
  max_external_queries: 5
  max_fetch_pages: 10
  max_total_cost: 0.10
  max_execution_time_ms: 30000
  max_retries: 2
  max_query_expansions: 3
```

预算应支持：

- 单次任务。
- Agent。
- Workspace。
- Provider。

预算限制必须在实际执行前检查，并对并发任务预留预算，避免多个任务同时突破额度。

## 9.7 Failure Fallback

Provider 错误分类：

- Authentication Error。
- Rate Limit。
- Timeout。
- Network Error。
- Invalid Response。
- Quota Exhausted。
- Provider Unavailable。

只有可重试错误才进入 Retry。

429 遵循 Retry-After。

403 不进行无限重试。

Provider 失败时：

- 尝试符合质量要求的其他 Provider。
- 记录失败费用。
- 更新健康状态。
- 保留原始错误。
- 返回部分结果或明确失败。

## 9.8 Singleflight

相同有效请求并发时：

- 生成 Request Fingerprint。
- 只允许一个执行者执行。
- 其他请求等待共享结果。
- 所有请求保留各自 Agent Attribution。
- 真实外部成本只记录一次。
- 共享收益单独记录，不重复计费。

Gateway 后续支持跨进程请求合并。

Local Mode 首先使用进程内 Singleflight。
