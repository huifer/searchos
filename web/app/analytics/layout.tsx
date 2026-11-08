import type { ReactNode } from "react";

import { pageMetadata, PAGES } from "@/lib/seo";

export const metadata = pageMetadata(PAGES.analytics);

export default function RouteLayout({ children }: { children: ReactNode }) {
  return <>{children}</>;
}
