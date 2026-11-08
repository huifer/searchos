# RFC-027: Analytics Data Model & Aggregation

> 分析数据模型与聚合口径

## English

### 1. Operational vs analytical authority

- Operational Store (`search_tasks`, `cost_events`, `decision_traces`,
  `documents`, `document_chunks`, `embeddings`, `crawler_runs`, ...) is the
  business source of truth.
- `analytics_events` is the append-only domain event ledger.
- `analytics_read_models` is the only query surface for the Dashboard.
- Every analytical number remains traceable to operational rows.

### 2. Event contract (migration v3)

```sql
analytics_events(
  id, event_id UNIQUE, event_type, schema_version,
  occurred_at, recorded_at, workspace_id,
  request_id, execution_id, source_module, correlation_id,
  payload_json
)
```

`search.request.completed` is emitted best-effort by the search pipeline with
status, decision path, quality, estimated cost, workspace, request id, task id
and trace id. `event_id` uniqueness makes replays idempotent.

### 3. Read Models rebuilt by `AnalyticsRepo::rebuild`

| Model key | Content |
|---|---|
| `overview` | searches, local/external counts and rates, quality pass rate, cost, savings, knowledge counts, denominators, definitions, drilldowns |
| `search_timeseries` | one row per day: total / local / external / failed |
| `search_strategies` | `final_strategy` distribution |
| `search_requests` | recent request rows with mode, strategy, status |
| `cost_breakdown` | by operation / provider / workspace / agent |
| `quality_dimensions` | freshness / coverage / source / failure classes |
| `knowledge_growth` | documents per day, chunk and embedding totals |
| `knowledge_freshness` | explicit `null` buckets with a reason until instrumented |
| `providers` | provider cost and event counts |
| `crawlers_summary` | crawl runs, success, failure, extension health reason |
| `infrastructure_summary` | SQLite bytes, index chunks, null CPU/GPU/RAM/network |

All rows carry `aggregation_version = "1"` and `updated_at`.

### 4. Metric definitions

- `Local Resolution Rate = quality-valid local tasks / all tasks`.
- `External Search Rate = tasks with external path / all tasks`.
- `Quality Pass Rate = quality-gate passing tasks / all tasks`.
- `Estimated Savings = Baseline - full SearchOS cost`, where Baseline is
  `task_count * budgets.baseline_per_query_micros`.
- Only quality-passing tasks count as valid savings; avoided external calls
  alone are not a success metric.
- Estimated costs are never presented as invoices; reconciliation writes
  actuals separately.

### 5. Missing data rule

Uninstrumented dimensions return `null` plus `reason`. The UI renders `--` or
an explicit "not instrumented" note, never `0`. Empty time series produce an
empty state, not a fabricated flat line.

---

## 中文

业务表是业务权威来源，`analytics_events` 是事件账本，`analytics_read_models`
是 Dashboard 唯一查询模型。迁移 v3 新增两张表；事件写入由搜索流程尽力而为，
`event_id` 唯一保证幂等。

`AnalyticsRepo::rebuild` 从业务记录聚合出 overview、搜索时间序列、策略分布、
请求列表、四维成本拆分、质量维度、知识增长、Provider、爬虫与基础设施读模型，
每行带 `aggregation_version` 与更新时间。

所有 KPI 明确口径与分母；节省成本只统计质量通过的任务；估算值与实际账单分离。
未埋点的资源 / 新鲜度指标返回 `null` 加原因，前端显示 `--` 或明确说明，
绝不把缺失数据伪装成 0。
