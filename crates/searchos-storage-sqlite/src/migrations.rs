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
pub static MIGRATIONS: &[Migration] = &[
    Migration {
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
    },
    // v2 — feature-completion pass against the source RFC baseline:
    // idempotent request replay, budget reservation/settlement, provider bill
    // reconciliation, staged decision traces, and refresh outbox events.
    Migration {
        version: 2,
        name: "decision_gap_and_governance_completion",
        sql: r#"
-- RFC-014 14.7 / RFC-004 v1.1 14.2: request-level idempotency.
CREATE TABLE IF NOT EXISTS request_idempotency (
    request_id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    status TEXT NOT NULL,               -- running | completed | failed
    response_json TEXT,
    created_at TEXT NOT NULL,
    completed_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_idempotency_ws ON request_idempotency(workspace_id, created_at);

-- RFC-004 v1.1 15 / RFC-006 6.11: reserve -> execute -> settle.
CREATE TABLE IF NOT EXISTS budget_reservations (
    id TEXT PRIMARY KEY,
    task_id TEXT NOT NULL,
    trace_id TEXT,
    estimated_cost_micros INTEGER NOT NULL,
    spent_cost_micros INTEGER NOT NULL DEFAULT 0,
    currency TEXT NOT NULL,
    status TEXT NOT NULL,               -- reserved | settled | released | exceeded
    created_at TEXT NOT NULL,
    settled_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_reservations_task ON budget_reservations(task_id);

-- RFC-006 6.7: estimated cost is never presented as a confirmed bill.
CREATE TABLE IF NOT EXISTS cost_reconciliations (
    id TEXT PRIMARY KEY,
    cost_event_id TEXT NOT NULL,
    actual_cost_micros INTEGER NOT NULL,
    source TEXT NOT NULL,               -- provider_billing | local_measurement | user
    currency TEXT NOT NULL,
    at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS idx_reconciliation_event ON cost_reconciliations(cost_event_id);

-- RFC-004 v1.1 11.1: structured, immutable decision stage traces.
ALTER TABLE decision_traces ADD COLUMN stages_json TEXT NOT NULL DEFAULT '[]';
ALTER TABLE decision_traces ADD COLUMN final_strategy TEXT;
ALTER TABLE decision_traces ADD COLUMN final_outcome TEXT;
"#,
    },
    // v3 - RFC-026/027/028 Visualization & Analytics Layer.
    // Analytics events are an append-only ledger; the read model is the ONLY
    // source the Web Dashboard is allowed to query.
    Migration {
        version: 3,
        name: "analytics_event_store_and_read_model",
        sql: r#"
CREATE TABLE IF NOT EXISTS analytics_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    event_id TEXT NOT NULL UNIQUE,
    event_type TEXT NOT NULL,
    schema_version INTEGER NOT NULL DEFAULT 1,
    occurred_at TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    request_id TEXT,
    execution_id TEXT,
    source_module TEXT NOT NULL,
    correlation_id TEXT,
    payload_json TEXT NOT NULL DEFAULT '{}'
);
CREATE INDEX IF NOT EXISTS idx_analytics_events_type_time
    ON analytics_events(event_type, occurred_at);
CREATE INDEX IF NOT EXISTS idx_analytics_events_workspace
    ON analytics_events(workspace_id, occurred_at);

CREATE TABLE IF NOT EXISTS analytics_read_models (
    model_key TEXT NOT NULL,
    workspace_id TEXT NOT NULL,
    bucket_start TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    aggregation_version TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY(model_key, workspace_id, bucket_start)
);
CREATE INDEX IF NOT EXISTS idx_analytics_read_models_key
    ON analytics_read_models(model_key, bucket_start);
"#,
    },
    // v4 - RFC-029 Local Data Ingestion.
    // Documents gain provenance (web vs local) and a human title; batch /
    // directory ingest jobs become observable and resumable.
    Migration {
        version: 4,
        name: "local_ingestion",
        sql: r#"
-- RFC-029 5.2: origin of the content and a human-readable title.
ALTER TABLE documents ADD COLUMN source_type TEXT NOT NULL DEFAULT 'web';
ALTER TABLE documents ADD COLUMN title TEXT;

-- RFC-029 7: batch / directory ingest jobs.
CREATE TABLE IF NOT EXISTS ingest_jobs (
    id TEXT PRIMARY KEY,
    workspace_id TEXT NOT NULL,
    source_type TEXT NOT NULL,
    state TEXT NOT NULL,
    total_items INTEGER NOT NULL DEFAULT 0,
    ok_items INTEGER NOT NULL DEFAULT 0,
    failed_items INTEGER NOT NULL DEFAULT 0,
    error_detail TEXT,
    created_at TEXT NOT NULL,
    finished_at TEXT
);
CREATE INDEX IF NOT EXISTS idx_ingest_jobs_ws ON ingest_jobs(workspace_id, created_at);

CREATE TABLE IF NOT EXISTS ingest_job_items (
    id TEXT PRIMARY KEY,
    job_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    state TEXT NOT NULL,
    document_id TEXT,
    error_detail TEXT,
    FOREIGN KEY(job_id) REFERENCES ingest_jobs(id)
);
CREATE INDEX IF NOT EXISTS idx_ingest_items_job ON ingest_job_items(job_id);
"#,
    },
    // v5 - Part 30 execution-correctness recovery data.
    // Released v2/v4 SQL remains immutable above; the runner below makes
    // their ADD COLUMN statements idempotent at execution time.
    Migration {
        version: 5,
        name: "execution_correctness_recovery",
        sql: r#"
ALTER TABLE request_idempotency ADD COLUMN claim_owner_task_id TEXT;
ALTER TABLE request_idempotency ADD COLUMN claimed_at TEXT;

ALTER TABLE cost_events ADD COLUMN status TEXT NOT NULL DEFAULT 'succeeded';
ALTER TABLE cost_events ADD COLUMN error_code TEXT;
ALTER TABLE budget_reservations ADD COLUMN released_at TEXT;

CREATE INDEX IF NOT EXISTS idx_documents_workspace ON documents(workspace_id);
CREATE INDEX IF NOT EXISTS idx_chunks_document ON document_chunks(document_id);
"#,
    },
];

/// Applies pending migrations to a connection. Each version is atomic: SQL
/// and `user_version` commit together, and a failed version rolls back to the
/// previous usable schema (Part 30 INV-30-09).
pub fn migrate(conn: &Connection) -> Result<i64, rusqlite::Error> {
    migrate_with_failure(conn, None)
}

/// Test hook: fail after the Nth `ALTER TABLE` of one migration version.
/// The injected failure is inside the same transaction, so the test proves
/// rollback rather than merely reporting an error.
#[cfg(test)]
fn migrate_with_failure(
    conn: &Connection,
    fail_after_alter: Option<(i64, usize)>,
) -> Result<i64, rusqlite::Error> {
    apply_migrations(conn, fail_after_alter)
}

#[cfg(not(test))]
fn migrate_with_failure(
    conn: &Connection,
    _fail_after_alter: Option<(i64, usize)>,
) -> Result<i64, rusqlite::Error> {
    apply_migrations(conn, None)
}

fn apply_migrations(
    conn: &Connection,
    fail_after_alter: Option<(i64, usize)>,
) -> Result<i64, rusqlite::Error> {
    let current: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
    for m in MIGRATIONS {
        if m.version <= current {
            continue;
        }
        conn.execute_batch("BEGIN IMMEDIATE;")?;
        let result = (|| -> Result<(), rusqlite::Error> {
            apply_sql_idempotent(
                conn,
                m.sql,
                fail_after_alter
                    .filter(|(version, _)| *version == m.version)
                    .map(|(_, n)| n),
            )?;
            conn.execute_batch(&format!("PRAGMA user_version = {};", m.version))?;
            Ok(())
        })();
        match result {
            Ok(()) => conn.execute_batch("COMMIT;")?,
            Err(e) => {
                let _ = conn.execute_batch("ROLLBACK;");
                return Err(e);
            }
        }
    }
    conn.query_row("PRAGMA user_version", [], |r| r.get(0))
}

/// Executes one migration statement-by-statement, skipping `ADD COLUMN`
/// statements whose column already exists. This keeps released v2/v4 SQL
/// immutable while making a retry after a rolled-back migration safe.
fn apply_sql_idempotent(
    conn: &Connection,
    sql: &str,
    fail_after_alter: Option<usize>,
) -> Result<(), rusqlite::Error> {
    let mut alters = 0usize;
    for raw in sql.split(';') {
        let cleaned = raw
            .lines()
            .filter(|line| !line.trim_start().starts_with("--"))
            .collect::<Vec<_>>()
            .join("\n");
        let statement = cleaned.trim();
        if statement.is_empty() {
            continue;
        }
        let is_add_column = add_column_target(statement).is_some();
        if let Some((table, column)) = add_column_target(statement) {
            if column_exists(conn, table, column)? {
                continue;
            }
        }
        conn.execute_batch(&format!("{statement};"))?;
        if is_add_column {
            alters += 1;
            if fail_after_alter == Some(alters) {
                return Err(injected_migration_failure(conn));
            }
        }
    }
    Ok(())
}

fn injected_migration_failure(conn: &Connection) -> rusqlite::Error {
    conn.execute_batch("SELECT * FROM __searchos_injected_migration_failure__;")
        .unwrap_err()
}

fn add_column_target(statement: &str) -> Option<(&str, &str)> {
    let tokens: Vec<&str> = statement.split_whitespace().collect();
    if tokens.len() < 6
        || !tokens[0].eq_ignore_ascii_case("ALTER")
        || !tokens[1].eq_ignore_ascii_case("TABLE")
        || !tokens[3].eq_ignore_ascii_case("ADD")
        || !tokens[4].eq_ignore_ascii_case("COLUMN")
    {
        return None;
    }
    Some((
        tokens[2].trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']'),
        tokens[5].trim_matches(|c| c == '"' || c == '`' || c == '[' || c == ']'),
    ))
}

fn column_exists(conn: &Connection, table: &str, column: &str) -> Result<bool, rusqlite::Error> {
    // Identifiers come only from the static migration SQL above.
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = conn.prepare(&sql)?;
    let names = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for name in names {
        if name?.eq_ignore_ascii_case(column) {
            return Ok(true);
        }
    }
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migrates_fresh_db_and_is_idempotent() {
        let conn = Connection::open_in_memory().unwrap();
        let expected = MIGRATIONS.last().unwrap().version;
        let v = migrate(&conn).unwrap();
        assert_eq!(v, expected);
        // Idempotent: second run applies nothing.
        let v2 = migrate(&conn).unwrap();
        assert_eq!(v2, expected);

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
            "request_idempotency",
            "budget_reservations",
            "cost_reconciliations",
            "analytics_events",
            "analytics_read_models",
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

    /// Part 30 §30.11: injecting a failure at the second v2 ALTER must leave
    /// v1 usable and user_version unchanged; a retry must then succeed.
    #[test]
    fn failed_migration_leaves_previous_version_usable() {
        let conn = Connection::open_in_memory().unwrap();
        let err = migrate_with_failure(&conn, Some((2, 1))).unwrap_err();
        assert!(!err.to_string().is_empty());
        let version: i64 = conn
            .query_row("PRAGMA user_version", [], |r| r.get(0))
            .unwrap();
        assert_eq!(version, 1, "failed v2 rolled back to v1");
        conn.execute(
            "INSERT INTO workspaces (id,name,config_version,created_at,updated_at) VALUES ('ws','w',1,'t','t')",
            [],
        )
        .unwrap();
        assert!(!column_exists(&conn, "decision_traces", "stages_json").unwrap());
        let expected = MIGRATIONS.last().unwrap().version;
        assert_eq!(migrate(&conn).unwrap(), expected, "retry is idempotent");
        assert_eq!(migrate(&conn).unwrap(), expected, "second retry is a no-op");
    }

    #[test]
    fn migration_is_idempotent_on_retry() {
        let conn = Connection::open_in_memory().unwrap();
        let expected = MIGRATIONS.last().unwrap().version;
        assert_eq!(migrate(&conn).unwrap(), expected);
        // Simulate a stale user_version with columns already present; the
        // immutable released ALTER statements must be skipped, not duplicated.
        conn.execute_batch("PRAGMA user_version = 4;").unwrap();
        assert_eq!(migrate(&conn).unwrap(), expected);
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
