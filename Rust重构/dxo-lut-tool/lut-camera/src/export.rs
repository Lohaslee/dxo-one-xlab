//! 导出包：把内存库里的滤镜落到存储卡 `DXO_FILTERS/<名>/`。
//!
//! 一个滤镜目录含：
//!   · `<名>.bin`          铺满五槽的 87720 B 包（与相机 RAM 槽位同构）
//!   · `预览_原厂.jpg`     加滤镜前（内置色卡，CC 诊断模拟）
//!   · `预览_加滤镜.jpg`   加滤镜后（逐项用该项 CC 表渲染，非共享）
//!   · `manifest.json`     参数与内容指纹 + 算法版本（D06）
//! 顶层另写 `DXO_FILTERS/manifest.json`（导出顺序 + 是否含 ORIGINAL + 算法版本）。
//!
//! 预览为「源 LUT 外观 / 相机 CC 表诊断模拟」，非相机 DSP 实测；
//! 每张 `预览_加滤镜.jpg` 由该项 `cc` 渲染，manifest 以 `table_sha256` 关联表与预览。
//!
//! 全部写入走 [`crate::safe_write`]：备份 → 写 → fsync → 逐字节回读校验，
//! 且强制拒绝 `DCIM` / `touch_prefs`。

use crate::{safe_write, validate_safe_path, CameraError};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// 相机自定义档位上限。
pub const CUSTOM_MAX: usize = 5;
/// 单槽 CC 表长度。
pub const TABLE_LEN: usize = 17536;
/// 导出预览/算法版本（D06）：CC 表诊断模拟 v1。
pub const EXPORT_ALGO_VERSION: u32 = 1;
/// 预览模式标识（与 app 预览同源）。
pub const PREVIEW_MODE: &str = "cc-diagnostic";
const PREVIEW_JPEG_QUALITY: u8 = 88;
const PREVIEW_JPEG_MAX_W: u32 = 520;
/// 统一 manifest schema 版本（R4-02）：单一相对路径 + 逐文件 sha256/size 导出计划。
pub const MANIFEST_SCHEMA: u32 = 2;

/// 字节 SHA-256（hex），复用 lut-persist 实现，避免重复依赖。
fn sha256_hex(data: &[u8]) -> String {
    lut_persist::Store::hash_bytes(data)
}

/// 单滤镜在 `DXO_FILTERS` 内的相对路径——**唯一生成器**。
/// 写盘与 manifest 共用同一组路径，杜绝 D07（manifest 路径 ≠ 实际文件）。
pub struct ItemPaths {
    pub bin: String,
    pub before: String,
    pub after: String,
    pub manifest: String,
}

pub fn item_paths(safe: &str) -> ItemPaths {
    ItemPaths {
        bin: format!("{safe}/{safe}.bin"),
        before: format!("{safe}/预览_原厂.jpg"),
        after: format!("{safe}/预览_加滤镜.jpg"),
        manifest: format!("{safe}/manifest.json"),
    }
}

/// 导出计划里的一项文件记录（相对路径 + 内容指纹 + 字节数）。
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct RelFile {
    pub path: String,
    pub sha256: String,
    pub size: u64,
}

impl RelFile {
    fn of(path: &str, data: &[u8]) -> Self {
        RelFile {
            path: path.to_string(),
            sha256: sha256_hex(data),
            size: data.len() as u64,
        }
    }
}

/// 校验 manifest 内相对路径（D09 路径白名单）：
/// 必须相对、无 `..`/`.`/空分量、无反斜杠或盘符、不触碰保护目录。
pub fn validate_rel_path(p: &str) -> Result<(), CameraError> {
    if p.is_empty() || p.starts_with('/') || p.contains('\\') || p.contains(':') {
        return Err(CameraError::BadInput(format!("非法相对路径: {p}")));
    }
    for comp in p.split('/') {
        if comp.is_empty() || comp == "." || comp == ".." {
            return Err(CameraError::BadInput(format!("路径含非法分量: {p}")));
        }
        if crate::is_protected(Path::new(comp)) {
            return Err(CameraError::ProtectedPath(p.to_string()));
        }
    }
    Ok(())
}

/// 待导出的一项（借用调用方的内存数据）。
pub struct ExportItem<'a> {
    pub name: &'a str,
    pub src: &'a str,
    pub format: &'a str,
    pub tone: &'a str,
    pub strength: f32,
    pub bw: bool,
    pub locked: bool,
    pub source_size: i64,
    /// 单槽 17536 B CC 表
    pub cc: &'a [u8],
    /// 该表 SHA-256 hex
    pub table_sha256: &'a str,
}

pub struct ExportReport {
    pub written: Vec<PathBuf>,
    pub names: Vec<String>,
    pub root: PathBuf,
}

/// 卡上目录名：与相机菜单一致的 12 位大写字母数字（对齐 Python `sanitize_name`）。
pub fn card_safe_name(name: &str, used: &mut HashSet<String>) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase();
    if s.is_empty() {
        s = "LUT".into();
    }
    s.truncate(12);
    let base = s.clone();
    let mut i = 1;
    while used.contains(&s) {
        let keep: String = base.chars().take(10).collect();
        s = format!("{keep}{i}");
        i += 1;
    }
    used.insert(s.clone());
    s
}

/// RGB → JPEG（q88、限宽 520，与 app 预览同参数）。
fn encode_jpeg(w: u32, h: u32, rgb: &[u8]) -> Result<Vec<u8>, CameraError> {
    let img = image::RgbImage::from_raw(w, h, rgb.to_vec())
        .ok_or_else(|| CameraError::BadInput("预览像素尺寸不匹配".into()))?;
    let dynimg = image::DynamicImage::ImageRgb8(img);
    let dynimg = if dynimg.width() > PREVIEW_JPEG_MAX_W {
        let ratio = PREVIEW_JPEG_MAX_W as f32 / dynimg.width() as f32;
        let nh = (dynimg.height() as f32 * ratio).round().max(1.0) as u32;
        dynimg.resize_exact(PREVIEW_JPEG_MAX_W, nh, image::imageops::FilterType::Lanczos3)
    } else {
        dynimg
    };
    let mut buf = std::io::Cursor::new(Vec::new());
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, PREVIEW_JPEG_QUALITY);
    dynimg
        .write_with_encoder(enc)
        .map_err(|e| CameraError::BadInput(format!("JPEG 编码失败: {e}")))?;
    Ok(buf.into_inner())
}

/// 写入一组滤镜到 `dxo_dir`（应当是 `<卡>/DXO_FILTERS`）。
///
/// 逐项生成 `预览_原厂.jpg` / `预览_加滤镜.jpg`（内置色卡过该项 CC 表），
/// 并在 manifest 记录算法版本与表指纹（D06）。
pub fn export_bundle(
    dxo_dir: &Path,
    keep_original: bool,
    items: &[ExportItem<'_>],
    generated: &str,
) -> Result<ExportReport, CameraError> {
    validate_safe_path(dxo_dir)?;

    if items.is_empty() {
        return Err(CameraError::BadInput("没有可导出的滤镜".into()));
    }
    if items.len() > CUSTOM_MAX {
        return Err(CameraError::BadInput(format!(
            "自定义滤镜 {} 个，超过上限 {CUSTOM_MAX}",
            items.len()
        )));
    }

    let mut used: HashSet<String> = HashSet::new();
    let mut order: Vec<serde_json::Value> = Vec::new();
    let mut written: Vec<PathBuf> = Vec::new();
    let mut names: Vec<String> = Vec::new();
    let mut all_files: Vec<RelFile> = Vec::new();

    if keep_original {
        order.push(serde_json::json!({
            "idx": 0, "name": "ORIGINAL", "card_file": null, "factory": true
        }));
    }

    // 预览源：内置色卡（CC 诊断模拟），before 全项共享、after 逐项渲染。
    let (pw, ph, before_rgb) = lut_core::render::build_test_chart();
    let before_jpg = encode_jpeg(pw, ph, &before_rgb)?;

    for (n, it) in items.iter().enumerate() {
        if it.cc.len() != TABLE_LEN {
            return Err(CameraError::BadInput(format!(
                "「{}」的 CC 表长度 {} ≠ {TABLE_LEN}",
                it.name,
                it.cc.len()
            )));
        }
        let safe = card_safe_name(it.name, &mut used);
        let paths = item_paths(&safe);
        let idx = n + if keep_original { 1 } else { 0 };

        let pkg = lut_core::pack_five_slots(it.cc);
        let bin = dxo_dir.join(&paths.bin);
        safe_write(&bin, &pkg)?;
        written.push(bin);
        let bin_rel = RelFile::of(&paths.bin, &pkg);

        let after_rgb = lut_core::render::render_rgb(it.cc, pw, ph, &before_rgb);
        let after_jpg = encode_jpeg(pw, ph, &after_rgb)?;
        let pb = dxo_dir.join(&paths.before);
        safe_write(&pb, &before_jpg)?;
        written.push(pb);
        let before_rel = RelFile::of(&paths.before, &before_jpg);
        let pa = dxo_dir.join(&paths.after);
        safe_write(&pa, &after_jpg)?;
        written.push(pa);
        let after_rel = RelFile::of(&paths.after, &after_jpg);

        let man = serde_json::json!({
            "schema_version": MANIFEST_SCHEMA,
            "display_name": it.name,
            "card_file": paths.bin,
            "source": it.src,
            "format": it.format,
            "lut_size": it.source_size,
            "tone": it.tone,
            "strength": it.strength,
            "bw": it.bw,
            "locked": it.locked,
            "table_sha256": it.table_sha256,
            "algo_version": EXPORT_ALGO_VERSION,
            "preview": {
                "mode": PREVIEW_MODE,
                "source": "builtin-chart",
                "jpeg_quality": PREVIEW_JPEG_QUALITY,
                "max_width": PREVIEW_JPEG_MAX_W,
                "table_sha256": it.table_sha256,
            },
            "files": [&bin_rel, &before_rel, &after_rel],
            "generated": generated,
        });
        let man_bytes = serde_json::to_vec_pretty(&man).unwrap_or_default();
        let mp = dxo_dir.join(&paths.manifest);
        safe_write(&mp, &man_bytes)?;
        written.push(mp);
        let man_rel = RelFile::of(&paths.manifest, &man_bytes);

        all_files.push(bin_rel);
        all_files.push(before_rel);
        all_files.push(after_rel);
        all_files.push(man_rel);

        order.push(serde_json::json!({
            "idx": idx, "name": it.name,
            "card_file": paths.bin, "manifest": paths.manifest
        }));
        names.push(safe);
    }

    let top = serde_json::json!({
        "schema_version": MANIFEST_SCHEMA,
        "menu_slots": 5,
        "camera_custom_max": CUSTOM_MAX,
        "keep_original": keep_original,
        "order": order,
        "files": all_files,
        "algo_version": EXPORT_ALGO_VERSION,
        "preview_mode": PREVIEW_MODE,
        "note": "本组为卡上导出资产；相机当前仍为固定 5 档菜单。预览为 CC 诊断模拟，非相机 DSP 实测。",
        "generated": generated,
    });
    let tp = dxo_dir.join("manifest.json");
    safe_write(&tp, &serde_json::to_vec_pretty(&top).unwrap_or_default())?;
    written.push(tp);

    // 落盘：确保目录项也到介质上
    unsafe { libc::sync() };

    Ok(ExportReport {
        written,
        names,
        root: dxo_dir.to_path_buf(),
    })
}

/// 读取卡上 `DXO_FILTERS` 里由本工具导出的滤镜目录名（含 manifest 是否存在）。
pub fn list_bundles(dxo_dir: &Path) -> Result<Vec<(String, PathBuf)>, CameraError> {
    let mut out = Vec::new();
    if !dxo_dir.is_dir() {
        return Ok(out);
    }
    for e in std::fs::read_dir(dxo_dir)?.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let dir = e.file_name().to_string_lossy().to_string();
        if p.join(format!("{dir}.bin")).is_file() {
            out.push((dir, p));
        }
    }
    out.sort();
    Ok(out)
}

/// 从导出的 `.bin` 里取第 0 槽单槽表（跳过 8 字节 `{dest,len}` 头）。
pub fn table_from_package(pkg: &[u8]) -> Option<Vec<u8>> {
    const HEAD: usize = 8;
    if pkg.len() < HEAD + TABLE_LEN {
        return None;
    }
    Some(pkg[HEAD..HEAD + TABLE_LEN].to_vec())
}

/// 仅凭 manifest 离线重读整包的结果（D07：不猜文件名）。
#[derive(Debug)]
pub struct BundleRead {
    pub schema_version: u32,
    pub keep_original: bool,
    pub order: Vec<serde_json::Value>,
    /// 已逐文件校验（path 白名单 + size + sha256）的导出计划。
    pub files: Vec<RelFile>,
    /// 按 order 里的 manifest 路径读回的逐项 manifest。
    pub items: Vec<serde_json::Value>,
    pub root: PathBuf,
}

/// 仅凭 `DXO_FILTERS/manifest.json` 重读整包：
/// 1. 解析顶层 manifest，取 `files` 导出计划；
/// 2. 对每条相对路径做白名单校验（D09），按路径读盘，核对 size 与 sha256；
/// 3. 按 `order[].manifest` 路径读回逐项 manifest（不靠目录名猜测，D07）。
///
/// 任一文件缺失、尺寸/指纹不符或路径非法即报错；全程只读。
pub fn read_bundle(dxo_dir: &Path) -> Result<BundleRead, CameraError> {
    let top_bytes = std::fs::read(dxo_dir.join("manifest.json"))?;
    let top: serde_json::Value = serde_json::from_slice(&top_bytes)
        .map_err(|e| CameraError::BadInput(format!("顶层 manifest 解析失败: {e}")))?;

    let schema_version = top["schema_version"]
        .as_u64()
        .ok_or_else(|| CameraError::BadInput("顶层 manifest 缺 schema_version".into()))?
        as u32;
    if schema_version != MANIFEST_SCHEMA {
        return Err(CameraError::BadInput(format!(
            "manifest schema {schema_version} ≠ 本工具 {MANIFEST_SCHEMA}"
        )));
    }
    let keep_original = top["keep_original"].as_bool().unwrap_or(false);
    let order = top["order"]
        .as_array()
        .ok_or_else(|| CameraError::BadInput("顶层 manifest 缺 order".into()))?
        .clone();

    let plan = top["files"]
        .as_array()
        .ok_or_else(|| CameraError::BadInput("顶层 manifest 缺 files 导出计划".into()))?;
    let mut files: Vec<RelFile> = Vec::new();
    for f in plan {
        let rel: RelFile = serde_json::from_value(f.clone())
            .map_err(|e| CameraError::BadInput(format!("files 项解析失败: {e}")))?;
        validate_rel_path(&rel.path)?;
        let data = std::fs::read(dxo_dir.join(&rel.path))?;
        if data.len() as u64 != rel.size {
            return Err(CameraError::BadInput(format!(
                "「{}」尺寸 {} ≠ 计划 {}",
                rel.path,
                data.len(),
                rel.size
            )));
        }
        let actual = sha256_hex(&data);
        if actual != rel.sha256 {
            return Err(CameraError::BadInput(format!(
                "「{}」指纹不符: 实际 {actual} ≠ 计划 {}",
                rel.path, rel.sha256
            )));
        }
        files.push(rel);
    }

    let mut items: Vec<serde_json::Value> = Vec::new();
    for entry in &order {
        let mp = match entry["manifest"].as_str() {
            Some(s) => s,
            None => continue, // ORIGINAL 等工厂项无 manifest
        };
        validate_rel_path(mp)?;
        let bytes = std::fs::read(dxo_dir.join(mp))?;
        let man: serde_json::Value = serde_json::from_slice(&bytes)
            .map_err(|e| CameraError::BadInput(format!("逐项 manifest 解析失败: {e}")))?;
        items.push(man);
    }

    Ok(BundleRead {
        schema_version,
        keep_original,
        order,
        files,
        items,
        root: dxo_dir.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(seed: u8) -> Vec<u8> {
        // 真实 CC 表（magic=1）基础上扰动数据区，保证可渲染且逐项不同
        let mut v = lut_core::STOCK_TABLE.to_vec();
        for i in 128..256 {
            v[i] = v[i].wrapping_add(seed);
        }
        v
    }

    #[test]
    fn test_card_safe_name() {
        let mut used = HashSet::new();
        assert_eq!(card_safe_name("Ricoh Posi", &mut used), "RICOHPOSI");
        assert_eq!(card_safe_name("ricoh posi", &mut used), "RICOHPOSI1");
        assert_eq!(card_safe_name("冬野 FUYUNO", &mut used), "FUYUNO");
        assert_eq!(card_safe_name("名", &mut used), "LUT");
        assert_eq!(card_safe_name("abcdefghijklmnop", &mut used), "ABCDEFGHIJKL");
    }

    #[test]
    fn test_export_bundle_writes_expected_layout() {
        let dir = tempfile::tempdir().unwrap();
        let dxo = dir.path().join("DXO_FILTERS");
        let a = cc(1);
        let b = cc(2);
        let items = vec![
            ExportItem {
                name: "FUYUNO",
                src: "fuyuno.cube",
                format: "cube",
                tone: "pass",
                strength: 1.0,
                bw: false,
                locked: false,
                source_size: 33,
                cc: &a,
                table_sha256: "aaa",
            },
            ExportItem {
                name: "KYUSHU",
                src: "kyushu.cube",
                format: "cube",
                tone: "neutral",
                strength: 0.65,
                bw: true,
                locked: false,
                source_size: 17,
                cc: &b,
                table_sha256: "bbb",
            },
        ];
        let rep = export_bundle(&dxo, true, &items, "2026-11-12T09:00:00")
            .unwrap();

        assert_eq!(rep.names, vec!["FUYUNO", "KYUSHU"]);
        let sub = dxo.join("FUYUNO");
        assert!(sub.join("FUYUNO.bin").is_file());
        assert!(sub.join("预览_原厂.jpg").is_file());
        assert!(sub.join("预览_加滤镜.jpg").is_file());
        assert!(sub.join("manifest.json").is_file());
        assert!(dxo.join("manifest.json").is_file());

        // 逐项预览：before 共享、after 因 CC 表不同而不同（D06/R3-03）
        let sub2 = dxo.join("KYUSHU");
        let b1 = std::fs::read(sub.join("预览_原厂.jpg")).unwrap();
        let b2 = std::fs::read(sub2.join("预览_原厂.jpg")).unwrap();
        let a1 = std::fs::read(sub.join("预览_加滤镜.jpg")).unwrap();
        let a2 = std::fs::read(sub2.join("预览_加滤镜.jpg")).unwrap();
        assert_eq!(b1, b2, "before 应为同一内置色卡");
        assert_ne!(a1, a2, "after 应逐项渲染、互不相同");
        assert_ne!(a1, b1, "after 不应等于 before（CC 表非恒等）");
        assert!(a1.starts_with(&[0xFF, 0xD8]), "应为 JPEG");

        // manifest 记录算法版本与表↔预览指纹关联
        let m1: serde_json::Value =
            serde_json::from_slice(&std::fs::read(sub.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(m1["algo_version"], EXPORT_ALGO_VERSION);
        assert_eq!(m1["preview"]["mode"], PREVIEW_MODE);
        assert_eq!(m1["preview"]["table_sha256"], "aaa");
        assert_eq!(m1["table_sha256"], "aaa");

        // 五槽包长度与首槽地址
        let pkg = std::fs::read(sub.join("FUYUNO.bin")).unwrap();
        assert_eq!(pkg.len(), 8 * 5 + TABLE_LEN * 5);
        let addr = u32::from_le_bytes(pkg[0..4].try_into().unwrap());
        assert_eq!(addr, lut_core::SLOT_BASE);

        // 回读单槽表应与输入一致
        let back = table_from_package(&pkg).unwrap();
        assert_eq!(back, a);

        // 顶层清单记录了顺序与 ORIGINAL
        let top: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dxo.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(top["keep_original"], true);
        assert_eq!(top["order"][0]["name"], "ORIGINAL");
        assert_eq!(top["order"][2]["name"], "KYUSHU");
    }

    #[test]
    fn test_export_rejects_too_many() {
        let dir = tempfile::tempdir().unwrap();
        let dxo = dir.path().join("DXO_FILTERS");
        let tables: Vec<Vec<u8>> = (0..6).map(|i| cc(i + 1)).collect();
        let items: Vec<ExportItem> = tables
            .iter()
            .map(|t| ExportItem {
                name: "DUP",
                src: "x.cube",
                format: "cube",
                tone: "pass",
                strength: 1.0,
                bw: false,
                locked: false,
                source_size: 33,
                cc: t,
                table_sha256: "h",
            })
            .collect();
        assert!(export_bundle(&dxo, false, &items, "t").is_err());
    }

    #[test]
    fn test_export_refuses_protected_path() {
        let dir = tempfile::tempdir().unwrap();
        let bad = dir.path().join("DCIM");
        let a = cc(1);
        let items = vec![ExportItem {
            name: "A",
            src: "a.cube",
            format: "cube",
            tone: "pass",
            strength: 1.0,
            bw: false,
            locked: false,
            source_size: 33,
            cc: &a,
            table_sha256: "h",
        }];
        assert!(export_bundle(&bad, false, &items, "t").is_err());
    }

    fn two_items<'a>(a: &'a [u8], b: &'a [u8]) -> Vec<ExportItem<'a>> {
        vec![
            ExportItem {
                name: "FUYUNO", src: "fuyuno.cube", format: "cube", tone: "pass",
                strength: 1.0, bw: false, locked: false, source_size: 33,
                cc: a, table_sha256: "aaa",
            },
            ExportItem {
                name: "KYUSHU", src: "kyushu.cube", format: "cube", tone: "neutral",
                strength: 0.65, bw: true, locked: false, source_size: 17,
                cc: b, table_sha256: "bbb",
            },
        ]
    }

    #[test]
    fn test_validate_rel_path_whitelist() {
        assert!(validate_rel_path("FUYUNO/FUYUNO.bin").is_ok());
        assert!(validate_rel_path("FUYUNO/预览_原厂.jpg").is_ok());
        assert!(validate_rel_path("../evil.bin").is_err());
        assert!(validate_rel_path("FUYUNO/../../x").is_err());
        assert!(validate_rel_path("/abs/path.bin").is_err());
        assert!(validate_rel_path("C:\\x.bin").is_err());
        assert!(validate_rel_path("DCIM/x.bin").is_err());
        assert!(validate_rel_path("touch_prefs.bin").is_err());
        assert!(validate_rel_path("dxot/x").is_err());
        assert!(validate_rel_path("").is_err());
        assert!(validate_rel_path("a//b").is_err());
    }

    #[test]
    fn test_manifest_paths_match_disk() {
        // D07：manifest 里每条相对路径都必须真实落盘，card_file 不再带 lut_ 前缀漂移。
        let dir = tempfile::tempdir().unwrap();
        let dxo = dir.path().join("DXO_FILTERS");
        let (a, b) = (cc(1), cc(2));
        export_bundle(&dxo, true, &two_items(&a, &b), "t").unwrap();

        let top: serde_json::Value =
            serde_json::from_slice(&std::fs::read(dxo.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(top["schema_version"], MANIFEST_SCHEMA);
        let files = top["files"].as_array().unwrap();
        assert_eq!(files.len(), 8, "2 项 × (bin+before+after+manifest)");
        for f in files {
            let p = f["path"].as_str().unwrap();
            assert!(dxo.join(p).is_file(), "计划路径未落盘: {p}");
            assert_eq!(f["size"].as_u64().unwrap(), std::fs::metadata(dxo.join(p)).unwrap().len());
        }
        // 逐项 manifest 的 card_file == 实际 bin 相对路径，且 order 一致
        for entry in top["order"].as_array().unwrap() {
            if entry["manifest"].is_null() {
                continue;
            }
            let man: serde_json::Value = serde_json::from_slice(
                &std::fs::read(dxo.join(entry["manifest"].as_str().unwrap())).unwrap(),
            ).unwrap();
            assert_eq!(man["card_file"], entry["card_file"]);
            assert!(dxo.join(man["card_file"].as_str().unwrap()).is_file());
            assert!(!man["card_file"].as_str().unwrap().starts_with("lut_"));
        }
    }

    #[test]
    fn test_read_bundle_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let dxo = dir.path().join("DXO_FILTERS");
        let (a, b) = (cc(1), cc(2));
        export_bundle(&dxo, true, &two_items(&a, &b), "t").unwrap();

        let read = read_bundle(&dxo).unwrap();
        assert_eq!(read.schema_version, MANIFEST_SCHEMA);
        assert!(read.keep_original);
        assert_eq!(read.files.len(), 8);
        assert_eq!(read.items.len(), 2);
        assert_eq!(read.items[0]["display_name"], "FUYUNO");
        assert_eq!(read.items[1]["table_sha256"], "bbb");
        // 重读不猜文件名：首项 bin 回读单槽表应等于输入
        let bin_rel = read.items[0]["card_file"].as_str().unwrap();
        let pkg = std::fs::read(dxo.join(bin_rel)).unwrap();
        assert_eq!(table_from_package(&pkg).unwrap(), a);
    }

    #[test]
    fn test_read_bundle_detects_tamper() {
        let dir = tempfile::tempdir().unwrap();
        let dxo = dir.path().join("DXO_FILTERS");
        let (a, b) = (cc(1), cc(2));
        export_bundle(&dxo, false, &two_items(&a, &b), "t").unwrap();
        // 篡改首项 bin 一个字节 → 指纹校验必须失败
        let bin = dxo.join("FUYUNO/FUYUNO.bin");
        let mut data = std::fs::read(&bin).unwrap();
        data[100] ^= 0xFF;
        std::fs::write(&bin, &data).unwrap();
        let err = read_bundle(&dxo).unwrap_err();
        assert!(format!("{err}").contains("指纹不符"), "应报指纹不符: {err}");
    }

    #[test]
    fn test_read_bundle_rejects_bad_path() {
        let dir = tempfile::tempdir().unwrap();
        let dxo = dir.path().join("DXO_FILTERS");
        let (a, b) = (cc(1), cc(2));
        export_bundle(&dxo, false, &two_items(&a, &b), "t").unwrap();
        let tp = dxo.join("manifest.json");
        let mut top: serde_json::Value =
            serde_json::from_slice(&std::fs::read(&tp).unwrap()).unwrap();
        // 注入越界路径 → 白名单拒绝（D09）
        top["files"][0]["path"] = serde_json::json!("../evil.bin");
        std::fs::write(&tp, serde_json::to_vec_pretty(&top).unwrap()).unwrap();
        assert!(read_bundle(&dxo).is_err());
    }
}
