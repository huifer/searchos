import { describe, expect, it, vi, beforeEach } from "vitest";
import { useState } from "react";
import { render, screen, waitFor, within } from "@testing-library/react";
import userEvent from "@testing-library/user-event";

import { DocumentLibrary } from "./knowledge";
import { apiGet, apiPost, apiDelete } from "@/lib/client";

// ---------------------------------------------------------------------------
// Mocks
// ---------------------------------------------------------------------------

vi.mock("@/lib/client", async (importOriginal) => {
  const actual = await importOriginal<typeof import("@/lib/client")>();
  return {
    ...actual,
    apiGet: vi.fn(),
    apiPost: vi.fn(),
    apiDelete: vi.fn(),
  };
});

const apiGetMock = vi.mocked(apiGet);
const apiPostMock = vi.mocked(apiPost);
const apiDeleteMock = vi.mocked(apiDelete);

class ResizeObserverStub {
  observe() {}
  unobserve() {}
  disconnect() {}
}
globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;

const rows = [
  {
    id: "doc_local",
    canonical_url: "searchos-local://default/notes/a",
    title: "Local note",
    source_type: "local_text",
    status: "normalized",
    workspace_id: "default",
    version_count: 1,
    chunk_count: 2,
    updated_at: "2026-11-09T10:00:00Z",
  },
  {
    id: "doc_web",
    canonical_url: "https://example.com/page",
    title: "Fetched page",
    source_type: "web",
    status: "normalized",
    workspace_id: "default",
    version_count: 2,
    chunk_count: 5,
    updated_at: "2026-11-08T10:00:00Z",
  },
];

const detail = {
  document: { id: "doc_local" },
  summary: rows[0],
  current_version: {
    id: "docv_1",
    document_id: "doc_local",
    content_hash: "a".repeat(64),
  },
  versions: [
    {
      id: "docv_1",
      document_id: "doc_local",
      content_hash: "a".repeat(64),
      fetched_at: "2026-11-09T10:00:00Z",
      extractor_version: "extractor/1.0.0",
    },
  ],
};

beforeEach(() => {
  apiGetMock.mockImplementation(async (path: string) => {
    if (path.startsWith("documents?")) return { documents: rows };
    return detail;
  });
  apiPostMock.mockResolvedValue({ reindexed: 2 });
  apiDeleteMock.mockResolvedValue({ deleted: true, document_id: "doc_local" });
  vi.spyOn(window, "confirm").mockReturnValue(true);
});


function Harness({ workspace = "default" }: { workspace?: string }) {
  const [selectedId, setSelectedId] = useState<string | null>(null);
  return (
    <DocumentLibrary
      workspace={workspace}
      refreshKey={0}
      selectedId={selectedId}
      onSelect={setSelectedId}
    />
  );
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

describe("DocumentLibrary", () => {
  it("lists workspace-scoped documents with their lifecycle counts", async () => {
    render(<DocumentLibrary workspace="default" refreshKey={0} />);
    expect(await screen.findByText("Local note")).toBeTruthy();
    expect(screen.getByText("Fetched page")).toBeTruthy();
    expect(apiGetMock).toHaveBeenCalledWith(
      expect.stringContaining("workspace_id=default"),
    );
  });

  it("opens the document detail with the full version history", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await screen.findByText("Local note");
    await user.click(screen.getByText("Local note"));

    expect(await screen.findByText(/Version history/)).toBeTruthy();
    expect(screen.getByText("docv_1")).toBeTruthy();
    expect(apiGetMock).toHaveBeenCalledWith("documents/doc_local");
  });

  it("reindexes without creating a new version", async () => {
    const user = userEvent.setup();
    render(
      <DocumentLibrary
        workspace="default"
        refreshKey={0}
        selectedId="doc_local"
        onSelect={vi.fn()}
      />,
    );
    await screen.findByText(/Version history/);
    await user.click(screen.getByRole("button", { name: /re-index/i }));

    await waitFor(() =>
      expect(apiPostMock).toHaveBeenCalledWith("documents/doc_local/reindex", {}),
    );
    expect(await screen.findByText(/Re-indexed 2 chunk/)).toBeTruthy();
  });

  it("asks for confirmation and deletes the document", async () => {
    const user = userEvent.setup();
    const onSelect = vi.fn();
    render(
      <DocumentLibrary
        workspace="default"
        refreshKey={0}
        selectedId="doc_local"
        onSelect={onSelect}
      />,
    );
    await screen.findByText(/Version history/);
    await user.click(screen.getByRole("button", { name: /delete/i }));

    expect(window.confirm).toHaveBeenCalled();
    await waitFor(() =>
      expect(apiDeleteMock).toHaveBeenCalledWith("documents/doc_local"),
    );
    expect(onSelect).toHaveBeenCalledWith(null);
    expect(await screen.findByText(/no longer returned by document search/i)).toBeTruthy();
  });

  it("does not delete when the operator cancels the confirmation", async () => {
    vi.spyOn(window, "confirm").mockReturnValue(false);
    const user = userEvent.setup();
    render(
      <DocumentLibrary
        workspace="default"
        refreshKey={0}
        selectedId="doc_local"
        onSelect={vi.fn()}
      />,
    );
    await screen.findByText(/Version history/);
    await user.click(screen.getByRole("button", { name: /delete/i }));
    expect(apiDeleteMock).not.toHaveBeenCalled();
  });

  it("shows the empty state with an import hint when there are no documents", async () => {
    apiGetMock.mockImplementation(async () => ({ documents: [] }));
    render(<DocumentLibrary workspace="default" refreshKey={0} />);
    expect(await screen.findByText("No documents yet")).toBeTruthy();
    expect(screen.getByText(/import text or files/i)).toBeTruthy();
  });

  it("scopes the source filter into the list request", async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await screen.findByText("Local note");
    // The source Select is a Radix trigger; open it via the combobox role.
    const trigger = screen.getAllByRole("combobox")[0];
    await user.click(trigger);
    const option = await screen.findByRole("option", { name: /local \(ingested\)/i });
    await user.click(option);

    await waitFor(() =>
      expect(apiGetMock).toHaveBeenCalledWith(
        expect.stringContaining("source_type=local"),
      ),
    );
  });
});
