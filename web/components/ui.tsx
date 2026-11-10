"use client";

import type { ReactNode } from "react";
import { useI18n } from "@/lib/i18n";
import dynamic from "next/dynamic";
import { AlertTriangle, ArrowRight, ArrowUpRight, RefreshCw } from "lucide-react";

import {
  Card,
  CardContent,
  CardHeader,
  CardTitle,
} from "@/components/ui/card";
import { Badge as ShadcnBadge } from "@/components/ui/badge";
import { Button as ShadcnButton } from "@/components/ui/button";
import { Skeleton as ShadcnSkeleton } from "@/components/ui/skeleton";
import { Alert, AlertDescription, AlertTitle } from "@/components/ui/alert";
import { cn } from "@/lib/utils";

const TimeSeriesChart = dynamic(
  () => import("@/components/analytics-charts").then((m) => m.TimeSeriesChart),
  { ssr: false, loading: () => <ShadcnSkeleton className="h-[220px] w-full" /> },
);

export type Tone = "neutral" | "ok" | "warn" | "accent" | "muted";

const badgeVariant: Record<Tone, "outline" | "secondary" | "warn" | "default" | "muted"> = {
  neutral: "outline",
  ok: "outline",
  warn: "warn",
  accent: "default",
  muted: "muted",
};

export function Panel({
  title,
  meta,
  children,
  className = "",
  action,
}: {
  title: string;
  meta?: ReactNode;
  children: ReactNode;
  className?: string;
  action?: ReactNode;
}) {
  const { t } = useI18n();
  return (
    <Card className={cn("panel", className)}>
      <CardHeader>
        <CardTitle>{t(title)}</CardTitle>
        <div className="panel-meta">
          {meta}
          {action}
        </div>
      </CardHeader>
      <CardContent className="panel-body">{children}</CardContent>
    </Card>
  );
}

export function Metric({
  label,
  value,
  unit,
  note,
  tone = "neutral",
}: {
  label: string;
  value: string;
  unit?: string;
  note?: string;
  tone?: Tone;
}) {
  return (
    <div className={cn("metric", tone === "accent" && "metric-accent")}>
      <div className="metric-label">{label}</div>
      <div className="metric-value">
        <span>{value}</span>
        {unit ? <span className="metric-unit">{unit}</span> : null}
      </div>
      {note ? <div className="metric-note">{note}</div> : null}
    </div>
  );
}

export function Badge({ children, tone = "neutral" }: { children: ReactNode; tone?: Tone }) {
  return <ShadcnBadge variant={badgeVariant[tone]}>{children}</ShadcnBadge>;
}

export function Kv({ label, value, mono = true }: { label: string; value: ReactNode; mono?: boolean }) {
  return (
    <div className="kv">
      <dt>{label}</dt>
      <dd className={mono ? "mono" : ""}>{value}</dd>
    </div>
  );
}

export function Skeleton({ lines = 3 }: { lines?: number }) {
  return (
    <div className="skeleton" aria-hidden="true">
      {Array.from({ length: lines }, (_, i) => (
        <ShadcnSkeleton className="skeleton-line" key={i} style={{ width: `${92 - i * 14}%` }} />
      ))}
    </div>
  );
}

export function Empty({ title, body }: { title: string; body: string }) {
  const { t } = useI18n();
  return (
    <div className="state-empty">
      <div className="state-mark">[ ]</div>
      <div>
        <div className="state-title">{t(title)}</div>
        <p className="state-body">{t(body)}</p>
      </div>
    </div>
  );
}

export function ErrorNotice({ message, onRetry }: { message: string; onRetry?: () => void }) {
  const { t } = useI18n();
  return (
    <Alert variant="destructive">
      <AlertTriangle className="size-4" aria-hidden="true" />
      <AlertTitle>{t("Gateway request failed")}</AlertTitle>
      <AlertDescription>{message}</AlertDescription>
      {onRetry ? (
        <div className="col-start-2 mt-2">
          <ShadcnButton variant="outline" size="sm" onClick={onRetry}>
            <RefreshCw className="size-3.5" /> {t("Retry")}
          </ShadcnButton>
        </div>
      ) : null}
    </Alert>
  );
}

export function Button({
  children,
  loading = false,
  disabled,
  type = "button",
  onClick,
  variant = "default",
  size = "default",
  icon = true,
}: {
  children: ReactNode;
  loading?: boolean;
  disabled?: boolean;
  type?: "button" | "submit";
  onClick?: () => void;
  variant?: "default" | "outline" | "secondary" | "ghost" | "link" | "destructive";
  size?: "default" | "sm" | "lg" | "icon";
  icon?: boolean;
}) {
  return (
    <ShadcnButton type={type} variant={variant} size={size} disabled={disabled || loading} onClick={onClick}>
      <span>{loading ? "Running" : children}</span>
      {loading || !icon ? null : <ArrowRight className="size-4" strokeWidth={1.5} aria-hidden="true" />}
    </ShadcnButton>
  );
}

/** Recharts-backed percentile chart. No fake SVG decoration. */
export function Sparkline({ values, label }: { values: number[]; label: string }) {
  const data = values.map((v, i) => ({ date: ["P50", "P95", "P99"][i] ?? `P${i}`, value: v }));
  return (
    <TimeSeriesChart
      title={label}
      data={data}
      series={[{ key: "value", label: "latency ms" }]}
      timeRange="current process"
    />
  );
}

export function Measure({ label, value, display }: { label: string; value: number; display: string }) {
  const width = Math.max(0, Math.min(100, value * 100));
  return (
    <div className="measure">
      <div className="measure-head">
        <span>{label}</span>
        <span className="mono">{display}</span>
      </div>
      <div className="measure-track" aria-hidden="true">
        <div className="measure-fill" style={{ width: `${width}%` }} />
      </div>
    </div>
  );
}

export function SectionIntro({ title, description }: { title: string; description: string }) {
  const { t } = useI18n();
  return (
    <div className="intro">
      <h1>{t(title)}</h1>
      <p>{t(description)}</p>
    </div>
  );
}

export function InlineLink({ href, children }: { href: string; children: ReactNode }) {
  return (
    <a className="inline-link" href={href}>
      {children}
      <ArrowUpRight size={13} strokeWidth={1.5} aria-hidden="true" />
    </a>
  );
}
