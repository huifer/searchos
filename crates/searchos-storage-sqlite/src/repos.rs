//! Repositories over the Part-17 schema.

use rusqlite::{params, Connection, OptionalExtension};
use searchos_domain::cost::{
    CostAttemptStatus, CostEvent, CostOperation, CostQuery, CostSummary, Money,
};
use searchos_domain::decision::DecisionTrace;
use searchos_domain::document::{Document, DocumentVersion};
use searchos_domain::ids::{DocumentId, DocumentVersionId, SourceId, WorkspaceId};
use searchos_domain::retrieval::{IndexedDocument, RetrievalCandidate, SearchCacheKey};
use searchos_domain::SearchOsError;

use crate::store::SqliteStore;

fn now() -> String {
    chrono::Utc::now().to_rfc3339()
}

fn e(sql_err: rusqlite::Error) -> SearchOsError {
    SearchOsError::Storage(sql_err.to_string())
}

/// Ensures a workspace row exists; returns its id.
pub fn ensure_workspace(
    store: &SqliteStore,
    id: &WorkspaceId,
    name: &str,
) -> Result<(), SearchOsError> {
    store.with_conn(|c| {
        c.execute(
            "INSERT OR IGNORE INTO workspaces (id, name, config_version, created_at, updated_at) VALUES (?1, ?2, 1, ?3, ?3)",
            params![id.as_str(), name, now()],
        )
        .map_err(e)
        .map(|_| ())
    })
}

/// Ensures an agent row exists.
pub fn ensure_agent(
    store: &SqliteStore,
    id: &searchos_domain::ids::AgentId,
    workspace_id: &WorkspaceId,
    name: &str,
    agent_type: Option<&str>,
) -> Result<(), SearchOsError> {
    ensure_workspace(store, workspace_id, workspace_id.as_str())?;
    store.with_conn(|c| {
        c.execute(
            "INSERT OR IGNORE INTO agents (id, workspace_id, name, agent_type, created_at) VALUES (?1, ?2, ?3, ?4, ?5)",
            params![id.as_str(), workspace_id.as_str(), name, agent_type, now()],
        )
        .map_err(e)
        .map(|_| ())
    })
}

/// Search task persistence.
pub struct TaskRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> TaskRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Inserts a task row (RFC-004 trace: every search task recorded).
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &self,
        task: &searchos_domain::ids::TaskId,
        workspace_id: &WorkspaceId,
        agent_id: Option<&searchos_domain::ids::AgentId>,
        query: &str,
        normalized_query: &str,
        fingerprint: &str,
        profile_id: &str,
        profile_version: &str,
        mode: &str,
        trace_id: &searchos_domain::ids::TraceId,
    ) -> Result<(), SearchOsError> {
        ensure_workspace(self.store, workspace_id, workspace_id.as_str())?;
        if let Some(agent) = agent_id {
            ensure_agent(self.store, agent, workspace_id, agent.as_str(), None)?;
        }
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO search_tasks (id, workspace_id, agent_id, query, normalized_query, query_fingerprint, profile_id, profile_version, mode, status, started_at, trace_id)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'running', ?10, ?11)",
                params![
                    task.as_str(),
                    workspace_id.as_str(),
                    agent_id.map(|a| a.as_str()),
                    query,
                    normalized_query,
                    fingerprint,
                    profile_id,
                    profile_version,
                    mode,
                    now(),
                    trace_id.as_str(),
                ],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Marks a task completed/failed.
    pub fn finish(
        &self,
        task: &searchos_domain::ids::TaskId,
        status: &str,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE search_tasks SET status = ?2, completed_at = ?3 WHERE id = ?1",
                params![task.as_str(), status, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Finds a recent task with the same fingerprint (for singleflight
    /// bookkeeping and cache priming hints).
    pub fn find_by_fingerprint(&self, fingerprint: &str) -> Result<Option<String>, SearchOsError> {
        self.store
            .with_conn(|c| {
                c.query_row(
                    "SELECT id FROM search_tasks WHERE query_fingerprint = ?1 ORDER BY started_at DESC LIMIT 1",
                    params![fingerprint],
                    |r| r.get::<_, String>(0),
                )
                .optional()
                .map_err(e)
            })
    }
}

/// Decision + trace persistence.
pub struct DecisionRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> DecisionRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Records a decision row (RFC-004 4.3).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_decision(
        &self,
        id: &str,
        task_id: &searchos_domain::ids::TaskId,
        decision_type: &str,
        reason_code: &str,
        reason_detail: Option<&str>,
        quality_status: Option<&str>,
        estimated_cost_micros: Option<i64>,
        actual_cost_micros: Option<i64>,
        policy_version: &str,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO search_decisions (id, task_id, decision_type, reason_code, reason_detail, quality_status, estimated_cost_micros, actual_cost_micros, policy_version, created_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                params![id, task_id.as_str(), decision_type, reason_code, reason_detail, quality_status, estimated_cost_micros, actual_cost_micros, policy_version, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Records a full decision trace (RFC-004 4.5, 覆盖率目标 100%).
    #[allow(clippy::too_many_arguments)]
    pub fn insert_trace(&self, t: &DecisionTrace) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO decision_traces (id, task_id, intents_json, profile_version, local_candidates, accepted_candidates, freshness_status, coverage_status, source_status, selected_path, rejection_reasons_json, estimated_cost_micros, estimated_currency, actual_cost_micros, decision_at, stages_json, final_strategy, final_outcome)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                params![
                    t.trace_id.as_str(),
                    t.task_id.as_str(),
                    serde_json::to_string(&t.intents).map_err(|er| SearchOsError::Internal(er.to_string()))?,
                    t.profile_version,
                    t.local_candidates as i64,
                    t.accepted_candidates as i64,
                    t.freshness_status,
                    t.coverage_status,
                    t.source_status,
                    t.selected_path,
                    serde_json::to_string(&t.rejection_reasons).map_err(|er| SearchOsError::Internal(er.to_string()))?,
                    t.estimated_cost.amount_micros,
                    t.estimated_cost.currency,
                    t.actual_cost.as_ref().map(|m| m.amount_micros),
                    t.decision_at.to_rfc3339(),
                    serde_json::to_string(&t.stages)
                        .map_err(|er| SearchOsError::Internal(er.to_string()))?,
                    t.final_strategy,
                    t.final_outcome,
                ],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Recent traces for the console (`GET /traces`, RFC-015 15.1).
    pub fn list_traces(&self, limit: u32) -> Result<Vec<DecisionTrace>, SearchOsError> {
        self.list_traces_scoped(limit, None)
    }

    /// Recent traces restricted at the SQL level to one workspace (plus the
    /// shared `*` namespace), INV-30-10.
    pub fn list_traces_scoped(
        &self,
        limit: u32,
        workspace: Option<&str>,
    ) -> Result<Vec<DecisionTrace>, SearchOsError> {
        let ids: Vec<String> = self.store.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT dt.id FROM decision_traces dt
                     LEFT JOIN search_tasks st ON st.id = dt.task_id
                     WHERE (?1 IS NULL OR COALESCE(st.workspace_id, 'default') = ?1 OR st.workspace_id = '*')
                     ORDER BY dt.decision_at DESC LIMIT ?2",
                )
                .map_err(e)?;
            let rows = stmt
                .query_map(params![workspace, limit.clamp(1, 100)], |r| {
                    r.get::<_, String>(0)
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })?;
        let mut out = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(trace) = self.get_trace_scoped(&id, workspace)? {
                out.push(trace);
            }
        }
        Ok(out)
    }

    /// Fetches a trace (GET /decisions/:trace_id).
    pub fn get_trace(&self, trace_id: &str) -> Result<Option<DecisionTrace>, SearchOsError> {
        self.get_trace_scoped(trace_id, None)
    }

    /// Fetches a trace with a workspace predicate in the query itself.
    pub fn get_trace_scoped(
        &self,
        trace_id: &str,
        workspace: Option<&str>,
    ) -> Result<Option<DecisionTrace>, SearchOsError> {
        self.store.with_conn(|c| {
            let row = c
                .query_row(
                    "SELECT dt.id, dt.task_id, dt.intents_json, dt.profile_version, dt.local_candidates, dt.accepted_candidates, dt.freshness_status, dt.coverage_status, dt.source_status, dt.selected_path, dt.rejection_reasons_json, dt.estimated_cost_micros, dt.estimated_currency, dt.actual_cost_micros, dt.decision_at, dt.stages_json, dt.final_strategy, dt.final_outcome
                     FROM decision_traces dt
                     LEFT JOIN search_tasks st ON st.id = dt.task_id
                     WHERE dt.id = ?1
                       AND (?2 IS NULL OR COALESCE(st.workspace_id, 'default') = ?2 OR st.workspace_id = '*')",
                    params![trace_id, workspace],
                    |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, String>(3)?,
                            r.get::<_, i64>(4)?,
                            r.get::<_, i64>(5)?,
                            r.get::<_, String>(6)?,
                            r.get::<_, String>(7)?,
                            r.get::<_, String>(8)?,
                            r.get::<_, String>(9)?,
                            r.get::<_, String>(10)?,
                            r.get::<_, i64>(11)?,
                            r.get::<_, String>(12)?,
                            r.get::<_, Option<i64>>(13)?,
                            r.get::<_, String>(14)?,
                            r.get::<_, String>(15)?,
                            r.get::<_, Option<String>>(16)?,
                            r.get::<_, Option<String>>(17)?,
                        ))
                    },
                )
                .optional()
                .map_err(e)?;
            let Some(row) = row else { return Ok(None) };
            let intents: Vec<searchos_domain::request::SearchIntent> =
                serde_json::from_str(&row.2).map_err(|er| SearchOsError::Internal(er.to_string()))?;
            let reasons: Vec<String> =
                serde_json::from_str(&row.10).map_err(|er| SearchOsError::Internal(er.to_string()))?;
            let currency = row.12.clone();
            let stages: Vec<searchos_domain::decision::DecisionStageTrace> =
                serde_json::from_str(&row.15)
                    .map_err(|er| SearchOsError::Internal(er.to_string()))?;
            Ok(Some(DecisionTrace {
                trace_id: searchos_domain::ids::TraceId::new(row.0),
                task_id: searchos_domain::ids::TaskId::new(row.1),
                intents,
                profile_version: row.3,
                local_candidates: row.4 as usize,
                accepted_candidates: row.5 as usize,
                freshness_status: row.6,
                coverage_status: row.7,
                source_status: row.8,
                selected_path: row.9,
                rejection_reasons: reasons,
                estimated_cost: Money { amount_micros: row.11, currency: currency.clone() },
                actual_cost: row.13.map(|m| Money { amount_micros: m, currency }),
                decision_at: chrono::DateTime::parse_from_rfc3339(&row.14)
                    .map_err(|er| SearchOsError::Internal(er.to_string()))?
                    .into(),
                stages,
                final_strategy: row.16,
                final_outcome: row.17,
            }))
        })
    }
}

/// Source + document + version + chunk persistence with the transactional
/// outbox (RFC-006 6.7, ADR-011).
pub struct DocumentRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> DocumentRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Upserts a source keyed by canonical URL.
    pub fn upsert_source(
        &self,
        source_id: &SourceId,
        canonical_url: &str,
        domain: &str,
        source_type: &str,
        reliability: Option<&str>,
    ) -> Result<SourceId, SearchOsError> {
        self.store.with_conn(|c| {
            let existing: Option<String> = c
                .query_row("SELECT id FROM sources WHERE canonical_url = ?1", params![canonical_url], |r| r.get(0))
                .optional()
                .map_err(e)?;
            if let Some(id) = existing {
                Ok(SourceId::new(id))
            } else {
                c.execute(
                    "INSERT INTO sources (id, canonical_url, domain, source_type, reliability_level, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?6)",
                    params![source_id.as_str(), canonical_url, domain, source_type, reliability, now()],
                )
                .map_err(e)?;
                Ok(source_id.clone())
            }
        })
    }

    /// Saves a document with a new version + chunks + outbox events in ONE
    /// transaction (RFC-006 6.7): version + chunk metadata + outbox, commit.
    /// Content-hash unchanged ⇒ metadata update only (RFC-006 6.4).
    pub fn save_versioned(
        &self,
        document: &Document,
        version: &DocumentVersion,
        chunks: &[searchos_domain::document::DocumentChunk],
        index_payloads: &[IndexedDocument],
        workspace_id: &searchos_domain::ids::WorkspaceId,
    ) -> Result<searchos_document::VersionDiff, SearchOsError> {
        // Pre-compute diff outside the transaction (needs current version).
        let current_hash: Option<String> =
            self.store
                .with_conn(|c| match &document.current_version_id {
                    Some(vid) => c
                        .query_row(
                            "SELECT content_hash FROM document_versions WHERE id = ?1",
                            params![vid.as_str()],
                            |r| r.get::<_, String>(0),
                        )
                        .optional()
                        .map_err(e),
                    None => Ok(None),
                })?;
        let diff =
            searchos_document::decide_version_diff(current_hash.as_deref(), &version.content_hash);

        self.store.with_conn(|c| {
            let tx = c.unchecked_transaction().map_err(e)?;

            // Source. RFC-029: local ingest keeps source_type/domain honest.
            let source_domain = source_domain(&document.canonical_url, &document.source_type);
            tx.execute(
                "INSERT OR IGNORE INTO sources (id, canonical_url, domain, source_type, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?5)",
                params![document.source_id.as_str(), document.canonical_url, source_domain, document.source_type.as_str(), now()],
            )
            .map_err(e)?;

            // Document row.
            tx.execute(
                "INSERT INTO documents (id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, workspace_id, source_type, title)
                 VALUES (?1,?2,?3,NULL,?4,?5,?6,?7,?7,?8,?9,?10)
                 ON CONFLICT(id) DO UPDATE SET updated_at = ?7, source_type = ?9, title = ?10, workspace_id = ?8",
                params![document.id.as_str(), document.source_id.as_str(), document.canonical_url, document.language, document.content_type, "normalized", now(), workspace_id.as_str(), document.source_type.as_str(), document.title],
            )
            .map_err(e)?;

            match diff {
                searchos_document::VersionDiff::Unchanged => {
                    // Update fetch metadata only; no new version / chunks /
                    // index events (RFC-006 6.4).
                    tx.execute(
                        "UPDATE documents SET updated_at = ?2 WHERE id = ?1",
                        params![document.id.as_str(), now()],
                    )
                    .map_err(e)?;
                    tx.commit().map_err(e)?;
                    return Ok(diff);
                }
                searchos_document::VersionDiff::NewVersion => {}
            }

            // New version.
            tx.execute(
                "INSERT OR IGNORE INTO document_versions (id, document_id, content_hash, raw_content_ref, markdown_ref, published_at, fetched_at, extractor_version, crawler_version, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    version.id.as_str(),
                    document.id.as_str(),
                    version.content_hash,
                    version.raw_content_ref,
                    version.markdown_ref,
                    version.published_at.map(|t| t.to_rfc3339()),
                    version.fetched_at.to_rfc3339(),
                    version.extractor_version,
                    version.crawler_version,
                    now(),
                ],
            )
            .map_err(e)?;

            // Chunks.
            for ch in chunks {
                tx.execute(
                    "INSERT OR IGNORE INTO document_chunks (id, version_id, document_id, chunk_hash, content_ref, token_count, chunk_index, created_at)
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
                    params![ch.id.as_str(), version.id.as_str(), document.id.as_str(), ch.chunk_hash, ch.content_ref, ch.token_count, ch.chunk_index, now()],
                )
                .map_err(e)?;
            }

            // Outbox events for index sync (per chunk upsert).
            for p in index_payloads {
                tx.execute(
                    "INSERT INTO index_outbox (id, entity_type, entity_id, operation, status, payload, created_at)
                     VALUES (?1,'document_chunk',?2,'upsert','pending',?3,?4)",
                    params![searchos_domain::new_id("obx"), p.chunk_id.as_str(), serde_json::to_string(p).map_err(|er| SearchOsError::Internal(er.to_string()))?, now()],
                )
                .map_err(e)?;
            }

            // Move current-version pointer.
            tx.execute(
                "UPDATE documents SET current_version_id = ?2, status = 'normalized', updated_at = ?3 WHERE id = ?1",
                params![document.id.as_str(), version.id.as_str(), now()],
            )
            .map_err(e)?;

            tx.commit().map_err(e)?;
            Ok(diff)
        })
    }

    /// Domain DocumentRepository::get (RFC-002 2.4).
    pub fn get(&self, id: &DocumentId) -> Result<Option<Document>, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row(
                "SELECT id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, source_type, title, workspace_id FROM documents WHERE id = ?1",
                params![id.as_str()],
                |r| {
                    Ok(Document {
                        id: DocumentId::new(r.get::<_, String>(0)?),
                        source_id: SourceId::new(r.get::<_, String>(1)?),
                        canonical_url: r.get(2)?,
                        current_version_id: r.get::<_, Option<String>>(3)?.map(DocumentVersionId::new),
                        language: r.get(4)?,
                        content_type: r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "text/html".into()),
                        source_type: r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "web".into()),
                        title: r.get(10)?,
                        workspace_id: r.get::<_, Option<String>>(11)?.unwrap_or_else(|| "default".into()),
                        created_at: parse_time(&r.get::<_, String>(7)?),
                        updated_at: parse_time(&r.get::<_, String>(8)?),
                    })
                },
            )
            .optional()
            .map_err(e)
        })
    }

    /// Finds the document for a canonical URL.
    pub fn find_by_url(&self, canonical_url: &str) -> Result<Option<Document>, SearchOsError> {
        let id: Option<String> = self.store.with_conn(|c| {
            c.query_row(
                "SELECT id FROM documents WHERE canonical_url = ?1",
                params![canonical_url],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(e)
        })?;
        match id {
            Some(id) => self.get(&DocumentId::new(id)),
            None => Ok(None),
        }
    }

    /// Current version of a document.
    pub fn get_current_version(
        &self,
        id: &DocumentId,
    ) -> Result<Option<DocumentVersion>, SearchOsError> {
        let Some(doc) = self.get(id)? else {
            return Ok(None);
        };
        let Some(vid) = doc.current_version_id else {
            return Ok(None);
        };
        self.store.with_conn(|c| {
            c.query_row(
                "SELECT id, document_id, content_hash, raw_content_ref, markdown_ref, published_at, fetched_at, extractor_version, crawler_version FROM document_versions WHERE id = ?1",
                params![vid.as_str()],
                |r| {
                    Ok(DocumentVersion {
                        id: DocumentVersionId::new(r.get::<_, String>(0)?),
                        document_id: DocumentId::new(r.get::<_, String>(1)?),
                        content_hash: r.get(2)?,
                        raw_content_ref: r.get(3)?,
                        markdown_ref: r.get(4)?,
                        published_at: r.get::<_, Option<String>>(5)?.map(|s| parse_time(&s)),
                        fetched_at: parse_time(&r.get::<_, String>(6)?),
                        extractor_version: r.get(7)?,
                        crawler_version: r.get(8)?,
                    })
                },
            )
            .optional()
            .map_err(e)
        })
    }

    /// Lists documents (for `document list`).
    pub fn list(&self, limit: u32) -> Result<Vec<Document>, SearchOsError> {
        self.list_filtered(limit, None, None)
    }

    /// Lists documents filtered by source type and/or workspace (RFC-029 6.2).
    /// `source_type` accepts `"web"` or a local kind (`"local_text"` /
    /// `"local_file"` / `"local_directory"`).
    pub fn list_filtered(
        &self,
        limit: u32,
        source_type: Option<&str>,
        workspace: Option<&str>,
    ) -> Result<Vec<Document>, SearchOsError> {
        self.store.with_conn(|c| {
            let local_filter = source_type
                .map(|s| s.eq_ignore_ascii_case("local"))
                .unwrap_or(false);
            let effective_source = if local_filter { None } else { source_type };
            let sql = match (effective_source, workspace) {
                (Some(_), Some(_)) => "SELECT id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, source_type, title, workspace_id FROM documents WHERE source_type = ?1 AND workspace_id = ?2 ORDER BY updated_at DESC LIMIT ?3",
                (Some(_), None) => "SELECT id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, source_type, title, workspace_id FROM documents WHERE source_type = ?1 ORDER BY updated_at DESC LIMIT ?2",
                (None, Some(_)) => "SELECT id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, source_type, title, workspace_id FROM documents WHERE workspace_id = ?1 ORDER BY updated_at DESC LIMIT ?2",
                (None, None) => "SELECT id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, source_type, title, workspace_id FROM documents ORDER BY updated_at DESC LIMIT ?1",
            };
            let map_row = |r: &rusqlite::Row| -> rusqlite::Result<Document> {
                Ok(Document {
                    id: DocumentId::new(r.get::<_, String>(0)?),
                    source_id: SourceId::new(r.get::<_, String>(1)?),
                    canonical_url: r.get(2)?,
                    current_version_id: r.get::<_, Option<String>>(3)?.map(DocumentVersionId::new),
                    language: r.get(4)?,
                    content_type: r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "text/html".into()),
                    source_type: r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "web".into()),
                    title: r.get(10)?,
                    workspace_id: r.get::<_, Option<String>>(11)?.unwrap_or_else(|| "default".into()),
                    created_at: parse_time(&r.get::<_, String>(7)?),
                    updated_at: parse_time(&r.get::<_, String>(8)?),
                })
            };
            let mut stmt = c.prepare(sql).map_err(e)?;
            let rows = match (effective_source, workspace) {
                (Some(st), Some(ws)) => stmt.query_map(params![st, ws, limit], map_row).map_err(e)?,
                (Some(st), None) => stmt.query_map(params![st, limit], map_row).map_err(e)?,
                (None, Some(ws)) => stmt.query_map(params![ws, limit], map_row).map_err(e)?,
                (None, None) => stmt.query_map(params![limit], map_row).map_err(e)?,
            };
            let mut docs = rows.collect::<Result<Vec<_>, _>>().map_err(e)?;
            if local_filter {
                docs.retain(|d| d.source_type.starts_with("local_"));
            }
            Ok(docs)
        })
    }

    /// Searches live documents by URL or title text.
    ///
    /// Escapes SQL LIKE wildcards in the caller query and never loads more
    /// than the requested page, so this is safe for large stores. Deleted
    /// documents are excluded.
    pub fn search(
        &self,
        query: &str,
        limit: u32,
        workspace: Option<&str>,
    ) -> Result<Vec<Document>, SearchOsError> {
        let trimmed = query.trim();
        let pattern = format!(
            "%{}%",
            trimmed
                .to_lowercase()
                .replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        let limit = limit.clamp(1, 100);
        let docs = self.store.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, source_id, canonical_url, current_version_id, language, content_type, status, created_at, updated_at, source_type, title, workspace_id
                     FROM documents
                     WHERE status != 'deleted'
                       AND (?3 IS NULL OR workspace_id = ?3 OR workspace_id = '*')
                       AND (lower(coalesce(canonical_url, '')) LIKE ?1 ESCAPE '\\'
                            OR lower(coalesce(title, '')) LIKE ?1 ESCAPE '\\')
                     ORDER BY updated_at DESC
                     LIMIT ?2",
                )
                .map_err(e)?;
            let rows = stmt
                .query_map(params![pattern, limit, workspace], |r| {
                    Ok(Document {
                        id: DocumentId::new(r.get::<_, String>(0)?),
                        source_id: SourceId::new(r.get::<_, String>(1)?),
                        canonical_url: r.get(2)?,
                        current_version_id: r.get::<_, Option<String>>(3)?.map(DocumentVersionId::new),
                        language: r.get(4)?,
                        content_type: r.get::<_, Option<String>>(5)?.unwrap_or_else(|| "text/html".into()),
                        source_type: r.get::<_, Option<String>>(9)?.unwrap_or_else(|| "web".into()),
                        title: r.get(10)?,
                        workspace_id: r.get::<_, Option<String>>(11)?.unwrap_or_else(|| "default".into()),
                        created_at: parse_time(&r.get::<_, String>(7)?),
                        updated_at: parse_time(&r.get::<_, String>(8)?),
                    })
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })?;
        Ok(docs)
    }

    /// Returns the chunk rows belonging to one version, in chunk order.
    pub fn list_chunks(
        &self,
        version_id: &searchos_domain::ids::DocumentVersionId,
    ) -> Result<Vec<searchos_domain::document::DocumentChunk>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT id, version_id, document_id, chunk_hash, content_ref, token_count, chunk_index, created_at
                     FROM document_chunks WHERE version_id = ?1 ORDER BY chunk_index ASC",
                )
                .map_err(e)?;
            let rows = stmt
                .query_map(params![version_id.as_str()], |r| {
                    Ok(searchos_domain::document::DocumentChunk {
                        id: searchos_domain::ids::ChunkId::new(r.get::<_, String>(0)?),
                        version_id: DocumentVersionId::new(r.get::<_, String>(1)?),
                        document_id: DocumentId::new(r.get::<_, String>(2)?),
                        chunk_hash: r.get(3)?,
                        content_ref: r.get(4)?,
                        text: String::new(),
                        token_count: r.get(5)?,
                        chunk_index: r.get(6)?,
                        created_at: parse_time(&r.get::<_, String>(7)?),
                    })
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }

    /// Soft-deletes a document + removes chunks + enqueues index deletes
    /// (RFC-006 6.7 deletion flow).
    pub fn delete(&self, id: &DocumentId) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            let tx = c.unchecked_transaction().map_err(e)?;
            // Collect chunk ids for index deletion events.
            let chunk_ids: Vec<String> = {
                let mut stmt = tx.prepare("SELECT id FROM document_chunks WHERE document_id = ?1").map_err(e)?;
                let rows = stmt.query_map(params![id.as_str()], |r| r.get::<_, String>(0)).map_err(e)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(e)?
            };
            let document_id = id.as_str().to_string();
            for cid in &chunk_ids {
                // The identifier space is explicit: `entity_id` is a chunk id,
                // not a document id. The outbox consumer must delete by chunk.
                let payload = serde_json::json!({
                    "identifier_space": "chunk_id",
                    "chunk_ids": [cid],
                    "document_id": document_id,
                })
                .to_string();
                tx.execute(
                    "INSERT INTO index_outbox (id, entity_type, entity_id, operation, status, payload, created_at) VALUES (?1,'document_chunk',?2,'delete','pending',?3,?4)",
                    params![searchos_domain::new_id("obx"), cid, payload, now()],
                )
                .map_err(e)?;
            }
            tx.execute("DELETE FROM document_chunks WHERE document_id = ?1", params![id.as_str()]).map_err(e)?;
            tx.execute("UPDATE documents SET status = 'deleted', current_version_id = NULL, updated_at = ?2 WHERE id = ?1", params![id.as_str(), now()]).map_err(e)?;
            tx.commit().map_err(e)
        })
    }
}

/// Cost ledger repository (append-only, RFC-008 8.7).
pub struct CostRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> CostRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Appends one event; history is never rewritten (adjustments are new
    /// events — see searchos_cost::ledger::adjustment_event_for).
    pub fn append(&self, ev: &CostEvent) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO cost_events (id, task_id, workspace_id, agent_id, operation, provider_id, input_units, output_units, estimated_cost_micros, actual_cost_micros, currency, duration_ms, occurred_at, trace_id, shared, pricing_model_version, status, error_code)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18)",
                params![
                    ev.id.as_str(),
                    ev.task_id.as_str(),
                    ev.workspace_id.as_str(),
                    ev.agent_id.as_ref().map(|a| a.as_str()),
                    ev.operation.as_str(),
                    ev.provider_id,
                    ev.input_units as i64,
                    ev.output_units as i64,
                    ev.estimated_cost.amount_micros,
                    ev.actual_cost.as_ref().map(|m| m.amount_micros),
                    ev.estimated_cost.currency,
                    ev.duration_ms as i64,
                    ev.occurred_at.to_rfc3339(),
                    ev.trace_id.as_ref().map(|t| t.as_str()),
                    ev.shared as i64,
                    ev.pricing_model_version,
                    ev.status.as_str(),
                    ev.error_code,
                ],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Loads events matching a CostQuery (filtering via searchos_cost).
    pub fn query(&self, q: &CostQuery) -> Result<Vec<CostEvent>, SearchOsError> {
        let mut rows = self.store.with_conn(|c| {
            let mut stmt = c
                .prepare("SELECT id, task_id, workspace_id, agent_id, operation, provider_id, input_units, output_units, estimated_cost_micros, actual_cost_micros, currency, duration_ms, occurred_at, trace_id, shared, pricing_model_version, status, error_code FROM cost_events ORDER BY occurred_at ASC")
                .map_err(e)?;
            let rows = stmt
                .query_map([], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?,
                        r.get::<_, String>(4)?,
                        r.get::<_, Option<String>>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(7)?,
                        r.get::<_, i64>(8)?,
                        r.get::<_, Option<i64>>(9)?,
                        r.get::<_, String>(10)?,
                        r.get::<_, i64>(11)?,
                        r.get::<_, String>(12)?,
                        r.get::<_, Option<String>>(13)?,
                        r.get::<_, i64>(14)?,
                        r.get::<_, String>(15)?,
                        r.get::<_, String>(16)?,
                        r.get::<_, Option<String>>(17)?,
                    ))
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })?;
        let events: Vec<CostEvent> = rows
            .drain(..)
            .map(|r| CostEvent {
                id: searchos_domain::ids::CostEventId::new(r.0),
                task_id: searchos_domain::ids::TaskId::new(r.1),
                workspace_id: WorkspaceId::new(r.2),
                agent_id: r.3.map(searchos_domain::ids::AgentId::new),
                operation: CostOperation::parse(&r.4).unwrap_or(CostOperation::LocalCompute),
                provider_id: r.5,
                input_units: r.6 as u64,
                output_units: r.7 as u64,
                estimated_cost: Money {
                    amount_micros: r.8,
                    currency: r.10.clone(),
                },
                actual_cost: r.9.map(|m| Money {
                    amount_micros: m,
                    currency: r.10.clone(),
                }),
                duration_ms: r.11 as u64,
                occurred_at: chrono::DateTime::parse_from_rfc3339(&r.12)
                    .unwrap_or_default()
                    .into(),
                trace_id: r.13.map(searchos_domain::ids::TraceId::new),
                shared: r.14 != 0,
                pricing_model_version: r.15,
                status: CostAttemptStatus::parse(&r.16),
                error_code: r.17,
            })
            .collect();
        let refs: Vec<&CostEvent> = events.iter().collect();
        let filtered = searchos_cost::filter_events(&refs, q);
        Ok(filtered.into_iter().cloned().collect())
    }

    /// Summary aggregation over the ledger.
    pub fn summarize(
        &self,
        q: &CostQuery,
        quality_passing_tasks: &std::collections::HashSet<String>,
    ) -> Result<CostSummary, SearchOsError> {
        let events = self.query(q)?;
        let refs: Vec<&CostEvent> = events.iter().collect();
        let stats = searchos_cost::LedgerStats::aggregate(refs, quality_passing_tasks);
        let currency = events
            .first()
            .map(|ev| ev.estimated_cost.currency.clone())
            .unwrap_or_else(|| "USD".into());
        Ok(stats.to_summary(&currency))
    }
}

/// Search cache repository (Part 17 17.9).
pub struct SearchCacheRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> SearchCacheRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Puts a serialized result with TTL.
    pub fn put(
        &self,
        key: &SearchCacheKey,
        result_json: &str,
        profile_version: &str,
        policy_version: &str,
        ttl_seconds: u64,
    ) -> Result<(), SearchOsError> {
        let expires =
            (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds as i64)).to_rfc3339();
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO search_cache (id, cache_key, result_ref, profile_version, policy_version, created_at, expires_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7)
                 ON CONFLICT(cache_key) DO UPDATE SET result_ref=?3, profile_version=?4, policy_version=?5, created_at=?6, expires_at=?7",
                params![searchos_domain::new_id("cache"), key.cache_key(), result_json, profile_version, policy_version, now(), expires],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Gets a non-expired cached result.
    pub fn get(
        &self,
        key: &SearchCacheKey,
        policy_version: &str,
    ) -> Result<Option<String>, SearchOsError> {
        self.store.with_conn(|c| {
            let row: Option<(String, String)> = c
                .query_row(
                    "SELECT result_ref, policy_version FROM search_cache WHERE cache_key = ?1 AND expires_at > ?2",
                    params![key.cache_key(), now()],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()
                .map_err(e)?;
            Ok(row.filter(|(_, pv)| pv == policy_version).map(|(result, _)| result))
        })
    }

    /// Purges expired entries (maintenance).
    pub fn purge_expired(&self) -> Result<u64, SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "DELETE FROM search_cache WHERE expires_at <= ?1",
                params![now()],
            )
            .map(|n| n as u64)
            .map_err(e)
        })
    }
}

/// Provider registry persistence (Part 17 17.10).
pub struct ProviderRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> ProviderRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Upserts provider status/config reference (never the API key itself —
    /// RFC-014 14.3: 数据库仅保存 Secret Reference).
    pub fn upsert(
        &self,
        id: &str,
        name: &str,
        provider_type: &str,
        status: &str,
        config_ref: &str,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO providers (id, name, provider_type, status, config_ref, created_at, updated_at) VALUES (?1,?2,?3,?4,?5,?6,?6)
                 ON CONFLICT(id) DO UPDATE SET status=?4, config_ref=?5, updated_at=?6",
                params![id, name, provider_type, status, config_ref, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Lists providers.
    pub fn list(&self) -> Result<Vec<(String, String, String, String)>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare("SELECT id, name, provider_type, status FROM providers ORDER BY id")
                .map_err(e)?;
            let rows = stmt
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }
}

/// Ingest job persistence (RFC-029 7): batch / directory import tracking.
pub struct IngestJobRepo<'a> {
    store: &'a SqliteStore,
}

/// One ingest job row.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestJobRow {
    pub id: String,
    pub workspace_id: String,
    pub source_type: String,
    pub state: String,
    pub total_items: i64,
    pub ok_items: i64,
    pub failed_items: i64,
    pub error_detail: Option<String>,
    pub created_at: String,
    pub finished_at: Option<String>,
}

/// One batch item outcome.
#[derive(Debug, Clone, serde::Serialize)]
pub struct IngestJobItemRow {
    pub id: String,
    pub job_id: String,
    pub source_key: String,
    pub state: String,
    pub document_id: Option<String>,
    pub error_detail: Option<String>,
}

impl<'a> IngestJobRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Creates a job in `pending`.
    pub fn create(
        &self,
        id: &str,
        workspace_id: &str,
        source_type: &str,
        total_items: i64,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO ingest_jobs (id, workspace_id, source_type, state, total_items, ok_items, failed_items, created_at)
                 VALUES (?1,?2,?3,'pending',?4,0,0,?5)",
                params![id, workspace_id, source_type, total_items, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Marks a job `running`.
    pub fn mark_running(&self, id: &str) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE ingest_jobs SET state='running' WHERE id=?1",
                params![id],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Closes a job with per-item counters.
    pub fn finish(
        &self,
        id: &str,
        state: &str,
        ok_items: i64,
        failed_items: i64,
        error_detail: Option<&str>,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE ingest_jobs SET state=?2, ok_items=?3, failed_items=?4, error_detail=?5, finished_at=?6 WHERE id=?1",
                params![id, state, ok_items, failed_items, error_detail, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Fetches one job.
    pub fn get(&self, id: &str) -> Result<Option<IngestJobRow>, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row(
                "SELECT id, workspace_id, source_type, state, total_items, ok_items, failed_items, error_detail, created_at, finished_at
                 FROM ingest_jobs WHERE id=?1",
                params![id],
                |r| {
                    Ok(IngestJobRow {
                        id: r.get(0)?,
                        workspace_id: r.get(1)?,
                        source_type: r.get(2)?,
                        state: r.get(3)?,
                        total_items: r.get(4)?,
                        ok_items: r.get(5)?,
                        failed_items: r.get(6)?,
                        error_detail: r.get(7)?,
                        created_at: r.get(8)?,
                        finished_at: r.get(9)?,
                    })
                },
            )
            .optional()
            .map_err(e)
        })
    }

    /// Records one item outcome.
    pub fn insert_item(
        &self,
        id: &str,
        job_id: &str,
        source_key: &str,
        state: &str,
        document_id: Option<&str>,
        error_detail: Option<&str>,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO ingest_job_items (id, job_id, source_key, state, document_id, error_detail)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![id, job_id, source_key, state, document_id, error_detail],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Lists the items of a job.
    pub fn list_items(&self, job_id: &str) -> Result<Vec<IngestJobItemRow>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare("SELECT id, job_id, source_key, state, document_id, error_detail FROM ingest_job_items WHERE job_id=?1 ORDER BY rowid")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![job_id], |r| {
                    Ok(IngestJobItemRow {
                        id: r.get(0)?,
                        job_id: r.get(1)?,
                        source_key: r.get(2)?,
                        state: r.get(3)?,
                        document_id: r.get(4)?,
                        error_detail: r.get(5)?,
                    })
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }
}

/// Outbox worker support (RFC-006 6.7): fetch pending, mark processed, bump
/// attempts with retry backoff. Failed jobs can be retried.
pub struct OutboxRepo<'a> {
    store: &'a SqliteStore,
}

/// One pending outbox row.
#[derive(Debug, Clone)]
pub struct OutboxRow {
    pub id: String,
    pub entity_type: String,
    pub entity_id: String,
    pub operation: String,
    pub attempts: i64,
    pub payload: Option<String>,
}

impl<'a> OutboxRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Pending rows ready for processing.
    pub fn pending(&self, limit: u32) -> Result<Vec<OutboxRow>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare("SELECT id, entity_type, entity_id, operation, attempts, payload FROM index_outbox WHERE status = 'pending' AND (next_retry_at IS NULL OR next_retry_at <= ?1) ORDER BY created_at ASC LIMIT ?2")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![now(), limit], |r| {
                    Ok(OutboxRow {
                        id: r.get(0)?,
                        entity_type: r.get(1)?,
                        entity_id: r.get(2)?,
                        operation: r.get(3)?,
                        attempts: r.get(4)?,
                        payload: r.get(5)?,
                    })
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }

    /// Marks a row processed.
    pub fn mark_processed(&self, id: &str) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE index_outbox SET status='processed', processed_at=?2 WHERE id=?1",
                params![id, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Marks failure: bumps attempts, schedules retry with exponential
    /// backoff; too many attempts → dead-lettered (RFC-014 14.4 DLQ).
    pub fn mark_failed(
        &self,
        id: &str,
        max_attempts: i64,
    ) -> Result<OutboxDisposition, SearchOsError> {
        self.store.with_conn(|c| {
            let attempts: i64 = c
                .query_row(
                    "SELECT attempts FROM index_outbox WHERE id=?1",
                    params![id],
                    |r| r.get(0),
                )
                .optional()
                .map_err(e)?
                .ok_or_else(|| e(rusqlite::Error::QueryReturnedNoRows))?;
            let new_attempts = attempts + 1;
            if new_attempts >= max_attempts {
                c.execute(
                    "UPDATE index_outbox SET status='dead', attempts=?2 WHERE id=?1",
                    params![id, new_attempts],
                )
                .map_err(e)?;
                Ok(OutboxDisposition::DeadLetter)
            } else {
                let backoff_secs = 2i64.pow(new_attempts as u32).min(600);
                let next =
                    (chrono::Utc::now() + chrono::Duration::seconds(backoff_secs)).to_rfc3339();
                c.execute(
                    "UPDATE index_outbox SET attempts=?2, next_retry_at=?3 WHERE id=?1",
                    params![id, new_attempts, next],
                )
                .map_err(e)?;
                Ok(OutboxDisposition::Retry)
            }
        })
    }

    /// Counts by status (index status diagnostics).
    pub fn counts(&self) -> Result<(i64, i64, i64), SearchOsError> {
        self.store.with_conn(|c| {
            let one = |status: &str, c: &Connection| -> Result<i64, SearchOsError> {
                c.query_row(
                    "SELECT count(*) FROM index_outbox WHERE status=?1",
                    params![status],
                    |r| r.get(0),
                )
                .map_err(e)
            };
            Ok((one("pending", c)?, one("processed", c)?, one("dead", c)?))
        })
    }
}

/// Result of a failed outbox row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboxDisposition {
    Retry,
    DeadLetter,
}

/// Memory entry persistence (RFC-013; isolation filtering in memory crate).
pub struct MemoryRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> MemoryRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Inserts a memory entry.
    pub fn insert(
        &self,
        entry: &searchos_domain::memory::MemoryEntry,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO memory_entries (id, workspace_id, project, agent, sharing, title, content, url, tags_json, embedding_status, created_at, updated_at)
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
                params![
                    entry.id.as_str(),
                    entry.namespace.workspace.as_str(),
                    entry.namespace.project,
                    entry.namespace.agent,
                    sharing_str(entry.sharing),
                    entry.title,
                    entry.content,
                    entry.url,
                    serde_json::to_string(&entry.tags).map_err(|er| SearchOsError::Internal(er.to_string()))?,
                    entry.embedding_status,
                    entry.created_at.to_rfc3339(),
                    entry.updated_at.to_rfc3339(),
                ],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Lists entries in a workspace (isolation filtering by caller).
    pub fn list_workspace(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<Vec<searchos_domain::memory::MemoryEntry>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare("SELECT id, workspace_id, project, agent, sharing, title, content, url, tags_json, embedding_status, created_at, updated_at FROM memory_entries WHERE workspace_id = ?1 ORDER BY created_at DESC")
                .map_err(e)?;
            let rows = stmt
                .query_map(params![workspace_id.as_str()], row_memory)
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }

    /// Lists everything (export / retention sweep — caller filters).
    pub fn list_all(&self) -> Result<Vec<searchos_domain::memory::MemoryEntry>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare("SELECT id, workspace_id, project, agent, sharing, title, content, url, tags_json, embedding_status, created_at, updated_at FROM memory_entries ORDER BY created_at DESC")
                .map_err(e)?;
            let rows = stmt.query_map([], row_memory).map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }

    /// Deletes an entry (RFC-013 13.5 governance Delete).
    pub fn delete(&self, entry_id: &str) -> Result<bool, SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "DELETE FROM memory_entries WHERE id = ?1",
                params![entry_id],
            )
            .map(|n| n > 0)
            .map_err(e)
        })
    }
}

fn row_memory(r: &rusqlite::Row) -> rusqlite::Result<searchos_domain::memory::MemoryEntry> {
    Ok(searchos_domain::memory::MemoryEntry {
        id: searchos_domain::ids::MemoryEntryId::new(r.get::<_, String>(0)?),
        namespace: searchos_domain::memory::MemoryNamespace {
            workspace: WorkspaceId::new(r.get::<_, String>(1)?),
            project: r.get(2)?,
            agent: r.get(3)?,
        },
        sharing: sharing_from(&r.get::<_, String>(4)?),
        title: r.get(5)?,
        content: r.get(6)?,
        url: r.get(7)?,
        tags: serde_json::from_str(&r.get::<_, String>(8)?).unwrap_or_default(),
        created_at: parse_time(&r.get::<_, String>(10)?),
        updated_at: parse_time(&r.get::<_, String>(11)?),
        embedding_status: r.get(9)?,
    })
}

/// Singleflight bookkeeping (RFC-009 9.8): records shared executions so
/// savings are auditable without double billing.
pub struct SingleflightRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> SingleflightRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Records a merged execution: leader + followers + saved cost.
    pub fn record(
        &self,
        fingerprint: &str,
        leader_task_id: &searchos_domain::ids::TaskId,
        follower_task_ids: &[String],
        saved_cost_micros: i64,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO singleflight_records (id, fingerprint, leader_task_id, follower_task_ids_json, saved_cost_micros, created_at)
                 VALUES (?1,?2,?3,?4,?5,?6)",
                params![
                    searchos_domain::new_id("sf"),
                    fingerprint,
                    leader_task_id.as_str(),
                    serde_json::to_string(follower_task_ids).map_err(|er| SearchOsError::Internal(er.to_string()))?,
                    saved_cost_micros,
                    now(),
                ],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Total saved micros (cost report input).
    pub fn total_saved(&self) -> Result<i64, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row(
                "SELECT COALESCE(SUM(saved_cost_micros),0) FROM singleflight_records",
                [],
                |r| r.get(0),
            )
            .map_err(e)
        })
    }
}

/// Audit events (17.13 audit_events): security-relevant actions recorded.
pub fn audit(
    store: &SqliteStore,
    actor: &str,
    action: &str,
    target: Option<&str>,
    detail_json: Option<&str>,
) -> Result<(), SearchOsError> {
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO audit_events (id, actor, action, target, detail_json, at) VALUES (?1,?2,?3,?4,?5,?6)",
            params![searchos_domain::new_id("aud"), actor, action, target, detail_json, now()],
        )
        .map(|_| ())
        .map_err(e)
    })
}

fn domain_of(url: &str) -> String {
    let rest = url
        .strip_prefix("https://")
        .or_else(|| url.strip_prefix("http://"))
        .unwrap_or(url);
    rest.split(['/', '?', '#'])
        .next()
        .unwrap_or("unknown")
        .to_string()
}

/// Domain recorded for a source row (RFC-029 5.2): local ingest is not a
/// network host, so it gets the fixed domain `local`.
fn source_domain(canonical_url: &str, source_type: &str) -> String {
    if source_type != "web" || canonical_url.starts_with("searchos-local://") {
        "local".to_string()
    } else {
        domain_of(canonical_url)
    }
}

fn sharing_str(s: searchos_domain::memory::SharingPolicy) -> &'static str {
    match s {
        searchos_domain::memory::SharingPolicy::Private => "private",
        searchos_domain::memory::SharingPolicy::WorkspaceShared => "workspace_shared",
        searchos_domain::memory::SharingPolicy::ProjectShared => "project_shared",
        searchos_domain::memory::SharingPolicy::GlobalPublic => "global_public",
    }
}

fn sharing_from(s: &str) -> searchos_domain::memory::SharingPolicy {
    match s {
        "workspace_shared" => searchos_domain::memory::SharingPolicy::WorkspaceShared,
        "project_shared" => searchos_domain::memory::SharingPolicy::ProjectShared,
        "global_public" => searchos_domain::memory::SharingPolicy::GlobalPublic,
        _ => searchos_domain::memory::SharingPolicy::Private,
    }
}

fn parse_time(s: &str) -> chrono::DateTime<chrono::Utc> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|t| t.into())
        .unwrap_or_default()
}

/// Retrieval candidates for outbox payloads (round-trip helper).
pub fn parse_index_payload(payload: &str) -> Result<IndexedDocument, SearchOsError> {
    serde_json::from_str(payload)
        .map_err(|er| SearchOsError::Internal(format!("outbox payload: {er}")))
}

/// Persists retrieval-cache candidates (Part 17 retrieval_cache).
pub fn put_retrieval_cache(
    store: &SqliteStore,
    fingerprint: &str,
    index_version: u64,
    top_k: u32,
    candidates: &[RetrievalCandidate],
    policy_version: &str,
    ttl_seconds: u64,
) -> Result<(), SearchOsError> {
    let expires = (chrono::Utc::now() + chrono::Duration::seconds(ttl_seconds as i64)).to_rfc3339();
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO retrieval_cache (id, query_fingerprint, index_version, top_k, candidates_json, policy_version, created_at, expires_at)
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(query_fingerprint, index_version, top_k) DO UPDATE SET candidates_json=?5, policy_version=?6, created_at=?7, expires_at=?8",
            params![
                searchos_domain::new_id("rc"),
                fingerprint,
                index_version as i64,
                top_k,
                serde_json::to_string(candidates).map_err(|er| SearchOsError::Internal(er.to_string()))?,
                policy_version,
                now(),
                expires,
            ],
        )
        .map(|_| ())
        .map_err(e)
    })
}

/// Reads retrieval-cache candidates.
pub fn get_retrieval_cache(
    store: &SqliteStore,
    fingerprint: &str,
    index_version: u64,
    top_k: u32,
    policy_version: &str,
) -> Result<Option<Vec<RetrievalCandidate>>, SearchOsError> {
    store.with_conn(|c| {
        let row: Option<(String, String)> = c
            .query_row(
                "SELECT candidates_json, policy_version FROM retrieval_cache WHERE query_fingerprint=?1 AND index_version=?2 AND top_k=?3 AND expires_at > ?4",
                params![fingerprint, index_version as i64, top_k, now()],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(e)?;
        Ok(row
            .filter(|(_, pv)| pv == policy_version)
            .map(|(json, _)| serde_json::from_str(&json).unwrap_or_default()))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::SqliteStore;
    use chrono::Utc;
    use searchos_document::{chunk_text, ChunkingConfig};
    use searchos_domain::document::Document;
    use searchos_domain::ids::{ChunkId, DocumentId, DocumentVersionId};

    fn store() -> SqliteStore {
        SqliteStore::open_in_memory().unwrap()
    }

    fn document(url: &str) -> Document {
        let sid = SourceId::generate();
        let now = Utc::now();
        Document {
            id: DocumentId::generate(),
            source_id: sid,
            canonical_url: url.into(),
            current_version_id: None,
            content_type: "text/html".into(),
            language: Some("en".into()),
            source_type: "web".into(),
            title: None,
            workspace_id: "default".into(),
            created_at: now,
            updated_at: now,
        }
    }

    fn version(
        doc: &Document,
        content: &str,
    ) -> (
        DocumentVersion,
        Vec<searchos_domain::document::DocumentChunk>,
        Vec<IndexedDocument>,
    ) {
        let hash = searchos_document::content_hash(content.as_bytes());
        let ref_ = searchos_document::content_ref(content.as_bytes());
        let ref_clone = ref_.clone();
        let version = DocumentVersion {
            id: DocumentVersionId::generate(),
            document_id: doc.id.clone(),
            content_hash: hash.clone(),
            raw_content_ref: ref_.clone(),
            markdown_ref: Some(ref_),
            fetched_at: Utc::now(),
            published_at: None,
            extractor_version: searchos_document::EXTRACTOR_VERSION.into(),
            crawler_version: None,
        };
        let chunks = chunk_text(content, &ChunkingConfig::default())
            .into_iter()
            .map(|c| searchos_domain::document::DocumentChunk {
                id: ChunkId::generate(),
                version_id: version.id.clone(),
                document_id: doc.id.clone(),
                chunk_hash: c.hash,
                content_ref: ref_clone.clone(),
                text: c.text,
                token_count: None,
                chunk_index: c.index,
                created_at: Utc::now(),
            })
            .collect::<Vec<_>>();
        let payloads = chunks
            .iter()
            .map(|ch| IndexedDocument {
                chunk_id: ch.id.clone(),
                document_id: doc.id.clone(),
                version_id: version.id.clone(),
                title: Some("Test Doc".into()),
                body: ch.text.clone(),
                url: doc.canonical_url.clone(),
                language: Some("en".into()),
                workspace_id: "ws".into(),
                fetched_at: Utc::now(),
            })
            .collect();
        (version, chunks, payloads)
    }

    #[test]
    fn list_chunks_returns_rows_in_order() {
        let s = store();
        let repo = DocumentRepo::new(&s);
        let doc = document("https://example.com/chunks");
        let (v1, chunks, payloads) = version(&doc, "alpha beta gamma delta");
        let ws = searchos_domain::ids::WorkspaceId::new("ws");
        repo.save_versioned(&doc, &v1, &chunks, &payloads, &ws)
            .unwrap();
        let rows = repo.list_chunks(&v1.id).unwrap();
        assert_eq!(rows.len(), chunks.len());
        assert_eq!(rows[0].id.as_str(), chunks[0].id.as_str());
        assert_eq!(rows[0].chunk_index, 0);
    }

    #[test]
    fn search_matches_title_and_excludes_deleted() {
        let s = store();
        let repo = DocumentRepo::new(&s);
        let mut doc = document("https://example.com/searchable");
        doc.title = Some("Alpha Handbook".into());
        let (v1, chunks, payloads) = version(&doc, "alpha content");
        let ws = searchos_domain::ids::WorkspaceId::new("ws");
        repo.save_versioned(&doc, &v1, &chunks, &payloads, &ws)
            .unwrap();
        let hits = repo.search("alpha", 10, None).unwrap();
        assert_eq!(hits.len(), 1);
        // The workspace predicate is in SQL, not just the token layer.
        assert!(repo.search("alpha", 10, Some("other")).unwrap().is_empty());
        assert!(!repo.search("alpha", 10, Some("ws")).unwrap().is_empty());
        // LIKE wildcards in the caller query are escaped, not treated as SQL.
        assert!(repo.search("%", 10, None).unwrap().is_empty());
        repo.delete(&doc.id).unwrap();
        assert!(repo.search("alpha", 10, None).unwrap().is_empty());
    }

    #[test]
    fn versioning_unchanged_then_new() {
        let s = store();
        let repo = DocumentRepo::new(&s);
        let doc = document("https://example.com/a");
        let (v1, chunks1, payloads1) = version(&doc, "first content body");
        let ws = searchos_domain::ids::WorkspaceId::new("ws");
        let diff1 = repo
            .save_versioned(&doc, &v1, &chunks1, &payloads1, &ws)
            .unwrap();
        assert_eq!(diff1, searchos_document::VersionDiff::NewVersion);
        // Refresh in-memory pointer from the DB (as the application layer does).
        let doc = repo.get(&doc.id).unwrap().unwrap();

        // Same content again → Unchanged, no new version, no outbox rows.
        let (v1b, _, _) = version(&doc, "first content body");
        let diff2 = repo
            .save_versioned(&doc, &v1b, &chunks1, &payloads1, &ws)
            .unwrap();
        assert_eq!(diff2, searchos_document::VersionDiff::Unchanged);
        let outbox = OutboxRepo::new(&s);
        let (pending, processed, _) = outbox.counts().unwrap();
        assert_eq!(pending, chunks1.len() as i64);
        assert_eq!(processed, 0);

        // Changed content → NewVersion + more outbox rows.
        let (v2, chunks2, payloads2) = version(&doc, "changed content body!");
        let diff3 = repo
            .save_versioned(&doc, &v2, &chunks2, &payloads2, &ws)
            .unwrap();
        assert_eq!(diff3, searchos_document::VersionDiff::NewVersion);
        let fetched = repo.get(&doc.id).unwrap().unwrap();
        assert_eq!(
            fetched.current_version_id.as_ref().map(|v| v.as_str()),
            Some(v2.id.as_str())
        );
        let cur = repo.get_current_version(&doc.id).unwrap().unwrap();
        assert_eq!(cur.content_hash, v2.content_hash);
    }

    #[test]
    fn outbox_retry_and_dead_letter() {
        let s = store();
        let doc = document("https://example.com/b");
        let repo = DocumentRepo::new(&s);
        let (v, chunks, payloads) = version(&doc, "content for outbox");
        let ws = searchos_domain::ids::WorkspaceId::new("ws");
        repo.save_versioned(&doc, &v, &chunks, &payloads, &ws)
            .unwrap();
        let outbox = OutboxRepo::new(&s);
        let rows = outbox.pending(100).unwrap();
        assert!(!rows.is_empty());
        let row = rows[0].clone();
        // Fail twice → retry, then dead-letter at max_attempts.
        assert_eq!(
            outbox.mark_failed(&row.id, 2).unwrap(),
            OutboxDisposition::Retry
        );
        assert_eq!(
            outbox.mark_failed(&row.id, 2).unwrap(),
            OutboxDisposition::DeadLetter
        );
        let rows2 = outbox.pending(100).unwrap();
        assert!(
            !rows2.iter().any(|r| r.id == row.id),
            "dead rows not re-delivered"
        );
        // Success path on a remaining row (or a fresh one from a new save).
        let target = rows2.first().cloned().unwrap_or_else(|| {
            let (v3, c3, p3) = version(&doc, "second content for success");
            repo.save_versioned(&doc, &v3, &c3, &p3, &ws).unwrap();
            let mut doc2 = repo.get(&doc.id).unwrap().unwrap();
            let (v4, c4, p4) = version(&doc2, "third content for success");
            doc2.current_version_id = Some(v3.id.clone());
            repo.save_versioned(&doc2, &v4, &c4, &p4, &ws).unwrap();
            outbox.pending(100).unwrap().remove(0)
        });
        outbox.mark_processed(&target.id).unwrap();
        let (_, processed, _) = outbox.counts().unwrap();
        assert!(processed >= 1);
    }

    #[test]
    fn cost_append_and_summarize() {
        let s = store();
        let ws = WorkspaceId::new("ws1");
        ensure_workspace(&s, &ws, "ws one").unwrap();
        let task = searchos_domain::ids::TaskId::generate();
        let trace = searchos_domain::ids::TraceId::generate();
        TaskRepo::new(&s)
            .insert(
                &task,
                &ws,
                None,
                "q",
                "q",
                "fp",
                "general",
                "1.0.0",
                "local_first",
                &trace,
            )
            .unwrap();
        let cost = CostRepo::new(&s);
        let ev = CostEvent {
            id: searchos_domain::ids::CostEventId::generate(),
            task_id: task.clone(),
            workspace_id: ws.clone(),
            agent_id: Some(searchos_domain::ids::AgentId::new("agent_1")),
            operation: CostOperation::WebSearch,
            provider_id: Some("tavily".into()),
            input_units: 1,
            output_units: 0,
            estimated_cost: Money::parse("0.005", "USD").unwrap(),
            actual_cost: None,
            duration_ms: 800,
            occurred_at: Utc::now(),
            trace_id: Some(trace),
            shared: false,
            pricing_model_version: "1".into(),
            status: Default::default(),
            error_code: None,
        };
        cost.append(&ev).unwrap();
        let summary = cost
            .summarize(&CostQuery::default(), &Default::default())
            .unwrap();
        assert_eq!(summary.total_estimated.amount_micros, 5_000);
        assert_eq!(summary.by_provider[0].0, "tavily");
        assert_eq!(
            summary.cost_per_successful_query, None,
            "no passing queries yet"
        );
        let mut passing = std::collections::HashSet::new();
        passing.insert(task.as_str().to_string());
        let summary2 = cost.summarize(&CostQuery::default(), &passing).unwrap();
        assert_eq!(
            summary2.cost_per_successful_query.unwrap().amount_micros,
            5_000
        );
    }

    #[test]
    fn search_cache_ttl_and_policy() {
        let s = store();
        let repo = SearchCacheRepo::new(&s);
        let key = SearchCacheKey {
            query_fingerprint: "fp".into(),
            profile_version: "1.0.0".into(),
            freshness_policy_version: "1".into(),
            source_policy_version: "1".into(),
            language: "en".into(),
            workspace_scope: "ws".into(),
            mode: "local_only".into(),
            freshness_requirement: "recent".into(),
            key_version: "v2".into(),
        };
        repo.put(&key, r#"{"q":"x"}"#, "1.0.0", "1", 600).unwrap();
        assert!(repo.get(&key, "1").unwrap().is_some());
        assert!(
            repo.get(&key, "2").unwrap().is_none(),
            "policy change invalidates"
        );
        // Expired entries purged.
        repo.put(&key, r#"{}"#, "1.0.0", "1", 0).unwrap();
        assert!(repo.get(&key, "1").unwrap().is_none(), "ttl 0 = expired");
        assert!(repo.purge_expired().is_ok());
    }

    #[test]
    fn memory_and_traces() {
        let s = store();
        let m = MemoryRepo::new(&s);
        let entry = searchos_domain::memory::MemoryEntry {
            id: searchos_domain::ids::MemoryEntryId::generate(),
            namespace: searchos_domain::memory::MemoryNamespace {
                workspace: WorkspaceId::new("ws1"),
                project: Some("proj".into()),
                agent: None,
            },
            sharing: searchos_domain::memory::SharingPolicy::WorkspaceShared,
            title: "Rust frameworks".into(),
            content: "tokio and axum notes".into(),
            url: Some("https://tokio.rs".into()),
            tags: vec!["rust".into()],
            created_at: Utc::now(),
            updated_at: Utc::now(),
            embedding_status: "none".into(),
        };
        m.insert(&entry).unwrap();
        let listed = m.list_workspace(&WorkspaceId::new("ws1")).unwrap();
        assert_eq!(listed.len(), 1);
        assert!(m.delete(entry.id.as_str()).unwrap());
        assert!(m
            .list_workspace(&WorkspaceId::new("ws1"))
            .unwrap()
            .is_empty());

        let d = DecisionRepo::new(&s);
        let trace = DecisionTrace {
            trace_id: searchos_domain::ids::TraceId::generate(),
            task_id: searchos_domain::ids::TaskId::generate(),
            intents: vec![searchos_domain::request::SearchIntent::Technical],
            profile_version: "1.0.0".into(),
            local_candidates: 3,
            accepted_candidates: 2,
            freshness_status: "recent".into(),
            coverage_status: "1".into(),
            source_status: "open".into(),
            selected_path: "local".into(),
            rejection_reasons: vec!["local_evidence_passed_quality_gate".into()],
            estimated_cost: Money::zero("USD"),
            actual_cost: None,
            decision_at: Utc::now(),
            stages: vec![searchos_domain::decision::DecisionStageTrace {
                stage: "validation".into(),
                status: "completed".into(),
                started_at_ms: 0,
                duration_ms: 1,
                input_summary: "request".into(),
                output_summary: "valid".into(),
                reasons: vec![],
            }],
            final_strategy: Some("local_resolve".into()),
            final_outcome: Some("completed".into()),
        };
        d.insert_trace(&trace).unwrap();
        let back = d.get_trace(trace.trace_id.as_str()).unwrap().unwrap();
        assert_eq!(back.selected_path, "local");
        assert_eq!(back.stages.len(), 1);
        assert_eq!(back.final_strategy.as_deref(), Some("local_resolve"));
        assert_eq!(
            back.intents,
            vec![searchos_domain::request::SearchIntent::Technical]
        );
        assert!(d.get_trace("nope").unwrap().is_none());
    }

    #[test]
    fn deletion_soft_with_index_events() {
        let s = store();
        let repo = DocumentRepo::new(&s);
        let doc = document("https://example.com/c");
        let (v, chunks, payloads) = version(&doc, "to be deleted");
        let ws = searchos_domain::ids::WorkspaceId::new("ws");
        repo.save_versioned(&doc, &v, &chunks, &payloads, &ws)
            .unwrap();
        repo.delete(&doc.id).unwrap();
        let outbox = OutboxRepo::new(&s);
        let rows = outbox.pending(100).unwrap();
        let delete_rows = rows.iter().filter(|r| r.operation == "delete").count();
        assert!(delete_rows >= 1, "deletion enqueues index delete events");
        let fetched = repo.get(&doc.id).unwrap().unwrap();
        assert!(
            fetched.current_version_id.is_none(),
            "current pointer cleared on delete"
        );
    }

    #[test]
    fn singleflight_bookkeeping() {
        let s = store();
        let sf = SingleflightRepo::new(&s);
        sf.record(
            "fp1",
            &searchos_domain::ids::TaskId::generate(),
            &["t2".into(), "t3".into()],
            5000,
        )
        .unwrap();
        assert_eq!(sf.total_saved().unwrap(), 5000);
    }
}

// ---------------------------------------------------------------------------
// RFC-014 14.7 / RFC-004 v1.1 14.2 — request-level idempotency.
// ---------------------------------------------------------------------------

/// Replay record for one client-supplied `request_id`.
#[derive(Debug, Clone)]
pub struct IdempotencyRecord {
    pub request_id: String,
    pub task_id: String,
    pub status: String,
    pub response_json: Option<String>,
    pub claim_owner_task_id: Option<String>,
    pub claimed_at: Option<String>,
}

/// Idempotency ledger: a repeated `request_id` must not re-run an external
/// search, re-bill, or re-write the same document version.
pub struct IdempotencyRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> IdempotencyRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Atomically claims a request id. Returns `true` for the first caller.
    pub fn begin(
        &self,
        request_id: &str,
        task_id: &str,
        workspace_id: &str,
    ) -> Result<bool, SearchOsError> {
        self.store.with_conn(|c| {
            let at = now();
            let n = c
                .execute(
                    "INSERT OR IGNORE INTO request_idempotency (request_id, task_id, workspace_id, status, claim_owner_task_id, claimed_at, created_at)
                     VALUES (?1, ?2, ?3, 'running', ?2, ?4, ?4)",
                    params![request_id, task_id, workspace_id, at],
                )
                .map_err(e)?;
            Ok(n == 1)
        })
    }

    /// Fetches a completed replay payload, if any.
    pub fn get(&self, request_id: &str) -> Result<Option<IdempotencyRecord>, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row(
                "SELECT request_id, task_id, status, response_json, claim_owner_task_id, claimed_at FROM request_idempotency WHERE request_id = ?1",
                params![request_id],
                |r| {
                    Ok(IdempotencyRecord {
                        request_id: r.get(0)?,
                        task_id: r.get(1)?,
                        status: r.get(2)?,
                        response_json: r.get(3)?,
                        claim_owner_task_id: r.get(4)?,
                        claimed_at: r.get(5)?,
                    })
                },
            )
            .optional()
            .map_err(e)
        })
    }

    /// Stores the response so a replay returns the recoverable task state.
    pub fn complete(&self, request_id: &str, payload_json: &str) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE request_idempotency SET status = 'completed', response_json = ?2, completed_at = ?3 WHERE request_id = ?1",
                params![request_id, payload_json, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Current durable claim status (`running | completed | failed`).
    pub fn status(&self, request_id: &str) -> Result<Option<String>, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row(
                "SELECT status FROM request_idempotency WHERE request_id = ?1",
                params![request_id],
                |r| r.get::<_, String>(0),
            )
            .optional()
            .map_err(e)
        })
    }

    /// Marks a failed task so replays do not hide the failure.
    pub fn fail(&self, request_id: &str) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE request_idempotency SET status = 'failed', completed_at = ?2 WHERE request_id = ?1",
                params![request_id, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }
}

// ---------------------------------------------------------------------------
// RFC-004 v1.1 15 / RFC-006 6.11 — reserve, execute, settle.
// ---------------------------------------------------------------------------

/// One budget reservation row.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BudgetReservationRow {
    pub id: String,
    pub task_id: String,
    pub trace_id: Option<String>,
    pub estimated_cost_micros: i64,
    pub spent_cost_micros: i64,
    pub currency: String,
    pub status: String,
    pub created_at: String,
    pub settled_at: Option<String>,
    pub released_at: Option<String>,
}

/// Budget reservation ledger (estimated cost is reserved before execution).
pub struct BudgetRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> BudgetRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    pub fn reserve(
        &self,
        id: &str,
        task_id: &str,
        trace_id: Option<&str>,
        estimated_cost_micros: i64,
        currency: &str,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO budget_reservations (id, task_id, trace_id, estimated_cost_micros, spent_cost_micros, currency, status, created_at)
                 VALUES (?1, ?2, ?3, ?4, 0, ?5, 'reserved', ?6)",
                params![id, task_id, trace_id, estimated_cost_micros, currency, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Writes back actual spend and releases the unused reservation.
    pub fn settle(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE budget_reservations SET spent_cost_micros = ?2, status = 'settled', settled_at = ?3, released_at = ?3 WHERE task_id = ?1 AND status = 'reserved'",
                params![task_id, spent_micros, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    pub fn mark_exceeded(&self, task_id: &str) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE budget_reservations SET status = 'exceeded', settled_at = ?2, released_at = ?2 WHERE task_id = ?1 AND status = 'reserved'",
                params![task_id, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Releases a reservation without treating it as a bill.
    pub fn release(&self, task_id: &str, spent_micros: i64) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "UPDATE budget_reservations SET spent_cost_micros = ?2, status = 'released', released_at = ?3 WHERE task_id = ?1 AND status = 'reserved'",
                params![task_id, spent_micros, now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    pub fn list(&self, limit: u32) -> Result<Vec<BudgetReservationRow>, SearchOsError> {
        self.list_scoped(limit, None)
    }

    /// Reservations with a workspace predicate pushed into SQL (INV-30-10).
    pub fn list_scoped(
        &self,
        limit: u32,
        workspace: Option<&str>,
    ) -> Result<Vec<BudgetReservationRow>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c
                .prepare(
                    "SELECT br.id, br.task_id, br.trace_id, br.estimated_cost_micros, br.spent_cost_micros, br.currency, br.status, br.created_at, br.settled_at, br.released_at
                     FROM budget_reservations br
                     LEFT JOIN search_tasks st ON st.id = br.task_id
                     WHERE (?1 IS NULL OR COALESCE(st.workspace_id, 'default') = ?1 OR st.workspace_id = '*')
                     ORDER BY br.created_at DESC LIMIT ?2",
                )
                .map_err(e)?;
            let rows = stmt
                .query_map(params![workspace, limit], |r| {
                    Ok(BudgetReservationRow {
                        id: r.get(0)?,
                        task_id: r.get(1)?,
                        trace_id: r.get(2)?,
                        estimated_cost_micros: r.get(3)?,
                        spent_cost_micros: r.get(4)?,
                        currency: r.get(5)?,
                        status: r.get(6)?,
                        created_at: r.get(7)?,
                        settled_at: r.get(8)?,
                        released_at: r.get(9)?,
                    })
                })
                .map_err(e)?;
            rows.collect::<Result<Vec<_>, _>>().map_err(e)
        })
    }
}

// ---------------------------------------------------------------------------
// RFC-006 6.7 — estimated vs actual reconciliation from provider billing.
// ---------------------------------------------------------------------------

pub struct ReconciliationRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> ReconciliationRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Appends a billing adjustment and updates the event's actual cost.
    /// The original estimated cost is never rewritten.
    pub fn record(
        &self,
        cost_event_id: &str,
        actual_cost_micros: i64,
        source: &str,
        currency: &str,
    ) -> Result<(), SearchOsError> {
        self.store.with_conn(|c| {
            c.execute(
                "INSERT INTO cost_reconciliations (id, cost_event_id, actual_cost_micros, source, currency, at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    searchos_domain::new_id("recon"),
                    cost_event_id,
                    actual_cost_micros,
                    source,
                    currency,
                    now()
                ],
            )
            .map_err(e)?;
            let n = c
                .execute(
                    "UPDATE cost_events SET actual_cost_micros = ?2 WHERE id = ?1",
                    params![cost_event_id, actual_cost_micros],
                )
                .map_err(e)?;
            if n != 1 {
                return Err(SearchOsError::Retrieval(format!(
                    "cost event {cost_event_id} not found"
                )));
            }
            Ok(())
        })
    }

    pub fn count(&self) -> Result<u64, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row("SELECT count(*) FROM cost_reconciliations", [], |r| {
                r.get::<_, i64>(0)
            })
            .map(|n| n as u64)
            .map_err(e)
        })
    }
}

/// Enqueues a document refresh event (RFC-004 v1.1 10.1: refresh the existing
/// source rather than deleting knowledge or re-buying a full search).
pub fn enqueue_refresh(
    store: &SqliteStore,
    url: &str,
    workspace_id: &str,
) -> Result<(), SearchOsError> {
    let payload = serde_json::json!({ "url": url, "workspace_id": workspace_id }).to_string();
    store.with_conn(|c| {
        c.execute(
            "INSERT INTO index_outbox (id, entity_type, entity_id, operation, status, attempts, payload, created_at)
             VALUES (?1, 'document', ?2, 'refresh', 'pending', 0, ?3, ?4)",
            params![searchos_domain::new_id("out"), url, payload, now()],
        )
        .map(|_| ())
        .map_err(e)
    })
}

#[cfg(test)]
mod completion_tests {
    use super::*;

    fn store() -> SqliteStore {
        crate::store::SqliteStore::open_in_memory().unwrap()
    }

    #[test]
    fn idempotency_replay_is_atomic() {
        let s = store();
        let r = IdempotencyRepo::new(&s);
        assert!(r.begin("req_1", "task_1", "ws").unwrap());
        assert!(!r.begin("req_1", "task_2", "ws").unwrap());
        assert!(r.get("req_1").unwrap().unwrap().response_json.is_none());
        r.complete("req_1", "{\"status\":\"completed\"}").unwrap();
        let rec = r.get("req_1").unwrap().unwrap();
        assert_eq!(rec.status, "completed");
        assert_eq!(rec.task_id, "task_1");
        assert!(rec.response_json.unwrap().contains("completed"));
    }

    #[test]
    fn reservation_settles_and_exceeds() {
        let s = store();
        let r = BudgetRepo::new(&s);
        r.reserve("r1", "t1", Some("tr1"), 10_000, "USD").unwrap();
        r.settle("t1", 4_000).unwrap();
        let rows = r.list(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].status, "settled");
        assert_eq!(rows[0].spent_cost_micros, 4_000);
        assert_eq!(rows[0].estimated_cost_micros, 10_000);
        r.reserve("r2", "t2", None, 5_000, "USD").unwrap();
        r.mark_exceeded("t2").unwrap();
        assert_eq!(r.list(10).unwrap()[0].status, "exceeded");
    }

    #[test]
    fn reconciliation_requires_existing_event() {
        let s = store();
        let r = ReconciliationRepo::new(&s);
        assert!(r.record("missing", 100, "provider_billing", "USD").is_err());
    }

    #[test]
    fn refresh_events_are_outbox_rows() {
        let s = store();
        enqueue_refresh(&s, "https://example.com/a", "default").unwrap();
        let rows = super::OutboxRepo::new(&s).pending(10).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].operation, "refresh");
        assert!(rows[0].payload.as_deref().unwrap().contains("example.com"));
    }
}

// ---------------------------------------------------------------------------
// RFC-027 / RFC-028 Analytics Event Store and Read Model repository.
// ---------------------------------------------------------------------------

/// Aggregated Dashboard Read Models. The HTTP layer only reads these rows,
/// never the operational tables directly.
pub struct AnalyticsRepo<'a> {
    store: &'a SqliteStore,
}

impl<'a> AnalyticsRepo<'a> {
    pub fn new(store: &'a SqliteStore) -> Self {
        Self { store }
    }

    /// Append-only event ledger (RFC-027 3).
    #[allow(clippy::too_many_arguments)]
    pub fn record_event(
        &self,
        event_id: &str,
        event_type: &str,
        workspace_id: &str,
        request_id: Option<&str>,
        execution_id: Option<&str>,
        source_module: &str,
        correlation_id: Option<&str>,
        payload_json: &str,
    ) -> Result<(), SearchOsError> {
        let now = chrono::Utc::now().to_rfc3339();
        self.store.with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO analytics_events (event_id, event_type, schema_version, occurred_at, recorded_at, workspace_id, request_id, execution_id, source_module, correlation_id, payload_json)
                 VALUES (?1, ?2, 1, ?3, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
                params![
                    event_id,
                    event_type,
                    now,
                    workspace_id,
                    request_id,
                    execution_id,
                    source_module,
                    correlation_id,
                    payload_json
                ],
            )
            .map(|_| ())
            .map_err(e)
        })
    }

    /// Rebuilds all Dashboard Read Models from operational records.
    /// This is the Analytics Aggregation Engine for the MVP.
    pub fn rebuild(&self, baseline_per_query_micros: i64) -> Result<(), SearchOsError> {
        const VERSION: &str = "1";
        let now = chrono::Utc::now().to_rfc3339();
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.store.with_conn(|c| {
            let scalar = |sql: &str| -> Result<i64, SearchOsError> {
                c.query_row(sql, [], |r| r.get(0))
                    .map_err(|e| SearchOsError::Storage(e.to_string()))
            };
            let put = |key: &str, bucket: &str, value: &serde_json::Value| -> Result<(), SearchOsError> {
                c.execute(
                    "INSERT OR REPLACE INTO analytics_read_models (model_key, workspace_id, bucket_start, payload_json, aggregation_version, updated_at)
                     VALUES (?1, '*', ?2, ?3, ?4, ?5)",
                    params![key, bucket, value.to_string(), VERSION, now],
                )
                .map(|_| ())
                .map_err(e)
            };

            let total = scalar("SELECT count(*) FROM search_tasks")?;
            let completed = scalar("SELECT count(*) FROM search_tasks WHERE status = 'completed'")?;
            let failed = scalar("SELECT count(*) FROM search_tasks WHERE status = 'failed'")?;
            let partial = scalar("SELECT count(*) FROM search_tasks WHERE status = 'partial'")?;
            // Local resolution is a quality-valid local answer, not merely a
            // decision path. An insufficient local-only task must not inflate
            // this KPI (RFC-026 6.1).
            let local = scalar(
                "SELECT count(*) FROM decision_traces dt
                 WHERE dt.selected_path IN ('local','local_with_refresh')
                   AND EXISTS (SELECT 1 FROM audit_events ae
                               WHERE ae.action = 'task_passed' AND ae.target = dt.task_id)",
            )?;
            let external = scalar(
                "SELECT count(*) FROM decision_traces WHERE selected_path IN ('hybrid','external')",
            )?;
            let quality_passed = scalar(
                "SELECT count(DISTINCT target) FROM audit_events WHERE action = 'task_passed'",
            )?;
            let cost_estimated = scalar("SELECT COALESCE(sum(estimated_cost_micros),0) FROM cost_events")?;
            let cost_actual_rows = scalar("SELECT count(*) FROM cost_events WHERE actual_cost_micros IS NOT NULL")?;
            let cost_actual = scalar("SELECT COALESCE(sum(actual_cost_micros),0) FROM cost_events")?;
            let documents = scalar("SELECT count(*) FROM documents WHERE status != 'deleted'")?;
            let chunks = scalar("SELECT count(*) FROM document_chunks")?;
            let embeddings = scalar("SELECT count(*) FROM embeddings")?;
            let baseline = total.saturating_mul(baseline_per_query_micros);
            let savings = baseline - cost_estimated;
            let rate = |part: i64, whole: i64| -> f64 {
                if whole > 0 { part as f64 / whole as f64 } else { 0.0 }
            };

            let overview = serde_json::json!({
                "searches": { "total": total, "completed": completed, "failed": failed, "partial": partial },
                "local_resolution": { "count": local, "rate": rate(local, total), "denominator": total,
                    "definition": "quality-valid tasks served by local knowledge / all tasks", "drilldown": "/search?filter=local" },
                "external_search": { "count": external, "rate": rate(external, total), "denominator": total,
                    "definition": "tasks with an external acquisition path / all tasks", "drilldown": "/search?filter=external" },
                "quality": { "passed": quality_passed, "partial": partial, "failed": failed,
                    "pass_rate": rate(quality_passed, total), "denominator": total,
                    "definition": "quality-gate passing tasks / all tasks", "drilldown": "/quality" },
                "cost": { "estimated_micros": cost_estimated,
                    "actual_micros": if cost_actual_rows > 0 { Some(cost_actual) } else { None },
                    "estimated_savings_micros": savings, "baseline_micros": baseline, "currency": "USD",
                    "is_estimate": cost_actual_rows == 0,
                    "definition": "baseline minus full SearchOS cost; estimates are not invoices",
                    "drilldown": "/cost" },
                "knowledge": { "documents": documents, "chunks": chunks, "embeddings": embeddings,
                    "definition": "active document / chunk / embedding rows", "drilldown": "/knowledge" },
                "evidence_reuse_rate": rate(local, total),
                "data_freshness": if total > 0 { "complete" } else { "empty" },
                "aggregation_version": VERSION,
                "metric_definitions_version": "1",
            });
            put("overview", &today, &overview)?;

            // Search activity time series. One Read Model row per day.
            let mut stmt = c.prepare(
                "SELECT substr(st.started_at,1,10) AS day, count(*),
                        sum(CASE WHEN dt.selected_path IN ('local','local_with_refresh') THEN 1 ELSE 0 END),
                        sum(CASE WHEN dt.selected_path IN ('hybrid','external') THEN 1 ELSE 0 END),
                        sum(CASE WHEN st.status = 'failed' THEN 1 ELSE 0 END)
                 FROM search_tasks st LEFT JOIN decision_traces dt ON dt.task_id = st.id
                 GROUP BY day ORDER BY day",
            ).map_err(e)?;
            let rows = stmt.query_map([], |r| Ok((
                r.get::<_, String>(0)?, r.get::<_, i64>(1)?,
                r.get::<_, Option<i64>>(2)?.unwrap_or(0),
                r.get::<_, Option<i64>>(3)?.unwrap_or(0),
                r.get::<_, Option<i64>>(4)?.unwrap_or(0),
            ))).map_err(e)?;
            let mut series: Vec<(String, i64, i64, i64, i64)> = Vec::new();
            for row in rows { series.push(row.map_err(e)?); }
            for (day, n, l, x, f) in series {
                put("search_timeseries", &day, &serde_json::json!({
                    "date": day, "total": n, "local": l, "external": x, "failed": f,
                }))?;
            }

            // Strategy distribution.
            let mut stmt = c.prepare(
                "SELECT COALESCE(final_strategy,'unknown'), count(*) FROM decision_traces GROUP BY 1",
            ).map_err(e)?;
            let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(e)?;
            let mut strategies = Vec::new();
            for row in rows { strategies.push(row.map_err(e)?); }
            put("search_strategies", &today, &serde_json::json!({ "items": strategies }))?;

            // Cost breakdown by four dimensions.
            let breakdown = |sql: &str| -> Result<Vec<(String, i64, i64)>, SearchOsError> {
                let mut st = c.prepare(sql).map_err(e)?;
                let rs = st.query_map([], |r| Ok((
                    r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?,
                ))).map_err(e)?;
                let mut out = Vec::new();
                for row in rs { out.push(row.map_err(e)?); }
                Ok(out)
            };
            put("cost_breakdown", &today, &serde_json::json!({
                "by_operation": breakdown("SELECT operation, count(*), COALESCE(sum(estimated_cost_micros),0) FROM cost_events GROUP BY operation")?,
                "by_provider": breakdown("SELECT COALESCE(provider_id,'local'), count(*), COALESCE(sum(estimated_cost_micros),0) FROM cost_events GROUP BY 1")?,
                "by_workspace": breakdown("SELECT workspace_id, count(*), COALESCE(sum(estimated_cost_micros),0) FROM cost_events GROUP BY workspace_id")?,
                "by_agent": breakdown("SELECT COALESCE(agent_id,'unattributed'), count(*), COALESCE(sum(estimated_cost_micros),0) FROM cost_events GROUP BY 1")?,
                "is_estimate": cost_actual_rows == 0,
            }))?;

            // Quality dimensions and failure classes.
            let dim = |sql: &str| -> Result<Vec<(String, i64)>, SearchOsError> {
                let mut st = c.prepare(sql).map_err(e)?;
                let rs = st.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(e)?;
                let mut out = Vec::new();
                for row in rs { out.push(row.map_err(e)?); }
                Ok(out)
            };
            put("quality_dimensions", &today, &serde_json::json!({
                "freshness": dim("SELECT COALESCE(freshness_status,'unknown'), count(*) FROM decision_traces GROUP BY 1")?,
                "coverage": dim("SELECT COALESCE(coverage_status,'unknown'), count(*) FROM decision_traces GROUP BY 1")?,
                "source": dim("SELECT COALESCE(source_status,'unknown'), count(*) FROM decision_traces GROUP BY 1")?,
                "failures": dim("SELECT COALESCE(rejection_reasons_json,'none'), count(*) FROM decision_traces GROUP BY 1")?,
            }))?;

            // Knowledge growth and freshness buckets.
            let mut stmt = c.prepare(
                "SELECT substr(created_at,1,10) AS day, count(*) FROM documents GROUP BY day ORDER BY day",
            ).map_err(e)?;
            let rs = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))).map_err(e)?;
            let mut growth = Vec::new();
            for row in rs { growth.push(row.map_err(e)?); }
            put("knowledge_growth", &today, &serde_json::json!({ "items": growth, "chunks": chunks, "embeddings": embeddings }))?;
            put("knowledge_freshness", &today, &serde_json::json!({
                "fresh": null, "near_expiry": null, "stale": null, "expired": null, "unknown": null,
                "reason": "document-level freshness classification is not instrumented in the MVP; missing values are not zero",
            }))?;

            // Provider and crawler summaries.
            put("providers", &today, &serde_json::json!({
                "items": breakdown("SELECT COALESCE(provider_id,'local'), count(*), COALESCE(sum(estimated_cost_micros),0) FROM cost_events GROUP BY 1")?,
                "health": "see /cost/providers",
            }))?;
            let crawl_total = scalar("SELECT count(*) FROM crawler_runs")?;
            let crawl_ok = scalar("SELECT count(*) FROM crawler_runs WHERE status = 'success'")?;
            let crawl_failed = scalar("SELECT count(*) FROM crawler_runs WHERE status != 'success'")?;
            put("crawlers_summary", &today, &serde_json::json!({
                "total": crawl_total, "successful": crawl_ok, "failed": crawl_failed,
                "extensions": null, "reason": "extension health comes from the crawler registry, not from a fake zero",
                "drilldown": "/crawlers",
            }))?;

            // Infrastructure: only measured values; unavailable dimensions are null.
            let page_count = scalar("PRAGMA page_count")?;
            let page_size = scalar("PRAGMA page_size")?;
            put("infrastructure_summary", &today, &serde_json::json!({
                "sqlite_bytes": page_count.saturating_mul(page_size),
                "index_chunks": chunks,
                "cpu": null, "gpu": null, "memory": null, "network": null,
                "reason": "process resource sampling is not instrumented in the MVP; null is not zero",
            }))?;

            // Recent search requests for the data table.
            let mut stmt = c.prepare(
                "SELECT st.id, st.query, st.mode, st.status, st.started_at,
                        COALESCE(dt.selected_path,'unknown'), COALESCE(dt.final_strategy,'unknown')
                 FROM search_tasks st LEFT JOIN decision_traces dt ON dt.task_id = st.id
                 ORDER BY st.started_at DESC LIMIT 200",
            ).map_err(e)?;
            let rs = stmt.query_map([], |r| Ok(serde_json::json!({
                "request_id": r.get::<_, String>(0)?,
                "query": r.get::<_, String>(1)?,
                "mode": r.get::<_, String>(2)?,
                "status": r.get::<_, String>(3)?,
                "created_at": r.get::<_, String>(4)?,
                "strategy": r.get::<_, String>(5)?,
                "final_strategy": r.get::<_, String>(6)?,
            }))).map_err(e)?;
            let mut requests = Vec::new();
            for row in rs { requests.push(row.map_err(e)?); }
            put("search_requests", &today, &serde_json::json!({ "items": requests, "limit": 200 }))?;

            Ok(())
        })
    }

    /// Reads one aggregate Read Model payload.
    pub fn read_model(&self, key: &str) -> Result<Option<serde_json::Value>, SearchOsError> {
        self.store.with_conn(|c| {
            let row: Option<String> = c.query_row(
                "SELECT payload_json FROM analytics_read_models WHERE model_key = ?1 AND workspace_id = '*' ORDER BY bucket_start DESC LIMIT 1",
                params![key], |r| r.get(0),
            ).optional().map_err(e)?;
            Ok(row.and_then(|s| serde_json::from_str(&s).ok()))
        })
    }

    /// Reads a time-series Read Model as ordered buckets.
    pub fn read_series(
        &self,
        key: &str,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT payload_json FROM analytics_read_models WHERE model_key = ?1 AND workspace_id = '*' ORDER BY bucket_start ASC LIMIT ?2",
            ).map_err(e)?;
            let rows = stmt.query_map(params![key, limit.clamp(1, 500)], |r| r.get::<_, String>(0)).map_err(e)?;
            let mut out = Vec::new();
            for row in rows {
                let s = row.map_err(e)?;
                if let Ok(v) = serde_json::from_str(&s) { out.push(v); }
            }
            Ok(out)
        })
    }

    pub fn events_since(
        &self,
        cursor: i64,
        limit: u32,
    ) -> Result<Vec<serde_json::Value>, SearchOsError> {
        self.store.with_conn(|c| {
            let mut stmt = c.prepare(
                "SELECT id, event_id, event_type, occurred_at, workspace_id, request_id, payload_json
                 FROM analytics_events WHERE id > ?1 ORDER BY id ASC LIMIT ?2",
            ).map_err(e)?;
            let rows = stmt.query_map(params![cursor, limit.clamp(1, 100)], |r| Ok(serde_json::json!({
                "id": r.get::<_, i64>(0)?,
                "event_id": r.get::<_, String>(1)?,
                "event_type": r.get::<_, String>(2)?,
                "occurred_at": r.get::<_, String>(3)?,
                "workspace_id": r.get::<_, String>(4)?,
                "request_id": r.get::<_, Option<String>>(5)?,
                "payload": r.get::<_, String>(6)?,
            }))).map_err(e)?;
            let mut out = Vec::new();
            for row in rows { out.push(row.map_err(e)?); }
            Ok(out)
        })
    }

    pub fn event_count(&self) -> Result<i64, SearchOsError> {
        self.store.with_conn(|c| {
            c.query_row("SELECT count(*) FROM analytics_events", [], |r| r.get(0))
                .map_err(e)
        })
    }
}

#[cfg(test)]
mod analytics_tests {
    use super::*;

    fn store() -> SqliteStore {
        crate::store::SqliteStore::open_in_memory().unwrap()
    }

    #[test]
    fn event_ledger_is_append_only_and_read_model_rebuilds() {
        let s = store();
        let repo = AnalyticsRepo::new(&s);
        repo.record_event(
            "evt_1",
            "search.request.completed",
            "default",
            Some("req_1"),
            Some("task_1"),
            "search_service",
            None,
            "{\"status\":\"completed\"}",
        )
        .unwrap();
        repo.record_event(
            "evt_1",
            "search.request.completed",
            "default",
            Some("req_1"),
            Some("task_1"),
            "search_service",
            None,
            "{\"status\":\"completed\"}",
        )
        .unwrap();
        assert_eq!(repo.event_count().unwrap(), 1, "event_id is unique");
        repo.rebuild(5_000).unwrap();
        let overview = repo.read_model("overview").unwrap().unwrap();
        assert_eq!(overview["searches"]["total"], 0);
        assert_eq!(overview["cost"]["baseline_micros"], 0);
        assert_eq!(overview["aggregation_version"], "1");
        assert!(repo
            .read_series("search_timeseries", 30)
            .unwrap()
            .is_empty());
        assert_eq!(repo.events_since(0, 10).unwrap().len(), 1);
    }
}

impl<'a> AnalyticsRepo<'a> {
    /// Overwrites one global Read Model row (aggregation engine only).
    pub fn write_model(&self, key: &str, value: &serde_json::Value) -> Result<(), SearchOsError> {
        let today = chrono::Utc::now().format("%Y-%m-%d").to_string();
        self.store.with_conn(|c| {
            c.execute(
                "INSERT OR REPLACE INTO analytics_read_models (model_key, workspace_id, bucket_start, payload_json, aggregation_version, updated_at)
                 VALUES (?1, '*', ?2, ?3, '1', ?4)",
                params![key, today, value.to_string(), now()],
            )
            .map(|_| ())
            .map_err(e)
        })
    }
}
