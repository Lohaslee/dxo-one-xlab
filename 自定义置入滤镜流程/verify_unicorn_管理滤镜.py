#!/usr/bin/env python3
"""unicorn 真执行门：把生成的 ph_menu 装进 ARM 仿真，逐 idx 跑 menu_draw_active，
在 menu_text_at 入口捕获实际分发出的名字串，证明分发链可达且 idx→名字正确。

这是比静态校验更强的离线门——Build 70 的 INSTA 死代码（装载正确、显示错误）
只有真执行才能暴露。依赖 vendored unicorn（实验/42_滤镜菜单离线拟真/deps）。
不接触相机、不写卡。实机验收仍由用户亲自做。

用法：python3 verify_unicorn_管理滤镜.py
"""
import struct
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent
sys.path.insert(0, str(ROOT / '实验/42_滤镜菜单离线拟真/deps'))
sys.path.insert(0, str(HERE))
from unicorn import Uc, UC_ARCH_ARM, UC_MODE_ARM, UC_HOOK_CODE
from unicorn.arm_const import (UC_ARM_REG_R0, UC_ARM_REG_SP, UC_ARM_REG_LR, UC_ARM_REG_PC)
import 管理滤镜 as M

FB = 0x095D5AA8
PAGE_ADDR = 0x095D6DAD
OLED_MODE = 0x095D6DAA
CORE_STATE = 0x08EF6100
FW_CLEAR = 0x08ECA0BC
FW_AREA = 0x08EE9D04
CODE_LO, CODE_SIZE = 0x08C00000, 0x003F0000   # 到 0x08FF0000，与 scratch 不重叠
SCRATCH_LO, SCRATCH_SIZE = 0x08FF0000, 0x20000
STOP = 0x08FF1000


def run_idx(menu_text: bytes, idx: int):
    """真执行 menu_draw_FLUSH(current=idx, r7=1)，返回 menu_text_at 收到的名字串。
    用 flush 而非 active：flush 走 .Ldc_flush 必调 fw_area，能暴露 fw_area 重定位失配
    （active r7=0 跳过 fw_area，对此类进页崩溃无感）。同时守卫固件入口绝对地址：
    若 bl 落进 fw_clear/fw_area 函数中间（而非精确入口）立即判失败。"""
    parts = M.split_menu(menu_text)
    # menu_text_at = emit 处 bl 的目标
    words = struct.unpack_from(f'<{len(menu_text)//4}I', menu_text, 0)
    mtat = M.dec_branch(words[parts['f1_start'] // 4 + 1], parts['f1_start'] + 4)
    mu = Uc(UC_ARCH_ARM, UC_MODE_ARM)
    mu.mem_map(CODE_LO, CODE_SIZE)
    mu.mem_write(M.MENU_BASE, menu_text)
    mu.mem_write(FW_CLEAR, struct.pack('<I', 0xE12FFF1E))   # bx lr 桩
    mu.mem_write(FW_AREA, struct.pack('<I', 0xE12FFF1E))
    mu.mem_map(FB & ~0xFFFF, 0x20000)                        # 帧缓冲+页标记+模式
    mu.mem_write(PAGE_ADDR, b'\x95')
    mu.mem_write(OLED_MODE, b'\x00')
    mu.mem_write(FB, bytes(64 * 48))
    mu.mem_write(CORE_STATE, struct.pack('<4I', idx, 0, 0, 0))
    mu.mem_map(SCRATCH_LO, SCRATCH_SIZE)
    mu.reg_write(UC_ARM_REG_SP, 0x08FF8000)
    mu.reg_write(UC_ARM_REG_LR, STOP)
    captured = []

    mtat_addr = M.MENU_BASE + mtat

    def hook(mu_, addr, size, user):
        if addr == mtat_addr:
            r0 = mu_.reg_read(UC_ARM_REG_R0)
            raw = bytes(mu_.mem_read(r0, 32))
            captured.append(raw.split(b'\0')[0].decode('ascii'))
        # 固件入口守卫：只允许精确入口，落进函数中间=重定位失配（实机进页崩溃源）
        if FW_CLEAR <= addr < FW_CLEAR + 0x40 and addr != FW_CLEAR:
            raise RuntimeError(f'bl fw_clear 落点 {addr:#x} ≠ 入口 {FW_CLEAR:#x}（重定位失配）')
        if FW_AREA <= addr < FW_AREA + 0x40 and addr != FW_AREA:
            raise RuntimeError(f'bl fw_area 落点 {addr:#x} ≠ 入口 {FW_AREA:#x}（重定位失配）')

    mu.hook_add(UC_HOOK_CODE, hook)
    # menu_draw_flush：第一个 push{r4,r6,r7,lr}=0xE92D40D0，mov r7,#1（flush 必调 fw_area）
    mda = next(i * 4 for i, w in enumerate(words) if w == 0xE92D40D0)
    mu.emu_start(M.MENU_BASE + mda, STOP, count=500000)
    if not captured:
        raise RuntimeError(f'idx{idx}: menu_text_at 未被调用（分发不可达）')
    return captured[0]


def check(menu_text: bytes, names, label: str) -> bool:
    ok = True
    for idx, expect in enumerate(names):
        try:
            got = run_idx(menu_text, idx)
        except Exception as e:
            print(f'    {label} idx{idx}: 执行异常 {e}')
            ok = False
            continue
        if got != expect:
            print(f'    {label} idx{idx}: 真执行 {got!r} != 期望 {expect!r} ✗')
            ok = False
    if ok:
        print(f'    {label}: n={len(names)} 全 idx 真执行分发 ✓')
    return ok


def main():
    ok = True
    for tag, rel in (('69', 'patch/ph_menu.bin'),
                     ('71', '实验/71_修复INSTA分发/ph_menu.bin'),
                     ('72', '实验/72_滤镜记忆/ph_menu.bin')):
        path = ROOT / rel
        if not path.exists():
            print(f'  [跳过] Build {tag}: 缺 {rel}')
            continue
        template = path.read_bytes()
        segs = M.parse_records(template)
        menu = M.get_seg(segs, M.MENU_BASE)
        entries = M.read_template_entries(template)
        names = [e[0] for e in entries]
        print(f'  Build {tag} (n={len(names)}):')
        ok &= check(menu, names, '模板')
        canon = M.rebuild_menu_text(menu, names, canonical=True)
        ok &= check(canon, names, '规范布局')
        if len(names) >= 2:
            d = M.rebuild_ph_menu(template, entries[:-1], canonical=True)
            ok &= check(M.get_seg(M.parse_records(d), M.MENU_BASE),
                        [e[0] for e in entries[:-1]], '删末档')
        if len(names) < M.MAX_SLOTS:
            fp = 0xABCD0000 + len(names)
            ea = entries + [('TEST', f'c:\\DXO_FILTERS\\lut_test.bin', fp, fp)]
            a = M.rebuild_ph_menu(template, ea, canonical=True)
            ok &= check(M.get_seg(M.parse_records(a), M.MENU_BASE),
                        [e[0] for e in ea], '增一档')
    # 满容量 n=12
    base = ROOT / 'patch/ph_menu.bin'
    if base.exists():
        template = base.read_bytes()
        entries = M.read_template_entries(template)
        while len(entries) < M.MAX_SLOTS:
            i = len(entries)
            entries.append((f'T{i}', f'c:\\DXO_FILTERS\\lut_t{i}.bin',
                            0xBEEF0000 + i, 0xBEEF0000 + i))
        ph = M.rebuild_ph_menu(template, entries, canonical=True)
        print(f'  满容量 n={M.MAX_SLOTS}:')
        ok &= check(M.get_seg(M.parse_records(ph), M.MENU_BASE),
                    [e[0] for e in entries], 'n=12')
    print('unicorn 真执行门', '通过' if ok else '失败')
    return 0 if ok else 1


if __name__ == '__main__':
    sys.exit(main())