"use client";

import { Languages } from "lucide-react";

import { Button } from "@/components/ui/button";
import { useI18n } from "@/lib/i18n";

/** Cycles EN ↔ 中文. The choice is persisted by I18nProvider. */
export function LanguageToggle() {
  const { locale, setLocale, t } = useI18n();
  const next = locale === "en" ? "zh" : "en";
  return (
    <Button
      variant="ghost"
      size="sm"
      className="h-8 gap-1.5 px-2 font-mono text-[10px] uppercase tracking-wider"
      aria-label={t("Switch language")}
      title={t("Language")}
      onClick={() => setLocale(next)}
    >
      <Languages className="size-3.5" aria-hidden="true" />
      <span>{locale === "en" ? "EN" : "中文"}</span>
    </Button>
  );
}
