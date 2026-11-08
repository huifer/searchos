# Part 18. CLI Command Contract

```
# Core
searchos init
searchos doctor
searchos version

# Search
searchos search "Rust Tokio"
searchos search "Latest Rust release" --profile latest
searchos search "Rust Tokio" --mode local-only
searchos search "Latest OpenAI models" --mode external-only

# Fetch
searchos fetch https://example.com

# Documents
searchos document list
searchos document inspect <id>
searchos document refresh <id>
searchos document delete <id>

# Memory
searchos memory search "Rust web framework"
searchos memory stats

# Provider
searchos provider list
searchos provider add tavily
searchos provider health

# Cost
searchos cost summary
searchos cost agents
searchos cost providers
searchos cost operations
searchos cost export

# Trace
searchos trace show <trace-id>

# Index
searchos index status
searchos index rebuild
searchos index verify

# Gateway
searchos gateway start --port 8787
searchos gateway status

# Crawler
searchos crawler list
searchos crawler inspect github.com
searchos crawler test <url>
searchos crawler failures

# Extension
searchos extension list
searchos extension validate
searchos extension inspect <id>

# Backup
searchos backup create
searchos backup restore <path>
```

CLI 必须支持：

- JSON Output。
- Human-readable Output。
- Markdown Output。
- Shell Completion。
- Exit Codes。
- Non-interactive Mode。
- stdout / stderr 分离。
- 配置文件覆盖。
- 环境变量覆盖。
