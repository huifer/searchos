"use client";

import { useState, type FormEvent } from "react";

import { Badge } from "@/components/ui/badge";
import { Input } from "@/components/ui/input";
import { Label } from "@/components/ui/label";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Button, Empty, ErrorNotice, Kv, Metric, Panel, SectionIntro, Skeleton } from "@/components/ui";
import { useCostBreakdown } from "@/lib/analytics";
import { apiPost, count, micros, pct, useApi } from "@/lib/client";
import type { CostSummary, ReservationList, SavingsReport } from "@/lib/types";

type MoneyLike = { amount_micros?: number } | null | undefined;
function moneyMicros(m: MoneyLike): number | undefined { return m?.amount_micros; }

export default function CostPage() {
  const savings = useApi<SavingsReport>("cost/savings", 30_000);
  const summary = useApi<{ summary: CostSummary; is_estimate: boolean }>("cost/summary", 30_000);
  const reservations = useApi<ReservationList>("cost/reservations", 30_000);
  const breakdown = useCostBreakdown();

  const [eventId, setEventId] = useState("");
  const [amount, setAmount] = useState("0.005");
  const [source, setSource] = useState("provider_billing");
  const [message, setMessage] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);

  async function reconcile(event: FormEvent<HTMLFormElement>) {
    event.preventDefault();
    setBusy(true);
    setError(null);
    setMessage(null);
    try {
      const result = await apiPost<{ reconciled: string; actual_cost_micros: number }>("cost/reconcile", {
        cost_event_id: eventId.trim(), actual_cost: Number(amount), source,
      });
      setMessage(`Reconciled ${result.reconciled} at ${result.actual_cost_micros} micros. The original estimate is unchanged.`);
      savings.reload();
      summary.reload();
      breakdown.reload();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setBusy(false);
    }
  }

  const anyError = savings.error || summary.error || reservations.error || breakdown.error;

  return (
    <>
      <SectionIntro
        title="Cost ledger"
        description="Estimated and actual costs stay separate. Savings use an equivalent-quality external-only baseline, and reservations follow reserve, execute, settle."
      />

      {anyError ? <div className="mb-6"><ErrorNotice message={anyError} onRetry={() => { savings.reload(); summary.reload(); reservations.reload(); breakdown.reload(); }} /></div> : null}
      {savings.loading && !savings.data ? <Panel title="Loading ledger"><Skeleton lines={4} /></Panel> : null}

      {savings.data ? (
        <div className="grid grid-4">
          <Metric label="Baseline" value={micros(savings.data.baseline_micros)} unit="USD" note="External-only equivalent quality" />
          <Metric label="SearchOS" value={micros(savings.data.searchos_micros)} unit="USD" note="Includes local compute" />
          <Metric label="Savings" value={micros(savings.data.savings_micros)} unit="USD" note={pct(savings.data.savings_rate)} tone="accent" />
          <Metric label="CPQPA" value={micros(savings.data.cost_per_quality_passing_answer_micros)} unit="USD" note={`${count(savings.data.quality_passing_tasks)} passing tasks`} />
        </div>
      ) : null}

      <div className="split">
        <div className="stack">
          <Panel title="Cost analytics" meta={<Badge variant="warn">Estimate</Badge>}>
            <Tabs defaultValue="operation">
              <TabsList>
                <TabsTrigger value="operation">Operation</TabsTrigger>
                <TabsTrigger value="provider">Provider</TabsTrigger>
                <TabsTrigger value="workspace">Workspace</TabsTrigger>
                <TabsTrigger value="agent">Agent</TabsTrigger>
              </TabsList>
              {(["by_operation", "by_provider", "by_workspace", "by_agent"] as const).map((key) => (
                <TabsContent key={key} value={key.replace("by_", "")} className="pt-3">
                  {(breakdown.data?.data?.[key] ?? []).length === 0 ? (
                    <Empty title="No cost events" body="Run a search to populate the analytics cost breakdown." />
                  ) : (
                    <Table>
                      <TableHeader><TableRow><TableHead>Key</TableHead><TableHead>Events</TableHead><TableHead>Estimated micros</TableHead></TableRow></TableHeader>
                      <TableBody>
                        {(breakdown.data?.data?.[key] ?? []).slice(0, 20).map(([name, n, m]) => (
                          <TableRow key={name}><TableCell>{name}</TableCell><TableCell>{count(n)}</TableCell><TableCell>{micros(m)}</TableCell></TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  )}
                </TabsContent>
              ))}
            </Tabs>
            <Separator className="my-4" />
            <div className="stack">
              <Kv label="Total estimated" value={`${micros(moneyMicros(summary.data?.summary.total_estimated))} USD`} />
              <Kv label="Total actual" value={summary.data?.summary.total_actual ? `${micros(moneyMicros(summary.data.summary.total_actual))} USD` : "no billing data"} />
              <Kv label="Cost events" value={count(summary.data?.summary.event_count)} />
              <Kv label="Cost per successful query" value={`${micros(moneyMicros(summary.data?.summary.cost_per_successful_query))} USD`} />
            </div>
          </Panel>

          <Panel title="Reuse cost views" meta={<span>RFC-006 6.9</span>}>
            <div className="stack">
              <Kv label="Original acquisition" value={`${micros(savings.data?.reuse_views?.original_acquisition_micros)} USD`} />
              <Kv label="Reuse execution" value={`${micros(savings.data?.reuse_views?.reuse_execution_micros)} USD`} />
              <Kv label="Allocated amortized" value={`${micros(savings.data?.reuse_views?.amortized_micros)} USD`} />
              <Kv label="Marginal" value={`${micros(savings.data?.reuse_views?.marginal_micros)} USD`} />
              <Kv label="External calls / local resolved" value={`${count(savings.data?.external_calls)} / ${count(savings.data?.local_resolved_tasks)}`} />
            </div>
          </Panel>
        </div>

        <div className="stack">
          <Panel title="Provider billing reconciliation" meta={<span>POST /cost/reconcile</span>}>
            <form onSubmit={reconcile}>
              <div className="form-grid" style={{ gridTemplateColumns: "minmax(0, 1fr)" }}>
                <div className="field">
                  <Label htmlFor="eventId">Cost event id</Label>
                  <Input id="eventId" value={eventId} onChange={(e) => setEventId(e.target.value)} placeholder="cost_<uuid>" required />
                  <span className="field-helper">Find ids in the local SQLite cost_events table.</span>
                </div>
                <div className="field">
                  <Label htmlFor="amount">Actual billed amount (USD)</Label>
                  <Input id="amount" type="number" min="0" step="0.000001" value={amount} onChange={(e) => setAmount(e.target.value)} required />
                  <span className="field-helper">The estimate is never overwritten.</span>
                </div>
                <div className="field">
                  <Label htmlFor="source">Source</Label>
                  <Select value={source} onValueChange={setSource}>
                    <SelectTrigger id="source"><SelectValue /></SelectTrigger>
                    <SelectContent>
                      <SelectItem value="provider_billing">Provider billing</SelectItem>
                      <SelectItem value="local_measurement">Local measurement</SelectItem>
                      <SelectItem value="user">User import</SelectItem>
                    </SelectContent>
                  </Select>
                  <span className="field-helper">Billing data flips the report from estimate to actual.</span>
                </div>
              </div>
              <div className="form-actions">
                <Button type="submit" loading={busy} disabled={!eventId.trim()}>Reconcile</Button>
              </div>
            </form>
            {message ? <div className="form-message">{message}</div> : null}
            {error ? <div className="form-message">{error}</div> : null}
          </Panel>

          <Panel title="Budget reservations" meta={<span>{reservations.data?.reservations.length ?? 0} rows</span>}>
            {(reservations.data?.reservations.length ?? 0) === 0 ? (
              <Empty title="No reservations yet" body="Each executed task reserves estimated budget before external work, then settles with actual spend." />
            ) : (
              <Table>
                <TableHeader><TableRow><TableHead>Task</TableHead><TableHead>Status</TableHead><TableHead>Reserved</TableHead><TableHead>Spent</TableHead></TableRow></TableHeader>
                <TableBody>
                  {(reservations.data?.reservations ?? []).map((r) => (
                    <TableRow key={r.id}>
                      <TableCell>{r.task_id}</TableCell>
                      <TableCell><Badge variant={r.status === "settled" ? "outline" : "warn"}>{r.status ?? "--"}</Badge></TableCell>
                      <TableCell>{micros(r.estimated_cost_micros)}</TableCell>
                      <TableCell>{micros(r.spent_cost_micros)}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </Panel>
        </div>
      </div>
    </>
  );
}
