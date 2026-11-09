"use client";

import Link from "next/link";
import { ArrowUpRight, Info } from "lucide-react";

import { Badge } from "@/components/ui/badge";
import { Card, CardContent } from "@/components/ui/card";
import { Tooltip, TooltipContent, TooltipTrigger } from "@/components/ui/tooltip";
import { cn } from "@/lib/utils";

/** RFC-026 6.1: every KPI carries a denominator, definition and drill-down. */
export function MetricCard({
  title,
  value,
  unit,
  denominator,
  definition,
  drilldown,
  estimate,
  tone = "default",
}: {
  title: string;
  value: string;
  unit?: string;
  denominator?: string;
  definition?: string;
  drilldown?: string;
  estimate?: boolean;
  tone?: "default" | "accent";
}) {
  return (
    <Card className="panel">
      <CardContent className="metric">
        <div className="flex items-center justify-between gap-2">
          <span className="metric-label">{title}</span>
          {estimate ? <Badge variant="warn">Estimate</Badge> : null}
        </div>
        <div className={cn("metric-value", tone === "accent" && "text-primary")}>
          <span>{value}</span>
          {unit ? <span className="metric-unit">{unit}</span> : null}
        </div>
        {denominator ? <div className="metric-note">Denominator: {denominator}</div> : null}
        <div className="mt-3 flex items-center justify-between gap-2">
          {definition ? (
            <Tooltip>
              <TooltipTrigger asChild>
                <button type="button" className="inline-flex items-center gap-1 font-mono text-[10px] text-muted-foreground hover:text-foreground" aria-label={`Definition of ${title}`}>
                  <Info className="size-3" /> Definition
                </button>
              </TooltipTrigger>
              <TooltipContent>{definition}</TooltipContent>
            </Tooltip>
          ) : <span />}
          {drilldown ? (
            <Link href={drilldown} className="inline-link">
              Drill down <ArrowUpRight className="size-3" />
            </Link>
          ) : null}
        </div>
      </CardContent>
    </Card>
  );
}
