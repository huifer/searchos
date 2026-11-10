import { describe, expect, it, vi, beforeEach } from "vitest";
import {
  apiPath,
  apiGet,
  apiPost,
  apiDelete,
  apiUpload,
  micros,
  pct,
  count,
  withQuery,
  useApi,
} from "./client";
import { renderHook, waitFor } from "@testing-library/react";
import { QueryClient, QueryClientProvider } from "@tanstack/react-query";
import type { ReactNode } from "react";

// ---------------------------------------------------------------------------
// helpers
// ---------------------------------------------------------------------------

function jsonResponse(body: unknown, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    text: async () => JSON.stringify(body),
  } as unknown as Response;
}

function textResponse(body: string, status = 200): Response {
  return {
    ok: status >= 200 && status < 300,
    status,
    text: async () => body,
  } as unknown as Response;
}

const fetchMock = vi.fn();
globalThis.fetch = fetchMock as unknown as typeof fetch;

beforeEach(() => {
  fetchMock.mockReset();
});

function wrapper({ children }: { children: ReactNode }) {
  const client = new QueryClient({
    defaultOptions: { queries: { retry: false } },
  });
  return <QueryClientProvider client={client}>{children}</QueryClientProvider>;
}

// ---------------------------------------------------------------------------
// apiPath
// ---------------------------------------------------------------------------

describe("apiPath", () => {
  it("routes through the same-origin proxy", () => {
    expect(apiPath("health")).toBe("/api/searchos/health");
  });

  it("strips leading slashes so a caller cannot escape the mount", () => {
    expect(apiPath("/health")).toBe("/api/searchos/health");
    expect(apiPath("///health")).toBe("/api/searchos/health");
  });

  it("keeps nested dataset paths", () => {
    expect(apiPath("api/v1/dashboard/search/timeseries")).toBe(
      "/api/searchos/api/v1/dashboard/search/timeseries",
    );
  });
});

// ---------------------------------------------------------------------------
// formatters
// ---------------------------------------------------------------------------

describe("formatters", () => {
  it("micros renders micro-dollars and never invents a zero", () => {
    expect(micros(1_500_000)).toBe("1.500000");
    expect(micros(0)).toBe("0.000000");
    expect(micros(null)).toBe("--");
    expect(micros(undefined)).toBe("--");
  });

  it("pct renders a percentage and treats missing as unknown", () => {
    expect(pct(0.5)).toBe("50.0%");
    expect(pct(1)).toBe("100.0%");
    expect(pct(null)).toBe("--");
  });

  it("count localises and treats missing as unknown", () => {
    expect(count(1234)).toBe("1,234");
    expect(count(undefined)).toBe("--");
  });
});

// ---------------------------------------------------------------------------
// withQuery
// ---------------------------------------------------------------------------

describe("withQuery", () => {
  it("omits undefined and empty values", () => {
    expect(
      withQuery("api/v1/dashboard/overview", {
        workspace_id: "default",
        interval: undefined,
        from: "",
      }),
    ).toBe("api/v1/dashboard/overview?workspace_id=default");
  });

  it("returns the bare path when nothing is set", () => {
    expect(withQuery("health", {})).toBe("health");
  });

  it("encodes filter values", () => {
    const out = withQuery("x", { q: "rust tokio" });
    expect(out).toBe("x?q=rust+tokio");
  });
});

// ---------------------------------------------------------------------------
// apiGet / apiPost / apiDelete
// ---------------------------------------------------------------------------

describe("apiGet", () => {
  it("returns the parsed envelope", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ data: { searches: 1 } }));
    await expect(apiGet("health")).resolves.toEqual({ data: { searches: 1 } });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/searchos/health",
      expect.objectContaining({ cache: "no-store" }),
    );
  });

  it("surfaces the gateway error code and message", async () => {
    fetchMock.mockResolvedValueOnce(
      jsonResponse({ error: { code: "NOT_FOUND", message: "nope" } }, 404),
    );
    await expect(apiGet("documents/doc_x")).rejects.toThrow("404: nope");
  });

  it("falls back to the raw body when the error is not an envelope", async () => {
    fetchMock.mockResolvedValueOnce(textResponse("bad gateway", 502));
    await expect(apiGet("health")).rejects.toThrow("502: bad gateway");
  });
});

describe("apiPost", () => {
  it("sends a JSON body with the proxy content type", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ ok: 1 }));
    await apiPost("ingest", { content: "hello" });
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/searchos/ingest",
      expect.objectContaining({
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ content: "hello" }),
      }),
    );
  });
});

describe("apiDelete", () => {
  it("issues DELETE without a body", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ deleted: true }));
    await apiDelete("documents/doc_1");
    expect(fetchMock).toHaveBeenCalledWith(
      "/api/searchos/documents/doc_1",
      expect.objectContaining({ method: "DELETE" }),
    );
  });
});

describe("apiUpload", () => {
  it("posts multipart without setting a content type (the boundary belongs to the runtime)", async () => {
    fetchMock.mockResolvedValueOnce(jsonResponse({ document_id: "doc_1" }));
    const form = new FormData();
    form.set("file", new Blob(["body"]), "notes.txt");
    await apiUpload("ingest/file", form);
    const [, init] = fetchMock.mock.calls[0];
    expect(init.method).toBe("POST");
    expect(init.body).toBe(form);
    expect(init.headers).toBeUndefined();
  });
});

// ---------------------------------------------------------------------------
// useApi hook
// ---------------------------------------------------------------------------

describe("useApi", () => {
  it("loads, exposes data and supports reload", async () => {
    fetchMock.mockResolvedValue(jsonResponse({ data: { total: 2 } }));
    const { result } = renderHook(() => useApi<{ data: { total: number } }>("health"), {
      wrapper,
    });
    await waitFor(() => expect(result.current.loading).toBe(false));
    expect(result.current.error).toBeNull();
    expect(result.current.data?.data.total).toBe(2);

    fetchMock.mockClear();
    fetchMock.mockResolvedValue(jsonResponse({ data: { total: 3 } }));
    result.current.reload();
    await waitFor(() => expect(result.current.data?.data.total).toBe(3));
  });

  it("reports errors instead of throwing", async () => {
    fetchMock.mockResolvedValue(
      jsonResponse({ error: { code: "X", message: "boom" } }, 500),
    );
    const { result } = renderHook(() => useApi("health"), { wrapper });
    await waitFor(
        () => expect(result.current.loading).toBe(false),
        { timeout: 5000 },
      );
    expect(result.current.error).toContain("boom");
    expect(result.current.data).toBeNull();
  });

  it("does not fetch when the path is null", () => {
    renderHook(() => useApi(null), { wrapper });
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
