"use client";

import { useCallback } from "react";
import { useQuery } from "@tanstack/react-query";

export function apiPath(path: string): string {
  return `/api/searchos/${path.replace(/^\/+/, "")}`;
}

async function parse<T>(res: Response): Promise<T> {
  const text = await res.text();
  if (!res.ok) {
    let detail = text;
    try {
      const body = JSON.parse(text) as { error?: { message?: string; code?: string } };
      detail = body.error?.message || body.error?.code || text;
    } catch {
      // Keep the raw gateway response.
    }
    throw new Error(`${res.status}: ${detail}`);
  }
  try {
    return JSON.parse(text) as T;
  } catch {
    throw new Error("gateway returned a non-JSON response");
  }
}

export async function apiGet<T>(path: string): Promise<T> {
  return parse<T>(await fetch(apiPath(path), { cache: "no-store" }));
}

export async function apiPost<T>(path: string, body: unknown): Promise<T> {
  return parse<T>(
    await fetch(apiPath(path), {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(body),
      cache: "no-store",
    }),
  );
}

export type ApiState<T> = {
  data: T | null;
  error: string | null;
  loading: boolean;
  reload: () => void;
};

/**
 * RFC-028 7.2: Page -> Query Hook -> API Service -> Dashboard API.
 * Implemented with TanStack Query for deduplication, caching, polling and
 * retry. Chart components never call the API themselves.
 */
export function useApi<T>(path: string | null, refreshMs = 0): ApiState<T> {
  const query = useQuery<T, Error>({
    queryKey: ["searchos", path],
    queryFn: () => apiGet<T>(path as string),
    enabled: Boolean(path),
    refetchInterval: refreshMs > 0 ? refreshMs : false,
    staleTime: 5_000,
    retry: 1,
  });

  const reload = useCallback(() => {
    void query.refetch();
  }, [query]);

  return {
    data: query.data ?? null,
    error: query.error ? query.error.message : null,
    loading: query.isLoading,
    reload,
  };
}

export function micros(value: number | undefined | null): string {
  if (value === undefined || value === null) return "--";
  return (value / 1_000_000).toFixed(6);
}

export function pct(value: number | undefined | null): string {
  if (value === undefined || value === null) return "--";
  return `${(value * 100).toFixed(1)}%`;
}

export function count(value: number | undefined | null): string {
  if (value === undefined || value === null) return "--";
  return value.toLocaleString("en-US");
}

export function withQuery(path: string, params: Record<string, string | undefined>): string {
  const search = new URLSearchParams();
  for (const [key, value] of Object.entries(params)) {
    if (value) search.set(key, value);
  }
  const qs = search.toString();
  return qs ? `${path}?${qs}` : path;
}
