"use client";

import { useCallback, useEffect, useState, type ChangeEvent, type FormEvent } from "react";

import { Badge } from "@/components/ui";
import { Button, Empty, ErrorNotice, Kv, Panel, Skeleton } from "@/components/ui";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { apiDelete, apiGet, apiPost, apiUpload, count } from "@/lib/client";
import type {
  DocumentDetail,
  DocumentList,
  DocumentSummary,
  IngestBatchResult,
  IngestJob,
  IngestedDocument,
} from "@/lib/types";

const SOURCE_TYPES = [
  { value: "all", label: "All sources" },
  { value: "web", label: "Web (fetched)" },
  { value: "local", label: "Local (ingested)" },
  { value: "local_text", label: "Local text" },
  { value: "local_file", label: "Local file" },
];

function statusTone(status?: string): "ok" | "warn" | "muted" | "neutral" {
  switch (status) {
    case "indexed":
    case "normalized":
      return "ok";
    case "deleted":
      return "muted";
    case "fetched":
      return "neutral";
    default:
      return "warn";
  }
}

/** Reads a file into an ArrayBuffer. `Blob.arrayBuffer()` is missing in older
 *  browsers (Safari < 14) and in jsdom, so fall back to FileReader. */
async function readAsArrayBuffer(file: File): Promise<ArrayBuffer> {
  if (typeof file.arrayBuffer === "function") {
    return file.arrayBuffer();
  }
  return new Promise((resolve, reject) => {
    const reader = new FileReader();
    reader.onload = () => resolve(reader.result as ArrayBuffer);
    reader.onerror = () => reject(new Error("failed to read the file"));
    reader.readAsArrayBuffer(file);
  });
}

/** Decodes a file as strict UTF-8 so the browser reports the same rejection
 *  the gateway would, instead of silently substituting U+FFFD. */
async function decodeUtf8(file: File): Promise<string> {
  const bytes = await readAsArrayBuffer(file);
  return new TextDecoder("utf-8", { fatal: true }).decode(bytes);
}

// ---------------------------------------------------------------------------
// Ingest
// ---------------------------------------------------------------------------

export function IngestPanel({
  workspace,
  onIngested,
}: {
  workspace: string;
  onIngested?: () => void;
}) {
  const [tab, setTab] = useState("text");
  const [content, setContent] = useState("");
  const [sourceKey, setSourceKey] = useState("");
  const [title, setTitle] = useState("");
  const [files, setFiles] = useState<File[]>([]);
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [created, setCreated] = useState<IngestedDocument | null>(null);
  const [job, setJob] = useState<IngestBatchResult | null>(null);

  function reset() {
    setError(null);
    setCreated(null);
    setJob(null);
  }

  async function submitText(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    reset();
    try {
      const doc = await apiPost<IngestedDocument>("ingest", {
        content,
        source_key: sourceKey || undefined,
        title: title || undefined,
        workspace_id: workspace,
      });
      setCreated(doc);
      setContent("");
      onIngested?.();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }

  async function submitFiles(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    if (files.length === 0) {
      setError("Choose at least one file first.");
      return;
    }
    setBusy(true);
    reset();
    try {
      const decoded = await Promise.all(
        files.map(async (file) => ({ file, text: await decodeUtf8(file) })),
      );
      if (decoded.length === 1) {
        const form = new FormData();
        form.set("file", decoded[0].file);
        form.set("workspace_id", workspace);
        form.set("source_key", decoded[0].file.name);
        form.set("title", decoded[0].file.name);
        setCreated(await apiUpload<IngestedDocument>("ingest/file", form));
      } else {
        setJob(
          await apiPost<IngestBatchResult>("ingest/batch", {
            items: decoded.map((entry) => ({
              content: entry.text,
              source_key: entry.file.name,
              title: entry.file.name,
              workspace_id: workspace,
            })),
          }),
        );
      }
      setFiles([]);
      onIngested?.();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }

  function onFileChange(event: ChangeEvent<HTMLInputElement>) {
    setFiles(Array.from(event.target.files ?? []));
  }

  return (
    <Panel
      title="Import knowledge"
      meta={<span>POST /ingest · /ingest/file · /ingest/batch</span>}
    >
      <Tabs value={tab} onValueChange={setTab}>
        <TabsList>
          <TabsTrigger value="text">Text</TabsTrigger>
          <TabsTrigger value="files">Files</TabsTrigger>
        </TabsList>

        <TabsContent value="text" className="pt-3">
          <form onSubmit={submitText}>
            <div className="stack">
              <div className="field">
                <Label htmlFor="ingest-content">Content</Label>
                <textarea
                  id="ingest-content"
                  className="w-full rounded-md border border-border bg-background px-3 py-2 font-mono text-xs"
                  rows={6}
                  value={content}
                  onChange={(e) => setContent(e.target.value)}
                  placeholder="Paste the text you want searchable…"
                  required
                />
                <span className="field-helper">
                  Imported into the same knowledge store as URL fetches: same versioning,
                  chunking, workspace isolation and rebuild path.
                </span>
              </div>
              <div className="form-grid" style={{ gridTemplateColumns: "repeat(auto-fit, minmax(180px, 1fr))" }}>
                <div className="field">
                  <Label htmlFor="ingest-key">Source key</Label>
                  <Input
                    id="ingest-key"
                    value={sourceKey}
                    onChange={(e) => setSourceKey(e.target.value)}
                    placeholder="notes/alpha"
                  />
                  <span className="field-helper">
                    Re-importing the same key + unchanged content is a no-op.
                  </span>
                </div>
                <div className="field">
                  <Label htmlFor="ingest-title">Title</Label>
                  <Input
                    id="ingest-title"
                    value={title}
                    onChange={(e) => setTitle(e.target.value)}
                    placeholder="Optional display title"
                  />
                </div>
              </div>
              <div>
                <Button type="submit" loading={busy}>
                  Import text
                </Button>
              </div>
            </div>
          </form>
        </TabsContent>

        <TabsContent value="files" className="pt-3">
          <form onSubmit={submitFiles}>
            <div className="stack">
              <div className="field">
                <Label htmlFor="ingest-files">Files</Label>
                <Input id="ingest-files" type="file" multiple onChange={onFileChange} />
                <span className="field-helper">
                  {files.length === 0
                    ? "Pick one or more text files. One file uses /ingest/file; several use /ingest/batch."
                    : `${files.length} file(s): ${files.map((f) => f.name).join(", ")}`}
                </span>
              </div>
              <div>
                <Button type="submit" loading={busy}>
                  Import {files.length > 1 ? `${files.length} files` : "file"}
                </Button>
              </div>
            </div>
          </form>
        </TabsContent>
      </Tabs>

      {error ? (
        <div className="mt-3">
          <ErrorNotice message={error} />
        </div>
      ) : null}

      {created ? (
        <div className="mt-3">
          <div className="kv">
            <dt>Imported</dt>
            <dd className="mono">{created.canonical_url ?? created.document_id}</dd>
          </div>
          <div className="kv">
            <dt>Document</dt>
            <dd className="mono">{created.document_id}</dd>
          </div>
          <div className="kv">
            <dt>Version</dt>
            <dd className="mono">{created.version_id ?? "--"}</dd>
          </div>
          <div className="kv">
            <dt>Chunks</dt>
            <dd className="mono">{count(created.chunk_count)}</dd>
          </div>
          <div className="kv">
            <dt>Changed</dt>
            <dd className="mono">{created.changed ? "new content" : "unchanged"}</dd>
          </div>
        </div>
      ) : null}

      {job ? <JobCard job={job} /> : null}
    </Panel>
  );
}

/** Polls a batch job until it leaves pending/running, then shows per-item
 *  outcomes so a failed import is inspectable and retryable. */
function JobCard({ job }: { job: IngestBatchResult }) {
  const jobId = job.job_id ?? "";
  const [detail, setDetail] = useState<IngestJob | null>(null);
  const state = detail?.job?.state ?? job.state ?? "pending";
  const active = state === "pending" || state === "running";
  const failedItems = (detail?.items ?? []).filter((item) => item.state === "failed");

  useEffect(() => {
    if (!jobId || !active) return;
    const timer = setInterval(() => {
      void apiGet<IngestJob>(`ingest/jobs/${jobId}`)
        .then(setDetail)
        .catch(() => {});
    }, 2_000);
    return () => clearInterval(timer);
  }, [jobId, active]);

  useEffect(() => {
    if (jobId && !active) {
      void apiGet<IngestJob>(`ingest/jobs/${jobId}`)
        .then(setDetail)
        .catch(() => {});
    }
  }, [jobId, active]);

  return (
    <div className="mt-3">
      <Separator />
      <div className="mt-3 stack">
        <div className="flex items-center gap-2">
          <Badge tone={active ? "warn" : "ok"}>{active ? "running" : state}</Badge>
          <span className="font-mono text-[10px] text-muted-foreground">{jobId}</span>
        </div>
        <div className="kv">
          <dt>Items</dt>
          <dd className="mono">
            {count(detail?.job?.total_items ?? job.total_items)} total ·{" "}
            {count(detail?.job?.ok_items ?? job.ok_items)} ok ·{" "}
            {count(detail?.job?.failed_items ?? job.failed_items)} failed
          </dd>
        </div>
        {failedItems.length > 0 ? (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Source key</TableHead>
                <TableHead>Error</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {failedItems.map((item) => (
                <TableRow key={item.id}>
                  <TableCell>{item.source_key ?? "--"}</TableCell>
                  <TableCell>{item.error_detail ?? "unknown error"}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        ) : null}
        {(job.failures ?? []).length > 0 && failedItems.length === 0 ? (
          <Table>
            <TableHeader>
              <TableRow>
                <TableHead>Source key</TableHead>
                <TableHead>Error</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {job.failures!.map((failure, index) => (
                <TableRow key={`${failure.source_key ?? "item"}-${index}`}>
                  <TableCell>{failure.source_key ?? "--"}</TableCell>
                  <TableCell>{failure.error ?? "unknown error"}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        ) : null}
      </div>
    </div>
  );
}

// ---------------------------------------------------------------------------
// Document library
// ---------------------------------------------------------------------------

export function DocumentLibrary({
  workspace,
  refreshKey,
  selectedId,
  onSelect,
}: {
  workspace: string;
  refreshKey: number;
  selectedId?: string | null;
  onSelect?: (id: string | null) => void;
}) {
  const [sourceType, setSourceType] = useState("all");
  const [docs, setDocs] = useState<DocumentList | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [notice, setNotice] = useState<string | null>(null);
  const [busyId, setBusyId] = useState<string | null>(null);

  const load = useCallback(async () => {
    const query = new URLSearchParams({ workspace_id: workspace, limit: "100" });
    if (sourceType !== "all") query.set("source_type", sourceType);
    setDocs(await apiGet<DocumentList>(`documents?${query.toString()}`));
  }, [workspace, sourceType]);

  useEffect(() => {
    setError(null);
    void load().catch((e) => setError(String(e instanceof Error ? e.message : e)));
  }, [load, refreshKey]);

  async function reindex(id: string) {
    setBusyId(id);
    setNotice(null);
    setError(null);
    try {
      const result = await apiPost<{ reindexed?: number }>(`documents/${id}/reindex`, {});
      setNotice(`Re-indexed ${result.reindexed ?? 0} chunk(s); metadata unchanged.`);
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusyId(null);
    }
  }

  async function remove(id: string, url: string) {
    if (!window.confirm(`Delete ${url}?\n\nThe document is soft-deleted and removed from the full-text index. It stays visible under status=deleted.`)) {
      return;
    }
    setBusyId(id);
    setNotice(null);
    setError(null);
    try {
      await apiDelete<{ deleted?: boolean }>(`documents/${id}`);
      setNotice(`Deleted ${url}. It is no longer returned by document search.`);
      onSelect?.(null);
      await load();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusyId(null);
    }
  }

  return (
    <div className="stack">
      <Panel
        title="Document library"
        meta={<span>GET /documents</span>}
      >
        <div className="form-grid" style={{ gridTemplateColumns: "minmax(140px, 200px)", alignItems: "end" }}>
          <div className="field">
            <Label htmlFor="doc-source-type">Source</Label>
            <Select value={sourceType} onValueChange={setSourceType}>
              <SelectTrigger id="doc-source-type">
                <SelectValue />
              </SelectTrigger>
              <SelectContent>
                {SOURCE_TYPES.map((option) => (
                  <SelectItem key={option.value} value={option.value}>
                    {option.label}
                  </SelectItem>
                ))}
              </SelectContent>
            </Select>
          </div>
        </div>

        {error ? <div className="mt-3"><ErrorNotice message={error} /></div> : null}
        {notice ? (
          <div className="mt-3">
            <Badge tone="ok">{notice}</Badge>
          </div>
        ) : null}
        {!docs && !error ? <Skeleton lines={3} /> : null}
        {docs && docs.documents.length === 0 ? (
          <Empty
            title="No documents yet"
            body="Import text or files above, or fetch a URL with POST /extract."
          />
        ) : null}
        {docs && docs.documents.length > 0 ? (
          <Table className="mt-3">
            <TableHeader>
              <TableRow>
                <TableHead>Title / URL</TableHead>
                <TableHead>Source</TableHead>
                <TableHead>Status</TableHead>
                <TableHead>Versions</TableHead>
                <TableHead>Chunks</TableHead>
                <TableHead>Updated</TableHead>
              </TableRow>
            </TableHeader>
            <TableBody>
              {docs.documents.map((doc) => (
                <TableRow
                  key={doc.id}
                  onClick={() => onSelect?.(doc.id)}
                  className={selectedId === doc.id ? "bg-muted/40" : undefined}
                  style={{ cursor: "pointer" }}
                >
                  <TableCell>
                    <div className="row-title">{doc.title ?? doc.canonical_url}</div>
                    <div className="row-body">{doc.canonical_url}</div>
                  </TableCell>
                  <TableCell>{doc.source_type ?? "web"}</TableCell>
                  <TableCell>
                    <Badge tone={statusTone(doc.status)}>{doc.status ?? "unknown"}</Badge>
                  </TableCell>
                  <TableCell>{count(doc.version_count)}</TableCell>
                  <TableCell>{count(doc.chunk_count)}</TableCell>
                  <TableCell>{doc.updated_at?.slice(0, 19).replace("T", " ") ?? "--"}</TableCell>
                </TableRow>
              ))}
            </TableBody>
          </Table>
        ) : null}
      </Panel>

      {selectedId ? (
        <DocumentDetail
          id={selectedId}
          busy={busyId === selectedId}
          onReindex={() => void reindex(selectedId)}
          onDelete={(url) => void remove(selectedId, url)}
          onChanged={load}
        />
      ) : null}
    </div>
  );
}

function DocumentDetail({
  id,
  busy,
  onReindex,
  onDelete,
}: {
  id: string;
  busy: boolean;
  onReindex: () => void;
  onDelete: (url: string) => void;
  onChanged: () => void;
}) {
  const [detail, setDetail] = useState<DocumentDetail | null>(null);
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDetail(null);
    setError(null);
    void apiGet<DocumentDetail>(`documents/${id}`)
      .then(setDetail)
      .catch((e) => setError(String(e instanceof Error ? e.message : e)));
  }, [id]);

  const summary: DocumentSummary | undefined = detail?.summary ?? undefined;
  const versions = detail?.versions ?? [];

  return (
    <Panel title="Document detail" meta={<span>GET /documents/{id}</span>}>
      {error ? <ErrorNotice message={error} /> : null}
      {!detail && !error ? <Skeleton lines={4} /> : null}
      {summary ? (
        <div className="stack">
          <div className="kv">
            <dt>Title</dt>
            <dd className="mono">{summary.title ?? summary.canonical_url}</dd>
          </div>
          <div className="kv">
            <dt>Canonical URL</dt>
            <dd className="mono">{summary.canonical_url}</dd>
          </div>
          <div className="kv">
            <dt>Status</dt>
            <dd className="mono">
              <Badge tone={statusTone(summary.status)}>{summary.status ?? "unknown"}</Badge>
            </dd>
          </div>
          <div className="kv">
            <dt>Workspace</dt>
            <dd className="mono">{summary.workspace_id ?? "default"}</dd>
          </div>
          <div className="kv">
            <dt>Versions / chunks</dt>
            <dd className="mono">
              {count(summary.version_count)} / {count(summary.chunk_count)}
            </dd>
          </div>
          <Separator />
          <div className="flex flex-wrap gap-2">
            <Button onClick={onReindex} loading={busy}>
              Re-index
            </Button>
            <Button variant="destructive" onClick={() => onDelete(summary.canonical_url)}>
              Delete
            </Button>
          </div>
          <p className="text-xs text-muted-foreground">
            Re-index rewrites only the full-text index entries; the version history is never
            rewritten. Delete is a soft delete plus index removal.
          </p>
          <Separator />
          <div className="kv">
            <dt>Version history</dt>
            <dd className="mono">{versions.length} version(s)</dd>
          </div>
          {versions.length === 0 ? (
            <Empty title="No versions" body="The document has no persisted content version." />
          ) : (
            <Table>
              <TableHeader>
                <TableRow>
                  <TableHead>Version</TableHead>
                  <TableHead>Content hash</TableHead>
                  <TableHead>Fetched</TableHead>
                  <TableHead>Extractor</TableHead>
                </TableRow>
              </TableHeader>
              <TableBody>
                {versions.map((version) => (
                  <TableRow key={version.id}>
                    <TableCell>{version.id}</TableCell>
                    <TableCell>{version.content_hash.slice(0, 16)}…</TableCell>
                    <TableCell>{version.fetched_at?.slice(0, 19).replace("T", " ") ?? "--"}</TableCell>
                    <TableCell>{version.extractor_version ?? "--"}</TableCell>
                  </TableRow>
                ))}
              </TableBody>
            </Table>
          )}
        </div>
      ) : null}
    </Panel>
  );
}
