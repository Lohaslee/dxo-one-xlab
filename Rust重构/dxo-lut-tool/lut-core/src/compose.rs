//! LUT 复合运算：neutral 反函数 + compose 主流程。
//!
//! 复合公式：新表(输入) = 目标LUT( 原厂表(输入) )
//! tone: pass 直接用 / neutral 逐通道预补偿灰轴 / blend 混合

use crate::N;
use crate::sample::{sample, build_grid};

/// 计算 LUT 灰轴响应的反函数表。
///
/// 给定目标亮度 t，返回应喂入的 x。
/// 这是有损的影调取消，不是 Log 转换。
///
/// 返回 (ti[1025], inv[1025])。
pub fn neutral_inverse(
    lut: &[[[[f32; 3]; N]; N]; N],
    lo: f32,
    hi: f32,
    lut_size: usize,
) -> (Vec<f32>, Vec<f32>) {
    let size = lut_size;
    let mut ax = vec![0.0f32; size];
    for i in 0..size {
        ax[i] = lo + (hi - lo) * (i as f32) / ((size - 1) as f32);
    }

    // 灰轴坐标 [size][3]
    let mut g = vec![[0.0f32; 3]; size];
    for i in 0..size {
        g[i] = [ax[i], ax[i], ax[i]];
    }

    let r = sample(lut, &g, size);

    // y = 0.299*r + 0.587*g + 0.114*b
    let mut y = vec![0.0f32; size];
    for i in 0..size {
        y[i] = 0.299 * r[i][0] + 0.587 * r[i][1] + 0.114 * r[i][2];
    }

    // 强制单调
    for i in 1..size {
        if y[i] <= y[i - 1] {
            y[i] = y[i - 1] + 1e-6;
        }
    }

    // ti = linspace(0, 1, 1025)
    let n_ti = 1025;
    let mut ti = vec![0.0f32; n_ti];
    let mut inv = vec![0.0f32; n_ti];
    for i in 0..n_ti {
        ti[i] = (i as f32) / ((n_ti - 1) as f32);
        inv[i] = linear_interp(ti[i], &y, &ax);
    }
    (ti, inv)
}

/// 一维线性插值（与 numpy.interp 一致）。
fn linear_interp(x: f32, xp: &[f32], fp: &[f32]) -> f32 {
    let n = xp.len();
    if x <= xp[0] {
        return fp[0];
    }
    if x >= xp[n - 1] {
        return fp[n - 1];
    }
    let mut lo = 0;
    let mut hi = n - 1;
    while hi - lo > 1 {
        let mid = (lo + hi) / 2;
        if xp[mid] <= x {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    let t = (x - xp[lo]) / (xp[hi] - xp[lo]);
    fp[lo] + (fp[hi] - fp[lo]) * t
}

/// 主复合函数。
///
/// tone: "pass" | "neutral" | "blend"
/// strength: 0..1（仅 blend 时有效）
/// desaturate: 是否在最末统一灰度化
/// lut_size: LUT 实际边长（stock_cube 恒为 N=16，外部 LUT 可能小于 16）
pub fn compose(
    lut: Option<&[[[[f32; 3]; N]; N]; N]>,
    stock_cube: &[[[[f32; 3]; N]; N]; N],
    tone: &str,
    strength: f32,
    desaturate: bool,
    lut_size: usize,
) -> [[[[f32; 3]; N]; N]; N] {
    let grid = build_grid();
    let mut mid = sample(stock_cube, &flatten_grid(&grid), N);

    if desaturate {
        for p in 0..mid.len() {
            let y = 0.299 * mid[p][0] + 0.587 * mid[p][1] + 0.114 * mid[p][2];
            mid[p] = [y, y, y];
        }
    }

    let lut = match lut {
        Some(l) => l,
        None => return unflatten(&mid),
    };

    let fed: Vec<[f32; 3]> = if tone == "neutral" || tone == "blend" {
        let lo = 0.0f32;
        let hi = 1.0f32;
        let (ti, inv) = neutral_inverse(lut, lo, hi, lut_size);

        let mut pre = vec![[0.0f32; 3]; mid.len()];
        for p in 0..mid.len() {
            // np.interp(mid, ti, inv) 逐通道独立插值
            pre[p][0] = linear_interp(mid[p][0], &ti, &inv);
            pre[p][1] = linear_interp(mid[p][1], &ti, &inv);
            pre[p][2] = linear_interp(mid[p][2], &ti, &inv);
        }

        let w = if tone == "neutral" { 1.0 } else { strength.clamp(0.0, 1.0) };
        let mut fed = vec![[0.0f32; 3]; mid.len()];
        for p in 0..mid.len() {
            fed[p][0] = mid[p][0] * (1.0 - w) + pre[p][0] * w;
            fed[p][1] = mid[p][1] * (1.0 - w) + pre[p][1] * w;
            fed[p][2] = mid[p][2] * (1.0 - w) + pre[p][2] * w;
        }
        fed
    } else {
        mid
    };

    let result = sample(lut, &fed, lut_size);
    unflatten(&result)
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