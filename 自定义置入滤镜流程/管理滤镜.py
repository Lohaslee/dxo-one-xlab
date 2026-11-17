#!/usr/bin/env python3
"""管理滤镜：在已验证菜单模板上做「增 / 删 / 改清单」的模板补丁构建器。

不重新编译固件、不需要用户侧 Xcode：以固定回归夹具 testdata/ph_menu_69.bin（Build 69 十档）为模板，
用户自定义时传入发布层 Lightning-L口/ph_menu.bin（当前 Build 72），
只重写随清单变化的 menu.text 分发链/名字串、core.text 立即数、STR 路径与指纹区，
其余固件补丁段逐字节保留。ph_menu.bin 仍由本工具生成（不手改二进制）。

离线工具：不接触相机、不写卡。实机验收由用户亲自做。
"""
from __future__ import annotations
import argparse
import json
import struct
import sys
from pathlib import Path

# ---- 内存布局常量（与实验层 build_71.py 一致）----
MENU_BASE, MENU_LIMIT = 0x08EF2800, 0x08EF3800
CORE_BASE, CORE_LIMIT = 0x08EF3800, 0x08EF5000
STR_BASE = 0x08EF6400
DURATIONS_BASE = 0x08EF6600
PATH_STRIDE = 32
SLOT_ADDRS = [0x092D72E8, 0x092DB768, 0x092DFBE8, 0x092E4068, 0x092E84E8]
SLOT_SIZE, SLOT_COUNT = 17536, 5
MAX_SLOTS = (DURATIONS_BASE - STR_BASE) // (PATH_STRIDE + 8)  # 512//40 = 12

# 自制字模字符集（9x12，23 字符）；缺 C D J M Q X Z 与数字 1 3 4 6 7 8 9
CHARSET = set('-025ABEFGHIKLNOPRSTUVWY')
MAX_NAME_LEN = 6  # 箭头间 [5,59] 共 54px / 9px = 6 字符

# 五槽包格式（与 重登记指纹.py 一致）
CARD_PREFIX = 'c:\\DXO_FILTERS\\'
PKG_SIZE = SLOT_COUNT * (8 + SLOT_SIZE)   # 87,720B
FP_OFFSET = 0x1000                        # 指纹在槽表体内的偏移
# LOG_HDR 信标：69=D5GP / 70=E5GP / 71=F5GP / 57=75GP；自定义构建可选 C5GP 以便实机区分
CUSTOM_MAGIC = 0x50473543                 # "C5GP"

# menu.text 内固定补丁点（P 区，长度恒定，偏移稳定）
P_CYC_UP_CMP = 0x558     # cmp r0,#(n-1)   .Lfk_up 上限
P_CYC_WRAP_CMP = 0x564   # cmp r0,#n       .Lfk_up 回绕
P_CYC_DN_CMP = 0x57C     # cmp r0,#(n-1)   .Lfk_down 上限
P_CYC_DN_MOV = 0x58C     # mov r0,#(n-1)   .Lfk_down 回绕
P_DISP_CMP = 0x6A8       # cmp r6,#(n-1)   分发上限
P_DISP_BHI = 0x6AC       # bhi .Ldc_unknown
P_F1_SETUP_A = 0x68C     # b .Ldc_setup    进页状态设置入口A（跳 F1+0x68）
P_F1_SETUP_B = 0x6A0     # b .Ldc_setup    进页状态设置入口B（跳 F1+0x68）
F1_UNKNOWN_OFF = 0x20    # .Ldc_unknown 在 F1 内偏移
F1_SETUP_OFF = 0x68      # .Ldc_setup 在 F1 内偏移（movw/movt r4,#0x08EF6000）
F1_ADR_INDEX = 0x114     # adr r9,menu_index 在 F1 内偏移
F1_ADR_FONT = 0x12C      # adr r9,menu_font  在 F1 内偏移
F1_BL_CLEAR = 0x30       # .Ldc_unknown bl fw_clear（固件绝对地址，F1 平移须重定位）
F1_ADR_WAIT = 0x3C       # .Ldc_unknown adr r0,WAIT（N 区，增删须重定位）
F1_BL_AREA = 0x64        # .Ldc_flush bl fw_area（固件绝对地址；flush 路径每次进页必调）
FW_CLEAR_ADDR = 0x08ECA0BC
FW_AREA_ADDR = 0x08EE9D04
F2_FONT_OFF = 96         # menu_font 在 F2 内偏移（menu_index 占 96B）
F1_LEN = 420             # emit+unknown+flush+return+text_at+draw_text 恒定
F2_LEN = 648             # menu_index(96)+menu_font(23*24=552) 恒定


# ================= ARM 编码 / 解码 =================
def enc_imm(val: int) -> int:
    """把 32 位值编码为 ARM 旋转立即数 (rot<<8)|imm8；不可表示则报错。"""
    if not 0 <= val <= 0xFFFFFFFF:
        raise ValueError(f'立即数越界 {val:#x}')
    for rot in range(16):
        k = 2 * rot
        imm8 = ((val << k) | (val >> (32 - k))) & 0xFFFFFFFF if k else val
        if imm8 <= 0xFF:
            return (rot << 8) | imm8
    raise ValueError(f'立即数不可表示为 ARM 旋转立即数：{val:#x}')


def dec_imm(word: int) -> int:
    imm8 = word & 0xFF
    rot = (word >> 8) & 0xF
    k = 2 * rot
    return ((imm8 >> k) | (imm8 << (32 - k))) & 0xFFFFFFFF if k else imm8


def enc_branch(cond: int, src: int, dst: int, link: bool = False) -> int:
    d = dst - src - 8
    if d % 4 or not -(1 << 25) <= d < (1 << 25):
        raise ValueError(f'ARM 跳转越界 {src:#x}->{dst:#x}')
    w = (cond << 28) | 0x0A000000 | ((d >> 2) & 0xFFFFFF)
    return w | 0x01000000 if link else w


def dec_branch(word: int, src: int) -> int:
    d = (word & 0xFFFFFF) << 2
    if d & (1 << 25):
        d -= 1 << 26
    return src + 8 + d


def enc_cmp(rd: int, imm: int) -> int:
    if not 0 <= imm <= 255:
        raise ValueError(f'cmp 立即数越界 {imm}')
    return 0xE3500000 | (rd << 16) | imm


def enc_mov(rd: int, imm: int) -> int:
    if not 0 <= imm <= 255:
        raise ValueError(f'mov 立即数越界 {imm}')
    return 0xE3A00000 | (rd << 12) | imm


def enc_adr(rd: int, src: int, dst: int) -> int:
    """add rd, pc, #imm（dst 须在 src 之后，偏移可表示为旋转立即数）。"""
    off = dst - src - 8
    if off < 0:
        raise ValueError(f'adr 目标在指令之前 {src:#x}->{dst:#x}')
    return 0xE28F0000 | (rd << 12) | enc_imm(off)


# ================= ph_menu 记录 =================
def parse_records(data: bytes):
    pos, out = 0, []
    while pos < len(data):
        if pos + 8 > len(data):
            raise ValueError('记录头截断')
        addr, size = struct.unpack_from('<II', data, pos)
        pos += 8
        if not size or pos + size > len(data):
            raise ValueError('记录载荷截断或为空')
        out.append((addr, data[pos:pos + size]))
        pos += size
    return out


def build_records(segments) -> bytes:
    return b''.join(struct.pack('<II', a, len(p)) + p for a, p in segments)


def get_seg(segments, addr):
    for a, p in segments:
        if a == addr:
            return p
    raise KeyError(f'缺段 {addr:#010x}')


# ================= menu.text 切分 =================
def split_menu(menu: bytes):
    """按指令签名把 menu.text 切成 P / V / F1 / N / F2（不依赖硬编码长度）。"""
    if len(menu) % 4:
        raise ValueError('menu.text 非 4 字节对齐')
    words = struct.unpack_from(f'<{len(menu)//4}I', menu, 0)
    # V 起点：分发链第一条 cmp r6,#0
    v_start = next((i * 4 for i, w in enumerate(words) if w == 0xE3560000), None)
    if v_start is None:
        raise ValueError('未找到分发链起点 cmp r6,#0')
    # F1 起点：.Ldc_emit 的 mov r2,#21
    f1_start = next((i * 4 for i, w in enumerate(words)
                     if w == 0xE3A02015 and i * 4 > v_start), None)
    if f1_start is None:
        raise ValueError('未找到 .Ldc_emit')
    # menu_text_at = emit 处 bl 的目标
    mtat = dec_branch(words[f1_start // 4 + 1], f1_start + 4)
    # menu_draw_text = menu_text_at 之后第一条 push {r4-r11,lr}
    mdt = next((i * 4 for i, w in enumerate(words)
                if w == 0xE92D4FF0 and i * 4 > mtat), None)
    if mdt is None:
        raise ValueError('未找到 menu_draw_text')
    # draw_text 收尾 bx lr → N 起点
    bx = next((i * 4 for i, w in enumerate(words)
               if w == 0xE12FFF1E and i * 4 > mdt), None)
    if bx is None:
        raise ValueError('未找到 menu_draw_text 收尾')
    n_start = bx + 4
    # F2 起点 = draw_text 内 adr r9,menu_index 的目标
    f2_start = None
    for i in range(mdt // 4, bx // 4):
        w = words[i]
        if (w & 0xFFFFF000) == 0xE28F9000:
            f2_start = i * 4 + 8 + dec_imm(w)
            break
    if f2_start is None or f2_start < n_start:
        raise ValueError('未找到 menu_index')
    return {
        'P': menu[:v_start], 'V': menu[v_start:f1_start],
        'F1': menu[f1_start:n_start], 'N': menu[n_start:f2_start],
        'F2': menu[f2_start:],
        'v_start': v_start, 'f1_start': f1_start,
        'n_start': n_start, 'f2_start': f2_start,
    }


def decode_names(N: bytes):
    """N 区 = idx 顺序名字串 + 末尾 WAIT，每条 .asciz 补到 4B 对齐。"""
    names, pos = [], 0
    while pos < len(N):
        end = N.index(b'\0', pos)
        names.append(N[pos:end].decode('ascii'))
        pos = (end + 1 + 3) & ~3
    if not names or names[-1] != 'WAIT':
        raise ValueError('N 区末尾不是 WAIT')
    return names[:-1]


def encode_names(names) -> bytes:
    out = bytearray()
    for nm in list(names) + ['WAIT']:
        b = nm.encode('ascii') + b'\0'
        out += b + b'\0' * ((-len(b)) & 3)
    return bytes(out)


# ================= menu.text 生成 =================
def gen_V(names, v_start, f1_start, n_start) -> bytes:
    """规范布局：分发链 cmp/beq idx0..n-2；名字块默认项(n-1)在前，随后 idx0..n-2。"""
    n = len(names)
    if n < 1:
        raise ValueError('至少保留一个档位')
    name_off, acc = [], 0
    for nm in names:
        name_off.append(acc)
        acc += (len(nm) + 1 + 3) & ~3
    block_order = [n - 1] + list(range(n - 1))
    pos_of = {idx: p for p, idx in enumerate(block_order)}
    chain_len = 8 * (n - 1)
    block_base = v_start + chain_len

    def block_addr(idx):
        return block_base + 12 * pos_of[idx]

    out = bytearray()
    for idx in range(n - 1):                       # 分发链
        out += struct.pack('<I', enc_cmp(6, idx))
        out += struct.pack('<I', enc_branch(0, v_start + 8 * idx + 4, block_addr(idx)))
    for idx in block_order:                        # 名字块
        ba = block_addr(idx)
        out += struct.pack('<I', enc_adr(0, ba, n_start + name_off[idx]))
        out += struct.pack('<I', enc_mov(1, len(names[idx])))
        out += struct.pack('<I', enc_branch(14, ba + 8, f1_start))
    return bytes(out)


def patch_P(P: bytes, n: int, f1_start: int) -> bytes:
    b = bytearray(P)

    def put(off, word, mask, expect):
        old = struct.unpack_from('<I', b, off)[0]
        if old & mask != expect:
            raise ValueError(f'P 补丁点 {off:#x}  opcode 不符：{old:#010x}')
        struct.pack_into('<I', b, off, word)

    put(P_CYC_UP_CMP, enc_cmp(0, n - 1), 0xFFFFF000, 0xE3500000)
    put(P_CYC_WRAP_CMP, enc_cmp(0, n), 0xFFFFF000, 0xE3500000)
    put(P_CYC_DN_CMP, enc_cmp(0, n - 1), 0xFFFFF000, 0xE3500000)
    put(P_CYC_DN_MOV, enc_mov(0, n - 1), 0xFFFFF000, 0xE3A00000)
    put(P_DISP_CMP, enc_cmp(6, n - 1), 0xFFFFF000, 0xE3560000)
    put(P_DISP_BHI, enc_branch(8, P_DISP_BHI, f1_start + F1_UNKNOWN_OFF),
        0xFF000000, 0x8A000000)
    # 进页两条条件分支（bne）跳 F1+0x68 状态设置入口；V 长度变化使 F1 移位时必须同步重定位，
    # 否则落点写死跳进 F1 指令中间→进滤镜页即崩（69/71 因 V 未变而侥幸正确）。保留原条件码。
    for off in (P_F1_SETUP_A, P_F1_SETUP_B):
        old = struct.unpack_from('<I', b, off)[0]
        if old & 0x0F000000 != 0x0A000000:
            raise ValueError(f'P 进页分支 {off:#x} 非分支指令：{old:#010x}')
        struct.pack_into('<I', b, off,
                         enc_branch(old >> 28, off, f1_start + F1_SETUP_OFF))
    return bytes(b)


def patch_F1(F1: bytes, f1_start: int, f2_start: int, n_start: int,
             wait_off: int, tmpl: dict) -> bytes:
    """重定位 F1 内所有跨区 PC 相对引用（F1 整体随 V 长度平移时必须同步）。

    - F1 内部引用（bl menu_text_at / menu_draw_text、区内分支）：源与目标同平移，自然保持，不动。
    - 固件 bl（fw_clear / fw_area，绝对地址固定）：目标不随菜单平移，须按新 F1 位置重编码。
      漏补则 menu_draw_flush(r7=1) 调 fw_area 跳进固件函数中间→进滤镜页即崩；
      menu_draw_active(r7=0) 不调 fw_area，故旧 unicorn 门对此无感。
    - N 区 WAIT adr：增删时 N 长度变化，WAIT 偏移改变，须指向新 N 的 WAIT。
    - F2 index/font adr：F2 块内偏移恒定，指向新 F2 对应位置。
    """
    b = bytearray(F1)
    tf1, tn, tf2 = tmpl['f1_start'], tmpl['n_start'], tmpl['f2_start']
    words = struct.unpack(f'<{len(F1)//4}I', F1)
    for i, w in enumerate(words):
        off = i * 4
        src_old = tf1 + off
        if w & 0x0E000000 == 0x0A000000:          # b / bl（含 link 位）
            tgt = dec_branch(w, src_old)
            is_adr, rd, cond, link = False, 0, w >> 28, bool(w & 0x01000000)
        elif w & 0xFFFF0000 == 0xE28F0000:        # adr rd, pc, #imm（rd 任意，掩码忽略 rd 字段）
            tgt = src_old + 8 + dec_imm(w)
            is_adr, rd, cond, link = True, (w >> 12) & 0xF, 0, False
        else:
            continue
        if tf1 <= tgt < tf1 + len(F1):            # F1 内部，自然平移
            continue
        if tgt >= tf2:                            # F2（index/font）
            new = f2_start + (tgt - tf2)
        elif tgt >= tn:                           # N（仅 WAIT，用新偏移）
            new = n_start + wait_off
        else:                                     # 固件绝对地址（fw_clear/fw_area）
            new = tgt
        if is_adr:
            struct.pack_into('<I', b, off, enc_adr(rd, f1_start + off, new))
        else:
            struct.pack_into('<I', b, off, enc_branch(cond, f1_start + off, new, link=link))
    return bytes(b)


def rebuild_menu_text(template_menu: bytes, names, canonical: bool = False) -> bytes:
    """按新清单重建 menu.text。清单与模板一致时 V/N 逐字节复用（no-op 不改已验证产物）；
    canonical=True 强制用规范布局重生成 V（用于测试编码器与增删路径）。"""
    names = list(names)
    parts = split_menu(template_menu)
    P, V, F1, N, F2 = parts['P'], parts['V'], parts['F1'], parts['N'], parts['F2']
    if len(F1) != F1_LEN or len(F2) != F2_LEN:
        raise ValueError(f'模板固定块长度异常 F1={len(F1)} F2={len(F2)}')
    tmpl_names = decode_names(N)
    n = len(names)
    if not 1 <= n <= MAX_SLOTS:
        raise ValueError(f'档位数 {n} 超出 1..{MAX_SLOTS}')
    v_start = len(P)
    if names == tmpl_names and not canonical:
        V_new, N_new = V, N
    else:
        N_new = encode_names(names)
        vlen = 8 * (n - 1) + 12 * n          # 分发链 + 名字块，长度只与 n 有关
        V_new = gen_V(names, v_start, v_start + vlen, v_start + vlen + F1_LEN)
    f1_start = v_start + len(V_new)
    n_start = f1_start + F1_LEN
    f2_start = n_start + len(N_new)
    P_new = patch_P(P, n, f1_start)
    F1_new = patch_F1(F1, f1_start, f2_start, n_start, len(N_new) - 8, parts)
    menu = P_new + V_new + F1_new + N_new + F2
    if len(menu) > MENU_LIMIT - MENU_BASE:
        raise ValueError(f'menu.text {len(menu)}B 超出分配区 {MENU_LIMIT-MENU_BASE}B')
    return menu


# ================= core.text 补丁 + STR + 重组 =================
C_SELECT_CMP = 0x02C     # cmp r4,#(n-1)   filter_select 上限
C_EXP_MOVW = 0x180       # movw r2,#lower16(EXP_BASE)
C_EXP_MOVT = 0x184       # movt r2,#upper16(EXP_BASE)
# bootstrap 补丁点分布局：69/71 无记忆；72 带滤镜记忆（read_last+防抖清零使 bootstrap 后移）
C_BOOT_CMP_69 = 0x304    # cmp r0,#(n-1)   filter_bootstrap 上限
C_PRELOAD_MOV_69 = 0x314  # mov r0,#preload 开机预装档
C_BOOT_CMP_72 = 0x30C    # cmp r0,#(n-1)   current 已知判定
C_BOOT_LAST_CMP_72 = 0x344  # cmp r0,#(n-1) 记忆 idx 范围校验
C_PRELOAD_MOV_72 = 0x36C  # mov r0,#preload 回退预装档


def core_layout(core: bytes) -> str:
    """识别 core 布局：72 的 0x304 是 MOV32 r0,CORE_STATE 的 movt 上半字，
    69/71 的 0x304 是 cmp r0,#(n-1)。其余 opcode 不符则拒绝。"""
    w = struct.unpack_from('<I', core, C_BOOT_CMP_69)[0]
    if w == 0xE3400000 | (0 << 12) | 0x08EF:   # movt r0,#0x08EF
        return '72'
    if w & 0xFFFFF000 == 0xE3500000:           # cmp r0,#imm
        return '69'
    raise ValueError(f'core 布局不认识：0x304 处 opcode {w:#010x}')


def boot_points(layout: str):
    if layout == '72':
        return C_BOOT_CMP_72, C_BOOT_LAST_CMP_72, C_PRELOAD_MOV_72
    return C_BOOT_CMP_69, None, C_PRELOAD_MOV_69


def read_preload(core: bytes) -> int:
    _, _, preload_mov = boot_points(core_layout(core))
    return struct.unpack_from('<I', core, preload_mov)[0] & 0xFFF


def enc_movw(rd: int, imm16: int) -> int:
    return 0xE3000000 | (((imm16 >> 12) & 0xF) << 16) | (rd << 12) | (imm16 & 0xFFF)


def enc_movt(rd: int, imm16: int) -> int:
    return 0xE3400000 | (((imm16 >> 12) & 0xF) << 16) | (rd << 12) | (imm16 & 0xFFF)


def patch_core(core: bytes, n: int, preload: int) -> bytes:
    if not 1 <= n <= MAX_SLOTS:
        raise ValueError(f'档位数 {n} 超出 1..{MAX_SLOTS}')
    if not 0 <= preload < n:
        raise ValueError(f'预装档 {preload} 越界（须 0..{n-1}）')
    layout = core_layout(core)
    boot_cmp, last_cmp, preload_mov = boot_points(layout)
    b = bytearray(core)
    exp_base = STR_BASE + n * PATH_STRIDE
    if exp_base + n * 8 > DURATIONS_BASE:
        raise ValueError('STR 区超出分配范围')

    def put(off, word, mask, expect):
        old = struct.unpack_from('<I', b, off)[0]
        if old & mask != expect:
            raise ValueError(f'core 补丁点 {off:#x} opcode 不符：{old:#010x}')
        struct.pack_into('<I', b, off, word)

    put(C_SELECT_CMP, enc_cmp(4, n - 1), 0xFFFFF000, 0xE3540000)
    put(C_EXP_MOVW, enc_movw(2, exp_base & 0xFFFF), 0xFFF0F000, 0xE3002000)
    put(C_EXP_MOVT, enc_movt(2, (exp_base >> 16) & 0xFFFF), 0xFFF0F000, 0xE3402000)
    put(boot_cmp, enc_cmp(0, n - 1), 0xFFFFF000, 0xE3500000)
    if last_cmp is not None:
        put(last_cmp, enc_cmp(0, n - 1), 0xFFFFF000, 0xE3500000)
    put(preload_mov, enc_mov(0, preload), 0xFFFFF000, 0xE3A00000)
    return bytes(b)


def verify_core(core: bytes, n: int, preload: int) -> None:
    """核对 core.text 的立即数补丁点与 n/preload/EXP_BASE 一致（按布局）。"""
    layout = core_layout(core)
    boot_cmp, last_cmp, preload_mov = boot_points(layout)
    cw = struct.unpack_from(f'<{len(core)//4}I', core, 0)
    exp_base = STR_BASE + n * PATH_STRIDE
    assert cw[C_SELECT_CMP // 4] == enc_cmp(4, n - 1), 'filter_select 上限不符'
    assert cw[C_EXP_MOVW // 4] == enc_movw(2, exp_base & 0xFFFF), 'EXP_BASE movw 不符'
    assert cw[C_EXP_MOVT // 4] == enc_movt(2, (exp_base >> 16) & 0xFFFF), 'EXP_BASE movt 不符'
    assert cw[boot_cmp // 4] == enc_cmp(0, n - 1), 'bootstrap 上限不符'
    if last_cmp is not None:
        assert cw[last_cmp // 4] == enc_cmp(0, n - 1), '记忆 idx 范围上限不符'
    assert cw[preload_mov // 4] == enc_mov(0, preload), '预装档不符'


def build_STR(entries) -> bytes:
    """entries: (name, card_path, fp1, fp5)。n×32B 路径 + n×8B 指纹。"""
    n = len(entries)
    paths = bytearray()
    for _, card_path, _, _ in entries:
        b = card_path.encode('ascii')
        if len(b) >= PATH_STRIDE or b'\0' in b:
            raise ValueError(f'路径过长或含 null：{card_path!r}')
        paths += b + b'\0' * (PATH_STRIDE - len(b))
    fps = b''.join(struct.pack('<2I', fp1, fp5) for _, _, fp1, fp5 in entries)
    out = bytes(paths) + fps
    if len(out) != n * (PATH_STRIDE + 8):
        raise ValueError('STR 长度异常')
    return out


def read_template_entries(template: bytes):
    """从模板 ph_menu 还原登记清单：名字来自 menu N，路径/指纹来自 STR。"""
    segs = parse_records(template)
    names = decode_names(split_menu(get_seg(segs, MENU_BASE))['N'])
    strbody = get_seg(segs, STR_BASE)
    n = len(strbody) // (PATH_STRIDE + 8)
    if len(strbody) != n * (PATH_STRIDE + 8) or n != len(names):
        raise ValueError('模板 STR 与 menu 名字数不一致')
    entries = []
    for i in range(n):
        path = strbody[i * PATH_STRIDE:(i + 1) * PATH_STRIDE].split(b'\0')[0].decode('ascii')
        fp1, fp5 = struct.unpack_from('<2I', strbody, n * PATH_STRIDE + i * 8)
        entries.append((names[i], path, fp1, fp5))
    return entries


def rebuild_ph_menu(template: bytes, entries, preload: int | None = None,
                    canonical: bool = False, magic: int | None = None) -> bytes:
    """按新清单重组 ph_menu：只替换 menu.text / core.text / STR，其余段逐字节保留。
    entries: (name, card_path, fp1, fp5)。preload 缺省沿用模板（越界则取 0）。"""
    segs = parse_records(template)
    entries = list(entries)
    n = len(entries)
    if not 1 <= n <= MAX_SLOTS:
        raise ValueError(f'档位数 {n} 超出 1..{MAX_SLOTS}')
    names = [e[0] for e in entries]
    if len({e[2] for e in entries}) != n:
        raise ValueError('slot1 指纹有重复，无法区分滤镜')
    tmpl_core = get_seg(segs, CORE_BASE)
    if preload is None:
        old_preload = read_preload(tmpl_core)
        preload = old_preload if old_preload < n else 0
    menu_new = rebuild_menu_text(get_seg(segs, MENU_BASE), names, canonical=canonical)
    core_new = patch_core(tmpl_core, n, preload)
    str_new = build_STR(entries)
    replace = {MENU_BASE: menu_new, CORE_BASE: core_new, STR_BASE: str_new}
    out = []
    for addr, payload in segs:
        if addr in replace:
            payload = replace.pop(addr)
        if addr == 0x08EF61F0 and magic is not None:   # LOG_HDR magic 字
            payload = payload[:12] + struct.pack('<I', magic) + payload[16:]
        out.append((addr, payload))
    if replace:
        raise ValueError(f'模板缺段：{[hex(a) for a in replace]}')
    # 段重叠检查
    occupied = []
    for addr, payload in out:
        for s, e in occupied:
            if addr < e and s < addr + len(payload):
                raise ValueError(f'段重叠 {addr:#010x} 与 {s:#010x}')
        occupied.append((addr, addr + len(payload)))
    return build_records(out)


# ================= 包指纹提取 + 输入校验 =================
def extract_fingerprints(pkg_path) -> tuple[int, int]:
    """从 87,720B 五槽包提取 (slot1_fp, slot5_fp)，与 重登记指纹.py 同逻辑。"""
    data = Path(pkg_path).read_bytes()
    if len(data) != PKG_SIZE:
        raise ValueError(f'包大小 {len(data)}B ≠ {PKG_SIZE}B（不是五槽包）')
    slots, pos = [], 0
    while pos < len(data):
        addr, size = struct.unpack_from('<II', data, pos)
        pos += 8
        if size != SLOT_SIZE:
            raise ValueError(f'槽表体 {size}B ≠ {SLOT_SIZE}B')
        slots.append((addr, data[pos:pos + size]))
        pos += size
    if len(slots) != SLOT_COUNT:
        raise ValueError(f'槽数 {len(slots)} ≠ {SLOT_COUNT}')
    addrs = [a for a, _ in slots]
    if addrs != SLOT_ADDRS:
        raise ValueError(f'槽地址与标准不符：{[hex(a) for a in addrs]}')
    fp1 = struct.unpack_from('<I', slots[1][1], FP_OFFSET)[0]
    fp5 = struct.unpack_from('<I', slots[4][1], FP_OFFSET)[0]
    return fp1, fp5


def validate_name(name: str) -> str:
    if not name:
        raise ValueError('名字不能为空')
    if len(name) > MAX_NAME_LEN:
        raise ValueError(f'名字 {name!r} 超过 {MAX_NAME_LEN} 字符（箭头间最多 6 字）')
    bad = sorted(set(name) - CHARSET)
    if bad:
        raise ValueError(f'名字 {name!r} 含字模缺字 {bad}；可用：{"".join(sorted(CHARSET))}')
    return name


def validate_card_path(path: str) -> str:
    b = path.encode('ascii')
    if len(b) >= PATH_STRIDE:
        raise ValueError(f'卡路径 {path!r} ≥ {PATH_STRIDE}B')
    if not path.startswith(CARD_PREFIX):
        raise ValueError(f'卡路径须以 {CARD_PREFIX!r} 开头：{path!r}')
    return path


# ================= 静态分发校验 =================
def verify_dispatch(menu: bytes, names) -> dict:
    """解码 menu.text，逐 idx 核对分发链→名字块→名字串映射；不执行代码。
    返回 {idx: 名字}。任何越界/错配/opcode 不符都抛错。"""
    names = list(names)
    n = len(names)
    parts = split_menu(menu)
    P, V = parts['P'], parts['V']
    v_start, f1_start = parts['v_start'], parts['f1_start']
    pw = struct.unpack_from(f'<{len(P)//4}I', P, 0)
    assert pw[P_CYC_UP_CMP // 4] == enc_cmp(0, n - 1), '循环上限(up)不符'
    assert pw[P_CYC_WRAP_CMP // 4] == enc_cmp(0, n), '循环回绕不符'
    assert pw[P_CYC_DN_CMP // 4] == enc_cmp(0, n - 1), '循环上限(dn)不符'
    assert pw[P_CYC_DN_MOV // 4] == enc_mov(0, n - 1), '循环回绕(dn)不符'
    assert pw[P_DISP_CMP // 4] == enc_cmp(6, n - 1), '分发上限不符'
    assert dec_branch(pw[P_DISP_BHI // 4], P_DISP_BHI) == f1_start + F1_UNKNOWN_OFF, 'bhi 目标不符'
    # 进页两条无条件分支必须落在 F1+0x68 状态设置入口；V 移位后若未重定位会跳进指令中间→实机进页即崩。
    for off, tag in ((P_F1_SETUP_A, 'A'), (P_F1_SETUP_B, 'B')):
        assert pw[off // 4] & 0x0F000000 == 0x0A000000, f'进页分支{tag} 非分支指令'
        assert dec_branch(pw[off // 4], off) == f1_start + F1_SETUP_OFF, f'进页分支{tag}目标不符'
    # F1 跨区引用：固件 bl 须保持绝对目标（fw_clear/fw_area），WAIT/index/font adr 须落在新区。
    # 漏补 fw_area 重定位→menu_draw_flush 跳进固件函数中间→进页即崩（active 路径不调 fw_area 故无感）。
    F1 = parts['F1']
    for off, fw, tag in ((F1_BL_CLEAR, FW_CLEAR_ADDR, 'fw_clear'),
                         (F1_BL_AREA, FW_AREA_ADDR, 'fw_area')):
        w = struct.unpack_from('<I', F1, off)[0]
        assert w & 0x0E000000 == 0x0A000000 and w & 0x01000000, f'F1+{off:#x} 非 bl'
        assert MENU_BASE + dec_branch(w, f1_start + off) == fw, f'F1 bl {tag} 目标不符（进页崩溃源）'
    w = struct.unpack_from('<I', F1, F1_ADR_WAIT)[0]
    assert w & 0xFFFFF000 == 0xE28F0000, 'F1 WAIT 非 adr r0'
    wt = f1_start + F1_ADR_WAIT + 8 + dec_imm(w)
    assert parts['n_start'] <= wt < parts['f2_start'] and menu[wt:wt + 5] == b'WAIT\0', 'F1 WAIT adr 目标不符'
    for off, dst, tag in ((F1_ADR_INDEX, parts['f2_start'], 'index'),
                          (F1_ADR_FONT, parts['f2_start'] + F2_FONT_OFF, 'font')):
        w = struct.unpack_from('<I', F1, off)[0]
        assert w & 0xFFFFF000 == 0xE28F9000, f'F1 {tag} 非 adr r9'
        assert f1_start + off + 8 + dec_imm(w) == dst, f'F1 {tag} adr 目标不符'
    vw = struct.unpack_from(f'<{len(V)//4}I', V, 0)
    dispatch, i = {}, 0
    while i < len(vw) and (vw[i] & 0xFFFFF000) == 0xE3560000:
        idx = vw[i] & 0xFFF
        beq = vw[i + 1]
        assert beq & 0xFF000000 == 0x0A000000, f'idx{idx} 非 beq'
        dispatch[idx] = dec_branch(beq, v_start + (i + 1) * 4)
        i += 2
    default_block = v_start + i * 4
    missing = set(range(n)) - set(dispatch)
    assert len(missing) == 1, f'默认项缺失或多于一个：{missing}'
    default_idx = missing.pop()
    blocks, off = {}, default_block
    while off < f1_start:
        adr, mov = struct.unpack_from('<2I', menu, off)
        assert adr & 0xFFFFF000 == 0xE28F0000, f'名字块 {off:#x} 非 adr r0'
        tgt = off + 8 + dec_imm(adr)
        assert parts['n_start'] <= tgt < parts['f2_start'], f'adr 目标 {tgt:#x} 不在 N 区'
        nm = menu[tgt:menu.index(b'\0', tgt)].decode('ascii')
        assert mov & 0xFFFFF000 == 0xE3A01000, f'名字块 {off:#x} 非 mov r1,#imm'
        remaining = f1_start - off
        if remaining == 8:                       # 末块直落 emit（无 b，汇编优化）
            pass
        elif remaining >= 12:
            b = struct.unpack_from('<I', menu, off + 8)[0]
            assert dec_branch(b, off + 8) == f1_start, f'名字块 {off:#x} b 目标非 emit'
        else:
            raise AssertionError(f'名字块 {off:#x} 长度异常 remaining={remaining}')
        blocks[off] = nm
        off += 8 if remaining == 8 else 12
    assert off == f1_start, '名字块未填满 V 区'
    mapping = {}
    for idx in range(n):
        blk = dispatch.get(idx, default_block)
        assert blk in blocks, f'idx{idx} 指向无效块 {blk:#x}'
        assert blocks[blk] == names[idx], f'idx{idx} -> {blocks[blk]!r} != {names[idx]!r}'
        mapping[idx] = blocks[blk]
    return mapping


# ================= 自检：逐字节回归 =================
def _selftest():
    root = Path(__file__).resolve().parent.parent
    ok = True
    for tag, rel in (('69', '自定义置入滤镜流程/testdata/ph_menu_69.bin'),
                     ('71', '实验/71_修复INSTA分发/ph_menu.bin'),
                     ('72', 'Lightning-L口/ph_menu.bin')):
        path = root / rel
        if not path.exists():
            print(f'  [跳过] Build {tag}: 缺 {rel}')
            continue
        segs = parse_records(path.read_bytes())
        template_bytes = path.read_bytes()
        menu = get_seg(segs, MENU_BASE)
        parts = split_menu(menu)
        names = decode_names(parts['N'])
        n = len(names)
        # 1) no-op 逐字节回归
        rebuilt = rebuild_menu_text(menu, names)
        same = rebuilt == menu
        ok &= same
        print(f'  Build {tag} (n={n}): no-op 逐字节回归 {"一致 ✓" if same else "不一致 ✗"}')
        # 2) 模板自身静态分发校验
        try:
            verify_dispatch(menu, names)
            print(f'    模板静态分发校验 ✓')
        except AssertionError as e:
            ok = False
            print(f'    模板静态分发校验 ✗ {e}')
        # 3) 规范布局重生成：F2 逐字节一致；F1 仅跨区引用(fw_clear/fw_area/WAIT/index/font)随平移重定位；
        #    P 仅三条进页分支(bhi+68c+6a0)随 V 长度变；N 名字等价
        canon = rebuild_menu_text(menu, names, canonical=True)
        cp = split_menu(canon)
        def mask_p_branches(P):
            m = bytearray(P)
            for off in (P_DISP_BHI, P_F1_SETUP_A, P_F1_SETUP_B):
                m[off:off + 4] = b'\0' * 4
            return bytes(m)
        def mask_f1_refs(F1):
            m = bytearray(F1)
            for off in (F1_BL_CLEAR, F1_ADR_WAIT, F1_BL_AREA, F1_ADR_INDEX, F1_ADR_FONT):
                m[off:off + 4] = b'\0' * 4
            return bytes(m)
        fixed_same = (mask_f1_refs(cp['F1']) == mask_f1_refs(parts['F1']) and cp['F2'] == parts['F2']
                      and mask_p_branches(parts['P']) == mask_p_branches(cp['P'])
                      and decode_names(cp['N']) == names)
        ok &= fixed_same
        try:
            verify_dispatch(canon, names)
            vok = True
        except AssertionError as e:
            vok = False
            ok = False
            print(f'    规范布局分发校验 ✗ {e}')
        print(f'    规范布局：F1(除跨区引用)/F2 逐字节一致+P(除三条进页分支)+N名字等价 {"✓" if fixed_same else "✗"}，'
              f'分发校验 {"✓" if vok else "✗"}，V {len(parts["V"])}B->{len(cp["V"])}B')
        # 4) 增删场景
        if n >= 2:
            deleted = names[:-1]                      # 删末档
            m = rebuild_menu_text(menu, deleted, canonical=True)
            try:
                verify_dispatch(m, deleted)
                print(f'    删末档 -> n={len(deleted)} 分发校验 ✓ ({len(m)}B)')
            except AssertionError as e:
                ok = False
                print(f'    删末档分发校验 ✗ {e}')
        if n < MAX_SLOTS:
            added = names + ['TEST']                  # 增一档（TEST 全在字符集内）
            m = rebuild_menu_text(menu, added, canonical=True)
            try:
                verify_dispatch(m, added)
                print(f'    增一档 -> n={len(added)} 分发校验 ✓ ({len(m)}B)')
            except AssertionError as e:
                ok = False
                print(f'    增一档分发校验 ✗ {e}')
        # 5) ph_menu 全量 no-op 逐字节回归（含 core/STR/其余段）
        entries = read_template_entries(template_bytes)
        ph_noop = rebuild_ph_menu(template_bytes, entries)
        same_ph = ph_noop == template_bytes
        ok &= same_ph
        print(f'    ph_menu no-op 逐字节回归 {"一致 ✓" if same_ph else "不一致 ✗"} ({len(template_bytes)}B)')
        if not same_ph:
            for i in range(min(len(ph_noop), len(template_bytes))):
                if ph_noop[i] != template_bytes[i]:
                    print(f'      首差异 offset {i:#x}: {template_bytes[i]:02x} != {ph_noop[i]:02x}')
                    break
        # 6) ph_menu 删末档 / 增一档：menu 分发 + core 立即数 + STR 一致
        def check_ph(ph, ents, label):
            nonlocal ok
            try:
                ss = parse_records(ph)
                nn = len(ents)
                verify_dispatch(get_seg(ss, MENU_BASE), [e[0] for e in ents])
                pl = read_preload(get_seg(ss, CORE_BASE))
                verify_core(get_seg(ss, CORE_BASE), nn, pl)
                assert get_seg(ss, STR_BASE) == build_STR(ents), 'STR 不符'
                print(f'    {label} -> n={nn} menu+core+STR 校验 ✓ ({len(ph)}B)')
            except AssertionError as e:
                ok = False
                print(f'    {label} 校验 ✗ {e}')
        if n >= 2:
            check_ph(rebuild_ph_menu(template_bytes, entries[:-1], canonical=True),
                     entries[:-1], 'ph_menu 删末档')
        if n < MAX_SLOTS:
            fp_new = 0xABCD0000 + n
            ent_add = entries + [('TEST', f'c:\\DXO_FILTERS\\lut_test{n}.bin', fp_new, fp_new)]
            check_ph(rebuild_ph_menu(template_bytes, ent_add, canonical=True),
                     ent_add, 'ph_menu 增一档')
    print('自检', '通过' if ok else '失败')
    return ok


# ================= 命令行：list / add / delete / rebuild =================
def _load_template(path):
    p = Path(path)
    if not p.is_file():
        raise SystemExit(f'错误：找不到模板 {p}')
    data = p.read_bytes()
    return data, read_template_entries(data)


def _safe_output(input_path, output):
    """输出只落在工作目录；拒绝原地覆盖与写可移动卷（相机卡安全闸）。"""
    src = Path(input_path).resolve()
    out = (Path(output) if output else Path.cwd() / (Path(input_path).stem + '.new.bin')).resolve()
    if out == src:
        raise SystemExit('错误：拒绝原地覆盖输入文件，请用 -o 指定别的输出路径')
    if str(out).startswith('/Volumes/'):
        raise SystemExit('错误：拒绝写入 /Volumes/（相机卡安全闸）。本工具只在工作目录生成文件，写卡须你另行授权。')
    return out


def _resolve_magic(args):
    if getattr(args, 'magic', None):
        return int(args.magic, 16)
    if getattr(args, 'custom_magic', False):
        return CUSTOM_MAGIC
    return None


def _emit(args, template, entries, action):
    """统一出口：校验 → 重组 ph_menu → 静态分发+core 复核 → 写文件。"""
    n = len(entries)
    names = [e[0] for e in entries]
    if len({e[2] for e in entries}) != n:
        raise SystemExit('错误：slot1 指纹有重复，无法区分滤镜')
    magic = _resolve_magic(args)
    # canonical=False：清单与模板一致时逐字节复用（no-op 不改已验证产物），
    # 清单变化时仍走 gen_V 重生成（与 unicorn 真执行门验证过的布局一致）。
    ph = rebuild_ph_menu(template, entries, preload=args.preload, canonical=False, magic=magic)
    segs = parse_records(ph)
    verify_dispatch(get_seg(segs, MENU_BASE), names)
    pl = read_preload(get_seg(segs, CORE_BASE))
    verify_core(get_seg(segs, CORE_BASE), n, pl)
    if get_seg(segs, STR_BASE) != build_STR(entries):
        raise SystemExit('错误：STR 复核不符')
    out = _safe_output(args.ph_menu, args.output)
    out.write_bytes(ph)
    beacon = f'0x{magic:08x}' if magic is not None else '沿用模板'
    print(f'{action}：{n} 档，开机预装 idx={pl}，LOG 信标 {beacon}')
    print(f'已写出 {out}（{len(ph)}B）；静态分发 + core 立即数 + STR 复核通过。')
    print('下一步（须你亲自授权）：备份卡上原 ph_menu.bin，把本文件拷贝为卡根 ph_menu.bin，')
    print('逐字节回读校验后正常弹出卷。其余档位包文件须与本 ph_menu 同套使用。')


def cmd_list(args):
    data, entries = _load_template(args.ph_menu)
    core = get_seg(parse_records(data), CORE_BASE)
    preload = read_preload(core)
    print(f'模板 {args.ph_menu}：{len(entries)} 档（上限 {MAX_SLOTS}），开机预装 idx={preload}')
    print(f'{"idx":>3}  {"名字":8s} {"卡路径":30s} 指纹(槽2/槽5)')
    for i, (nm, path, fp1, fp5) in enumerate(entries):
        mark = ' ←预装' if i == preload else ''
        print(f'{i:>3}  {nm:8s} {path:30s} 0x{fp1:08x}/0x{fp5:08x}{mark}')
    return 0


def cmd_add(args):
    data, entries = _load_template(args.ph_menu)
    name = validate_name(args.name)
    if any(e[0] == name for e in entries):
        raise SystemExit(f'错误：名字 {name!r} 已存在，菜单名字须唯一')
    pkg = Path(args.pkg)
    if not pkg.is_file():
        raise SystemExit(f'错误：找不到包文件 {pkg}')
    fp1, fp5 = extract_fingerprints(pkg)
    if any(e[2] == fp1 for e in entries):
        raise SystemExit(f'错误：新包 slot1 指纹 0x{fp1:08x} 与现有档重复')
    if len(entries) >= MAX_SLOTS:
        raise SystemExit(f'错误：已达上限 {MAX_SLOTS} 档，无法新增')
    card_path = validate_card_path(CARD_PREFIX + pkg.name)
    pos = args.position if args.position is not None else len(entries)
    if not 0 <= pos <= len(entries):
        raise SystemExit(f'错误：插入位置 {pos} 越界（0..{len(entries)}）')
    new = entries[:pos] + [(name, card_path, fp1, fp5)] + entries[pos:]
    _emit(args, data, new, f'新增 {name} @ idx{pos}')
    return 0


def cmd_delete(args):
    data, entries = _load_template(args.ph_menu)
    n = len(entries)
    idxs = set(args.idx or [])
    for nm in (args.name or []):
        matches = [i for i, e in enumerate(entries) if e[0] == nm]
        if not matches:
            raise SystemExit(f'错误：找不到名字 {nm!r}')
        if len(matches) > 1:
            raise SystemExit(f'错误：名字 {nm!r} 对应多档，请改用 --idx')
        idxs.add(matches[0])
    if not idxs:
        raise SystemExit('错误：未指定要删除的档（--idx 或 --name，可多个）')
    for i in idxs:
        if not 0 <= i < n:
            raise SystemExit(f'错误：idx {i} 越界（0..{n-1}）')
    if len(idxs) >= n:
        raise SystemExit('错误：不能删空，至少保留 1 档')
    keep = [e for i, e in enumerate(entries) if i not in idxs]
    removed = ', '.join(f'idx{i}={entries[i][0]}' for i in sorted(idxs))
    _emit(args, data, keep, f'删除 {removed}（菜单同步收缩到 {len(keep)} 档）')
    return 0


def cmd_rebuild(args):
    data, _ = _load_template(args.ph_menu)
    manifest = json.loads(Path(args.manifest).read_text('utf-8'))
    pkg_dir = Path(args.pkg_dir)
    filters = manifest.get('filters')
    if not filters or not 1 <= len(filters) <= MAX_SLOTS:
        raise SystemExit(f'错误：清单档数须 1..{MAX_SLOTS}')
    entries, seen_fp, seen_name = [], set(), set()
    for f in filters:
        name = validate_name(f['name'])
        if name in seen_name:
            raise SystemExit(f'错误：名字重复 {name!r}')
        seen_name.add(name)
        pkg = pkg_dir / f['pkg']
        fp1, fp5 = extract_fingerprints(pkg)
        if fp1 in seen_fp:
            raise SystemExit(f'错误：slot1 指纹重复 0x{fp1:08x}（{name}）')
        seen_fp.add(fp1)
        card_path = validate_card_path(CARD_PREFIX + Path(f['pkg']).name)
        entries.append((name, card_path, fp1, fp5))
    if args.preload is None and manifest.get('preload') is not None:
        args.preload = manifest['preload']
    _emit(args, data, entries, f'按清单重建 {len(entries)} 档')
    return 0


def _build_argparser():
    ap = argparse.ArgumentParser(
        prog='管理滤镜.py',
        description='DXO ONE 滤镜菜单 增/删/改清单 模板补丁构建器（离线，不写卡）。无参数运行=自检。')
    sub = ap.add_subparsers(dest='cmd', required=True)

    def common(p):
        p.add_argument('ph_menu', help='模板 ph_menu.bin（仓库 Lightning-L口/ 或卡上只读拷贝）')
        p.add_argument('-o', '--output', help='输出路径（默认 工作目录/<输入名>.new.bin）')
        p.add_argument('--preload', type=int, help='开机预装档 idx（缺省沿用模板，越界取 0）')
        p.add_argument('--magic', help='LOG_HDR 信标 hex（如 0x50473543）；缺省沿用模板')
        p.add_argument('--custom-magic', action='store_true',
                       help='用自定义信标 C5GP(0x50473543) 标记本构建，便于实机区分')

    p = sub.add_parser('list', help='列出模板登记清单')
    p.add_argument('ph_menu')
    p.set_defaults(func=cmd_list)

    p = sub.add_parser('add', help='新增一档（菜单同步扩张）')
    common(p)
    p.add_argument('--name', required=True, help='菜单名（≤6 字符，限字模字符集）')
    p.add_argument('--pkg', required=True, help='新滤镜五槽包 lut_*.bin 路径')
    p.add_argument('--position', type=int, help='插入位置 idx（缺省追加到末尾）')
    p.set_defaults(func=cmd_add)

    p = sub.add_parser('delete', help='删除一或多档（菜单同步收缩、重新索引）')
    common(p)
    p.add_argument('--idx', type=int, action='append', help='按 idx 删除（可重复）')
    p.add_argument('--name', action='append', help='按名字删除（可重复）')
    p.set_defaults(func=cmd_delete)

    p = sub.add_parser('rebuild', help='按 JSON 清单全量重建')
    common(p)
    p.add_argument('--manifest', required=True, help='JSON 清单：{filters:[{name,pkg}],preload?}')
    p.add_argument('--pkg-dir', required=True, help='五槽包目录（含清单引用的 lut_*.bin）')
    p.set_defaults(func=cmd_rebuild)

    p = sub.add_parser('selftest', help='逐字节回归自检')
    p.set_defaults(func=lambda a: 0 if _selftest() else 1)
    return ap


def main(argv=None):
    argv = sys.argv[1:] if argv is None else list(argv)
    if not argv:
        return 0 if _selftest() else 1
    args = _build_argparser().parse_args(argv)
    try:
        return args.func(args)
    except (ValueError, KeyError) as e:
        print(f'错误：{e}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())