"use client";

import Link from "next/link";
import { usePathname } from "next/navigation";
import {
  Activity,
  ChartNoAxesCombined,
  Database,
  DollarSign,
  FileText,
  Search,
} from "lucide-react";
import type { ReactNode } from "react";

import { ThemeToggle } from "@/components/theme-toggle";
import { useApi } from "@/lib/client";
import type { HealthResponse, SystemStatus } from "@/lib/types";

const NAV = [
  { href: "/", label: "Overview", Icon: Activity },
  { href: "/search", label: "Search", Icon: Search },
  { href: "/traces", label: "Traces", Icon: FileText },
  { href: "/analytics", label: "Analytics", Icon: ChartNoAxesCombined },
  { href: "/cost", label: "Cost", Icon: DollarSign },
  { href: "/knowledge", label: "Knowledge", Icon: Database },
];

export function Shell({ children }: { children: ReactNode }) {
  const pathname = usePathname();
  const health = useApi<HealthResponse>("health", 15_000);
  const system = useApi<SystemStatus>("system/status", 60_000);
  const online = health.data?.status === "ok" && !health.error;

  return (
    <div className="app-frame">
      <header className="topbar">
        <Link href="/" className="brand" aria-label="SearchOS console home">
          <span className="brand-mark">SO</span>
          <span className="brand-name">
            SEARCHOS<span className="brand-sub"> / CONSOLE</span>
          </span>
        </Link>

        <nav className="nav" aria-label="Primary navigation">
          {NAV.map(({ href, label, Icon }) => {
            const active = href === "/" ? pathname === "/" : pathname.startsWith(href);
            return (
              <Link
                key={href}
                href={href}
                className={`nav-link${active ? " nav-link-active" : ""}`}
                aria-current={active ? "page" : undefined}
              >
                <Icon className="size-3.5" strokeWidth={1.5} aria-hidden="true" />
                {label}
              </Link>
            );
          })}
        </nav>

        <div className="topbar-right">
          <span className="conn" title={health.error ?? "Gateway health"}>
            <span className={`conn-dot ${online ? "conn-online" : "conn-offline"}`} />
            {online ? "GATEWAY ONLINE" : "GATEWAY OFFLINE"}
          </span>
          <span className="version">
            API v{system.data?.api_version ?? "--"} / SCHEMA v{system.data?.schema_version ?? "--"}
          </span>
          <ThemeToggle />
        </div>
      </header>

      <main className="main">{children}</main>

      <footer className="footer">
        <span>SearchOS quality-first local search runtime / Analytics Read Model v1</span>
        <span>
          {online
            ? `Providers: ${health.data?.providers?.length ?? 0} / Extensions: ${system.data?.extensions ?? 0} / Crawlers: ${system.data?.crawlers ?? 0}`
            : "Start the gateway with: searchos gateway start"}
        </span>
      </footer>
    </div>
  );
}
