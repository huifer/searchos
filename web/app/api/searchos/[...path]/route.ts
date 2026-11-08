import { NextRequest, NextResponse } from "next/server";

export const dynamic = "force-dynamic";
export const runtime = "nodejs";

/**
 * Server-side proxy for the Rust gateway.
 *
 * The browser only ever talks to the same origin (`/api/searchos/*`), so no
 * CORS setup is needed and the bearer token never reaches client code.
 * Everything stays in-process on the developer machine.
 */
function baseUrl(): string {
  return (process.env.SEARCHOS_GATEWAY_URL || "http://127.0.0.1:8787").replace(/\/+$/, "");
}

function jsonError(status: number, message: string): NextResponse {
  return NextResponse.json(
    { error: { code: "GATEWAY_PROXY_ERROR", message } },
    { status, headers: { "cache-control": "no-store" } },
  );
}

type Context = { params: Promise<{ path: string[] }> };

async function handle(req: NextRequest, ctx: Context): Promise<NextResponse> {
  const { path } = await ctx.params;
  if (!path || path.length === 0) {
    return jsonError(400, "missing gateway path");
  }
  if (path.some((segment) => segment.includes("..") || segment.includes("\\"))) {
    return jsonError(400, "invalid gateway path");
  }

  const target = `${baseUrl()}/${path.map(encodeURIComponent).join("/")}${req.nextUrl.search}`;
  const headers = new Headers();
  headers.set("content-type", "application/json");
  const token = process.env.SEARCHOS_AUTH_TOKEN?.trim();
  if (token) {
    headers.set("authorization", `Bearer ${token}`);
  }

  let body: string | undefined;
  if (["POST", "PUT", "PATCH", "DELETE"].includes(req.method)) {
    body = await req.text();
  }

  try {
    const upstream = await fetch(target, {
      method: req.method,
      headers,
      body,
      cache: "no-store",
      signal: AbortSignal.timeout(30_000),
    });
    // SSE must stream through the proxy without buffering (RFC-028 6.2).
    const contentType = upstream.headers.get("content-type") ?? "";
    if (contentType.includes("text/event-stream") && upstream.body) {
      return new NextResponse(upstream.body, {
        status: upstream.status,
        headers: {
          "content-type": "text/event-stream; charset=utf-8",
          "cache-control": "no-store, no-transform",
          connection: "keep-alive",
          "x-searchos-proxy": "1",
        },
      });
    }
    const payload = await upstream.text();
    return new NextResponse(payload, {
      status: upstream.status,
      headers: {
        "content-type": "application/json; charset=utf-8",
        "cache-control": "no-store",
        "x-searchos-proxy": "1",
      },
    });
  } catch (err) {
    return jsonError(
      502,
      `gateway unreachable at ${baseUrl()}. Start it with "searchos gateway start". Detail: ${String(err)}`,
    );
  }
}

export const GET = handle;
export const POST = handle;
export const PUT = handle;
export const PATCH = handle;
export const DELETE = handle;
export const OPTIONS = handle;
