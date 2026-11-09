import type { Metadata, Viewport } from "next";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import { Shell } from "@/components/Shell";
import { Providers } from "@/components/providers";
import { OG_IMAGES, SITE } from "@/lib/seo";
import "./globals.css";

export const metadata: Metadata = {
  metadataBase: new URL(SITE.url),
  title: { default: SITE.console, template: `%s · ${SITE.console}` },
  description: SITE.description,
  applicationName: SITE.console,
  keywords: [
    "SearchOS",
    "AI search infrastructure",
    "search memory",
    "Tavily-compatible API",
    "Rust",
    "MCP",
    "decision engine",
    "cost-aware routing",
  ],
  authors: [{ name: "SearchOS Authors" }],
  creator: "SearchOS Authors",
  openGraph: {
    title: SITE.console,
    description: SITE.description,
    type: "website",
    siteName: SITE.name,
    locale: SITE.locale,
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
    title: SITE.console,
    description: SITE.description,
    images: [OG_IMAGES.en.url],
  },
};

export const viewport: Viewport = {
  colorScheme: "light dark",
};

export default function RootLayout({ children }: Readonly<{ children: React.ReactNode }>) {
  return (
    <html lang="en" suppressHydrationWarning className={`${GeistSans.variable} ${GeistMono.variable}`}>
      <body>
        <Providers>
          <Shell>{children}</Shell>
        </Providers>
      </body>
    </html>
  );
}
