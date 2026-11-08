"use client";

import { useMemo, useState } from "react";
import { Database, HardDrive, Info, Server } from "lucide-react";

import { BreakdownBars, DistributionDonut, TimeSeriesChart } from "@/components/analytics-charts";
import { Badge } from "@/components/ui/badge";
import { Select, SelectContent, SelectItem, SelectTrigger, SelectValue } from "@/components/ui/select";
import { Separator } from "@/components/ui/separator";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { Tabs, TabsContent, TabsList, TabsTrigger } from "@/components/ui/tabs";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { Empty, ErrorNotice, Panel, SectionIntro, Skeleton } from "@/components/ui";
import {
  useCostBreakdown,
  useCrawlers,
  useInfrastructure,
  useKnowledgeFreshness,
  useKnowledgeGrowth,
  useProviders,
  useQualityDimensions,
  useSearchRequests,
  useSearchStrategies,
  useSearchTimeseries,
} from "@/lib/analytics";
import { count, micros } from "@/lib/client";

function CountTable({
  title,
  rows,
  valueLabel = "Count",
  empty,
}: {
  title: string;
  rows: [string, number][];
  valueLabel?: string;
  empty?: string;
}) {
  return (
    <Panel title={title}>
      {rows.length === 0 ? (
        <Empty title="No data in this dimension" body={empty ?? "The Read Model has no rows yet. Missing data is not shown as zero."} />
      ) : (
        <Table>
          <TableHeader><TableRow><TableHead>Key</TableHead><TableHead>{valueLabel}</TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map(([key, value]) => (
              <TableRow key={key}><TableCell>{key}</TableCell><TableCell>{count(value)}</TableCell></TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </Panel>
  );
}

function CostTripleTable({
  title,
  rows,
}: {
  title: string;
  rows: [string, number, number][];
}) {
  return (
    <Panel title={title} meta={<Badge variant="warn">Estimate</Badge>}>
      {rows.length === 0 ? (
        <Empty title="No cost events" body="Run a search to populate the cost ledger." />
      ) : (
        <Table>
          <TableHeader><TableRow><TableHead>Key</TableHead><TableHead>Events</TableHead><TableHead>Micros</TableHead></TableRow></TableHeader>
          <TableBody>
            {rows.map(([key, n, m]) => (
              <TableRow key={key}><TableCell>{key}</TableCell><TableCell>{count(n)}</TableCell><TableCell>{micros(m)}</TableCell></TableRow>
            ))}
          </TableBody>
        </Table>
      )}
    </Panel>
  );
}

export default function AnalyticsPage() {
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

  const series = useSearchTimeseries(params);
  const strategies = useSearchStrategies(params);
  const requests = useSearchRequests(params);
  const cost = useCostBreakdown(params);
  const quality = useQualityDimensions(params);
  const knowledge = useKnowledgeGrowth(params);
  const freshness = useKnowledgeFreshness(params);
  const providers = useProviders(params);
  const crawlers = useCrawlers(params);
  const infra = useInfrastructure(params);

  const anyError =
    series.error || strategies.error || requests.error || cost.error || quality.error ||
    knowledge.error || freshness.error || providers.error || crawlers.error || infra.error;

  return (
    <>
      <SectionIntro
        title="Analytics"
        description="RFC-026 to RFC-028: Search, Cost, Knowledge, Quality, Provider, Crawler and Infrastructure views all come from the same Analytics Read Model and standard Dashboard API."
      />

      <div className="mb-6 flex flex-wrap items-center gap-2">
        <Select name="time-range" value={range} onValueChange={setRange}>
          <SelectTrigger id="time-range" name="time-range" className="w-[150px]" aria-label="Time range"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="7">Last 7 days</SelectItem>
            <SelectItem value="30">Last 30 days</SelectItem>
            <SelectItem value="all">All history</SelectItem>
          </SelectContent>
        </Select>
        <Select name="interval" value={interval} onValueChange={setInterval}>
          <SelectTrigger id="interval" name="interval" className="w-[130px]" aria-label="Interval"><SelectValue /></SelectTrigger>
          <SelectContent>
            <SelectItem value="hour">Hour</SelectItem>
            <SelectItem value="day">Day</SelectItem>
            <SelectItem value="week">Week</SelectItem>
            <SelectItem value="month">Month</SelectItem>
          </SelectContent>
        </Select>
        <Badge variant="outline">UTC</Badge>
        <Badge variant="outline">Aggregation v1</Badge>
        <Tooltip>
          <TooltipTrigger asChild>
            <button type="button" aria-label="Read model information" className="inline-flex items-center gap-1 font-mono text-[10px] text-muted-foreground hover:text-foreground">
              <Info className="size-3" /> Read model
            </button>
          </TooltipTrigger>
          <TooltipContent>
            The frontend calls /api/v1/dashboard/*. The gateway queries analytics_read_models, never operational tables directly.
          </TooltipContent>
        </Tooltip>
      </div>

      {anyError ? <div className="mb-6"><ErrorNotice message={anyError} /></div> : null}

      <Tabs defaultValue="search">
        <TabsList className="mb-6 flex-wrap">
          <TabsTrigger value="search">Search</TabsTrigger>
          <TabsTrigger value="cost">Cost</TabsTrigger>
          <TabsTrigger value="quality">Quality</TabsTrigger>
          <TabsTrigger value="knowledge">Knowledge</TabsTrigger>
          <TabsTrigger value="providers">Providers</TabsTrigger>
          <TabsTrigger value="crawlers">Crawlers</TabsTrigger>
          <TabsTrigger value="infra">Infrastructure</TabsTrigger>
        </TabsList>

        <TabsContent value="search" className="stack">
          <div className="split">
            <TimeSeriesChart
              title="Search activity"
              data={(series.data?.data?.items ?? []) as Record<string, unknown>[]}
              series={[{ key: "local", label: "Local" }, { key: "external", label: "External" }, { key: "failed", label: "Failed" }]}
              timeRange={range === "all" ? "all available history" : `last ${range} days`}
              stacked
            />
            <DistributionDonut
              title="Strategy distribution"
              items={(strategies.data?.data?.items ?? []).map(([name, value]) => ({ name, value }))}
              meta="final_strategy"
            />
          </div>
          <Panel title="Search requests" meta={<span>Read Model / cursor-ready</span>}>
            {requests.loading && !requests.data ? <Skeleton lines={5} /> : null}
            {(requests.data?.data?.items?.length ?? 0) === 0 ? (
              <Empty title="No search requests" body="Run a query in the Search workbench to populate this table." />
            ) : (
              <Table>
                <TableHeader>
                  <TableRow>
                    <TableHead>Request</TableHead><TableHead>Query</TableHead><TableHead>Mode</TableHead>
                    <TableHead>Strategy</TableHead><TableHead>Status</TableHead><TableHead>Created</TableHead>
                  </TableRow>
                </TableHeader>
                <TableBody>
                  {(requests.data?.data?.items ?? []).slice(0, 50).map((r) => (
                    <TableRow key={r.request_id}>
                      <TableCell>{r.request_id}</TableCell><TableCell>{r.query}</TableCell>
                      <TableCell>{r.mode}</TableCell><TableCell>{r.final_strategy}</TableCell>
                      <TableCell><Badge variant={r.status === "completed" ? "outline" : "warn"}>{r.status}</Badge></TableCell>
                      <TableCell>{r.created_at}</TableCell>
                    </TableRow>
                  ))}
                </TableBody>
              </Table>
            )}
          </Panel>
        </TabsContent>

        <TabsContent value="cost" className="stack">
          <div className="grid grid-2">
            <CostTripleTable title="By operation" rows={cost.data?.data?.by_operation ?? []} />
            <CostTripleTable title="By provider" rows={cost.data?.data?.by_provider ?? []} />
            <CostTripleTable title="By workspace" rows={cost.data?.data?.by_workspace ?? []} />
            <CostTripleTable title="By agent" rows={cost.data?.data?.by_agent ?? []} />
          </div>
          <BreakdownBars title="Cost by provider" items={(cost.data?.data?.by_provider ?? []).map(([name, , m]) => ({ name, value: m }))} meta="estimated micros" />
        </TabsContent>

        <TabsContent value="quality" className="stack">
          <div className="grid grid-2">
            <CountTable title="Freshness status" rows={quality.data?.data?.freshness ?? []} />
            <CountTable title="Coverage status" rows={quality.data?.data?.coverage ?? []} />
            <CountTable title="Source status" rows={quality.data?.data?.source ?? []} />
            <CountTable title="Failure classes" rows={quality.data?.data?.failures ?? []} />
          </div>
        </TabsContent>

        <TabsContent value="knowledge" className="stack">
          <div className="split">
            <TimeSeriesChart
              title="Knowledge growth"
              data={(knowledge.data?.data?.items ?? []).map(([date, value]) => ({ date, documents: value }))}
              series={[{ key: "documents", label: "Documents" }]}
              timeRange="all available history"
            />
            <Panel title="Knowledge summary">
              <div className="stack">
                <div className="kv"><dt>Chunks</dt><dd className="mono">{count(knowledge.data?.data?.chunks)}</dd></div>
                <div className="kv"><dt>Embeddings</dt><dd className="mono">{count(knowledge.data?.data?.embeddings)}</dd></div>
                <Separator />
                <p className="text-xs text-muted-foreground">{String(freshness.data?.data?.reason ?? "Freshness classification is not instrumented in the MVP.")}</p>
                <Badge variant="warn">Missing values are null, not zero</Badge>
              </div>
            </Panel>
          </div>
        </TabsContent>

        <TabsContent value="providers" className="stack">
          <CostTripleTable title="Provider usage / cost" rows={providers.data?.data?.items ?? []} />
          <Panel title="Provider health">
            <p className="text-xs text-muted-foreground">{String(providers.data?.data?.health ?? "See /cost/providers for live circuit-breaker state.")}</p>
          </Panel>
        </TabsContent>

        <TabsContent value="crawlers" className="stack">
          <Panel title="Crawler summary">
            <div className="stack">
              <div className="kv"><dt>Total runs</dt><dd className="mono">{count(crawlers.data?.data?.total)}</dd></div>
              <div className="kv"><dt>Successful</dt><dd className="mono">{count(crawlers.data?.data?.successful)}</dd></div>
              <div className="kv"><dt>Failed</dt><dd className="mono">{count(crawlers.data?.data?.failed)}</dd></div>
              <Separator />
              <p className="text-xs text-muted-foreground">{String(crawlers.data?.data?.reason ?? "")}</p>
            </div>
          </Panel>
        </TabsContent>

        <TabsContent value="infra" className="stack">
          <Panel title="Infrastructure">
            <div className="stack">
              <div className="kv"><dt>SQLite size</dt><dd className="mono">{count(infra.data?.data?.sqlite_bytes)} bytes</dd></div>
              <div className="kv"><dt>Indexed chunks</dt><dd className="mono">{count(infra.data?.data?.index_chunks)}</dd></div>
              <Separator />
              <div className="flex items-start gap-3 text-xs text-muted-foreground">
                <Server className="mt-0.5 size-4" />
                <span>{String(infra.data?.data?.reason ?? "Resource sampling not instrumented in the MVP.")}</span>
              </div>
              <div className="flex items-start gap-3 text-xs text-muted-foreground">
                <HardDrive className="mt-0.5 size-4" />
                <span>CPU / GPU / memory / network metrics are intentionally null until a resource monitor is implemented.</span>
              </div>
              <div className="flex items-start gap-3 text-xs text-muted-foreground">
                <Database className="mt-0.5 size-4" />
                <span>No fake zeros. A missing metric is displayed as unavailable.</span>
              </div>
            </div>
          </Panel>
        </TabsContent>
      </Tabs>
    </>
  );
}
