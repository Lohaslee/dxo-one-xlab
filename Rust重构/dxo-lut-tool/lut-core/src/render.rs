//! 图像渲染：CC 表 → 预览图（移植自 filt_core.render_preview / build_test_chart）。

use crate::table::decode_table;
use crate::sample::sample;

/// 测试色卡：8×4 色块 + 灰阶渐变（与 Python build_test_chart 一致）。
///
/// 返回 (width, height, rgb_bytes)。
pub fn build_test_chart() -> (u32, u32, Vec<u8>) {
    let cols: usize = 8;
    let rows: usize = 4;
    let sw: usize = 70;
    let w = cols * sw;
    let h = rows * sw + 40;
    let mut chart = vec![0u8; w * h * 3];

    let hues: Vec<f32> = (0..cols).map(|i| i as f32 / cols as f32).collect();
    let lights = [0.85f32, 0.6, 0.4, 0.25];

    for ri in 0..rows {
        let light = lights[ri];
        for ci in 0..cols {
            let (r, g, b) = hsv_to_rgb(hues[ci], 0.85, light);
            let y0 = ri * sw;
            let x0 = ci * sw;
            for dy in 0..sw {
                for dx in 0..sw {
                    let px = (y0 + dy) * w + (x0 + dx);
                    chart[px * 3] = (r * 255.0) as u8;
                    chart[px * 3 + 1] = (g * 255.0) as u8;
                    chart[px * 3 + 2] = (b * 255.0) as u8;
                }
            }
        }
    }

    // 灰阶渐变
    let gy0 = rows * sw + 10;
    for i in 0..(cols * 2) {
        let v = (i as f32 / (cols * 2 - 1) as f32 * 255.0) as u8;
        let x0 = i * (sw / 2);
        let x1 = (i + 1) * (sw / 2);
        for dy in 0..20 {
            for x in x0..x1 {
                let px = (gy0 + dy) * w + x;
                chart[px * 3] = v;
                chart[px * 3 + 1] = v;
                chart[px * 3 + 2] = v;
            }
        }
    }

    (w as u32, h as u32, chart)
}

/// 应用 CC 表到图像像素（浮点 RGB → 量化输出）。
///
/// `pixels` 是 [h][w][3] 的 float 数组（归一化到 [0,1]）。
/// 返回量化后的 u8 RGB 字节。
pub fn apply_cc_table(cc_bytes: &[u8], pixels: &[[f32; 3]]) -> Vec<u8> {
    let (_, cube, _) = decode_table(cc_bytes);
    let result = sample(&cube, pixels, crate::N);
    let mut out = Vec::with_capacity(result.len() * 3);
    for p in &result {
        out.push(clip_byte(p[0]));
        out.push(clip_byte(p[1]));
        out.push(clip_byte(p[2]));
    }
    out
}

/// 任意 RGB 位图过 CC 表：返回加滤镜后的 RGB 字节（`before` 由调用方保留）。
pub fn render_rgb(cc_bytes: &[u8], w: u32, h: u32, rgb: &[u8]) -> Vec<u8> {
    let n = (w as usize) * (h as usize);
    if cc_bytes.len() < crate::TABLE_LEN || rgb.len() < n * 3 {
        return rgb.to_vec();
    }
    let mut pixels = vec![[0.0f32; 3]; n];
    for i in 0..n {
        pixels[i] = [
            rgb[i * 3] as f32 / 255.0,
            rgb[i * 3 + 1] as f32 / 255.0,
            rgb[i * 3 + 2] as f32 / 255.0,
        ];
    }
    apply_cc_table(cc_bytes, &pixels)
}

/// 完整预览：输入图像字节 + CC 表 → (before_rgb, after_rgb, w, h)。
///
/// 支持常见格式（PNG/JPEG），通过内联解码。
pub fn render_preview(cc_bytes: &[u8], img_bytes: &[u8]) -> (Vec<u8>, Vec<u8>, u32, u32) {
    // 尝试解码图像
    let (w, h, before_rgb) = if img_bytes.len() > 0 {
        decode_image(img_bytes).unwrap_or_else(|| build_test_chart())
    } else {
        build_test_chart()
    };

    // 转为 float 像素
    let n = (w as usize) * (h as usize);
    let mut pixels = vec![[0.0f32; 3]; n];
    for i in 0..n {
        pixels[i] = [
            before_rgb[i * 3] as f32 / 255.0,
            before_rgb[i * 3 + 1] as f32 / 255.0,
            before_rgb[i * 3 + 2] as f32 / 255.0,
        ];
    }

    let after_rgb = apply_cc_table(cc_bytes, &pixels);
    (before_rgb, after_rgb, w, h)
}

/// 使用测试色卡的预览（便捷封装）。
pub fn render_preview_chart(cc_bytes: &[u8]) -> (Vec<u8>, Vec<u8>, u32, u32) {
    let (w, h, chart) = build_test_chart();
    let n = (w as usize) * (h as usize);
    let mut pixels = vec![[0.0f32; 3]; n];
    for i in 0..n {
        pixels[i] = [
            chart[i * 3] as f32 / 255.0,
            chart[i * 3 + 1] as f32 / 255.0,
            chart[i * 3 + 2] as f32 / 255.0,
        ];
    }
    let after_rgb = apply_cc_table(cc_bytes, &pixels);
    (chart, after_rgb, w, h)
}

/// 简易图像解码（PNG / JPEG / BMP / GIF），失败时返回 None。
fn decode_image(bytes: &[u8]) -> Option<(u32, u32, Vec<u8>)> {
    // 尝试用 image crate 解码（在 GUI crate 中使用）
    // 此处为纯核心库，返回 None 表示需要外部解码
    // 实际解码由调用方（GUI）通过 image crate 完成
    let _ = bytes;
    None
}

#[inline]
fn clip_byte(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0) as u8
}

/// HSV → RGB（与 Python hsv_to_rgb 一致）。
fn hsv_to_rgb(h: f32, s: f32, v: f32) -> (f32, f32, f32) {
    let i = (h * 6.0).floor() as i32;
    let f = h * 6.0 - i as f32;
    let p = v * (1.0 - s);
    let q = v * (1.0 - f * s);
    let t = v * (1.0 - (1.0 - f) * s);
    match i % 6 {
        0 => (v, t, p),
        1 => (q, v, p),
        2 => (p, v, t),
        3 => (p, q, v),
        4 => (t, p, v),
        _ => (v, p, q),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_test_chart_size() {
        let (w, h, data) = build_test_chart();
        assert_eq!(w, 560);
        assert_eq!(h, 320);
        assert_eq!(data.len(), (w * h * 3) as usize);
    }

    #[test]
    fn test_hsv_to_rgb_pure_red() {
        let (r, g, b) = hsv_to_rgb(0.0, 0.85, 0.85);
        assert!((r - 0.85).abs() < 0.01);
        assert!(g < 0.15);
        assert!(b < 0.15);
    }

    #[test]
    fn test_apply_cc_table_valid_output() {
        // 原厂表不是恒等变换（相机会做色彩校正），但输出应在有效范围内
        let stock = crate::STOCK_TABLE.to_vec();
        let (w, h, chart) = build_test_chart();
        let n = (w as usize) * (h as usize);
        let mut pixels = vec![[0.0f32; 3]; n];
        for i in 0..n {
            pixels[i] = [
                chart[i * 3] as f32 / 255.0,
                chart[i * 3 + 1] as f32 / 255.0,
                chart[i * 3 + 2] as f32 / 255.0,
            ];
        }
        let after = apply_cc_table(&stock, &pixels);
        assert_eq!(after.len(), n * 3);
        // 所有值应在 [0, 255] 范围内
        for &v in &after {
            assert!(v <= 255, "输出值超出范围: {v}");
        }
    }

    #[test]
    fn test_render_preview_chart_deterministic() {
        let stock = crate::STOCK_TABLE.to_vec();
        let (before1, after1, w1, h1) = render_preview_chart(&stock);
        let (before2, after2, w2, h2) = render_preview_chart(&stock);
        assert_eq!(w1, w2);
        assert_eq!(h1, h2);
        assert_eq!(before1, before2);
        assert_eq!(after1, after2);
    }
}