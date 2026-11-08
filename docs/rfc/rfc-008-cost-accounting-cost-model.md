# RFC-008: Cost Accounting & Cost Model

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 8.1 成本范围

必须覆盖：

| 类别              | 子项                    |
| --------------- | --------------------- |
| External Search | Tavily、Exa、Brave      |
| External Fetch  | Extract API、Crawl API |
| Network         | 请求流量、响应流量             |
| Embedding       | 本地和远程 Embedding       |
| Reranking       | 本地和远程 Reranker        |
| Local Compute   | CPU、GPU、RAM、I/O       |
| Storage         | SSD、对象存储、备份           |
| Maintenance     | 索引重建、重新抓取             |
| Model Inference | 决策、分类等模型推理            |

## 8.2 Cost Model

```
pub struct Money {
    pub amount_micros: i64,
    pub currency: String,
}

pub struct CostEvent {
    pub id: CostEventId,

    pub task_id: TaskId,
    pub workspace_id: WorkspaceId,
    pub agent_id: Option<AgentId>,

    pub operation: CostOperation,
    pub provider_id: Option<String>,

    pub input_units: u64,
    pub output_units: u64,

    pub estimated_cost: Money,
    pub actual_cost: Option<Money>,

    pub duration_ms: u64,
    pub occurred_at: DateTime<Utc>,
}
```

金额必须使用整数最小货币单位，避免浮点金额计算误差。

对于不能直接货币化的资源，例如 CPU 时间和内存占用，应同时保留原始资源指标与折算成本。

## 8.3 Cost Operations

```
pub enum CostOperation {
    WebSearch,
    WebFetch,
    WebCrawl,
    Embedding,
    Reranking,
    QueryInference,
    LocalBm25,
    LocalVector,
    LocalCompute,
    StorageWrite,
    StorageRead,
    IndexMaintenance,
    NetworkTransfer,
}
```

## 8.4 Cost Model Versioning

```
provider:
  tavily:
    currency: USD
    pricing_model: request_based
    unit_price: 0.005
    version: 1

local:
  embedding:
    pricing_model: resource_based
    electricity_cost_per_kwh: 0.10
    amortization_enabled: false
```

上面的价格只是配置格式示例，不能作为真实供应商报价。

每次成本计算都必须记录：

- Pricing Model Version。
- 汇率版本。
- 计费时间。
- 单位。
- 实际用量。
- 是否来自真实账单。

## 8.5 Cost Attribution

每笔成本必须归因到：

- Workspace。
- Agent。
- Search Task。
- Query。
- Provider。
- Operation。

不能只统计 Provider 月账单而无法定位具体任务。

## 8.6 Cost Metrics

核心指标：

[\
CostPerSuccessfulQuery =\
\frac{TotalCost}{QualityPassingQueries}\
]

[\
Savings =\
1-\frac{SearchOSCost}{ExternalOnlyBaselineCost}\
]

[\
ExternalAvoidanceRate =\
1-\frac{ExternalCalls\_{SearchOS}}{ExternalCalls\_{Baseline}}\
]

必须注意：

- Baseline 使用同等质量标准。
- Baseline 必须使用可重复的测试环境。
- 不能把本地成本记为零。
- 不能将估算成本说成真实账单。
- 缓存未命中不等于成本优化失败。

## 8.7 Cost Ledger

Cost Event 采用 Append-Only 设计。

纠错通过补充调整事件完成，不直接覆盖历史账本。

支持：

- 日统计。
- 月统计。
- Agent 统计。
- Workspace 统计。
- Provider 统计。
- Operation 统计。
- Cost Per Quality-Passing Answer。
