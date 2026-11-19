//! 离线 py↔Rust 逐字节交叉验证入口（只读模板 / 只写 /tmp 输出，不接触相机卡）。
//!
//! 用法：
//!   rebuild_cross --synth <out.bin> [69|72] [name,name,...]
//!   rebuild_cross <template.bin> <entries.json> <out.bin> [preload|-] [magic_hex|-] [0|1]
//! entries.json: [{"name","card_path","fp1","fp5"}]；preload `-` = 沿用模板；magic `-` = 沿用模板。

use std::path::Path;

use lut_camera::rebuild::{self, RebuildEntry};

fn fail(m: String) -> ! {
    eprintln!("ERR {m}");
    std::process::exit(1);
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.first().map(|s| s.as_str()) == Some("--synth") {
        if args.len() < 2 {
            fail("--synth 需要输出路径".into());
        }
        let layout = args.get(2).map(|s| s.as_str()).unwrap_or("72");
        let names: Vec<String> = args
            .get(3)
            .map(|s| s.split(',').map(|x| x.to_string()).collect())
            .unwrap_or_else(|| vec!["ORIG".into(), "GOL2".into(), "SATOBI".into()]);
        let data = rebuild::synth::synth_ph_menu(&names, layout, 0).unwrap_or_else(|e| fail(e.to_string()));
        std::fs::write(&args[1], &data).unwrap_or_else(|e| fail(e.to_string()));
        println!("OK synth len={} layout={layout} names={names:?}", data.len());
        return;
    }
    if args.len() < 3 {
        fail("参数不足".into());
    }
    let template = std::fs::read(&args[0]).unwrap_or_else(|e| fail(e.to_string()));
    let entries: Vec<RebuildEntry> =
        serde_json::from_str(&std::fs::read_to_string(&args[1]).unwrap_or_else(|e| fail(e.to_string())))
            .unwrap_or_else(|e| fail(e.to_string()));
    let preload = match args.get(3).map(|s| s.as_str()) {
        None | Some("-") => None,
        Some(s) => Some(s.parse::<usize>().unwrap_or_else(|e| fail(e.to_string()))),
    };
    let magic = match args.get(4).map(|s| s.as_str()) {
        None | Some("-") => None,
        Some(s) => Some(u32::from_str_radix(s.trim_start_matches("0x"), 16).unwrap_or_else(|e| fail(e.to_string()))),
    };
    let canonical = args.get(5).map(|s| s == "1").unwrap_or(false);
    let out = rebuild::rebuild_ph_menu(&template, &entries, preload, canonical, magic)
        .unwrap_or_else(|e| fail(e.to_string()));
    let path = Path::new(&args[2]);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap_or_else(|e| fail(e.to_string()));
    }
    std::fs::write(path, &out).unwrap_or_else(|e| fail(e.to_string()));
    println!("OK rebuild len={} entries={}", out.len(), entries.len());
}