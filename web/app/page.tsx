"use client";

import { useCallback, useMemo, useState } from "react";
import Link from "next/link";
import dynamic from "next/dynamic";
import { Download } from "lucide-react";

import { MetricCard } from "@/components/kpi";
import { Skeleton as ShadcnSkeleton } from "@/components/ui/skeleton";

const TimeSeriesChart = dynamic(
  () => import("@/components/analytics-charts").then((m) => m.TimeSeriesChart),
  { ssr: false, loading: () => <ShadcnSkeleton className="h-[220px] w-full" /> },
);
const DistributionDonut = dynamic(
  () => import("@/components/analytics-charts").then((m) => m.DistributionDonut),
  { ssr: false, loading: () => <ShadcnSkeleton className="h-[200px] w-full" /> },
);
const BreakdownBars = dynamic(
  () => import("@/components/analytics-charts").then((m) => m.BreakdownBars),
  { ssr: false, loading: () => <ShadcnSkeleton className="h-[220px] w-full" /> },
);
import { Badge } from "@/components/ui/badge";
import { Button } from "@/components/ui/button";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { ErrorNotice, Panel, SectionIntro, Skeleton } from "@/components/ui";
import {
  useAnalyticsEvents,
  useCostBreakdown,
  useOverview,
  useQualityDimensions,
  useSearchRequests,
  useSearchStrategies,
  useSearchTimeseries,
} from "@/lib/analytics";
import { count, micros, pct } from "@/lib/client";

export default function OverviewPage() {
  const [range, setRange] = useState("all");
  const [interval, setInterval] = useState("day");
  const params = useMemo(() => {
    const now = new Date();
    const from = new Date(now.getTime() - (range === "all" ? 365 : Number(range)) * 86_400_000);
    return {
      from: range === "all" ? undefined : from.toISOString(),
      to: range === "all" ? undefined : now.toISOString(),
      interval,
      timezone: "UTC",
    };
  }, [range, interval]);

  const overview = useOverview(params, 15_000);
  const series = useSearchTimeseries(params);
  const strategies = useSearchStrategies(params);
  const requests = useSearchRequests(params);
  const cost = useCostBreakdown(params);
  const quality = useQualityDimensions(params);

  const reloadAll = useCallback(() => {
    overview.reload();
    series.reload();
    strategies.reload();
    requests.reload();
    cost.reload();
    quality.reload();
  }, [overview, series, strategies, requests, cost, quality]);
  useAnalyticsEvents(reloadAll);

  const data = overview.data?.data;
  const meta = overview.data?.meta;
  const anyError =
    overview.error || series.error || strategies.error || requests.error || cost.error || quality.error;

  return (
    <>
      <SectionIntro
        title="System overview"
        description="Aggregated by the Analytics Read Model, not by direct table reads. Every KPI carries its denominator, definition, time range and drill-down."
      />

      <div className="mb-6 flex flex-wrap items-center justify-between gap-3">
        <div className="flex flex-wrap items-center gap-2">
          <Select name="time-range" value={range} onValueChange={setRange}>
            <SelectTrigger id="time-range" name="time-range" className="w-[150px]" aria-label="Time range">
              <SelectValue placeholder="Time range" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="7">Last 7 days</SelectItem>
              <SelectItem value="30">Last 30 days</SelectItem>
              <SelectItem value="all">All history</SelectItem>
            </SelectContent>
          </Select>
          <Select name="interval" value={interval} onValueChange={setInterval}>
            <SelectTrigger id="interval" name="interval" className="w-[130px]" aria-label="Interval">
              <SelectValue placeholder="Interval" />
            </SelectTrigger>
            <SelectContent>
              <SelectItem value="hour">Hour</SelectItem>
              <SelectItem value="day">Day</SelectItem>
              <SelectItem value="week">Week</SelectItem>
            </SelectContent>
          </Select>
          <Badge variant="outline">Aggregation v{meta?.aggregation_version ?? "--"}</Badge>
          <Badge variant={meta?.data_freshness === "complete" ? "outline" : "warn"}>
            {meta?.data_freshness ?? "--"}
          </Badge>
        </div>
        <div className="flex items-center gap-2">
          <Button onClick={reloadAll}>Refresh</Button>
          <a href="/api/searchos/api/v1/dashboard/export?format=csv" download>
            <Button variant="outline">
              <Download className="size-3.5" /> CSV
            </Button>
          </a>
          <a href="/api/searchos/api/v1/dashboard/export?format=json" download>
            <Button variant="outline">
              <Download className="size-3.5" /> JSON
            </Button>
          </a>
        </div>
      </div>

      {anyError ? <div className="mb-6"><ErrorNotice message={anyError} onRetry={reloadAll} /></div> : null}
      {overview.loading && !data ? <Panel title="Loading snapshot"><Skeleton lines={5} /></Panel> : null}

      {data ? (
        <>
          <div className="grid grid-4">
            <MetricCard title="Total searches" value={count(data.searches?.total)} denominator={`${count(data.searches?.completed)} completed / ${count(data.searches?.failed)} failed / ${count(data.searches?.partial)} partial`} definition="Count of search_tasks rows in the Analytics Read Model." drilldown="/analytics" />
            <MetricCard title="Local resolution" value={pct(data.local_resolution?.rate)} denominator={`${count(data.local_resolution?.count)} / ${count(data.local_resolution?.denominator)}`} definition={data.local_resolution?.definition} drilldown={data.local_resolution?.drilldown} />
            <MetricCard title="External search" value={pct(data.external_search?.rate)} denominator={`${count(data.external_search?.count)} / ${count(data.external_search?.denominator)}`} definition={data.external_search?.definition} drilldown="/analytics" />
            <MetricCard title="Quality pass rate" value={pct(data.quality?.pass_rate)} denominator={`${count(data.quality?.passed)} / ${count(data.quality?.denominator)}`} definition={data.quality?.definition} drilldown="/analytics" />
            <MetricCard title="Total cost" value={micros(data.cost?.estimated_micros)} unit="USD" denominator={data.cost?.is_estimate ? "estimated, not invoiced" : "reconciled actual"} definition={data.cost?.definition} drilldown="/cost" estimate={data.cost?.is_estimate} />
            <MetricCard title="Estimated savings" value={micros(data.cost?.estimated_savings_micros)} unit="USD" denominator={`baseline ${micros(data.cost?.baseline_micros)} USD`} definition="Baseline minus full SearchOS cost. Only quality-valid tasks count." drilldown="/cost" estimate tone="accent" />
            <MetricCard title="Knowledge documents" value={count(data.knowledge?.documents)} denominator={`${count(data.knowledge?.chunks)} chunks / ${count(data.knowledge?.embeddings)} embeddings`} definition={data.knowledge?.definition} drilldown="/knowledge" />
            <MetricCard title="Evidence reuse rate" value={pct(data.evidence_reuse_rate)} denominator={`${count(data.local_resolution?.count)} local / ${count(data.searches?.total)} total`} definition="Share of tasks served by local evidence. Not a success metric unless the quality gate passes." drilldown="/analytics" />
          </div>

          <div className="split">
            <div className="stack">
              <TimeSeriesChart
                title="Search activity"
                data={(series.data?.data?.items ?? []) as Record<string, unknown>[]}
                series={[
                  { key: "local", label: "Local" },
                  { key: "external", label: "External" },
                  { key: "failed", label: "Failed" },
                ]}
                timeRange={range === "all" ? "all available history" : `last ${range} days`}
                stacked
              />
              <Panel title="Recent search requests" meta={<Link href="/analytics" className="inline-link">Analytics</Link>}>
                <Table>
                  <TableHeader>
                    <TableRow>
                      <TableHead>Query</TableHead>
                      <TableHead>Mode</TableHead>
                      <TableHead>Strategy</TableHead>
                      <TableHead>Status</TableHead>
                    </TableRow>
                  </TableHeader>
                  <TableBody>
                    {(requests.data?.data?.items ?? []).slice(0, 8).map((row) => (
                      <TableRow key={row.request_id}>
                        <TableCell>{row.query}</TableCell>
                        <TableCell>{row.mode}</TableCell>
                        <TableCell>{row.final_strategy}</TableCell>
                        <TableCell><Badge variant={row.status === "completed" ? "outline" : "warn"}>{row.status}</Badge></TableCell>
                      </TableRow>
                    ))}
                  </TableBody>
                </Table>
                {(requests.data?.data?.items?.length ?? 0) === 0 ? (
                  <p className="mt-3 font-mono text-[11px] text-muted-foreground">No search requests recorded yet.</p>
                ) : null}
              </Panel>
            </div>

            <div className="stack">
              <DistributionDonut
                title="Decision strategies"
                items={(strategies.data?.data?.items ?? []).map(([name, value]) => ({ name, value }))}
                meta="RFC-004 v1.1"
              />
              <Panel title="Metric definitions and sources">
                <Tabs defaultValue="quality">
                  <TabsList>
                    <TabsTrigger value="quality">Quality</TabsTrigger>
                    <TabsTrigger value="cost">Cost</TabsTrigger>
                    <TabsTrigger value="meta">Meta</TabsTrigger>
                  </TabsList>
                  <TabsContent value="quality" className="pt-3">
                    <Table>
                      <TableHeader><TableRow><TableHead>Dimension</TableHead><TableHead>Count</TableHead></TableRow></TableHeader>
                      <TableBody>
                        {(quality.data?.data?.freshness ?? []).map(([k, v]) => (
                          <TableRow key={k}><TableCell>freshness: {k}</TableCell><TableCell>{v}</TableCell></TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </TabsContent>
                  <TabsContent value="cost" className="pt-3">
                    <Table>
                      <TableHeader><TableRow><TableHead>Operation</TableHead><TableHead>Events</TableHead><TableHead>Micros</TableHead></TableRow></TableHeader>
                      <TableBody>
                        {(cost.data?.data?.by_operation ?? []).slice(0, 8).map(([k, n, m]) => (
                          <TableRow key={k}><TableCell>{k}</TableCell><TableCell>{n}</TableCell><TableCell>{m}</TableCell></TableRow>
                        ))}
                      </TableBody>
                    </Table>
                  </TabsContent>
                  <TabsContent value="meta" className="pt-3">
                    <div className="stack">
                      <div className="kv"><dt>Generated at</dt><dd className="mono">{meta?.generated_at ?? "--"}</dd></div>
                      <div className="kv"><dt>Timezone</dt><dd className="mono">{meta?.time_range?.timezone ?? "UTC"}</dd></div>
                      <div className="kv"><dt>Interval</dt><dd className="mono">{meta?.interval ?? "--"}</dd></div>
                      <div className="kv"><dt>Workspace scope</dt><dd className="mono">{meta?.workspace_scope ?? "*"}</dd></div>
                      <div className="kv"><dt>Freshness</dt><dd className="mono">{meta?.data_freshness ?? "--"}</dd></div>
                      <Separator />
                      <p className="text-xs text-muted-foreground">Estimated values are not invoices. Missing resource metrics are shown as null, never as zero.</p>
                    </div>
                  </TabsContent>
                </Tabs>
              </Panel>
            </div>
          </div>

          <div className="mt-6">
            <BreakdownBars
              title="Cost by provider (estimated micros)"
              items={(cost.data?.data?.by_provider ?? []).map(([name, , microsValue]) => ({ name, value: microsValue }))}
              meta="RFC-028 cost breakdown"
            />
          </div>
        </>
      ) : null}
    </>
  );
}
