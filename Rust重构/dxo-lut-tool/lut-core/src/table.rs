//! DxO ONE 机内 CC 3D LUT 表的二进制编解码。
//!
//! 格式（17,536 字节 = 0x4480）：
//!   · 0x0000..0x007F  128 字节文件头（u32 magic=1, u32 version=1，其余 0）
//!   · 0x0080..0x4080  16,384 字节 = 4096 个节点 × 4 字节
//!     每节点：bits 0-9 = R，bits 10-19 = G，bits 20-29 = B（均 0..1023）
//!     索引 = R_idx*256 + G_idx*16 + B_idx   （B 变化最快）
//!   · 0x4080..0x4480  1024 字节尾部（原样继承）

use crate::N;

pub const BODY_OFF: usize = 0x80;
pub const BODY_LEN: usize = N * N * N * 4; // 16384
pub const TABLE_LEN: usize = 17536;
pub const MAXV: f32 = 1023.0;

/// 解码相机 CC 表 -> (header128, cube[16][16][16][3] 浮点0..1, tail1024)
pub fn decode_table(data: &[u8]) -> ([u8; 128], [[[[f32; 3]; N]; N]; N], [u8; 1024]) {
    assert_eq!(data.len(), TABLE_LEN, "长度不是 17536");

    let header: [u8; 128] = data[..BODY_OFF].try_into().unwrap();
    // 校验文件头
    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    assert_eq!(magic, 1, "CC 文件头 magic 不是 1");
    assert_eq!(version, 1, "CC 文件头 version 不是 1");

    let tail: [u8; 1024] = data[BODY_OFF + BODY_LEN..].try_into().unwrap();

    let mut cube = [[[[0.0f32; 3]; N]; N]; N];
    for r in 0..N {
        for g in 0..N {
            for b in 0..N {
                let idx = r * 256 + g * 16 + b;
                let off = BODY_OFF + 4 * idx;
                let w = u32::from_le_bytes([data[off], data[off + 1], data[off + 2], data[off + 3]]);
                cube[r][g][b][0] = ((w & 0x3FF) as f32) / MAXV;
                cube[r][g][b][1] = (((w >> 10) & 0x3FF) as f32) / MAXV;
                cube[r][g][b][2] = (((w >> 20) & 0x3FF) as f32) / MAXV;
            }
        }
    }
    (header, cube, tail)
}

/// 编码 CC 表。cube 形状必须为 [16][16][16][3]。
pub fn encode_table(header: &[u8; 128], cube: &[[[[f32; 3]; N]; N]; N], tail: &[u8; 1024]) -> Vec<u8> {
    // 校验文件头
    let magic = u32::from_le_bytes([header[0], header[1], header[2], header[3]]);
    let version = u32::from_le_bytes([header[4], header[5], header[6], header[7]]);
    assert_eq!(magic, 1, "不支持的 CC 文件头 magic");
    assert_eq!(version, 1, "不支持的 CC 文件头 version");

    let mut out: Vec<u8> = Vec::with_capacity(TABLE_LEN);
    out.extend_from_slice(header);

    for r in 0..N {
        for g in 0..N {
            for b in 0..N {
                let v = cube[r][g][b];
                // 逐通道 clip + round，与 numpy 行为对齐
                let q0 = (v[0].clamp(0.0, 1.0) * MAXV).round() as i64;
                let q1 = (v[1].clamp(0.0, 1.0) * MAXV).round() as i64;
                let q2 = (v[2].clamp(0.0, 1.0) * MAXV).round() as i64;
                let w: u32 = (q0 as u32) | ((q1 as u32) << 10) | ((q2 as u32) << 20);
                out.extend_from_slice(&w.to_le_bytes());
            }
        }
    }
    out.extend_from_slice(tail);
    assert_eq!(out.len(), TABLE_LEN, "编码后长度不是 17536");
    out
}