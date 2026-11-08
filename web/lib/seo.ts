import type { Metadata } from "next";

/**
 * Single source of truth for site identity, social cards and per-page tags.
 * The console SEO page (app/seo) renders from these same constants, so what
 * the interface shows is what the crawlers receive.
 */
export const SITE = {
  name: "SearchOS",
  console: "SearchOS Console",
  description:
    "Analytics, decision traces, evidence and cost telemetry for the SearchOS local runtime.",
  url: process.env.NEXT_PUBLIC_SITE_URL ?? "http://localhost:3100",
  locale: "en_US",
  alternateLocale: "zh_CN",
} as const;

export const OG_IMAGES = {
  en: {
    url: "/og.png",
    width: 1200,
    height: 630,
    alt: "SearchOS - the search & memory infrastructure for AI agents",
  },
  zh: {
    url: "/og-zh.png",
    width: 1200,
    height: 630,
    alt: "SearchOS - 面向 AI Agent 的搜索与记忆基础设施",
  },
  githubEn: {
    url: "/og-github.png",
    width: 1280,
    height: 640,
    alt: "SearchOS - the search & memory infrastructure for AI agents (GitHub card)",
  },
  githubZh: {
    url: "/og-zh-github.png",
    width: 1280,
    height: 640,
    alt: "SearchOS - 面向 AI Agent 的搜索与记忆基础设施（GitHub 卡片）",
  },
} as const;

export type PageKey =
  | "overview"
  | "search"
  | "traces"
  | "analytics"
  | "cost"
  | "knowledge"
  | "seo";

export interface PageSeo {
  key: PageKey;
  path: string;
  nav: string;
  /** Document title suffix; the root layout adds the " · SearchOS Console" template. */
  title: string;
  heading: string;
  description: string;
}

export const PAGES: Record<PageKey, PageSeo> = {
  overview: {
    key: "overview",
    path: "/",
    nav: "Overview",
    title: "System Overview",
    heading: "SYSTEM OVERVIEW",
    description:
      "KPI cards with denominators and drill-down, search activity and decision strategy charts, and CSV/JSON export from the Analytics Read Model.",
  },
  search: {
    key: "search",
    path: "/search",
    nav: "Search",
    title: "Search Workbench",
    heading: "SEARCH WORKBENCH",
    description:
      "Run a query against the local runtime and inspect its decision, evidence, warnings and raw response.",
  },
  traces: {
    key: "traces",
    path: "/traces",
    nav: "Traces",
    title: "Decision Traces",
    heading: "DECISION TRACES",
    description:
      "Staged decision timeline per request: planner, router, retrieval, evidence merge and quality gate.",
  },
  analytics: {
    key: "analytics",
    path: "/analytics",
    nav: "Analytics",
    title: "Analytics",
    heading: "ANALYTICS",
    description:
      "Search, cost, quality, knowledge, provider, crawler and infrastructure tabs over the Analytics Read Model.",
  },
  cost: {
    key: "cost",
    path: "/cost",
    nav: "Cost",
    title: "Cost Telemetry",
    heading: "COST TELEMETRY",
    description:
      "Cost ledger, four-dimension breakdown, reuse views, reservations and billing reconciliation.",
  },
  knowledge: {
    key: "knowledge",
    path: "/knowledge",
    nav: "Knowledge",
    title: "Knowledge & Memory",
    heading: "KNOWLEDGE & MEMORY",
    description:
      "Document explorer, semantic memory, versioned profiles, Read Model growth and runtime health.",
  },
  seo: {
    key: "seo",
    path: "/seo",
    nav: "SEO",
    title: "SEO & Share",
    heading: "SEO / SHARE",
    description:
      "Social card previews, the meta tags this console emits, and the bilingual OG asset set.",
  },
};

export const PAGE_LIST = Object.values(PAGES);

export function pageTitle(page: PageSeo): string {
  return `${page.title} · ${SITE.console}`;
}

export function pageMetadata(page: PageSeo): Metadata {
  return {
    title: page.title,
    description: page.description,
    openGraph: {
      title: pageTitle(page),
      description: page.description,
      type: "website",
      siteName: SITE.name,
      locale: SITE.locale,
      alternateLocale: [SITE.alternateLocale],
      images: [
        {
          url: OG_IMAGES.en.url,
          width: OG_IMAGES.en.width,
          height: OG_IMAGES.en.height,
          alt: OG_IMAGES.en.alt,
        },
      ],
    },
    twitter: {
      card: "summary_large_image",
      title: pageTitle(page),
      description: page.description,
      images: [OG_IMAGES.en.url],
    },
  };
}

export interface MetaTag {
  property: string;
  content: string;
  note: string;
}

/** The tags actually emitted for a page, in crawler order. Shown on /seo. */
export function metaTagsFor(page: PageSeo): MetaTag[] {
  return [
    { property: "og:title", content: pageTitle(page), note: "Card headline" },
    { property: "og:description", content: page.description, note: "Card body" },
    { property: "og:image", content: `${SITE.url}${OG_IMAGES.en.url}`, note: "1200x630 card image" },
    { property: "og:image:width", content: String(OG_IMAGES.en.width), note: "Aspect hint" },
    { property: "og:image:height", content: String(OG_IMAGES.en.height), note: "Aspect hint" },
    { property: "og:image:alt", content: OG_IMAGES.en.alt, note: "Accessibility text" },
    { property: "og:locale", content: SITE.locale, note: "Primary locale" },
    { property: "og:locale:alternate", content: SITE.alternateLocale, note: "Alternate locale" },
    { property: "twitter:card", content: "summary_large_image", note: "Large card layout" },
    { property: "twitter:title", content: pageTitle(page), note: "Card headline" },
    { property: "twitter:description", content: page.description, note: "Card body" },
    { property: "twitter:image", content: `${SITE.url}${OG_IMAGES.en.url}`, note: "Card image" },
  ];
}

export const OG_ASSETS = [
  {
    file: "web/public/og.png",
    url: "/og.png",
    size: "1200x630",
    lang: "EN",
    usage: "Default Open Graph / Twitter card, wired in app/layout.tsx",
  },
  {
    file: "web/public/og-zh.png",
    url: "/og-zh.png",
    size: "1200x630",
    lang: "中文",
    usage: "中文社区分享时直接引用",
  },
  {
    file: "web/public/og-github.png",
    url: "/og-github.png",
    size: "1280x640",
    lang: "EN",
    usage: "GitHub 仓库社交预览（Settings → Social preview）",
  },
  {
    file: "web/public/og-zh-github.png",
    url: "/og-zh-github.png",
    size: "1280x640",
    lang: "中文",
    usage: "中文 GitHub 仓库社交预览",
  },
] as const;
