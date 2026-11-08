# RFC-004: Search Decision Engine

**Architecture Baseline:** v1.0
**Document Status:** Proposed / Engineering Baseline

## 4.1 目标

决定当前任务应该：

- 只使用本地数据。
- 本地数据加外部补充。
- 直接外部搜索。
- 更新过期文档。
- 拒绝执行并说明预算或质量限制。

## 4.2 Decision Pipeline

```
Search Request
      |
      v
Resolve Policy
      |
      v
Classify Query
      |
      v
Evaluate Local Availability
      |
      v
Select Retrieval Depth
      |
      v
Local Retrieval
      |
      v
Quality Gate
      |
      +-------- Pass --------> Local Result
      |
      +-------- Partial -----> Gap Analysis
      |
      +-------- Fail --------> External Search
                                  |
                                  v
                            Provider Router
                                  |
                                  v
                            Evidence Evaluation
                                  |
                                  v
                            Final Quality Gate
                                  |
                                  v
                               Response
```

## 4.3 Decision Types

```
pub enum SearchDecision {
    LocalOnly {
        reason: DecisionReason,
    },

    LocalWithRefresh {
        documents: Vec<DocumentId>,
        reason: DecisionReason,
    },

    Hybrid {
        missing_requirements: Vec<CoverageGap>,
        reason: DecisionReason,
    },

    ExternalOnly {
        reason: DecisionReason,
    },

    Reject {
        reason: DecisionReason,
    },
}
```

## 4.4 Decision Rules

### Rule 1：强时效性查询

当用户明确要求：

- Latest。
- Today。
- Current。
- Live。
- Real-time。

默认不允许仅凭历史本地数据完成任务。

除非本地来源本身提供可验证的实时数据，并且满足该 Profile 的时效要求。

### Rule 2：指定来源

用户指定：

- 官方网站。
- 特定 GitHub Repository。
- 指定 URL。

必须优先使用指定来源。

其他来源不能替代明确指定的来源要求。

### Rule 3：本地数据过期

若文档超过 Profile 的 Maximum Age：

- 不允许直接作为当前事实。
- 可以作为历史背景。
- 需要 Refresh 或 External Search。

### Rule 4：本地数据覆盖不足

只补齐缺失内容。

不能因为一个子问题缺少信息，就无条件重新执行所有搜索。

### Rule 5：成本预算

若所有满足质量约束的路径都超过预算：

- 不允许偷偷选择质量不合格的路径。
- 返回 Budget Exceeded。
- 提供可用的部分结果。
- 明确说明缺失信息。
- 不将部分结果标记为完整通过。

## 4.5 Decision Trace

每个决策必须记录：

```
pub struct DecisionTrace {
    pub trace_id: TraceId,
    pub task_id: TaskId,

    pub intent: Vec<SearchIntent>,
    pub profile_version: String,

    pub local_candidates: usize,
    pub accepted_candidates: usize,

    pub freshness_status: String,
    pub coverage_status: String,
    pub source_status: String,

    pub selected_path: String,
    pub rejection_reasons: Vec<String>,

    pub estimated_cost: Money,
    pub actual_cost: Option<Money>,

    pub decision_at: DateTime<Utc>,
}
```

Decision Trace 不应保存不必要的原始 API Key、隐私数据或网页中的恶意指令。
