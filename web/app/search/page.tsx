"use client";

import { useState, type FormEvent } from "react";
import { Info } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Textarea } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import {
  Select, SelectContent, SelectItem, SelectTrigger, SelectValue,
} from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Badge as LegacyBadge, Button, Empty, ErrorNotice, InlineLink, Kv, Panel, SectionIntro, Skeleton, type Tone } from "@/components/ui";
import { apiPost, micros, useApi } from "@/lib/client";
import type { ProfileList, SearchResponse } from "@/lib/types";

const MODES = [
  { id: "local_only", label: "Local only", hint: "No external calls allowed" },
  { id: "local_first", label: "Local first", hint: "Default path" },
  { id: "external_first", label: "External first", hint: "Local evidence supplements" },
  { id: "external_only", label: "External only", hint: "Cache bypassed" },
  { id: "hybrid", label: "Hybrid", hint: "Local and external merged" },
];

function statusTone(status?: string): Tone {
  switch (status) {
    case "completed": return "neutral";
    case "completed_with_warnings":
    case "partial": return "warn";
    case "insufficient_evidence":
    case "budget_exhausted":
    case "failed": return "accent";
    default: return "muted";
  }
}

export default function SearchPage() {
  const profiles = useApi<ProfileList>("profiles");
  const [query, setQuery] = useState("What is the Tokio async runtime?");
  const [profile, setProfile] = useState("general");
  const [mode, setMode] = useState("local_first");
  const [workspace, setWorkspace] = useState("default");
  const [maxQueries, setMaxQueries] = useState("3");
  const [maxCost, setMaxCost] = useState("0.05");
  const [requestId, setRequestId] = useState("");
  const [response, setResponse] = useState<SearchResponse | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [history, setHistory] = useState<{ query: string; status: string; path: string; trace?: string }[]>([]);

  async function run(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setLoading(true);
    setError(null);
    const body: Record<string, unknown> = {
      query, profile, mode,
      workspace_id: workspace || "default",
      max_external_queries: Number(maxQueries) || 0,
      max_total_cost: Number(maxCost) || 0,
    };
    if (requestId.trim()) body.request_id = requestId.trim();
    try {
      const result = await apiPost<SearchResponse>("search", body);
      setResponse(result);
      setHistory((prev) => [
        { query, status: result.status ?? "unknown", path: result.decision?.path ?? "unknown", trace: result.trace_id },
        ...prev,
      ].slice(0, 5));
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setLoading(false);
    }
  }

  const activeMode = MODES.find((m) => m.id === mode);

  return (
    <>
      <SectionIntro
        title="Search workbench"
        description="Submit a query through the shared application service. The response carries an explicit task status, quality verdict, cost, warnings and an explainable trace."
      />

      <div className="split">
        <div className="stack">
          <Panel title="Request" meta={<span>POST /search</span>}>
            <form onSubmit={run}>
              <div className="form-grid">
                <div className="field field-wide">
                  <Label htmlFor="query">Query</Label>
                  <Textarea id="query" value={query} onChange={(e) => setQuery(e.target.value)} required placeholder="Ask a question or paste a URL" />
                  <span className="field-helper">Strict freshness queries bypass the exact cache automatically.</span>
                </div>

                <div className="field">
                  <Label htmlFor="profile">Profile</Label>
                  <Select value={profile} onValueChange={setProfile}>
                    <SelectTrigger id="profile"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {(profiles.data?.profiles ?? [{ id: "general" }]).map((p) => (
                        <SelectItem key={p.id} value={p.id}>{p.id} / {p.default_mode ?? "local_first"}</SelectItem>
                      ))}
                    </SelectContent>
                  </Select>
                  <span className="field-helper">Versioned quality and freshness policy.</span>
                </div>

                <div className="field">
                  <Label htmlFor="mode">Mode</Label>
                  <Select value={mode} onValueChange={setMode}>
                    <SelectTrigger id="mode"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      {MODES.map((m) => <SelectItem key={m.id} value={m.id}>{m.label}</SelectItem>)}
                    </SelectContent>
                  </Select>
                  <span className="field-helper">{activeMode?.hint}</span>
                </div>

                <div className="field">
                  <Label htmlFor="workspace">Workspace</Label>
                  <Input id="workspace" value={workspace} onChange={(e) => setWorkspace(e.target.value)} />
                  <span className="field-helper">Knowledge namespace and cost attribution scope.</span>
                </div>

                <div className="field">
                  <Label htmlFor="requestId">Request id</Label>
                  <Input id="requestId" value={requestId} onChange={(e) => setRequestId(e.target.value)} placeholder="Optional idempotency key" />
                  <span className="field-helper">Replaying a key does not create a second bill.</span>
                </div>

                <div className="field">
                  <Label htmlFor="maxQueries">Max external queries</Label>
                  <Input id="maxQueries" type="number" min="0" max="20" value={maxQueries} onChange={(e) => setMaxQueries(e.target.value)} />
                  <span className="field-helper">Hard cap on targeted gap queries.</span>
                </div>

                <div className="field">
                  <Label htmlFor="maxCost">Max total cost (USD)</Label>
                  <Input id="maxCost" type="number" min="0" step="0.001" value={maxCost} onChange={(e) => setMaxCost(e.target.value)} />
                  <span className="field-helper">Budget exhaustion returns an explicit status.</span>
                </div>
              </div>

              <div className="form-actions">
                <Button type="submit" loading={loading}>Run search</Button>
                <Tooltip>
                  <TooltipTrigger asChild>
                    <span className="inline-flex cursor-help items-center gap-1 text-[11px] text-muted-foreground">
                      <Info className="size-3" /> Mode semantics
                    </span>
                  </TooltipTrigger>
                  <TooltipContent>Local-only mode never calls an external provider. External-only always bypasses the exact cache.</TooltipContent>
                </Tooltip>
              </div>
            </form>
            {error ? <div className="mt-4"><ErrorNotice message={error} /></div> : null}
            {profiles.error ? <div className="form-message">Profile list unavailable: {profiles.error}</div> : null}
          </Panel>

          {history.length > 0 ? (
            <Panel title="Recent runs" meta={<span>this session</span>}>
              <div className="row-list">
                {history.map((h, i) => (
                  <div className="row" key={`${h.query}-${i}`}>
                    <div className="row-title">{h.query}</div>
                    <div className="row-meta">
                      <LegacyBadge tone={statusTone(h.status)}>{h.status}</LegacyBadge>
                      <span>{h.path}</span>
                      {h.trace ? <InlineLink href={`/traces?trace=${encodeURIComponent(h.trace)}`}>Inspect trace</InlineLink> : null}
                    </div>
                  </div>
                ))}
              </div>
            </Panel>
          ) : null}
        </div>

        <div className="stack">
          {loading ? <Panel title="Executing"><Skeleton lines={6} /></Panel> : null}
          {!loading && !response && !error ? (
            <Panel title="Response">
              <Empty title="No response yet" body="Submit a query to see the decision path, evidence, quality verdict and cost attribution." />
            </Panel>
          ) : null}

          {response ? (
            <>
              <Panel title="Decision" meta={<LegacyBadge tone={statusTone(response.status)}>{response.status ?? "unknown"}</LegacyBadge>}>
                <Tabs defaultValue="summary">
                  <TabsList>
                    <TabsTrigger value="summary">Summary</TabsTrigger>
                    <TabsTrigger value="raw">Raw JSON</TabsTrigger>
                  </TabsList>
                  <TabsContent value="summary" className="pt-3">
                    <div className="stack">
                      <Kv label="Path" value={response.decision?.path ?? "--"} />
                      <Kv label="Reason" value={response.decision?.reason ?? "--"} />
                      <Kv label="Quality" value={response.quality?.status ?? "--"} />
                      <Kv label="Freshness" value={response.quality?.freshness ?? "--"} />
                      <Kv label="Coverage" value={response.quality?.coverage ?? "--"} />
                      <Kv label="Estimated cost" value={`${micros(response.cost?.estimated === undefined ? undefined : Math.round(response.cost.estimated * 1_000_000))} ${response.cost?.currency ?? "USD"}`} />
                      <Kv label="Task id" value={response.task_id ?? "--"} />
                      <Kv label="Replayed" value={response.replayed ? "yes" : "no"} />
                    </div>
                    {response.warnings && response.warnings.length > 0 ? (
                      <div className="form-message">Warnings: {response.warnings.join(" / ")}</div>
                    ) : null}
                    {response.trace_id ? (
                      <div className="mt-4"><InlineLink href={`/traces?trace=${encodeURIComponent(response.trace_id)}`}>Inspect decision trace</InlineLink></div>
                    ) : null}
                  </TabsContent>
                  <TabsContent value="raw" className="pt-3">
                    <pre className="code">{JSON.stringify(response, null, 2)}</pre>
                  </TabsContent>
                </Tabs>
              </Panel>

              <Panel title="Evidence" meta={<span>{response.results.length} results / {response.provenance?.length ?? 0} provenance</span>}>
                {response.results.length === 0 ? (
                  <Empty title="No evidence returned" body="An insufficient or budget-exhausted task returns an explicit status instead of empty success." />
                ) : (
                  <Table>
                    <TableHeader><TableRow><TableHead>Result</TableHead><TableHead>Source</TableHead><TableHead>Score</TableHead></TableRow></TableHeader>
                    <TableBody>
                      {response.results.map((item, i) => (
                        <TableRow key={`${item.url}-${i}`}>
                          <TableCell>
                            <div className="row-title">{item.title ?? item.url}</div>
                            <div className="row-url">{item.url}</div>
                            <div className="row-body">{item.content}</div>
                          </TableCell>
                          <TableCell><Badge variant="muted">{item.source}</Badge></TableCell>
                          <TableCell>{item.score.toFixed(3)}</TableCell>
                        </TableRow>
                      ))}
                    </TableBody>
                  </Table>
                )}
                <Separator className="my-3" />
                <p className="text-xs text-muted-foreground">Snippets are untrusted evidence, never instructions. Provenance records remain attached to the response.</p>
              </Panel>
            </>
          ) : null}
        </div>
      </div>
    </>
  );
}
