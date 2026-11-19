//! .cube LUT 文件解析。
//!
//! 格式：标准 .cube 格式，支持 TITLE / LUT_3D_SIZE / DOMAIN_MIN / DOMAIN_MAX /
//! LUT_3D_INPUT_RANGE 头字段。数据行顺序 index = b*size² + g*size + r。
//!
//! 相机 CC 表固定为 16³ 网格，因此任意尺寸的 .cube（17³ / 33³ / 65³ …）都会被
//! 三线性重采样到 16³ 内部网格。原始尺寸记录在 `CubeLUT::source_size`。

use crate::N;

#[derive(Clone, Debug)]
pub struct CubeLUT {
    /// 内部网格固定为 16³
    pub values: [[[[f32; 3]; N]; N]; N],
    pub domain_min: [f32; 3],
    pub domain_max: [f32; 3],
    /// 内部网格尺寸（恒为 16）
    pub size: usize,
    /// .cube 文件里声明的原始尺寸
    pub source_size: usize,
}

/// 读取 .cube 文件 -> (内部网格尺寸, CubeLUT)。
pub fn read_cube(path: &str) -> Result<(usize, CubeLUT), String> {
    let (src_size, vals, domain_min, domain_max) = parse_cube_raw(path)?;

    let values = if src_size == N {
        // 尺寸一致，直接铺入
        let mut v = [[[[0.0f32; 3]; N]; N]; N];
        for (idx, px) in vals.iter().enumerate() {
            let b = idx / (src_size * src_size);
            let g = (idx / src_size) % src_size;
            let r = idx % src_size;
            v[r][g][b] = *px;
        }
        v
    } else {
        resample_to_grid(src_size, &vals, domain_min, domain_max)
    };

    Ok((
        N,
        CubeLUT {
            values,
            domain_min,
            domain_max,
            size: N,
            source_size: src_size,
        },
    ))
}

/// 已加载 LUT 的二进制序列化 magic（用于本地库里保存「可重算」的源 LUT）。
const CUBE_MAGIC: &[u8; 8] = b"DXOCUBE1";

/// 序列化后的固定长度：magic(8) + size(4) + source_size(4) + domain(24) + 网格(49152)
pub const CUBE_BLOB_LEN: usize = 8 + 4 + 4 + 24 + N * N * N * 3 * 4;

impl CubeLUT {
    /// 序列化为字节（供本地库持久化，之后可重新复合不同 tone/strength）。
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(CUBE_BLOB_LEN);
        out.extend_from_slice(CUBE_MAGIC);
        out.extend_from_slice(&(self.size as u32).to_le_bytes());
        out.extend_from_slice(&(self.source_size as u32).to_le_bytes());
        for c in self.domain_min.iter().chain(self.domain_max.iter()) {
            out.extend_from_slice(&c.to_le_bytes());
        }
        for r in 0..N {
            for g in 0..N {
                for b in 0..N {
                    for c in 0..3 {
                        out.extend_from_slice(&self.values[r][g][b][c].to_le_bytes());
                    }
                }
            }
        }
        out
    }

    /// 从字节还原；格式不符返回 None。
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() != CUBE_BLOB_LEN || &data[0..8] != CUBE_MAGIC {
            return None;
        }
        let rd_u32 = |o: usize| u32::from_le_bytes(data[o..o + 4].try_into().unwrap()) as usize;
        let rd_f32 = |o: usize| f32::from_le_bytes(data[o..o + 4].try_into().unwrap());

        let size = rd_u32(8);
        if size != N {
            return None;
        }
        let source_size = rd_u32(12);
        let mut domain_min = [0.0f32; 3];
        let mut domain_max = [0.0f32; 3];
        for i in 0..3 {
            domain_min[i] = rd_f32(16 + i * 4);
            domain_max[i] = rd_f32(28 + i * 4);
        }

        let mut values = [[[[0.0f32; 3]; N]; N]; N];
        let mut o = 40;
        for r in 0..N {
            for g in 0..N {
                for b in 0..N {
                    for c in 0..3 {
                        values[r][g][b][c] = rd_f32(o);
                        o += 4;
                    }
                }
            }
        }

        Some(CubeLUT {
            values,
            domain_min,
            domain_max,
            size,
            source_size: source_size.max(2),
        })
    }
}


/// 从任意尺寸的 flat 源网格构造 16³ CubeLUT（domain 0..1）。
///
/// 供 formats.rs（3DL/CSP/Hald）复用 read_cube 的铺入/重采样路径。
/// `src` 布局：index = b*size² + g*size + r。
pub fn cube_lut_from_flat(size: usize, src: &[[f32; 3]]) -> Result<CubeLUT, String> {
    if size < 2 {
        return Err("网格尺寸至少为 2".into());
    }
    if src.len() != size * size * size {
        return Err(format!("网格数据 {} ≠ {}³", src.len(), size));
    }
    let domain_min = [0.0f32; 3];
    let domain_max = [1.0f32; 3];
    let values = if size == N {
        let mut v = [[[[0.0f32; 3]; N]; N]; N];
        for (idx, px) in src.iter().enumerate() {
            let b = idx / (size * size);
            let g = (idx / size) % size;
            let r = idx % size;
            v[r][g][b] = *px;
        }
        v
    } else {
        resample_to_grid(size, src, domain_min, domain_max)
    };
    Ok(CubeLUT {
        values,
        domain_min,
        domain_max,
        size: N,
        source_size: size,
    })
}

/// 把任意尺寸的 LUT 数据重采样到 16³ 内部网格。
///
/// `src` 为原始顺序 index = b*size² + g*size + r。
fn resample_to_grid(
    size: usize,
    src: &[[f32; 3]],
    domain_min: [f32; 3],
    domain_max: [f32; 3],
) -> [[[[f32; 3]; N]; N]; N] {
    let mut out = [[[[0.0f32; 3]; N]; N]; N];
    let denom = (N - 1) as f32;
    let span = [
        domain_max[0] - domain_min[0],
        domain_max[1] - domain_min[1],
        domain_max[2] - domain_min[2],
    ];

    for ri in 0..N {
        for gi in 0..N {
            for bi in 0..N {
                // 目标节点在 [0,1] 输入空间的位置
                let n = [
                    ri as f32 / denom,
                    gi as f32 / denom,
                    bi as f32 / denom,
                ];
                // 映射到源 LUT 的输入域，再落到 [0, size-1] 网格
                let mut c = [0.0f32; 3];
                for k in 0..3 {
                    let t = if span[k].abs() > f32::EPSILON {
                        (n[k] - domain_min[k]) / span[k]
                    } else {
                        n[k]
                    };
                    c[k] = t.clamp(0.0, 1.0) * (size - 1) as f32;
                }

                out[ri][gi][bi] = trilinear_at(size, src, c);
            }
        }
    }
    out
}

#[inline]
fn src_at(size: usize, src: &[[f32; 3]], r: usize, g: usize, b: usize) -> [f32; 3] {
    let idx = b * size * size + g * size + r;
    src[idx]
}

#[inline]
fn lerp3(a: [f32; 3], b: [f32; 3], f: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// 在源 LUT 上按网格坐标 `c`（已映射到 [0, size-1]）做三线性插值。
fn trilinear_at(size: usize, src: &[[f32; 3]], c: [f32; 3]) -> [f32; 3] {
    let last = size - 1;
    let x = c[0];
    let y = c[1];
    let z = c[2];

    let i0r = (x.floor() as isize).clamp(0, last as isize) as usize;
    let i0g = (y.floor() as isize).clamp(0, last as isize) as usize;
    let i0b = (z.floor() as isize).clamp(0, last as isize) as usize;
    let i1r = (i0r + 1).min(last);
    let i1g = (i0g + 1).min(last);
    let i1b = (i0b + 1).min(last);

    let fr = x - i0r as f32;
    let fg = y - i0g as f32;
    let fb = z - i0b as f32;

    let c00 = lerp3(src_at(size, src, i0r, i0g, i0b), src_at(size, src, i0r, i0g, i1b), fb);
    let c01 = lerp3(src_at(size, src, i0r, i1g, i0b), src_at(size, src, i0r, i1g, i1b), fb);
    let c10 = lerp3(src_at(size, src, i1r, i0g, i0b), src_at(size, src, i1r, i0g, i1b), fb);
    let c11 = lerp3(src_at(size, src, i1r, i1g, i0b), src_at(size, src, i1r, i1g, i1b), fb);

    let c0 = lerp3(c00, c01, fg);
    let c1 = lerp3(c10, c11, fg);
    lerp3(c0, c1, fr)
}

/// 解析 .cube 文本 -> (size, 原始顺序 RGB 数据, domain_min, domain_max)。
fn parse_cube_raw(
    path: &str,
) -> Result<(usize, Vec<[f32; 3]>, [f32; 3], [f32; 3]), String> {
    let content = std::fs::read_to_string(path)
        .map_err(|e| format!("读取 .cube 失败: {}", e))?;

    let mut size: Option<usize> = None;
    let mut domain_min = [0.0f32; 3];
    let mut domain_max = [1.0f32; 3];
    let mut seen: Vec<String> = Vec::new();
    let mut vals: Vec<[f32; 3]> = Vec::new();

    for (lineno, line) in content.lines().enumerate() {
        let lineno = lineno + 1;
        // 去注释
        let s = if let Some(pos) = line.find('#') {
            &line[..pos]
        } else {
            line
        };
        let s = s.trim();
        if s.is_empty() {
            continue;
        }

        let parts: Vec<&str> = s.split_whitespace().collect();
        let key = parts[0].to_uppercase();

        match key.as_str() {
            "TITLE" => {
                if !vals.is_empty() || seen.contains(&key) {
                    return Err(format!("第 {} 行：重复或位于数据后的头字段 TITLE", lineno));
                }
                seen.push(key);
                if parts.len() < 2 {
                    return Err(format!("第 {} 行：TITLE 为空", lineno));
                }
            }
            "LUT_3D_SIZE" => {
                if !vals.is_empty() || seen.contains(&key) {
                    return Err(format!("第 {} 行：重复或位于数据后的头字段 LUT_3D_SIZE", lineno));
                }
                seen.push(key);
                if parts.len() != 2 {
                    return Err("LUT_3D_SIZE 必须有一个整数".to_string());
                }
                let n = parts[1]
                    .parse::<usize>()
                    .map_err(|_| "LUT_3D_SIZE 不是整数".to_string())?;
                if !(2..=256).contains(&n) {
                    return Err(format!("LUT_3D_SIZE 超出支持范围 (2..=256): {n}"));
                }
                size = Some(n);
            }
            "DOMAIN_MIN" | "DOMAIN_MAX" | "LUT_3D_INPUT_RANGE" => {
                if !vals.is_empty() || seen.contains(&key) {
                    return Err(format!("第 {} 行：重复或位于数据后的头字段 {}", lineno, key));
                }
                seen.push(key.clone());
                let count = if key == "LUT_3D_INPUT_RANGE" { 2 } else { 3 };
                if parts.len() != count + 1 {
                    return Err(format!("{} 需要 {} 个数值", key, count));
                }
                let v: Vec<f32> = parts[1..]
                    .iter()
                    .map(|s| s.parse::<f32>().map_err(|_| format!("{} 包含非数值", key)))
                    .collect::<Result<Vec<f32>, _>>()?;
                if key == "LUT_3D_INPUT_RANGE" {
                    domain_min = [v[0], v[0], v[0]];
                    domain_max = [v[1], v[1], v[1]];
                } else if key == "DOMAIN_MIN" {
                    domain_min = [v[0], v[1], v[2]];
                } else {
                    domain_max = [v[0], v[1], v[2]];
                }
            }
            _ => {
                if key.starts_with("LUT_") {
                    return Err(format!("不支持的 LUT 头字段 {}", key));
                }
                if size.is_none() || parts.len() != 3 {
                    return Err(format!("第 {} 行：缺少尺寸或不是 RGB 数据", lineno));
                }
                let rgb: Vec<f32> = parts
                    .iter()
                    .map(|s| s.parse::<f32>().map_err(|_| format!("第 {} 行：非法 RGB 数据", lineno)))
                    .collect::<Result<Vec<f32>, _>>()?;
                vals.push([rgb[0], rgb[1], rgb[2]]);
            }
        }
    }

    let size = size.ok_or("没找到 LUT_3D_SIZE")?;

    // 检查 LUT_3D_INPUT_RANGE 与 DOMAIN_MIN/MAX 不可混用
    if seen.contains(&"LUT_3D_INPUT_RANGE".to_string())
        && (seen.contains(&"DOMAIN_MIN".to_string()) || seen.contains(&"DOMAIN_MAX".to_string()))
    {
        return Err("不可混用 LUT_3D_INPUT_RANGE 与 DOMAIN_MIN/MAX".to_string());
    }

    if domain_max[0] <= domain_min[0]
        || domain_max[1] <= domain_min[1]
        || domain_max[2] <= domain_min[2]
    {
        return Err("输入域上限必须大于下限".to_string());
    }

    if vals.len() != size * size * size {
        return Err(format!("RGB 数据行数 {} ≠ {}^3", vals.len(), size));
    }

    Ok((size, vals, domain_min, domain_max))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// 33³ 恒等 LUT 重采样到 16³ 后应当仍是恒等映射
    #[test]
    fn test_resample_33_identity() {
        let size = 33usize;
        let mut vals = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    vals.push([
                        r as f32 / (size - 1) as f32,
                        g as f32 / (size - 1) as f32,
                        b as f32 / (size - 1) as f32,
                    ]);
                }
            }
        }

        let grid = resample_to_grid(size, &vals, [0.0; 3], [1.0; 3]);
        let denom = (N - 1) as f32;
        let mut max_err = 0.0f32;
        for r in 0..N {
            for g in 0..N {
                for b in 0..N {
                    let want = [r as f32 / denom, g as f32 / denom, b as f32 / denom];
                    for k in 0..3 {
                        let e = (grid[r][g][b][k] - want[k]).abs();
                        if e > max_err {
                            max_err = e;
                        }
                    }
                }
            }
        }
        assert!(max_err < 1e-5, "33³ 恒等 LUT 重采样误差过大: {max_err}");
    }

    /// 17³ 恒等 LUT 重采样后也应恒等（非整除尺寸）
    #[test]
    fn test_resample_17_identity() {
        let size = 17usize;
        let mut vals = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    vals.push([
                        r as f32 / (size - 1) as f32,
                        g as f32 / (size - 1) as f32,
                        b as f32 / (size - 1) as f32,
                    ]);
                }
            }
        }

        let grid = resample_to_grid(size, &vals, [0.0; 3], [1.0; 3]);
        let denom = (N - 1) as f32;
        let mut max_err = 0.0f32;
        for r in 0..N {
            for g in 0..N {
                for b in 0..N {
                    let want = [r as f32 / denom, g as f32 / denom, b as f32 / denom];
                    for k in 0..3 {
                        let e = (grid[r][g][b][k] - want[k]).abs();
                        if e > max_err {
                            max_err = e;
                        }
                    }
                }
            }
        }
        assert!(max_err < 1e-3, "17³ 恒等 LUT 重采样误差过大: {max_err}");
    }

    /// DOMAIN 输入域应被正确映射
    #[test]
    fn test_resample_domain() {
        let size = 33usize;
        // 源 LUT 输入域 [0, 0.5]，输出恒等于输入的 2 倍（即归一化后恒等）
        let mut vals = Vec::with_capacity(size * size * size);
        for b in 0..size {
            for g in 0..size {
                for r in 0..size {
                    vals.push([
                        r as f32 / (size - 1) as f32,
                        g as f32 / (size - 1) as f32,
                        b as f32 / (size - 1) as f32,
                    ]);
                }
            }
        }
        let grid = resample_to_grid(size, &vals, [0.0; 3], [0.5; 3]);
        // 输入 n 在域 [0, 0.5] 内被拉伸 2 倍并截断到 1
        // 节点 ri 的 n = ri/15，期望输出 = min(n*2, 1)
        for ri in 0..N {
            let n = ri as f32 / (N - 1) as f32;
            let want = (n * 2.0).min(1.0);
            let got = grid[ri][ri][ri][0];
            assert!(
                (got - want).abs() < 1e-3,
                "DOMAIN 映射错误 @ ri={ri}: 期望 {want}, 实际 {got}"
            );
        }
    }
}
