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
                     [--max-cost 0.05] [--request-id <idempotency-key>]
searchos fetch <url> [--workspace default]
```

## Documents / memory

```text
searchos document list|inspect <id>|refresh <id>|delete <id>
searchos memory search "Rust web framework" [--workspace default] [--top-k 5]
searchos memory stats
```

## Providers / profiles / cost / trace

```text
searchos provider list|add <tavily|brave>|health
searchos profile list
searchos profile inspect <id>                 # versioned SearchProfile
searchos cost summary|agents|providers|operations|savings|reservations
searchos cost reconcile --event <cost-event-id> --actual 0.005 [--source provider_billing]
searchos cost export [--out file]
searchos trace show <trace-id>               # includes structured stage trace
```

`cost savings` / `stats savings` expose `SavingsRate`, `CPQPA`, external
avoidance and the four reuse cost views. `cost reconcile` imports real
provider billing without ever rewriting the original estimate.

## Index / stats / gateway / config

```text
searchos index status|stats|rebuild|verify
searchos stats retrieval                    # hit rates, status counts
searchos stats savings
searchos config validate                   # exit 2 on invalid config
searchos gateway start --port 8787 --host 127.0.0.1
searchos gateway status
```

## Crawler / extension / backup

```text
searchos crawler list|inspect <domain>|test <url>|preview <url>|failures|status|validate
searchos extension list|validate|inspect <id>
searchos backup create|restore <path>
```

## Evaluation / export / backup

```text
searchos eval run [--dataset fixtures/evaluations/dataset-v1.jsonl]
                  [--limit N] [--out report.json]
searchos export [--out file] [--workspace default]   # secrets never exported
searchos backup create                              # directory + SHA-256 manifest
searchos backup restore <backup-dir | legacy .db>   # checksum + integrity check
```

## MCP

```text
searchos mcp    # JSON-RPC 2.0 over stdio: initialize, tools/list, tools/call
```

Tools: `search`, `fetch`, `extract`, `memory_search`, `document_search`,
`cost_summary`, `trace_inspect` (`search_trace` alias) — each with an explicit
parameter schema (RFC-012 12.8, RFC-021 21.5). `search` supports a
`request_id` idempotency key and describes `local_only` vs external-capable modes.

## Search response status

Every `/search` and `searchos search` response now carries an explicit
`status` (`completed | completed_with_warnings | partial | insufficient_evidence |
budget_exhausted | failed | cancelled`), `warnings[]`, `task_id` and `replayed`.
A partial or insufficient result is never reported as a completed
quality-passing search (RFC-002 2.8). `ExternalOnly` / strict-freshness queries
bypass the exact cache (RFC-003 3.4, RFC-004 v1.1 9.5).
