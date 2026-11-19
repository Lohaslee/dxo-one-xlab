//! 异步预览服务：代次校验 + 缓存 + 取消，预览运算不在 UI 主循环。

use base64::Engine as _;
use image::ImageDecoder;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

pub const PREVIEW_MAX_SIDE: u32 = 1024;
pub const PREVIEW_JPEG_MAX_W: u32 = 1024;
pub const PREVIEW_JPEG_QUALITY: u8 = 92;
pub const PREVIEW_MODE: &str = "camera-cc-table";

pub struct PreviewImage {
    pub w: u32,
    pub h: u32,
    pub rgb: Vec<u8>,
    pub label: String,
}

#[derive(Default)]
pub struct PreviewState {
    pub img: Mutex<Option<PreviewImage>>,
    pub img_version: AtomicU64,
    pub gen: AtomicU64,
    pub cache: Mutex<HashMap<String, PreviewPayload>>,
}

const SAVED_IMAGE: &str = "preview-image.json";

#[derive(Serialize, Deserialize)]
struct SavedImage {
    label: String,
    png: String,
}

impl PreviewState {
    pub fn load(db_dir: &Path) -> Self {
        let state = Self::default();
        let _ = std::fs::remove_file(db_dir.join("preview-image.tmp"));
        match restore_image(db_dir) {
            Ok(im) => *state.img.lock().unwrap() = im,
            Err(e) => crate::log_line(&format!("preview restore failed: {e}")),
        }
        state
    }
}

fn restore_image(db_dir: &Path) -> Result<Option<PreviewImage>, String> {
    use std::io::Read;
    let file = match std::fs::File::open(db_dir.join(SAVED_IMAGE)) {
        Ok(f) => f,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(e.to_string()),
    };
    let mut bytes = Vec::new();
    file.take(6 * 1024 * 1024 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > 6 * 1024 * 1024 { return Err("保存的预览图过大".into()); }
    let saved: SavedImage = serde_json::from_slice(&bytes).map_err(|e| e.to_string())?;
    let png = base64::engine::general_purpose::STANDARD.decode(saved.png).map_err(|e| e.to_string())?;
    decode_preview_image(&png, &saved.label).map(Some)
}

/// One lossless, oriented/downscaled copy with its display name; never retain originals.
/// Hold the image lock across disk replacement so reset/selection cannot reorder writes.
fn set_image(state: &PreviewState, db_dir: &Path, bytes: &[u8], label: &str) -> Result<(u32, u32, u64), String> {
    use std::io::Write;
    let im = decode_preview_image(bytes, label)?;
    let (w, h) = (im.w, im.h);
    let rgb = image::RgbImage::from_raw(w, h, im.rgb.clone()).ok_or("预览像素尺寸不匹配")?;
    let mut png = std::io::Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(rgb).write_to(&mut png, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    let saved = SavedImage { label: label.into(), png: base64::engine::general_purpose::STANDARD.encode(png.into_inner()) };
    let bytes = serde_json::to_vec(&saved).map_err(|e| e.to_string())?;
    let mut guard = state.img.lock().unwrap();
    std::fs::create_dir_all(db_dir).map_err(|e| format!("保存预览照片失败: {e}"))?;
    let temp = db_dir.join("preview-image.tmp");
    let target = db_dir.join(SAVED_IMAGE);
    let write = (|| -> std::io::Result<()> {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        drop(file);
        lut_camera::volume::replace_file(&temp, &target)?;
        lut_camera::volume::fsync_parent(&target);
        Ok(())
    })();
    if let Err(e) = write {
        let _ = std::fs::remove_file(&temp);
        return Err(format!("保存预览照片失败: {e}"));
    }
    *guard = Some(im);
    let version = state.img_version.fetch_add(1, Ordering::SeqCst) + 1;
    cancel(state);
    state.cache.lock().unwrap().clear();
    Ok((w, h, version))
}

fn reset_image(state: &PreviewState, db_dir: &Path) -> Result<u64, String> {
    let mut guard = state.img.lock().unwrap();
    match std::fs::remove_file(db_dir.join(SAVED_IMAGE)) {
        Ok(()) => lut_camera::volume::fsync_parent(&db_dir.join(SAVED_IMAGE)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
        Err(e) => return Err(format!("清除预览照片失败: {e}")),
    }
    *guard = None;
    let version = state.img_version.fetch_add(1, Ordering::SeqCst) + 1;
    cancel(state);
    state.cache.lock().unwrap().clear();
    Ok(version)
}

#[derive(Serialize, Clone)]
pub struct PreviewPayload {
    pub key: String,
    pub gen: u64,
    pub id: i64,
    pub before: String,
    pub after: String,
    pub table_sha: String,
    pub w: u32,
    pub h: u32,
    pub img_version: u64,
    pub mode: String,
}

pub fn cache_key(id: i64, tone: &str, strength: f32, bw: bool, img_version: u64) -> String {
    format!("{id}|{tone}|{strength:.2}|{bw}|{img_version}")
}

/// 与编辑保存/写卡共用 CC 表；卡载包取 Still1（物理槽 1，非首槽 Video）。
fn preview_table(
    db_dir: &std::path::Path,
    id: i64,
    tone: &str,
    strength: f32,
    bw: bool,
    card_mount: Option<&str>,
    card_path: Option<&str>,
) -> Result<Vec<u8>, String> {
    let cc = match (card_mount, card_path) {
        (Some(mount), Some(cpath)) => {
            let file = cpath.rsplit(['\\', '/']).next().unwrap_or("");
            if file.is_empty() || file == "." || file == ".."
                || !file.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-' || c == '.')
            {
                return Err(format!("非法卡上路径: {cpath}"));
            }
            let full = std::path::Path::new(mount).join("DXO_FILTERS").join(file);
            let pkg = std::fs::read(&full).map_err(|e| format!("读卡载包失败: {e}"))?;
            let slots = lut_camera::card::parse_package(&pkg).map_err(|e| e.to_string())?;
            Ok(slots[1].1.clone())
        }
        (None, None) => {
            let store = lut_persist::Store::open(db_dir).map_err(|e| e.to_string())?;
            let rec = store.get_record(id).map_err(|e| e.to_string())?;
            // 参数未变时直接读已保存字节，参数变化时复用实际保存的重算路径。
            crate::stage_edit(&store, id, &rec.name, tone, strength, bw).map(|edit| edit.cc)
        }
        _ => Err("卡挂载点与滤镜路径须同时提供".into()),
    }?;
    if cc.len() != lut_core::TABLE_LEN
        || cc[..8] != [1, 0, 0, 0, 1, 0, 0, 0]
    {
        return Err("CC 表长度或文件头无效，无法预览".into());
    }
    Ok(cc)
}

/// RGB → JPEG base64（最长边限宽，与 Web pil_to_b64 同参数）。
pub fn jpeg_b64(w: u32, h: u32, rgb: &[u8]) -> Result<String, String> {
    let img = image::RgbImage::from_raw(w, h, rgb.to_vec())
        .ok_or_else(|| "预览像素尺寸不匹配".to_string())?;
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
    dynimg.write_with_encoder(enc).map_err(|e| format!("JPEG 编码失败: {e}"))?;
    Ok(base64_encode(&buf.into_inner()))
}

fn base64_encode(data: &[u8]) -> String {
    const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity((data.len() + 2) / 3 * 4);
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

/// DNG/TIFF 的 IFD0 常是 160×120 缩略图，放大即马赛克；可看的渲染层是 SubIFD 里的 JPEG 预览。
/// 遍历 IFD0 + SubIFD +  next-IFD 链，取像素数最大的 JPEG（comp=7、RGB/YCbCr）预览层字节；
/// 方向统一取 IFD0 的 EXIF orientation（预览层自身常不带该标签）。
fn tiff_largest_jpeg_preview(bytes: &[u8]) -> Option<(Vec<u8>, u8)> {
    let bo = match bytes.get(0..2)? {
        b"II" => false,
        b"MM" => true,
        _ => return None,
    };
    let u16 = |o: usize| -> Option<u16> {
        let b = bytes.get(o..o + 2)?;
        Some(if bo { u16::from_be_bytes([b[0], b[1]]) } else { u16::from_le_bytes([b[0], b[1]]) })
    };
    let u32 = |o: usize| -> Option<u32> {
        let b = bytes.get(o..o + 4)?;
        Some(if bo { u32::from_be_bytes([b[0], b[1], b[2], b[3]]) } else { u32::from_le_bytes([b[0], b[1], b[2], b[3]]) })
    };
    if u16(2)? != 42 {
        return None;
    }
    struct Ifd {
        w: u32,
        h: u32,
        orient: u8,
        jpeg: Option<(u32, Vec<u32>, Vec<u32>)>, // comp=7 且 RGB/YCbCr 时的 (备用JPEG偏移, strip偏移, strip长度)
        jpeg_len: u32,
        subs: Vec<u32>,
        next: u32,
    }
    let parse = |off: u32| -> Option<Ifd> {
        let off = off as usize;
        let n = u16(off)? as usize;
        if n > 512 {
            return None;
        }
        let mut tags: HashMap<u16, (u16, u32, u32)> = HashMap::new();
        for i in 0..n {
            let e = off + 2 + i * 12;
            let tag = u16(e)?;
            let typ = u16(e + 2)?;
            let cnt = u32(e + 4)?;
            let val = u32(e + 8)?;
            tags.insert(tag, (typ, cnt, val));
        }
        let short = |t: u16| -> Option<u32> {
            let (typ, cnt, val) = *tags.get(&t)?;
            if typ == 3 && cnt == 1 { Some(val & 0xffff) } else { None }
        };
        let longs = |t: u16| -> Option<Vec<u32>> {
            let (typ, cnt, val) = *tags.get(&t)?;
            if typ != 4 || cnt == 0 || cnt > 4096 {
                return None;
            }
            if cnt == 1 {
                return Some(vec![val]);
            }
            (0..cnt).map(|i| u32(val as usize + i as usize * 4)).collect()
        };
        let w = short(0x0100).or_else(|| longs(0x0100).and_then(|v| v.first().copied()))?;
        let h = short(0x0101).or_else(|| longs(0x0101).and_then(|v| v.first().copied()))?;
        let comp = short(0x0103)?;
        let photo = short(0x0106).unwrap_or(2);
        let spp = short(0x0115).unwrap_or(1);
        let jpeg = if comp == 7 && (photo == 2 || photo == 6) && spp == 3 {
            let offs = longs(0x0111)?;
            let lens = longs(0x0117)?;
            if offs.len() != lens.len() {
                return None;
            }
            Some((tags.get(&0x0201).map(|t| t.2).unwrap_or(0), offs, lens))
        } else {
            None
        };
        let jpeg_len = tags.get(&0x0202).map(|t| t.2).unwrap_or(0);
        let subs = longs(0x014a).unwrap_or_default();
        let next = u32(off + 2 + n * 12)?;
        let orient = short(0x0112).unwrap_or(1) as u8;
        Some(Ifd { w, h, orient, jpeg, jpeg_len, subs, next })
    };
    let root = u32(4)?;
    let orient = parse(root).map(|i| i.orient).unwrap_or(1);
    let mut best: Option<(u64, Vec<u8>)> = None;
    let mut queue = vec![root];
    let mut seen = std::collections::HashSet::new();
    let mut visits = 0;
    while let Some(off) = queue.pop() {
        if off == 0 || !seen.insert(off) || visits > 64 {
            continue;
        }
        visits += 1;
        let ifd = match parse(off) {
            Some(v) => v,
            None => continue,
        };
        if let Some((joff, offs, lens)) = &ifd.jpeg {
            let area = ifd.w as u64 * ifd.h as u64;
            if best.as_ref().map(|b| area > b.0).unwrap_or(true) {
                let mut buf = Vec::new();
                if *joff != 0 && ifd.jpeg_len != 0 {
                    buf.extend_from_slice(bytes.get(*joff as usize..(*joff + ifd.jpeg_len) as usize)?);
                } else {
                    for (o, l) in offs.iter().zip(lens.iter()) {
                        buf.extend_from_slice(bytes.get(*o as usize..(*o + *l) as usize)?);
                    }
                }
                if buf.starts_with(&[0xff, 0xd8]) {
                    best = Some((area, buf));
                }
            }
        }
        queue.extend(ifd.subs.iter().copied());
        queue.push(ifd.next);
    }
    best.map(|(_, buf)| (buf, orient))
}

fn decode_oriented(bytes: &[u8]) -> Result<image::DynamicImage, String> {
    let reader = image::ImageReader::new(std::io::Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|e| format!("预览图读取失败: {e}"))?;
    if reader.format() == Some(image::ImageFormat::Tiff) {
        if let Some((jpeg, exif)) = tiff_largest_jpeg_preview(bytes) {
            let mut img = image::load_from_memory(&jpeg)
                .map_err(|e| format!("预览图读取失败: {e}"))?;
            img.apply_orientation(
                image::metadata::Orientation::from_exif(exif)
                    .unwrap_or(image::metadata::Orientation::NoTransforms),
            );
            return Ok(img);
        }
    }
    let mut decoder = reader.into_decoder().map_err(|e| format!("预览图读取失败: {e}"))?;
    let orient = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("预览图读取失败: {e}"))?;
    img.apply_orientation(orient);
    Ok(img)
}

/// 解码用户预览图：RGB8 + 最长边 ≤1024（LANCZOS，与 Web 一致）。
pub fn decode_preview_image(bytes: &[u8], label: &str) -> Result<PreviewImage, String> {
    // 手机/相机竖拍靠 EXIF orientation 标记方向，裸解码会横躺；解码后先转正再缩放
    let img = decode_oriented(bytes)?;
    let img = img.to_rgb8();
    let (w, h) = (img.width(), img.height());
    let img = if w.max(h) > PREVIEW_MAX_SIDE {
        let scale = PREVIEW_MAX_SIDE as f32 / w.max(h) as f32;
        let nw = (w as f32 * scale + 0.5) as u32;
        let nh = (h as f32 * scale + 0.5) as u32;
        image::DynamicImage::ImageRgb8(img)
            .resize_exact(nw.max(1), nh.max(1), image::imageops::FilterType::Lanczos3)
            .to_rgb8()
    } else {
        img
    };
    let (w, h) = (img.width(), img.height());
    Ok(PreviewImage { w, h, rgb: img.into_raw(), label: label.to_string() })
}

fn current_image(state: &PreviewState) -> (u32, u32, Vec<u8>, u64) {
    let guard = state.img.lock().unwrap();
    match guard.as_ref() {
        Some(im) => (im.w, im.h, im.rgb.clone(), state.img_version.load(Ordering::SeqCst)),
        None => {
            let (w, h, rgb) = lut_core::render::build_test_chart();
            (w, h, rgb, state.img_version.load(Ordering::SeqCst))
        }
    }
}

/// 发起一次预览：返回代次；结果经 `preview-result` 事件回推（晚到代次被丢弃）。
/// card_mount/card_path 同时给出时预览卡上五槽包的 Still1 表（只读，参数固定）。
#[tauri::command]
pub async fn preview_request(
    app: tauri::AppHandle,
    id: i64,
    tone: String,
    strength: f32,
    bw: bool,
    card_mount: Option<String>,
    card_path: Option<String>,
) -> Result<u64, String> {
    if !matches!(tone.as_str(), "pass" | "neutral" | "blend") {
        return Err(format!("无效 tone: {tone}"));
    }
    if !(0.0..=1.0).contains(&strength) {
        return Err("强度须在 0..1".into());
    }
    let state = app.state::<PreviewState>();
    let gen = state.gen.fetch_add(1, Ordering::SeqCst) + 1;
    let img_version = state.img_version.load(Ordering::SeqCst);
    let mut key = cache_key(id, &tone, strength, bw, img_version);
    if let (Some(m), Some(p)) = (&card_mount, &card_path) {
        key.push_str(&format!("|card:{m}|{p}"));
    }

    let (w, h, rgb, _) = current_image(&state);
    let db_dir = lut_persist::Store::default_db_dir();
    let handle = app.clone();
    tauri::async_runtime::spawn_blocking(move || {
        let result = (|| -> Result<PreviewPayload, String> {
            let cc = preview_table(
                &db_dir, id, &tone, strength, bw, card_mount.as_deref(), card_path.as_deref(),
            )?;
            let table_sha = lut_persist::Store::hash_bytes(&cc);
            // 先读表再查缓存：同路径覆盖包、库内重算均不能命中旧图。
            key.push_str(&format!("|{table_sha}"));
            if let Some(mut hit) = handle.state::<PreviewState>().cache.lock().unwrap().get(&key).cloned() {
                hit.gen = gen;
                return Ok(hit);
            }
            let after = lut_core::render::render_rgb(&cc, w, h, &rgb);
            let before_b64 = jpeg_b64(w, h, &rgb)?;
            let after_b64 = jpeg_b64(w, h, &after)?;
            Ok(PreviewPayload {
                key: key.clone(),
                gen,
                id,
                before: before_b64,
                after: after_b64,
                table_sha,
                w,
                h,
                img_version,
                mode: PREVIEW_MODE.into(),
            })
        })();
        let st = handle.state::<PreviewState>();
        match result {
            Ok(payload) => {
                st.cache.lock().unwrap().insert(payload.key.clone(), payload.clone());
                if st.gen.load(Ordering::SeqCst) == gen {
                    let _ = handle.emit("preview-result", payload);
                }
            }
            Err(msg) => {
                if st.gen.load(Ordering::SeqCst) == gen {
                    let _ = handle.emit("preview-error", serde_json::json!({ "gen": gen, "id": id, "msg": msg }));
                }
            }
        }
    });
    Ok(gen)
}

#[tauri::command]
pub fn preview_set_image_bytes(
    state: tauri::State<'_, PreviewState>,
    bytes: Vec<u8>,
    label: String,
) -> Result<serde_json::Value, String> {
    let (w, h, version) = set_image(&state, &lut_persist::Store::default_db_dir(), &bytes, &label)?;
    Ok(serde_json::json!({ "w": w, "h": h, "version": version }))
}

#[tauri::command]
pub fn preview_reset_image(state: tauri::State<'_, PreviewState>) -> Result<u64, String> {
    reset_image(&state, &lut_persist::Store::default_db_dir())
}

#[tauri::command]
pub fn preview_cancel(state: tauri::State<'_, PreviewState>) -> u64 {
    cancel(&state)
}

pub fn cancel(state: &PreviewState) -> u64 {
    state.gen.fetch_add(1, Ordering::SeqCst) + 1
}

#[tauri::command]
pub fn preview_image_info(state: tauri::State<'_, PreviewState>) -> serde_json::Value {
    let guard = state.img.lock().unwrap();
    let version = state.img_version.load(Ordering::SeqCst);
    match guard.as_ref() {
        Some(im) => serde_json::json!({ "version": version, "label": im.label, "w": im.w, "h": im.h, "custom": true }),
        None => serde_json::json!({ "version": version, "label": "内置色卡", "w": 560, "h": 320, "custom": false }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn preview_photo_replaces_one_copy_restores_pixels_and_resets() {
        let dir = tempfile::tempdir().unwrap();
        let first = include_bytes!("../tests/fixtures/portrait-exif6.jpg");
        let second = include_bytes!("../tests/fixtures/portrait-plain.jpg");
        let state = PreviewState::load(dir.path());
        assert!(state.img.lock().unwrap().is_none());
        set_image(&state, dir.path(), first, "first.jpg").unwrap();
        let original = state.img.lock().unwrap().as_ref().unwrap().rgb.clone();
        let saved_first = std::fs::read(dir.path().join(SAVED_IMAGE)).unwrap();
        drop(state);
        let restored = PreviewState::load(dir.path());
        {
            let guard = restored.img.lock().unwrap();
            let im = guard.as_ref().unwrap();
            assert_eq!((im.w, im.h), (60, 80)); // EXIF orientation is baked once.
            assert_eq!(im.label, "first.jpg");
            assert_eq!(im.rgb, original); // Lossless: restarting must not alter preview colors.
        }
        assert!(set_image(&restored, dir.path(), b"not an image", "bad.jpg").is_err());
        assert_eq!(std::fs::read(dir.path().join(SAVED_IMAGE)).unwrap(), saved_first);
        assert_eq!(restored.img.lock().unwrap().as_ref().unwrap().label, "first.jpg");
        set_image(&restored, dir.path(), second, "second.jpg").unwrap();
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 1);
        assert_ne!(std::fs::read(dir.path().join(SAVED_IMAGE)).unwrap(), saved_first);
        assert_eq!(PreviewState::load(dir.path()).img.lock().unwrap().as_ref().unwrap().label, "second.jpg");
        reset_image(&restored, dir.path()).unwrap();
        assert!(PreviewState::load(dir.path()).img.lock().unwrap().is_none());
        assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);
        reset_image(&restored, dir.path()).unwrap(); // Already on chart is harmless.
    }

    #[test]
    fn preview_photo_failed_write_and_reset_preserve_current_image() {
        let dir = tempfile::tempdir().unwrap();
        let bytes = include_bytes!("../tests/fixtures/portrait-plain.jpg");
        let state = PreviewState::default();
        set_image(&state, dir.path(), bytes, "old.jpg").unwrap();
        let version = state.img_version.load(Ordering::SeqCst);
        // Occupied staging path simulates a failed write, without permission assumptions.
        std::fs::create_dir(dir.path().join("preview-image.tmp")).unwrap();
        assert!(set_image(&state, dir.path(), bytes, "new.jpg").is_err());
        assert_eq!(state.img_version.load(Ordering::SeqCst), version);
        assert_eq!(state.img.lock().unwrap().as_ref().unwrap().label, "old.jpg");
        assert_eq!(PreviewState::load(dir.path()).img.lock().unwrap().as_ref().unwrap().label, "old.jpg");
        std::fs::remove_file(dir.path().join(SAVED_IMAGE)).unwrap();
        std::fs::create_dir(dir.path().join(SAVED_IMAGE)).unwrap();
        assert!(reset_image(&state, dir.path()).is_err());
        assert_eq!(state.img.lock().unwrap().as_ref().unwrap().label, "old.jpg");
    }

    #[test]
    fn preview_photo_corruption_falls_back_to_chart_and_cleans_staging() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join(SAVED_IMAGE), b"broken").unwrap();
        std::fs::write(dir.path().join("preview-image.tmp"), b"interrupted write").unwrap();
        let state = PreviewState::load(dir.path());
        assert!(state.img.lock().unwrap().is_none());
        assert!(!dir.path().join("preview-image.tmp").exists());
        set_image(&state, dir.path(), include_bytes!("../tests/fixtures/portrait-plain.jpg"), "recovered.jpg").unwrap();
        assert_eq!(PreviewState::load(dir.path()).img.lock().unwrap().as_ref().unwrap().label, "recovered.jpg");
    }

    #[test]
    fn test_local_preview_matches_saved_and_packed_table() {
        let db = tempfile::tempdir().unwrap();
        let source = db.path().join("IDENTITY.cube");
        std::fs::write(&source, "LUT_3D_SIZE 2\n0 0 0\n1 0 0\n0 1 0\n1 1 0\n0 0 1\n1 0 1\n0 1 1\n1 1 1\n").unwrap();
        let result = crate::import_service(db.path(), vec![source], "pass", 1.0, false);
        assert!(result[0].ok);
        let mut store = lut_persist::Store::open(db.path()).unwrap();
        let rec = store.list().unwrap().remove(0);
        let original = preview_table(db.path(), rec.id, "pass", 1.0, false, None, None).unwrap();
        assert_eq!(original, store.load_bytes(rec.id).unwrap());
        // 恒等 LOOK 仍应包含原厂 CC，旧纯 LOOK 预览会在这里失败。
        assert_eq!(original, lut_core::load_stock_bytes().unwrap());
        let source = db.path().join("TINT.cube");
        let mut text = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 { for g in 0..2 { for r in 0..2 {
            text.push_str(&format!("{} {} {}\n", r as f32 * 0.6, g as f32 * 0.8, b));
        } } }
        std::fs::write(&source, text).unwrap();
        assert!(crate::import_service(db.path(), vec![source], "pass", 1.0, false)[0].ok);
        let rec = store.list().unwrap().into_iter().find(|r| r.name == "TINT").unwrap();
        for (tone, strength, bw) in [("pass", 0.0, false), ("neutral", 0.3, false), ("blend", 0.65, true), ("pass", 1.0, false)] {
            let shown = preview_table(db.path(), rec.id, tone, strength, bw, None, None).unwrap();
            let edit = crate::stage_edit(&store, rec.id, &rec.name, tone, strength, bw).unwrap();
            let mut pending = HashMap::from([(rec.id, edit)]);
            let report = crate::flush_pending(&mut store, &mut pending);
            assert!(report.failed.is_empty());
            let saved = store.load_bytes(rec.id).unwrap();
            let packed = lut_core::pack_five_slots(&saved);
            let slots = lut_camera::card::parse_package(&packed).unwrap();
            assert_eq!(shown, slots[1].1, "预览必须等于实际写卡表");
            assert_eq!(lut_persist::Store::hash_bytes(&shown), store.get_record(rec.id).unwrap().hash);
            let (w, h, rgb) = lut_core::render::build_test_chart();
            assert_eq!(lut_core::render::render_rgb(&shown, w, h, &rgb),
                lut_core::render::render_rgb(&slots[1].1, w, h, &rgb));
        }
    }

    #[test]
    fn test_card_preview_uses_still1_and_rereads_replaced_package() {
        let vol = tempfile::tempdir().unwrap();
        let dir = vol.path().join("DXO_FILTERS");
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("lut_orig.bin");
        // NONE 的 Video/Still1 原厂表不同，不能把首槽当照片表。
        let mut package = lut_core::pack_five_slots(lut_core::STOCK_TABLE);
        package[8 + lut_core::BODY_OFF] ^= 0xff;
        std::fs::write(&path, &package).unwrap();
        let read = || preview_table(vol.path(), -1, "pass", 1.0, false,
            Some(vol.path().to_str().unwrap()), Some("c:\\DXO_FILTERS\\lut_orig.bin"));
        let before = read().unwrap();
        assert_eq!(before, lut_core::STOCK_TABLE);
        // 同一路径内容更换（即使旧版只检查的短指纹未变）也必须改变缓存身份。
        package[lut_core::TABLE_LEN + 16 + lut_core::BODY_OFF] ^= 0x01;
        std::fs::write(&path, &package).unwrap();
        let after = read().unwrap();
        assert_ne!(lut_persist::Store::hash_bytes(&before), lut_persist::Store::hash_bytes(&after));
        package[0..4].copy_from_slice(&0u32.to_le_bytes());
        std::fs::write(&path, &package).unwrap();
        assert!(read().is_err(), "错误槽地址不得悄悄渲染");
        std::fs::write(&path, &package[..100]).unwrap();
        assert!(read().is_err(), "截断包不得悄悄渲染");
        std::fs::remove_file(&path).unwrap();
        assert!(read().is_err(), "拔卡/缺包不得沿用缓存");
    }

    #[test]
    fn test_cache_key_includes_image_version() {
        let a = cache_key(1, "pass", 0.65, false, 3);
        let b = cache_key(1, "pass", 0.65, false, 4);
        assert_ne!(a, b);
        assert_ne!(a, cache_key(1, "blend", 0.65, false, 3));
        assert_ne!(a, cache_key(2, "pass", 0.65, false, 3));
    }

    #[test]
    fn test_generation_guard_drops_late_results() {
        let state = PreviewState::default();
        let g1 = state.gen.fetch_add(1, Ordering::SeqCst) + 1;
        let g2 = state.gen.fetch_add(1, Ordering::SeqCst) + 1;
        assert!(g2 > g1);
        // 晚到的 g1 结果不得覆盖当前代次
        assert_ne!(state.gen.load(Ordering::SeqCst), g1);
        let g3 = cancel(&state);
        assert!(g3 > g2);
        assert_ne!(state.gen.load(Ordering::SeqCst), g1);
    }

    #[test]
    fn test_jpeg_b64_roundtrip_size_cap() {
        let (w, h, rgb) = lut_core::render::build_test_chart();
        let b64 = jpeg_b64(w, h, &rgb).unwrap();
        assert!(b64.len() > 100);
        assert!(b64.len() % 4 == 0);
    }

    #[test]
    fn test_jpeg_b64_keeps_1024_width() {
        let w = 1024u32;
        let h = 640u32;
        let rgb: Vec<u8> = (0..(w * h * 3) as usize).map(|i| ((i * 37) % 256) as u8).collect();
        let b64 = jpeg_b64(w, h, &rgb).unwrap();
        let bytes: Vec<u8> = b64.bytes().filter(|c| *c != b'=').map(|c| {
            const T: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
            T.iter().position(|t| *t == c).unwrap() as u8
        }).collect();
        let mut raw = Vec::new();
        for chunk in bytes.chunks(4) {
            let n = (chunk[0] as u32) << 18 | (chunk[1] as u32) << 12
                | (*chunk.get(2).unwrap_or(&0) as u32) << 6 | *chunk.get(3).unwrap_or(&0) as u32;
            raw.push((n >> 16) as u8);
            if chunk.len() > 2 { raw.push((n >> 8) as u8); }
            if chunk.len() > 3 { raw.push(n as u8); }
        }
        let img = image::load_from_memory(&raw).unwrap();
        assert_eq!(img.width(), 1024, "1024 宽预览不得再被压缩");
        assert_eq!(img.height(), 640);
    }

    #[test]
    fn test_decode_preview_image_downscales() {
        let img = image::RgbImage::from_pixel(2048, 1024, image::Rgb([120, 60, 30]));
        let mut buf = std::io::Cursor::new(Vec::new());
        image::DynamicImage::ImageRgb8(img)
            .write_to(&mut buf, image::ImageFormat::Png)
            .unwrap();
        let im = decode_preview_image(&buf.into_inner(), "big.png").unwrap();
        assert_eq!(im.w, 1024);
        assert_eq!(im.h, 512);
    }

    #[test]
    fn test_decode_dng_picks_jpeg_preview_not_thumbnail() {
        fn probe(bytes: &[u8]) -> (u32, u32, [u8; 3], [u8; 3]) {
            let im = decode_preview_image(bytes, "p.dng").unwrap();
            let at = |x: u32, y: u32| {
                let i = ((y * im.w + x) * 3) as usize;
                [im.rgb[i], im.rgb[i + 1], im.rgb[i + 2]]
            };
            (im.w, im.h, at(5, 2), at(5, 77))
        }
        // IFD0 为 8×6 灰缩略图 + CFA SubIFD + JPEG 预览层；取向沿用 IFD0 的 EXIF6
        let (w, h, top, bot) = probe(include_bytes!("../tests/fixtures/dng-thumb-plus-preview.dng"));
        assert_eq!((w, h), (60, 80), "DNG 须取 JPEG 预览层而非 IFD0 缩略图");
        assert!(top[0] > 150 && top[1] < 100, "顶应为红: {top:?}");
        assert!(bot[2] > 150 && bot[0] < 100, "底应为蓝: {bot:?}");
        // IFD0 自身即 JPEG 缩略图时仍取更大的预览层
        let (w2, h2, top2, bot2) = probe(include_bytes!("../tests/fixtures/dng-jpeg-thumb-plus-preview.dng"));
        assert_eq!((w2, h2), (60, 80));
        assert!(top2[0] > 150 && bot2[2] > 150);
    }

    #[test]
    fn test_decode_applies_exif_orientation() {
        fn probe(bytes: &[u8]) -> (u32, u32, [u8; 3], [u8; 3]) {
            let im = decode_preview_image(bytes, "p.jpg").unwrap();
            let at = |x: u32, y: u32| {
                let i = ((y * im.w + x) * 3) as usize;
                [im.rgb[i], im.rgb[i + 1], im.rgb[i + 2]]
            };
            (im.w, im.h, at(5, 2), at(5, 77))
        }
        let (w, h, top, bot) = probe(include_bytes!("../tests/fixtures/portrait-exif6.jpg"));
        assert_eq!((w, h), (60, 80), "EXIF6 横存储须解回纵向");
        assert!(top[0] > 150 && top[1] < 100, "顶应为红: {top:?}");
        assert!(bot[2] > 150 && bot[0] < 100, "底应为蓝: {bot:?}");
        let (w2, h2, top2, bot2) = probe(include_bytes!("../tests/fixtures/portrait-plain.jpg"));
        assert_eq!((w2, h2), (60, 80));
        assert!(top2[0] > 150 && bot2[2] > 150);
    }
}
