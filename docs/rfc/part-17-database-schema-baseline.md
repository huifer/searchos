# Part 17. Database Schema Baseline

第一版数据库使用 SQLite。

以下为核心 Schema 基线。正式 Migration 必须为每张表补齐完整外键、索引、CHECK 约束和版本管理。

## 17.1 Workspaces

```
CREATE TABLE workspaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    config_version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

## 17.2 Agents

```
CREATE TABLE agents (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    agent_type TEXT,
    created_at TEXT NOT NULL,

    FOREIGN KEY(workspace_id)
      REFERENCES workspaces(id)
);
```

## 17.3 Search Tasks

```
CREATE TABLE search_tasks (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    agent_id TEXT,

    query TEXT NOT NULL,
    normalized_query TEXT NOT NULL,
    query_fingerprint TEXT NOT NULL,

    profile_id TEXT NOT NULL,
    profile_version TEXT NOT NULL,

    mode TEXT NOT NULL,
    status TEXT NOT NULL,

    started_at TEXT NOT NULL,
    completed_at TEXT,

    trace_id TEXT NOT NULL,

    FOREIGN KEY(workspace_id)
      REFERENCES workspaces(id),

    FOREIGN KEY(agent_id)
      REFERENCES agents(id)
);
```

## 17.4 Search Decisions

```
CREATE TABLE search_decisions (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,

    decision_type TEXT NOT NULL,
    reason_code TEXT NOT NULL,
    reason_detail TEXT,

    quality_status TEXT,
    estimated_cost_micros INTEGER,
    actual_cost_micros INTEGER,

    policy_version TEXT NOT NULL,
    created_at TEXT NOT NULL,

    FOREIGN KEY(task_id)
      REFERENCES search_tasks(id)
);
```

## 17.5 Sources

```
CREATE TABLE sources (
    id TEXT PRIMARY KEY,
    canonical_url TEXT NOT NULL,
    domain TEXT NOT NULL,

    source_type TEXT NOT NULL,
    reliability_level TEXT,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    UNIQUE(canonical_url)
);
```

## 17.6 Documents

```
CREATE TABLE documents (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,

    canonical_url TEXT NOT NULL,
    current_version_id TEXT,

    language TEXT,
    content_type TEXT,

    status TEXT NOT NULL,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,

    FOREIGN KEY(source_id)
      REFERENCES sources(id)
);
```

## 17.7 Document Versions

```
CREATE TABLE document_versions (
    id TEXT PRIMARY KEY,
    document_id TEXT NOT NULL,

    content_hash TEXT NOT NULL,
    raw_content_ref TEXT NOT NULL,
    markdown_ref TEXT,

    published_at TEXT,
    fetched_at TEXT NOT NULL,

    extractor_version TEXT NOT NULL,
    crawler_version TEXT,

    created_at TEXT NOT NULL,

    FOREIGN KEY(document_id)
      REFERENCES documents(id),

    UNIQUE(document_id, content_hash)
);
```

## 17.8 Document Chunks

```
CREATE TABLE document_chunks (
    id TEXT PRIMARY KEY,
    version_id TEXT NOT NULL,

    chunk_hash TEXT NOT NULL,
    content_ref TEXT NOT NULL,

    token_count INTEGER,
    chunk_index INTEGER NOT NULL,

    created_at TEXT NOT NULL,

    FOREIGN KEY(version_id)
      REFERENCES document_versions(id),

    UNIQUE(version_id, chunk_hash)
);
```

## 17.9 Search Cache

```
CREATE TABLE search_cache (
    id TEXT PRIMARY KEY,

    cache_key TEXT NOT NULL UNIQUE,
    result_ref TEXT NOT NULL,

    profile_version TEXT NOT NULL,
    policy_version TEXT NOT NULL,

    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);
```

## 17.10 Providers

```
CREATE TABLE providers (
    id TEXT PRIMARY KEY,

    name TEXT NOT NULL,
    provider_type TEXT NOT NULL,

    status TEXT NOT NULL,
    config_ref TEXT NOT NULL,

    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
```

## 17.11 Cost Events

```
CREATE TABLE cost_events (
    id TEXT PRIMARY KEY,

    task_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    agent_id TEXT,

    operation TEXT NOT NULL,
    provider_id TEXT,

    input_units INTEGER NOT NULL DEFAULT 0,
    output_units INTEGER NOT NULL DEFAULT 0,

    estimated_cost_micros INTEGER,
    actual_cost_micros INTEGER,

    currency TEXT NOT NULL,

    duration_ms INTEGER,
    occurred_at TEXT NOT NULL,

    FOREIGN KEY(task_id)
      REFERENCES search_tasks(id)
);
```

## 17.12 Index Outbox

```
CREATE TABLE index_outbox (
    id TEXT PRIMARY KEY,

    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,

    operation TEXT NOT NULL,
    status TEXT NOT NULL,

    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,

    created_at TEXT NOT NULL,
    processed_at TEXT
);
```

## 17.13 Additional Required Tables

正式实现还必须包括：

- search_results。
- search_result_sources。
- search_profiles。
- search_profile_versions。
- search_budgets。
- provider_usage。
- cost_models。
- cost_model_versions。
- extension_registry。
- extension_versions。
- crawler_runs。
- crawler_failures。
- embeddings。
- retrieval_cache。
- evaluation_runs。
- evaluation_results。
- index_status。
- audit_events。

所有 Schema 变更必须使用版本化 Migration。
