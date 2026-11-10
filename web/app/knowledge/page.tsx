"use client";

import { useI18n } from "@/lib/i18n";

import { useCallback, useState, type FormEvent } from "react";

import { Badge } from "@/components/ui";
import { Button, Empty, ErrorNotice, Kv, Panel, SectionIntro, Skeleton } from "@/components/ui";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { DocumentLibrary, IngestPanel } from "@/components/knowledge";
import { useKnowledgeFreshness, useKnowledgeGrowth } from "@/lib/analytics";
import { apiPost, count, useApi } from "@/lib/client";
import type { DocumentList, MemoryList, ProfileList, SystemStatus } from "@/lib/types";

const WORKSPACES = ["default", "tavily", "ws_demo"];

export default function KnowledgePage() {
  const { t } = useI18n();
  const profiles = useApi<ProfileList>("profiles");
  const system = useApi<SystemStatus>("system/status", 60_000);
  const growth = useKnowledgeGrowth();
  const freshness = useKnowledgeFreshness();

  const [workspace, setWorkspace] = useState("default");
  const [refreshKey, setRefreshKey] = useState(0);
  const [selectedDoc, setSelectedDoc] = useState<string | null>(null);

  const [docQuery, setDocQuery] = useState("example.com");
  const [docs, setDocs] = useState<DocumentList | null>(null);
  const [docError, setDocError] = useState<string | null>(null);
  const [docBusy, setDocBusy] = useState(false);

  const [memQuery, setMemQuery] = useState("search");
  const [memory, setMemory] = useState<MemoryList | null>(null);
  const [memError, setMemError] = useState<string | null>(null);
  const [memBusy, setMemBusy] = useState(false);

  const refresh = useCallback(() => setRefreshKey((key) => key + 1), []);

  async function findDocuments(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setDocBusy(true);
    setDocError(null);
    try {
      setDocs(await apiPost<DocumentList>("documents/search", {
        query: docQuery,
        limit: 20,
        workspace_id: workspace,
      }));
    } catch (e) {
      setDocError(String(e instanceof Error ? e.message : e));
    } finally {
      setDocBusy(false);
    }
  }

  async function findMemory(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setMemBusy(true);
    setMemError(null);
    try {
      setMemory(await apiPost<MemoryList>("memory/search", { query: memQuery, top_k: 10 }));
    } catch (e) {
      setMemError(String(e instanceof Error ? e.message : e));
    } finally {
      setMemBusy(false);
    }
  }

  return (
    <>
      <SectionIntro
        title="Knowledge"
        description="Import text and files into the same knowledge store as URL fetches, then inspect versions, re-index, or delete. Documents, namespaced memory, versioned search profiles and runtime health are all here."
      />

      <div className="mb-6 flex flex-wrap items-center gap-2">
        <Select value={workspace} onValueChange={setWorkspace}>
          <SelectTrigger className="w-[180px]" aria-label="Workspace">
            <SelectValue />
          </SelectTrigger>
          <SelectContent>
            {WORKSPACES.map((ws) => (
              <SelectItem key={ws} value={ws}>
                {ws}
              </SelectItem>
            ))}
          </SelectContent>
        </Select>
        <Badge tone="neutral">{t("workspace scope")}</Badge>
        <span className="text-xs text-muted-foreground">
          Imports and listings are scoped to this workspace; global (<span className="mono">*</span>)
          rows stay visible.
        </span>
      </div>

      <div className="split">
        <div className="stack">
          <IngestPanel workspace={workspace} onIngested={refresh} />
        </div>
        <div className="stack">
          <DocumentLibrary
            workspace={workspace}
            refreshKey={refreshKey}
            selectedId={selectedDoc}
            onSelect={setSelectedDoc}
          />
        </div>
      </div>

      <div className="split mt-6">
        <div className="stack">
          <Panel title="Find by URL or title" meta={<span>POST /documents/search</span>}>
            <form onSubmit={findDocuments}>
              <div className="form-grid" style={{ gridTemplateColumns: "minmax(0, 1fr) auto", alignItems: "end" }}>
                <div className="field">
                  <Label htmlFor="docQuery">{t("URL or keyword")}</Label>
                  <Input id="docQuery" value={docQuery} onChange={(e) => setDocQuery(e.target.value)} required />
                  <span className="field-helper">{t("Matches canonical URL or title text.")}</span>
                </div>
                <Button type="submit" loading={docBusy}>{t("Find")}</Button>
              </div>
            </form>
            {docError ? <div className="mt-3"><ErrorNotice message={docError} /></div> : null}
            {docBusy ? <Skeleton lines={3} /> : null}
            {!docBusy && docs ? (
              docs.documents.length === 0 ? (
                <Empty title="No matching documents" body="Import text above, or fetch a URL with POST /extract." />
              ) : (
                <Table className="mt-3">
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("URL")}</TableHead>
                      <TableHead>{t("Status")}</TableHead>
                      <TableHead>{t("Versions")}</TableHead>
                      <TableHead>{t("Chunks")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {docs.documents.map((doc) => (
                      <TableRow
                        key={doc.id}
                        onClick={() => setSelectedDoc(doc.id)}
                        style={{ cursor: "pointer" }}
                        className={selectedDoc === doc.id ? "bg-muted/40" : undefined}
                      >
                        <TableCell>{doc.canonical_url}</TableCell>
                        <TableCell>
                          <Badge tone={doc.status === "deleted" ? "muted" : "ok"}>
                            {doc.status ?? "active"}
                          </Badge>
                        </TableCell>
                        <TableCell>{count(doc.version_count)}</TableCell>
                        <TableCell>{count(doc.chunk_count)}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )
            ) : null}
          </Panel>

          <Panel title="Search memory" meta={<span>POST /memory/search</span>}>
            <form onSubmit={findMemory}>
              <div className="form-grid" style={{ gridTemplateColumns: "minmax(0, 1fr) auto", alignItems: "end" }}>
                <div className="field">
                  <Label htmlFor="memQuery">{t("Query")}</Label>
                  <Input id="memQuery" value={memQuery} onChange={(e) => setMemQuery(e.target.value)} required />
                  <span className="field-helper">{t("Namespaced entries visible to the current workspace only.")}</span>
                </div>
                <Button type="submit" loading={memBusy}>{t("Search")}</Button>
              </div>
            </form>
            {memError ? <div className="mt-3"><ErrorNotice message={memError} /></div> : null}
            {!memBusy && memory ? (
              memory.results.length === 0 ? (
                <Empty title="No memory hits" body="Memory entries are created by agents through the memory service." />
              ) : (
                <Table className="mt-3">
                  <TableHeader>
                    <TableRow>
                      <TableHead>{t("Entry")}</TableHead>
                      <TableHead>{t("Score")}</TableHead>
                      <TableHead>{t("URL")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {memory.results.map((hit, i) => (
                      <TableRow key={`${hit.entry?.id ?? i}`}>
                        <TableCell>
                          <div className="row-title">{hit.entry?.title ?? "Untitled entry"}</div>
                          <div className="row-body">{hit.entry?.content ?? ""}</div>
                        </TableCell>
                        <TableCell>{(hit.score ?? 0).toFixed(3)}</TableCell>
                        <TableCell>{hit.entry?.url ?? "local entry"}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              )
            ) : null}
          </Panel>
        </div>

        <div className="stack">
          <Panel title="Knowledge Read Model" meta={<span>RFC-028</span>}>
            <Tabs defaultValue="growth">
              <TabsList>
                <TabsTrigger value="growth">{t("Growth")}</TabsTrigger>
                <TabsTrigger value="freshness">{t("Freshness")}</TabsTrigger>
                <TabsTrigger value="profiles">{t("Profiles")}</TabsTrigger>
                <TabsTrigger value="health">{t("Health")}</TabsTrigger>
              </TabsList>
              <TabsContent value="growth" className="pt-3">
                <div className="stack">
                  <Kv label={t("Days with documents")} value={count(growth.data?.data?.items?.length)} />
                  <Kv label={t("Chunks")} value={count(growth.data?.data?.chunks)} />
                  <Kv label={t("Embeddings")} value={count(growth.data?.data?.embeddings)} />
                  <Separator />
                  {(growth.data?.data?.items ?? []).slice(-7).map(([day, n]) => (
                    <div className="kv" key={day}>
                      <dt>{day}</dt>
                      <dd className="mono">{count(n)}</dd>
                    </div>
                  ))}
                </div>
              </TabsContent>
              <TabsContent value="freshness" className="pt-3">
                <Badge tone="warn">{t("Not instrumented in MVP")}</Badge>
                <p className="mt-3 text-xs text-muted-foreground">
                  {String(freshness.data?.data?.reason ?? "Freshness classification is not instrumented. Missing values are null, not zero.")}
                </p>
              </TabsContent>
              <TabsContent value="profiles" className="pt-3">
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Id</TableHead>
                      <TableHead>{t("Freshness")}</TableHead>
                      <TableHead>{t("Mode")}</TableHead>
                      <TableHead>{t("Version")}</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {(profiles.data?.profiles ?? []).map((p) => (
                      <TableRow key={p.id}>
                        <TableCell>{p.id}</TableCell>
                        <TableCell>{p.freshness ?? "--"}</TableCell>
                        <TableCell>{p.default_mode ?? "--"}</TableCell>
                        <TableCell>{p.version ?? "--"}</TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TabsContent>
              <TabsContent value="health" className="pt-3">
                <div className="stack">
                  <Kv label={t("Database")} value={system.data?.database_ok ? "ok" : "unavailable"} />
                  <Kv
                    label={t("Index")}
                    value={`${count(system.data?.index?.tantivy_chunks)} full-text / ${count(system.data?.index?.sqlite_chunks)} SQLite chunks`}
                  />
                  <Kv label={t("Consistent")} value={system.data?.index?.consistent ? "yes" : "run index rebuild"} />
                  <Kv label={t("Providers")} value={(system.data?.providers ?? []).join(", ") || "none configured"} />
                  <Kv label={t("Crawler extensions")} value={count(system.data?.crawlers)} />
                  <Kv label={t("Extensions")} value={count(system.data?.extensions)} />
                  <Kv label={t("Auth")} value={system.data?.auth_required ? "bearer token required" : "open local dev"} />
                  <Kv label={t("Rate limit")} value={`${count(system.data?.rate_limit_per_minute)} requests / minute`} />
                </div>
              </TabsContent>
            </Tabs>
          </Panel>
        </div>
      </div>
    </>
  );
}
