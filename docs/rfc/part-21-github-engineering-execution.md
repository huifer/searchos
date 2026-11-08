# Part 21. GitHub Engineering Execution

## 21.1 建议 Repository Structure

```
searchos/
├── .github/
│   ├── ISSUE_TEMPLATE/
│   ├── workflows/
│   ├── CODEOWNERS
│   └── PULL_REQUEST_TEMPLATE.md
│
├── docs/
│   ├── RFC/
│   ├── ADR/
│   ├── architecture/
│   ├── api/
│   └── development/
│
├── crates/
├── extensions/
├── fixtures/
├── tests/
└── examples/
```

## 21.2 GitHub Labels

```
type:rfc
type:feature
type:bug
type:refactor
type:test
type:documentation

priority:p0
priority:p1
priority:p2

area:core
area:decision
area:retrieval
area:cost
area:provider
area:crawler
area:storage
area:gateway
area:security
area:evaluation

status:blocked
status:ready
status:in-progress
status:review
```

## 21.3 Milestones

| Milestone | 目标                       |
| --------- | ------------------------ |
| M0        | Architecture Foundation  |
| M1        | External Provider + Cost |
| M2        | Local Knowledge          |
| M3        | Decision Engine          |
| M4        | Cost Optimizer           |
| M5        | Semantic Reuse           |
| M6        | Agent Integration        |
| M7        | Crawler Extensions       |
| M8        | Production Release       |

## 21.4 Issue Dependency Rules

不能并行开发存在强依赖的模块。

建议：

```
Domain
  |
  +---- Application
  |
  +---- Policy
          |
          v
       Decision
          |
          v
       Planner
          |
          v
       Execution
```

存储依赖：

```
Storage Traits
    |
    +---- SQLite
    +---- Filesystem
    +---- Tantivy
    +---- LanceDB
```

成本依赖：

```
Cost Domain
    |
    +---- Provider Usage
    +---- Local Compute
    +---- Storage Usage
    +---- Cost Ledger
```

质量评估必须在 Decision Engine 完整实现之前开始准备测试数据。

## 21.5 Agent Development Rules

Codex、Claude Code 和 Pi 的开发任务必须：

- 一个 Issue 对应一个明确交付物。
- 先读取相关 RFC。
- 先检查依赖 Crate。
- 不允许擅自改变 Domain Model。
- 不允许绕过 Cost Ledger。
- 不允许在 HTTP Handler 中实现业务逻辑。
- 不允许让 Local CLI 调用 localhost HTTP。
- 不允许直接修改已发布 Migration。
- 不允许只通过 Mock 测试验证 Provider。
- 必须补齐测试。
- 必须提供完成报告和变更文件列表。

## 21.6 Pull Request Template

```
## Summary

## Related RFC

## Related Issue

## Architecture Changes

## API Changes

## Data Migration

## Cost Impact

## Quality Impact

## Security Impact

## Tests

## Benchmark

## Breaking Changes

## Checklist

- [ ] RFC compliant
- [ ] Tests passed
- [ ] No local HTTP loopback
- [ ] Cost attribution verified
- [ ] Quality regression checked
- [ ] Documentation updated
```
