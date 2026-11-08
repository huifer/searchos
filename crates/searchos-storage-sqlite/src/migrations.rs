//! Versioned migrations (Part 17, RFC-016 16.x: 不允许直接修改已发布
//! Migration — new schema changes append new versions).
//!
//! Version tracking uses `PRAGMA user_version`.

use rusqlite::Connection;

/// A single migration step.
pub struct Migration {
    pub version: i64,
    pub name: &'static str,
    pub sql: &'static str,
}

/// All migrations in order. Version 1 = Part 17 core schema + the additional
/// required tables listed in 17.13.
pub static MIGRATIONS: &[Migration] = &[Migration {
    version: 1,
    name: "part17_schema_baseline",
    sql: r#"
-- 17.1 Workspaces
CREATE TABLE IF NOT EXISTS workspaces (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    config_version INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- 17.2 Agents
CREATE TABLE IF NOT EXISTS agents (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    name TEXT NOT NULL,
    agent_type TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(workspace_id) REFERENCES workspaces(id)
);

-- 17.3 Search Tasks
CREATE TABLE IF NOT EXISTS search_tasks (
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
    FOREIGN KEY(workspace_id) REFERENCES workspaces(id),
    FOREIGN KEY(agent_id) REFERENCES agents(id)
);
CREATE INDEX IF NOT EXISTS idx_search_tasks_fingerprint ON search_tasks(query_fingerprint);
CREATE INDEX IF NOT EXISTS idx_search_tasks_workspace ON search_tasks(workspace_id, started_at);

-- 17.4 Search Decisions
CREATE TABLE IF NOT EXISTS search_decisions (
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
    FOREIGN KEY(task_id) REFERENCES search_tasks(id)
);
CREATE INDEX IF NOT EXISTS idx_decisions_task ON search_decisions(task_id);

-- 17.5 Sources
CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY,
    canonical_url TEXT NOT NULL,
    domain TEXT NOT NULL,
    source_type TEXT NOT NULL,
    reliability_level TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(canonical_url)
);

-- 17.6 Documents
CREATE TABLE IF NOT EXISTS documents (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL,
    canonical_url TEXT NOT NULL,
    current_version_id TEXT,
    language TEXT,
    content_type TEXT,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    workspace_id TEXT,
    FOREIGN KEY(source_id) REFERENCES sources(id)
);
CREATE INDEX IF NOT EXISTS idx_documents_url ON documents(canonical_url);

-- 17.7 Document Versions
CREATE TABLE IF NOT EXISTS document_versions (
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
    FOREIGN KEY(document_id) REFERENCES documents(id),
    UNIQUE(document_id, content_hash)
);

-- 17.8 Document Chunks
CREATE TABLE IF NOT EXISTS document_chunks (
    id TEXT PRIMARY KEY,
    version_id TEXT NOT NULL,
    document_id TEXT NOT NULL,
    chunk_hash TEXT NOT NULL,
    content_ref TEXT NOT NULL,
    token_count INTEGER,
    chunk_index INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(version_id) REFERENCES document_versions(id),
    UNIQUE(version_id, chunk_hash)
);
CREATE INDEX IF NOT EXISTS idx_chunks_version ON document_chunks(version_id);

-- 17.9 Search Cache
CREATE TABLE IF NOT EXISTS search_cache (
    id TEXT PRIMARY KEY,
    cache_key TEXT NOT NULL UNIQUE,
    result_ref TEXT NOT NULL,
    profile_version TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL
);

-- 17.10 Providers
CREATE TABLE IF NOT EXISTS providers (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    provider_type TEXT NOT NULL,
    status TEXT NOT NULL,
    config_ref TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

-- 17.11 Cost Events (append-only ledger, ADR-015)
CREATE TABLE IF NOT EXISTS cost_events (
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
    trace_id TEXT,
    shared INTEGER NOT NULL DEFAULT 0,
    pricing_model_version TEXT NOT NULL DEFAULT '1',
    FOREIGN KEY(task_id) REFERENCES search_tasks(id)
);
CREATE INDEX IF NOT EXISTS idx_cost_workspace_time ON cost_events(workspace_id, occurred_at);
CREATE INDEX IF NOT EXISTS idx_cost_task ON cost_events(task_id);
CREATE INDEX IF NOT EXISTS idx_cost_provider ON cost_events(provider_id);

-- 17.12 Index Outbox
CREATE TABLE IF NOT EXISTS index_outbox (
    id TEXT PRIMARY KEY,
    entity_type TEXT NOT NULL,
    entity_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending',
    attempts INTEGER NOT NULL DEFAULT 0,
    next_retry_at TEXT,
    payload TEXT,
    created_at TEXT NOT NULL,
    processed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_outbox_pending ON index_outbox(status, next_retry_at);

-- 17.13 Additional required tables
CREATE TABLE IF NOT EXISTS search_results (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    url TEXT NOT NULL,
    title TEXT,
    snippet TEXT,
    score REAL,
    published_at TEXT,
    evidence_id TEXT,
    FOREIGN KEY(task_id) REFERENCES search_tasks(id)
);
CREATE INDEX IF NOT EXISTS idx_results_task ON search_results(task_id, position);

CREATE TABLE IF NOT EXISTS search_result_sources (
    result_id TEXT NOT NULL,
    source_id TEXT NOT NULL,
    FOREIGN KEY(result_id) REFERENCES search_results(id),
    FOREIGN KEY(source_id) REFERENCES sources(id),
    PRIMARY KEY(result_id, source_id)
);

CREATE TABLE IF NOT EXISTS search_profiles (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    current_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS search_profile_versions (
    id TEXT PRIMARY KEY,
    profile_id TEXT NOT NULL,
    version TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(profile_id) REFERENCES search_profiles(id),
    UNIQUE(profile_id, version)
);

CREATE TABLE IF NOT EXISTS search_budgets (
    id TEXT PRIMARY KEY,
    scope TEXT NOT NULL,            -- task | agent | workspace | provider
    scope_id TEXT NOT NULL,
    max_external_queries INTEGER,
    max_fetch_pages INTEGER,
    max_total_cost_micros INTEGER,
    max_execution_time_ms INTEGER,
    max_retries INTEGER,
    max_query_expansions INTEGER,
    currency TEXT NOT NULL DEFAULT 'USD',
    created_at TEXT NOT NULL,
    UNIQUE(scope, scope_id)
);

CREATE TABLE IF NOT EXISTS provider_usage (
    id TEXT PRIMARY KEY,
    provider_id TEXT NOT NULL,
    operation TEXT NOT NULL,
    calls INTEGER NOT NULL DEFAULT 0,
    input_units INTEGER NOT NULL DEFAULT 0,
    output_units INTEGER NOT NULL DEFAULT 0,
    window_start TEXT NOT NULL,
    window_end TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    UNIQUE(provider_id, operation, window_start)
);

CREATE TABLE IF NOT EXISTS cost_models (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    current_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS cost_model_versions (
    id TEXT PRIMARY KEY,
    cost_model_id TEXT NOT NULL,
    version TEXT NOT NULL,
    spec_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(cost_model_id) REFERENCES cost_models(id),
    UNIQUE(cost_model_id, version)
);

CREATE TABLE IF NOT EXISTS extension_registry (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    runtime TEXT NOT NULL,
    current_version TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS extension_versions (
    id TEXT PRIMARY KEY,
    extension_id TEXT NOT NULL,
    version TEXT NOT NULL,
    manifest_json TEXT NOT NULL,
    created_at TEXT NOT NULL,
    FOREIGN KEY(extension_id) REFERENCES extension_registry(id),
    UNIQUE(extension_id, version)
);

CREATE TABLE IF NOT EXISTS crawler_runs (
    id TEXT PRIMARY KEY,
    crawler_id TEXT NOT NULL,
    url TEXT NOT NULL,
    status TEXT NOT NULL,
    duration_ms INTEGER,
    started_at TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_crawler_runs ON crawler_runs(crawler_id, started_at);

CREATE TABLE IF NOT EXISTS crawler_failures (
    id TEXT PRIMARY KEY,
    crawler_id TEXT NOT NULL,
    url TEXT NOT NULL,
    error_kind TEXT NOT NULL,
    detail TEXT,
    at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_crawler_failures ON crawler_failures(crawler_id, at);

CREATE TABLE IF NOT EXISTS embeddings (
    id TEXT PRIMARY KEY,
    chunk_id TEXT NOT NULL,
    model_id TEXT NOT NULL,
    model_version TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    normalized INTEGER NOT NULL,
    embedding_status TEXT NOT NULL,
    vector_ref TEXT,
    created_at TEXT NOT NULL,
    FOREIGN KEY(chunk_id) REFERENCES document_chunks(id),
    UNIQUE(chunk_id, model_id, model_version)
);

CREATE TABLE IF NOT EXISTS retrieval_cache (
    id TEXT PRIMARY KEY,
    query_fingerprint TEXT NOT NULL,
    index_version INTEGER NOT NULL,
    top_k INTEGER NOT NULL,
    candidates_json TEXT NOT NULL,
    policy_version TEXT NOT NULL,
    created_at TEXT NOT NULL,
    expires_at TEXT NOT NULL,
    UNIQUE(query_fingerprint, index_version, top_k)
);

CREATE TABLE IF NOT EXISTS evaluation_runs (
    id TEXT PRIMARY KEY,
    dataset_id TEXT NOT NULL,
    config_json TEXT NOT NULL,
    started_at TEXT NOT NULL,
    completed_at TEXT
);

CREATE TABLE IF NOT EXISTS evaluation_results (
    id TEXT PRIMARY KEY,
    run_id TEXT NOT NULL,
    question_id TEXT NOT NULL,
    quality_status TEXT NOT NULL,
    local_resolved INTEGER NOT NULL,
    external_calls INTEGER NOT NULL DEFAULT 0,
    cost_micros INTEGER NOT NULL DEFAULT 0,
    detail_json TEXT,
    FOREIGN KEY(run_id) REFERENCES evaluation_runs(id)
);

CREATE TABLE IF NOT EXISTS index_status (
    entity TEXT PRIMARY KEY,        -- 'tantivy' | 'lancedb'
    index_version INTEGER NOT NULL,
    last_synced_at TEXT,
    healthy INTEGER NOT NULL DEFAULT 1,
    detail TEXT
);

CREATE TABLE IF NOT EXISTS audit_events (
    id TEXT PRIMARY KEY,
    actor TEXT NOT NULL,
    action TEXT NOT NULL,
    target TEXT,
    detail_json TEXT,
    at TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS decision_traces (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    intents_json TEXT NOT NULL,
    profile_version TEXT NOT NULL,
    local_candidates INTEGER NOT NULL,
    accepted_candidates INTEGER NOT NULL,
    freshness_status TEXT NOT NULL,
    coverage_status TEXT NOT NULL,
    source_status TEXT NOT NULL,
    selected_path TEXT NOT NULL,
    rejection_reasons_json TEXT NOT NULL,
    estimated_cost_micros INTEGER NOT NULL,
    estimated_currency TEXT NOT NULL,
    actual_cost_micros INTEGER,
    decision_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_traces_task ON decision_traces(task_id);

CREATE TABLE IF NOT EXISTS memory_entries (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    project TEXT,
    agent TEXT,
    sharing TEXT NOT NULL,
    title TEXT NOT NULL,
    content TEXT NOT NULL,
    url TEXT,
    tags_json TEXT NOT NULL DEFAULT '[]',
    embedding_status TEXT NOT NULL DEFAULT 'none',
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_memory_ws ON memory_entries(workspace_id);

CREATE TABLE IF NOT EXISTS singleflight_records (
    id TEXT PRIMARY KEY,
    fingerprint TEXT NOT NULL,
    leader_task_id TEXT NOT NULL,
    follower_task_ids_json TEXT NOT NULL DEFAULT '[]',
    saved_cost_micros INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_singleflight_fp ON singleflight_records(fingerprint);
"#,
}];

/// Applies pending migrations to a connection. Released migrations are
/// immutable; schema changes append new steps.
pub fn migrate(conn: &Connection) -> Result<i64, rusqlite::Error> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let mut applied = 0i64;
    for m in MIGRATIONS {
        if m.version > current {
            conn.execute_batch(m.sql)?;
            conn.execute_batch(&format!("PRAGMA user_version = {};", m.version))?;
            applied += 1;
        }
    }
    let final_version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    let _ = applied;
    Ok(final_version)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_fresh_db_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        let v = migrate(&conn).unwrap();
        assert_eq!(v, 1);
        // Idempotent: second run applies nothing.
        let v2 = migrate(&conn).unwrap();
        assert_eq!(v2, 1);

        // Core tables exist.
        for t in [
            "workspaces",
            "agents",
            "search_tasks",
            "search_decisions",
            "sources",
            "documents",
            "document_versions",
            "document_chunks",
            "search_cache",
            "providers",
            "cost_events",
            "index_outbox",
            "search_results",
            "search_result_sources",
            "search_profiles",
            "search_profile_versions",
            "search_budgets",
            "provider_usage",
            "cost_models",
            "cost_model_versions",
            "extension_registry",
            "extension_versions",
            "crawler_runs",
            "crawler_failures",
            "embeddings",
            "retrieval_cache",
            "evaluation_runs",
            "evaluation_results",
            "index_status",
            "audit_events",
            "decision_traces",
            "memory_entries",
            "singleflight_records",
        ] {
            let count: i64 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_master WHERE type='table' AND name=?1",
                    [t],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(count, 1, "table {t} missing");
        }
    }

    #[test]
    fn cost_events_unique_id_append_only() {
        let conn = Connection::open_in_memory().unwrap();
        migrate(&conn).unwrap();
        conn.execute(
            "INSERT INTO workspaces (id,name,config_version,created_at,updated_at) VALUES ('ws','w',1,'t','t')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO search_tasks (id,workspace_id,agent_id,query,normalized_query,query_fingerprint,profile_id,profile_version,mode,status,started_at,trace_id)
             VALUES ('t1','ws',NULL,'q','q','fp','general','1.0.0','local_first','running','t','tr')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO cost_events (id,task_id,workspace_id,operation,input_units,output_units,estimated_cost_micros,currency,occurred_at)
             VALUES ('c1','t1','ws','web_search',1,0,5000,'USD','t')",
            [],
        ).unwrap();
        // Append-only: PK conflict on rewrite attempt.
        let dup = conn.execute(
            "INSERT INTO cost_events (id,task_id,workspace_id,operation,input_units,output_units,estimated_cost_micros,currency,occurred_at)
             VALUES ('c1','t1','ws','web_search',1,0,5000,'USD','t')",
            [],
        );
        assert!(dup.is_err(), "ledger rows are append-only");
    }
}
