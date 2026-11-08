import type { Metadata, Viewport } from "next";
import { GeistSans } from "geist/font/sans";
import { GeistMono } from "geist/font/mono";
import { Shell } from "@/components/Shell";
import { Providers } from "@/components/providers";
import "./globals.css";

export const metadata: Metadata = {
  metadataBase: new URL(process.env.NEXT_PUBLIC_SITE_URL ?? "http://localhost:3100"),
  title: "SearchOS Console",
  description:
    "Analytics, decision traces, evidence and cost telemetry for the SearchOS local runtime.",
  openGraph: {
    title: "SearchOS Console",
    description:
      "Analytics, decision traces, evidence and cost telemetry for the SearchOS local runtime.",
    type: "website",
    siteName: "SearchOS",
    locale: "en_US",
    alternateLocale: ["zh_CN"],
    images: [
      {
        url: "/og.png",
        width: 1200,
        height: 630,
        alt: "SearchOS — the search & memory infrastructure for AI agents",
      },
    ],
  },
  twitter: {
    card: "summary_large_image",
    title: "SearchOS Console",
    description:
      "Analytics, decision traces, evidence and cost telemetry for the SearchOS local runtime.",
    images: ["/og.png"],
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
