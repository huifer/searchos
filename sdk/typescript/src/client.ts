/**
 * SearchOS Gateway API — typed TypeScript client.
 *
 * Generated from `docs/openapi.yaml`. The contract is verified against the real
 * Axum route table by `openapi_spec_matches_the_route_table` in
 * `crates/searchos-interface-http`, so a method here cannot drift from the
 * gateway silently.
 *
 * Same paths are served unversioned and under `/v1`; pass `basePath: "/v1"` in
 * the client options to use the versioned mount.
 */

// ---------------------------------------------------------------------------
// Contract types (mirror docs/openapi.yaml components/schemas)
// ---------------------------------------------------------------------------

export type SearchMode = "local-only" | "local-first" | "hybrid" | "external-only" | "external-first";

export type SearchStatus =
  | "completed"
  | "completed_with_warnings"
  | "partial"
  | "insufficient_evidence"
  | "budget_exhausted"
  | "failed"
  | "cancelled";

export interface SearchRequest {
  query: string;
  mode?: SearchMode;
  profile?: string;
  workspace_id?: string;
  /** Idempotency key: replaying it never creates a second task, call or bill. */
  request_id?: string;
  max_results?: number;
  max_external_queries?: number;
  max_total_cost?: number;
  /** Bounded iterative retrieval rounds (total query/cost caps stay cumulative). */
  max_acquisition_rounds?: number;
  /** Agent session id; turns are recorded so a follow-up can target open gaps. */
  session_id?: string;
  /** Explicit follow-up requirements (replaces the derived decomposition). */
  focus_requirements?: string[];
  /** RFC-057: judge freshness as of this RFC3339 instant (historical view). */
  as_of?: string;
  /** RFC-076: downsize the external plan to fit the budget instead of rejecting. */
  cost_adaptive?: boolean;
}

export interface SearchResultItem {
  title?: string | null;
  url: string;
  content: string;
  score: number;
  published_at?: string | null;
  source: string;
  /** Query-matched passage with `<b>…</b>` markers, when available. */
  highlight?: string | null;
  /** RFC-058: stable entity id when a registered alias matched. */
  entity_id?: string;
}

/** RFC-038: a relation between two pieces of evidence. */
export type EvidenceRelation = "duplicate_of" | "same_content";

export interface EvidenceGraphEdge {
  from: string;
  to: string;
  relation: EvidenceRelation;
}

/** RFC-038: folds and syndication relations the merge observed. */
export interface EvidenceGraphBlock {
  nodes: string[];
  edges: EvidenceGraphEdge[];
}

/** RFC-071 v2: earlier workspace evidence carried into a continuation turn. */
export interface WorkspaceEvidenceItem {
  url: string;
  title?: string | null;
  source: string;
  score: number;
  trace_id: string;
  recorded_at: string;
}

export interface WorkspaceEvidenceBlock {
  items: WorkspaceEvidenceItem[];
  invalidated: number;
  considered: number;
}

/** RFC-036: extractive summary + keywords computed at ingest. */
export interface DocumentInsights {
  summary?: string | null;
  keywords?: string[];
}

/** RFC-087: what an ingest redaction pass removed. */
export interface RedactionHit {
  rule: string;
  count: number;
}

export interface RedactionReport {
  redacted: boolean;
  hits: RedactionHit[];
  rules_version: string;
}

export interface QualityReport {
  status?: "passed" | "partial" | "failed" | "conflict" | "unknown";
  freshness?: string;
  coverage?: string;
  score?: number;
  failed_constraints?: string[];
}

export interface CoverageGap {
  question: string;
  missing_kind: string;
  status?: string;
  priority?: string;
  strategy?: string;
  requirement_id: string;
}

export interface SearchResponse {
  query: string;
  status: SearchStatus;
  decision?: { path?: string; reason?: string };
  results: SearchResultItem[];
  quality?: QualityReport;
  cost?: { estimated?: number; actual?: number | null; currency?: string };
  provenance?: Record<string, unknown>[];
  warnings?: string[];
  trace_id?: string;
  task_id?: string | null;
  replayed?: boolean;
  /** Open sub-questions from the final quality gate; a search agent can use
   * `requirement_id` to issue a targeted follow-up query. */
  coverage_gaps?: CoverageGap[];
  /** Agent session this turn belongs to. */
  session_id?: string | null;
  /** Open requirements a follow-up turn should target. */
  focus_requirements?: string[];
  /** Tavily compatibility only: present when the request was routed through the compat layer. */
  unsupported_features?: string[];
  /** Tavily compatibility only: always null, SearchOS never generates answers. */
  answer?: string | null;
  response_time?: number;
}

export interface ExtractRequest {
  url: string;
  workspace_id?: string;
  /** When false only previews (no persistence). Defaults to true. */
  persist?: boolean;
}

/**
 * Tavily-shaped extract body. SearchOS extracts one URL per call: the first
 * is honored, the rest are returned in `failed_results` with an explicit reason.
 */
export interface TavilyExtractRequest {
  urls: string[];
  extract_depth?: "basic" | "advanced";
  format?: "markdown" | "text";
  include_images?: boolean;
  include_favicon?: boolean;
  workspace_id?: string;
}

export interface TavilyExtractResult {
  url: string;
  title?: string | null;
  /** Always null: SearchOS returns extracted text, not raw HTML. */
  raw_content?: string | null;
}

export interface TavilyExtractFailure {
  url: string;
  error: string;
}

export interface TavilyExtractResponse {
  results: TavilyExtractResult[];
  failed_results: TavilyExtractFailure[];
  unsupported_features: string[];
  response_time: number;
}

export interface IngestRequest {
  content: string;
  source_key?: string;
  title?: string;
  content_type?: string;
  language?: string;
  workspace_id?: string;
  metadata?: Record<string, unknown>;
}

export interface IngestBatchRequest {
  items?: IngestRequest[];
  directory?: {
    files: Array<{
      file_name: string;
      content: string;
      source_key?: string;
      title?: string;
      content_type?: string;
      language?: string;
    }>;
    recursive?: boolean;
    extensions?: string[];
  };
}

export interface IngestedDocument {
  document_id: string;
  version_id?: string;
  canonical_url?: string;
  source_type?: "web" | "local_text" | "local_file" | "local_directory" | "local_mixed";
  content_hash?: string;
  changed?: boolean;
  title?: string | null;
  chunk_count?: number;
  status?: string;
}

export interface IngestFailure {
  source_key?: string;
  error?: string;
}

export interface IngestBatchResult {
  job_id?: string;
  state?: "pending" | "running" | "completed" | "partial" | "failed";
  total_items?: number;
  ok_items?: number;
  failed_items?: number;
  documents?: IngestedDocument[];
  failures?: IngestFailure[];
}

export interface IngestJobItem {
  id: string;
  job_id: string;
  source_key?: string;
  state?: string;
  document_id?: string | null;
  error_detail?: string | null;
}

export interface IngestJob {
  job?: {
    id: string;
    workspace_id?: string;
    source_type?: string;
    state?: string;
    total_items?: number;
    ok_items?: number;
    failed_items?: number;
    error_detail?: string | null;
    created_at?: string;
    finished_at?: string | null;
  };
  items?: IngestJobItem[];
}

export type DocumentStatus = "discovered" | "fetched" | "normalized" | "indexed" | "deleted";

export interface DocumentSummary {
  id: string;
  source_id?: string;
  canonical_url: string;
  current_version_id?: string | null;
  title?: string | null;
  content_type?: string;
  language?: string | null;
  source_type?: string;
  status?: DocumentStatus;
  workspace_id?: string;
  version_count?: number;
  chunk_count?: number;
  created_at?: string;
  updated_at?: string;
}

export interface DocumentVersion {
  id: string;
  document_id: string;
  content_hash: string;
  raw_content_ref?: string;
  markdown_ref?: string | null;
  published_at?: string | null;
  fetched_at?: string;
  extractor_version?: string;
  crawler_version?: string | null;
}

export interface DocumentDetail {
  document?: Record<string, unknown>;
  summary?: DocumentSummary;
  current_version?: DocumentVersion | null;
  versions?: DocumentVersion[];
}

export interface MemoryHit {
  entry?: { id?: string; title?: string; content?: string; url?: string | null };
  score?: number;
}

export interface MemorySearchResult {
  results: MemoryHit[];
}

export interface SavingsReport {
  currency?: string;
  task_count?: number;
  baseline_micros?: number;
  searchos_micros?: number;
  savings_micros?: number;
  savings_rate?: number;
  quality_passing_tasks?: number;
  cost_per_quality_passing_answer_micros?: number | null;
  external_avoidance_rate?: number;
  valid_local_resolution_rate?: number;
  external_calls?: number;
  local_resolved_tasks?: number;
  reuse_views?: {
    original_acquisition_micros?: number;
    reuse_execution_micros?: number;
    amortized_micros?: number;
    marginal_micros?: number;
  };
  is_estimate?: boolean;
}

export interface StageTrace {
  stage: string;
  status: string;
  started_at_ms?: number;
  duration_ms?: number;
  input_summary?: string;
  output_summary?: string;
  reasons?: string[];
}

export interface DecisionTrace {
  trace_id?: string;
  task_id?: string;
  intents?: string[];
  profile_version?: string;
  local_candidates?: number;
  accepted_candidates?: number;
  freshness_status?: string;
  coverage_status?: string;
  source_status?: string;
  selected_path?: string;
  rejection_reasons?: string[];
  estimated_cost?: { amount_micros?: number; currency?: string };
  actual_cost?: { amount_micros?: number; currency?: string } | null;
  decision_at?: string;
  stages?: StageTrace[];
  final_strategy?: string | null;
  final_outcome?: string | null;
}

export interface SystemStatus {
  version?: string;
  api_version?: string;
  config_version?: number;
  schema_version?: number;
  database_ok?: boolean;
  index?: { tantivy_chunks?: number; sqlite_chunks?: number; consistent?: boolean };
  providers?: string[];
  crawlers?: number;
  extensions?: number;
  auth_required?: boolean;
  rate_limit_per_minute?: number;
}

export interface Health {
  status?: string;
  version?: string;
  index?: {
    fulltext_backend?: string;
    fulltext_chunks?: number;
    tantivy_chunks?: number;
    sqlite_chunks?: number;
    consistent?: boolean;
  };
  providers?: string[];
}

export interface DashboardMeta {
  generated_at?: string;
  time_range?: { from?: string | null; to?: string | null; timezone?: string };
  interval?: string;
  aggregation_version?: string;
  data_freshness?: "complete" | "fresh" | "live" | "stale" | "empty";
  query_mode?: "precomputed" | "sql";
  applied_filters?: string[];
  ignored_filters?: Array<{ filter?: string; reason?: string }>;
  refresh_skipped?: boolean;
  model_age_seconds?: number | null;
  workspace_scope?: string;
}

export type DashboardEnvelope<T> = { data?: T; meta?: DashboardMeta };

export interface DashboardQuery {
  workspace_id?: string;
  project_id?: string;
  agent_id?: string;
  provider_id?: string;
  search_mode?: string;
  quality_status?: string;
  from?: string;
  to?: string;
  interval?: "minute" | "hour" | "day" | "week" | "month";
  timezone?: "UTC";
  limit?: number;
  cursor?: string;
}

export interface ApiErrorShape {
  error?: { code?: string; message?: string };
}

export class SearchOSError extends Error {
  constructor(
    message: string,
    readonly status: number,
    readonly code?: string,
  ) {
    super(message);
    this.name = "SearchOSError";
  }
}

// ---------------------------------------------------------------------------
// Client
// ---------------------------------------------------------------------------

export interface SearchOSClientOptions {
  /** Gateway root, e.g. http://127.0.0.1:8787. Defaults to that. */
  baseUrl?: string;
  /** Use the versioned mount (`/v1`). */
  basePath?: "/v1" | "";
  /** Bearer token; omit when auth is disabled (local dev). */
  token?: string;
  /** Fetch implementation; defaults to global fetch. */
  fetch?: typeof fetch;
}

export class SearchOSClient {
  private readonly baseUrl: string;
  private readonly token?: string;
  private readonly fetchImpl: typeof fetch;

  constructor(options: SearchOSClientOptions = {}) {
    this.baseUrl = (options.baseUrl ?? "http://127.0.0.1:8787")
      .replace(/\/+$/, "")
      .concat(options.basePath ?? "");
    this.token = options.token;
    this.fetchImpl = options.fetch ?? globalThis.fetch;
  }

  // -- Search ---------------------------------------------------------------

  search(request: SearchRequest): Promise<SearchResponse> {
    return this.post("/search", request) as Promise<SearchResponse>;
  }

  // -- Knowledge ------------------------------------------------------------

  extract(request: ExtractRequest): Promise<Record<string, unknown>> {
    return this.post("/extract", request);
  }

  /** Tavily-shaped extract: `{ urls: [...] }`. One URL per call. */
  extractTavily(request: TavilyExtractRequest): Promise<TavilyExtractResponse> {
    return this.post("/extract", request) as Promise<TavilyExtractResponse>;
  }

  ingestText(request: IngestRequest): Promise<IngestedDocument> {
    return this.post("/ingest", request) as Promise<IngestedDocument>;
  }

  ingestFile(file: Blob, metadata: Omit<IngestRequest, "content"> = {}): Promise<IngestedDocument> {
    const form = new FormData();
    form.set("file", file);
    for (const [key, value] of Object.entries(metadata)) {
      if (value !== undefined) form.set(key, String(value));
    }
    // Let the runtime set the boundary-carrying content type.
    return this.postForm("/ingest/file", form) as Promise<IngestedDocument>;
  }

  ingestBatch(request: IngestBatchRequest): Promise<IngestBatchResult> {
    return this.post("/ingest/batch", request) as Promise<IngestBatchResult>;
  }

  getIngestJob(id: string): Promise<IngestJob> {
    return this.get(`/ingest/jobs/${encodeURIComponent(id)}`) as Promise<IngestJob>;
  }

  listDocuments(
    query: { workspace_id?: string; source_type?: string; status?: string; limit?: number } = {},
  ): Promise<{ documents: DocumentSummary[] }> {
    return this.get("/documents", query) as Promise<{ documents: DocumentSummary[] }>;
  }

  getDocument(id: string): Promise<DocumentDetail> {
    return this.get(`/documents/${encodeURIComponent(id)}`) as Promise<DocumentDetail>;
  }

  getDocumentVersions(id: string): Promise<{ document: DocumentSummary; versions: DocumentVersion[] }> {
    return this.get(`/documents/${encodeURIComponent(id)}/versions`) as Promise<{
      document: DocumentSummary;
      versions: DocumentVersion[];
    }>;
  }

  reindexDocument(
    id: string,
  ): Promise<{ document_id: string; reindexed: number; version_count: number; chunk_count: number }> {
    return this.post(`/documents/${encodeURIComponent(id)}/reindex`, {}) as Promise<{
      document_id: string;
      reindexed: number;
      version_count: number;
      chunk_count: number;
    }>;
  }

  deleteDocument(id: string): Promise<{ deleted: boolean; document_id: string }> {
    return this.delete(`/documents/${encodeURIComponent(id)}`) as Promise<{
      deleted: boolean;
      document_id: string;
    }>;
  }

  searchDocuments(request: {
    query: string;
    workspace_id?: string;
    limit?: number;
  }): Promise<{ documents: DocumentSummary[] }> {
    return this.post("/documents/search", request) as Promise<{ documents: DocumentSummary[] }>;
  }

  // -- Memory ---------------------------------------------------------------

  searchMemory(request: { query: string; workspace_id?: string; top_k?: number }): Promise<MemorySearchResult> {
    return this.post("/memory/search", request) as Promise<MemorySearchResult>;
  }

  // -- Cost -----------------------------------------------------------------

  costSummary(): Promise<Record<string, unknown>> {
    return this.get("/cost/summary");
  }

  costProviders(): Promise<Record<string, unknown>> {
    return this.get("/cost/providers");
  }

  costSavings(): Promise<SavingsReport> {
    return this.get("/cost/savings") as Promise<SavingsReport>;
  }

  costReservations(): Promise<{ reservations: Record<string, unknown>[] }> {
    return this.get("/cost/reservations") as Promise<{ reservations: Record<string, unknown>[] }>;
  }

  reconcileCost(request: {
    cost_event_id: string;
    actual_cost_micros: number;
    source: string;
    currency?: string;
  }): Promise<void> {
    return this.post("/cost/reconcile", request) as Promise<void>;
  }

  // -- Observability --------------------------------------------------------

  statsSearch(): Promise<Record<string, unknown>> {
    return this.get("/stats/search");
  }

  statsSavings(): Promise<SavingsReport> {
    return this.get("/stats/savings") as Promise<SavingsReport>;
  }

  statsRetrieval(): Promise<Record<string, unknown>> {
    return this.get("/stats/retrieval");
  }

  getDecision(traceId: string): Promise<DecisionTrace> {
    return this.get(`/decisions/${encodeURIComponent(traceId)}`) as Promise<DecisionTrace>;
  }

  listTraces(limit = 20): Promise<{ traces: DecisionTrace[] }> {
    return this.get("/traces", { limit }) as Promise<{ traces: DecisionTrace[] }>;
  }

  systemStatus(): Promise<SystemStatus> {
    return this.get("/system/status") as Promise<SystemStatus>;
  }

  listProfiles(): Promise<{ profiles: Record<string, unknown>[] }> {
    return this.get("/profiles") as Promise<{ profiles: Record<string, unknown>[] }>;
  }

  getProfile(id: string): Promise<Record<string, unknown>> {
    return this.get(`/profiles/${encodeURIComponent(id)}`);
  }

  health(): Promise<Health> {
    return this.get("/health") as Promise<Health>;
  }

  /** Prometheus text exposition 0.0.4. Returns the raw text. */
  metrics(): Promise<string> {
    return this.get("/metrics", undefined, "text") as Promise<string>;
  }

  // -- Analytics (RFC-026/027/028) ------------------------------------------

  dashboardOverview(query: DashboardQuery = {}): Promise<DashboardEnvelope<Record<string, unknown>>> {
    return this.get("/api/v1/dashboard/overview", query);
  }

  dashboardDataset(
    path: string,
    query: DashboardQuery = {},
  ): Promise<DashboardEnvelope<Record<string, unknown>>> {
    return this.get(`/api/v1/dashboard/${path.replace(/^\/+/, "")}`, query);
  }

  dashboardExport(format: "json" | "csv" = "json"): Promise<unknown> {
    const request = this.get(
      `/api/v1/dashboard/export?format=${encodeURIComponent(format)}`,
      undefined,
      format === "csv" ? "text" : "json",
    );
    return request as Promise<unknown>;
  }

  /** SSE refetch hint. Returns the EventSource so the caller can close it. */
  dashboardEvents(onEvent: () => void, onError?: (error: unknown) => void): EventSource {
    const source = new EventSource(`${this.baseUrl}/api/v1/dashboard/events`);
    source.addEventListener("analytics.events", () => onEvent());
    if (onError) source.addEventListener("error", (event) => onError(event));
    return source;
  }

  /** Continue an agent session; reuses earlier evidence when still valid. */
  searchFollowUp(request: SearchRequest): Promise<SearchResponse> {
    return this.post("/search/follow-up", request) as Promise<SearchResponse>;
  }

  // -- Evidence acquisition / reuse (RFC-031) --------------------------------

  evidenceAcquisitions(): Promise<Record<string, unknown>> {
    return this.get("/evidence/acquisitions");
  }

  evidenceAcquisition(id: string): Promise<Record<string, unknown>> {
    return this.get(`/evidence/acquisitions/${encodeURIComponent(id)}`);
  }

  evidenceReuse(): Promise<Record<string, unknown>> {
    return this.get("/evidence/reuse");
  }

  /** Raw recorded decision trace at the canonical path. */
  trace(traceId: string): Promise<DecisionTrace> {
    return this.get(`/trace/${encodeURIComponent(traceId)}`) as Promise<DecisionTrace>;
  }

  // -- Search Watch (RFC-035) -----------------------------------------------

  /** Run every applicable check and append one snapshot. */
  watchRun(): Promise<Record<string, unknown>> {
    return this.post("/watch/run", {});
  }

  /** Append-only snapshot history, newest first. */
  watchSnapshots(limit = 20): Promise<{ snapshots: Record<string, unknown>[] }> {
    return this.get("/watch/snapshots", { limit }) as Promise<{
      snapshots: Record<string, unknown>[];
    }>;
  }

  // -- Planning (RFC-031/004) ------------------------------------------------

  /** Plan-only: what WOULD be searched, without buying anything. */
  searchPlan(request: SearchRequest): Promise<Record<string, unknown>> {
    return this.post("/search/plan", request) as Promise<Record<string, unknown>>;
  }

  // -- Research workspace (RFC-071) ------------------------------------------

  /** Bounded deep search; each round is a full search recorded as a turn. */
  researchRun(body: {
    query: string;
    profile?: string;
    mode?: SearchMode;
    workspace?: string;
    session_id?: string;
    max_rounds?: number;
    max_total_cost_micros?: number;
    max_external_queries?: number;
  }): Promise<Record<string, unknown>> {
    return this.post("/research/run", body);
  }

  /** Inspect the research workspace (turns + accumulated evidence). */
  getResearchWorkspace(sessionId: string): Promise<Record<string, unknown>> {
    return this.get(`/research/${encodeURIComponent(sessionId)}`);
  }

  // -- Entities (RFC-058) ----------------------------------------------------

  listEntities(): Promise<Record<string, unknown>> {
    return this.get("/entities");
  }

  registerEntity(body: {
    canonical_name: string;
    aliases?: string[];
    source?: "operator" | "heuristic";
  }): Promise<Record<string, unknown>> {
    return this.post("/entities", body);
  }

  resolveEntity(text: string): Promise<Record<string, unknown>> {
    return this.post("/entities/resolve", { text });
  }

  // -- Collections (RFC-030) -------------------------------------------------

  listCollections(workspaceId?: string): Promise<Record<string, unknown>> {
    return this.get("/collections", workspaceId ? { workspace_id: workspaceId } : undefined);
  }

  createCollection(body: {
    name: string;
    workspace_id?: string;
    description?: string;
  }): Promise<Record<string, unknown>> {
    return this.post("/collections", body);
  }

  addCollectionDocument(collectionId: string, documentId: string): Promise<Record<string, unknown>> {
    return this.post(
      `/collections/${encodeURIComponent(collectionId)}/documents`,
      { document_id: documentId },
    );
  }

  removeCollectionDocument(collectionId: string, documentId: string): Promise<void> {
    return this.delete(
      `/collections/${encodeURIComponent(collectionId)}/documents/${encodeURIComponent(documentId)}`,
    ) as Promise<void>;
  }

  collectionDocuments(collectionId: string): Promise<{ documents: string[] }> {
    return this.get(`/collections/${encodeURIComponent(collectionId)}/documents`) as Promise<{
      documents: string[];
    }>;
  }

  // -- Replay / simulation / evaluation (RFC-040/075/086) --------------------

  /** Offline replay of a recorded decision; optional what-if policy. */
  replayTrace(
    traceId: string,
    body: { policy?: Record<string, unknown> } = {},
  ): Promise<Record<string, unknown>> {
    return this.post(`/replay/${encodeURIComponent(traceId)}`, body);
  }

  /** Batch evaluation over recent traces. */
  evaluateReplays(body: {
    limit?: number;
    workspace?: string;
    policy?: Record<string, unknown>;
  } = {}): Promise<Record<string, unknown>> {
    return this.post("/evaluation/replay", body);
  }

  // -- Deletion audit (RFC-087) ----------------------------------------------

  listDeletions(
    query: { document_id?: string; workspace_id?: string; limit?: number } = {},
  ): Promise<{ deletions: Record<string, unknown>[] }> {
    return this.get("/deletions", query) as Promise<{ deletions: Record<string, unknown>[] }>;
  }

  // -- Policies (RFC-034) ----------------------------------------------------

  listPolicies(): Promise<{ policies: unknown[]; live: Record<string, unknown> }> {
    return this.get("/policies") as Promise<{ policies: unknown[]; live: Record<string, unknown> }>;
  }

  policyVersions(kind: "quality" | "freshness" | "source"): Promise<{
    kind: string;
    versions: Record<string, unknown>[];
  }> {
    return this.get(`/policies/${encodeURIComponent(kind)}/versions`);
  }

  submitPolicy(
    kind: "quality" | "freshness" | "source",
    body: { policy: Record<string, unknown>; notes?: string },
  ): Promise<Record<string, unknown>> {
    return this.put(`/policies/${encodeURIComponent(kind)}`, body) as Promise<
      Record<string, unknown>
    >;
  }

  rollbackPolicy(
    kind: "quality" | "freshness" | "source",
    version: number,
  ): Promise<Record<string, unknown>> {
    return this.post(`/policies/${encodeURIComponent(kind)}/rollback`, { version });
  }

  // -- Memory lifecycle (RFC-070) --------------------------------------------

  memoryLifecycle(apply = false): Promise<Record<string, unknown>> {
    return this.post("/memory/lifecycle", { apply });
  }

  // -- Federated source catalog (RFC-063) ------------------------------------

  sources(): Promise<Record<string, unknown>> {
    return this.get("/sources");
  }

  // -- Transport ------------------------------------------------------------


  private put<T = unknown>(path: string, body: unknown): Promise<T> {
    return this.request("PUT", path, { body: JSON.stringify(body) }) as Promise<T>;
  }

  private async request<T>(
    method: string,
    path: string,
    init: { body?: BodyInit; headers?: Record<string, string>; parse?: "json" | "text" },
  ): Promise<T> {
    const headers: Record<string, string> = { ...(init.headers ?? {}) };
    if (this.token) headers.authorization = `Bearer ${this.token}`;
    const response = await this.fetchImpl(`${this.baseUrl}${path}`, {
      method,
      headers,
      body: init.body,
      cache: "no-store",
    });
    const contentType = response.headers.get("content-type") ?? "";
    const parse = init.parse ?? (contentType.includes("json") ? "json" : "text");
    const text = await response.text();
    if (!response.ok) {
      let message = text;
      let code: string | undefined;
      try {
        const parsed = JSON.parse(text) as ApiErrorShape;
        message = parsed.error?.message ?? parsed.error?.code ?? text;
        code = parsed.error?.code;
      } catch {
        // Keep the raw gateway response.
      }
      throw new SearchOSError(`${response.status}: ${message}`, response.status, code);
    }
    if (parse === "text") return text as unknown as T;
    try {
      return JSON.parse(text) as T;
    } catch {
      throw new SearchOSError("gateway returned a non-JSON response", response.status);
    }
  }

  private get<T>(path: string, query?: object, parse: "json" | "text" = "json"): Promise<T> {
    const search = new URLSearchParams();
    for (const [key, value] of Object.entries(query ?? {})) {
      if (value !== undefined && value !== null) search.set(key, String(value));
    }
    const suffix = search.toString();
    return this.request<T>("GET", `${path}${suffix ? `?${suffix}` : ""}`, { parse });
  }

  private post<T>(path: string, body: unknown): Promise<T> {
    return this.request<T>("POST", path, {
      body: JSON.stringify(body ?? {}),
      headers: { "content-type": "application/json" },
    });
  }

  private postForm<T>(path: string, form: FormData): Promise<T> {
    // No explicit content type: the runtime sets the multipart boundary.
    return this.request<T>("POST", path, { body: form });
  }

  private delete<T>(path: string): Promise<T> {
    return this.request<T>("DELETE", path, {});
  }
}

export default SearchOSClient;
