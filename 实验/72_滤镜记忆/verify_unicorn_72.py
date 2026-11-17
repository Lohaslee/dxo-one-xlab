#!/usr/bin/env python3
"""Build 72 滤镜记忆 unicorn 真执行门：mock 固件文件API与装载器，
真执行 filter_select / filter_bootstrap，覆盖保存/恢复全路径。

mock 边界：fw_loader 按真实五槽包写内存（指纹校验走真数据）；
fopen/fread/fwrite/fclose 用内存VFS替身，签名与固件反汇编+ambsh_savebin一致
（fopen(path,mode)->fp；fclose(fp)->0；fread/fwrite七参
(buf,0,size=1,0,栈count_lo,栈count_hi,栈fp)->字节数，fp在栈arg7）。
替身按真实七参建模：若 core 退回三参 (buf,count,fp)，fp 取到栈垃圾→写0字节，
save_ok/file 内容用例即失败（调用约定回归由此捕获）。
替身不证明 RTOS/FS锁/真实SD行为，实机验收仍由用户亲自做。不写卡。

用法：python3 verify_unicorn_72.py
"""
import struct
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROOT = HERE.parent.parent
sys.path.insert(0, str(ROOT / '实验/42_滤镜菜单离线拟真/deps'))
sys.path.insert(0, str(ROOT / '自定义置入滤镜流程'))
from unicorn import Uc, UC_ARCH_ARM, UC_MODE_ARM, UC_HOOK_CODE
from unicorn.arm_const import (UC_ARM_REG_R0, UC_ARM_REG_R1, UC_ARM_REG_R2,
                               UC_ARM_REG_SP, UC_ARM_REG_LR, UC_ARM_REG_PC)
import 管理滤镜 as M

PH = HERE / 'ph_menu.bin'
PKG_DIR = ROOT / 'patch/DXO_FILTERS'
CODE_LO, CODE_SIZE = 0x08C00000, 0x003F0000
SCRATCH_LO, SCRATCH_SIZE = 0x08FF0000, 0x20000
STOP = 0x08FF1000
TICK_PTR = 0x09CC238C
CORE_STATE = 0x08EF6100
SAVE_STAT = 0x08EF6190
LOG_HDR = 0x08EF61F0
SLOT1, SLOT5 = 0x092DB768, 0x092E84E8
FW_LOADER = 0x08EE7478
FW_FOPEN, FW_FREAD, FW_FWRITE, FW_FCLOSE = 0x08E65974, 0x08E65D78, 0x08E65DEC, 0x08E65AC0
LAST_PATH = b'c:\\ph_last.bin'
LAST_MAGIC = 0x534C4850
BX_LR = struct.pack('<I', 0xE12FFF1E)
RESULTS = []


def check(name, cond, detail=''):
    RESULTS.append((name, bool(cond)))
    print(f'  {"OK " if cond else "FAIL"} {name}' + (f'  {detail}' if not cond else ''))


class Sim:
    """一次仿真会话：装载 ph_menu 全部段 + mock 文件API/装载器。"""

    def __init__(self, ph: bytes, files: dict | None = None, tick=60000,
                 fopen_fail=False, fwrite_short=False):
        self.mu = Uc(UC_ARCH_ARM, UC_MODE_ARM)
        self.mu.mem_map(CODE_LO, CODE_SIZE)
        self.mu.mem_map(SCRATCH_LO, SCRATCH_SIZE)
        self.mu.mem_map(TICK_PTR & ~0xFFFF, 0x10000)
        self.mu.mem_map(0x092D0000, 0x20000)   # 五槽区 0x092D72E8..0x092EC968
        for addr, payload in M.parse_records(ph):
            self.mu.mem_write(addr, payload)
        for stub in (FW_LOADER, FW_FOPEN, FW_FREAD, FW_FWRITE, FW_FCLOSE):
            self.mu.mem_write(stub, BX_LR)
        self.mu.mem_write(TICK_PTR, struct.pack('<I', tick))
        self.files = dict(files or {})       # path -> bytes（内存VFS）
        self.open_fp = {}                    # fp -> [path, pos, mode]
        self.next_fp = 0x08FF2000
        self.fopen_fail = fopen_fail
        self.fwrite_short = fwrite_short
        self.loader_calls = []
        self.mu.reg_write(UC_ARM_REG_SP, 0x08FF8000)
        self.mu.reg_write(UC_ARM_REG_LR, STOP)
        self.mu.hook_add(UC_HOOK_CODE, self._hook)

    def _cstr(self, addr):
        raw = bytes(self.mu.mem_read(addr, 64))
        return raw.split(b'\0')[0]

    def _hook(self, mu, addr, size, user):
        if addr == FW_LOADER:
            sp = mu.reg_read(UC_ARM_REG_SP)
            argv = mu.reg_read(UC_ARM_REG_R2)
            path_ptr = struct.unpack('<I', bytes(mu.mem_read(argv + 4, 4)))[0]
            path = self._cstr(path_ptr)
            self.loader_calls.append(path)
            data = (PKG_DIR / path.split(b'\\')[-1].decode()).read_bytes()
            for a, p in M.parse_records(data):
                mu.mem_write(a, p)
            mu.reg_write(UC_ARM_REG_R0, 0)
            mu.reg_write(UC_ARM_REG_PC, mu.reg_read(UC_ARM_REG_LR))
        elif addr == FW_FOPEN:
            path = self._cstr(mu.reg_read(UC_ARM_REG_R0))
            mode = self._cstr(mu.reg_read(UC_ARM_REG_R1))
            fp = 0
            if not self.fopen_fail and mode in (b'r', b'w'):
                if mode == b'r' and path not in self.files:
                    fp = 0
                else:
                    fp = self.next_fp
                    self.next_fp += 4
                    if mode == b'w':
                        self.files[path] = b''
                    self.open_fp[fp] = [path, 0, mode]
            mu.reg_write(UC_ARM_REG_R0, fp)
            mu.reg_write(UC_ARM_REG_PC, mu.reg_read(UC_ARM_REG_LR))
        elif addr in (FW_FREAD, FW_FWRITE):
            # 真实七参 (buf,0,size=1,0,栈count_lo,栈count_hi,栈fp)：
            # 钩子在函数入口触发，sp 仍是调用方 sp，arg5=[sp+0] arg7=[sp+8]。
            buf = mu.reg_read(UC_ARM_REG_R0)
            size = mu.reg_read(UC_ARM_REG_R2)
            sp = mu.reg_read(UC_ARM_REG_SP)
            count = struct.unpack('<I', bytes(mu.mem_read(sp + 0, 4)))[0]
            fp = struct.unpack('<I', bytes(mu.mem_read(sp + 8, 4)))[0]
            nbytes = size * count
            n = 0
            if fp in self.open_fp:
                path, pos, mode = self.open_fp[fp]
                if addr == FW_FREAD and mode == b'r':
                    chunk = self.files[path][pos:pos + nbytes]
                    mu.mem_write(buf, chunk)
                    n = len(chunk)
                    self.open_fp[fp][1] += n
                elif addr == FW_FWRITE and mode == b'w':
                    data = bytes(mu.mem_read(buf, nbytes))
                    if self.fwrite_short:
                        data = data[:nbytes - 1]
                    self.files[path] = self.files[path][:pos] + data
                    n = len(data)
                    self.open_fp[fp][1] += n
            mu.reg_write(UC_ARM_REG_R0, n)
            mu.reg_write(UC_ARM_REG_PC, mu.reg_read(UC_ARM_REG_LR))
        elif addr == FW_FCLOSE:
            fp = mu.reg_read(UC_ARM_REG_R0)
            self.open_fp.pop(fp, None)
            mu.reg_write(UC_ARM_REG_R0, 0)
            mu.reg_write(UC_ARM_REG_PC, mu.reg_read(UC_ARM_REG_LR))

    def call(self, entry, r0=0):
        self.mu.reg_write(UC_ARM_REG_R0, r0)
        self.mu.reg_write(UC_ARM_REG_LR, STOP)
        self.mu.emu_start(entry, STOP, count=2000000)
        return self.mu.reg_read(UC_ARM_REG_R0)

    def word(self, addr):
        return struct.unpack('<I', bytes(self.mu.mem_read(addr, 4)))[0]

    def select(self, idx, tick=None):
        if tick is not None:
            self.mu.mem_write(TICK_PTR, struct.pack('<I', tick))
        return self.call(M.CORE_BASE, idx)

    def bootstrap(self, tick=None):
        if tick is not None:
            self.mu.mem_write(TICK_PTR, struct.pack('<I', tick))
        segs = dict(M.parse_records(PH.read_bytes()))
        core = segs[M.CORE_BASE]
        words = struct.unpack_from(f'<{len(core)//4}I', core, 0)
        # filter_bootstrap 入口 = MOV32 r0,CORE_STATE 对的前一字（push {r4,lr}）
        movw = M.enc_movw(0, CORE_STATE & 0xFFFF)
        movt = M.enc_movt(0, (CORE_STATE >> 16) & 0xFFFF)
        entry = None
        for i in range(len(words) - 1):
            if words[i] == movw and words[i + 1] == movt:
                entry = M.CORE_BASE + i * 4 - 4
                break
        assert entry is not None, '找不到 filter_bootstrap 入口'
        return self.call(entry)


def last_file(idx):
    return {LAST_PATH: struct.pack('<2I', LAST_MAGIC, idx)}


def main():
    ph = PH.read_bytes()
    segs = dict(M.parse_records(ph))
    entries = M.read_template_entries(ph)
    n = len(entries)
    preload = M.read_preload(segs[M.CORE_BASE])
    print(f'Build 72 滤镜记忆真执行门（n={n}, preload={preload}）')

    # --- 保存侧 ---
    s = Sim(ph)
    r = s.select(4)
    check('切换成功返回0', r == 0, f'r0={r}')
    check('current=4', s.word(CORE_STATE) == 4)
    check('ph_last.bin 内容={magic,4}',
          s.files.get(LAST_PATH) == struct.pack('<2I', LAST_MAGIC, 4),
          repr(s.files.get(LAST_PATH)))
    check('save_ok=1', s.word(SAVE_STAT) == 1)
    check('装载器收到正确路径', s.loader_calls == [entries[4][1].encode()],
          repr(s.loader_calls))
    s.select(7, tick=70000)
    check('再切idx7后文件更新', s.files[LAST_PATH] == struct.pack('<2I', LAST_MAGIC, 7))
    check('save_ok=2', s.word(SAVE_STAT) == 2)

    s = Sim(ph, fopen_fail=True)
    r = s.select(3)
    check('fopen失败不影响切换', r == 0 and s.word(CORE_STATE) == 3)
    check('fopen失败计save_fail', s.word(SAVE_STAT + 4) == 1)
    check('fopen失败无文件写出', LAST_PATH not in s.files)

    s = Sim(ph, fwrite_short=True)
    r = s.select(3)
    check('短写不影响切换', r == 0 and s.word(CORE_STATE) == 3)
    check('短写计save_fail', s.word(SAVE_STAT + 4) == 1)

    s = Sim(ph)
    r = s.select(99)
    check('无效idx拒切且不写文件', r == 2 and LAST_PATH not in s.files)
    s.select(4)                    # 成功一次，落盘 idx4
    r = s.select(7, tick=60050)    # 距上次成功切换 50ms < 180 防抖
    check('防抖拒切且文件不变', r == 1 and s.files[LAST_PATH] == struct.pack('<2I', LAST_MAGIC, 4))

    # --- 恢复侧 ---
    s = Sim(ph, files=last_file(6))
    r = s.bootstrap()
    check('恢复合法idx6', r == 0 and s.word(CORE_STATE) == 6, f'r0={r}')
    check('restore_src=1(记忆)', s.word(SAVE_STAT + 8) == 1)
    check('read_diag=0(成功)', s.word(SAVE_STAT + 16) == 0)
    check('恢复也回写文件', s.files[LAST_PATH] == struct.pack('<2I', LAST_MAGIC, 6))

    s = Sim(ph)   # 无文件
    r = s.bootstrap()
    check('无文件回退预装档', r == 0 and s.word(CORE_STATE) == preload)
    check('restore_src=0(预装)', s.word(SAVE_STAT + 8) == 0)
    check('read_diag=1(无文件)', s.word(SAVE_STAT + 16) == 1)

    s = Sim(ph, files={LAST_PATH: struct.pack('<2I', 0xDEADBEEF, 6)})
    r = s.bootstrap()
    check('坏magic回退预装档', r == 0 and s.word(CORE_STATE) == preload)
    check('read_diag=3(坏magic)', s.word(SAVE_STAT + 16) == 3)

    s = Sim(ph, files={LAST_PATH: struct.pack('<2I', LAST_MAGIC, 6)[:5]})
    r = s.bootstrap()
    check('短读回退预装档', r == 0 and s.word(CORE_STATE) == preload)
    check('read_diag=2(短读)', s.word(SAVE_STAT + 16) == 2)

    s = Sim(ph, files=last_file(n + 3))
    r = s.bootstrap()
    check('越界idx回退预装档', r == 0 and s.word(CORE_STATE) == preload)
    check('越界时restore_src=0', s.word(SAVE_STAT + 8) == 0)

    s = Sim(ph, files=last_file(6))
    s.mu.mem_write(CORE_STATE, struct.pack('<4I', 2, 0, 0, 0))
    r = s.bootstrap()
    check('current已知不覆盖', r == 0 and s.word(CORE_STATE) == 2)
    check('已知态不读文件', s.word(SAVE_STAT + 16) == 0 and s.loader_calls == [])

    s = Sim(ph, files=last_file(6), tick=50)
    r = s.bootstrap()
    check('早期tick=50恢复不被防抖吞', r == 0 and s.word(CORE_STATE) == 6, f'r0={r}')

    # --- 清单收缩后的越界回退（管理滤镜重建 + 72 布局） ---
    small = M.rebuild_ph_menu(ph, entries[:5], preload=1)
    s = Sim(small, files=last_file(8))
    r = s.bootstrap()
    check('n=5时旧idx8回退新预装1', r == 0 and s.word(CORE_STATE) == 1, f'r0={r}')
    s = Sim(small, files=last_file(3))
    r = s.bootstrap()
    check('n=5时合法idx3正常恢复', r == 0 and s.word(CORE_STATE) == 3)
    s = Sim(small)
    s.select(4)
    check('n=5切换保存正常', s.files[LAST_PATH] == struct.pack('<2I', LAST_MAGIC, 4))

    failed = [name for name, ok in RESULTS if not ok]
    print(f'\n{len(RESULTS) - len(failed)}/{len(RESULTS)} 通过')
    if failed:
        print('失败项:', failed)
    return 1 if failed else 0


if __name__ == '__main__':
    sys.exit(main())