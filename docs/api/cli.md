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

## Search / fetch / ingest

```text
searchos search "Rust Tokio" [--profile latest] [--mode local-only]
                     [--workspace default] [--max-external-queries 3]
                     [--max-cost 0.05] [--max-results 5]
                     [--request-id <idempotency-key>]
searchos fetch <url> [--workspace default]
```

## Local ingestion (RFC-029)

```text
searchos ingest --text "..." [--source-key notes/alpha] [--title "..."]
                     [--content-type text/plain] [--workspace default]
searchos ingest --file ./notes.md [--source-key notes/alpha] [--workspace default]
searchos ingest --dir ./docs [--recursive] [--ext md,txt] [--workspace default]
```

Exactly one of `--text`, `--file`, `--dir` is required. `--source-key` is the
stable logical identity; without it the source is content-addressed. `--dir`
skips dotfiles, sorts by relative path, and uses each file's relative path as
its `source_key`. Imported documents are searchable immediately under
`--mode local-only` / `local_first`.

## Documents / memory

```text
searchos document list [--limit 50] [--source-type web|local|local_text|local_file|local_directory]
                             [--workspace <id>]
searchos document inspect <id>|refresh <id>|delete <id>
searchos memory remember --title "..." --content "..." [--workspace default]
                         [--sharing private|workspace_shared|project_shared|global_public]
                         [--tags a,b]
searchos memory search "Rust web framework" [--workspace default] [--top-k 5]
searchos memory stats
```

## Providers / profiles / cost / trace

```text
searchos provider list|check-key <tavily|brave>|health   # add = env key check, never writes config
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
searchos extension list [--quarantined]|validate|inspect <id>
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

**Transport: stdio only (ADR-031).** There is no HTTP MCP listener and the
gateway exposes no MCP route; remote agents use the REST/Tavily surface. See
[`docs/mcp.md`](../mcp.md).

Tools: `search`, `fetch`, `extract`, `ingest_text`, `ingest_file`,
`memory_search`, `document_search`, `cost_summary`, `trace_inspect`
(`search_trace` alias) — RFC-021's tools plus the two RFC-029 ingest tools and
one documented compatibility alias. Each has an explicit parameter schema
(RFC-012 12.8, RFC-021 21.5). `notifications/cancelled` is acknowledged
(counted + logged) rather than silently dropped. `search` accepts snake_case or kebab-case
mode names, rejects unknown modes with -32602, supports a `request_id`
idempotency key and a `max_results` output cap, and describes `local_only`
vs external-capable modes.

## Search response status

Every `/search` and `searchos search` response now carries an explicit
`status` (`completed | completed_with_warnings | partial | insufficient_evidence |
budget_exhausted | failed | cancelled`), `warnings[]`, `task_id` and `replayed`.
A partial or insufficient result is never reported as a completed
quality-passing search (RFC-002 2.8). `ExternalOnly` / strict-freshness queries
bypass the exact cache (RFC-003 3.4, RFC-004 v1.1 9.5).
