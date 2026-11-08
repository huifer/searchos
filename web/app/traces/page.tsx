"use client";

import { useEffect, useState } from "react";

import { Badge } from "@/components/ui/badge";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Empty, ErrorNotice, Kv, Panel, SectionIntro, Skeleton, type Tone } from "@/components/ui";
import { micros, useApi } from "@/lib/client";
import type { DecisionTrace, TraceList } from "@/lib/types";

function stageTone(status: string): Tone {
  if (status === "completed") return "neutral";
  if (status === "partial" || status === "failed") return "warn";
  return "muted";
}

export default function TracesPage() {
  const list = useApi<TraceList>("traces?limit=25", 15_000);
  const [selected, setSelected] = useState<string | null>(null);
  const detail = useApi<DecisionTrace>(selected ? `trace/${encodeURIComponent(selected)}` : null);

  useEffect(() => {
    const fromUrl = new URLSearchParams(window.location.search).get("trace");
    if (fromUrl) setSelected(fromUrl);
  }, []);

  function choose(id: string) {
    setSelected(id);
    window.history.replaceState(null, "", `/traces?trace=${encodeURIComponent(id)}`);
  }

  return (
    <>
      <SectionIntro
        title="Decision traces"
        description="Every search decision produces an immutable, staged trace. Inspect why local evidence was accepted or rejected, which gaps were planned, and what the final cost was."
      />

      {list.error ? <div className="mb-6"><ErrorNotice message={list.error} onRetry={list.reload} /></div> : null}

      <div className="trace-layout">
        <div>
          <div className="panel-head">
            <h2 className="panel-title">Recent traces</h2>
            <span className="panel-meta">{list.data?.traces.length ?? 0} records</span>
          </div>
          {list.loading && !list.data ? <div className="p-4"><Skeleton lines={5} /></div> : null}
          {!list.loading && (list.data?.traces.length ?? 0) === 0 ? (
            <div className="p-4"><Empty title="No traces recorded" body="Run a search in the workbench. Each task writes a staged trace to SQLite." /></div>
          ) : null}
          {(list.data?.traces ?? []).map((t) => {
            const id = t.trace_id ?? "";
            return (
              <button type="button" key={id} className={`trace-item${id === selected ? " trace-item-active" : ""}`} onClick={() => choose(id)}>
                <span className="trace-id">{id}</span>
                <span className="trace-summary">{t.selected_path ?? "--"} / {t.final_strategy ?? "--"} / {t.intents?.join(",") || "no intent"}</span>
                <span className="trace-summary">{t.decision_at ?? "--"}</span>
              </button>
            );
          })}
        </div>

        <div>
          <div className="panel-head">
            <h2 className="panel-title">Trace detail</h2>
            <span className="panel-meta">GET /trace/{selected ?? "{id}"}</span>
          </div>

          {!selected ? (
            <div className="p-4"><Empty title="Select a trace" body="Choose a record on the left, or open a trace link from the Search workbench." /></div>
          ) : null}
          {selected && detail.loading && !detail.data ? <div className="p-4"><Skeleton lines={6} /></div> : null}
          {selected && detail.error ? <div className="p-4"><ErrorNotice message={detail.error} onRetry={detail.reload} /></div> : null}

          {detail.data ? (
            <div className="p-4">
              <div className="grid grid-3 mb-5">
                <div className="metric"><div className="metric-label">Selected path</div><div className="metric-value" style={{ fontSize: 20 }}>{detail.data.selected_path ?? "--"}</div></div>
                <div className="metric"><div className="metric-label">Final strategy</div><div className="metric-value" style={{ fontSize: 20 }}>{detail.data.final_strategy ?? "--"}</div></div>
                <div className="metric"><div className="metric-label">Final outcome</div><div className="metric-value" style={{ fontSize: 20 }}>{detail.data.final_outcome ?? "--"}</div></div>
              </div>

              <Tabs defaultValue="stages">
                <TabsList>
                  <TabsTrigger value="stages">Stages</TabsTrigger>
                  <TabsTrigger value="meta">Metadata</TabsTrigger>
                  <TabsTrigger value="raw">Raw JSON</TabsTrigger>
                </TabsList>
                <TabsContent value="stages" className="pt-3">
                  {(detail.data.stages ?? []).length === 0 ? (
                    <Empty title="No stage records" body="This trace was written by an older build. New decisions always include structured stages." />
                  ) : (
                    <Table>
                      <TableHeader><TableRow><TableHead>Stage</TableHead><TableHead>Output</TableHead><TableHead>Status</TableHead></TableRow></TableHeader>
                      <TableBody>
                        {(detail.data.stages ?? []).map((stage, i) => (
                          <TableRow key={`${stage.stage}-${i}`}>
                            <TableCell className="stage-name">{stage.stage.replaceAll("_", " ")}</TableCell>
                            <TableCell>
                              <div className="stage-detail">{stage.output_summary || stage.input_summary || "--"}</div>
                              {stage.reasons && stage.reasons.length > 0 ? <div className="stage-reasons">{stage.reasons.join(" / ")}</div> : null}
                            </TableCell>
                            <TableCell><Badge variant={stageTone(stage.status) === "warn" ? "warn" : stageTone(stage.status) === "muted" ? "muted" : "outline"}>{stage.status}</Badge></TableCell>
                          </TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  )}
                </TabsContent>
                <TabsContent value="meta" className="pt-3">
                  <div className="stack">
                    <Kv label="Trace id" value={detail.data.trace_id ?? "--"} />
                    <Kv label="Task id" value={detail.data.task_id ?? "--"} />
                    <Kv label="Profile version" value={detail.data.profile_version ?? "--"} />
                    <Kv label="Local candidates" value={`${detail.data.local_candidates ?? 0} / accepted ${detail.data.accepted_candidates ?? 0}`} />
                    <Kv label="Estimated cost" value={`${micros(detail.data.estimated_cost?.amount_micros)} ${detail.data.estimated_cost?.currency ?? "USD"}`} />
                    <Kv label="Actual cost" value={detail.data.actual_cost ? `${micros(detail.data.actual_cost.amount_micros)} ${detail.data.actual_cost.currency}` : "not reconciled"} />
                    <Kv label="Rejection reasons" value={(detail.data.rejection_reasons ?? []).join(" / ") || "none"} />
                  </div>
                  <Separator className="my-4" />
                  <p className="text-xs text-muted-foreground">Estimated and actual costs are separate. Replaying a request id never creates a second bill.</p>
                </TabsContent>
                <TabsContent value="raw" className="pt-3">
                  <pre className="code">{JSON.stringify(detail.data, null, 2)}</pre>
                </TabsContent>
              </Tabs>
            </div>
          ) : null}
        </div>
      </div>
    </>
  );
}
