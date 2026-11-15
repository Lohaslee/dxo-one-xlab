#!/usr/bin/env python3
"""重登记指纹.py 的独立测试（6 组 16 项）。

T1/T4/T5/T6 在"只有仓库文件"的环境可跑（用 patch/ 自带的十包）；
T2/T3 需要 实验/71_修复INSTA分发 的 11 档现场，缺失时跳过。
"""
import shutil, struct, subprocess, sys, tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
TOOL = HERE / "重登记指纹.py"
PATCH = ROOT / "patch"
EXP71 = ROOT / "实验/71_修复INSTA分发"
EXP70_PKG = ROOT / "实验/70_第十一个滤镜/pkg/lut_insta.bin"

PY = sys.executable
PASS, SKIP = [], []


def run(*args):
    return subprocess.run([PY, str(TOOL), *map(str, args)],
                          capture_output=True, text=True)


def check(name, cond, detail=""):
    if cond:
        PASS.append(name)
        print(f"  OK  {name}")
    else:
        print(f"  FAIL {name} {detail}")
        sys.exit(1)


def seg_str_payload(ph_menu_bytes):
    """返回 STR 段载荷（路径+指纹）。"""
    off = 0
    while off + 8 <= len(ph_menu_bytes):
        a, s = struct.unpack_from("<II", ph_menu_bytes, off)
        off += 8
        if a == 0x08EF6400:
            return ph_menu_bytes[off:off + s]
        off += s
    raise AssertionError("没找到 STR 段")


def main():
    tmp = Path(tempfile.mkdtemp(prefix="reReg_test_"))
    print("== T1 仓库 Build 69 布局 no-op（10 档，patch/ 自带数据）==")
    out1 = tmp / "t1.bin"
    r = run(PATCH / "ph_menu.bin", PATCH / "DXO_FILTERS", "-o", out1)
    check("T1a 工具退出码 0", r.returncode == 0, r.stderr)
    check("T1b 输出与输入逐字节一致",
          out1.read_bytes() == (PATCH / "ph_menu.bin").read_bytes())
    check("T1c 报告 0 组变化", "0/10 组指纹有变化" in r.stdout, r.stdout)

    has71 = EXP71.is_dir() and (EXP71 / "ph_menu.bin").is_file() and EXP70_PKG.is_file()
    if not has71:
        SKIP.append("T2/T3（缺 实验/71 现场）")
        print("  SKIP T2/T3：实验/71 现场不在（发布环境正常）")
    else:
        pkg11 = tmp / "pkgs11"
        pkg11.mkdir()
        for f in (PATCH / "DXO_FILTERS").glob("lut_*.bin"):
            shutil.copy2(f, pkg11 / f.name)
        shutil.copy2(EXP70_PKG, pkg11 / "lut_insta.bin")
        m71 = (EXP71 / "ph_menu.bin").read_bytes()
        print("== T2 Build 71 布局 no-op（11 档）==")
        out2 = tmp / "t2.bin"
        r = run(EXP71 / "ph_menu.bin", pkg11, "-o", out2)
        check("T2a 工具退出码 0", r.returncode == 0, r.stderr)
        check("T2b 输出与输入逐字节一致", out2.read_bytes() == m71)
        check("T2c 报告 0 组变化", "0/11 组指纹有变化" in r.stdout, r.stdout)

        print("== T3 篡改指纹后恢复 ==")
        bad = bytearray(m71)
        payload_off = m71.find(seg_str_payload(m71))
        n = len(seg_str_payload(m71)) // 40
        reg = payload_off + n * 32 + 1 * 8          # idx1 = lut_vel.bin
        bad[reg:reg + 8] = b"\xde\xad\xbe\xef\xde\xad\xbe\xef"
        badf = tmp / "bad.bin"
        badf.write_bytes(bytes(bad))
        out3 = tmp / "t3.bin"
        r = run(badf, pkg11, "-o", out3)
        check("T3a 工具退出码 0", r.returncode == 0, r.stderr)
        check("T3b 报告 1 组变化", "1/11 组指纹有变化" in r.stdout, r.stdout)
        check("T3c 恢复后与原版逐字节一致", out3.read_bytes() == m71)

    print("== T4 缺包报错 ==")
    empty = tmp / "empty"
    empty.mkdir()
    r = run(PATCH / "ph_menu.bin", empty, "-o", tmp / "t4.bin")
    check("T4a 退出码非 0", r.returncode != 0)
    check("T4b 报'档位只能替换内容，不能删除文件'", "不能删除文件" in r.stderr, r.stderr)
    check("T4c 没有产出输出文件", not (tmp / "t4.bin").exists())

    print("== T5 包尺寸不对报错 ==")
    onepkg = tmp / "one"
    onepkg.mkdir()
    for f in (PATCH / "DXO_FILTERS").glob("lut_*.bin"):
        shutil.copy2(f, onepkg / f.name)
    (onepkg / "lut_vel.bin").write_bytes(b"\0" * 100)
    r = run(PATCH / "ph_menu.bin", onepkg, "-o", tmp / "t5.bin")
    check("T5a 退出码非 0", r.returncode != 0)
    check("T5b 报'不是五槽包'", "不是五槽包" in r.stderr, r.stderr)

    print("== T6 拒绝原地覆盖 ==")
    same = tmp / "same.bin"
    same.write_bytes((PATCH / "ph_menu.bin").read_bytes())
    r = run(same, PATCH / "DXO_FILTERS", "-o", same)
    check("T6a 退出码非 0", r.returncode != 0)
    check("T6b 报'拒绝原地覆盖'", "拒绝原地覆盖" in r.stderr, r.stderr)

    shutil.rmtree(tmp)
    print(f"\n{len(PASS)} 项 PASS" + (f"；跳过：{'、'.join(SKIP)}" if SKIP else ""))
    return 0


if __name__ == "__main__":
    sys.exit(main())
