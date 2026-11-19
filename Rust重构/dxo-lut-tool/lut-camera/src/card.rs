//! R4-01 相机服务：只读探测 / 包解析 / 工具默认项 / 容量 profile / 完整五槽校验。
//!
//! 本关一律使用临时/模拟卷，绝不写真实相机卡；未知固件只读。
//! 布局常量与几何推导对齐只读参考 `自定义置入滤镜流程/管理滤镜.py`（不硬编码 Build 档数）。

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::CameraError;

// ── ph_menu / 包布局（与只读参考一致） ────────────────────
pub const MENU_BASE: u32 = 0x08EF_2800;
pub const CORE_BASE: u32 = 0x08EF_3800;
pub const STR_BASE: u32 = 0x08EF_6400;
pub const DURATIONS_BASE: u32 = 0x08EF_6600;
pub const LOG_HDR_BASE: u32 = 0x08EF_61F0;
pub const PATH_STRIDE: usize = 32;
pub const SLOT_ADDRS: [u32; 5] = [
    0x092D_72E8,
    0x092D_B768,
    0x092D_FBE8,
    0x092E_4068,
    0x092E_84E8,
];
pub const SLOT_SIZE: usize = 17_536;
pub const SLOT_COUNT: usize = 5;
pub const PKG_SIZE: usize = SLOT_COUNT * (8 + SLOT_SIZE);
pub const FP_OFFSET: usize = 0x1000;
pub const C_BOOT_CMP_69: usize = 0x304;
pub const C_PRELOAD_MOV_69: usize = 0x314;
pub const C_PRELOAD_MOV_72: usize = 0x36C;
pub const MAX_NAME_LEN: usize = 6;
pub const CHARSET: &str = "-025ABEFGHIKLNOPRSTUVWY";

/// 容量上限由 STR 区几何推导（512/40=12），不硬编码任何 Build 的档数。
pub fn max_slots() -> usize {
    (DURATIONS_BASE - STR_BASE) as usize / (PATH_STRIDE + 8)
}

/// LOG_HDR magic → 已核验 Build tag；未知 magic 返回 None（只读）。
pub fn build_tag(magic: u32) -> Option<&'static str> {
    match magic {
        0x5047_3544 => Some("69"), // D5GP
        0x5047_3545 => Some("70"), // E5GP
        0x5047_3546 => Some("71"), // F5GP
        0x5047_3547 => Some("72"), // G5GP（实机核验卡 ph_menu sha 6349d43d）
        0x5047_3543 => Some("custom"), // C5GP
        _ => None,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SlotEntry {
    pub name: String,
    pub path: String,
    pub fp1: u32,
    pub fp5: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PackageInfo {
    pub file: String,
    pub size: usize,
    pub slot_count: usize,
    pub fp1: u32,
    pub fp5: u32,
    pub valid: bool,
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardProfile {
    pub build_tag: Option<String>,
    pub known_firmware: bool,
    /// 未知固件只读；本关写入入口另由安全服务恒禁用。
    pub read_only: bool,
    pub n_slots: usize,
    pub max_slots: usize,
    pub preload: usize,
    pub entries: Vec<SlotEntry>,
    pub packages: Vec<PackageInfo>,
    pub issues: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefaults {
    pub charset: String,
    pub max_name_len: usize,
    pub slot_size: usize,
    pub slot_count: usize,
    pub max_slots: usize,
}

pub fn tool_defaults() -> ToolDefaults {
    ToolDefaults {
        charset: CHARSET.to_string(),
        max_name_len: MAX_NAME_LEN,
        slot_size: SLOT_SIZE,
        slot_count: SLOT_COUNT,
        max_slots: max_slots(),
    }
}

#[derive(Debug, Clone)]
pub struct Segment {
    pub addr: u32,
    pub payload: Vec<u8>,
}

/// 解析 ph_menu 记录流：(addr u32, size u32, payload[size])。
pub fn parse_records(data: &[u8]) -> Result<Vec<Segment>, CameraError> {
    let mut pos = 0usize;
    let mut out = Vec::new();
    while pos < data.len() {
        if pos + 8 > data.len() {
            return Err(CameraError::BadInput("ph_menu 记录头截断".into()));
        }
        let addr = u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap());
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        if size == 0 || pos + size > data.len() {
            return Err(CameraError::BadInput("ph_menu 记录载荷截断或为空".into()));
        }
        out.push(Segment { addr, payload: data[pos..pos + size].to_vec() });
        pos += size;
    }
    Ok(out)
}

pub fn get_seg<'a>(segs: &'a [Segment], addr: u32) -> Option<&'a [u8]> {
    segs.iter().find(|s| s.addr == addr).map(|s| s.payload.as_slice())
}

fn le32(b: &[u8], off: usize) -> u32 {
    u32::from_le_bytes(b[off..off + 4].try_into().unwrap())
}

/// 解析 STR 区：n 条 (path[32], fp1 u32, fp5 u32)。n 由长度推导。
pub fn parse_str(strbody: &[u8]) -> Result<Vec<SlotEntry>, CameraError> {
    let stride = PATH_STRIDE + 8;
    if strbody.len() % stride != 0 {
        return Err(CameraError::BadInput("STR 长度不是 40 的整数倍".into()));
    }
    let n = strbody.len() / stride;
    let mut out = Vec::with_capacity(n);
    for i in 0..n {
        let raw = &strbody[i * PATH_STRIDE..(i + 1) * PATH_STRIDE];
        let end = raw.iter().position(|&b| b == 0).unwrap_or(raw.len());
        let path = String::from_utf8(raw[..end].to_vec())
            .map_err(|_| CameraError::BadInput("STR 路径非 ASCII".into()))?;
        let fp_off = n * PATH_STRIDE + i * 8;
        let fp1 = le32(strbody, fp_off);
        let fp5 = le32(strbody, fp_off + 4);
        let name = path
            .rsplit(['\\', '/'])
            .next()
            .unwrap_or("")
            .trim_start_matches("lut_")
            .trim_end_matches(".bin")
            .to_uppercase();
        out.push(SlotEntry { name, path, fp1, fp5 });
    }
    Ok(out)
}

/// core 布局识别（对齐 管理滤镜.py core_layout）：72 的 0x304 是 movt r0,#0x08EF
/// 上半字，69/71 的 0x304 是 cmp r0,#imm；其余 opcode 不认识 → None（只读不猜）。
pub fn core_layout(core: &[u8]) -> Option<&'static str> {
    if core.len() < C_BOOT_CMP_69 + 4 {
        return None;
    }
    let w = le32(core, C_BOOT_CMP_69);
    if w == 0xE340_0000 | 0x08EF {
        return Some("72");
    }
    if w & 0xFFFF_F000 == 0xE350_0000 {
        return Some("69");
    }
    None
}

/// 开机预装档（相机「默认滤镜」）：按布局取 mov 立即数；布局不认识 → None。
pub fn read_preload(core: &[u8]) -> Option<usize> {
    let off = match core_layout(core)? {
        "72" => C_PRELOAD_MOV_72,
        _ => C_PRELOAD_MOV_69,
    };
    if core.len() < off + 4 {
        return None;
    }
    Some((le32(core, off) & 0xFFF) as usize)
}

pub(crate) fn dec_imm(w: u32) -> u32 {
    let imm8 = w & 0xFF;
    let k = 2 * ((w >> 8) & 0xF);
    if k == 0 { imm8 } else { (imm8 >> k) | (imm8 << (32 - k)) }
}

fn dec_branch(w: u32, src: usize) -> usize {
    let mut d = ((w & 0x00FF_FFFF) << 2) as i64;
    if d & (1 << 25) != 0 {
        d -= 1 << 26;
    }
    (src as i64 + 8 + d) as usize
}

pub(crate) struct MenuParts {
    pub(crate) v_start: usize,
    pub(crate) f1_start: usize,
    pub(crate) n_start: usize,
    pub(crate) f2_start: usize,
}

/// 按指令签名切 menu.text（对齐 管理滤镜.py split_menu，不硬编码长度）。
pub(crate) fn split_menu(menu: &[u8]) -> Result<MenuParts, CameraError> {
    if menu.len() % 4 != 0 {
        return Err(CameraError::BadInput("menu.text 非 4 字节对齐".into()));
    }
    let words: Vec<u32> = menu
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let bad = |m: &str| CameraError::BadInput(m.into());
    let v_start = words.iter().position(|&w| w == 0xE356_0000).ok_or_else(|| bad("未找到分发链起点 cmp r6,#0"))? * 4;
    let f1_start = words.iter().enumerate()
        .find(|(i, &w)| w == 0xE3A0_2015 && i * 4 > v_start)
        .ok_or_else(|| bad("未找到 .Ldc_emit"))?.0 * 4;
    if f1_start / 4 + 1 >= words.len() {
        return Err(bad("emit 后缺 bl"));
    }
    let mtat = dec_branch(words[f1_start / 4 + 1], f1_start + 4);
    let mdt = words.iter().enumerate()
        .find(|(i, &w)| w == 0xE92D_4FF0 && i * 4 > mtat)
        .ok_or_else(|| bad("未找到 menu_draw_text"))?.0 * 4;
    let bx = words.iter().enumerate()
        .find(|(i, &w)| w == 0xE12F_FF1E && i * 4 > mdt)
        .ok_or_else(|| bad("未找到 menu_draw_text 收尾"))?.0 * 4;
    let n_start = bx + 4;
    let mut f2_start = None;
    for i in mdt / 4..bx / 4 {
        let w = words[i];
        if w & 0xFFFF_F000 == 0xE28F_9000 {
            f2_start = Some(i * 4 + 8 + dec_imm(w) as usize);
            break;
        }
    }
    let f2_start = f2_start.filter(|&f| f >= n_start && f <= menu.len()).ok_or_else(|| bad("未找到 menu_index"))?;
    Ok(MenuParts { v_start, f1_start, n_start, f2_start })
}

/// 解码 menu.text 分发链→名字块→名字串，返回 idx 顺序的相机显示名
/// （对齐 管理滤镜.py verify_dispatch 的映射部分；默认项 = 分发链缺失的 idx）。
pub fn decode_menu_names(menu: &[u8], n: usize) -> Result<Vec<String>, CameraError> {
    let bad = |m: String| CameraError::BadInput(m);
    let p = split_menu(menu)?;
    let words: Vec<u32> = menu
        .chunks_exact(4)
        .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
        .collect();
    let vw = &words[p.v_start / 4..p.f1_start / 4];
    let mut dispatch: std::collections::HashMap<usize, usize> = std::collections::HashMap::new();
    let mut i = 0;
    while i < vw.len() && vw[i] & 0xFFFF_F000 == 0xE356_0000 {
        let idx = (vw[i] & 0xFFF) as usize;
        if idx >= n || i + 1 >= vw.len() {
            return Err(bad(format!("分发链 idx{idx} 越界或缺 beq")));
        }
        let beq = vw[i + 1];
        if beq & 0xFF00_0000 != 0x0A00_0000 {
            return Err(bad(format!("idx{idx} 非 beq")));
        }
        dispatch.insert(idx, dec_branch(beq, p.v_start + (i + 1) * 4));
        i += 2;
    }
    let default_block = p.v_start + i * 4;
    let mut blocks: std::collections::HashMap<usize, String> = std::collections::HashMap::new();
    let mut off = default_block;
    while off < p.f1_start {
        if off + 8 > p.f1_start {
            return Err(bad(format!("名字块 {off:#x} 截断")));
        }
        let adr = le32(menu, off);
        let mov = le32(menu, off + 4);
        if adr & 0xFFFF_F000 != 0xE28F_0000 {
            return Err(bad(format!("名字块 {off:#x} 非 adr r0")));
        }
        let tgt = off + 8 + dec_imm(adr) as usize;
        if tgt < p.n_start || tgt >= p.f2_start {
            return Err(bad(format!("名字块 adr 目标 {tgt:#x} 不在 N 区")));
        }
        let end = menu[tgt..p.f2_start].iter().position(|&b| b == 0).map(|x| x + tgt)
            .ok_or_else(|| bad("名字串无 NUL 收尾".into()))?;
        let nm = std::str::from_utf8(&menu[tgt..end]).map_err(|_| bad("名字串非 UTF-8".into()))?.to_string();
        if mov & 0xFFFF_F000 != 0xE3A0_1000 {
            return Err(bad(format!("名字块 {off:#x} 非 mov r1,#imm")));
        }
        let remaining = p.f1_start - off;
        if remaining == 8 {
            // 末块直落 emit（无 b，汇编优化）
        } else if remaining >= 12 {
            let b = le32(menu, off + 8);
            if dec_branch(b, off + 8) != p.f1_start {
                return Err(bad(format!("名字块 {off:#x} b 目标非 emit")));
            }
        } else {
            return Err(bad(format!("名字块 {off:#x} 长度异常 remaining={remaining}")));
        }
        blocks.insert(off, nm);
        off += if remaining == 8 { 8 } else { 12 };
    }
    if off != p.f1_start {
        return Err(bad("名字块未填满 V 区".into()));
    }
    let missing: Vec<usize> = (0..n).filter(|idx| !dispatch.contains_key(idx)).collect();
    if missing.len() != 1 {
        return Err(bad(format!("默认项缺失或多于一个：{missing:?}")));
    }
    let mut out = Vec::with_capacity(n);
    for idx in 0..n {
        let blk = if idx == missing[0] { default_block } else { dispatch[&idx] };
        out.push(blocks.get(&blk).ok_or_else(|| bad(format!("idx{idx} 指向无效块 {blk:#x}")))?.clone());
    }
    Ok(out)
}

/// 解析 ph_menu：段 + STR 清单 + 预装档 + LOG_HDR magic。
/// 名字以 menu N 分发链解码为准（相机实际显示名）；解码失败回退 STR 路径反推名。
pub fn parse_ph_menu(
    data: &[u8],
) -> Result<(Vec<Segment>, Vec<SlotEntry>, usize, Option<u32>), CameraError> {
    let segs = parse_records(data)?;
    let strbody = get_seg(&segs, STR_BASE)
        .ok_or_else(|| CameraError::BadInput("ph_menu 缺 STR 段".into()))?;
    let mut entries = parse_str(strbody)?;
    if let Some(menu) = get_seg(&segs, MENU_BASE) {
        if let Ok(names) = decode_menu_names(menu, entries.len()) {
            for (e, nm) in entries.iter_mut().zip(names) {
                e.name = nm;
            }
        }
    }
    let preload = get_seg(&segs, CORE_BASE).and_then(read_preload).unwrap_or(0);
    let magic = get_seg(&segs, LOG_HDR_BASE)
        .filter(|p| p.len() >= 16)
        .map(|p| le32(p, 12));
    Ok((segs, entries, preload, magic))
}

/// 完整五槽校验：大小 / 槽数 / 槽地址 / 每槽表体尺寸。
pub fn parse_package(data: &[u8]) -> Result<Vec<(u32, Vec<u8>)>, CameraError> {
    if data.len() != PKG_SIZE {
        return Err(CameraError::SizeMismatch { expected: PKG_SIZE, actual: data.len() });
    }
    let mut pos = 0usize;
    let mut slots = Vec::new();
    while pos < data.len() {
        let addr = le32(data, pos);
        let size = le32(data, pos + 4) as usize;
        pos += 8;
        if size != SLOT_SIZE {
            return Err(CameraError::BadInput(format!("槽表体 {size}B ≠ {SLOT_SIZE}B")));
        }
        slots.push((addr, data[pos..pos + size].to_vec()));
        pos += size;
    }
    if slots.len() != SLOT_COUNT {
        return Err(CameraError::BadInput(format!("槽数 {} ≠ {SLOT_COUNT}", slots.len())));
    }
    let addrs: Vec<u32> = slots.iter().map(|(a, _)| *a).collect();
    if addrs != SLOT_ADDRS {
        return Err(CameraError::BadInput("槽地址与标准不符".into()));
    }
    Ok(slots)
}

/// 提取 (slot1_fp, slot5_fp)，与只读参考同逻辑（FP_OFFSET）。
pub fn package_fingerprints(data: &[u8]) -> Result<(u32, u32), CameraError> {
    let slots = parse_package(data)?;
    let fp1 = le32(&slots[1].1, FP_OFFSET);
    let fp5 = le32(&slots[4].1, FP_OFFSET);
    Ok((fp1, fp5))
}

/// 只读探测模拟卷根：ph_menu.bin + DXO_FILTERS/*.bin，交叉校验指纹与五槽。
pub fn probe(root: &Path) -> Result<CardProfile, CameraError> {
    let ph = fs::read(root.join("ph_menu.bin"))
        .map_err(|_| CameraError::BadInput("模拟卷缺 ph_menu.bin".into()))?;
    let (segs, entries, preload, magic) = parse_ph_menu(&ph)?;
    let tag = magic.and_then(build_tag);
    let known = tag.is_some();
    let mut issues = Vec::new();
    let n = entries.len();
    if n == 0 || n > max_slots() {
        issues.push(format!("档数 {n} 超出 1..{}", max_slots()));
    }
    if let Some(menu) = get_seg(&segs, MENU_BASE) {
        if decode_menu_names(menu, n).is_err() {
            issues.push("menu 名字解码失败：卡清单回退路径反推名（与相机显示不一致）".into());
        }
    }
    if let Some(core) = get_seg(&segs, CORE_BASE) {
        if core_layout(core).is_none() {
            issues.push("core 布局不认识：预装档按 0 显示".into());
        }
    }

    let mut packages = Vec::new();
    let filters = root.join("DXO_FILTERS");
    for e in &entries {
        let file = e.path.rsplit(['\\', '/']).next().unwrap_or("").to_string();
        let ppath = filters.join(&file);
        let mut info = PackageInfo {
            file: file.clone(),
            size: 0,
            slot_count: 0,
            fp1: 0,
            fp5: 0,
            valid: false,
            error: None,
        };
        match fs::read(&ppath) {
            Ok(data) => {
                info.size = data.len();
                match parse_package(&data) {
                    Ok(slots) => {
                        info.slot_count = slots.len();
                        match package_fingerprints(&data) {
                            Ok((f1, f5)) => {
                                info.fp1 = f1;
                                info.fp5 = f5;
                                if f1 != e.fp1 || f5 != e.fp5 {
                                    info.error = Some("包指纹与 ph_menu STR 不符".into());
                                    issues.push(format!("{file}: 指纹不符"));
                                } else {
                                    info.valid = true;
                                }
                            }
                            Err(err) => info.error = Some(err.to_string()),
                        }
                    }
                    Err(err) => {
                        info.error = Some(err.to_string());
                        issues.push(format!("{file}: {err}"));
                    }
                }
            }
            Err(_) => {
                info.error = Some("包文件缺失".into());
                issues.push(format!("{file}: 缺失"));
            }
        }
        packages.push(info);
    }

    Ok(CardProfile {
        build_tag: tag.map(|s| s.to_string()),
        known_firmware: known,
        read_only: !known,
        n_slots: n,
        max_slots: max_slots(),
        preload,
        entries,
        packages,
        issues,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seg(addr: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&addr.to_le_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn str_body(entries: &[(String, u32, u32)]) -> Vec<u8> {
        let n = entries.len();
        let mut v = vec![0u8; n * (PATH_STRIDE + 8)];
        for (i, (path, f1, f5)) in entries.iter().enumerate() {
            v[i * PATH_STRIDE..i * PATH_STRIDE + path.len()].copy_from_slice(path.as_bytes());
            let off = n * PATH_STRIDE + i * 8;
            v[off..off + 4].copy_from_slice(&f1.to_le_bytes());
            v[off + 4..off + 8].copy_from_slice(&f5.to_le_bytes());
        }
        v
    }

    fn package(fp1: u32, fp5: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for (i, &addr) in SLOT_ADDRS.iter().enumerate() {
            v.extend_from_slice(&addr.to_le_bytes());
            v.extend_from_slice(&(SLOT_SIZE as u32).to_le_bytes());
            let mut body = vec![0u8; SLOT_SIZE];
            let fp = if i == 1 { fp1 } else if i == 4 { fp5 } else { 0 };
            body[FP_OFFSET..FP_OFFSET + 4].copy_from_slice(&fp.to_le_bytes());
            v.extend_from_slice(&body);
        }
        v
    }

    fn ph_menu(entries: &[(String, u32, u32)], magic: u32) -> Vec<u8> {
        let names: Vec<String> = entries
            .iter()
            .map(|(p, _, _)| {
                p.rsplit(['\\', '/']).next().unwrap()
                    .trim_start_matches("lut_").trim_end_matches(".bin").to_uppercase()
            })
            .collect();
        ph_menu_named(entries, magic, &names)
    }

    fn ph_menu_named(entries: &[(String, u32, u32)], magic: u32, names: &[String]) -> Vec<u8> {
        let n = entries.len();
        let mut core = vec![0u8; C_PRELOAD_MOV_69 + 8];
        core[C_BOOT_CMP_69..C_BOOT_CMP_69 + 4]
            .copy_from_slice(&(0xE350_0000 | n as u32 - 1).to_le_bytes());
        core[C_PRELOAD_MOV_69..C_PRELOAD_MOV_69 + 4]
            .copy_from_slice(&0xE3A0_0002u32.to_le_bytes());
        let mut log = vec![0u8; 16];
        log[12..16].copy_from_slice(&magic.to_le_bytes());
        let mut v = Vec::new();
        v.extend(seg(MENU_BASE, &build_menu(names, n.saturating_sub(1))));
        v.extend(seg(CORE_BASE, &core));
        v.extend(seg(LOG_HDR_BASE, &log));
        v.extend(seg(STR_BASE, &str_body(entries)));
        v
    }

    fn branch(op: u32, src: usize, dst: usize) -> u32 {
        let d = (dst as i64 - src as i64 - 8) >> 2;
        op | ((d as u32) & 0x00FF_FFFF)
    }

    /// 合成 menu.text：P filler + V 分发链/名字块（默认块在前、其余倒序）+ F1/N/F2 签名。
    fn build_menu(names: &[String], default_idx: usize) -> Vec<u8> {
        let n = names.len();
        let mut nbody = Vec::new();
        let mut str_off = Vec::new();
        for nm in names {
            str_off.push(nbody.len());
            nbody.extend_from_slice(nm.as_bytes());
            nbody.push(0);
            while nbody.len() % 4 != 0 { nbody.push(0); }
        }
        nbody.extend_from_slice(b"WAIT\0");
        while nbody.len() % 4 != 0 { nbody.push(0); }
        let v_start = 8;
        let disp: Vec<usize> = (0..n).filter(|&i| i != default_idx).collect();
        let default_block = v_start + disp.len() * 8;
        let order: Vec<usize> = std::iter::once(default_idx).chain(disp.iter().rev().copied()).collect();
        let mut block_off = Vec::new();
        let mut off = default_block;
        for k in 0..order.len() {
            block_off.push(off);
            off += if k == order.len() - 1 { 8 } else { 12 };
        }
        let f1_start = off;
        let mdt = f1_start + 16;
        let bx = mdt + 8;
        let n_start = bx + 4;
        let f2_start = n_start + nbody.len();
        let mut w: Vec<u32> = vec![0xE1A0_0000; 2];
        for (k, &idx) in disp.iter().enumerate() {
            let src = v_start + k * 8;
            let dst = block_off[order.iter().position(|&x| x == idx).unwrap()];
            w.push(0xE356_0000 | idx as u32);
            w.push(branch(0x0A00_0000, src + 4, dst));
        }
        for (k, &idx) in order.iter().enumerate() {
            let boff = block_off[k];
            let tgt = n_start + str_off[idx];
            w.push(0xE28F_0000 | (tgt - boff - 8) as u32);
            w.push(0xE3A0_1000 | names[idx].len() as u32);
            if k != order.len() - 1 {
                w.push(branch(0xEA00_0000, boff + 8, f1_start));
            }
        }
        assert_eq!(w.len() * 4, f1_start);
        w.push(0xE3A0_2015);
        w.push(branch(0xEB00_0000, f1_start + 4, f1_start + 8));
        w.push(0xE1A0_0000);
        w.push(0xE1A0_0000);
        assert_eq!(w.len() * 4, mdt);
        w.push(0xE92D_4FF0);
        w.push(0xE28F_9000 | (f2_start - (mdt + 4) - 8) as u32);
        w.push(0xE12F_FF1E);
        assert_eq!(w.len() * 4, n_start);
        let mut menu = Vec::new();
        for x in &w { menu.extend_from_slice(&x.to_le_bytes()); }
        menu.extend_from_slice(&nbody);
        menu.extend_from_slice(&[0u8; 8]);
        menu
    }

    #[test]
    fn test_decode_menu_names_default_and_block_order() {
        let names: Vec<String> = ["FUYUNO", "K12SAT", "GOLD", "WARM"].iter().map(|s| s.to_string()).collect();
        // 固件签名：分发链首条恒 cmp r6,#0，默认项恒 n-1（与 管理滤镜.py 规范布局一致）
        for default_idx in 1..names.len() {
            let menu = build_menu(&names, default_idx);
            assert_eq!(decode_menu_names(&menu, names.len()).unwrap(), names, "default={default_idx}");
        }
    }

    #[test]
    fn test_decode_menu_names_rejects_corrupt() {
        let names = vec!["A".to_string(), "B".to_string()];
        let mut menu = build_menu(&names, 1);
        let default_block = 8 + 8;
        menu[default_block..default_block + 4].copy_from_slice(&0xE1A0_0000u32.to_le_bytes());
        assert!(decode_menu_names(&menu, 2).is_err());
        assert!(decode_menu_names(&menu[..menu.len() - 4], 2).is_err());
    }

    #[test]
    fn test_read_preload_layout_69_vs_72() {
        let mut c69 = vec![0u8; C_PRELOAD_MOV_69 + 8];
        c69[C_BOOT_CMP_69..C_BOOT_CMP_69 + 4].copy_from_slice(&0xE350_0005u32.to_le_bytes());
        c69[C_PRELOAD_MOV_69..C_PRELOAD_MOV_69 + 4].copy_from_slice(&0xE3A0_0003u32.to_le_bytes());
        assert_eq!(core_layout(&c69), Some("69"));
        assert_eq!(read_preload(&c69), Some(3));
        let mut c72 = vec![0u8; C_PRELOAD_MOV_72 + 8];
        c72[C_BOOT_CMP_69..C_BOOT_CMP_69 + 4].copy_from_slice(&0xE340_08EFu32.to_le_bytes());
        c72[C_PRELOAD_MOV_72..C_PRELOAD_MOV_72 + 4].copy_from_slice(&0xE3A0_0007u32.to_le_bytes());
        assert_eq!(core_layout(&c72), Some("72"));
        assert_eq!(read_preload(&c72), Some(7));
        assert_eq!(core_layout(&vec![0u8; 0x400]), None);
        assert_eq!(read_preload(&vec![0u8; 0x400]), None);
    }

    #[test]
    fn test_parse_ph_menu_names_follow_menu_not_path() {
        let entries = vec![
            ("c:\\DXO_FILTERS\\lut_alpha.bin".to_string(), 1, 2),
            ("c:\\DXO_FILTERS\\lut_beta.bin".to_string(), 3, 4),
        ];
        let names = vec!["GOLD".to_string(), "WARM".to_string()];
        let ph = ph_menu_named(&entries, 0x5047_3547, &names);
        let (_segs, got, preload, _magic) = parse_ph_menu(&ph).unwrap();
        assert_eq!(got.iter().map(|e| e.name.clone()).collect::<Vec<_>>(), names);
        assert_eq!(got[0].path, entries[0].0);
        assert_eq!(preload, 2);
    }

    fn sim_volume(entries: &[(String, u32, u32)], magic: u32) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        fs::write(dir.path().join("ph_menu.bin"), ph_menu(entries, magic)).unwrap();
        fs::create_dir_all(dir.path().join("DXO_FILTERS")).unwrap();
        for (path, f1, f5) in entries {
            let file = path.rsplit('\\').next().unwrap();
            fs::write(dir.path().join("DXO_FILTERS").join(file), package(*f1, *f5)).unwrap();
        }
        dir
    }

    #[test]
    fn test_max_slots_derived_not_hardcoded() {
        assert_eq!(max_slots(), 12);
        assert_eq!(max_slots(), (DURATIONS_BASE - STR_BASE) as usize / (PATH_STRIDE + 8));
    }

    #[test]
    fn test_build_tag_and_unknown_readonly() {
        assert_eq!(build_tag(0x5047_3546), Some("71"));
        assert_eq!(build_tag(0x5047_3547), Some("72"));
        assert_eq!(build_tag(0x5047_3544), Some("69"));
        assert_eq!(build_tag(0xDEAD_BEEF), None);
        let entries = vec![("c:\\DXO_FILTERS\\lut_a.bin".to_string(), 1, 2)];
        let dir = sim_volume(&entries, 0xDEAD_BEEF);
        let p = probe(dir.path()).unwrap();
        assert!(!p.known_firmware);
        assert!(p.read_only, "未知固件必须只读");
    }

    #[test]
    fn test_probe_build71_profile_and_five_slot() {
        let entries: Vec<(String, u32, u32)> = (0..11)
            .map(|i| (format!("c:\\DXO_FILTERS\\lut_f{i}.bin"), 0x1000 + i, 0x2000 + i))
            .collect();
        let dir = sim_volume(&entries, 0x5047_3546);
        let p = probe(dir.path()).unwrap();
        assert_eq!(p.build_tag.as_deref(), Some("71"));
        assert!(p.known_firmware);
        assert_eq!(p.n_slots, 11);
        assert_eq!(p.max_slots, 12);
        assert!(p.issues.is_empty(), "{:?}", p.issues);
        assert!(p.packages.iter().all(|pk| pk.valid && pk.slot_count == 5));
        assert_eq!(p.packages[1].fp1, 0x1001);
    }

    #[test]
    fn test_probe_detects_fingerprint_mismatch() {
        let entries = vec![("c:\\DXO_FILTERS\\lut_a.bin".to_string(), 1, 2)];
        let dir = sim_volume(&entries, 0x5047_3546);
        // 破坏包指纹
        let mut bad = package(9, 9);
        bad[8 + FP_OFFSET..8 + FP_OFFSET + 4].copy_from_slice(&9u32.to_le_bytes());
        fs::write(dir.path().join("DXO_FILTERS/lut_a.bin"), bad).unwrap();
        let p = probe(dir.path()).unwrap();
        assert!(!p.packages[0].valid);
        assert!(!p.issues.is_empty());
    }

    #[test]
    fn test_parse_package_rejects_bad_layout() {
        let good = package(1, 2);
        assert!(parse_package(&good).is_ok());
        assert!(parse_package(&good[..good.len() - 1]).is_err());
        let mut bad_addr = good.clone();
        bad_addr[0..4].copy_from_slice(&0xDEADu32.to_le_bytes());
        assert!(parse_package(&bad_addr).is_err());
        let mut bad_size = good.clone();
        bad_size[4..8].copy_from_slice(&(SLOT_SIZE as u32 - 1).to_le_bytes());
        assert!(parse_package(&bad_size).is_err());
    }

    #[test]
    fn test_parse_str_rejects_non_multiple() {
        assert!(parse_str(&[0u8; 41]).is_err());
        assert!(parse_str(&[0u8; 40]).is_ok());
    }
}