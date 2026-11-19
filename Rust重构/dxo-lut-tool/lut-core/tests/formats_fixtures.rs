//! 3DL/CSP/Hald 解析器与 Python 真源（filt_core.py）的交叉验证。
//!
//! fixtures 由 golden/gen_format_fixtures.py 生成：期望值是
//! filt_core.parse_* 输出（bake 后源网格），已转为 Rust b 主序。

use lut_core::formats::{grid_to_cube, hald_from_pixels, parse_3dl_text, parse_csp_text};

fn read_fixture(name: &str) -> (usize, Vec<[f32; 3]>) {
    let text = std::fs::read_to_string(format!("{}/../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR")))
        .expect("fixture 缺失：先跑 golden/gen_format_fixtures.py");
    let mut lines = text.lines();
    let head: Vec<usize> = lines
        .next()
        .unwrap()
        .split(',')
        .map(|s| s.parse().unwrap())
        .collect();
    let (size, count) = (head[0], head[1]);
    let mut vals = vec![[0.0f32; 3]; count];
    for ln in lines {
        let p: Vec<&str> = ln.split(',').collect();
        let i: usize = p[0].parse().unwrap();
        vals[i] = [
            p[1].parse().unwrap(),
            p[2].parse().unwrap(),
            p[3].parse().unwrap(),
        ];
    }
    (size, vals)
}

fn fixture_path(name: &str) -> String {
    format!("{}/../tests/fixtures/{name}", env!("CARGO_MANIFEST_DIR"))
}

fn compare(name: &str, got: &[([f32; 3], [f32; 3])]) {
    let mut max_err = 0.0f32;
    let mut over = 0usize;
    for (a, b) in got {
        for c in 0..3 {
            let e = (a[c] - b[c]).abs();
            if e > 2.0 / 255.0 {
                over += 1;
            }
            max_err = max_err.max(e);
        }
    }
    assert!(
        over == 0,
        "{name}: {over} 个节点超出 2/255 容差（max_err={max_err}）"
    );
    println!("{name}: max_err={max_err:.6}");
}

#[test]
fn parse_3dl_matches_python() {
    let src = std::fs::read_to_string(fixture_path("fixture.3dl")).unwrap();
    let g = parse_3dl_text(&src).expect("3DL 解析失败");
    let (size, exp) = read_fixture("expected_3dl.csv");
    assert_eq!(g.size, size);
    assert_eq!(g.vals.len(), exp.len());
    let pairs: Vec<_> = g.vals.iter().zip(exp.iter()).map(|(a, b)| (*a, *b)).collect();
    compare("3dl", &pairs);
}

#[test]
fn parse_csp_matches_python() {
    let src = std::fs::read_to_string(fixture_path("fixture.csp")).unwrap();
    let g = parse_csp_text(&src).expect("CSP 解析失败");
    let (size, exp) = read_fixture("expected_csp.csv");
    assert_eq!(g.size, size);
    assert_eq!(g.vals.len(), exp.len());
    let pairs: Vec<_> = g.vals.iter().zip(exp.iter()).map(|(a, b)| (*a, *b)).collect();
    compare("csp", &pairs);
}

#[test]
fn hald_matches_python() {
    // 用 image crate 解码 PNG（dxo-gui 已有该依赖；此处经 lut-core 之外的
    // 测试依赖可用 workspace 统一解析——见 Cargo.toml dev-dependencies）。
    let img = image::open(fixture_path("fixture_hald8.png")).expect("PNG 打开失败");
    let rgb = img.to_rgb8();
    let (w, h) = (rgb.width(), rgb.height());
    let g = hald_from_pixels(w, h, rgb.as_raw()).expect("Hald 解析失败");
    let (size, exp) = read_fixture("expected_hald.csv");
    assert_eq!(g.size, size);
    assert_eq!(g.vals.len(), exp.len());
    let pairs: Vec<_> = g.vals.iter().zip(exp.iter()).map(|(a, b)| (*a, *b)).collect();
    compare("hald", &pairs);
}

#[test]
fn hald_rejects_non_square_and_non_perfect_square() {
    assert!(hald_from_pixels(64, 48, &[0u8; 64 * 48 * 3]).is_err());
    // 512 不是完全平方数 -> 拒（与 Python 行为一致）
    assert!(hald_from_pixels(512, 512, &[0u8; 512 * 512 * 3]).is_err());
}

#[test]
fn grid_to_cube_resamples_to_16() {
    let g = parse_3dl_text(&std::fs::read_to_string(fixture_path("fixture.3dl")).unwrap()).unwrap();
    let cube = grid_to_cube(g).unwrap();
    assert_eq!(cube.size, 16);
    assert_eq!(cube.source_size, 4);
    assert_eq!(cube.to_bytes().len(), lut_core::cube::CUBE_BLOB_LEN);
}
