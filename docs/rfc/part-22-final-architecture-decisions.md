# Part 22. Final Architecture Decisions

SearchOS 的最终技术路线：

| 领域              | 最终选择                          |
| --------------- | ----------------------------- |
| Language        | Rust                          |
| Runtime         | Tokio                         |
| HTTP            | Axum                          |
| CLI             | Clap                          |
| Serialization   | Serde                         |
| Metadata        | SQLite                        |
| Full Text       | Tantivy                       |
| Vector          | LanceDB                       |
| Content         | Filesystem                    |
| Query Cache     | SQLite + In-Memory            |
| External Search | Tavily / Brave / Exa Adapter  |
| Fetch           | Rust HTTP Client              |
| HTML Parsing    | HTML Parser Adapter           |
| Markdown        | Markdown Extraction Adapter   |
| Embedding       | Pluggable Model Provider      |
| Reranker        | Optional Provider             |
| MCP             | MCP stdio                     |
| Observability   | tracing + metrics             |
| Evaluation      | SearchOS Evaluation Framework |
| Extensions      | YAML + Native + Later WASM    |
| Gateway         | Axum                          |
| Local Runtime   | In-Process Rust Core          |

以上是初始实现基线，不意味着所有组件都必须同时进入 MVP。
