//! DXO-ONE-XLAB · 核心库（Rust 移植）
//!
//! 纯计算模块：CC 表编解码、三线性插值、LUT 复合、.cube 解析。
//! 不依赖平台特定代码；原厂基线编译期打包，可选由 `DXO_STOCK_FILE` 覆盖。

pub const N: usize = 16;

/// 原厂 CC 基线（编译期打包，干净账户无需项目目录）。
/// 来源：固件镜像/romfs/section_2/Imx183_Cc_Still1.bin，17536 B。
pub const STOCK_TABLE: &[u8] = include_bytes!("../resources/Imx183_Cc_Still1.bin");
pub const STOCK_TABLE_SHA256: &str =
    "284ec88189184c6a725ef3aa528bbfdb78b1c586947d452d4f331d1b2b82e33c";

pub mod table;
pub mod sample;
pub mod compose;
pub mod cube;
pub mod formats;
pub mod xmp;
pub mod render;

// --- 常量 ---------------------------------------------------------------
pub const BODY_OFF: usize = 0x80;
pub const BODY_LEN: usize = N * N * N * 4; // 16384
pub const TABLE_LEN: usize = 17536;
pub const MAXV: f32 = 1023.0;

pub const SLOT_BASE: u32 = 0x092D72E8;
pub const SLOT_SIZE: usize = 0x4480; // 17536
pub const SLOT_COUNT: usize = 5;
pub const MENU_SLOTS: usize = 5;
pub const CAMERA_CUSTOM_MAX: usize = 5;

// --- 便捷函数 -----------------------------------------------------------

/// 读取原厂 CC 基线：默认用打包资源，`DXO_STOCK_FILE` 可显式指定替换表。
///
/// 替换表会做长度校验，但不会自动宣称与原厂基线等价；调用方应自行核对 SHA。
pub fn load_stock_bytes() -> Result<Vec<u8>, String> {
    let bytes = match std::env::var_os("DXO_STOCK_FILE") {
        Some(p) => std::fs::read(&p)
            .map_err(|e| format!("读原厂表 {} 失败: {e}", std::path::Path::new(&p).display()))?,
        None => STOCK_TABLE.to_vec(),
    };
    if bytes.len() != TABLE_LEN {
        return Err(format!(
            "原厂表长度 {} B ≠ {} B（基线 SHA256 {}）",
            bytes.len(), TABLE_LEN, STOCK_TABLE_SHA256
        ));
    }
    Ok(bytes)
}

/// 解码相机 CC 表文件字节。
pub fn decode_table_bytes(data: &[u8]) -> ([u8; 128], [[[[f32; 3]; N]; N]; N], [u8; 1024]) {
    table::decode_table(data)
}

/// 编码 CC 表为字节。
pub fn encode_table_bytes(header: &[u8; 128], cube: &[[[[f32; 3]; N]; N]; N], tail: &[u8; 1024]) -> Vec<u8> {
    table::encode_table(header, cube, tail)
}

/// 读 .cube 文件 -> (size, CubeLUT)。
pub fn read_cube(path: &str) -> Result<(usize, cube::CubeLUT), String> {
    cube::read_cube(path)
}

/// 三线性插值（便捷封装）。
pub fn sample_lut(lut: &[[[[f32; 3]; N]; N]; N], coords: &[[f32; 3]], size: usize) -> Vec<[f32; 3]> {
    sample::sample(lut, coords, size)
}

/// 主复合：已加载 LUT -> CC 表字节。
///
/// 双层混合（与 Python filt_core.compose_with_strength 完全一致）：
///   base  = 原厂 CC 输出（无滤镜）
///   full  = compose(lut, stock_cube, tone, strength=1.0)  — 满套滤镜输出
///   new  = base * (1 - strength) + full * strength
pub fn compose_with_strength(
    lut: Option<&cube::CubeLUT>,
    stock_cube: &[[[[f32; 3]; N]; N]; N],
    tone: &str,
    strength: f32,
    desaturate: bool,
    header: &[u8; 128],
    tail: &[u8; 1024],
) -> Vec<u8> {
    let lut_arr = lut.map(|l| l.values);
    let lut_size = lut.map_or(N, |l| l.size);

    // base = 原厂 CC 输出
    let grid = sample::build_grid();
    let flat_grid = flatten_grid(&grid);
    let base_flat = sample::sample(stock_cube, &flat_grid, N);
    let base = unflatten(&base_flat);

    let w = strength.clamp(0.0, 1.0);

    let new_cube = if lut_arr.is_none() {
        base
    } else {
        let full_pass = compose::compose(lut_arr.as_ref(), stock_cube, "pass", 1.0, false, lut_size);
        let full_neutral = compose::compose(lut_arr.as_ref(), stock_cube, "neutral", 1.0, false, lut_size);

        let full = match tone {
            "neutral" => full_neutral,
            "blend" => blend_cubes(&full_pass, &full_neutral, 0.5),
            _ => full_pass,
        };

        blend_cubes(&base, &full, w)
    };

    // bw 灰度化
    let mut result = new_cube;
    if desaturate {
        for r in 0..N {
            for g in 0..N {
                for bl in 0..N {
                    let y = 0.299 * result[r][g][bl][0]
                        + 0.587 * result[r][g][bl][1]
                        + 0.114 * result[r][g][bl][2];
                    result[r][g][bl] = [y, y, y];
                }
            }
        }
    }

    table::encode_table(header, &result, tail)
}

/// 纯 LOOK 预览合成（GUI 预览专用，语义与导出用的 compose_with_strength 不同）：
/// 不含原厂 CC 底色——
///   base = 恒等表（原图原样）
///   full = LOOK 直接作用于原图（tone 决定是否灰轴中性化）
///   new  = base * (1 - strength) + full * strength
/// 输出仍为 17536B CC 表字节（header/tail 沿用调用方提供值），供 render_rgb 渲染预览。
/// 相机实拍/导出表仍走 compose_with_strength（含原厂 CC），预览观感与之存在差异。
pub fn compose_pure_look_with_strength(
    lut: Option<&cube::CubeLUT>,
    tone: &str,
    strength: f32,
    desaturate: bool,
    header: &[u8; 128],
    tail: &[u8; 1024],
) -> Vec<u8> {
    let base = sample::build_grid();

    let w = strength.clamp(0.0, 1.0);

    let new_cube = if let Some(l) = lut {
        let lut_size = l.size;
        let full_pass = compose::compose(Some(&l.values), &base, "pass", 1.0, false, lut_size);
        let full_neutral = compose::compose(Some(&l.values), &base, "neutral", 1.0, false, lut_size);

        let full = match tone {
            "neutral" => full_neutral,
            "blend" => blend_cubes(&full_pass, &full_neutral, 0.5),
            _ => full_pass,
        };

        blend_cubes(&base, &full, w)
    } else {
        base
    };

    // bw 灰度化
    let mut result = new_cube;
    if desaturate {
        for r in 0..N {
            for g in 0..N {
                for bl in 0..N {
                    let y = 0.299 * result[r][g][bl][0]
                        + 0.587 * result[r][g][bl][1]
                        + 0.114 * result[r][g][bl][2];
                    result[r][g][bl] = [y, y, y];
                }
            }
        }
    }

    table::encode_table(header, &result, tail)
}

/// 两个 cube 逐元素线性混合：c = a * (1 - t) + b * t
fn blend_cubes(
    av: &[[[[f32; 3]; N]; N]; N],
    bv: &[[[[f32; 3]; N]; N]; N],
    t: f32,
) -> [[[[f32; 3]; N]; N]; N] {
    let mut out = [[[[0.0f32; 3]; N]; N]; N];
    let u = 1.0 - t;
    for r in 0..N {
        for g in 0..N {
            for bl in 0..N {
                for c in 0..3 {
                    out[r][g][bl][c] = av[r][g][bl][c] * u + bv[r][g][bl][c] * t;
                }
            }
        }
    }
    out
}

fn flatten_grid(grid: &[[[[f32; 3]; N]; N]; N]) -> Vec<[f32; 3]> {
    let mut v = Vec::with_capacity(N * N * N);
    for r in 0..N {
        for g in 0..N {
            for b in 0..N {
                v.push(grid[r][g][b]);
            }
        }
    }
    v
}

fn unflatten(v: &[[f32; 3]]) -> [[[[f32; 3]; N]; N]; N] {
    let mut out = [[[[0.0f32; 3]; N]; N]; N];
    let mut idx = 0;
    for r in 0..N {
        for g in 0..N {
            for b in 0..N {
                out[r][g][b] = v[idx];
                idx += 1;
            }
        }
    }
    out
}

/// 铺满五槽打包。
pub fn pack_five_slots(single_slot_bytes: &[u8]) -> Vec<u8> {
    assert_eq!(single_slot_bytes.len(), SLOT_SIZE, "不是 17536B 单槽镜像");
    let mut out = Vec::with_capacity(8 * SLOT_COUNT + TABLE_LEN * SLOT_COUNT);
    for i in 0..SLOT_COUNT {
        let addr = SLOT_BASE + (SLOT_SIZE as u32) * i as u32;
        out.extend_from_slice(&addr.to_le_bytes());
        out.extend_from_slice(&(SLOT_SIZE as u32).to_le_bytes());
        out.extend_from_slice(single_slot_bytes);
    }
    out
}