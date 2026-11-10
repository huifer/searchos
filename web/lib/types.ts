// Wire types mirror the Rust gateway JSON. Fields are intentionally optional
// at the edges: a gateway running an older build must not crash the console.

export type HealthResponse = {
  status?: string;
  version?: string;
  index?: { tantivy_chunks?: number; sqlite_chunks?: number; consistent?: boolean };
  providers?: string[];
};

export type MetricsSnapshot = {
  search_requests?: number;
  local_resolution_rate?: number;
  external_search_rate?: number;
  cache_hit_rate?: number;
  semantic_reuse_rate?: number;
  singleflight_reuse_rate?: number;
  search_latency?: { p50?: number; p95?: number; p99?: number };
  quality_pass_count?: number;
  quality_pass_rate?: number;
  coverage_pass_rate?: number;
  freshness_violation_rate?: number;
  conflict_count?: number;
  cost_micros_total?: number;
  provider_failure_count?: number;
  crawler_success_rate?: number;
  index_outbox_processed?: number;
  index_outbox_dead?: number;
  status_counts?: Record<string, number>;
  external_avoidance_rate?: number;
  valid_local_resolution_rate?: number;
  cost_per_quality_passing_answer_micros?: number;
};

export type ReuseCostViews = {
  original_acquisition_micros?: number;
  reuse_execution_micros?: number;
  amortized_micros?: number;
  marginal_micros?: number;
};

export type SavingsReport = {
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
  reuse_views?: ReuseCostViews;
  is_estimate?: boolean;
};

export type SystemStatus = {
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
};

export type SearchResultItem = {
  title?: string | null;
  url: string;
  content: string;
  score: number;
  published_at?: string | null;
  source: string;
};

export type Evidence = {
  url: string;
  title?: string | null;
  snippet?: string | null;
  source_reliability?: string;
  provenance?: { origin?: string; source_url?: string | null };
};

export type SearchResponse = {
  query: string;
  decision?: { path?: string; reason?: string };
  results: SearchResultItem[];
  quality?: {
    status?: string;
    freshness?: string;
    coverage?: string;
    score?: number;
    failed_constraints?: string[];
  };
  cost?: { estimated?: number; actual?: number | null; currency?: string };
  provenance?: Evidence[];
  trace_id?: string;
  status?: string;
  warnings?: string[];
  task_id?: string | null;
  replayed?: boolean;
};

export type StageTrace = {
  stage: string;
  status: string;
  started_at_ms?: number;
  duration_ms?: number;
  input_summary?: string;
  output_summary?: string;
  reasons?: string[];
};

export type DecisionTrace = {
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
};

export type TraceList = { traces: DecisionTrace[] };
export type Profile = {
  id: string;
  name?: string;
  version?: string;
  freshness?: string;
  source?: string;
  coverage?: string;
  default_mode?: string;
  content_category?: string;
};
export type ProfileList = { profiles: Profile[] };

export type CostSummary = {
  total_estimated?: { amount_micros?: number; currency?: string };
  total_actual?: { amount_micros?: number; currency?: string } | null;
  event_count?: number;
  by_operation?: [string, { amount_micros?: number }][];
  by_provider?: [string, { amount_micros?: number }][];
  by_workspace?: [string, { amount_micros?: number }][];
  by_agent?: [string, { amount_micros?: number }][];
  quality_passing_queries?: number;
  cost_per_successful_query?: { amount_micros?: number } | null;
};

export type Reservation = {
  id: string;
  task_id: string;
  trace_id?: string | null;
  estimated_cost_micros?: number;
  spent_cost_micros?: number;
  currency?: string;
  status?: string;
  created_at?: string;
  settled_at?: string | null;
};
export type ReservationList = { reservations: Reservation[] };

export type DocumentRecord = {
  id: string;
  canonical_url: string;
  content_type?: string;
  language?: string | null;
  status?: string;
  created_at?: string;
};

/** Console-facing document summary: lifecycle status + version / chunk counts. */
export type DocumentSummary = {
  id: string;
  source_id?: string;
  canonical_url: string;
  current_version_id?: string | null;
  title?: string | null;
  content_type?: string;
  language?: string | null;
  source_type?: string;
  status?: string;
  workspace_id?: string;
  version_count?: number;
  chunk_count?: number;
  created_at?: string;
  updated_at?: string;
};
export type DocumentList = { documents: DocumentSummary[] };

/** One immutable content version of a document. */
export type DocumentVersion = {
  id: string;
  document_id: string;
  content_hash: string;
  raw_content_ref?: string;
  markdown_ref?: string | null;
  published_at?: string | null;
  fetched_at?: string;
  extractor_version?: string;
  crawler_version?: string | null;
};

export type DocumentDetail = {
  document?: DocumentRecord & Record<string, unknown>;
  summary?: DocumentSummary;
  current_version?: DocumentVersion | null;
  versions?: DocumentVersion[];
};

/** Result of one local ingest call (text / file / batch item). */
export type IngestedDocument = {
  document_id: string;
  version_id?: string;
  canonical_url?: string;
  source_type?: string;
  content_hash?: string;
  changed?: boolean;
  title?: string | null;
  chunk_count?: number;
  status?: string;
};

export type IngestFailure = { source_key?: string; error?: string };

export type IngestBatchResult = {
  job_id?: string;
  state?: string;
  total_items?: number;
  ok_items?: number;
  failed_items?: number;
  documents?: IngestedDocument[];
  failures?: IngestFailure[];
};

export type IngestJobItem = {
  id: string;
  job_id: string;
  source_key?: string;
  state?: string;
  document_id?: string | null;
  error_detail?: string | null;
};

export type IngestJob = {
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
};

export type MemoryHit = {
  entry?: { id?: string; title?: string; content?: string; url?: string | null };
  score?: number;
};
export type MemoryList = { results: MemoryHit[] };
