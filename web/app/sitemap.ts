import type { MetadataRoute } from "next";
import { PAGE_LIST, SITE } from "@/lib/seo";

export default function sitemap(): MetadataRoute.Sitemap {
  return PAGE_LIST.map((page) => ({
    url: `${SITE.url}${page.path}`,
    changeFrequency: "weekly" as const,
    priority: page.key === "overview" ? 1 : 0.7,
  }));
}
