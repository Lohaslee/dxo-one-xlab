#!/usr/bin/env python3
"""管理滤镜.py 的发布版独立测试（纯标准库，不依赖 unicorn）。

T1..T5 在"只有仓库文件"的环境可跑（用 patch/ 自带 Build 69 与十包）；
涉及 Build 71 的项在 实验/71 现场缺失时跳过（发布环境正常）。
unicorn 真执行门是另一道可选离线门，见 verify_unicorn_管理滤镜.py。
"""
import json
import shutil
import struct
import subprocess
import sys
import tempfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(HERE))
import 管理滤镜 as M

TOOL = HERE / "管理滤镜.py"
PATCH = ROOT / "patch"
PKG = PATCH / "DXO_FILTERS"
T69 = PATCH / "ph_menu.bin"
T71 = ROOT / "实验/71_修复INSTA分发/ph_menu.bin"
T72 = ROOT / "实验/72_滤镜记忆/ph_menu.bin"
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


def fp_off(slot):
    return (8 + M.SLOT_SIZE) * slot + 8 + M.FP_OFFSET


def make_synth_pkg(dst, fp, base=None):
    """复制一个真包并改写 slot1/slot5 指纹为 fp，造出指纹唯一的测试包。"""
    b = bytearray((base or (PKG / "lut_sato.bin")).read_bytes())
    struct.pack_into("<I", b, fp_off(1), fp)
    struct.pack_into("<I", b, fp_off(4), fp)
    dst.write_bytes(bytes(b))


def main():
    tmp = Path(tempfile.mkdtemp(prefix="mgmt_test_"))
    m69 = T69.read_bytes()
    segs69 = M.parse_records(m69)
    menu69 = M.get_seg(segs69, M.MENU_BASE)
    names69 = M.decode_names(M.split_menu(menu69)["N"])
    entries69 = M.read_template_entries(m69)

    print("== T1 ARM 编码/解码 ==")
    ok = all(M.dec_imm(M.enc_imm(v)) == v
             for v in (0, 1, 21, 0xFF, 0x100, 0x400, 0xFF000000))
    check("T1a 旋转立即数 round-trip", ok)
    check("T1a2 不可表示立即数报错", _raises(ValueError, M.enc_imm, 0x1234))
    check("T1b cmp/mov 寄存器位", M.enc_cmp(6, 9) == 0xE3560009
          and M.enc_mov(0, 9) == 0xE3A00009)
    w = M.enc_movw(2, 0x6580)
    check("T1c movw/movt imm16 拆分", (w & 0xFFF) == 0x580
          and ((w >> 16) & 0xF) == 6 and (w & 0xFFF0F000) == 0xE3002000)
    src, dst = 0x100, 0x200
    check("T1d branch round-trip", M.dec_branch(M.enc_branch(0, src, dst), src) == dst)
    check("T1e adr 目标在指令前报错", _raises(ValueError, M.enc_adr, 0, 0x200, 0x100))

    print("== T2 menu 切分 + no-op 逐字节回归 ==")
    parts = M.split_menu(menu69)
    check("T2a 固定块长度", len(parts["F1"]) == M.F1_LEN and len(parts["F2"]) == M.F2_LEN)
    check("T2b names decode/encode round-trip",
          M.decode_names(M.encode_names(names69)) == names69)
    check("T2c Build69 menu no-op 逐字节一致", M.rebuild_menu_text(menu69, names69) == menu69)
    check("T2d Build69 ph_menu no-op 逐字节一致",
          M.rebuild_ph_menu(m69, entries69) == m69)
    if T71.is_file():
        m71 = T71.read_bytes()
        menu71 = M.get_seg(M.parse_records(m71), M.MENU_BASE)
        names71 = M.decode_names(M.split_menu(menu71)["N"])
        e71 = M.read_template_entries(m71)
        check("T2e Build71 menu no-op 逐字节一致", M.rebuild_menu_text(menu71, names71) == menu71)
        check("T2f Build71 ph_menu no-op 逐字节一致", M.rebuild_ph_menu(m71, e71) == m71)
    else:
        SKIP.append("T2e/T2f（缺 实验/71）")
        print("  SKIP T2e/T2f：实验/71 现场不在")
    if T72.is_file():
        m72 = T72.read_bytes()
        menu72 = M.get_seg(M.parse_records(m72), M.MENU_BASE)
        names72 = M.decode_names(M.split_menu(menu72)["N"])
        e72 = M.read_template_entries(m72)
        check("T2g Build72 menu no-op 逐字节一致", M.rebuild_menu_text(menu72, names72) == menu72)
        check("T2h Build72 ph_menu no-op 逐字节一致", M.rebuild_ph_menu(m72, e72) == m72)
        check("T2i 布局识别 69/72", M.core_layout(M.get_seg(segs69, M.CORE_BASE)) == "69"
              and M.core_layout(M.get_seg(M.parse_records(m72), M.CORE_BASE)) == "72")
    else:
        SKIP.append("T2g/T2h/T2i（缺 实验/72）")
        print("  SKIP T2g/T2h/T2i：实验/72 现场不在")

    print("== T3 规范布局 + 静态分发校验 ==")
    canon = M.rebuild_menu_text(menu69, names69, canonical=True)
    cp = M.split_menu(canon)
    def mask_p(P):
        m = bytearray(P)
        for off in (M.P_DISP_BHI, M.P_F1_SETUP_A, M.P_F1_SETUP_B):
            m[off:off + 4] = b"\0" * 4
        return bytes(m)
    def mask_f1(F1):
        m = bytearray(F1)
        for off in (M.F1_BL_CLEAR, M.F1_ADR_WAIT, M.F1_BL_AREA, M.F1_ADR_INDEX, M.F1_ADR_FONT):
            m[off:off + 4] = b"\0" * 4
        return bytes(m)
    check("T3a 规范布局 F1(除跨区引用)/F2 逐字节一致",
          mask_f1(cp["F1"]) == mask_f1(parts["F1"]) and cp["F2"] == parts["F2"])
    check("T3b 规范布局 P(除三条进页分支) 一致", mask_p(parts["P"]) == mask_p(cp["P"]))
    check("T3c 模板静态分发", _ok(M.verify_dispatch, menu69, names69))
    check("T3d 规范布局静态分发", _ok(M.verify_dispatch, canon, names69))
    deleted = names69[:-1]
    md = M.rebuild_menu_text(menu69, deleted, canonical=True)
    check("T3e 删末档分发 (n=%d)" % len(deleted), _ok(M.verify_dispatch, md, deleted))
    added = names69 + ["TEST"]
    ma = M.rebuild_menu_text(menu69, added, canonical=True)
    check("T3f 增一档分发 (n=%d)" % len(added), _ok(M.verify_dispatch, ma, added))
    # T3g 崩溃根因回归：进页分支 P+68c/6a0 若未随 V 移位重定位（落点写死），分发校验必须拒绝。
    broken = bytearray(canon)
    for off in (M.P_F1_SETUP_A, M.P_F1_SETUP_B):
        struct.pack_into("<I", broken, off, struct.unpack_from("<I", parts["P"], off)[0])
    check("T3g 进页分支失配被拒（进页即崩根因）", not _ok(M.verify_dispatch, bytes(broken), names69))
    # T3h 崩溃根因回归②：F1 的 fw_area bl 若未随 F1 平移重定位（落进固件函数中间），
    # menu_draw_flush 进页必调 fw_area→实机进页即崩；分发校验必须拒绝（active 路径不调 fw_area 故无感）。
    broken2 = bytearray(canon)
    struct.pack_into("<I", broken2, cp["f1_start"] + M.F1_BL_AREA,
                     struct.unpack_from("<I", parts["F1"], M.F1_BL_AREA)[0])
    check("T3h fw_area 重定位失配被拒（进页即崩根因②）", not _ok(M.verify_dispatch, bytes(broken2), names69))

    print("== T4 ph_menu 增删 menu+core+STR 一致 ==")
    def ph_consistent(ph, ents):
        ss = M.parse_records(ph)
        nn = len(ents)
        M.verify_dispatch(M.get_seg(ss, M.MENU_BASE), [e[0] for e in ents])
        pl = M.read_preload(M.get_seg(ss, M.CORE_BASE))
        M.verify_core(M.get_seg(ss, M.CORE_BASE), nn, pl)
        return M.get_seg(ss, M.STR_BASE) == M.build_STR(ents)
    check("T4a 删末档 ph_menu 一致",
          ph_consistent(M.rebuild_ph_menu(m69, entries69[:-1], canonical=True), entries69[:-1]))
    fp_new = 0xABCD0000 + len(entries69)
    ent_add = entries69 + [("TEST", "c:\\DXO_FILTERS\\lut_test.bin", fp_new, fp_new)]
    check("T4b 增一档 ph_menu 一致",
          ph_consistent(M.rebuild_ph_menu(m69, ent_add, canonical=True), ent_add))
    check("T4c slot1 指纹重复被拒",
          _raises(ValueError, M.rebuild_ph_menu, m69,
                  entries69[:1] + entries69[:1]))
    if T72.is_file():
        m72 = T72.read_bytes()
        e72 = M.read_template_entries(m72)
        check("T4d Build72 删末档 ph_menu 一致",
              ph_consistent(M.rebuild_ph_menu(m72, e72[:-1], canonical=True), e72[:-1]))
        fp72 = 0xABCD0000 + len(e72)
        ent72 = e72 + [("TEST", "c:\\DXO_FILTERS\\lut_test.bin", fp72, fp72)]
        check("T4e Build72 增一档 ph_menu 一致",
              ph_consistent(M.rebuild_ph_menu(m72, ent72, canonical=True), ent72))
        check("T4f Build72 改预装档生效",
              M.read_preload(M.get_seg(M.parse_records(
                  M.rebuild_ph_menu(m72, e72, preload=5)), M.CORE_BASE)) == 5)
    else:
        SKIP.append("T4d/T4e/T4f（缺 实验/72）")
        print("  SKIP T4d/T4e/T4f：实验/72 现场不在")

    print("== T5 包指纹提取 + 输入校验 ==")
    ok = True
    for nm, path, fp1, fp5 in entries69:
        got = M.extract_fingerprints(PKG / Path(path.split("\\")[-1]))
        ok &= got == (fp1, fp5)
    check("T5a 十包指纹与模板 STR 逐一相符", ok)
    check("T5b 包尺寸不对报错", _raises(ValueError, M.extract_fingerprints, _bad_pkg(tmp)))
    check("T5c 名字缺字被拒", _raises(ValueError, M.validate_name, "ABC"))
    check("T5d 名字超长被拒", _raises(ValueError, M.validate_name, "ABCDEFG"))
    check("T5e 合法名字通过", M.validate_name("R-POSI") == "R-POSI")
    check("T5f 卡路径过长被拒", _raises(ValueError, M.validate_card_path,
                                        M.CARD_PREFIX + "x" * 30))
    check("T5g build_STR/read round-trip",
          M.read_template_entries(M.rebuild_ph_menu(m69, entries69)) == entries69)

    print("== T6 CLI list/add/delete/rebuild + 安全闸 ==")
    r = run("list", T69)
    check("T6a list 退出码 0 且含 10 档", r.returncode == 0 and "10 档" in r.stdout, r.stderr)
    # 全量清单 no-op rebuild → 逐字节一致
    manifest = tmp / "m.json"
    manifest.write_text(json.dumps({"preload": 2, "filters": [
        {"name": e[0], "pkg": e[1].split("\\")[-1]} for e in entries69]}), encoding="utf-8")
    out = tmp / "rebuilt.bin"
    r = run("rebuild", T69, "--manifest", manifest, "--pkg-dir", PKG, "-o", out)
    check("T6b rebuild no-op 退出码 0", r.returncode == 0, r.stderr)
    check("T6c rebuild no-op 逐字节一致", out.read_bytes() == m69)
    # 删多档
    out = tmp / "del2.bin"
    r = run("delete", T69, "--idx", "9", "--idx", "5", "-o", out)
    check("T6d delete 多档退出码 0", r.returncode == 0, r.stderr)
    dnames = M.decode_names(M.split_menu(M.get_seg(M.parse_records(out.read_bytes()),
                                                   M.MENU_BASE))["N"])
    check("T6e delete 后 n=8 且菜单同步", len(dnames) == 8
          and dnames == [e[0] for i, e in enumerate(entries69) if i not in (5, 9)])
    # 删后重加（用真包，先删 idx9 腾出指纹）
    d1 = tmp / "del1.bin"
    run("delete", T69, "--idx", "9", "-o", d1)
    out = tmp / "add.bin"
    r = run("add", d1, "--name", "SATOBI", "--pkg", PKG / "lut_sato.bin", "-o", out)
    check("T6f add 退出码 0", r.returncode == 0, r.stderr)
    anames = M.decode_names(M.split_menu(M.get_seg(M.parse_records(out.read_bytes()),
                                                   M.MENU_BASE))["N"])
    check("T6g add 后恢复 10 档", anames == names69)
    # 容量上限：造两个唯一指纹包加到 12，再加第 13 个须失败
    p10, p11, p12 = tmp / "t10.bin", tmp / "t11.bin", tmp / "t12.bin"
    make_synth_pkg(p10, 0x11110000)
    make_synth_pkg(p11, 0x22220000)
    make_synth_pkg(p12, 0x33330000)
    c11, c12 = tmp / "c11.bin", tmp / "c12.bin"
    run("add", T69, "--name", "T20", "--pkg", p10, "-o", c11)
    r = run("add", c11, "--name", "T22", "--pkg", p11, "-o", c12)
    check("T6h 加到 n=12 退出码 0", r.returncode == 0, r.stderr)
    r = run("add", c12, "--name", "T25", "--pkg", p12, "-o", tmp / "c13.bin")
    check("T6i 第 13 档被容量闸拒绝", r.returncode != 0 and "上限" in r.stderr, r.stderr)
    # 安全闸与输入校验
    r = run("delete", T69, "--idx", "9", "-o", T69)
    check("T6j 拒绝原地覆盖", r.returncode != 0 and "拒绝原地覆盖" in r.stderr, r.stderr)
    r = run("delete", T69, "--idx", "9", "-o", "/Volumes/CARD/ph_menu.bin")
    check("T6k 拒绝写 /Volumes/", r.returncode != 0 and "/Volumes/" in r.stderr, r.stderr)
    r = run("add", T69, "--name", "ABC", "--pkg", PKG / "lut_sato.bin", "-o", tmp / "x.bin")
    check("T6l 名字缺字被拒", r.returncode != 0 and "缺字" in r.stderr, r.stderr)
    r = run("delete", T69, "--idx", "0", "--idx", "1", "--idx", "2", "--idx", "3",
            "--idx", "4", "--idx", "5", "--idx", "6", "--idx", "7", "--idx", "8",
            "--idx", "9", "-o", tmp / "z.bin")
    check("T6m 删空被拒", r.returncode != 0 and "不能删空" in r.stderr, r.stderr)
    # 自定义信标
    out = tmp / "magic.bin"
    r = run("delete", T69, "--idx", "9", "--custom-magic", "-o", out)
    hdr = M.get_seg(M.parse_records(out.read_bytes()), 0x08EF61F0)
    check("T6n --custom-magic 写入 C5GP", r.returncode == 0
          and struct.unpack_from("<I", hdr, 12)[0] == M.CUSTOM_MAGIC)

    shutil.rmtree(tmp)
    print(f"\n{len(PASS)} 项 PASS" + (f"；跳过：{'、'.join(SKIP)}" if SKIP else ""))
    return 0


def _raises(exc, fn, *a):
    try:
        fn(*a)
    except exc:
        return True
    except Exception:
        return False
    return False


def _ok(fn, *a):
    try:
        fn(*a)
        return True
    except Exception:
        return False


def _bad_pkg(tmp):
    p = tmp / "bad.bin"
    p.write_bytes(b"\0" * 100)
    return p


if __name__ == "__main__":
    sys.exit(main())