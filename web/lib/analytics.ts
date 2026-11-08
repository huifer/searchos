"use client";

import { useEffect, useRef } from "react";

import { useApi, withQuery } from "@/lib/client";

export type DashboardMeta = {
  generated_at?: string;
  time_range?: { from?: string | null; to?: string | null; timezone?: string };
  interval?: string;
  aggregation_version?: string;
  data_freshness?: string;
  workspace_scope?: string;
};

export type DashboardEnvelope<T> = { data?: T; meta?: DashboardMeta };

export type OverviewData = {
  searches?: { total?: number; completed?: number; failed?: number; partial?: number };
  local_resolution?: { count?: number; rate?: number; denominator?: number; definition?: string; drilldown?: string };
  external_search?: { count?: number; rate?: number; denominator?: number; definition?: string; drilldown?: string };
  quality?: { passed?: number; partial?: number; failed?: number; pass_rate?: number; denominator?: number; definition?: string; drilldown?: string };
  cost?: {
    estimated_micros?: number;
    actual_micros?: number | null;
    estimated_savings_micros?: number;
    baseline_micros?: number;
    currency?: string;
    is_estimate?: boolean;
    definition?: string;
    drilldown?: string;
  };
  knowledge?: { documents?: number; chunks?: number; embeddings?: number; definition?: string; drilldown?: string };
  evidence_reuse_rate?: number;
  data_freshness?: string;
  aggregation_version?: string;
  metric_definitions_version?: string;
};

export type TimeSeriesPoint = {
  date?: string;
  total?: number;
  local?: number;
  external?: number;
  failed?: number;
};

export type NamedCount = [string, number];
export type CostTriple = [string, number, number];

export type Dataset<T> = { items?: T[]; [key: string]: unknown };

export type SearchRequestRow = {
  request_id?: string;
  query?: string;
  mode?: string;
  status?: string;
  created_at?: string;
  strategy?: string;
  final_strategy?: string;
};

export function useOverview(params: Record<string, string | undefined> = {}, refreshMs = 15_000) {
  return useApi<DashboardEnvelope<OverviewData>>(
    withQuery("api/v1/dashboard/overview", params),
    refreshMs,
  );
}

export function useSearchTimeseries(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ items: TimeSeriesPoint[] }>>(
    withQuery("api/v1/dashboard/search/timeseries", params),
    30_000,
  );
}

export function useSearchStrategies(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ items: NamedCount[] }>>(
    withQuery("api/v1/dashboard/search/strategies", params),
    30_000,
  );
}

export function useSearchRequests(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ items: SearchRequestRow[]; limit?: number }>>(
    withQuery("api/v1/dashboard/search/requests", params),
    30_000,
  );
}

export function useCostBreakdown(params: Record<string, string | undefined> = {}) {
  return useApi<
    DashboardEnvelope<{
      by_operation?: CostTriple[];
      by_provider?: CostTriple[];
      by_workspace?: CostTriple[];
      by_agent?: CostTriple[];
      is_estimate?: boolean;
    }>
  >(withQuery("api/v1/dashboard/cost/breakdown", params), 30_000);
}

export function useQualityDimensions(params: Record<string, string | undefined> = {}) {
  return useApi<
    DashboardEnvelope<{
      freshness?: NamedCount[];
      coverage?: NamedCount[];
      source?: NamedCount[];
      failures?: NamedCount[];
    }>
  >(withQuery("api/v1/dashboard/quality/dimensions", params), 30_000);
}

export function useKnowledgeGrowth(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ items?: NamedCount[]; chunks?: number; embeddings?: number }>>(
    withQuery("api/v1/dashboard/knowledge/documents", params),
    60_000,
  );
}

export function useKnowledgeFreshness(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<Record<string, unknown>>>(
    withQuery("api/v1/dashboard/knowledge/freshness", params),
    60_000,
  );
}

export function useProviders(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ items?: CostTriple[]; health?: string }>>(
    withQuery("api/v1/dashboard/providers", params),
    60_000,
  );
}

export function useCrawlers(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ total?: number; successful?: number; failed?: number; reason?: string }>>(
    withQuery("api/v1/dashboard/crawlers/summary", params),
    60_000,
  );
}

export function useInfrastructure(params: Record<string, string | undefined> = {}) {
  return useApi<DashboardEnvelope<{ sqlite_bytes?: number; index_chunks?: number; reason?: string }>>(
    withQuery("api/v1/dashboard/infrastructure/summary", params),
    60_000,
  );
}

/** Realtime SSE is a refetch hint only; the Read Model remains the truth. */
export function useAnalyticsEvents(onEvent: () => void) {
  const handler = useRef(onEvent);
  handler.current = onEvent;
  useEffect(() => {
    const source = new EventSource("/api/searchos/api/v1/dashboard/events");
    const listener = () => handler.current();
    source.addEventListener("analytics.events", listener);
    return () => {
      source.removeEventListener("analytics.events", listener);
      source.close();
    };
  }, []);
}
