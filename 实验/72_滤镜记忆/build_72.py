#!/usr/bin/env python3
"""实验72：滤镜记忆。在69复合定稿基础上只改core.S：
切换成功即把{magic,idx}8B写到c:\\ph_last.bin（best-effort）；开机bootstrap
先读回上次idx校验后走统一filter_select恢复，缺失/损坏/越界回退预装档。
菜单/数据与69逐字节一致；新增SAVE_STAT诊断段与core尾部字符串段。
只构建本地，不访问存储卡。
"""
from pathlib import Path
import hashlib
import json
import struct
import subprocess
import tempfile

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
FWBASE = 0x08C00000
MENU_BASE, MENU_LIMIT = 0x08EF2800, 0x08EF3800
MENU_STATE, MENU_STATE_LIMIT = 0x08EF6000, 0x08EF6100
CORE_BASE, CORE_LIMIT = 0x08EF3800, 0x08EF5000
CORE_STATE = 0x08EF6100
LOG_HDR, LOG_BASE, LOG_SLOTS = 0x08EF61F0, 0x08EF6200, 32
STR_BASE, EXP_BASE = 0x08EF6400, 0x08EF6540
PATH_STRIDE = 32
FILTER_DIR = 'DXO_FILTERS'
SLOT1, SLOT5 = 0x092DB768, 0x092E84E8
SLOT_SIZE, SLOT_COUNT = 17536, 5
DIAG_DIR = 'PH_DIAG'   # 诊断信标目录，避免根目录堆积
LOG_MAGIC = 0x50473547  # 72独立magic "G5GP"（69=D5GP/70=E5GP/71=F5GP）
SAVE_STAT = 0x08EF6190  # +0 save_ok +4 save_fail +8 restore_src +12 restore_result +16 read_diag
LAST_STRINGS = 0x08EF4F00  # core区尾部：路径16B + "w"\0 + "r"\0
LAST_MAGIC = 0x534C4850    # "PHLS" ph_last.bin首字
LAST_FILE = 'c:\\ph_last.bin'
# (显示名, 卡上路径, 本地五槽包名, 原始单槽文件 or None, 既有五槽包 or None)
FILTERS = (
    ('ORIGINAL', 'c:\\DXO_FILTERS\\lut_orig.bin', 'DXO_FILTERS/lut_orig.bin',
     None, '实验/41_阶段4b_OLED请求状态/lut_orig.bin'),
    ('VELVIA', 'c:\\DXO_FILTERS\\lut_vel.bin', 'DXO_FILTERS/lut_vel.bin',
     None, '实验/55_第三方全直出/lut_vel.bin'),
    ('GOLD200', 'c:\\DXO_FILTERS\\lut_gold.bin', 'DXO_FILTERS/lut_gold.bin',
     None, '实验/55_第三方全直出/lut_gold.bin'),
    ('PORTRA', 'c:\\DXO_FILTERS\\lut_port.bin', 'DXO_FILTERS/lut_port.bin',
     None, '实验/55_第三方全直出/lut_port.bin'),
    ('HP5', 'c:\\DXO_FILTERS\\lut_hp5.bin', 'DXO_FILTERS/lut_hp5.bin',
     None, '实验/55_第三方全直出/lut_hp5.bin'),
    ('RICOH POSI', 'c:\\DXO_FILTERS\\lut_posi.bin', 'DXO_FILTERS/lut_posi.bin',
     None, '实验/55_第三方全直出/lut_posi.bin'),
    ('RICOH HIBW', 'c:\\DXO_FILTERS\\lut_hibw.bin', 'DXO_FILTERS/lut_hibw.bin',
     None, '实验/55_第三方全直出/lut_hibw.bin'),
    ('FUYUNO', 'c:\\DXO_FILTERS\\lut_fuyu.bin', 'DXO_FILTERS/lut_fuyu.bin',
     None, '实验/55_第三方全直出/lut_fuyu.bin'),
    ('KYUSHU', 'c:\\DXO_FILTERS\\lut_kyus.bin', 'DXO_FILTERS/lut_kyus.bin',
     None, '实验/55_第三方全直出/lut_kyus.bin'),
    ('SATOBI', 'c:\\DXO_FILTERS\\lut_sato.bin', 'DXO_FILTERS/lut_sato.bin',
     None, '实验/55_第三方全直出/lut_sato.bin'),
)
PRELOAD = 2  # 开机预装 GOLD200：新加的柯达表零交互即可看效果

EXTERNAL = {
    'fw_page_resume': 0x08EE50A4,
    'fw_hz_group': 0x08E11F98,
    'fw_group_resume': 0x08EE514C,
    'fw_hz_early_return': 0x08E122E4,
    'fw_shooting_mode': 0x08E10C80,
    'fw_touch_resume': 0x08EE523C,
    'fw_touch_consumed': 0x08EE55D4,
    'fw_touch_original': 0x08EE55DC,
    'fw_mode3_resume': 0x08EE54E8,
    'fw_fresh_resume': 0x08EC9E18,
    'fw_area': 0x08EE9D04,
    'fw_clear': 0x08ECA0BC,
    'fw_text': 0x08ECD9AC,
    'fw_page_apply': 0x08EE5840,
    'fw_page_draw': 0x08EE56C0,
    'filter_select': CORE_BASE,
    'fw_loader': 0x08EE7478,
    'fw_oled_set_mode': 0x08EE195C,
    'fw_s1_ready': 0x08F19CF4,
    'fw_s1_resume': 0x08E069D4,
    'fw_home_resume': 0x08ECF120,
    'fw_vf_resume': 0x08E11C8C,
    'fw_vf_request_resume': 0x08E11C30,
    'fw_animation_resume': 0x08DFF398,
    'fw_oled_get_mode': 0x08EE1968,
    'fw_native_renderer_resume': 0x08EE9A58,
    'fw_area_gate_resume': 0x08EE9CA8,
    'fw_native_arrows': 0x08EDB46C,
    'fw_native_chrome': 0x08EE9E4C,
    # 72：固件文件API（反汇编+ambsh_savebin调用点确证）
    'fw_fopen': 0x08E65974,    # (path, mode) -> fp
    'fw_fread': 0x08E65D78,    # 七参(buf,0,size=1,0,栈count_lo,栈count_hi,栈fp)->n
    'fw_fwrite': 0x08E65DEC,   # 七参同上；fp在栈arg7[sp+8]，非寄存器r2
    'fw_fclose': 0x08E65AC0,   # (fp) -> 0
}
HOOKS = {
    0x08EE50A0: ('menu_page_group', 'ad0d06e3'),
    0x08EE5148: ('menu_group_dispatch', '020050e3'),
    0x08EE5238: ('menu_touch', '000059e3'),
    0x08EE54E4: ('menu_mode3', '920100e3'),
    0x08EE9C9C: ('menu_fresh', '10402de9'),
    0x08E069D0: ('menu_half_press', 'c74c04eb'),
    0x08ECF11C: ('menu_home_commit', '0e4a00eb'),
    0x08E11C88: ('menu_vf_commit', '0200c6e5'),
    0x08E11C2C: ('menu_vf_request', 'ea0fc7e5'),
    0x08DFF394: ('menu_animation_commit', 'ea0fc8e5'),
    0x08EE9A54: ('menu_native_renderer', 'ad0d06e3'),
    0x08EE9CA4: ('menu_area_gate', '0f502de9'),
}
MOD6 = {
    0x08E11FD0: ('050080e2', '060080e2'),
    0x08E11FD4: ('050050e3', '060050e3'),
    0x08E11FDC: ('05004022', '06004022'),
    0x08E11FE0: ('050050e3', '060050e3'),
    0x08E11FE4: ('05004022', '06004022'),
}
RESTORE_9774 = (0x08EE9774, bytes.fromhex('10402de9769dfceb'))
KEEP_WORDS = {
    0x08E11FD8: 'f8979fe5',
    0x08E11FEC: '4b4c03ea',
    0x08EE9774: '10402de9',
    0x08EE9778: '769dfceb',
    0x08EE9D04: 'f04d2de9',
    0x08EE9A54: 'ad0d06e3',
}
MENU_SECTIONS = {'.text': MENU_BASE, '.menu_state': MENU_STATE}
CORE_SECTIONS = {'.text': CORE_BASE}


def branch(src, dst):
    delta = dst - src - 8
    if delta % 4 or not -(1 << 25) <= delta < (1 << 25):
        raise ValueError(f'ARM 跳转越界：{src:08x} -> {dst:08x}')
    return struct.pack('<I', 0xEA000000 | ((delta >> 2) & 0xFFFFFF))


def parse_records(data):
    pos, result = 0, []
    while pos < len(data):
        if pos + 8 > len(data):
            raise ValueError('记录头截断')
        address, size = struct.unpack_from('<II', data, pos)
        if not size or pos + 8 + size > len(data):
            raise ValueError('记录载荷截断或为空')
        result.append((address, data[pos + 8:pos + 8 + size]))
        pos += 8 + size
    return result


def baseline():
    path = ROOT / '固件镜像/sections/section_0.bin'
    image = bytearray(path.read_bytes())
    sources = {str(path.relative_to(ROOT)): hashlib.sha256(image).hexdigest()}
    for name in ('a.bin', 'b.bin'):
        path = ROOT / '成品/触屏加LUT/dxot' / name
        data = path.read_bytes()
        sources[str(path.relative_to(ROOT))] = hashlib.sha256(data).hexdigest()
        for address, payload in parse_records(data):
            offset = address - FWBASE
            if not 0 <= offset <= len(image) - len(payload):
                raise ValueError(f'基线段越界：{address:08x}')
            image[offset:offset + len(payload)] = payload
    return image, sources


def link_object(data, section_bases, limits, externals):
    header = struct.unpack_from('<16sHHIIIIIHHHHHH', data)
    if header[0][:7] != b'\x7fELF\x01\x01\x01' or header[1:3] != (1, 40):
        raise ValueError('需要 ARM ELF32 可重定位对象')
    sections = [struct.unpack_from('<IIIIIIIIII', data, header[6] + i * header[11])
                for i in range(header[12])]
    name_table = sections[header[13]]
    names = data[name_table[4]:name_table[4] + name_table[5]]
    by_name = {names[s[0]:].split(b'\0')[0].decode(): (i, s)
               for i, s in enumerate(sections)}
    buffers, addresses = {}, {}
    for name, address in section_bases.items():
        index, section = by_name[name]
        addresses[index] = address
        buffers[index] = bytearray(data[section[4]:section[4] + section[5]])
    _, table = by_name['.symtab']
    strings_section = sections[table[6]]
    strings = data[strings_section[4]:strings_section[4] + strings_section[5]]
    symbols, resolved = [], {}
    for off in range(table[4], table[4] + table[5], 16):
        name_off, value, size, info, other, index = struct.unpack_from('<IIIBBH', data, off)
        name = strings[name_off:].split(b'\0')[0].decode()
        address = addresses[index] + value if index in addresses else externals.get(name)
        symbols.append((name, address))
        if name and address is not None and not name.startswith('$'):
            resolved[name] = address
    for _, section in by_name.values():
        if section[1] != 9:
            continue
        index = section[7]
        if index not in buffers or section[9] != 8:
            raise ValueError('未知重定位节')
        code, base = buffers[index], addresses[index]
        for off in range(section[4], section[4] + section[5], 8):
            pos, info = struct.unpack_from('<II', data, off)
            kind, sym = info & 255, info >> 8
            name, address = symbols[sym]
            if kind not in (28, 29) or address is None:
                raise ValueError(f'不支持或未解析的重定位：{name} / {kind}')
            word = struct.unpack_from('<I', code, pos)[0]
            if word & 0x0E000000 != 0x0A000000:
                raise ValueError('分支重定位指令不符')
            addend = (word & 0xFFFFFF) << 2
            if addend & (1 << 25):
                addend -= 1 << 26
            delta = address + addend - (base + pos)
            if delta % 4 or not -(1 << 25) <= delta < (1 << 25):
                raise ValueError(f'重定位越界：{name}')
            struct.pack_into('<I', code, pos,
                             (word & 0xFF000000) | ((delta >> 2) & 0xFFFFFF))
    bodies = []
    for index, code in buffers.items():
        address = addresses[index]
        limit = limits[address]
        if address + len(code) > limit:
            raise ValueError(f'段超出分配区：{address:08x} + {len(code):#x} > {limit:08x}')
        bodies.append((address, bytes(code)))
    return sorted(bodies), resolved


def compile_objects():
    with tempfile.TemporaryDirectory(prefix='dxo57-') as temp:
        temp = Path(temp)
        menu_obj, core_obj = temp / 'menu.o', temp / 'core.o'
        for source, obj in ((HERE / 'menu.S', menu_obj), (HERE / 'core.S', core_obj)):
            subprocess.run(['xcrun', 'clang', '--target=armv7-none-eabi', '-c',
                            str(source), '-o', str(obj)], check=True)
        menu_bodies, menu_symbols = link_object(
            menu_obj.read_bytes(), MENU_SECTIONS,
            {MENU_BASE: MENU_LIMIT, MENU_STATE: MENU_STATE_LIMIT}, EXTERNAL)
        core_bodies, core_symbols = link_object(
            core_obj.read_bytes(), CORE_SECTIONS, {CORE_BASE: CORE_LIMIT}, EXTERNAL)
    symbols = {**menu_symbols, **core_symbols}
    if symbols.get('filter_select') != CORE_BASE:
        raise ValueError('filter_select 入口不符')
    return menu_bodies + core_bodies, symbols


def lut_fingerprints():
    expected, sources = [], {}
    frozen = ROOT / '实验/55_第三方全直出'
    freeze = json.loads((frozen / 'run_55.json').read_text())
    assert freeze['passed'] is True, '55冻结报告未通过'
    sources['实验/55_第三方全直出/run_55.json'] = hashlib.sha256((frozen / 'run_55.json').read_bytes()).hexdigest()
    if not (HERE / FILTER_DIR).is_dir():
        (HERE / FILTER_DIR).mkdir()
    for name, _, local, raw, existing in FILTERS:
        assert raw is None, '57只复用冻结五槽包，不进行LUT转换'
        data = (ROOT / existing).read_bytes()
        digest = hashlib.sha256(data).hexdigest()
        if name != 'ORIGINAL':
            assert digest == freeze['outputs_sha256'][Path(existing).name], name
        else:
            baseline_manifest = json.loads((frozen / 'ph_manifest.json').read_text())
            assert digest == baseline_manifest['sources_sha256'][existing]
        sources[existing] = digest
        (HERE / local).write_bytes(data)
        records = parse_records(data)
        if len(records) != SLOT_COUNT or any(len(p) != SLOT_SIZE for _, p in records):
            raise ValueError(f'{name} 不是五槽 0x4480 多段格式')
        if [a for a, _ in records] != [0x092D72E8, 0x092DB768, 0x092DFBE8,
                                       0x092E4068, 0x092E84E8]:
            raise ValueError(f'{name} 槽地址不符')
        fp = (struct.unpack_from('<I', records[1][1], 0x1000)[0],
              struct.unpack_from('<I', records[4][1], 0x1000)[0])
        expected.append(fp)
    if len({fp[0] for fp in expected}) != len(FILTERS):
        raise ValueError('slot1 指纹有重复，无法区分滤镜')
    return expected, sources


def build_scripts(symbols):
    """57：开机装56菜单、统一核心从DXO_FILTERS预装并采样；保留ph文件名。"""
    samples = 14
    lines = [
        'echo PH-START',
        'savebin c:\\ph_init_state.bin 0x08ef6100 l 0x10',
        'sleep 6',
        'writel 0x08e755ec 0xe12fff1e',
        f'writel 0x97231b4 0x{symbols["filter_bootstrap"]:x}',
        f'alarm {FILTERS[PRELOAD][1]}',
        'writel 0x97231b4 0x92401c0',
        'echo PH-BOOTSTRAP-RETURNED-CHECK-STATE',
        'savebin c:\\ph_boot_state.bin 0x08ef6100 l 0x10',
        'savebin c:\\ph_tick.bin 0x09cc238c l 0x4',
        'savebin c:\\ph_hook_word.bin 0x08ee9c9c l 0x4',
        'savebin c:\\ph_ring.bin 0x08e11fd0 l 0x14',
        'echo PH-READY-ROUND-TRIP',
    ]
    for i in range(samples):
        lines.append('savebin c:\\ph_p%02d.bin 0x095d6dad l 0x4' % i)
        lines.append('savebin c:\\ph_f%02d.bin 0x095d5aa8 l 0xc00' % i)
        lines.append('savebin c:\\ph_m%02d.bin 0x09ba53f4 l 0x8' % i)
        lines.append('savebin c:\\ph_v%02d.bin 0x095d6daa l 0x4' % i)
        if i != samples - 1:
            lines.append('sleep 5')
    lines += [
        'savebin c:\\ph_log_end.bin 0x08ef61f0 l 0x210',
        'savebin c:\\ph_slots_end.bin 0x092d72e8 l 0x15680',
        'savebin c:\\ph_state_end.bin 0x08ef6000 l 0x220',
        'savebin c:\\ph_timing.bin 0x08ef6180 l 0x10',
        'savebin c:\\ph_save_stat.bin 0x08ef6190 l 0x14',
        'savebin c:\\ph_durations.bin 0x08ef6600 l 0x80',
        'echo PH-END',
    ]
    probe = '\n'.join(lines) + '\n'
    # 诊断文件统一写入 PH_DIAG 子目录（目录需预先存在；不写根目录）
    probe = probe.replace('savebin c:\\ph_', f'savebin c:\\{DIAG_DIR}\\ph_')
    autoexec = (
        # rm 已验证可用（del/delete/rename 不支持）：每次开机删掉上次日志，
        # 单次日志约 26~70KB，不会累积；需要保留崩溃现场时改用诊断版（去掉此行）
        'rm c:\\console_debug.txt\n'
        't dxo console 8\n'
        'echo PH-SESSION-START\n'
        'cd c:\\dxot\n'
        'pwd\n'
        'writeb 0x95d6dad 0\n'
        'c:\\dxot\\m01.ash\n'
        'c:\\dxot\\m02.ash\n'
        'writel 0x97231b4 0x8ee7478\n'
        'alarm c:\\dxot\\a.bin\n'
        'writel 0x97231b4 0x8ee7080\n'
        'alarm c:\\touch_prefs.bin 0x8ee986c\n'
        'writel 0x97231b4 0x8ee7478\n'
        'alarm c:\\dxot\\b.bin\n'
        # 菜单补丁跟着 a/b 一起装，不再等 8 秒（用户反馈：滤镜页要等一会儿才出现）
        'writel 0x97231b4 0x8ee7478\n'
        'alarm c:\\ph_menu.bin\n'
        'writel 0x97231b4 0x92401c0\n'
        'echo PH-MENU-LOADED\n'
        'echo PH-SUB-START\n'
        'c:\\ph_probe.ash\n'
        'echo PH-SUB-END\n'
        f'savebin c:\\{DIAG_DIR}\\ph_done.bin 0x08c00000 l 0x4\n'
    )
    for name, text in (('ph_probe.ash', probe), ('autoexec.ash', autoexec)):
        if len(text.encode()) >= 8192:
            raise ValueError(f'{name} 超过 ambsh 8192B 上限')
    return autoexec, probe


def main():
    image, sources = baseline()
    expected, lut_sources = lut_fingerprints()
    sources.update(lut_sources)
    code_bodies, symbols = compile_objects()
    if symbols['core_code_end'] > LAST_STRINGS:
        raise ValueError('core 代码越过字符串段基址，须重排 LAST_STRINGS')
    autoexec, probe = build_scripts(symbols)

    audit = []
    for address, (name, old) in HOOKS.items():
        actual = image[address - FWBASE:address - FWBASE + 4].hex()
        if actual != old:
            raise ValueError(f'拒绝未知 hook 旧字 {address:08x}: {actual} != {old}')
        audit.append({'address': address, 'expected': old, 'symbol': name})
    for address, (old, new) in MOD6.items():
        actual = image[address - FWBASE:address - FWBASE + 4].hex()
        if actual != old:
            raise ValueError(f'拒绝未知模5字 {address:08x}: {actual} != {old}')
        audit.append({'address': address, 'expected': old, 'replacement': new})
    for address, word in KEEP_WORDS.items():
        actual = image[address - FWBASE:address - FWBASE + 4].hex()
        if actual != word:
            raise ValueError(f'关键保留指令不符 {address:08x}: {actual} != {word}')
    if image[RESTORE_9774[0] - FWBASE:RESTORE_9774[0] - FWBASE + 8] != RESTORE_9774[1]:
        raise ValueError('9774 恢复字节与叠加 a/b 后的基线不符')

    segments = [RESTORE_9774]
    segments.extend((address, bytes.fromhex(new)) for address, (_, new) in MOD6.items())
    segments.extend(code_bodies)
    segments.append((CORE_STATE, struct.pack('<4I', 0xffffffff, 0, 0, 0)))
    segments.append((0x08EF6180, bytes(16)))
    segments.append((SAVE_STAT, bytes(20)))
    last_strings = LAST_FILE.encode('ascii') + b'\0'
    assert len(last_strings) <= 16
    last_strings = last_strings.ljust(16, b'\0') + b'w\0r\0'
    segments.append((LAST_STRINGS, last_strings))
    segments.append((0x08EF6600, b'\xff' * 128))
    segments.append((LOG_HDR, struct.pack('<4I', 0, 0, 0, LOG_MAGIC)
                     + bytes(LOG_SLOTS * 16)))
    assert PATH_STRIDE == 32 and EXP_BASE == STR_BASE + len(FILTERS) * PATH_STRIDE
    assert EXP_BASE + len(FILTERS) * 8 == 0x08EF6590 < 0x08EF6600
    strings = bytearray()
    for _, card_path, *_ in FILTERS:
        name = card_path.encode('ascii')
        assert len(name) < PATH_STRIDE and b'\0' not in name, card_path
        strings += name + b'\0' * (PATH_STRIDE - len(name))
    assert len(strings) == EXP_BASE - STR_BASE
    strings += b''.join(struct.pack('<2I', *fp) for fp in expected)
    assert len(strings) == len(FILTERS) * (PATH_STRIDE + 8), len(strings)
    segments.append((STR_BASE, bytes(strings)))
    for address, (name, _) in HOOKS.items():
        segments.append((address, branch(address, symbols[name])))

    occupied = []
    for address, payload in segments:
        for start, end in occupied:
            if address < end and start < address + len(payload):
                raise ValueError(f'段重叠：{address:08x} 与 {start:08x}')
        occupied.append((address, address + len(payload)))

    blob = b''.join(struct.pack('<II', address, len(payload)) + payload
                    for address, payload in segments)
    (HERE / 'ph_menu.bin').write_bytes(blob)
    (HERE / 'autoexec.ash').write_text(autoexec)
    (HERE / 'ph_probe.ash').write_text(probe)

    manifest = {
        'experiment': '72_滤镜记忆',
        'path_layout': {'path_base': hex(STR_BASE), 'stride': PATH_STRIDE,
                        'fingerprint_base': hex(EXP_BASE),
                        'table_end': hex(EXP_BASE + len(FILTERS) * 8),
                        'durations_base': hex(0x08EF6600),
                        'directory': FILTER_DIR, 'registered_bin_only': True,
                        'automatic_cube_scan': False},
        'engine': 'xcrun clang armv7-none-eabi + 自写 ELF 重定位链接',
        'symbols': {k: v for k, v in symbols.items() if not k.startswith('$')},
        'segments': [{'address': a, 'size': len(p),
                      'sha256': hashlib.sha256(p).hexdigest()} for a, p in segments],
        'static_audit': audit,
        'restore_9774': RESTORE_9774[1].hex(),
        'filters': [{'idx': i, 'name': name, 'card_path': path,
                     'source': raw or existing,
                     'slot1_fp': hex(fp[0]), 'slot5_fp': hex(fp[1])}
                    for i, ((name, path, _local, raw, existing), fp)
                    in enumerate(zip(FILTERS, expected))],
        'preload_idx': PRELOAD,
        'text_layout': {'ORIGINAL': {'x': 16, 'y': 21, 'width': 32},
                        'VELVIA': {'x': 21, 'y': 21, 'width': 22},
                        'GOLD200': {'x': 15, 'y': 21, 'width': 33},
                        'PORTRA': {'x': 18, 'y': 21, 'width': 28},
                        'HP5': {'x': 25, 'y': 21, 'width': 14},
                        'RICOH POSI': {'x': 11, 'y': 21, 'width': 41},
                        'RICOH HIBW': {'x': 11, 'y': 21, 'width': 42},
                        'FUYUNO': {'x': 18, 'y': 21, 'width': 27},
                        'KYUSHU': {'x': 18, 'y': 21, 'width': 28},
                        'SATOBI': {'x': 19, 'y': 21, 'width': 25},
                        'screen': [64, 48], 'spacing': 1,
                        'measurement': '真实固件fw_text + OledFixture，新增五档ASCII名'},
        'asset_boundary': {
            'source_input_domain_calibrated': False,
            'source_input_domain': 'K12源声明sRGB，Ricoh源仅TITLE标记Rec709，Dehancer四源编码未验证；相机CC输出与源编码域匹配未经硬件校准',
            'composition': '目标LUT(原厂CC输出)，按16³均匀节点假设进行实验性复合',
            'tone': 'pass', 'strength': 1.0, 'desaturate': False,
            'tone_boundary': '全部九张第三方表pass复合保留源影调；不使用neutral抵消源灰轴，不对HP5/HiBW再次去色；原厂五槽包不变',
            'third_party_count': 9,
            'third_party_parameters': {'tone': 'pass', 'strength': 1.0, 'desaturate': False},
            'conversion_report': '实验/55_第三方全直出/conversion_55.json',
            'frozen_packages': '九张第三方五槽包逐hash复用run_55.json冻结输出；原厂包逐hash复用55登记来源，不重新转换',
            'source_white_boundary': 'KYUSHU源白端为0.94901961，SATOBI为0.97254902；保留源响应，不拉伸到1，非量化后复合白端',
            'claim': '风格候选，不宣称原机色彩或原机影调还原',
        },
        'beacon_isolation': {
            'experiment_magic': hex(LOG_MAGIC),
            'filenames': '沿用ph名称，magic 0x50473547(G5GP)区分72',
            'before_deploy': '必须先归档卡上旧ph_*信标与console_debug.txt，再采集72记录',
            'physical_validation': '旧候选记录不得作为72实机验收证据',
        },
        'log_layout': {'header': hex(LOG_HDR), 'base': hex(LOG_BASE),
                       'slots': LOG_SLOTS, 'record_size': 16, 'magic': hex(LOG_MAGIC)},
        'filter_memory': {
            'last_file': LAST_FILE, 'file_magic': hex(LAST_MAGIC),
            'file_format': '{u32 magic, u32 idx} 8B，切换成功即写（best-effort）',
            'save_stat': hex(SAVE_STAT),
            'save_stat_fields': '+0 save_ok +4 save_fail +8 restore_src(0预装/1记忆) '
                                '+12 restore_result(filter_select返回) +16 read_diag(0成功/1无文件/2短读/3坏magic)',
            'strings_seg': hex(LAST_STRINGS),
            'file_api': {'fopen': '0x08e65974', 'fread': '0x08e65d78',
                         'fwrite': '0x08e65dec', 'fclose': '0x08e65ac0'},
            'file_api_signature': 'fopen(path,mode)->fp; fclose(fp)->0; '
                                  'fread/fwrite七参(buf,0,size=1,0,栈[sp+0]count_lo,'
                                  '栈[sp+4]count_hi,栈[sp+8]fp)->字节数（fp在栈arg7）',
            'restore_policy': 'bootstrap先read_last，magic+idx<n校验后走统一filter_select；'
                              '缺失/损坏/越界回退编译期预装档；恢复同样触发一次save_last',
            'runtime_created_file': 'ph_last.bin 由相机运行时创建，部署清单不含、不预置',
        },
        'sources_sha256': sources,
        'ph_menu_sha256': hashlib.sha256(blob).hexdigest(),
        'scripts_sha256': {'autoexec.ash': hashlib.sha256(autoexec.encode()).hexdigest(),
                           'ph_probe.ash': hashlib.sha256(probe.encode()).hexdigest()},
        'timing': {'debounce_ms': 180, 'block': hex(0x08EF6180), 'durations': hex(0x08EF6600)},
        'lifecycle': {'unknown_label': 'WAIT', 'unknown_vertical_loads': False,
                      'home_success_hook': hex(0x08ECF11C), 'vf_success_hook': hex(0x08E11C88),
                      'vf_animation_request_hook': hex(0x08E11C2C),
                      'animation_completion_hook': hex(0x08DFF394),
                      'private_vf_pending': hex(0x08EF6010),
                      'completion_gate': 'paired private/native pending + logical mode1 + animation active + page95',
                      'fresh_order': 'draw current frame before original fresh body',
                      'page_memory': 'retain95 on HOME/VF/animation completion; S1 still clears',
                      'preview_vertical': 'forward190/191 to native VF exit; no filter_select',
                      'preview_horizontal': 'forward192/193 to original mode3 exposure/dial dispatch; never force HOME',
                      'return_cleanup': 'clear full 64x48 framebuffer before safe native arrows/chrome/name; save and restore batch; true Fresh flush; offscreen no transport',
                      'renderer_hook': hex(0x08EE9A54),
                      'offscreen_area_gate_hook': hex(0x08EE9CA4),
                      'offscreen_gate': hex(0x095D6DE3)},
        'field_51': {'user_passed': ['横滑六页循环', '半按无重复A', '切换响应'],
                     'remaining': ['首次空白', '自动回预览后纵滑切滤镜'],
                     'loader_ms': {'median': 19, 'min': 9, 'max': 1032}, 'not_dsp_ack': True},
        'risks': [
            '离线候选，滤镜记忆的保存/恢复全链路尚须实机验收',
            '切换瞬间断电可能写半截ph_last.bin：magic+范围校验兜底回退预装档',
            '任何写卡都有极小FAT风险；只在切换成功时写8B，频率极低',
            '写卡持FS互斥锁，恰逢存照片可能短暂阻塞UI（与装表读卡同一暴露面）',
            '换滤镜清单后旧idx在范围内则指向新清单对应档，记忆语义以当前清单为准',
            'busy非共同原子锁、同步读卡仍可阻塞UI、DSP应用无回执',
            '菜单/导航/半按行为与69逐字节一致，回归以69实机结论为基线',
        ],
    }
    (HERE / 'ph_manifest.json').write_text(json.dumps(manifest, ensure_ascii=False, indent=2) + '\n')

    deploy = {
        'card_files_to_write': ['autoexec.ash', 'ph_probe.ash', 'ph_menu.bin'] + [f[2] for f in FILTERS],
        'card_files_reused_untouched': ['touch_prefs.bin', 'dxot/a.bin', 'dxot/b.bin',
                                        'dxot/m01.ash', 'dxot/m02.ash'],
        'registered_lut_directory': FILTER_DIR,
        'registered_bin_only': True,
        'automatic_cube_scan': False,
        'old_root_luts_no_longer_referenced': [Path(f[2]).name for f in FILTERS],
        'old_root_luts_policy': '仅列出不再引用的旧根目录文件；本构建不删除、不访问卡',
        'lut_sha256': {f[2]: hashlib.sha256((HERE / f[2]).read_bytes()).hexdigest() for f in FILTERS},
        'card_files_no_longer_referenced': ['pg_menu.bin', 'pg_probe.ash', 'pa_menu.bin', 'pa_probe.ash',
                                            'p9_menu.bin', 'p9_probe.ash', 'pb_menu.bin', 'pb_probe.ash'],
        'do_not_touch': ['DCIM', '所有 p8_*/p9_*/pa_* 信标', 'console_debug.txt'],
        'reuse_sha256': {raw or existing: lut_sources[raw or existing]
                         for _, _, _, raw, existing in FILTERS[:1]},
        'before_deploy': '先归档卡上旧ph_*信标与console_debug.txt；不得覆盖或混作72结果',
        'experiment_magic': hex(LOG_MAGIC),
        'recovery': '回退=恢复69/71整套ph_menu与脚本；删除卡根ph_last.bin即清除记忆回预装档；72不反向覆盖旧基线',
        'card_accessed_by_build': False,
    }
    (HERE / '部署清单.json').write_text(json.dumps(deploy, ensure_ascii=False, indent=2) + '\n')
    print(f'72 构建完成：ph_menu.bin {len(blob)}B / {len(segments)} 段；未写卡。')
    for key in ('menu_touch', 'menu_mode3', 'menu_fresh', 'filter_select', 'core_code_end'):
        print(f'  {key}: 0x{symbols[key]:08x}')
    for i, ((name, path, _l, _r, _s), fp) in enumerate(zip(FILTERS, expected)):
        print('  idx%d %-9s %s  fp=%s/%s' % (i, name, path, hex(fp[0]), hex(fp[1])))


if __name__ == '__main__':
    main()
