#!/usr/bin/env python3
"""由 AppIcon.iconset（相机设计，16px 为手工调版）拼装 icon.ico。

icon.ico 曾是 favicon D-mark 设计，与 AppIcon.icns/icon-1024.png 的相机设计不一致，
导致 Windows 包图标 ≠ Mac 包图标（2026-11-19 用户报告）。相机设计以
AppIcon.iconset/ + icon-1024.png 为唯一事实源，本脚本只做 ICO 装配，不重新绘制。
"""
import io
import os
import struct

from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
ICONSET = os.path.join(HERE, "AppIcon.iconset")


def png_bytes(im):
    buf = io.BytesIO()
    im.save(buf, "PNG")
    return buf.getvalue()


def build_entries():
    i256 = Image.open(os.path.join(ICONSET, "icon_256x256.png")).convert("RGBA")
    assert i256.size == (256, 256), i256.size
    sources = [
        (16, os.path.join(ICONSET, "icon_16x16.png"), None),
        (24, None, Image.LANCZOS),
        (32, os.path.join(ICONSET, "icon_32x32.png"), None),
        (48, None, Image.LANCZOS),
        (64, os.path.join(ICONSET, "icon_32x32@2x.png"), None),
        (128, os.path.join(ICONSET, "icon_128x128.png"), None),
        (256, os.path.join(ICONSET, "icon_256x256.png"), None),
    ]
    entries = []
    for width, path, resample in sources:
        if path is not None:
            im = Image.open(path).convert("RGBA")
        else:
            im = i256.resize((width, width), resample)
        assert im.size == (width, width), (path, im.size)
        entries.append((width, png_bytes(im)))
    return entries


def main():
    blobs = build_entries()
    out = struct.pack("<HHH", 0, 1, len(blobs))
    offset = 6 + 16 * len(blobs)
    for width, data in blobs:
        out += struct.pack("<BBBBHHII", width % 256, width % 256, 0, 0, 1, 32,
                           len(data), offset)
        offset += len(data)
    out += b"".join(data for _, data in blobs)
    path = os.path.join(HERE, "icon.ico")
    with open(path, "wb") as fh:
        fh.write(out)
    print("icon.ico written:", len(out), "bytes,", len(blobs), "entries")


if __name__ == "__main__":
    main()
