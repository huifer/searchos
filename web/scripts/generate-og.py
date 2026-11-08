#!/usr/bin/env python3
"""Generate the SearchOS Open Graph / social preview PNGs (EN + 中文).

The images are rendered in code rather than by an image model: OG cards are
text-heavy and must match the console design system exactly. Chinese glyphs
come from the local Noto Sans CJK SC when present; Latin always comes from
the Geist TTFs bundled by the `geist` npm package.

Design source : docs/ui.md (RFC-026, Tactical Telemetry / industrial-grid)
Color source  : web/app/globals.css  (.dark oklch() tokens)

Usage:
    python3 web/scripts/generate-og.py                 # all four variants
    python3 web/scripts/generate-og.py --lang zh       # Chinese only
    python3 web/scripts/generate-og.py --size 1200x630 --out /tmp/og.png
    python3 web/scripts/generate-og.py --lang en --variant github

Requires: Pillow (`python3 -m pip install Pillow`), and `npm ci` in web/.
"""

from __future__ import annotations

import argparse
import math
from pathlib import Path

from PIL import Image, ImageDraw, ImageFont

BASE_W, BASE_H = 1200, 630
WEB = Path(__file__).resolve().parents[1]
GEIST = WEB / "node_modules" / "geist" / "dist" / "fonts"
PUBLIC = WEB / "public"
_SANS = GEIST / "geist-sans"
_MONO = GEIST / "geist-mono"
GEIST_BOLD = _SANS / "Geist-Bold.ttf"

HOME = Path.home()
CF = {
    "NOTO_REG": HOME / "Library/Fonts/NotoSansCJKsc-Regular.otf",
    "NOTO_MED": HOME / "Library/Fonts/NotoSansCJKsc-Medium.otf",
    "NOTO_BOLD": HOME / "Library/Fonts/NotoSansCJKsc-Bold.otf",
    "ST_HEITI": Path("/System/Library/Fonts/STHeiti Medium.ttc"),
    "NOTO_SB": HOME / ".fonts/fde-book/NotoSansSC-Bold.otf",
    "NOTO_SC_REG": HOME / ".fonts/fde-book/NotoSansSC-Regular.otf",
    "NOTO_LINUX": Path("/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc"),
    "UNICODE": Path("/System/Library/Fonts/Supplemental/Arial Unicode.ttf"),
}

# ----------------------------------------------------------------------------
# copy per language

EN = {
    "header_left": "SEARCHOS",
    "header_mid": "/ SEARCH INFRASTRUCTURE",
    "header_right": "LOCAL-FIRST / TAVILY-COMPATIBLE",
    "eyebrow": "01 / SEARCH & MEMORY INFRASTRUCTURE",
    "tagline1": "The Search & Memory",
    "tagline2": "Infrastructure for AI Agents.",
    "sub1": "One search engine. Two runtime modes.",
    "sub2": "Shared intelligence.",
    "stack": "SQLite / Tantivy / LanceDB / Axum",
    "panel_title": "DECISION PIPELINE",
    "panel_right": "06 STAGES",
    "stages": ["QUERY INTAKE", "PLANNER", "ROUTER", "RETRIEVAL", "EVIDENCE MERGE", "QUALITY GATE"],
    "panel_footer": "ONE CORE / TWO RUNTIMES",
    "panel_footer_right": "RFC-026",
    "cells": [
        ("LOCAL MODE", "CLI / SDK / MCP"),
        ("GATEWAY MODE", "TAVILY-COMPAT REST"),
        ("SEARCH MEMORY", "DURABLE / REUSABLE"),
        ("STATUS", "OPERATIONAL"),
    ],
}

ZH = {
    "header_left": "SearchOS",
    "header_mid": "/ 搜索与记忆基础设施",
    "header_right": "本地优先 / 兼容 Tavily",
    "eyebrow": "01 / 搜索与记忆基础设施",
    "tagline1": "面向 AI Agent 的",
    "tagline2": "搜索与记忆基础设施。",
    "sub1": "一个搜索引擎。两种运行模式。",
    "sub2": "共享的智能。",
    "stack": "SQLite / Tantivy / LanceDB / Axum",
    "panel_title": "决策流水线",
    "panel_right": "06 个阶段",
    "stages": ["查询输入", "规划器", "路由", "检索", "证据合并", "质量门"],
    "panel_footer": "一个内核 / 两种模式",
    "panel_footer_right": "RFC-026",
    "cells": [
        ("本地模式", "CLI / SDK / MCP"),
        ("网关模式", "Tavily 兼容 REST"),
        ("搜索记忆", "持久 / 可复用"),
        ("状态", "运行中"),
    ],
}

LANGS = {"en": EN, "zh": ZH}

# per-language letter spacing for mono-ish labels
TRACK = {
    "en": {"hi": 3.2, "eyebrow": 1.2, "panel_title": 1.0, "stage": 0, "cell": 1.2, "footer": 0.8},
    "zh": {"hi": 2.0, "eyebrow": 0.8, "panel_title": 1.0, "stage": 0, "cell": 1.0, "footer": 0.6},
}

# font resolution per language/style; logo is always the brand wordmark
FONT_CANDIDATES = {
    "en": {
        "sans": [GEIST / _SANS.name / "Geist-Regular.ttf"],
        "sans_medium": [GEIST / _SANS.name / "Geist-Medium.ttf"],
        "sans_bold": [GEIST / _SANS.name / "Geist-Bold.ttf"],
        "mono": [GEIST / _MONO.name / "GeistMono-Regular.ttf"],
        "mono_medium": [GEIST / _MONO.name / "GeistMono-Medium.ttf"],
        "mono_semibold": [GEIST / _MONO.name / "GeistMono-SemiBold.ttf"],
        "logo": [GEIST_BOLD],
    },
    "zh": {
        "sans": [CF["NOTO_REG"], CF["NOTO_SC_REG"], CF["ST_HEITI"], CF["NOTO_LINUX"], CF["UNICODE"]],
        "sans_medium": [CF["NOTO_MED"], CF["NOTO_REG"], CF["ST_HEITI"], CF["NOTO_LINUX"], CF["UNICODE"]],
        "sans_bold": [CF["NOTO_BOLD"], CF["NOTO_MED"], CF["ST_HEITI"], CF["UNICODE"]],
        "mono": [CF["NOTO_REG"], CF["NOTO_SC_REG"], CF["ST_HEITI"], CF["NOTO_LINUX"], CF["UNICODE"]],
        "mono_medium": [CF["NOTO_MED"], CF["NOTO_REG"], CF["ST_HEITI"], CF["UNICODE"]],
        "mono_semibold": [CF["NOTO_BOLD"], CF["NOTO_MED"], CF["ST_HEITI"], CF["UNICODE"]],
        "logo": [GEIST_BOLD],
    },
}

# ----------------------------------------------------------------------------
# exact oklch() -> sRGB conversion, so tokens stay the single source of truth

def oklch(lightness: float, chroma: float, hue_deg: float) -> tuple[int, int, int]:
    h = math.radians(hue_deg)
    a, b = chroma * math.cos(h), chroma * math.sin(h)
    l_ = lightness + 0.3963377774 * a + 0.2158037573 * b
    m_ = lightness - 0.1055613458 * a - 0.0638541728 * b
    s_ = lightness - 0.0894841775 * a - 1.2914855480 * b
    l, m, s = l_ ** 3, m_ ** 3, s_ ** 3
    lin = (
        4.0767416621 * l - 3.3077115913 * m + 0.2309699292 * s,
        -1.2684380046 * l + 2.6097574011 * m - 0.3413193965 * s,
        -0.0041960863 * l - 0.7034186147 * m + 1.7076147010 * s,
    )

    def gamma(x: float) -> float:
        x = min(1.0, max(0.0, x))
        return 12.92 * x if x <= 0.0031308 else 1.055 * x ** (1 / 2.4) - 0.055

    return tuple(round(255 * gamma(v)) for v in lin)


def mix(a: tuple[int, int, int], b: tuple[int, int, int], t: float) -> tuple[int, int, int]:
    return tuple(round(x + (y - x) * t) for x, y in zip(a, b))


# .dark tokens from web/app/globals.css
BG = oklch(0.145, 0.006, 260)
CARD = oklch(0.175, 0.006, 260)
FG = oklch(0.94, 0.004, 260)
MUTED = oklch(0.70, 0.01, 260)
BORDER = oklch(0.30, 0.006, 260)
LINE_STRONG = oklch(0.40, 0.008, 260)
ACCENT = oklch(0.68, 0.20, 27)  # one hazard-red accent
SIGNAL = oklch(0.85, 0.25, 145)  # the single green connectivity indicator
DIM = mix(MUTED, BG, 0.30)
GRID = mix(BG, BORDER, 0.55)


class Canvas:
    """Drawing helpers in 1200x630 base units, rendered at 2x then downsampled."""

    def __init__(self, w: int, h: int, scale: int = 2, lang: str = "en"):
        self.w, self.h, self.s = w, h, scale
        self.lang = lang
        self.sx, self.sy = w / BASE_W, h / BASE_H
        self.fs = min(self.sx, self.sy)
        self.im = Image.new("RGB", (w * scale, h * scale), BG)
        self.d = ImageDraw.Draw(self.im)
        self._fonts: dict[tuple[str, int], ImageFont.FreeTypeFont] = {}

    def X(self, v: float) -> int:
        return round(v * self.sx * self.s)

    def Y(self, v) -> int:
        return round(v * self.sy * self.s)

    def F(self, v: float) -> int:
        return max(1, round(v * self.fs * self.s))

    def font(self, key: str, size: int) -> ImageFont.FreeTypeFont:
        cache_key = (key, size)
        if cache_key not in self._fonts:
            candidates = FONT_CANDIDATES[self.lang][key]
            path = next((p for p in candidates if p.exists()), None)
            if path is None:
                raise SystemExit(
                    f"missing font for lang={self.lang} style={key}: tried {candidates}"
                )
            self._fonts[cache_key] = ImageFont.truetype(str(path), self.F(size))
        return self._fonts[cache_key]

    def tw(self, s: str, font) -> float:
        """Text width converted back into base units."""
        return self.d.textlength(s, font=font) / (self.fs * self.s)

    def rect(self, x, y, w, h, fill=None, outline=None, width=1):
        self.d.rectangle(
            (self.X(x), self.Y(y), self.X(x + w), self.Y(y + h)),
            fill=fill,
            outline=outline,
            width=max(1, round(width * self.s)),
        )

    def ellipse(self, x, y, w, h, fill=None, outline=None, width=1):
        self.d.ellipse(
            (self.X(x), self.Y(y), self.X(x + w), self.Y(y + h)),
            fill=fill,
            outline=outline,
            width=max(1, round(width * self.s)),
        )

    def line(self, x1, y1, x2, y2, fill, width=1):
        self.d.line(
            (self.X(x1), self.Y(y1), self.X(x2), self.Y(y2)),
            fill=fill,
            width=max(1, round(width * self.s)),
        )

    def text(self, x, y, s, font, fill, tracking=0.0, anchor="lt"):
        if tracking:
            cx = x
            for ch in s:
                self.d.text((self.X(cx), self.Y(y)), ch, font=font, fill=fill, anchor=anchor)
                cx += self.tw(ch, font) + tracking
        else:
            self.d.text((self.X(x), self.Y(y)), s, font=font, fill=fill, anchor=anchor)

    def rtext(self, x_right, y, s, font, fill, tracking=0.0):
        if tracking:
            total = sum(self.tw(ch, font) for ch in s) + tracking * max(0, len(s) - 1)
            self.text(x_right - total, y, s, font, fill, tracking)
        else:
            self.text(x_right - self.tw(s, font), y, s, font, fill)

    def save(self, path: Path):
        path.parent.mkdir(parents=True, exist_ok=True)
        self.im.resize((self.w, self.h), Image.LANCZOS).save(path, optimize=True)
        print(f"wrote {path}  ({self.w}x{self.h}, {path.stat().st_size / 1024:.0f} KiB, lang={self.lang})")


def render(w: int, h: int, lang: str = "en") -> Canvas:
    c = Canvas(w, h, lang=lang)
    copy = LANGS[lang]
    tr = TRACK[lang]

    # --- substrate: rigid industrial grid + corner crop marks ---
    for x in range(0, BASE_W + 1, 48):
        c.line(x, 0, x, BASE_H, GRID)
    for y in range(0, BASE_H + 1, 48):
        c.line(0, y, BASE_W, y, GRID)
    for cx, cy, dx, dy in ((32, 32, 1, 1), (1168, 32, -1, 1), (32, 598, 1, -1), (1168, 598, -1, -1)):
        c.line(cx, cy, cx + 18 * dx, cy, LINE_STRONG, 2)
        c.line(cx, cy, cx, cy + 18 * dy, LINE_STRONG, 2)

    # --- header ---
    c.rect(64, 46, 44, 5, fill=ACCENT)
    chip = "v0.1.0"
    chip_font = c.font("mono", 12)
    chip_w = c.tw(chip, chip_font) + 16
    chip_x = 1136 - c.tw(copy["header_right"], c.font("mono", 13)) - 16 - chip_w
    c.rect(chip_x, 40, chip_w, 20, outline=LINE_STRONG)
    c.text(chip_x + 8, 45, chip, chip_font, DIM)
    head = c.font("mono_semibold", 16)
    c.text(64, 64, copy["header_left"], head, FG, tracking=tr["hi"])
    off = c.tw(copy["header_left"], head) + tr["hi"] * len(copy["header_left"])
    c.text(64 + off + 20, 66, copy["header_mid"], c.font("mono", 14), DIM)
    c.rtext(1136, 66, copy["header_right"], c.font("mono", 13), DIM)
    c.line(64, 104, 1136, 104, BORDER)

    # --- left column: wordmark + positioning ---
    c.text(64, 140, copy["eyebrow"], c.font("mono_medium", 13), ACCENT, tracking=tr["eyebrow"])
    c.text(64, 174, "SearchOS", c.font("logo", 104), FG)
    sans30 = c.font("sans", 30)
    c.text(64, 300, copy["tagline1"], sans30, FG)
    c.text(64, 342, copy["tagline2"], sans30, FG)
    c.rect(64, 404, 56, 4, fill=ACCENT)
    c.text(64, 432, copy["sub1"], c.font("mono", 17), MUTED)
    c.text(64, 458, copy["sub2"], c.font("mono", 17), MUTED)
    c.text(64, 496, copy["stack"], c.font("mono", 13), DIM)

    # --- right column: staged decision pipeline schematic ---
    c.rect(700, 140, 436, 360, fill=CARD, outline=BORDER)
    c.rect(722, 161, 9, 9, fill=ACCENT)
    c.text(742, 158, copy["panel_title"], c.font("mono_medium", 13), FG, tracking=tr["panel_title"])
    c.rtext(1112, 159, copy["panel_right"], c.font("mono", 12), DIM)
    c.line(700, 190, 1136, 190, BORDER)

    stages = copy["stages"]
    ys = [212 + 38 * i for i in range(6)]
    c.line(744, 220, 744, 416, LINE_STRONG)
    for i, (name, y) in enumerate(zip(stages, ys)):
        active = i == 3
        c.rect(739, y + 3, 10, 10, fill=ACCENT if active else BG,
               outline=ACCENT if active else LINE_STRONG)
        c.text(766, y, f"0{i + 1}", c.font("mono", 12), DIM)
        c.text(802, y - 2, name, c.font("mono_medium", 15), FG if active else MUTED)
        c.rect(1100, y + 4, 9, 9, fill=ACCENT if active else None, outline=LINE_STRONG)

    c.line(700, 440, 1136, 440, BORDER)
    c.text(722, 456, copy["panel_footer"], c.font("mono", 12), DIM, tracking=tr["footer"])
    c.rtext(1112, 456, copy["panel_footer_right"], c.font("mono", 12), DIM)

    # --- bottom telemetry strip ---
    c.line(64, 528, 1136, 528, BORDER)
    cells = copy["cells"]
    cell_w = (1136 - 64)
    for i, (label, value) in enumerate(cells):
        x0 = 64 + i * (cell_w / 4)
        if i:
            c.line(x0, 528, x0, 606, BORDER)
        tx = x0 + (0 if i == 0 else 16)
        c.text(tx, 544, label, c.font("mono_medium", 11), DIM, tracking=tr["cell"])
        if i == 3:
            c.ellipse(tx, 575, 9, 9, fill=SIGNAL)  # the one green dot
            c.text(tx + 18, 568, value, c.font("mono", 15), FG)
        else:
            c.text(tx, 568, value, c.font("mono", 15), FG)
    c.line(64, 606, 1136, 606, BORDER)
    return c


VARIANTS = {
    "og": (1200, 630, "og.png", "og-zh.png"),
    "github": (1280, 640, "og-github.png", "og-zh-github.png"),
}


def main() -> None:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lang", choices=["en", "zh"], default=None,
                        help="language for standard variants (default: all)")
    parser.add_argument("--variant", choices=list(VARIANTS), default=None,
                        help="only one standard variant pair")
    parser.add_argument("--size", default=None, help="WxH, e.g. 1200x630")
    parser.add_argument("--out", type=Path, default=None, help="output PNG path")
    args = parser.parse_args()

    if args.size:
        w, h = (int(p) for p in args.size.lower().split("x"))
        out = args.out or PUBLIC / (f"og-{args.lang or 'en'}.png")
        render(w, h, args.lang or "en").save(out)
        return

    langs = [args.lang] if args.lang else ["en", "zh"]
    for key, (w, h, en_name, zh_name) in VARIANTS.items():
        if args.variant and key != args.variant:
            continue
        for lang, name in (("en", en_name), ("zh", zh_name)):
            if lang not in langs:
                continue
            render(w, h, lang).save(PUBLIC / name)


if __name__ == "__main__":
    main()
