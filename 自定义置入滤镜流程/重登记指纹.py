#!/usr/bin/env python3
"""重登记指纹：替换 DXO_FILTERS 里某个 lut_*.bin 的内容后，同步更新
ph_menu.bin 的指纹登记，让被替换的档位能正常切换。

原理：ph_menu.bin 的 STR 段（0x08EF6400）= n 条 32B 路径 + n 组 8B 指纹
（每组 = 槽2 表体 0x1000 处 u32 + 槽5 表体 0x1000 处 u32，小端）。
本工具从包文件重新提取指纹并重写登记，档位数不变（n 由段大小自动识别，
Build 69 的 10 档与 Build 71 的 11 档通用；更高档数同样适用）。

用法：
  python3 重登记指纹.py <ph_menu.bin> <DXO_FILTERS目录> [-o 输出ph_menu.bin]

不给 -o 时输出到 <输入名>.reg.bin——绝不原地覆盖输入文件。
退出码：0 成功；1 出错（缺包/尺寸不对/布局不认识）。

边界：
  只重登记，不增删档位（档位数不变）。新增/删除档位、让菜单同步扩张/收缩，
  用同目录 管理滤镜.py（模板补丁构建器，离线已验证）；删除包文件后该档位
  行为未验证，请勿删除。
"""
import argparse, struct, sys
from pathlib import Path

STR_BASE = 0x08EF6400      # 路径+指纹段基址（Build 51 起固定）
PATH_PREFIX = b"c:\\DXO_FILTERS\\"
SLOT_BODY = 17536          # 每槽表体字节数
FP_OFFSET = 0x1000         # 指纹在表体内的偏移
PKG_SIZE = 5 * (8 + SLOT_BODY)   # 87,720


def parse_segments(data):
    segs, off = [], 0
    while off + 8 <= len(data):
        addr, size = struct.unpack_from("<II", data, off)
        off += 8
        if off + size > len(data):
            raise SystemExit("错误：ph_menu 段表损坏（段越过文件尾）")
        segs.append((addr, data[off:off + size]))
        off += size
    return segs


def extract_fingerprints(pkg_path):
    """从 87,720B 五槽包提取 (slot1_fp, slot5_fp)。"""
    data = pkg_path.read_bytes()
    if len(data) != PKG_SIZE:
        raise ValueError(f"包大小 {len(data)}B ≠ {PKG_SIZE}B（不是五槽包）")
    slots, pos = [], 0
    while pos < len(data):
        addr, size = struct.unpack_from("<II", data, pos)
        pos += 8
        if size != SLOT_BODY:
            raise ValueError(f"槽表体 {size}B ≠ {SLOT_BODY}B")
        slots.append((addr, data[pos:pos + size]))
        pos += size
    if len(slots) != 5:
        raise ValueError(f"槽数 {len(slots)} ≠ 5")
    addrs = [a for a, _ in slots]
    if addrs != sorted(addrs):
        raise ValueError("槽地址非递增，包结构可疑")
    fp1 = struct.unpack_from("<I", slots[1][1], FP_OFFSET)[0]
    fp5 = struct.unpack_from("<I", slots[4][1], FP_OFFSET)[0]
    return fp1, fp5


def main():
    ap = argparse.ArgumentParser(description="重登记 ph_menu.bin 指纹（档位数不变）")
    ap.add_argument("ph_menu", help="现有 ph_menu.bin（卡上或仓库 Lightning-L口/ 里的）")
    ap.add_argument("pkg_dir", help="DXO_FILTERS 目录（含替换后的 lut_*.bin）")
    ap.add_argument("-o", "--output", help="输出 ph_menu 路径（默认 <输入>.reg.bin）")
    args = ap.parse_args()

    src = Path(args.ph_menu)
    pkg_dir = Path(args.pkg_dir)
    out = Path(args.output) if args.output else src.with_suffix(src.suffix + ".reg.bin")
    if out.resolve() == src.resolve():
        raise SystemExit("错误：拒绝原地覆盖，请用 -o 指定别的输出路径")

    data = src.read_bytes()
    segs = parse_segments(data)
    strseg = next(((a, p) for a, p in segs if a == STR_BASE), None)
    if strseg is None:
        raise SystemExit(f"错误：没找到 0x{STR_BASE:08X} 的路径+指纹段，布局不认识")
    addr, body = strseg
    if len(body) % 40 != 0:
        raise SystemExit(f"错误：STR 段 {len(body)}B 不是 40 的倍数，布局不认识")
    n = len(body) // 40

    buf = bytearray(data)
    seg_off = data.find(body)          # 段载荷在文件内的偏移
    changed = 0
    print(f"档位数 n={n}（STR 段 {len(body)}B = {n}×32 路径 + {n}×8 指纹）\n")
    print(f"{'档':>3}  {'登记文件':28s} 指纹（槽2/槽5）            状态")
    for i in range(n):
        entry = body[i * 32:(i + 1) * 32]
        if not entry.startswith(PATH_PREFIX):
            raise SystemExit(f"错误：档位 {i} 路径不是 {PATH_PREFIX.decode()} 开头，布局不认识")
        fname = entry[len(PATH_PREFIX):].split(b"\0")[0].decode("ascii")
        pkg = pkg_dir / fname
        if not pkg.is_file():
            raise SystemExit(f"错误：找不到 {pkg}——档位只能替换内容，不能删除文件")
        try:
            fp1, fp5 = extract_fingerprints(pkg)
        except ValueError as e:
            raise SystemExit(f"错误：{fname} {e}")
        reg_off = seg_off + n * 32 + i * 8
        old = bytes(buf[reg_off:reg_off + 8])
        new = struct.pack("<II", fp1, fp5)
        status = "一致" if old == new else "已更新 ←"
        if old != new:
            buf[reg_off:reg_off + 8] = new
            changed += 1
        print(f"{i:>3}  {fname:28s} 0x{fp1:08x}/0x{fp5:08x}   {status}")

    out.write_bytes(bytes(buf))
    print(f"\n{changed}/{n} 组指纹有变化；已写出 {out}（{len(buf)}B）")
    print("下一步：把输出文件改名/拷贝为卡根的 ph_menu.bin（先备份原件），")
    print("逐字节回读校验后正常弹出卷。其余档位文件与 ph_menu 必须同套使用，")
    print("不要只换一半。")
    return 0


if __name__ == "__main__":
    sys.exit(main())
