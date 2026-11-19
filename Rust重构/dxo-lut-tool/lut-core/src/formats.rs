//! 3DL / CSP / HaldCLUT 解析（与 Web 版 工具/filt_core.py 语义对齐）。
//!
//! 输出约定与 `cube.rs::resample_to_grid` 一致：flat 网格，
//! index = b*size² + g*size + r，值域 0..1。
//! 交叉验证 fixture：`tests/fixtures/`（由 golden/gen_format_fixtures.py 生成）。

/// 解析后的源网格（bake 完成，值域 0..1）。
pub struct ParsedGrid {
    pub size: usize,
    /// len = size³，index = b*size² + g*size + r
    pub vals: Vec<[f32; 3]>,
}

/// 1D 曲线（pos 均匀 0..1，val 为映射值）。
type Curve = Option<(Vec<f64>, Vec<f64>)>;

/// 仿 filt_core._guess_scale：按数据最大值猜归一化分母。
fn guess_scale(maxv: f32) -> f32 {
    if maxv <= 1.0001 {
        return 1.0;
    }
    for top in [255.0f32, 1023.0, 4095.0, 65535.0] {
        if maxv <= top + 1.0 {
            return top;
        }
    }
    maxv
}

/// np.interp 等价：线性插值，越界取端点值。
fn interp(x: f64, pos: &[f64], val: &[f64]) -> f64 {
    debug_assert_eq!(pos.len(), val.len());
    if x <= pos[0] {
        return val[0];
    }
    let last = pos.len() - 1;
    if x >= pos[last] {
        return val[last];
    }
    let mut lo = 0usize;
    while lo + 1 < pos.len() && pos[lo + 1] < x {
        lo += 1;
    }
    let hi = lo + 1;
    let t = (x - pos[lo]) / (pos[hi] - pos[lo]);
    val[lo] + (val[hi] - val[lo]) * t
}

/// 仿 sample_raw：源网格三线性插值。coords 为 f64 (r,g,b) 各 0..1。
/// 网格按 r 主序理解（与 filt_core reshape 一致）：at(r,g,b) 取 idx=r*s²+g*s+b。
fn sample_raw(vals: &[[f32; 3]], size: usize, coords: &[[f64; 3]]) -> Vec<[f64; 3]> {
    let s = size as f64;
    coords
        .iter()
        .map(|c| {
            let x = [
                c[0].clamp(0.0, 1.0) * (s - 1.0),
                c[1].clamp(0.0, 1.0) * (s - 1.0),
                c[2].clamp(0.0, 1.0) * (s - 1.0),
            ];
            let i0: [usize; 3] = [x[0].floor() as usize, x[1].floor() as usize, x[2].floor() as usize];
            let i1 = [
                (i0[0] + 1).min(size - 1),
                (i0[1] + 1).min(size - 1),
                (i0[2] + 1).min(size - 1),
            ];
            let f = [x[0] - i0[0] as f64, x[1] - i0[1] as f64, x[2] - i0[2] as f64];
            let at = |r: usize, g: usize, b: usize| -> [f64; 3] {
                let idx = r * size * size + g * size + b;
                [
                    vals[idx][0] as f64,
                    vals[idx][1] as f64,
                    vals[idx][2] as f64,
                ]
            };
            let mix = |a: [f64; 3], b: [f64; 3], t: f64| -> [f64; 3] {
                [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
            };
            let c00 = mix(at(i0[0], i0[1], i0[2]), at(i0[0], i0[1], i1[2]), f[2]);
            let c01 = mix(at(i0[0], i1[1], i0[2]), at(i0[0], i1[1], i1[2]), f[2]);
            let c10 = mix(at(i1[0], i0[1], i0[2]), at(i1[0], i0[1], i1[2]), f[2]);
            let c11 = mix(at(i1[0], i1[1], i0[2]), at(i1[0], i1[1], i1[2]), f[2]);
            let c0 = mix(c00, c01, f[1]);
            let c1 = mix(c10, c11, f[1]);
            mix(c0, c1, f[0])
        })
        .collect()
}

/// 仿 make_cubelut：对网格坐标先过 in 曲线，采样后输出过 out 曲线。
fn bake(
    vals: &[[f32; 3]],
    size: usize,
    curves_in: &[Curve; 3],
    curves_out: &[Curve; 3],
) -> Vec<[f32; 3]> {
    let has_curves = curves_in.iter().any(|c| c.is_some()) || curves_out.iter().any(|c| c.is_some());
    let n = size;
    if !has_curves {
        // 输入按 r 主序理解，直接转 b 主序输出。
        let mut out = vec![[0.0f32; 3]; n * n * n];
        for r in 0..n {
            for g in 0..n {
                for b in 0..n {
                    out[b * n * n + g * n + r] = vals[r * n * n + g * n + b];
                }
            }
        }
        return out;
    }
    // Python: ax = linspace(0,1,N); R,G,B = meshgrid(ax, ax, ax, indexing='ij')
    // -> coords[(r,g,b)] = (ax[r], ax[g], ax[b])。
    let ax = |i: usize| i as f64 / (n.max(2) - 1) as f64;
    let mut coords: Vec<[f64; 3]> = Vec::with_capacity(n * n * n);
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                coords.push([ax(r), ax(g), ax(b)]);
            }
        }
    }
    let mut cin = coords.clone();
    for c in 0..3 {
        if let Some((pos, val)) = &curves_in[c] {
            for co in cin.iter_mut() {
                co[c] = interp(co[c], pos, val);
            }
        }
    }
    let mut sampled = sample_raw(vals, size, &cin);
    for c in 0..3 {
        if let Some((pos, val)) = &curves_out[c] {
            for sm in sampled.iter_mut() {
                sm[c] = interp(sm[c], pos, val);
            }
        }
    }
    // sampled 顺序 = coords 顺序 = r 主序节点 (r,g,b)。
    // 输出统一转 b 主序（resample_to_grid 约定）：out[b*s²+g*s+r] = sampled[r*s²+g*s+b]。
    let mut out = vec![[0.0f32; 3]; n * n * n];
    for r in 0..n {
        for g in 0..n {
            for b in 0..n {
                out[b * n * n + g * n + r] = {
                    let k = r * n * n + g * n + b;
                    let v = sampled[k];
                    [v[0] as f32, v[1] as f32, v[2] as f32]
                };
            }
        }
    }
    out
}

/// 3DL 1D IN/OUT 行。
fn parse_curve_tokens(tokens: &[&str]) -> Vec<f64> {
    tokens[1..].iter().filter_map(|t| t.parse::<f64>().ok()).collect()
}

/// 解析 .3dl 文本。支持 LUT_3D_SIZE / MESH / IN / OUT / 数据行。
pub fn parse_3dl_text(text: &str) -> Result<ParsedGrid, String> {
    let mut size: Option<usize> = None;
    let mut in_buf: [Vec<f64>; 3] = [vec![], vec![], vec![]];
    let mut out_buf: [Vec<f64>; 3] = [vec![], vec![], vec![]];
    let mut data: Vec<[f32; 3]> = vec![];

    for ln in text.lines() {
        let s = ln.split('#').next().unwrap_or("").trim();
        if s.is_empty() {
            continue;
        }
        let p: Vec<&str> = s.split_whitespace().collect();
        let k = p[0].to_uppercase();
        match k.as_str() {
            "LUT_3D_SIZE" | "MESH" => {
                size = Some(
                    p.get(1)
                        .and_then(|v| v.parse().ok())
                        .ok_or("3DL 尺寸行缺少整数")?,
                );
            }
            "IN" if p.len() == 4 => {
                let vals = parse_curve_tokens(&p);
                for c in 0..3 {
                    if let Some(v) = vals.get(c) {
                        in_buf[c].push(*v);
                    }
                }
            }
            "OUT" if p.len() == 4 => {
                let vals = parse_curve_tokens(&p);
                for c in 0..3 {
                    if let Some(v) = vals.get(c) {
                        out_buf[c].push(*v);
                    }
                }
            }
            _ => {
                if p.len() == 3 {
                    let ok = p.iter().all(|t| t.parse::<f32>().is_ok());
                    if ok {
                        data.push([
                            p[0].parse().unwrap_or(0.0),
                            p[1].parse().unwrap_or(0.0),
                            p[2].parse().unwrap_or(0.0),
                        ]);
                    }
                }
            }
        }
    }

    let size = match size {
        Some(s) => s,
        None => {
            let n = (data.len() as f64).powf(1.0 / 3.0).round() as usize;
            if n * n * n != data.len() {
                return Err("找不到 3DL 尺寸且数据量不是立方体".into());
            }
            n
        }
    };
    if size < 2 || size > 128 {
        return Err(format!("3DL 尺寸 {size} 超出支持范围 2..=128"));
    }
    if data.len() != size * size * size {
        return Err(format!("3DL 数据行 {} ≠ {}³", data.len(), size));
    }

    let maxv = data.iter().fold(0.0f32, |m, v| m.max(v[0]).max(v[1]).max(v[2]));
    let scale = guess_scale(maxv);
    for v in data.iter_mut() {
        v[0] /= scale;
        v[1] /= scale;
        v[2] /= scale;
    }

    let mut curves_in: [Curve; 3] = [None, None, None];
    let mut curves_out: [Curve; 3] = [None, None, None];
    for c in 0..3 {
        if in_buf[c].len() == size {
            let pos: Vec<f64> = (0..size).map(|i| i as f64 / (size - 1) as f64).collect();
            curves_in[c] = Some((pos, in_buf[c].clone()));
        }
        if out_buf[c].len() == size {
            let pos: Vec<f64> = (0..size).map(|i| i as f64 / (size - 1) as f64).collect();
            curves_out[c] = Some((pos, out_buf[c].clone()));
        }
    }

    let vals = bake(&data, size, &curves_in, &curves_out);
    Ok(ParsedGrid { size, vals })
}

/// 解析 .csp 文本（CSPLUT V1xx + 1D 曲线 + 3D 网格）。
pub fn parse_csp_text(text: &str) -> Result<ParsedGrid, String> {
    let lines: Vec<&str> = text
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    if lines.is_empty() || !lines[0].to_uppercase().starts_with("CSPLUT") {
        return Err("不是 CSP 文件（首行应为 CSPLUTV1xx）".into());
    }
    let sizes: Vec<usize> = lines
        .get(1)
        .ok_or("CSP 缺少 1D 尺寸行")?
        .split_whitespace()
        .filter_map(|t| t.parse().ok())
        .collect();
    if sizes.len() != 3 {
        return Err("CSP 1D 尺寸行需 3 个整数".into());
    }
    let mut idx = 2usize;
    let mut curves_in: [Curve; 3] = [None, None, None];
    for c in 0..3 {
        if sizes[c] > 0 {
            if idx + sizes[c] > lines.len() {
                return Err(format!("CSP 通道 {c} 曲线数据不足"));
            }
            let vals: Vec<f64> = (0..sizes[c])
                .filter_map(|k| lines[idx + k].parse().ok())
                .collect();
            if vals.len() != sizes[c] {
                return Err(format!("CSP 通道 {c} 曲线含非数字"));
            }
            let pos: Vec<f64> = (0..sizes[c])
                .map(|i| i as f64 / (sizes[c] - 1).max(1) as f64)
                .collect();
            curves_in[c] = Some((pos, vals));
            idx += sizes[c];
        }
    }
    let td_size: usize = lines
        .get(idx)
        .and_then(|l| l.trim().parse().ok())
        .ok_or("CSP 缺少 3D 尺寸行")?;
    idx += 1;
    if td_size < 2 || td_size > 128 {
        return Err(format!("CSP 尺寸 {td_size} 超出支持范围 2..=128"));
    }
    let n3 = td_size * td_size * td_size;
    if idx + n3 > lines.len() {
        return Err(format!("CSP 3D 数据行不足：需 {n3} 行"));
    }
    let mut data: Vec<[f32; 3]> = Vec::with_capacity(n3);
    for ln in &lines[idx..idx + n3] {
        let t: Vec<f32> = ln
            .split_whitespace()
            .filter_map(|x| x.parse().ok())
            .take(3)
            .collect();
        if t.len() != 3 {
            return Err("CSP 数据行需 3 个数值".into());
        }
        data.push([t[0], t[1], t[2]]);
    }

    let maxv = data.iter().fold(0.0f32, |m, v| m.max(v[0]).max(v[1]).max(v[2]));
    let scale = guess_scale(maxv);
    for v in data.iter_mut() {
        v[0] /= scale;
        v[1] /= scale;
        v[2] /= scale;
    }

    let vals = bake(&data, td_size, &curves_in, &[None, None, None]);
    Ok(ParsedGrid { size: td_size, vals })
}

/// HaldCLUT 像素 -> 网格。`rgb8` 为 w*h*3 交错字节（RGB）。
///
/// 与 filt_core.parse_hald 一致：要求正方形且边长为完全平方数，
/// level = sqrt(边长)；index = y*W+x，W = level²。
pub fn hald_from_pixels(w: u32, h: u32, rgb8: &[u8]) -> Result<ParsedGrid, String> {
    if w != h {
        return Err("HaldCLUT 必须是正方形图像".into());
    }
    let edge = (w as f64).sqrt().round() as usize;
    if (edge * edge) as u32 != w {
        return Err(format!("边长 {w} 不是完全平方数（Hald level 应为整数）"));
    }
    if edge < 2 || edge > 64 {
        return Err(format!("Hald level {edge} 超出支持范围 2..=64"));
    }
    let n3 = edge * edge * edge;
    let big = (edge * edge) as usize;
    let mut vals = vec![[0.0f32; 3]; n3];
    // filt_core.parse_hald：values.reshape(-1,3)[k] = img[k//W, k%W]，
    // k 为 values 的 C 顺序 = r 主序（k = r*W + g*s + b）。
    // 输出转 b 主序。
    for r in 0..edge {
        for g in 0..edge {
            for b in 0..edge {
                let k = r * big + g * edge + b;
                let x = k % big;
                let y = k / big;
                let o = (y * big + x) * 3;
                if o + 2 >= rgb8.len() {
                    return Err("像素数据不足".into());
                }
                vals[b * edge * edge + g * edge + r] = [
                    rgb8[o] as f32 / 255.0,
                    rgb8[o + 1] as f32 / 255.0,
                    rgb8[o + 2] as f32 / 255.0,
                ];
            }
        }
    }
    Ok(ParsedGrid { size: edge, vals })
}

/// 源网格 -> 内部 16³ CubeLUT（domain 0..1）。
/// size == N 直接铺入；否则走 resample_to_grid。
pub fn grid_to_cube(g: ParsedGrid) -> Result<crate::cube::CubeLUT, String> {
    crate::cube::cube_lut_from_flat(g.size, &g.vals)
}
