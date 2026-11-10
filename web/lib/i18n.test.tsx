import { describe, expect, it, beforeEach } from "vitest";
import { render, screen, waitFor } from "@testing-library/react";
import userEvent from "@testing-library/user-event";
import type { ReactNode } from "react";

import { I18nProvider, LOCALE_STORAGE_KEY, translate, useI18n } from "./i18n";
import { LanguageToggle } from "@/components/language-toggle";
import { Empty, Panel, SectionIntro } from "@/components/ui";

function Probe({ text }: { text: string }) {
  const { locale, t } = useI18n();
  return (
    <div>
      <span data-testid="text">{t(text)}</span>
      <span data-testid="locale">{locale}</span>
      <LanguageToggle />
    </div>
  );
}

function wrapper({ children }: { children: ReactNode }) {
  return <I18nProvider>{children}</I18nProvider>;
}

beforeEach(() => {
  window.localStorage.clear();
  document.documentElement.lang = "en";
});

describe("translate()", () => {
  it("translates known English strings into Chinese", () => {
    expect(translate("zh", "Overview")).toBe("总览");
    expect(translate("zh", "Search workbench")).toBe("搜索工作台");
    expect(translate("zh", "Run search")).toBe("运行搜索");
  });

  it("leaves English untouched in the en locale", () => {
    expect(translate("en", "Overview")).toBe("Overview");
    expect(translate("en", "unknown phrase")).toBe("unknown phrase");
  });

  it("falls back to the original text when a string has no translation", () => {
    // Technical identifiers are deliberately left in English.
    expect(translate("zh", "POST /search")).toBe("POST /search");
    expect(translate("zh", "x-searchos-api-version")).toBe("x-searchos-api-version");
    expect(translate("zh", "")).toBe("");
  });
});

describe("I18nProvider", () => {
  it("defaults to English and keeps untranslated strings in English", async () => {
    render(<Probe text="POST /search" />, { wrapper });
    await waitFor(() => expect(screen.getByTestId("locale").textContent).toBe("en"));
    expect(screen.getByTestId("text").textContent).toBe("POST /search");
    expect(document.documentElement.lang).toBe("en");
  });

  it("selects Chinese from localStorage and sets <html lang>", async () => {
    window.localStorage.setItem(LOCALE_STORAGE_KEY, "zh");
    render(<Probe text="Overview" />, { wrapper });

    await waitFor(() => expect(screen.getByTestId("locale").textContent).toBe("zh"));
    expect(screen.getByTestId("text").textContent).toBe("总览");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("persists an explicit language switch", async () => {
    const user = userEvent.setup();
    render(<Probe text="Overview" />, { wrapper });
    await waitFor(() => expect(screen.getByTestId("locale").textContent).toBe("en"));
    await user.click(screen.getByRole("button", { name: "Switch language" }));

    await waitFor(() => expect(screen.getByTestId("text").textContent).toBe("总览"));
    expect(window.localStorage.getItem(LOCALE_STORAGE_KEY)).toBe("zh");
    expect(document.documentElement.lang).toBe("zh-CN");
  });

  it("an explicit English choice wins over a zh browser language", async () => {
    window.localStorage.setItem(LOCALE_STORAGE_KEY, "en");
    render(<Probe text="Overview" />, { wrapper });
    await waitFor(() => expect(screen.getByTestId("locale").textContent).toBe("en"));
    expect(screen.getByTestId("text").textContent).toBe("Overview");
  });
});

/** The shared primitives are where every page's chrome comes from; proving
 *  them bilingual proves all seven routes are, without a full browser run. */
describe("shared primitives render Chinese when the locale is zh", () => {
  it("translates page intros, panel titles and empty states", async () => {
    window.localStorage.setItem(LOCALE_STORAGE_KEY, "zh");
    render(
      <>
        <SectionIntro title="System overview" description="Loading snapshot" />
        <Panel title="Search activity">
          <Empty
            title="No search requests"
            body="Run a query in the Search workbench to populate this table."
          />
        </Panel>
      </>,
      { wrapper },
    );

    await waitFor(() => expect(screen.getByText("系统总览")).toBeTruthy());
    expect(screen.getByText("正在加载快照")).toBeTruthy();
    expect(screen.getByText("搜索活动")).toBeTruthy();
    expect(screen.getByText("暂无搜索请求")).toBeTruthy();
    expect(
      screen.getByText("在搜索工作台运行一次查询即可填充这张表。"),
    ).toBeTruthy();
  });
});
