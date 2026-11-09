"use client";

import {
  Area,
  AreaChart,
  Bar,
  BarChart,
  CartesianGrid,
  Cell,
  Pie,
  PieChart,
  ResponsiveContainer,
  Tooltip as ChartTooltip,
  XAxis,
  YAxis,
} from "recharts";

import { Card, CardContent, CardHeader, CardTitle } from "@/components/ui/card";

const CHART_COLORS = [
  "var(--chart-1)",
  "var(--chart-2)",
  "var(--chart-3)",
  "var(--chart-4)",
  "var(--chart-5)",
];

const tooltipStyle = {
  background: "var(--popover)",
  border: "1px solid var(--border)",
  color: "var(--popover-foreground)",
  fontFamily: "var(--font-mono)",
  fontSize: 11,
  borderRadius: 0,
};

export type SeriesDef = { key: string; label: string };

function ChartFrame({
  title,
  meta,
  children,
  table,
}: {
  title: string;
  meta?: string;
  children: React.ReactNode;
  table?: React.ReactNode;
}) {
  return (
    <Card className="panel">
      <CardHeader>
        <CardTitle>{title}</CardTitle>
        <span className="font-mono text-[10px] text-muted-foreground">{meta}</span>
      </CardHeader>
      <CardContent className="panel-body">
        {children}
        {table ? <div className="sr-only">{table}</div> : null}
      </CardContent>
    </Card>
  );
}

function NoData({ label }: { label: string }) {
  return (
    <div className="flex min-h-[180px] items-center justify-center border border-dashed border-border bg-muted/30 px-4 text-center font-mono text-[11px] text-muted-foreground">
      {label}. Missing data is shown as empty, not as zero.
    </div>
  );
}

/** Stacked area / line chart with Tooltip, time-range label and a data table. */
export function TimeSeriesChart({
  title,
  data,
  series,
  timeRange = "all available history",
  stacked = false,
}: {
  title: string;
  data: Record<string, unknown>[];
  series: SeriesDef[];
  timeRange?: string;
  stacked?: boolean;
}) {
  const rows = data ?? [];
  return (
    <ChartFrame
      title={title}
      meta={`TIME RANGE: ${timeRange}`}
      table={
        <table>
          <thead>
            <tr>
              <th>date</th>
              {series.map((s) => <th key={s.key}>{s.label}</th>)}
            </tr>
          </thead>
          <tbody>
            {rows.map((row, i) => (
              <tr key={i}>
                <td>{String(row.date ?? i)}</td>
                {series.map((s) => <td key={s.key}>{String(row[s.key] ?? "--")}</td>)}
              </tr>
            ))}
          </tbody>
        </table>
      }
    >
      {rows.length === 0 ? (
        <NoData label="No time-series buckets yet" />
      ) : (
        <>
          <ResponsiveContainer width="100%" height={220}>
            <AreaChart data={rows} margin={{ top: 8, right: 8, left: -18, bottom: 0 }}>
              <CartesianGrid strokeDasharray="3 3" stroke="var(--border)" />
              <XAxis dataKey="date" tick={{ fontSize: 10, fill: "var(--muted-foreground)" }} stroke="var(--border)" />
              <YAxis allowDecimals={false} tick={{ fontSize: 10, fill: "var(--muted-foreground)" }} stroke="var(--border)" />
              <ChartTooltip contentStyle={tooltipStyle} labelStyle={{ color: "var(--foreground)" }} />
              {series.map((s, i) => (
                <Area
                  key={s.key}
                  type="monotone"
                  dataKey={s.key}
                  name={s.label}
                  stackId={stacked ? "total" : undefined}
                  stroke={CHART_COLORS[i % CHART_COLORS.length]}
                  fill={CHART_COLORS[i % CHART_COLORS.length]}
                  fillOpacity={0.18}
                  strokeWidth={1.5}
                />
              ))}
            </AreaChart>
          </ResponsiveContainer>
          <div className="mt-3 flex flex-wrap gap-3 font-mono text-[10px] text-muted-foreground">
            {series.map((s, i) => (
              <span key={s.key} className="inline-flex items-center gap-1.5">
                <span className="h-2 w-2" style={{ background: CHART_COLORS[i % CHART_COLORS.length] }} />
                {s.label}
              </span>
            ))}
          </div>
        </>
      )}
    </ChartFrame>
  );
}

/** Donut distribution with Tooltip. */
export function DistributionDonut({
  title,
  items,
  meta,
}: {
  title: string;
  items: { name: string; value: number }[];
  meta?: string;
}) {
  const rows = items.filter((i) => i.value > 0);
  return (
    <ChartFrame title={title} meta={meta} table={<span>{rows.map((r) => `${r.name}=${r.value}`).join(", ")}</span>}>
      {rows.length === 0 ? (
        <NoData label="No strategy records yet" />
      ) : (
        <div className="flex flex-wrap items-center gap-4">
          <ResponsiveContainer width="55%" height={200}>
            <PieChart>
              <Pie data={rows} dataKey="value" nameKey="name" innerRadius={45} outerRadius={75} strokeWidth={1}>
                {rows.map((_, i) => (
                  <Cell key={i} fill={CHART_COLORS[i % CHART_COLORS.length]} />
                ))}
              </Pie>
              <ChartTooltip contentStyle={tooltipStyle} />
            </PieChart>
          </ResponsiveContainer>
          <div className="grid min-w-[140px] gap-1.5 font-mono text-[10px]">
            {rows.map((r, i) => (
              <div key={r.name} className="flex items-center justify-between gap-3">
                <span className="inline-flex items-center gap-1.5 text-muted-foreground">
                  <span className="h-2 w-2" style={{ background: CHART_COLORS[i % CHART_COLORS.length] }} />
                  {r.name}
                </span>
                <span>{r.value}</span>
              </div>
            ))}
          </div>
        </div>
      )}
    </ChartFrame>
  );
}

/** Horizontal breakdown bar chart with Tooltip. */
export function BreakdownBars({
  title,
  items,
  meta,
}: {
  title: string;
  items: { name: string; value: number }[];
  meta?: string;
}) {
  const rows = items.slice(0, 12);
  return (
    <ChartFrame title={title} meta={meta} table={<span>{rows.map((r) => `${r.name}=${r.value}`).join(", ")}</span>}>
      {rows.length === 0 ? (
        <NoData label="No breakdown rows yet" />
      ) : (
        <ResponsiveContainer width="100%" height={220}>
          <BarChart data={rows} layout="vertical" margin={{ top: 4, right: 16, left: 8, bottom: 0 }}>
            <CartesianGrid strokeDasharray="3 3" stroke="var(--border)" horizontal={false} />
            <XAxis type="number" tick={{ fontSize: 10, fill: "var(--muted-foreground)" }} stroke="var(--border)" />
            <YAxis type="category" dataKey="name" width={110} tick={{ fontSize: 10, fill: "var(--muted-foreground)" }} stroke="var(--border)" />
            <ChartTooltip contentStyle={tooltipStyle} />
            <Bar dataKey="value" fill="var(--chart-1)" radius={0} />
          </BarChart>
        </ResponsiveContainer>
      )}
    </ChartFrame>
  );
}
