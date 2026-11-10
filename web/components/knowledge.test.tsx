import { beforeAll, describe, expect, it, vi } from "vitest";
import { render, screen, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { IngestPanel } from "./knowledge";
import { apiPost, apiUpload } from "@/lib/client";

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

vi.mock("@/lib/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/client")>();
  return {
    ...actual,
    apiPost: vi.fn(),
    apiUpload: vi.fn(),
    apiGet: vi.fn(async () => ({ documents: [] })),
  };
});


/** Radix keeps the inactive tab mounted (hidden), so scope queries to the
 *  active panel — otherwise the "Files" tab trigger collides with the
 *  "Files" field label. */
function activePanel(): HTMLElement {
  const panels = screen.getAllByRole("tabpanel");
  const active = panels.find((p) => p.getAttribute("data-state") === "active");
  if (!active) throw new Error("no active tab panel");
  return active;
}

const apiPostMock = vi.mocked(apiPost);
const apiUploadMock = vi.mocked(apiUpload);

beforeAll(() => {
  // jsdom lacks ResizeObserver, which the shadcn primitives use.
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;
});

// ---------------------------------------------------------------------------
// Text import path
// ---------------------------------------------------------------------------

describe("IngestPanel · text", () => {
  it("posts the content to /ingest and surfaces the created document", async () => {
    const user = userEvent.setup();
    apiPostMock.mockResolvedValueOnce({
      document_id: "doc_1",
      version_id: "docv_1",
      canonical_url: "searchos-local://default/notes/a",
      source_type: "local_text",
      changed: true,
      chunk_count: 1,
      status: "indexed",
    });
    const onIngested = vi.fn();

    render(<IngestPanel workspace="default" onIngested={onIngested} />);

    const panel = activePanel();
    await user.type(within(panel).getByLabelText("Content"), "hello world body");
    await user.type(within(panel).getByLabelText("Source key"), "notes/a");
    await user.type(within(panel).getByLabelText("Title"), "Note A");
    await user.click(within(panel).getByRole("button", { name: /import text/i }));

    expect(apiPostMock).toHaveBeenCalledWith("ingest", {
      content: "hello world body",
      source_key: "notes/a",
      title: "Note A",
      workspace_id: "default",
    });
    expect(await screen.findByText("searchos-local://default/notes/a")).toBeTruthy();
    expect(onIngested).toHaveBeenCalledTimes(1);
  });

  it("shows the error instead of clearing the form when the gateway rejects it", async () => {
    const user = userEvent.setup();
    apiPostMock.mockRejectedValueOnce(new Error("413: document too large"));

    render(<IngestPanel workspace="default" />);
    const panel = activePanel();

    await user.type(within(panel).getByLabelText("Content"), "body");
    await user.click(within(panel).getByRole("button", { name: /import text/i }));

    expect(await screen.findByText(/413: document too large/i)).toBeTruthy();
  });

  it("passes the selected workspace through to the gateway", async () => {
    const user = userEvent.setup();
    apiPostMock.mockResolvedValueOnce({
      document_id: "doc_2",
      canonical_url: "searchos-local://tavily/x",
    });

    render(<IngestPanel workspace="tavily" />);
    const panel = activePanel();
    await user.type(within(panel).getByLabelText("Content"), "body");
    await user.click(within(panel).getByRole("button", { name: /import text/i }));

    expect(apiPostMock).toHaveBeenCalledWith(
      "ingest",
      expect.objectContaining({ workspace_id: "tavily" }),
    );
  });
});

// ---------------------------------------------------------------------------
// File import path
// ---------------------------------------------------------------------------

describe("IngestPanel · files", () => {
  function makeFile(name: string, body: string): File {
    return new File([body], name, { type: "text/plain" });
  }

  it("uses /ingest/file for a single file", async () => {
    const user = userEvent.setup();
    apiUploadMock.mockResolvedValueOnce({
      document_id: "doc_3",
      canonical_url: "searchos-local://default/one.txt",
    });

    render(<IngestPanel workspace="default" />);
    await user.click(screen.getByRole("tab", { name: "Files" }));
    const input = activePanel().querySelector("#ingest-files") as HTMLInputElement;
    await user.upload(input, makeFile("one.txt", "single body"));
    expect(screen.getByText(/1 file\(s\): one\.txt/)).toBeTruthy();

    await user.click(within(activePanel()).getByRole("button", { name: /^import file\b/i }));

    expect(apiUploadMock).toHaveBeenCalledTimes(1);
    const [path, form] = apiUploadMock.mock.calls[0];
    expect(path).toBe("ingest/file");
    const sent = form as FormData;
    expect((sent.get("file") as File).name).toBe("one.txt");
    expect(sent.get("workspace_id")).toBe("default");
    expect(sent.get("source_key")).toBe("one.txt");
    expect(await screen.findByText("searchos-local://default/one.txt")).toBeTruthy();
  });

  it("uses /ingest/batch with one item per file for several files", async () => {
    const user = userEvent.setup();
    apiPostMock.mockResolvedValueOnce({
      job_id: "ing_1",
      state: "completed",
      total_items: 2,
      ok_items: 2,
      failed_items: 0,
      documents: [],
      failures: [],
    });

    render(<IngestPanel workspace="default" />);
    await user.click(screen.getByRole("tab", { name: "Files" }));
    const input = activePanel().querySelector("#ingest-files") as HTMLInputElement;
    await user.upload(input, [
      makeFile("a.txt", "alpha"),
      makeFile("b.txt", "beta"),
    ]);
    expect(screen.getByText(/2 file\(s\)/)).toBeTruthy();

    await user.click(within(activePanel()).getByRole("button", { name: /import 2 files/i }));

    expect(apiPostMock).toHaveBeenCalledWith("ingest/batch", {
      items: [
        expect.objectContaining({
          content: "alpha",
          source_key: "a.txt",
          title: "a.txt",
          workspace_id: "default",
        }),
        expect.objectContaining({
          content: "beta",
          source_key: "b.txt",
          title: "b.txt",
          workspace_id: "default",
        }),
      ],
    });
    expect(await screen.findByText("ing_1")).toBeTruthy();
  });

  it("refuses to submit with no files selected", async () => {
    const user = userEvent.setup();
    render(<IngestPanel workspace="default" />);
    await user.click(screen.getByRole("tab", { name: "Files" }));
    await user.click(within(activePanel()).getByRole("button", { name: /^import file\b/i }));
    expect(await screen.findByText(/choose at least one file/i)).toBeTruthy();
    expect(apiUploadMock).not.toHaveBeenCalled();
    expect(apiPostMock).not.toHaveBeenCalled();
  });

  it("reports invalid UTF-8 from the browser instead of uploading mojibake", async () => {
    const user = userEvent.setup();
    // A lone 0xFF byte is invalid UTF-8; TextDecoder(fatal) rejects it.
    const bad = new File([new Uint8Array([0xff, 0xfe, 0x00])], "binary.bin", {
      type: "application/octet-stream",
    });

    render(<IngestPanel workspace="default" />);
    await user.click(screen.getByRole("tab", { name: "Files" }));
    const input = activePanel().querySelector("#ingest-files") as HTMLInputElement;
    await user.upload(input, bad);
    await user.click(within(activePanel()).getByRole("button", { name: /^import file\b/i }));

    expect(await screen.findByText(/not valid|invalid|decode|failed to read/i)).toBeTruthy();
    expect(apiUploadMock).not.toHaveBeenCalled();
    expect(apiPostMock).not.toHaveBeenCalled();
  });
});
