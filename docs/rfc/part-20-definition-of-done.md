# Part 20. Definition of Done

一个 RFC 对应的开发任务只有满足以下条件才能关闭：

1. 代码实现与 RFC 一致。
2. Public API 有文档。
3. Unit Tests 通过。
4. Integration Tests 通过。
5. Error Handling 完整。
6. Logging 和 Tracing 完整。
7. Cost Attribution 完整（适用时）。
8. Security Checks 通过。
9. Migration 可执行（适用时）。
10. CLI / API 文档同步。
11. Benchmark 已更新（适用时）。
12. 不存在未解释的质量回归。

## 20.1 全项目 MVP 验收

SearchOS MVP 必须证明：

- Local Mode 不启动 HTTP Server。
- Gateway Mode 使用相同 Core。
- 本地知识能够被复用。
- 本地数据过期时能够正确触发外部搜索。
- 相似查询不会盲目复用错误证据。
- 多 Agent 相同请求可以合并执行。
- Provider 费用可以追踪。
- 本地计算成本可以估算。
- 文档内容版本可以追溯。
- 索引可以重建。
- 失败的 Provider 可以降级。
- Search Decision 可以解释。
- Search Quality 可以评估。
- 在同等质量标准下，能够证明成本下降。
