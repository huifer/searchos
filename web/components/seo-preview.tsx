"use client";

import { useI18n } from "@/lib/i18n";

import { useCallback, useState } from "react";
import { Check, Copy, ExternalLink } from "lucide-react";

import { OG_ASSETS, OG_IMAGES, PAGES, SITE, metaTagsFor, pageTitle } from "@/lib/seo";
import { Badge, Panel, SectionIntro } from "@/components/ui";
import { Table, TableBody, TableCell, TableHead, TableHeader, TableRow } from "@/components/ui/table";
import { cn } from "@/lib/utils";

type Lang = "en" | "zh";

const LANG_LABEL: Record<Lang, string> = { en: "EN", zh: "中文" };

function MockCard({
  label,
  image,
  title,
  description,
}: {
  label: string;
  image: { url: string; width: number; height: number; alt: string };
  title: string;
  description: string;
}) {
  const { t } = useI18n();
  return (
    <div className="flex flex-col gap-2">
      <div className="flex items-center justify-between">
        <span className="mono text-[11px] tracking-widest text-muted-foreground">{label}</span>
        <span className="mono text-[11px] text-muted-foreground">
          {image.width} x {image.height}
        </span>
      </div>
      <div className="border border-border bg-card">
        {/* eslint-disable-next-line @next/next/no-img-element */}
        <img
          src={image.url}
          alt={image.alt}
          className="w-full object-cover"
          style={{ aspectRatio: `${image.width} / ${image.height}` }}
        />
        <div className="space-y-1 p-3">
          <div className="truncate text-sm font-medium text-foreground">{title}</div>
          <div className="line-clamp-2 text-xs leading-relaxed text-muted-foreground">{description}</div>
          <div className="mono pt-1 text-[11px] text-muted-foreground">{SITE.url.replace(/^https?:\/\//, "")}</div>
        </div>
      </div>
    </div>
  );
}

function CopyButton({ value }: { value: string }) {
  const { t } = useI18n();
  const [copied, setCopied] = useState(false);

  const copy = useCallback(async () => {
    try {
      await navigator.clipboard.writeText(value);
      setCopied(true);
      window.setTimeout(() => setCopied(false), 1500);
    } catch {
      /* clipboard unavailable (non-secure context) */
    }
  }, [value]);

  return (
    <button
      type="button"
      onClick={copy}
      className="inline-flex items-center gap-1 border border-border px-2 py-0.5 text-[11px] text-muted-foreground transition-colors hover:border-line-strong hover:text-foreground"
      aria-label={`Copy ${value}`}
    >
      {copied ? <Check className="size-3" aria-hidden="true" /> : <Copy className="size-3" aria-hidden="true" />}
      {copied ? "COPIED" : "COPY"}
    </button>
  );
}

export function SeoPage() {
  const { t } = useI18n();
  const [lang, setLang] = useState<Lang>("en");
  const image = lang === "zh" ? OG_IMAGES.zh : OG_IMAGES.en;
  const githubImage = lang === "zh" ? OG_IMAGES.githubZh : OG_IMAGES.githubEn;
  const page = PAGES.seo;
  const tags = metaTagsFor(page);

  return (
    <div className="space-y-4">
      <SectionIntro
        title={page.heading}
        description="Social card previews, the meta tags this console emits, and the bilingual OG asset set. Same constants as app/layout.tsx and lib/seo.ts."
      />

      <Panel
        title="SOCIAL CARD PREVIEW"
        meta={
          <div className="flex items-center gap-1">
            {(["en", "zh"] as const).map((l) => (
              <button
                key={l}
                type="button"
                onClick={() => setLang(l)}
                className={cn(
                  "border px-2 py-0.5 text-[11px] transition-colors",
                  lang === l
                    ? "border-line-strong bg-secondary text-foreground"
                    : "border-border text-muted-foreground hover:text-foreground",
                )}
              >
                {LANG_LABEL[l]}
              </button>
            ))}
          </div>
        }
      >
        <div className="grid gap-4 lg:grid-cols-2">
          <MockCard
            label={t("X / TWITTER - SUMMARY_LARGE_IMAGE")}
            image={image}
            title={pageTitle(page)}
            description={page.description}
          />
          <MockCard
            label={t("GITHUB - REPO SOCIAL PREVIEW")}
            image={githubImage}
            title={pageTitle(page)}
            description={page.description}
          />
        </div>
      </Panel>

      <Panel title="META TAGS - THIS PAGE" meta={<Badge tone="accent">og + twitter</Badge>}>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("Property")}</TableHead>
              <TableHead>{t("Content")}</TableHead>
              <TableHead>{t("Note")}</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {tags.map((tag) => (
              <TableRow key={tag.property}>
                <TableCell className="mono text-xs">{tag.property}</TableCell>
                <TableCell className="max-w-md truncate text-xs" title={tag.content}>
                  {tag.content}
                </TableCell>
                <TableCell className="text-xs text-muted-foreground">{tag.note}</TableCell>
                <TableCell className="text-right">
                  <CopyButton value={`${tag.property} ${tag.content}`} />
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Panel>

      <Panel title="OG ASSETS" meta={<span className="mono text-[11px]">4 files / web/public</span>}>
        <Table>
          <TableHeader>
            <TableRow>
              <TableHead>{t("File")}</TableHead>
              <TableHead>{t("Size")}</TableHead>
              <TableHead>{t("Lang")}</TableHead>
              <TableHead>{t("Usage")}</TableHead>
              <TableHead />
            </TableRow>
          </TableHeader>
          <TableBody>
            {OG_ASSETS.map((asset) => (
              <TableRow key={asset.url}>
                <TableCell className="mono text-xs">{asset.file}</TableCell>
                <TableCell className="mono text-xs">{asset.size}</TableCell>
                <TableCell>
                  <Badge tone={asset.lang === "EN" ? "neutral" : "muted"}>{asset.lang}</Badge>
                </TableCell>
                <TableCell className="text-xs text-muted-foreground">{asset.usage}</TableCell>
                <TableCell className="text-right">
                  <a
                    className="inline-flex items-center gap-1 text-xs text-accent hover:underline"
                    href={asset.url}
                    target="_blank"
                    rel="noreferrer"
                  >
                    VIEW <ExternalLink className="size-3" aria-hidden="true" />
                  </a>
                </TableCell>
              </TableRow>
            ))}
          </TableBody>
        </Table>
      </Panel>

      <Panel title="SHARE CHECKLIST">
        <ul className="list-none space-y-2 text-sm text-muted-foreground">
          <li className="flex gap-2">
            <span className="mono text-accent">[1]</span>
            Set <span className="mono text-foreground">NEXT_PUBLIC_SITE_URL</span> in production so og:image
            resolves to an absolute URL.
          </li>
          <li className="flex gap-2">
            <span className="mono text-accent">[2]</span>
            GitHub repo social preview: upload <span className="mono text-foreground">web/public/og-github.png</span>{" "}
            (or the 中文 variant) under Settings, Social preview.
          </li>
          <li className="flex gap-2">
            <span className="mono text-accent">[3]</span>
            中文社区（微信公众号 / 知乎 / 小红书）直接引用{" "}
            <span className="mono text-foreground">og-zh.png</span>；og:image 每页只能挂一张，默认英文卡。
          </li>
          <li className="flex gap-2">
            <span className="mono text-accent">[4]</span>
            修改品牌色或文案后重新生成：
            <span className="mono text-foreground">python3 web/scripts/generate-og.py</span>
          </li>
        </ul>
      </Panel>
    </div>
  );
}
