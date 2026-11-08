# CLI Reference

> Part 18 command contract. Output: `--format json|human|markdown` (default
> human); exit codes: 0 ok, 2 validation, 3 budget, 4 policy/security,
> 5 provider/retrieval/timeout, 1 internal. Config: file
> (`SEARCHOS_CONFIG` or `~/.searchos/config/config.yaml`) then env overrides
> (`SEARCHOS_DATA_DIR`, `TAVILY_API_KEY`, `BRAVE_API_KEY`,
> `SEARCHOS_AUTH_TOKEN`). Local Mode is in-process — never HTTP loopback.

## Core

```text
searchos init              Initialize data dir + SQLite (migrations run)
searchos doctor            Diagnose config, storage, index consistency
searchos version           Print version
```

## Search / fetch

```text
searchos search "Rust Tokio" [--profile latest] [--mode local-only]
                     [--workspace default] [--max-external-queries 3]
                     [--max-cost 0.05]
searchos fetch <url> [--workspace default]
```

## Documents / memory

```text
searchos document list|inspect <id>|refresh <id>|delete <id>
searchos memory search "Rust web framework" [--workspace default] [--top-k 5]
searchos memory stats
```

## Providers / cost / trace

```text
searchos provider list|add <tavily|brave>|health
searchos cost summary|agents|providers|operations|export [--out file]
searchos trace show <trace-id>
```

## Index / gateway

```text
searchos index status|rebuild|verify
searchos gateway start --port 8787 --host 127.0.0.1
searchos gateway status
```

## Crawler / extension / backup

```text
searchos crawler list|inspect <domain>|test <url>|preview <url>|failures|status|validate
searchos extension list|validate|inspect <id>
searchos backup create|restore <path>
```

## MCP

```text
searchos mcp    # JSON-RPC 2.0 over stdio: initialize, tools/list, tools/call
```

Tools: `search`, `extract`, `memory_search`, `cost_summary`, `search_trace`
— each with an explicit parameter schema (RFC-012 12.8).
