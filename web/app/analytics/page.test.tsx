import { describe, expect, it, vi, beforeEach, afterAll } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import { TooltipProvider } from "@/components/ui/tooltip";
import type { ReactNode } from "react";

import AnalyticsPage from "@/app/analytics/page";

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

vi.mock("next/dynamic", () => ({
  default: (_loader: unknown, options?: { loading?: () => ReactNode }) => {
    const Loading = options?.loading;
    const Stub = () => (Loading ? <>{Loading()}</> : <div data-testid="chart-stub" />);
    return Stub;
  },
}));

const fetchMock = vi.fn();
globalThis.fetch = fetchMock as unknown as typeof fetch;

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;

function envelope(
  metaOverrides: Record<string, unknown> = {},
  data: Record<string, unknown> = {
    items: [{ date: "2026-11-09", total: 1, local: 1, external: 0, failed: 0 }],
  },
) {
  return {
    data,
    meta: {
      generated_at: "2026-11-09T00:00:00Z",
      time_range: { from: null, to: null, timezone: "UTC" },
      interval: "day",
      aggregation_version: "1",
      data_freshness: "complete",
      query_mode: "precomputed",
      applied_filters: [],
      ignored_filters: [],
      refresh_skipped: true,
      model_age_seconds: 12,
      workspace_scope: "*",
      ...metaOverrides,
    },
  };
}


/** Payload shape varies by dataset: named-count / cost views return tuples,
 *  time-series and search-request views return objects. Keep this mock aligned
 *  with `web/lib/analytics.ts` so every table row has a real key. */
function responseFor(url: string, metaOverrides: Record<string, unknown> = {}) {
  let data: Record<string, unknown>;
  if (url.includes("search/timeseries")) {
    data = { items: [{ date: "2026-11-09", total: 1, local: 1, external: 0, failed: 0 }] };
  } else if (url.includes("search/strategies")) {
    data = { items: [["local_resolve", 1]] };
  } else if (url.includes("search/requests")) {
    data = {
      items: [
        {
          request_id: "req-1",
          query: "rust ownership",
          mode: "local-only",
          final_strategy: "local_resolve",
          status: "completed",
          created_at: "2026-11-09T00:00:00Z",
        },
      ],
    };
  } else if (url.includes("cost/breakdown")) {
    data = {
      by_operation: [["search", 1, 100]],
      by_provider: [["local", 1, 100]],
      by_workspace: [["default", 1, 100]],
      by_agent: [["cli", 1, 100]],
    };
  } else if (url.includes("quality/dimensions")) {
    data = {
      freshness: [["fresh", 1]],
      coverage: [["complete", 1]],
      source: [["local", 1]],
      failures: [],
    };
  } else if (url.includes("knowledge/documents")) {
    data = { items: [["2026-11-09", 1]], chunks: 1, embeddings: 1 };
  } else if (url.includes("providers")) {
    data = { items: [["local", 1, 100]], health: "ok" };
  } else if (url.includes("crawlers/summary")) {
    data = { total: 1, successful: 1, failed: 0, reason: "" };
  } else if (url.includes("infrastructure/summary")) {
    data = { sqlite_bytes: 1024, index_chunks: 1, reason: "" };
  } else {
    data = { items: [{ date: "2026-11-09", total: 1, local: 1, external: 0, failed: 0 }] };
  }
  return {
    ok: true,
    status: 200,
    text: async () => JSON.stringify(envelope(metaOverrides, data)),
  } as unknown as Response;
}

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({
    defaultOptions: {
      queries: { retry: false, refetchInterval: false },
    },
  });
  return (
    <TooltipProvider delayDuration={0}>
      <QueryClientProvider client={client}>{children}</QueryClientProvider>
    </TooltipProvider>
  );
}

beforeEach(() => {
  fetchMock.mockReset();
  fetchMock.mockImplementation(async (input: unknown) => responseFor(String(input)));
});

afterAll(() => {
  globalThis.ResizeObserver = undefined as unknown as typeof ResizeObserver;
});

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("Analytics page · filter provenance", () => {
  it("shows a Precomputed badge for the unfiltered Read Model path", async () => {
    render(<AnalyticsPage />, { wrapper });
    expect(await screen.findByText("Precomputed")).toBeTruthy();
    expect(screen.queryByText("Live SQL")).toBeNull();
  });

  it("shows Live SQL and lists filters a view cannot apply", async () => {
    fetchMock.mockImplementation(async (input: unknown) =>
      responseFor(String(input), {
        query_mode: "sql",
        data_freshness: "live",
        applied_filters: ["from", "to"],
        ignored_filters: [
          {
            filter: "quality_status",
            reason: "the quality-gate outcome is only recorded per search task",
          },
        ],
      }),
    );

    render(<AnalyticsPage />, { wrapper });

    expect(await screen.findByText("Live SQL")).toBeTruthy();
    expect(screen.getByText(/1 filters? ignored/)).toBeTruthy();
    expect(
      screen.getByText(/quality_status — the quality-gate outcome is only recorded per search task/),
    ).toBeTruthy();
  });

  it("does not show an ignored-filter notice when every filter was applied", async () => {
    render(<AnalyticsPage />, { wrapper });
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    expect(screen.queryByText(/ignored/)).toBeNull();
  });

  it("sends the selected interval on every dataset request", async () => {
    render(<AnalyticsPage />, { wrapper });
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const urls = fetchMock.mock.calls.map((call) => String(call[0]));
    expect(urls.length).toBeGreaterThan(1);
    expect(urls.every((url) => url.includes("interval=day"))).toBe(true);
    expect(urls.some((url) => url.includes("search%2Ftimeseries") || url.includes("search/timeseries"))).toBe(true);
  });

  it("renders the Dashboard API explanation from the corrected contract", async () => {
    render(<AnalyticsPage />, { wrapper });
    await waitFor(() => expect(fetchMock).toHaveBeenCalled());
    const tooltip = screen.getByLabelText("Read model information");
    expect(tooltip).toBeTruthy();
  });
});
