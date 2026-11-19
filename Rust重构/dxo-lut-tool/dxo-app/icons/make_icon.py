#!/usr/bin/env python3
"""V06: 由 favicon D-mark 设计生成 16–1024 iconset（PIL 按比例绘制，Impact 字体）。"""
import os

from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
FONT = "/System/Library/Fonts/Supplemental/Impact.ttf"
BG = (20, 20, 24)
ACCENT = (78, 173, 185)


def render(size):
    k = size / 64.0
    im = Image.new("RGB", (size, size), BG)
    d = ImageDraw.Draw(im)
    w = max(1, round(5 * k))
    d.rectangle([6 * k - w / 2, 6 * k - w / 2, 58 * k + w / 2, 58 * k + w / 2], outline=ACCENT, width=w)
    f = ImageFont.truetype(FONT, max(6, round(36 * k)))
    d.text((32 * k, 45 * k), "D", font=f, fill=ACCENT, anchor="ms")
    return im


def main():
    out = os.path.join(HERE, "AppIcon.iconset")
    os.makedirs(out, exist_ok=True)
    for base in (16, 32, 128, 256, 512):
        render(base).save(os.path.join(out, f"icon_{base}x{base}.png"))
        render(base * 2).save(os.path.join(out, f"icon_{base}x{base}@2x.png"))
    render(1024).save(os.path.join(HERE, "icon-1024.png"))
    print("iconset written:", sorted(os.listdir(out)))


if __name__ == "__main__":
    main()