"use client";

import { useState, type FormEvent } from "react";

import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button, Empty, ErrorNotice, Kv, Panel, SectionIntro, Skeleton } from "@/components/ui";
import { useKnowledgeFreshness, useKnowledgeGrowth } from "@/lib/analytics";
import { apiPost, count, useApi } from "@/lib/client";
import type { DocumentList, MemoryList, ProfileList, SystemStatus } from "@/lib/types";

export default function KnowledgePage() {
  const profiles = useApi<ProfileList>("profiles");
  const system = useApi<SystemStatus>("system/status", 60_000);
  const growth = useKnowledgeGrowth();
  const freshness = useKnowledgeFreshness();

  const [docQuery, setDocQuery] = useState("example.com");
  const [docs, setDocs] = useState<DocumentList | null>(null);
  const [docError, setDocError] = useState<string | null>(null);
  const [docBusy, setDocBusy] = useState(false);

  const [memQuery, setMemQuery] = useState("search");
  const [memory, setMemory] = useState<MemoryList | null>(null);
  const [memError, setMemError] = useState<string | null>(null);
  const [memBusy, setMemBusy] = useState(false);

  async function findDocuments(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setDocBusy(true); setDocError(null);
    try { setDocs(await apiPost<DocumentList>("documents/search", { query: docQuery, limit: 20 })); }
    catch (e) { setDocError(String(e instanceof Error ? e.message : e)); }
    finally { setDocBusy(false); }
  }

  async function findMemory(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setMemBusy(true); setMemError(null);
    try { setMemory(await apiPost<MemoryList>("memory/search", { query: memQuery, top_k: 10 })); }
    catch (e) { setMemError(String(e instanceof Error ? e.message : e)); }
    finally { setMemBusy(false); }
  }

  return (
    <>
      <SectionIntro
        title="Knowledge"
        description="Persisted documents, namespaced memory, versioned search profiles and runtime health. Knowledge analytics come from the Dashboard Read Model."
      />

      <div className="split">
        <div className="stack">
          <Panel title="Document explorer" meta={<span>POST /documents/search</span>}>
            <form onSubmit={findDocuments}>
              <div className="form-grid" style={{ gridTemplateColumns: "minmax(0, 1fr) auto", alignItems: "end" }}>
                <div className="field">
                  <Label htmlFor="docQuery">URL or keyword</Label>
                  <Input id="docQuery" value={docQuery} onChange={(e) => setDocQuery(e.target.value)} required />
                  <span className="field-helper">Matches canonical URL text in the SQLite document store.</span>
                </div>
                <Button type="submit" loading={docBusy}>Find</Button>
              </div>
            </form>
            {docError ? <div className="mt-3"><ErrorNotice message={docError} /></div> : null}
            {docBusy ? <Skeleton lines={3} /> : null}
            {!docBusy && docs ? (
              docs.documents.length === 0 ? (
                <Empty title="No matching documents" body="Fetch a URL with POST /extract or the CLI fetch command to build local knowledge." />
              ) : (
                <Table className="mt-3">
                  <TableHeader><TableRow><TableHead>URL</TableHead><TableHead>Type</TableHead><TableHead>Status</TableHead><TableHead>Id</TableHead></TableRow></TableHeader>
                  <TableBody>
                    {docs.documents.map((doc) => (
                      <TableRow key={doc.id}>
                        <TableCell>{doc.canonical_url}</TableCell>
                        <TableCell>{doc.content_type ?? "--"}</TableCell>
                        <TableCell><Badge variant="muted">{doc.status ?? "active"}</Badge></TableCell>
                        <TableCell>{doc.id}</TableCell>
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
                  <Label htmlFor="memQuery">Query</Label>
                  <Input id="memQuery" value={memQuery} onChange={(e) => setMemQuery(e.target.value)} required />
                  <span className="field-helper">Namespaced entries visible to the current workspace only.</span>
                </div>
                <Button type="submit" loading={memBusy}>Search</Button>
              </div>
            </form>
            {memError ? <div className="mt-3"><ErrorNotice message={memError} /></div> : null}
            {!memBusy && memory ? (
              memory.results.length === 0 ? (
                <Empty title="No memory hits" body="Memory entries are created by agents through the memory service." />
              ) : (
                <Table className="mt-3">
                  <TableHeader><TableRow><TableHead>Entry</TableHead><TableHead>Score</TableHead><TableHead>URL</TableHead></TableRow></TableHeader>
                  <TableBody>
                    {memory.results.map((hit, i) => (
                      <TableRow key={`${hit.entry?.id ?? i}`}>
                        <TableCell><div className="row-title">{hit.entry?.title ?? "Untitled entry"}</div><div className="row-body">{hit.entry?.content ?? ""}</div></TableCell>
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
                <TabsTrigger value="growth">Growth</TabsTrigger>
                <TabsTrigger value="freshness">Freshness</TabsTrigger>
                <TabsTrigger value="profiles">Profiles</TabsTrigger>
                <TabsTrigger value="health">Health</TabsTrigger>
              </TabsList>
              <TabsContent value="growth" className="pt-3">
                <div className="stack">
                  <Kv label="Days with documents" value={count(growth.data?.data?.items?.length)} />
                  <Kv label="Chunks" value={count(growth.data?.data?.chunks)} />
                  <Kv label="Embeddings" value={count(growth.data?.data?.embeddings)} />
                  <Separator />
                  {(growth.data?.data?.items ?? []).slice(-7).map(([day, n]) => (
                    <div className="kv" key={day}><dt>{day}</dt><dd className="mono">{count(n)}</dd></div>
                  ))}
                </div>
              </TabsContent>
              <TabsContent value="freshness" className="pt-3">
                <Badge variant="warn">Not instrumented in MVP</Badge>
                <p className="mt-3 text-xs text-muted-foreground">{String(freshness.data?.data?.reason ?? "Freshness classification is not instrumented. Missing values are null, not zero.")}</p>
              </TabsContent>
              <TabsContent value="profiles" className="pt-3">
                <Table>
                  <TableHeader><TableRow><TableHead>Id</TableHead><TableHead>Freshness</TableHead><TableHead>Mode</TableHead><TableHead>Version</TableHead></TableRow></TableHeader>
                  <TableBody>
                    {(profiles.data?.profiles ?? []).map((p) => (
                      <TableRow key={p.id}><TableCell>{p.id}</TableCell><TableCell>{p.freshness ?? "--"}</TableCell><TableCell>{p.default_mode ?? "--"}</TableCell><TableCell>{p.version ?? "--"}</TableCell></TableRow>
                    ))}
                  </TableBody>
                </Table>
              </TabsContent>
              <TabsContent value="health" className="pt-3">
                <div className="stack">
                  <Kv label="Database" value={system.data?.database_ok ? "ok" : "unavailable"} />
                  <Kv label="Index" value={`${count(system.data?.index?.tantivy_chunks)} full-text / ${count(system.data?.index?.sqlite_chunks)} SQLite chunks`} />
                  <Kv label="Consistent" value={system.data?.index?.consistent ? "yes" : "run index rebuild"} />
                  <Kv label="Providers" value={(system.data?.providers ?? []).join(", ") || "none configured"} />
                  <Kv label="Crawler extensions" value={count(system.data?.crawlers)} />
                  <Kv label="Extensions" value={count(system.data?.extensions)} />
                  <Kv label="Auth" value={system.data?.auth_required ? "bearer token required" : "open local dev"} />
                  <Kv label="Rate limit" value={`${count(system.data?.rate_limit_per_minute)} requests / minute`} />
                </div>
              </TabsContent>
            </Tabs>
          </Panel>
        </div>
      </div>
    </>
  );
}
