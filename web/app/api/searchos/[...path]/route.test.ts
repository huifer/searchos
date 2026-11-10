import { describe, expect, it, vi, beforeEach, afterEach } from "vitest";
import { NextRequest } from "next/server";

import { GET, POST, DELETE } from "./route";

// ---------------------------------------------------------------------------
// The console proxy is the trust boundary: the browser never talks to the
// gateway directly, and the bearer token stays server-side.
// ---------------------------------------------------------------------------

const fetchMock = vi.fn();
globalThis.fetch = fetchMock as unknown as typeof fetch;

const ORIGINAL_ENV = { ...process.env };

beforeEach(() => {
  fetchMock.mockReset();
  process.env.SEARCHOS_GATEWAY_URL = "http://searchos-gateway:8787";
  delete process.env.SEARCHOS_AUTH_TOKEN;
  fetchMock.mockResolvedValue(
    new Response(JSON.stringify({ ok: true }), {
      status: 200,
      headers: { "content-type": "application/json" },
    }),
  );
});

afterEach(() => {
  process.env = { ...ORIGINAL_ENV };
});

function context(path: string[]) {
  return { params: Promise.resolve({ path }) };
}

describe("GET /api/searchos/[...path]", () => {
  it("forwards nested dashboard paths to the configured gateway", async () => {
    const req = new NextRequest("http://localhost:3100/api/searchos/api/v1/dashboard/overview?interval=week");
    const res = await GET(req, context(["api", "v1", "dashboard", "overview"]));

    expect(res.status).toBe(200);
    const [url, init] = fetchMock.mock.calls[0];
    expect(String(url)).toBe("http://searchos-gateway:8787/api/v1/dashboard/overview?interval=week");
    expect(init.method).toBe("GET");
    expect(init.headers.get("content-type")).toBe("application/json");
    expect(res.headers.get("x-searchos-proxy")).toBe("1");
  });

  it("injects the server-side bearer token", async () => {
    process.env.SEARCHOS_AUTH_TOKEN = "  secret-token  ";
    const req = new NextRequest("http://localhost:3100/api/searchos/health");
    await GET(req, context(["health"]));
    const [, init] = fetchMock.mock.calls[0];
    expect(init.headers.get("authorization")).toBe("Bearer secret-token");
  });

  it("rejects a path traversal attempt before any upstream call", async () => {
    const req = new NextRequest("http://localhost:3100/api/searchos/../etc/passwd");
    const res = await GET(req, context(["..", "etc", "passwd"]));
    expect(res.status).toBe(400);
    expect(fetchMock).not.toHaveBeenCalled();
  });
});

describe("POST /api/searchos/[...path] · multipart", () => {
  it("forwards the exact multipart content type and raw bytes", async () => {
    const boundary = "----test-boundary";
    const body =
      `--${boundary}\r\n` +
      `Content-Disposition: form-data; name="file"; filename="a.txt"\r\n` +
      `Content-Type: text/plain\r\n\r\n` +
      `hello\n` +
      `\r\n--${boundary}--\r\n`;
    const req = new NextRequest("http://localhost:3100/api/searchos/ingest/file", {
      method: "POST",
      headers: { "content-type": `multipart/form-data; boundary=${boundary}` },
      body,
    });

    const res = await POST(req, context(["ingest", "file"]));
    expect(res.status).toBe(200);

    const [, init] = fetchMock.mock.calls[0];
    expect(init.headers.get("content-type")).toBe(`multipart/form-data; boundary=${boundary}`);
    // Buffer is a BodyInit; verify the bytes survived untouched.
    expect(Buffer.from(init.body as Uint8Array).toString("utf8")).toBe(body);
  });

  it("sends JSON bodies with the JSON content type", async () => {
    const payload = { content: "hello", workspace_id: "default" };
    const req = new NextRequest("http://localhost:3100/api/searchos/ingest", {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify(payload),
    });

    await POST(req, context(["ingest"]));
    const [, init] = fetchMock.mock.calls[0];
    expect(init.headers.get("content-type")).toBe("application/json");
    expect(init.body).toBe(JSON.stringify(payload));
  });
});

describe("DELETE /api/searchos/[...path]", () => {
  it("forwards the method without inventing a body", async () => {
    const req = new NextRequest("http://localhost:3100/api/searchos/documents/doc_1", {
      method: "DELETE",
    });
    await DELETE(req, context(["documents", "doc_1"]));
    const [url, init] = fetchMock.mock.calls[0];
    expect(String(url)).toBe("http://searchos-gateway:8787/documents/doc_1");
    expect(init.method).toBe("DELETE");
  });
});
