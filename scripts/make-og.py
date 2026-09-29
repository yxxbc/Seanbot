#!/usr/bin/env python3
"""生成站点分享卡片 site/assets/og.png（1200×630）。

依赖：Python 3 + Pillow；品牌图标用仓库里的 site/assets/icon.svg 光栅化
（优先 rsvg-convert，其次 qlmanage）。生成结果会提交进仓库，改文案后重跑一次即可。

用法：python3 scripts/make-og.py
"""

from __future__ import annotations

import json
import shutil
import subprocess
import sys
import tempfile
from pathlib import Path

from PIL import Image, ImageDraw, ImageFilter, ImageFont

W, H = 1200, 630
ROOT = Path(__file__).resolve().parent.parent
ASSETS = ROOT / "site" / "assets"

INK = (14, 13, 16)
INK_2 = (21, 19, 26)
CREAM = (244, 233, 216)
GOLD = (230, 184, 92)
CORAL = (217, 95, 75)
MUTED = (244, 233, 216, 150)
LINE = (244, 233, 216, 38)

CJK_FONTS = [
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/Supplemental/Arial Unicode.ttf",
    "/Library/Fonts/Arial Unicode.ttf",
]
BOLD_FONTS = [
    "/System/Library/Fonts/STHeiti Medium.ttc",
    "/System/Library/Fonts/Hiragino Sans GB.ttc",
]
MONO_FONTS = [
    "/System/Library/Fonts/Menlo.ttc",
    "/System/Library/Fonts/Supplemental/Courier New.ttf",
]


def pick_font(candidates: list[str]) -> str:
    for path in candidates:
        if Path(path).exists():
            return path
    raise SystemExit(f"找不到可用字体，请改 make-og.py 里的候选列表：{candidates}")


def render(version: str) -> Image.Image:
    cjk = pick_font(CJK_FONTS)
    bold = pick_font(BOLD_FONTS)
    mono = pick_font(MONO_FONTS)

    card = Image.new("RGB", (W, H), INK)
    # 竖向渐变，让底色不那么平
    for y in range(H):
        t = y / H
        row = tuple(round(a + (b - a) * t) for a, b in zip(INK, INK_2))
        ImageDraw.Draw(card).line([(0, y), (W, y)], fill=row)

    # 右上角的金色光晕（品牌色）
    glow = Image.new("RGBA", (W, H), (0, 0, 0, 0))
    ImageDraw.Draw(glow).ellipse([W - 520, -300, W + 180, 400], fill=(*GOLD, 46))
    card = Image.alpha_composite(card.convert("RGBA"), glow.filter(ImageFilter.GaussianBlur(90)))
    card = card.convert("RGB")

    draw = ImageDraw.Draw(card, "RGBA")

    # 品牌图标
    icon_png = rasterize_icon()
    if icon_png is not None:
        icon = Image.open(icon_png).convert("RGBA").resize((176, 176), Image.LANCZOS)
        card.paste(icon, (80, 104), icon)

    title = ImageFont.truetype(bold, 92)
    tagline = ImageFont.truetype(cjk, 44)
    lead = ImageFont.truetype(cjk, 30)
    small = ImageFont.truetype(cjk, 24)
    code = ImageFont.truetype(mono, 26)

    x = 292
    draw.text((x, 118), "Seanbot", font=title, fill=CREAM)
    draw.text((x, 232), "终端里的全能 AI 代理", font=tagline, fill=CREAM)
    draw.text((x, 296), "读文件 · 改代码 · 跑命令 · 查资料，说清楚你要什么就行", font=lead, fill=MUTED)

    # 终端块：一行装好 + 一行升级，呼应「不用回官网」
    box = (x, 358, W - 72, 470)
    draw.rounded_rectangle(box, radius=14, fill=(21, 19, 26, 235), outline=LINE, width=2)
    draw.text((box[0] + 26, box[1] + 22), "$ curl -fsSL …/scripts/install.sh | sh", font=code, fill=CREAM)
    draw.text((box[0] + 26, box[1] + 66), "$ sean update", font=code, fill=(*GOLD, 255))
    draw.text((box[0] + 300, box[1] + 66), "# 以后一键升级，不必回官网", font=small, fill=MUTED)

    draw.text((x, 506), "macOS · Linux · Windows　|　Rust　|　Apache-2.0", font=small, fill=MUTED)
    draw.text((x, 546), "yxxbc.github.io/Seanbot", font=code, fill=(*GOLD, 235))

    if version:
        label = f"v{version}"
        font = ImageFont.truetype(mono, 24)
        bx = W - 72 - draw.textlength(label, font=font) - 32
        draw.rounded_rectangle([bx, 528, W - 72, 574], radius=23, outline=(*CORAL, 150), width=2,
                               fill=(217, 95, 75, 40))
        draw.text((bx + 16, 538), label, font=font, fill=(232, 132, 112, 255))

    draw.line([(80, 88), (W - 72, 88)], fill=(*GOLD, 90), width=3)
    return card


def rasterize_icon() -> Path | None:
    """把 icon.svg 转成 PNG；缺工具时返回 None（卡片仍然可用）。"""
    svg = ASSETS / "icon.svg"
    if not svg.exists():
        return None
    out = Path(tempfile.mkstemp(suffix=".png")[1])
    if shutil.which("rsvg-convert"):
        subprocess.run(["rsvg-convert", "-w", "352", "-h", "352", "-o", str(out), str(svg)], check=True)
        return out
    if shutil.which("qlmanage"):
        tmp = out.parent
        subprocess.run(["qlmanage", "-t", "-s", "352", "-o", str(tmp), str(svg)],
                       check=True, capture_output=True)
        produced = tmp / (svg.name + ".png")
        if produced.exists():
            produced.replace(out)
            return out
    return None


def main() -> int:
    version = ""
    version_file = ASSETS / "version.json"
    if version_file.exists():
        version = json.loads(version_file.read_text(encoding="utf-8")).get("version", "")

    card = render(version)
    target = ASSETS / "og.png"
    card.save(target, format="PNG", optimize=True)
    print(f"已生成 {target.relative_to(ROOT)}（{target.stat().st_size // 1024} KiB，{W}×{H}）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
