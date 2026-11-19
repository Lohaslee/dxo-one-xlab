#!/usr/bin/env python3
"""正式签名前的最后一步：等长抹除二进制里的个人绝对路径（HOME 前缀）。

`tauri::generate_context!` 通过 `env!` 展开 OUT_DIR / CARGO_MANIFEST_DIR，
`--remap-path-prefix` 不覆盖这类字符串字面量，因此必须在打包前原位替换。
替换保持字节长度不变，不移动任何偏移；必须在 codesign 之前执行。
"""
import os
import sys
from pathlib import Path

BIN_RELS = [
    "target/release/dxo-app",
    "target/aarch64-apple-darwin/release/dxo-app",
    "target/x86_64-apple-darwin/release/dxo-app",
]


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    home = os.environ.get("HOME", "").encode()
    if not home or len(home) < len(b"/Users/USER"):
        print("HOME 异常，拒绝改写", file=sys.stderr)
        return 1

    targets = [root / rel for rel in BIN_RELS]
    found = [p for p in targets if p.is_file()]
    if not found:
        print(f"未找到待抹除的二进制，期望路径之一：{targets}", file=sys.stderr)
        return 1

    neutral = (b"/Users/USER" + b"_" * len(home))[: len(home)]
    for p in found:
        data = p.read_bytes()
        hits = data.count(home)
        if hits:
            p.write_bytes(data.replace(home, neutral))
        if p.read_bytes().count(home):
            print(f"❌ {p} 仍含 {home.decode()}", file=sys.stderr)
            return 1
        print(f"  个人路径抹除: {p.relative_to(root)}（替换 {hits} 处，长度不变）")
    return 0


if __name__ == "__main__":
    sys.exit(main())
