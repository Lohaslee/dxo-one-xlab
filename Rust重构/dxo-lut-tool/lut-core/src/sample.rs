//! 三线性插值 + 均匀网格生成。
//!
//! 与 Python/numpy 版本保持相同运算顺序；使用 f32 降低累积误差。

use crate::N;

/// 三线性插值。
///
/// `lut` 形状 [s][s][s][3]（实际存储于 [N][N][N][3]，前 s 项有效），
/// `coords` 形状 [n][3]，返回 [n][3]。
/// 坐标在 [0,1] 内被映射到 [0, s-1]。
pub fn sample(lut: &[[[[f32; 3]; N]; N]; N], coords: &[[f32; 3]], size: usize) -> Vec<[f32; 3]> {
    let s = size as f32;
    let n = coords.len();
    let mut out = vec![[0.0f32; 3]; n];
    let sm1 = size as i32 - 1;

    for p in 0..n {
        let c = coords[p];
        // clip 到 [0,1]
        let cx = c[0].clamp(0.0, 1.0);
        let cy = c[1].clamp(0.0, 1.0);
        let cz = c[2].clamp(0.0, 1.0);

        let x = cx * (s - 1.0);
        let y = cy * (s - 1.0);
        let z = cz * (s - 1.0);

        let i0_r = x.floor() as i32;
        let i0_g = y.floor() as i32;
        let i0_b = z.floor() as i32;
        let i1_r = (i0_r + 1).min(sm1);
        let i1_g = (i0_g + 1).min(sm1);
        let i1_b = (i0_b + 1).min(sm1);

        let fr = x - i0_r as f32;
        let fg = y - i0_g as f32;
        let fb = z - i0_b as f32;

        let r0 = i0_r as usize;
        let r1 = i1_r as usize;
        let g0 = i0_g as usize;
        let g1 = i1_g as usize;
        let b0 = i0_b as usize;
        let b1 = i1_b as usize;

        let at = |r: usize, g: usize, b: usize| -> [f32; 3] { lut[r][g][b] };

        // 三线性插值（与 numpy 版本相同顺序）
        // add(a,b,f) = a + (b-a)*f = (1-f)*a + f*b  即 lerp
        let c00 = add(at(r0, g0, b0), at(r0, g0, b1), fb);
        let c01 = add(at(r0, g1, b0), at(r0, g1, b1), fb);
        let c10 = add(at(r1, g0, b0), at(r1, g0, b1), fb);
        let c11 = add(at(r1, g1, b0), at(r1, g1, b1), fb);

        let c0 = add(c00, c01, fg);
        let c1 = add(c10, c11, fg);

        out[p] = add(c0, c1, fr);
    }
    out
}

#[inline]
fn add(a: [f32; 3], b: [f32; 3], f: f32) -> [f32; 3] {
    [
        a[0] + (b[0] - a[0]) * f,
        a[1] + (b[1] - a[1]) * f,
        a[2] + (b[2] - a[2]) * f,
    ]
}

/// 生成相机 16³ 均匀节点坐标网格。
///
/// 返回 [N][N][N][3] 坐标数组。
pub fn build_grid() -> [[[[f32; 3]; N]; N]; N] {
    let ax: [f32; N] = {
        let mut a = [0.0f32; N];
        for i in 0..N {
            a[i] = i as f32 / (N as f32 - 1.0);
        }
        a
    };

    let mut grid = [[[[0.0f32; 3]; N]; N]; N];
    for r in 0..N {
        for g in 0..N {
            for b in 0..N {
                grid[r][g][b] = [ax[r], ax[g], ax[b]];
            }
        }
    }
    grid
}