import { pageMetadata, PAGES } from "@/lib/seo";
import { SeoPage } from "@/components/seo-preview";

export const metadata = pageMetadata(PAGES.seo);

export default function Page() {
  return <SeoPage />;
}
