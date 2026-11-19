//! 内置 ph_menu 重建器：与只读参考 `自定义置入滤镜流程/管理滤镜.py` rebuild 逐字节对齐。
//!
//! 只产出字节流（离线预演 / 受控写入的输入），不接触任何卡上文件；
//! 真实写卡仍由 [`crate::WRITE_GATE_ENABLED`] 恒禁用、须用户逐次授权。
//! 坐标约定与 py 一致：menu 内偏移为段内文件偏移，固件绝对地址以 MENU_BASE 为基准的相对值表示。

use std::collections::{HashMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::card::{
    self, parse_records, CHARSET, CORE_BASE, DURATIONS_BASE, LOG_HDR_BASE, MAX_NAME_LEN,
    MENU_BASE, PATH_STRIDE, STR_BASE,
};
use crate::CameraError;

pub const MENU_LIMIT: u32 = 0x08EF_3800;
pub const CARD_PREFIX: &str = "c:\\DXO_FILTERS\\";
pub const CUSTOM_MAGIC: u32 = 0x5047_3543; // "C5GP"

// menu.text P 区补丁点（偏移稳定）
const P_CYC_UP_CMP: usize = 0x558;
const P_CYC_WRAP_CMP: usize = 0x564;
const P_CYC_DN_CMP: usize = 0x57C;
const P_CYC_DN_MOV: usize = 0x58C;
const P_DISP_CMP: usize = 0x6A8;
const P_DISP_BHI: usize = 0x6AC;
const P_F1_SETUP_A: usize = 0x68C;
const P_F1_SETUP_B: usize = 0x6A0;
const F1_UNKNOWN_OFF: usize = 0x20;
const F1_SETUP_OFF: usize = 0x68;
const F1_BL_CLEAR: usize = 0x30;
const F1_ADR_WAIT: usize = 0x3C;
const F1_BL_AREA: usize = 0x64;
const FW_CLEAR_ADDR: i64 = 0x08EC_A0BC;
const FW_AREA_ADDR: i64 = 0x08EE_9D04;
const F1_LEN: usize = 420;
const F2_LEN: usize = 648;

// core.text 补丁点
const C_SELECT_CMP: usize = 0x02C;
const C_EXP_MOVW: usize = 0x180;
const C_EXP_MOVT: usize = 0x184;
const C_BOOT_CMP_72: usize = 0x30C;
const C_BOOT_LAST_CMP_72: usize = 0x344;

// dxot/a.bin 触屏改写 blob 横滑回绕补丁点（F-19 v2，与 py T_* 同）。
// 横滑（菜单页 ‹NAME› 与预览）共用 preview_ev；v2 重写 ev_right/ev_left 为模 n 回绕
// （13 词）。基线特例 right idx0→n 越界、left idx1→不动 为结构缺陷，n=10 一并修复。
const T_EV_SET: usize = 0x0508;
const T_ANCHORS: &[(usize, u32)] = &[
    (0x04B4, 0xE350_0012),
    (0x04BC, 0xE35B_0000),
    (0x04C0, 0x0A00_0008),
    (0x04C4, 0xE350_0012),
    (0x04C8, 0x0A00_0023),
    (0x04D4, 0x0A00_000B),
    (0x04E4, 0xEA00_0007),
    (0x0508, 0xE1A0_000A),
    (0x050C, 0xEBFC_3964),
];
// (偏移, 接受形式[(mask,expect)], 固定新字, kind: 0=固定 1=cmp r0,#(n-1) 2=moveq r1,#(n-1))
const T_POINTS: &[(usize, &[(u32, u32)], u32, u8)] = &[
    (0x04CC, &[(0xFFFF_F000, 0xE350_0000)], 0, 1),
    (0x04D0, &[(0xFFFF_F000, 0x03A0_1000)], 0x03A0_1000, 0),
    (0x04D8, &[(0xFFFF_F000, 0xE350_0000), (0xFFFF_FFFF, 0xE280_1001)], 0xE280_1001, 0),
    (0x04DC, &[(0xFFFF_F000, 0x03A0_1000), (0xFFFF_FFFF, 0xEA00_0009)], 0xEA00_0009, 0),
    (0x04E0, &[(0xFFFF_FFFF, 0x1280_1001), (0xFFFF_FFFF, 0xE1A0_0000)], 0xE1A0_0000, 0),
    (0x04E8, &[(0xFFFF_F000, 0xE350_0000)], 0xE350_0000, 0),
    (0x04EC, &[(0xFFFF_FFFF, 0x0A00_001A), (0xFFFF_F000, 0x03A0_1000)], 0, 2),
    (0x04F0, &[(0xFFFF_F000, 0xE350_0000), (0xFFFF_FFFF, 0x0A00_0004)], 0x0A00_0004, 0),
    (0x04F4, &[(0xFFFF_F000, 0x03A0_1000), (0xFFFF_FFFF, 0xE240_1001)], 0xE240_1001, 0),
    (0x04F8, &[(0xFFFF_FFFF, 0x0A00_0002), (0xFFFF_FFFF, 0xEA00_0002)], 0xEA00_0002, 0),
    (0x04FC, &[(0xFFFF_F000, 0xE350_0000), (0xFFFF_FFFF, 0xE1A0_0000)], 0xE1A0_0000, 0),
    (0x0500, &[(0xFFFF_F000, 0x03A0_1000), (0xFFFF_FFFF, 0xE1A0_0000)], 0xE1A0_0000, 0),
    (0x0504, &[(0xFFFF_FFFF, 0x1240_1001), (0xFFFF_FFFF, 0xE1A0_0000)], 0xE1A0_0000, 0),
];
pub const MAX_SLOTS: usize = (DURATIONS_BASE - STR_BASE) as usize / (PATH_STRIDE + 8);

// F-19 v6：纵向切档（滤镜页上下滑）改由 ph_menu 钩子接管。反汇编 a.bin seg0（RAM 基 0x08ee5080）
// 钉死根因：纵向事件 0x190/0x191（sb=0x84）经分派 `b 0x08EE5488` 进原生 block B —— 该块硬编码
// 胶片区 [2..11]（cmp#2 blo / cmp#0xb bhi）且顶端 add#1 越界卡死、NONE 特判跳 idx1，缩到 7 档后
// 划不到 G200(idx1)、不首尾循环。10-12 看着正常只因 [2..11] 恰好覆盖。直接改 a.bin block B 实机不
// 生效（早加载/指令行被预取）；工具既有生效明路 = ph_menu 晚加载钩子字覆盖同页原生 handler 行
// （0x8ee50a0/148/238/4e4 同页 hooks 实证）。
// v7：据实机 PH_DIAG 修正靶点（纵向事件 r4=0x190/0x191 时 sb=0x80，非 0x84）。分派 0x8ee53dc
// `cmp r9,#0x80; beq 0x8ee5400` → 纵向真入口 0x8ee5400：ldrb r0,[sl,#0x0a]; cmp r0,#0x10; bhi 0x8ee55cc;
// cmp fp,#0; beq DOWN; (UP)cmp r0,#0x10 beq bound/add +1; (DOWN)cmp r0,#0 beq bound/sub -1;
// mov r0,sl; bl 0x8df44fc; b 0x8ee55a4. 0x8ee55cc=pop 直接 return（夹住 idx）→「卡顶不循环」根因；
// 上限硬写 16、不绕回；setter=0x8df44fc；收尾=0x8ee55a4。0x8ee5480(block B/[sl,#0x40]/setV)是 sb=0x84
// else 分支，纵向从来不走（v5/v6 错靶）。ph_menu 在真入口写钩子字 V_HOOK=`b VSTUB`；VSTUB 落 CORE 空闲尾
// （CORE 0x08EF3800+0x508=0x08EF3D08 之后、SLIDE_STUB 0x08EF3F00 之前），读 [sl,#0x0a]、按方向 fp 做
// (idx±1) 于 1..n 双向回绕，复用原生 setter(0x8DF44FC) 与收尾(0x08EE55A4)，契约 sl=r10/fp=r11/新 idx 落 r1。
// n 随菜单档数自适应 → 同时满足「自适应档数 + 纵向首尾循环 + 到达 G200」。
pub const V_HOOK_ADDR: u32 = 0x08EE_5400; // 纵向真入口（sb=0x80/0x82）；ph_menu 在此写 `b VSTUB`
pub const VSTUB_ADDR: u32 = 0x08EF_3D80;  // 纵向回绕 stub 落址（CORE 尾空闲，与各方段无冲突）
const VSETV_ADDR: u32 = 0x08DF_44FC;      // 原生纵向 setter（0x8ee5400 handler 所用，r0=sl,r1=新idx）
const VRET_ADDR: u32 = 0x08EE_55A4;       // 原生返回收尾入口（setter 之后 b 到此）

/// V_HOOK_ADDR 处 `b VSTUB_ADDR`（AL 无条件）。
fn v_hook_word() -> Result<u32, CameraError> {
    enc_branch(14, V_HOOK_ADDR as i64, VSTUB_ADDR as i64, false)
}

/// (abs_addr, word)：纵向 (idx±1) 于 1..n 双向回绕 stub，寄存器契约同原生 0x8ee5400 入口。
fn v_stub_words(n: usize) -> Result<Vec<(u32, u32)>, CameraError> {
    if !(1..=MAX_SLOTS).contains(&n) {
        return Err(bad(format!("纵向档位数 {n} 超出 1..{MAX_SLOTS}")));
    }
    let b = VSTUB_ADDR;
    let commit = b + 0x30;
    let ws: [(u32, u32); 16] = [
        (b + 0x00, 0xE5DA_000A),                                                   // ldrb r0,[sl,#0x0a] idx（纵向真字段）
        (b + 0x04, 0xE35B_0000),                                                   // cmp fp,#0
        (b + 0x08, enc_branch(0, (b + 0x08) as i64, (b + 0x20) as i64, false)?),   // beq DOWN（fp==0）
        (b + 0x0c, 0xE280_1001),                                                   // UP: add r1,r0,#1
        (b + 0x10, enc_cmp(1, n as u32)?),                                         //     cmp r1,#n
        (b + 0x14, enc_branch(3, (b + 0x14) as i64, commit as i64, false)?),       //     blo COMMIT（LO/C=0：idx+1<n）
        (b + 0x18, 0xE3A0_1000),                                                   //     mov r1,#0（到 n 回 0）
        (b + 0x1c, enc_branch(14, (b + 0x1c) as i64, commit as i64, false)?),      //     b COMMIT
        (b + 0x20, 0xE250_1001),                                                   // DOWN: subs r1,r0,#1
        (b + 0x24, enc_branch(5, (b + 0x24) as i64, commit as i64, false)?),       //     bpl COMMIT（PL/N=0：idx-1>=0）
        (b + 0x28, enc_mov(1, (n - 1) as u32)?),                                   //     mov r1,#(n-1)（到底回 n-1）
        (b + 0x2c, enc_branch(14, (b + 0x2c) as i64, commit as i64, false)?),      //     b COMMIT
        (b + 0x30, 0xE1A0_000A),                                                   // COMMIT: mov r0,sl
        (b + 0x34, enc_branch(14, (b + 0x34) as i64, VSETV_ADDR as i64, true)?),   // bl setV
        (b + 0x38, enc_branch(14, (b + 0x38) as i64, VRET_ADDR as i64, false)?),   // b 原生回跳尾
        (b + 0x3c, 0xE1A0_0000),                                                   // NOP（补齐 16 字）
    ];
    Ok(ws.to_vec())
}

fn v_stub_payload(n: usize) -> Result<Vec<u8>, CameraError> {
    let ws = v_stub_words(n)?;
    Ok(ws.iter().map(|&(_, w)| w).flat_map(|w| w.to_le_bytes()).collect())
}

/// 校验 ph_menu 段流：V_HOOK 指 VSTUB、VSTUB 载荷 == v_stub_payload(n)。
fn verify_vertical_hook(segs: &[card::Segment], n: usize) -> Result<(), CameraError> {
    let hw = card::get_seg(segs, V_HOOK_ADDR).ok_or_else(|| bad("缺纵向钩子字 V_HOOK"))?;
    if hw.len() != 4 {
        return Err(bad("纵向钩子字长度异常"));
    }
    let target = dec_branch_s(u32::from_le_bytes(hw[..4].try_into().unwrap()), V_HOOK_ADDR as i64);
    if target != VSTUB_ADDR as i64 {
        return Err(bad("纵向钩子未指 VSTUB"));
    }
    let want = v_stub_payload(n)?;
    match card::get_seg(segs, VSTUB_ADDR) {
        Some(p) if p == want.as_slice() => Ok(()),
        _ => Err(bad(format!("纵向 stub 载荷不符（n={n}）"))),
    }
}

// F-19 v3：原生 OLED 滑动编辑「下一档」函数 next_filter_bound2 @0x08DFAE34（span 0x54=21 字）。
// 原固件硬编码边界 2（cmp r0,#2; movge r0,#0; addlt r0,r0,#1）→ 只循环 0→1→2→0（3 档），
// 与 Build72 扩档无关、不在 a.bin/menu 既有补丁覆盖区。唯一调用者 0x8E063C4（读 viewset+0x2b）。
// v3 以干净模 n 重写整函数：有效 idx→(idx+1) mod n、越界→0；去掉原生模式跳档逻辑（用户已选接受）。
// 载体定为 ph_menu.bin（最后加载）：实机 ph_ring dump 确证 ph_menu 晚加载原生补丁已写入 RAM；
// 而 a.bin 最先加载，当时 0x08DF 代码区尚未 remap 成可写 RAM，NF 写不进（aperture 跳档行为仍在）。
pub const NF_ADDR: u32 = 0x08DF_AE34;
pub const NF_SPAN: usize = 0x54; // 21 字
// 原固件基线 21 字（仅供固件版本核对/文档；ph_menu 不含固件，无法从中校验基线）。
const NF_BASELINE: [u32; 21] = [
    0xE59F_1778, 0xE350_0002, 0xA3A0_0000, 0xE5D1_1000, 0xB280_0001, 0xE351_0002,
    0x0A00_0005, 0xE351_0004, 0x0A00_0003, 0xE351_0003, 0x0A00_0001, 0xE350_0001,
    0x03A0_0002, 0xE59F_14AC, 0xE591_1000, 0xE5D1_104A, 0xE351_0005, 0x0A00_0001,
    0xE350_0002, 0x03A0_0000, 0xE12F_FF1E,
];

/// F-19 v3：干净模 n 重写 next_filter_bound2 的 21 字载荷（8 代码字 + 13 NOP，小端字节流）。
/// w0 cmp r0,#(n-1) / w1 bhi .Lzero / w2 add r0,r0,#1 / w3 cmp r0,#n / w4 moveq r0,#0 /
/// w5 bx lr / w6 .Lzero: mov r0,#0 / w7 bx lr / w8..20 nop。
fn nf_payload(n: usize) -> Result<Vec<u8>, CameraError> {
    if !(1..=MAX_SLOTS).contains(&n) {
        return Err(bad(format!("档位数 {n} 超出 1..{MAX_SLOTS}")));
    }
    let ws: [u32; 21] = [
        enc_cmp(0, (n - 1) as u32)?, // cmp r0,#(n-1)
        0x8A00_0003,                 // bhi .Lzero（src+8+3*4 = word6）
        0xE280_0001,                 // add r0,r0,#1
        enc_cmp(0, n as u32)?,       // cmp r0,#n
        0x03A0_0000,                 // moveq r0,#0
        0xE12F_FF1E,                 // bx lr
        0xE3A0_0000,                 // .Lzero: mov r0,#0
        0xE12F_FF1E,                 // bx lr
        0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000,
        0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000,
        0xE1A0_0000, 0xE1A0_0000, 0xE1A0_0000,
    ];
    let b = pack_words(&ws);
    if b.len() != NF_SPAN {
        return Err(bad("NF 载荷长度异常"));
    }
    Ok(b)
}

// ── F-19 v4：调用点绕行 ──────────────────────────────────────────────────────
// 实机证实：next_filter_bound2 @0x08DFAE34 所在 4KB 页 0x08DFA000 从未被任何补丁段写入
// （a.bin=0x08ee5/7/8000、b.bin=0x08ddf/df4/e05/…、ph_menu=0x08dff/06/11/ec9/eda/f2/…均不含
// 0x08dfa000）→ 该页只读/未 remap，NF 无论放 a.bin 还是 ph_menu 都静默写不进，bound2 体不变、
// 循环焊死三档。故不再改写只读的 bound2，改「可写调用点绕行」：
//   · CALL_SITE @0x08E063C4 是唯一调用者处那条 `bl #0x8dfae34`（页 0x08E06000，ph_menu 有原厂段
//     0x08e069d0 实证可写可执行）。把该 bl 重定向到 ph_menu 晚加载写入的执行页空闲尾 stub。
//   · SLIDE_STUB_ADDR @0x08EF3F00 在 menu/CORE 执行页 0x08EF3000、CORE(1288B→0x08EF3D08) 之后、
//     页界 0x08EF4000 之前的空闲区（已核与 ph_menu/a.bin/b.bin 全部段无冲突）。stub 内容即
//     nf_payload(n)（干净模 n：有效 idx→(idx+1) mod n、越界→0；自带相对跳转，与落址无关）。
pub const CALL_SITE: u32 = 0x08E0_63C4;
pub const SLIDE_STUB_ADDR: u32 = 0x08EF_3F00;

/// CALL_SITE 处把 `bl next_filter_bound2` 改指 SLIDE_STUB_ADDR 的 bl 字（AL+link）。
fn slide_redirect_word() -> Result<u32, CameraError> {
    enc_branch(14, CALL_SITE as i64, SLIDE_STUB_ADDR as i64, true)
}

fn bad(m: impl Into<String>) -> CameraError {
    CameraError::BadInput(m.into())
}

// ── ARM 编码（与 py enc_* 同） ────────────────────────────
fn enc_imm(val: u32) -> Result<u32, CameraError> {
    for rot in 0..16u32 {
        let k = 2 * rot;
        let imm8 = if k == 0 { val } else { val.rotate_left(k) };
        if imm8 <= 0xFF {
            return Ok((rot << 8) | imm8);
        }
    }
    Err(bad(format!("立即数不可表示为 ARM 旋转立即数：{val:#x}")))
}

fn enc_branch(cond: u32, src: i64, dst: i64, link: bool) -> Result<u32, CameraError> {
    let d = dst - src - 8;
    if d % 4 != 0 || !(-(1i64 << 25)..(1i64 << 25)).contains(&d) {
        return Err(bad(format!("ARM 跳转越界 {src:#x}->{dst:#x}")));
    }
    let w = (cond << 28) | 0x0A00_0000 | ((d >> 2) as u32 & 0x00FF_FFFF);
    Ok(if link { w | 0x0100_0000 } else { w })
}

fn dec_branch_s(w: u32, src: i64) -> i64 {
    let mut d = ((w & 0x00FF_FFFF) << 2) as i64;
    if d & (1 << 25) != 0 {
        d -= 1 << 26;
    }
    src + 8 + d
}

fn enc_cmp(rd: u32, imm: u32) -> Result<u32, CameraError> {
    if imm > 255 {
        return Err(bad(format!("cmp 立即数越界 {imm}")));
    }
    Ok(0xE350_0000 | (rd << 16) | imm)
}

fn enc_mov(rd: u32, imm: u32) -> Result<u32, CameraError> {
    if imm > 255 {
        return Err(bad(format!("mov 立即数越界 {imm}")));
    }
    Ok(0xE3A0_0000 | (rd << 12) | imm)
}

fn enc_adr(rd: u32, src: i64, dst: i64) -> Result<u32, CameraError> {
    let off = dst - src - 8;
    if off < 0 {
        return Err(bad(format!("adr 目标在指令之前 {src:#x}->{dst:#x}")));
    }
    Ok(0xE28F_0000 | (rd << 12) | enc_imm(off as u32)?)
}

fn enc_movw(rd: u32, imm16: u32) -> u32 {
    0xE300_0000 | (((imm16 >> 12) & 0xF) << 16) | (rd << 12) | (imm16 & 0xFFF)
}

fn enc_movt(rd: u32, imm16: u32) -> u32 {
    0xE340_0000 | (((imm16 >> 12) & 0xF) << 16) | (rd << 12) | (imm16 & 0xFFF)
}

fn put(b: &mut [u8], off: usize, word: u32, mask: u32, expect: u32) -> Result<(), CameraError> {
    let old = u32::from_le_bytes(b[off..off + 4].try_into().unwrap());
    if old & mask != expect {
        return Err(bad(format!("补丁点 {off:#x} opcode 不符：{old:#010x}")));
    }
    b[off..off + 4].copy_from_slice(&word.to_le_bytes());
    Ok(())
}

fn words(b: &[u8]) -> Vec<u32> {
    b.chunks_exact(4).map(|c| u32::from_le_bytes(c.try_into().unwrap())).collect()
}

fn pack_words(ws: &[u32]) -> Vec<u8> {
    ws.iter().flat_map(|w| w.to_le_bytes()).collect()
}

// ── 名字串 N 区 ──────────────────────────────────────────
pub fn encode_names(names: &[String]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut all: Vec<&str> = names.iter().map(|s| s.as_str()).collect();
    all.push("WAIT");
    for nm in all {
        let mut b = nm.as_bytes().to_vec();
        b.push(0);
        let pad = (-(b.len() as i64)) & 3;
        b.extend(std::iter::repeat(0u8).take(pad as usize));
        out.extend_from_slice(&b);
    }
    out
}

pub fn decode_names(n: &[u8]) -> Result<Vec<String>, CameraError> {
    let mut names = Vec::new();
    let mut pos = 0usize;
    while pos < n.len() {
        let end = n[pos..].iter().position(|&b| b == 0).map(|x| x + pos)
            .ok_or_else(|| bad("N 区名字串无 NUL 收尾"))?;
        names.push(String::from_utf8(n[pos..end].to_vec()).map_err(|_| bad("N 区名字非 ASCII"))?);
        pos = (end + 1 + 3) & !3;
    }
    if names.last().map(|s| s.as_str()) != Some("WAIT") {
        return Err(bad("N 区末尾不是 WAIT"));
    }
    names.pop();
    Ok(names)
}

// ── 输入校验（与 py validate_* 同约束） ───────────────────
pub fn validate_name(name: &str) -> Result<(), CameraError> {
    if name.is_empty() {
        return Err(bad("名字不能为空"));
    }
    if name.chars().count() > MAX_NAME_LEN {
        return Err(bad(format!("名字 {name:?} 超过 {MAX_NAME_LEN} 字符（箭头间最多 6 字）")));
    }
    let mut badc: Vec<char> = name.chars().filter(|c| !CHARSET.contains(*c)).collect();
    badc.sort_unstable();
    badc.dedup();
    if !badc.is_empty() {
        return Err(bad(format!("名字 {name:?} 含字模缺字 {badc:?}；可用：{CHARSET}")));
    }
    Ok(())
}

pub fn validate_card_path(path: &str) -> Result<(), CameraError> {
    if path.len() >= PATH_STRIDE {
        return Err(bad(format!("卡路径 {path:?} ≥ {PATH_STRIDE}B")));
    }
    if !path.is_ascii() {
        return Err(bad(format!("卡路径非 ASCII：{path:?}")));
    }
    if !path.starts_with(CARD_PREFIX) {
        return Err(bad(format!("卡路径须以 {CARD_PREFIX:?} 开头：{path:?}")));
    }
    Ok(())
}

// ── menu.text 生成 ───────────────────────────────────────
/// 规范布局：分发链 cmp/beq idx0..n-2；名字块默认项(n-1)在前，随后 idx0..n-2。
pub fn gen_v(names: &[String], v_start: usize, f1_start: usize, n_start: usize) -> Result<Vec<u8>, CameraError> {
    let n = names.len();
    if n < 1 {
        return Err(bad("至少保留一个档位"));
    }
    let mut name_off = Vec::with_capacity(n);
    let mut acc = 0usize;
    for nm in names {
        name_off.push(acc);
        acc += (nm.len() + 1 + 3) & !3;
    }
    let mut block_order = vec![n - 1];
    block_order.extend(0..n - 1);
    let pos_of: HashMap<usize, usize> = block_order.iter().enumerate().map(|(p, &idx)| (idx, p)).collect();
    let block_base = v_start + 8 * (n - 1);
    let block_addr = |idx: usize| block_base + 12 * pos_of[&idx];
    let mut ws = Vec::new();
    for idx in 0..n - 1 {
        ws.push(enc_cmp(6, idx as u32)?);
        ws.push(enc_branch(0, (v_start + 8 * idx + 4) as i64, block_addr(idx) as i64, false)?);
    }
    for &idx in &block_order {
        let ba = block_addr(idx);
        ws.push(enc_adr(0, ba as i64, (n_start + name_off[idx]) as i64)?);
        ws.push(enc_mov(1, names[idx].len() as u32)?);
        ws.push(enc_branch(14, (ba + 8) as i64, f1_start as i64, false)?);
    }
    Ok(pack_words(&ws))
}

fn patch_p(p: &[u8], n: usize, f1_start: usize) -> Result<Vec<u8>, CameraError> {
    let mut b = p.to_vec();
    put(&mut b, P_CYC_UP_CMP, enc_cmp(0, (n - 1) as u32)?, 0xFFFF_F000, 0xE350_0000)?;
    put(&mut b, P_CYC_WRAP_CMP, enc_cmp(0, n as u32)?, 0xFFFF_F000, 0xE350_0000)?;
    put(&mut b, P_CYC_DN_CMP, enc_cmp(0, (n - 1) as u32)?, 0xFFFF_F000, 0xE350_0000)?;
    put(&mut b, P_CYC_DN_MOV, enc_mov(0, (n - 1) as u32)?, 0xFFFF_F000, 0xE3A0_0000)?;
    put(&mut b, P_DISP_CMP, enc_cmp(6, (n - 1) as u32)?, 0xFFFF_F000, 0xE356_0000)?;
    put(&mut b, P_DISP_BHI,
        enc_branch(8, P_DISP_BHI as i64, (f1_start + F1_UNKNOWN_OFF) as i64, false)?,
        0xFF00_0000, 0x8A00_0000)?;
    // 进页两条条件分支跳 F1+0x68：V 长度变化使 F1 移位时必须同步重定位，保留原条件码。
    for off in [P_F1_SETUP_A, P_F1_SETUP_B] {
        let old = u32::from_le_bytes(b[off..off + 4].try_into().unwrap());
        if old & 0x0F00_0000 != 0x0A00_0000 {
            return Err(bad(format!("P 进页分支 {off:#x} 非分支指令：{old:#010x}")));
        }
        let w = enc_branch(old >> 28, off as i64, (f1_start + F1_SETUP_OFF) as i64, false)?;
        b[off..off + 4].copy_from_slice(&w.to_le_bytes());
    }
    Ok(b)
}

/// 重定位 F1 内所有跨区 PC 相对引用（F1 随 V 长度平移时必须同步）。
fn patch_f1(
    f1: &[u8],
    f1_start: usize,
    f2_start: usize,
    n_start: usize,
    wait_off: usize,
    tmpl: &card::MenuParts,
) -> Result<Vec<u8>, CameraError> {
    let (tf1, tn, tf2) = (tmpl.f1_start as i64, tmpl.n_start as i64, tmpl.f2_start as i64);
    let mut ws = words(f1);
    for (i, w) in ws.iter_mut().enumerate() {
        let off = i * 4;
        let src_old = tf1 + off as i64;
        let w0 = *w;
        let (tgt, is_adr, rd, cond, link) = if w0 & 0x0E00_0000 == 0x0A00_0000 {
            (dec_branch_s(w0, src_old), false, 0u32, w0 >> 28, w0 & 0x0100_0000 != 0)
        } else if w0 & 0xFFFF_0000 == 0xE28F_0000 {
            (src_old + 8 + card::dec_imm(w0) as i64, true, (w0 >> 12) & 0xF, 0u32, false)
        } else {
            continue;
        };
        if tf1 <= tgt && tgt < tf1 + f1.len() as i64 {
            continue; // F1 内部，自然平移
        }
        let new = if tgt >= tf2 {
            f2_start as i64 + (tgt - tf2)
        } else if tgt >= tn {
            n_start as i64 + wait_off as i64
        } else {
            tgt // 固件绝对地址（menu 相对坐标），不随菜单平移
        };
        *w = if is_adr {
            enc_adr(rd, f1_start as i64 + off as i64, new)?
        } else {
            enc_branch(cond, f1_start as i64 + off as i64, new, link)?
        };
    }
    Ok(pack_words(&ws))
}

/// 按新清单重建 menu.text。清单与模板一致时 V/N 逐字节复用（no-op 不改已验证产物）；
/// canonical=true 强制规范布局重生成 V（编码器与增删路径自检用）。
pub fn rebuild_menu_text(template_menu: &[u8], names: &[String], canonical: bool) -> Result<Vec<u8>, CameraError> {
    let parts = card::split_menu(template_menu)?;
    let p = &template_menu[..parts.v_start];
    let v = &template_menu[parts.v_start..parts.f1_start];
    let f1 = &template_menu[parts.f1_start..parts.n_start];
    let nseg = &template_menu[parts.n_start..parts.f2_start];
    let f2 = &template_menu[parts.f2_start..];
    if f1.len() != F1_LEN || f2.len() != F2_LEN {
        return Err(bad(format!("模板固定块长度异常 F1={} F2={}", f1.len(), f2.len())));
    }
    let tmpl_names = decode_names(nseg)?;
    let n = names.len();
    if !(1..=card::max_slots()).contains(&n) {
        return Err(bad(format!("档位数 {n} 超出 1..{}", card::max_slots())));
    }
    let v_start = p.len();
    let (v_new, n_new) = if names == tmpl_names.as_slice() && !canonical {
        (v.to_vec(), nseg.to_vec())
    } else {
        let n_new = encode_names(names);
        let vlen = 8 * (n - 1) + 12 * n;
        let v_new = gen_v(names, v_start, v_start + vlen, v_start + vlen + F1_LEN)?;
        (v_new, n_new)
    };
    let f1_start = v_start + v_new.len();
    let n_start = f1_start + F1_LEN;
    let f2_start = n_start + n_new.len();
    let p_new = patch_p(p, n, f1_start)?;
    let f1_new = patch_f1(f1, f1_start, f2_start, n_start, n_new.len() - 8, &parts)?;
    let mut menu = p_new;
    menu.extend_from_slice(&v_new);
    menu.extend_from_slice(&f1_new);
    menu.extend_from_slice(&n_new);
    menu.extend_from_slice(f2);
    if menu.len() > (MENU_LIMIT - MENU_BASE) as usize {
        return Err(bad(format!("menu.text {}B 超出分配区 {}B", menu.len(), MENU_LIMIT - MENU_BASE)));
    }
    Ok(menu)
}

// ── core.text 补丁 + STR ─────────────────────────────────
fn boot_points(layout: &str) -> (usize, Option<usize>, usize) {
    if layout == "72" {
        (C_BOOT_CMP_72, Some(C_BOOT_LAST_CMP_72), card::C_PRELOAD_MOV_72)
    } else {
        (card::C_BOOT_CMP_69, None, card::C_PRELOAD_MOV_69)
    }
}

pub fn patch_core(core: &[u8], n: usize, preload: usize) -> Result<Vec<u8>, CameraError> {
    if !(1..=card::max_slots()).contains(&n) {
        return Err(bad(format!("档位数 {n} 超出 1..{}", card::max_slots())));
    }
    if preload >= n {
        return Err(bad(format!("预装档 {preload} 越界（须 0..{}）", n - 1)));
    }
    let layout = card::core_layout(core).ok_or_else(|| bad("core 布局不认识"))?;
    let (boot_cmp, last_cmp, preload_mov) = boot_points(layout);
    let mut b = core.to_vec();
    let exp_base = STR_BASE + (n * PATH_STRIDE) as u32;
    if exp_base + (n * 8) as u32 > DURATIONS_BASE {
        return Err(bad("STR 区超出分配范围"));
    }
    put(&mut b, C_SELECT_CMP, enc_cmp(4, (n - 1) as u32)?, 0xFFFF_F000, 0xE354_0000)?;
    put(&mut b, C_EXP_MOVW, enc_movw(2, exp_base & 0xFFFF), 0xFFF0_F000, 0xE300_2000)?;
    put(&mut b, C_EXP_MOVT, enc_movt(2, (exp_base >> 16) & 0xFFFF), 0xFFF0_F000, 0xE340_2000)?;
    put(&mut b, boot_cmp, enc_cmp(0, (n - 1) as u32)?, 0xFFFF_F000, 0xE350_0000)?;
    if let Some(off) = last_cmp {
        put(&mut b, off, enc_cmp(0, (n - 1) as u32)?, 0xFFFF_F000, 0xE350_0000)?;
    }
    put(&mut b, preload_mov, enc_mov(0, preload as u32)?, 0xFFFF_F000, 0xE3A0_0000)?;
    Ok(b)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RebuildEntry {
    pub name: String,
    pub card_path: String,
    pub fp1: u32,
    pub fp5: u32,
}

pub fn build_str(entries: &[RebuildEntry]) -> Result<Vec<u8>, CameraError> {
    let n = entries.len();
    let mut paths = Vec::new();
    for e in entries {
        if !e.card_path.is_ascii() {
            return Err(bad(format!("路径非 ASCII：{:?}", e.card_path)));
        }
        let b = e.card_path.as_bytes();
        if b.len() >= PATH_STRIDE || b.contains(&0) {
            return Err(bad(format!("路径过长或含 null：{:?}", e.card_path)));
        }
        paths.extend_from_slice(b);
        paths.extend(std::iter::repeat(0u8).take(PATH_STRIDE - b.len()));
    }
    for e in entries {
        paths.extend_from_slice(&e.fp1.to_le_bytes());
        paths.extend_from_slice(&e.fp5.to_le_bytes());
    }
    if paths.len() != n * (PATH_STRIDE + 8) {
        return Err(bad("STR 长度异常"));
    }
    Ok(paths)
}

fn build_records(segs: &[(u32, Vec<u8>)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (addr, payload) in segs {
        out.extend_from_slice(&addr.to_le_bytes());
        out.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        out.extend_from_slice(payload);
    }
    out
}

fn t_gen_word(kind: u8, fixed: u32, n: usize) -> Result<u32, CameraError> {
    match kind {
        1 => enc_cmp(0, (n - 1) as u32),
        2 => Ok(enc_mov(1, (n - 1) as u32)? & 0x0FFF_FFFF),
        _ => Ok(fixed),
    }
}

/// F-19 v2：a.bin 触屏横滑重写为模 n 回绕（right=(idx+1) mod n、left=(idx-1) mod n）。
/// 接受基线/v1 δ/v2 已补丁三种形态（幂等、跨 n 重补丁）；只产出字节流，不接触卡。
pub fn patch_touch(a: &[u8], n: usize) -> Result<Vec<u8>, CameraError> {
    if !(1..=MAX_SLOTS).contains(&n) {
        return Err(bad(format!("档位数 {n} 超出 1..{MAX_SLOTS}")));
    }
    if a.len() <= T_EV_SET + 8 {
        return Err(bad("a.bin 过短：缺触屏循环补丁点"));
    }
    let mut b = a.to_vec();
    for &(off, word) in T_ANCHORS {
        let old = u32::from_le_bytes(b[off..off + 4].try_into().unwrap());
        if old != word {
            return Err(bad(format!("a.bin 锚点 {off:#x} 不符：{old:#010x}（非 Linda v26 触屏块）")));
        }
    }
    for &(off, accepts, fixed, kind) in T_POINTS {
        let old = u32::from_le_bytes(b[off..off + 4].try_into().unwrap());
        if !accepts.iter().any(|&(m, e)| old & m == e) {
            return Err(bad(format!("a.bin 补丁点 {off:#x} opcode 不符：{old:#010x}")));
        }
        let w = t_gen_word(kind, fixed, n)?;
        b[off..off + 4].copy_from_slice(&w.to_le_bytes());
    }
    // F-19 v6：纵向切档不再改写 a.bin（早加载/预取导致不生效），改由 ph_menu 钩子接管
    // （见 rebuild_ph_menu + v_stub_words）；a.bin 仅保留横滑 T_POINTS 的原厂缺陷修复。
    // 剥离任何遗留 NF/纵向钩子/纵向 stub 段（均属 ph_menu）。
    let segs: Vec<(u32, Vec<u8>)> = parse_records(&b)?
        .into_iter()
        .map(|s| (s.addr, s.payload))
        .filter(|(addr, _)| *addr != NF_ADDR && *addr != V_HOOK_ADDR && *addr != VSTUB_ADDR)
        .collect();
    Ok(build_records(&segs))
}

/// 校验 v2 产物：锚点 + 13 补丁点逐词吻合（与 py verify_touch 同）。
pub fn verify_touch(a: &[u8], n: usize) -> Result<(), CameraError> {
    if !(1..=MAX_SLOTS).contains(&n) {
        return Err(bad(format!("档位数 {n} 超出 1..{MAX_SLOTS}")));
    }
    if a.len() <= T_EV_SET + 8 {
        return Err(bad("a.bin 过短：缺触屏循环补丁点"));
    }
    let w = |off: usize| u32::from_le_bytes(a[off..off + 4].try_into().unwrap());
    for &(off, word) in T_ANCHORS {
        if w(off) != word {
            return Err(bad(format!("触屏锚点 {off:#x} 不符")));
        }
    }
    for &(off, _, fixed, kind) in T_POINTS {
        if w(off) != t_gen_word(kind, fixed, n)? {
            return Err(bad(format!("触屏回绕补丁点 {off:#x} 不符（n={n}）")));
        }
    }
    // F-19 v6：纵向已迁 ph_menu（见 verify_vertical_hook）；a.bin 不应含纵向钩子/stub 段。
    for s in parse_records(a)? {
        if s.addr == V_HOOK_ADDR || s.addr == VSTUB_ADDR {
            return Err(bad(format!("纵向 stub/钩子不应落 a.bin（属 ph_menu）：{:#010x}", s.addr)));
        }
    }
    Ok(())
}

/// F-20：ph_last.bin 记忆文件首字 magic（"PHLS" 小端），与 Build 72 core.S LAST_MAGIC 一致。
pub const PH_LAST_MAGIC: u32 = 0x534C_4850;

/// F-20：构造 ph_last.bin 内容（8 B = magic + idx，均小端）。
/// 写新菜单时把记忆重置为开机默认档，使 bootstrap 恢复到该档（而非上次关机档）。
/// 只产出字节流，不接触卡；idx 范围校验由调用方/固件兜底（越界 bootstrap 回退预装）。
pub fn ph_last_bytes(idx: u32) -> Vec<u8> {
    let mut v = Vec::with_capacity(8);
    v.extend_from_slice(&PH_LAST_MAGIC.to_le_bytes());
    v.extend_from_slice(&idx.to_le_bytes());
    v
}

/// 按新清单重组 ph_menu：只替换 menu.text / core.text / STR，其余段逐字节保留。
/// preload 缺省沿用模板（越界取 0）；magic 缺省沿用模板 LOG_HDR。
pub fn rebuild_ph_menu(
    template: &[u8],
    entries: &[RebuildEntry],
    preload: Option<usize>,
    canonical: bool,
    magic: Option<u32>,
) -> Result<Vec<u8>, CameraError> {
    let segs = parse_records(template)?;
    let n = entries.len();
    if !(1..=card::max_slots()).contains(&n) {
        return Err(bad(format!("档位数 {n} 超出 1..{}", card::max_slots())));
    }
    let mut seen = HashSet::new();
    for e in entries {
        if !seen.insert(e.fp1) {
            return Err(bad("slot1 指纹有重复，无法区分滤镜"));
        }
    }
    let tmpl_core = card::get_seg(&segs, CORE_BASE).ok_or_else(|| bad(format!("模板缺段 {CORE_BASE:#010x}")))?;
    let preload = match preload {
        Some(p) => p,
        None => {
            let old = card::read_preload(tmpl_core).ok_or_else(|| bad("core 布局不认识，无法沿用预装档"))?;
            if old < n { old } else { 0 }
        }
    };
    let names: Vec<String> = entries.iter().map(|e| e.name.clone()).collect();
    let tmpl_menu = card::get_seg(&segs, MENU_BASE).ok_or_else(|| bad(format!("模板缺段 {MENU_BASE:#010x}")))?;
    let menu_new = rebuild_menu_text(tmpl_menu, &names, canonical)?;
    let core_new = patch_core(tmpl_core, n, preload)?;
    let str_new = build_str(entries)?;
    let mut replace: HashMap<u32, Vec<u8>> = HashMap::new();
    replace.insert(MENU_BASE, menu_new);
    replace.insert(CORE_BASE, core_new);
    replace.insert(STR_BASE, str_new);
    let mut out: Vec<(u32, Vec<u8>)> = Vec::new();
    for s in &segs {
        let mut payload = s.payload.clone();
        if let Some(np) = replace.remove(&s.addr) {
            payload = np;
        }
        if s.addr == LOG_HDR_BASE {
            if let Some(m) = magic {
                if payload.len() < 16 {
                    return Err(bad("LOG_HDR 段不足 16B，无法写 magic"));
                }
                payload[12..16].copy_from_slice(&m.to_le_bytes());
            }
        }
        out.push((s.addr, payload));
    }
    if !replace.is_empty() {
        let miss: Vec<String> = replace.keys().map(|a| format!("{a:#010x}")).collect();
        return Err(bad(format!("模板缺段：{miss:?}")));
    }
    // F-19 v4：调用点绕行——stub(模 n) 落 ph_menu 执行页空闲尾 SLIDE_STUB_ADDR，
    // 并把唯一调用者 CALL_SITE 处 `bl next_filter_bound2` 重定向到 stub。二者随 ph_menu
    // 晚加载写入可写可执行页（bound2 原址 0x08DFA000 只读写不进）。移除任何遗留 NF 死写。幂等。
    // F-19 v7：纵向钩子字落真入口 0x8ee5400 V_HOOK_ADDR(`b VSTUB`)，stub 落 CORE 尾空闲 VSTUB_ADDR，
    // 按方向 fp 于 1..n 双向回绕后调原生 setter 收尾。载体段先剥离再追加（幂等）。
    out.retain(|(addr, _)| {
        *addr != NF_ADDR
            && *addr != SLIDE_STUB_ADDR
            && *addr != CALL_SITE
            && *addr != V_HOOK_ADDR
            && *addr != VSTUB_ADDR
    });
    out.push((SLIDE_STUB_ADDR, nf_payload(n)?));
    out.push((CALL_SITE, pack_words(&[slide_redirect_word()?])));
    out.push((VSTUB_ADDR, v_stub_payload(n)?));
    out.push((V_HOOK_ADDR, pack_words(&[v_hook_word()?])));
    let mut occupied: Vec<(u32, u32)> = Vec::new();
    for (addr, payload) in &out {
        let end = addr + payload.len() as u32;
        for &(s, e) in &occupied {
            if *addr < e && s < end {
                return Err(bad(format!("段重叠 {addr:#010x} 与 {s:#010x}")));
            }
        }
        occupied.push((*addr, end));
    }
    Ok(build_records(&out))
}

/// 从模板 ph_menu 还原登记清单（名字来自 menu N，路径/指纹来自 STR）。
pub fn read_template_entries(template: &[u8]) -> Result<Vec<RebuildEntry>, CameraError> {
    let segs = parse_records(template)?;
    let menu = card::get_seg(&segs, MENU_BASE).ok_or_else(|| bad("模板缺 MENU 段"))?;
    let parts = card::split_menu(menu)?;
    let names = decode_names(&menu[parts.n_start..parts.f2_start])?;
    let strbody = card::get_seg(&segs, STR_BASE).ok_or_else(|| bad("模板缺 STR 段"))?;
    let stride = PATH_STRIDE + 8;
    let n = strbody.len() / stride;
    if strbody.len() != n * stride || n != names.len() {
        return Err(bad("模板 STR 与 menu 名字数不一致"));
    }
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let raw = &strbody[i * PATH_STRIDE..(i + 1) * PATH_STRIDE];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let path = String::from_utf8(raw[..end].to_vec()).map_err(|_| bad("STR 路径非 ASCII"))?;
        let off = n * PATH_STRIDE + i * 8;
        let fp1 = u32::from_le_bytes(strbody[off..off + 4].try_into().unwrap());
        let fp5 = u32::from_le_bytes(strbody[off + 4..off + 8].try_into().unwrap());
        out.push(RebuildEntry { name: names[i].clone(), card_path: path, fp1, fp5 });
    }
    Ok(out)
}

/// 合成模板（单元测试与离线 py↔Rust 交叉验证专用；不含任何固件版权字节）。
pub mod synth {
    use super::*;

    const P_LEN: usize = 0x6B0;
    const NOP: u32 = 0xE1A0_0000;

    pub fn synth_menu(names: &[String]) -> Result<Vec<u8>, CameraError> {
        let n = names.len();
        let vlen = 8 * (n - 1) + 12 * n;
        let v_start = P_LEN;
        let f1_start = v_start + vlen;
        let n_start = f1_start + F1_LEN;
        let nseg = encode_names(names);
        let f2_start = n_start + nseg.len();
        let mut pw = vec![NOP; P_LEN / 4];
        pw[P_CYC_UP_CMP / 4] = enc_cmp(0, (n - 1) as u32)?;
        pw[P_CYC_WRAP_CMP / 4] = enc_cmp(0, n as u32)?;
        pw[P_CYC_DN_CMP / 4] = enc_cmp(0, (n - 1) as u32)?;
        pw[P_CYC_DN_MOV / 4] = enc_mov(0, (n - 1) as u32)?;
        pw[P_DISP_CMP / 4] = enc_cmp(6, (n - 1) as u32)?;
        pw[P_DISP_BHI / 4] = enc_branch(8, P_DISP_BHI as i64, (f1_start + F1_UNKNOWN_OFF) as i64, false)?;
        pw[P_F1_SETUP_A / 4] = enc_branch(1, P_F1_SETUP_A as i64, (f1_start + F1_SETUP_OFF) as i64, false)?;
        pw[P_F1_SETUP_B / 4] = enc_branch(1, P_F1_SETUP_B as i64, (f1_start + F1_SETUP_OFF) as i64, false)?;
        let v = gen_v(names, v_start, f1_start, n_start)?;
        let mut fw = vec![NOP; F1_LEN / 4];
        fw[0] = 0xE3A0_2015; // .Ldc_emit mov r2,#21
        fw[1] = enc_branch(14, (f1_start + 4) as i64, (f1_start + 0xC0) as i64, true)?; // bl menu_text_at
        fw[F1_BL_CLEAR / 4] = enc_branch(14, (f1_start + F1_BL_CLEAR) as i64, FW_CLEAR_ADDR - MENU_BASE as i64, true)?;
        fw[F1_ADR_WAIT / 4] = enc_adr(0, (f1_start + F1_ADR_WAIT) as i64, (n_start + nseg.len() - 8) as i64)?;
        fw[F1_BL_AREA / 4] = enc_branch(14, (f1_start + F1_BL_AREA) as i64, FW_AREA_ADDR - MENU_BASE as i64, true)?;
        fw[0x80 / 4] = enc_branch(0, (f1_start + 0x80) as i64, (f1_start + 0x90) as i64, false)?; // 区内分支
        fw[0x100 / 4] = 0xE92D_4FF0; // menu_draw_text push
        fw[0x114 / 4] = enc_adr(9, (f1_start + 0x114) as i64, f2_start as i64)?; // adr r9,menu_index
        fw[0x12C / 4] = enc_adr(9, (f1_start + 0x12C) as i64, (f2_start + 96) as i64)?; // adr r9,menu_font
        fw[0x1A0 / 4] = 0xE12F_FF1E; // bx lr
        let f2 = vec![NOP; F2_LEN / 4];
        let mut menu = pack_words(&pw);
        menu.extend_from_slice(&v);
        menu.extend_from_slice(&pack_words(&fw));
        menu.extend_from_slice(&nseg);
        menu.extend_from_slice(&pack_words(&f2));
        Ok(menu)
    }

    pub fn synth_core(layout: &str, n: usize, preload: usize) -> Result<Vec<u8>, CameraError> {
        let mut cw = vec![NOP; 0x400 / 4];
        let exp_base = STR_BASE + (n * PATH_STRIDE) as u32;
        cw[C_SELECT_CMP / 4] = enc_cmp(4, (n - 1) as u32)?;
        cw[C_EXP_MOVW / 4] = enc_movw(2, exp_base & 0xFFFF);
        cw[C_EXP_MOVT / 4] = enc_movt(2, (exp_base >> 16) & 0xFFFF);
        if layout == "72" {
            cw[card::C_BOOT_CMP_69 / 4] = 0xE340_0000 | 0x08EF;
            cw[C_BOOT_CMP_72 / 4] = enc_cmp(0, (n - 1) as u32)?;
            cw[C_BOOT_LAST_CMP_72 / 4] = enc_cmp(0, (n - 1) as u32)?;
            cw[card::C_PRELOAD_MOV_72 / 4] = enc_mov(0, preload as u32)?;
        } else {
            cw[card::C_BOOT_CMP_69 / 4] = enc_cmp(0, (n - 1) as u32)?;
            cw[card::C_PRELOAD_MOV_69 / 4] = enc_mov(0, preload as u32)?;
        }
        Ok(pack_words(&cw))
    }

    pub fn synth_ph_menu(names: &[String], layout: &str, preload: usize) -> Result<Vec<u8>, CameraError> {
        let entries: Vec<RebuildEntry> = names.iter().enumerate().map(|(i, nm)| RebuildEntry {
            name: nm.clone(),
            card_path: format!("{CARD_PREFIX}lut_{nm}.bin"),
            fp1: 0x11 + i as u32,
            fp5: 0x21 + i as u32,
        }).collect();
        let menu = synth_menu(names)?;
        let core = synth_core(layout, names.len(), preload)?;
        let strb = build_str(&entries)?;
        let mut log = vec![0u8; 32];
        log[12..16].copy_from_slice(&0x5047_3547u32.to_le_bytes());
        let extra = b"DXO-ONE-XLAB synth keep-segment".to_vec();
        let nf = nf_payload(names.len())?;
        let redirect = pack_words(&[slide_redirect_word()?]);
        let vstub = v_stub_payload(names.len())?;
        let vhook = pack_words(&[v_hook_word()?]);
        Ok(build_records(&[
            (MENU_BASE, menu),
            (CORE_BASE, core),
            (STR_BASE, strb),
            (LOG_HDR_BASE, log),
            (0x08EF_6800, extra),
            (SLIDE_STUB_ADDR, nf),
            (CALL_SITE, redirect),
            (VSTUB_ADDR, vstub),
            (V_HOOK_ADDR, vhook),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    fn tmpl() -> Vec<u8> {
        synth::synth_ph_menu(&names(&["ORIG", "GOL2", "SATOBI"]), "72", 2).unwrap()
    }

    fn entries_of(t: &[u8]) -> Vec<RebuildEntry> {
        read_template_entries(t).unwrap()
    }

    #[test]
    fn noop_is_byte_identical() {
        let t = tmpl();
        let e = entries_of(&t);
        let out = rebuild_ph_menu(&t, &e, None, false, None).unwrap();
        assert_eq!(out, t, "清单不变必须逐字节复用模板");
    }

    #[test]
    fn rename_decodes_and_keeps_other_segments() {
        let t = tmpl();
        let mut e = entries_of(&t);
        e[0].name = "NEWB".into();
        let out = rebuild_ph_menu(&t, &e, None, false, None).unwrap();
        assert_ne!(out, t);
        let segs = parse_records(&out).unwrap();
        let menu = card::get_seg(&segs, MENU_BASE).unwrap();
        assert_eq!(card::decode_menu_names(menu, 3).unwrap(), names(&["NEWB", "GOL2", "SATOBI"]));
        assert_eq!(card::get_seg(&segs, 0x08EF_6800).unwrap(), b"DXO-ONE-XLAB synth keep-segment");
        let log = card::get_seg(&segs, LOG_HDR_BASE).unwrap();
        assert_eq!(u32::from_le_bytes(log[12..16].try_into().unwrap()), 0x5047_3547);
        assert_eq!(card::read_preload(card::get_seg(&segs, CORE_BASE).unwrap()), Some(2));
    }

    #[test]
    fn remove_shrinks_and_clamps_preload() {
        let t = tmpl();
        let mut e = entries_of(&t);
        e.pop(); // 删默认项 idx2 → preload 越界回退 0
        let out = rebuild_ph_menu(&t, &e, None, false, None).unwrap();
        let segs = parse_records(&out).unwrap();
        assert_eq!(card::decode_menu_names(card::get_seg(&segs, MENU_BASE).unwrap(), 2).unwrap(),
            names(&["ORIG", "GOL2"]));
        assert_eq!(card::read_preload(card::get_seg(&segs, CORE_BASE).unwrap()), Some(0));
        let strb = card::get_seg(&segs, STR_BASE).unwrap();
        assert_eq!(strb.len(), 2 * (PATH_STRIDE + 8));
    }

    #[test]
    fn add_grows_and_relocates_f1() {
        let t = tmpl();
        let mut e = entries_of(&t);
        e.push(RebuildEntry { name: "ANON".into(), card_path: format!("{CARD_PREFIX}lut_anon.bin"), fp1: 0x99, fp5: 0x98 });
        let out = rebuild_ph_menu(&t, &e, Some(3), false, None).unwrap();
        let segs = parse_records(&out).unwrap();
        let menu = card::get_seg(&segs, MENU_BASE).unwrap();
        assert_eq!(card::decode_menu_names(menu, 4).unwrap(), names(&["ORIG", "GOL2", "SATOBI", "ANON"]));
        assert_eq!(card::read_preload(card::get_seg(&segs, CORE_BASE).unwrap()), Some(3));
        // F1 平移后固件 bl 仍指向固件绝对地址（menu 相对坐标不变）
        let p = card::split_menu(menu).unwrap();
        let fw = words(&menu[p.f1_start..p.n_start]);
        assert_eq!(dec_branch_s(fw[F1_BL_CLEAR / 4], (p.f1_start + F1_BL_CLEAR) as i64), FW_CLEAR_ADDR - MENU_BASE as i64);
        assert_eq!(dec_branch_s(fw[F1_BL_AREA / 4], (p.f1_start + F1_BL_AREA) as i64), FW_AREA_ADDR - MENU_BASE as i64);
        // N 区 WAIT adr 指向新 WAIT
        let nseg = &menu[p.n_start..p.f2_start];
        let wait_off = nseg.len() - 8;
        let adr = fw[F1_ADR_WAIT / 4];
        assert_eq!(p.f1_start + F1_ADR_WAIT + 8 + card::dec_imm(adr) as usize, p.n_start + wait_off);
        assert_eq!(&nseg[wait_off..wait_off + 4], b"WAIT");
    }

    #[test]
    fn reorder_and_canonical() {
        let t = tmpl();
        let mut e = entries_of(&t);
        e.reverse();
        let out = rebuild_ph_menu(&t, &e, Some(0), false, None).unwrap();
        let segs = parse_records(&out).unwrap();
        assert_eq!(card::decode_menu_names(card::get_seg(&segs, MENU_BASE).unwrap(), 3).unwrap(),
            names(&["SATOBI", "GOL2", "ORIG"]));
        // canonical 强制重生成 V：名字不变但布局规范化，解码仍一致
        let menu0 = card::get_seg(&segs, MENU_BASE).unwrap().to_vec();
        let canon = rebuild_menu_text(&menu0, &names(&["SATOBI", "GOL2", "ORIG"]), true).unwrap();
        assert_eq!(card::decode_menu_names(&canon, 3).unwrap(), names(&["SATOBI", "GOL2", "ORIG"]));
        let noop = rebuild_menu_text(&canon, &names(&["SATOBI", "GOL2", "ORIG"]), false).unwrap();
        assert_eq!(noop, canon, "规范布局产物再 no-op 必须逐字节不变");
    }

    #[test]
    fn core_layouts_69_and_72() {
        for layout in ["69", "72"] {
            let t = synth::synth_ph_menu(&names(&["ORIG", "GOL2", "SATOBI"]), layout, 1).unwrap();
            let e = entries_of(&t);
            let out = rebuild_ph_menu(&t, &e, Some(2), false, None).unwrap();
            let segs = parse_records(&out).unwrap();
            let core = card::get_seg(&segs, CORE_BASE).unwrap();
            assert_eq!(card::core_layout(core), Some(layout));
            assert_eq!(card::read_preload(core), Some(2));
            let cw = words(core);
            let exp_base = STR_BASE + 3 * PATH_STRIDE as u32;
            assert_eq!(cw[C_EXP_MOVW / 4], enc_movw(2, exp_base & 0xFFFF));
            assert_eq!(cw[C_EXP_MOVT / 4], enc_movt(2, (exp_base >> 16) & 0xFFFF));
            assert_eq!(cw[C_SELECT_CMP / 4], enc_cmp(4, 2).unwrap());
        }
    }

    #[test]
    fn custom_magic_and_rejects() {
        let t = tmpl();
        let e = entries_of(&t);
        let out = rebuild_ph_menu(&t, &e, None, false, Some(CUSTOM_MAGIC)).unwrap();
        let segs = parse_records(&out).unwrap();
        let log = card::get_seg(&segs, LOG_HDR_BASE).unwrap();
        assert_eq!(u32::from_le_bytes(log[12..16].try_into().unwrap()), CUSTOM_MAGIC);
        assert_eq!(card::build_tag(CUSTOM_MAGIC), Some("custom"));
        // 指纹重复 / 空清单 / 超容量 / 缺段
        let mut dup = e.clone();
        dup[1].fp1 = dup[0].fp1;
        assert!(rebuild_ph_menu(&t, &dup, None, false, None).is_err());
        assert!(rebuild_ph_menu(&t, &[], None, false, None).is_err());
        let many: Vec<RebuildEntry> = (0..13).map(|i| RebuildEntry {
            name: "A".into(), card_path: format!("{CARD_PREFIX}lut_{i}.bin"), fp1: i, fp5: i,
        }).collect();
        assert!(rebuild_ph_menu(&t, &many, None, false, None).is_err());
        let segs_only = parse_records(&t).unwrap();
        let no_str: Vec<(u32, Vec<u8>)> = segs_only.iter().filter(|s| s.addr != STR_BASE)
            .map(|s| (s.addr, s.payload.clone())).collect();
        assert!(rebuild_ph_menu(&build_records(&no_str), &e, None, false, None).is_err());
    }

    #[test]
    fn name_and_path_validation() {
        assert!(validate_name("SATOBI").is_ok());
        assert!(validate_name("GOLD").is_err(), "D 缺字");
        assert!(validate_name("MIYABI").is_err(), "M 缺字");
        assert!(validate_name("TOOLONG7").is_err());
        assert!(validate_name("").is_err());
        assert!(validate_card_path("c:\\DXO_FILTERS\\lut_a.bin").is_ok());
        assert!(validate_card_path("d:\\DXO_FILTERS\\lut_a.bin").is_err());
        assert!(validate_card_path(&format!("{CARD_PREFIX}{}", "x".repeat(32))).is_err());
    }

    #[test]
    fn max_capacity_menu_fits() {
        // 字模内字符构造 12 个互异名（数字 1/3/4/6/7/8/9 缺字）
        let ns: Vec<String> = ["A", "B", "E", "F", "G", "H", "I", "K", "L", "N", "O", "P"]
            .iter().map(|s| s.to_string()).collect();
        let t = synth::synth_ph_menu(&ns, "72", 0).unwrap();
        let e = entries_of(&t);
        let out = rebuild_ph_menu(&t, &e, None, true, None).unwrap();
        let segs = parse_records(&out).unwrap();
        assert_eq!(card::decode_menu_names(card::get_seg(&segs, MENU_BASE).unwrap(), 12).unwrap(), ns);
    }

    #[test]
    fn enc_imm_branch_roundtrip() {
        for v in [0u32, 1, 255, 256, 0x1D0, 0x1B4, 0x888, 0xFF00_0000] {
            if let Ok(w) = enc_imm(v) {
                assert_eq!(card::dec_imm(w), v, "{v:#x}");
            }
        }
        for (src, dst) in [(100i64, 200i64), (200, 100), (0x08EF_2800, 0x08EF_3000), (0x08EF_3000, 0x08EF_2800)] {
            let w = enc_branch(14, src, dst, true).unwrap();
            assert_eq!(dec_branch_s(w, src), dst);
            assert_ne!(w & 0x0100_0000, 0);
        }
        assert!(enc_branch(14, 0, 1 << 26, false).is_err());
        assert!(enc_branch(14, 0, 3, false).is_err(), "非 4 对齐");
        assert!(enc_adr(0, 100, 90).is_err());
    }

    fn touch_blob() -> Vec<u8> {
        // 真实 a.bin 为记录流：seg1 头 8 B + 载荷；T_* 为文件偏移，故载荷内偏移 = off-8。
        let mut payload = vec![0u8; 0x2000];
        for &(off, w) in T_ANCHORS {
            payload[off - 8..off - 8 + 4].copy_from_slice(&w.to_le_bytes());
        }
        for (off, w) in [
            // 横滑 preview_ev 基线（未补丁原字）
            (0x04CCusize, 0xE350_0000u32),
            (0x04D0, 0x03A0_100A),
            (0x04D8, 0xE350_0009),
            (0x04DC, 0x03A0_1000),
            (0x04E0, 0x1280_1001),
            (0x04E8, 0xE350_0001),
            (0x04EC, 0x0A00_001A),
            (0x04F0, 0xE350_0000),
            (0x04F4, 0x03A0_1009),
            (0x04F8, 0x0A00_0002),
            (0x04FC, 0xE350_000A),
            (0x0500, 0x03A0_1000),
            (0x0504, 0x1240_1001),
            // 纵向切档 block B 原生 [2..11]（v6 不再改写 a.bin；ph_menu 钩子接管，此处仅作真实样本）
            (0x0414, 0xE350_0000), (0x0418, 0x0A00_000D), (0x041C, 0xE350_0002),
            (0x0420, 0x3A00_004D), (0x0424, 0xE350_000B), (0x0428, 0x8A00_004B),
            (0x042C, 0xE35B_0000), (0x0430, 0x0A00_0003), (0x0434, 0xE350_000B),
            (0x0438, 0x0A00_0047), (0x043C, 0xE280_1001), (0x0440, 0xEA00_0002),
            (0x0444, 0xE350_0002), (0x0448, 0x03A0_1000), (0x044C, 0x1240_1001),
            (0x0450, 0xEA00_0002), (0x0454, 0xE35B_0000), (0x0458, 0x0A00_003F),
            (0x045C, 0xE3A0_1002),
        ] {
            payload[off - 8..off - 8 + 4].copy_from_slice(&w.to_le_bytes());
        }
        build_records(&[(0x08EE_5080, payload)])
    }

    #[test]
    fn touch_patch_v2_all_n_idempotent_cross_n() {
        let b = touch_blob();
        for n in 1..=MAX_SLOTS {
            let out = patch_touch(&b, n).unwrap();
            verify_touch(&out, n).unwrap();
            assert_eq!(patch_touch(&out, n).unwrap(), out, "n={n} 幂等");
            assert_eq!(out.len(), 8 + 0x2000, "a.bin 仅 seg1（NF 已挪 ph_menu）");
        }
        assert_ne!(patch_touch(&b, 10).unwrap(), b, "v2 修复基线潜缺陷，n=10 不再 no-op");
        let cross = patch_touch(&patch_touch(&b, 8).unwrap(), 5).unwrap();
        verify_touch(&cross, 5).unwrap();
    }

    #[test]
    fn touch_patch_v2_words_and_diff() {
        let out = patch_touch(&touch_blob(), 7).unwrap();
        let w = |off: usize| u32::from_le_bytes(out[off..off + 4].try_into().unwrap());
        assert_eq!(w(0x04CC), 0xE350_0006, "right: cmp r0,#(n-1)");
        assert_eq!(w(0x04D0), 0x03A0_1000, "right: 末档→0");
        assert_eq!(w(0x04D8), 0xE280_1001, "right: add r1,r0,#1");
        assert_eq!(w(0x04DC), 0xEA00_0009, "right: b ev_set");
        assert_eq!(w(0x04E8), 0xE350_0000, "left: cmp r0,#0");
        assert_eq!(w(0x04EC), 0x03A0_1006, "left: 首档→末档");
        assert_eq!(w(0x04F0), 0x0A00_0004, "left: beq ev_set");
        assert_eq!(w(0x04F4), 0xE240_1001, "left: sub r1,r0,#1");
        assert_eq!(w(0x04F8), 0xEA00_0002, "left: b ev_set");
        for off in [0x04E0, 0x04FC, 0x0500, 0x0504] {
            assert_eq!(w(off), 0xE1A0_0000, "nop @{off:#x}");
        }
        let base = touch_blob();
        let hw = (0x04CC..0x0508).step_by(4)
            .filter(|&off| out[off..off + 4] != base[off..off + 4]).count();
        assert_eq!(hw, 13, "横滑只动 13 个补丁词");
        // F-19 v6：纵向已迁 ph_menu 钩子，patch_touch 不再改写 block B（0x0410..0x0460 须原样）
        let base_bytes = touch_blob();
        assert_eq!(&out[0x0410..0x0464], &base_bytes[0x0410..0x0464], "纵向 block B 须未被 a.bin 改写");
    }

    #[test]
    fn touch_patch_v2_accepts_v1_delta_form() {
        let mut v1 = touch_blob();
        for (off, w) in [
            (0x04D0usize, 0x03A0_1007u32),
            (0x04D8, 0xE350_0006),
            (0x04F4, 0x03A0_1006),
            (0x04FC, 0xE350_0007),
        ] {
            v1[off..off + 4].copy_from_slice(&w.to_le_bytes());
        }
        let out = patch_touch(&v1, 7).unwrap();
        verify_touch(&out, 7).unwrap();
        assert_eq!(out, patch_touch(&touch_blob(), 7).unwrap(), "v1 输入与基线输入产物一致");
    }

    #[test]
    fn touch_patch_rejects_bad_input() {
        assert!(patch_touch(&touch_blob(), 0).is_err());
        assert!(patch_touch(&touch_blob(), MAX_SLOTS + 1).is_err());
        assert!(patch_touch(&[0u8; 16], 8).is_err());
        let mut b = touch_blob();
        b[0x04D0..0x04D0 + 4].copy_from_slice(&0x03A0_000Au32.to_le_bytes());
        assert!(patch_touch(&b, 8).is_err(), "opcode 不符须拒");
        let mut b = touch_blob();
        b[0x050C..0x050C + 4].copy_from_slice(&0xEB00_0000u32.to_le_bytes());
        assert!(patch_touch(&b, 8).is_err(), "锚点不符须拒");
        let out = patch_touch(&touch_blob(), 8).unwrap();
        assert!(verify_touch(&out, 7).is_err(), "verify 须区分 n");
    }

    // ── F-19 v3：NF 段（原生滑动编辑 next_filter 干净模 n） ──────────────
    // 逐字解码 8 代码字的小型 ARM 模拟器（cmp/bhi/add/moveq/bx/mov/nop），离线地面真相。
    fn sim_nf(payload: &[u8], idx: u32) -> u32 {
        let ws = words(payload);
        let mut pc = 0usize;
        let mut r0 = idx;
        let (mut z, mut c) = (false, false);
        loop {
            let w = ws[pc];
            let cond = w >> 28;
            let take = match cond {
                0xE => true,
                0x0 => z,
                0x8 => c && !z,
                _ => panic!("未用条件码 {cond:#x}"),
            };
            if take {
                if w == 0xE12F_FF1E {
                    return r0; // bx lr
                } else if (w >> 24) & 0xF == 0xA {
                    let mut d = ((w & 0x00FF_FFFF) << 2) as i32;
                    if d & (1 << 25) != 0 {
                        d -= 1 << 26;
                    }
                    pc = ((pc as i32 * 4 + 8 + d) / 4) as usize;
                    continue;
                } else if w & 0x0FFF_0000 == 0x0350_0000 {
                    let imm = w & 0xFF;
                    assert_eq!((w >> 16) & 0xF, 0, "cmp 须 r0");
                    z = r0 == imm;
                    c = r0 >= imm;
                } else if w & 0x0FFF_0000 == 0x0280_0000 {
                    let imm = w & 0xFF;
                    assert_eq!((w >> 12) & 0xF, 0);
                    assert_eq!((w >> 16) & 0xF, 0);
                    r0 = r0.wrapping_add(imm);
                } else if w & 0x0FFF_0000 == 0x03A0_0000 {
                    assert_eq!((w >> 12) & 0xF, 0);
                    r0 = w & 0xFF;
                } else if w == 0xE1A0_0000 {
                    // nop
                } else {
                    panic!("未预期指令 {w:#010x}");
                }
            }
            pc += 1;
        }
    }

    #[test]
    fn nf_payload_mod_n_all_idx() {
        for n in 1..=MAX_SLOTS {
            let p = nf_payload(n).unwrap();
            assert_eq!(p.len(), NF_SPAN);
            for idx in 0..=(n as u32 + 3) {
                let want = if idx >= n as u32 { 0 } else { (idx + 1) % n as u32 };
                assert_eq!(sim_nf(&p, idx), want, "n={n} idx={idx}");
            }
        }
        assert!(nf_payload(0).is_err());
        assert!(nf_payload(MAX_SLOTS + 1).is_err());
    }

    #[test]
    fn ph_menu_carries_slide_redirect() {
        // F-19 v4：ph_menu 携带「执行页空闲 stub + 调用点重定向」；a.bin 不含 stub/redirect；
        // F-19 v6：ph_menu 另携带「纵向钩子字 V_HOOK + 纵向模 n stub VSTUB」；a.bin 不含之；
        // 只读页 NF 死写(0x08DFAE34)从产物中移除。追加序：STUB → CALL → VSTUB → V_HOOK(末)。
        const NM: [&str; 12] = ["AA", "AB", "AE", "AF", "AG", "AH", "AI", "AK", "AL", "AN", "AO", "AP"];
        // n=1 的模 n 载荷由 nf_payload_mod_n_all_idx 覆盖；此处 n≥2（n=1 synth 菜单布局另有既有缺陷，与本机制无关）。
        for n in [2usize, 3, 7, 10, 12] {
            let names: Vec<String> = NM[..n].iter().map(|s| s.to_string()).collect();
            let t = synth::synth_ph_menu(&names, "72", 0).unwrap();
            let e = read_template_entries(&t).unwrap();
            let out = rebuild_ph_menu(&t, &e, Some(0), false, None).unwrap();
            let segs = parse_records(&out).unwrap();
            let len = segs.len();
            // 横滑 stub：唯一、== nf_payload(n)、追加四段中倒数第四
            let stub = card::get_seg(&segs, SLIDE_STUB_ADDR).unwrap();
            assert_eq!(stub, nf_payload(n).unwrap().as_slice(), "n={n} stub==nf_payload");
            assert_eq!(segs.iter().filter(|s| s.addr == SLIDE_STUB_ADDR).count(), 1, "stub 唯一");
            assert_eq!(segs[len - 4].addr, SLIDE_STUB_ADDR, "stub 居追加段首位");
            // 横滑 redirect：唯一、bl 反算回 stub 地址
            let red = card::get_seg(&segs, CALL_SITE).unwrap();
            assert_eq!(red, pack_words(&[slide_redirect_word().unwrap()]).as_slice(), "n={n} redirect bl");
            assert_eq!(segs.iter().filter(|s| s.addr == CALL_SITE).count(), 1, "redirect 唯一");
            assert_eq!(segs[len - 3].addr, CALL_SITE, "redirect 紧随 stub");
            assert_eq!(
                dec_branch_s(u32::from_le_bytes(red[..4].try_into().unwrap()), CALL_SITE as i64),
                SLIDE_STUB_ADDR as i64,
                "redirect bl 目标须为 stub"
            );
            // F-19 v6 纵向 stub：唯一、== v_stub_payload(n)、倒数第二
            let vstub = card::get_seg(&segs, VSTUB_ADDR).unwrap();
            assert_eq!(vstub, v_stub_payload(n).unwrap().as_slice(), "n={n} vstub==v_stub_payload");
            assert_eq!(segs.iter().filter(|s| s.addr == VSTUB_ADDR).count(), 1, "vstub 唯一");
            assert_eq!(segs[len - 2].addr, VSTUB_ADDR, "vstub 居倒数第二");
            // F-19 v6 纵向钩子：唯一、居末、b 反算回 VSTUB 地址
            let vhook = card::get_seg(&segs, V_HOOK_ADDR).unwrap();
            assert_eq!(vhook, pack_words(&[v_hook_word().unwrap()]).as_slice(), "n={n} vhook b");
            assert_eq!(segs.iter().filter(|s| s.addr == V_HOOK_ADDR).count(), 1, "vhook 唯一");
            assert_eq!(segs.last().unwrap().addr, V_HOOK_ADDR, "vhook 居末");
            assert_eq!(
                dec_branch_s(u32::from_le_bytes(vhook[..4].try_into().unwrap()), V_HOOK_ADDR as i64),
                VSTUB_ADDR as i64,
                "vhook b 目标须为 VSTUB"
            );
            verify_vertical_hook(&segs, n).unwrap();
            // 只读页 NF 死写不出现
            assert!(card::get_seg(&segs, NF_ADDR).is_none(), "不应含只读页 NF 死写");
            // 模 n 行为（ARM 模拟横滑 stub）全 idx
            for idx in 0..=(n as u32 + 3) {
                let want = if idx >= n as u32 { 0 } else { (idx + 1) % n as u32 };
                assert_eq!(sim_nf(stub, idx), want, "n={n} idx={idx}");
            }
            // 幂等：再次重建产物不变
            let out2 = rebuild_ph_menu(&out, &e, Some(0), false, None).unwrap();
            assert_eq!(out2, out, "n={n} ph_menu 幂等");
            // a.bin 不含 stub/redirect/NF/纵向钩子/纵向 stub
            let a = patch_touch(&touch_blob(), n).unwrap();
            let aseg = parse_records(&a).unwrap();
            for addr in [NF_ADDR, SLIDE_STUB_ADDR, CALL_SITE, V_HOOK_ADDR, VSTUB_ADDR] {
                assert!(card::get_seg(&aseg, addr).is_none(), "a.bin 不应含 {addr:#010x}");
            }
        }
        // 基线文档常量长度自洽 + stub 落点自洽（页界内、CORE 之后）
        assert_eq!(NF_BASELINE.len() * 4, NF_SPAN);
        assert!(SLIDE_STUB_ADDR >= 0x08EF_3D08, "stub 须在 CORE(1288B→0x08ef3d08) 之后");
        assert!(SLIDE_STUB_ADDR + NF_SPAN as u32 <= 0x08EF_4000, "stub 须落在执行页 0x08ef3000 内");
        assert_eq!(CALL_SITE & 0xFFFF_F000, 0x08E0_6000, "调用点在 ph_menu 已证可写页 0x08e06000");
        // 纵向 stub 落 CORE 空闲尾：CORE 之后(0x08ef3d08)、SLIDE_STUB 之前(0x08ef3f00)、同执行页 16 字内
        assert!(VSTUB_ADDR >= 0x08EF_3D08, "VSTUB 须在 CORE 之后");
        assert!(VSTUB_ADDR + 64 <= SLIDE_STUB_ADDR, "VSTUB 16 字须落在 SLIDE_STUB 之前空闲区");
    }

    // F-19 v7：钉住纵向钩子字与 n=7 stub 载荷 == py 端实测字节（跨语言逐字节 parity）。
    #[test]
    fn v7_hooks_byte_parity_with_py() {
        assert_eq!(V_HOOK_ADDR, 0x08EE_5400);
        assert_eq!(VSETV_ADDR, 0x08DF_44FC);
        assert_eq!(VRET_ADDR, 0x08EE_55A4);
        assert_eq!(v_hook_word().unwrap(), 0xEA00_3A5E);
        let py_stub: [u8; 64] = [
            0x0a,0x00,0xda,0xe5,0x00,0x00,0x5b,0xe3,0x04,0x00,0x00,0x0a,0x01,0x10,0x80,0xe2,
            0x07,0x00,0x51,0xe3,0x05,0x00,0x00,0x3a,0x00,0x10,0xa0,0xe3,0x03,0x00,0x00,0xea,
            0x01,0x10,0x50,0xe2,0x01,0x00,0x00,0x5a,0x06,0x10,0xa0,0xe3,0xff,0xff,0xff,0xea,
            0x0a,0x00,0xa0,0xe1,0xd0,0x01,0xfc,0xeb,0xf9,0xc5,0xff,0xea,0x00,0x00,0xa0,0xe1,
        ];
        assert_eq!(v_stub_payload(7).unwrap().as_slice(), &py_stub[..]);
        // 首指令须读 [sl,#0x0a]（纵向真字段），bl 落 setter、b 落收尾
        assert_eq!(u32::from_le_bytes(py_stub[0..4].try_into().unwrap()), 0xE5DA_000A);
    }

    // F-19 v6：纵向 stub 逐字 ARM 模拟（仅覆盖本 stub 用到的指令类），返回提交 setV 的 r1。
    // 寄存器契约：r0=idx（ldrb 得）、fp(r11)=方向。离线地面真相，与 py v_stub_run 同。
    fn sim_v_stub(n: usize, idx: u32, up: bool) -> u32 {
        let code: Vec<(u32, u32)> = v_stub_words(n).unwrap();
        let get = |a: u32| -> u32 { code.iter().find(|&&(o, _)| o == a).map(|&(_, w)| w).expect("stub 跳出代码区") };
        let dec_imm = |w: u32| -> u32 { let r = (w & 0xFF).rotate_right(2 * ((w >> 8) & 0xF)); r };
        let mut reg = [0u32; 16];
        reg[0] = idx;
        reg[11] = up as u32;
        let (mut z, mut c, mut nv) = (false, false, false);
        let mut pc = VSTUB_ADDR;
        for _ in 0..64 {
            let w = get(pc);
            let base = w & 0x0FFF_FFFF;
            let cls = base & 0x0FF0_0000;
            if base & 0x0E00_0000 == 0x0A00_0000 {
                let cond = w >> 28;
                let taken = match cond {
                    0xE => true,
                    0x0 => z,
                    0x3 => !c,
                    0x5 => !nv,
                    _ => panic!("纵向 stub 未支持条件码 {cond:#x}"),
                };
                let tgt = dec_branch_s(w, pc as i64) as u32;
                if w & 0x0100_0000 != 0 {
                    assert_eq!(tgt, VSETV_ADDR, "纵向 stub bl 目标异常");
                    return reg[1];
                }
                pc = if taken { tgt } else { pc + 4 };
                continue;
            }
            if base == 0x05DA_000A {
                reg[0] = idx;
            } else if cls == 0x0350_0000 {
                let v = reg[((w >> 16) & 0xF) as usize];
                let imm = dec_imm(w);
                z = v == imm;
                c = v >= imm;
                nv = v & 0x8000_0000 != 0;
            } else if cls == 0x0280_0000 {
                let rd = ((w >> 12) & 0xF) as usize;
                reg[rd] = reg[((w >> 16) & 0xF) as usize].wrapping_add(dec_imm(w));
            } else if cls == 0x0250_0000 {
                let rd = ((w >> 12) & 0xF) as usize;
                let rn = ((w >> 16) & 0xF) as usize;
                let imm = dec_imm(w);
                let r = reg[rn].wrapping_sub(imm);
                reg[rd] = r;
                z = r == 0;
                c = reg[rn] >= imm;
                nv = r & 0x8000_0000 != 0;
            } else if cls == 0x03A0_0000 {
                let rd = ((w >> 12) & 0xF) as usize;
                reg[rd] = dec_imm(w);
            } else if cls == 0x01A0_0000 {
                let rd = ((w >> 12) & 0xF) as usize;
                reg[rd] = reg[(w & 0xF) as usize];
            } else {
                panic!("纵向 stub 未预期指令 {w:#010x} @ {pc:#x}");
            }
            pc += 4;
        }
        panic!("纵向 stub 模拟步数超限");
    }

    #[test]
    fn v_stub_mod_n_all_idx_direction() {
        for n in 1..=MAX_SLOTS {
            for idx in 0..n as u32 {
                for up in [false, true] {
                    let want = if up {
                        (idx + 1) % n as u32
                    } else {
                        (idx + n as u32 - 1) % n as u32
                    };
                    assert_eq!(sim_v_stub(n, idx, up), want, "n={n} idx={idx} up={up}");
                }
            }
        }
        assert!(v_stub_payload(0).is_err());
        assert!(v_stub_payload(MAX_SLOTS + 1).is_err());
    }

    /// 与 py sync-touch 产物逐字节对齐（离线地面真相由 py 生成）；默认 ignore。
    /// 运行：DXO_TOUCH_A=<a.bin模板> DXO_TOUCH_GOLDEN=<py产物> DXO_TOUCH_N=8 cargo test -p lut-camera touch_parity_vs_py -- --ignored
    #[test]
    #[ignore]
    fn touch_parity_vs_py() {
        let a = std::fs::read(std::env::var("DXO_TOUCH_A").unwrap()).unwrap();
        let golden = std::fs::read(std::env::var("DXO_TOUCH_GOLDEN").unwrap()).unwrap();
        let n: usize = std::env::var("DXO_TOUCH_N").unwrap().parse().unwrap();
        assert_eq!(patch_touch(&a, n).unwrap(), golden);
    }

    /// 与 py rebuild_ph_menu 产物逐字节对齐（含 NF 段载体）；默认 ignore。
    /// 运行：DXO_PH_TEMPLATE=<ph_menu模板> DXO_PH_GOLDEN=<py产物> cargo test -p lut-camera ph_menu_parity_vs_py -- --ignored
    #[test]
    #[ignore]
    fn ph_menu_parity_vs_py() {
        let t = std::fs::read(std::env::var("DXO_PH_TEMPLATE").unwrap()).unwrap();
        let golden = std::fs::read(std::env::var("DXO_PH_GOLDEN").unwrap()).unwrap();
        let e = read_template_entries(&t).unwrap();
        assert_eq!(rebuild_ph_menu(&t, &e, None, false, None).unwrap(), golden);
    }

    #[test]
    fn ph_last_format_matches_build72() {
        // 卡上实测 ph_last.bin（idx=0）= 50 48 4c 53 00 00 00 00（"PHLS"+0）
        assert_eq!(ph_last_bytes(0), vec![0x50, 0x48, 0x4C, 0x53, 0, 0, 0, 0]);
        let b = ph_last_bytes(4);
        assert_eq!(b.len(), 8);
        assert_eq!(u32::from_le_bytes(b[0..4].try_into().unwrap()), PH_LAST_MAGIC);
        assert_eq!(u32::from_le_bytes(b[4..8].try_into().unwrap()), 4);
        assert_eq!(&b[0..4], b"PHLS");
    }
}