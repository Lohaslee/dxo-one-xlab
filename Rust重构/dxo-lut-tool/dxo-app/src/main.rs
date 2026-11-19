//! Tauri 版 GUI：Rust 后端（lut-persist / lut-core）+ 静态 HTML 前端（ui/）。
//!
//! 命令（IPC）：本文件 #[tauri::command] 函数即前端可调用的 API 面。
//! 数据库：Store::default_db_dir()（DXO_DATA_DIR 优先），与 dxo-cli / dxo-gui 同源。
//! 导入逻辑：选择器与外部拖放共用 import_service（R2-02），默认参数 pass/0.65/彩色（Web 基线）。
//! 保存闭环：编辑按稳定 ID 暂存（pending），flush 仅成功项清 dirty，失败保留可重试；
//! 关闭前 flush，失败挂起关闭（pending_close 语义，对齐 dxo-gui D03）。

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use lut_core::cube::CubeLUT;
use lut_persist::{FilterRecord, NewFilter, Store};

mod cardops;
mod patch;
mod preview;
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::{Emitter, Manager};

/// 预览库软上限（与 Web 基线 LIB_MAX 一致）。
const LIB_MAX: i64 = 100;

/// 极简文件日志：追加到 `<数据目录>/logs/app.log`（无外部依赖）。
/// 仅记录启动与关键业务事件，便于干净账户/故障排查；失败静默不影响主流程。
fn log_line(msg: &str) {
    use std::io::Write;
    let dir = Store::default_db_dir().join("logs");
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let line = format!("[unix:{secs}] {msg}\n");
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(dir.join("app.log"))
    {
        let _ = f.write_all(line.as_bytes());
    }
}

// ── 会话状态（仅本次运行有效；「存为导入默认」不跨重启） ─────

#[derive(Clone)]
struct Defaults {
    tone: String,
    strength: f32,
    bw: bool,
}

impl Defaults {
    fn initial() -> Self {
        Self { tone: "pass".into(), strength: 0.65, bw: false }
    }
}

/// 未落盘的编辑（稳定 ID → 重算后的 CC 表）。flush 成功才移除。
#[derive(Clone)]
struct PendingEdit {
    name: String,
    tone: String,
    strength: f32,
    bw: bool,
    cc: Vec<u8>,
}

struct Session {
    pending: Mutex<HashMap<i64, PendingEdit>>,
    defaults: Mutex<Defaults>,
    allow_close: AtomicBool,
}

impl Session {
    fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            defaults: Mutex::new(Defaults::initial()),
            allow_close: AtomicBool::new(false),
        }
    }
}

// ── 基础查询 ────────────────────────────────────────────

/// 列出在库中的全部滤镜（不含回收站）。
#[tauri::command]
fn list_luts() -> Result<Vec<FilterRecord>, String> {
    let store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    store.list().map_err(|e| e.to_string())
}

/// 回收站列表（最近删除在前）。
#[tauri::command]
fn list_trash() -> Result<Vec<FilterRecord>, String> {
    let store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    store.list_deleted().map_err(|e| e.to_string())
}

/// 应用信息：版本 + 数据库路径（前端「关于」页用）。
#[tauri::command]
fn app_info() -> Result<serde_json::Value, String> {
    let db_dir = Store::default_db_dir();
    Ok(serde_json::json!({
        "version": env!("CARGO_PKG_VERSION"),
        "db_dir": db_dir.to_string_lossy(),
    }))
}

/// 只读探测相机卡状态（diskutil info，不写入、不弹出）。
#[tauri::command]
async fn card_status() -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(|| match lut_camera::find_card() {
        Ok(card) => {
            let mounted = card.mount_point.as_ref().map(|p| p.exists()).unwrap_or(false);
            serde_json::json!({
                "found": true,
                "device": card.device,
                "uuid": card.uuid,
                "mount_point": card.mount_point.map(|p| p.to_string_lossy().to_string()),
                "volume_name": card.volume_name,
                "mounted": mounted,
            })
        }
        Err(e) => serde_json::json!({ "found": false, "reason": e.to_string() }),
    })
    .await
    .map_err(|e| format!("探测线程失败: {e}"))
}

// ── 导入（选择器 / 外部拖放共用同一服务） ─────────────────

#[derive(Serialize, Clone, Debug)]
struct ImportOutcome {
    file: String,
    ok: bool,
    msg: String,
}

/// 按扩展名分派解析（对齐 dxo-gui import_files / Web 版 filt_core.load_lut 支持面）。
fn parse_any(path: &Path) -> Result<(usize, CubeLUT), String> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "cube" => lut_core::read_cube(&path.to_string_lossy()),
        "3dl" => std::fs::read_to_string(path)
            .map_err(|e| format!("读取失败: {e}"))
            .and_then(|t| {
                lut_core::formats::parse_3dl_text(&t)
                    .and_then(lut_core::formats::grid_to_cube)
                    .map(|c| (c.source_size, c))
            }),
        "csp" => std::fs::read_to_string(path)
            .map_err(|e| format!("读取失败: {e}"))
            .and_then(|t| {
                lut_core::formats::parse_csp_text(&t)
                    .and_then(lut_core::formats::grid_to_cube)
                    .map(|c| (c.source_size, c))
            }),
        "png" | "jpg" | "jpeg" | "tif" | "tiff" => image::open(path)
            .map_err(|e| format!("图像解码失败: {e}"))
            .and_then(|img| {
                let rgb = img.to_rgb8();
                lut_core::formats::hald_from_pixels(rgb.width(), rgb.height(), rgb.as_raw())
                    .and_then(lut_core::formats::grid_to_cube)
                    .map(|c| (c.source_size, c))
            }),
        _ => Err("暂不支持该格式（支持 .cube/.3dl/.csp/Hald 图像）".into()),
    }
}

/// 逐文件导入：解析 → 复合 CC 表 → 入库。失败不污染库；库上限 LIB_MAX。
/// Web 选择器字节流（import_bytes）与外部拖放（Drop 事件）都调用本函数。
fn import_service(
    db_dir: &Path,
    paths: Vec<PathBuf>,
    def_tone: &str,
    def_strength: f32,
    def_bw: bool,
) -> Vec<ImportOutcome> {
    let mut out = Vec::new();
    if !matches!(def_tone, "pass" | "neutral" | "blend") {
        out.push(ImportOutcome { file: String::new(), ok: false, msg: format!("无效 tone: {def_tone}") });
        return out;
    }
    let strength = def_strength.clamp(0.0, 1.0);

    let stock_bytes = match lut_core::load_stock_bytes() {
        Ok(v) => v,
        Err(e) => {
            out.push(ImportOutcome { file: String::new(), ok: false, msg: format!("原厂表不可用: {e}") });
            return out;
        }
    };
    let (header, stock_cube, tail) = lut_core::decode_table_bytes(&stock_bytes);

    let mut store = match Store::open(db_dir) {
        Ok(s) => s,
        Err(e) => {
            out.push(ImportOutcome { file: String::new(), ok: false, msg: format!("打开本地库失败: {e}") });
            return out;
        }
    };

    for path in paths {
        let src = path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
        if store.count().unwrap_or(0) >= LIB_MAX {
            out.push(ImportOutcome { file: src, ok: false, msg: format!("库已达上限 {LIB_MAX}，未导入") });
            continue;
        }
        let ext = path.extension().map(|e| e.to_string_lossy().to_lowercase()).unwrap_or_default();
        let result = (|| -> Result<(), String> {
            let (_src_size, lut) = parse_any(&path)?;
            let cc = lut_core::compose_with_strength(
                Some(&lut), &stock_cube, def_tone, strength, def_bw, &header, &tail,
            );
            if cc.len() != lut_persist::CC_TABLE_LEN {
                return Err(format!("复合结果长度异常 {} B", cc.len()));
            }
            let raw_name = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "LUT".into());
            // F-15：自定义名仅英文与数字（与前端输入限制同规则），超长截断到库上限 64。
            let name: String = raw_name
                .chars()
                .filter(|c| c.is_ascii_alphanumeric())
                .collect::<String>()
                .to_uppercase()
                .chars()
                .take(64)
                .collect();
            let name = if name.is_empty() { "LUT".into() } else { name };
            let blob = lut.to_bytes();
            store
                .insert(NewFilter {
                    name: &name,
                    tone: def_tone,
                    strength,
                    bw: def_bw,
                    cc_bytes: &cc,
                    src: &src,
                    format: &ext,
                    source_size: lut.source_size as i64,
                    locked: false,
                    cube_blob: Some(&blob),
                })
                .map_err(|e| e.to_string())?;
            Ok(())
        })();
        match result {
            Ok(()) => out.push(ImportOutcome { file: src, ok: true, msg: format!("导入成功（{ext}）") }),
            Err(e) => out.push(ImportOutcome { file: src, ok: false, msg: e }),
        }
    }
    let ok = out.iter().filter(|o| o.ok).count();
    log_line(&format!("import total={} ok={} fail={}", out.len(), ok, out.len() - ok));
    out
}

fn session_defaults(session: &Session, tone: Option<String>, strength: Option<f32>, bw: Option<bool>) -> (String, f32, bool) {
    let d = session.defaults.lock().unwrap();
    (
        tone.unwrap_or_else(|| d.tone.clone()),
        strength.unwrap_or(d.strength),
        bw.unwrap_or(d.bw),
    )
}

/// Web 选择器字节流导入（与照片选择器同一条已验证的 WebKit 面板路径）：
/// base64 → 临时暂存目录 → 复用 import_service → 清理暂存。原生 rfd 面板在本应用
/// 曾出现离屏空白窗（导入点击无响应），故选择器改走 web input。
#[derive(serde::Deserialize)]
struct UploadFile {
    name: String,
    data: String,
}

const IMPORT_MAX_BYTES: usize = 64 * 1024 * 1024;

struct TempGuard(std::path::PathBuf);
impl Drop for TempGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn import_uploaded(
    db_dir: &Path,
    files: Vec<UploadFile>,
    tone: &str,
    strength: f32,
    bw: bool,
) -> Result<Vec<ImportOutcome>, String> {
    use base64::Engine as _;
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!("dxo-import-{}-{}", std::process::id(), nanos));
    std::fs::create_dir_all(&dir).map_err(|e| format!("创建导入暂存目录失败: {e}"))?;
    let _guard = TempGuard(dir.clone());
    let mut out = Vec::new();
    let mut paths = Vec::new();
    for f in files {
        let safe = Path::new(&f.name)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_default();
        if safe.is_empty() || safe != f.name {
            out.push(ImportOutcome { file: f.name.clone(), ok: false, msg: "非法文件名（仅允许纯文件名）".into() });
            continue;
        }
        let data = match base64::engine::general_purpose::STANDARD.decode(&f.data) {
            Ok(v) => v,
            Err(e) => {
                out.push(ImportOutcome { file: f.name.clone(), ok: false, msg: format!("base64 解码失败: {e}") });
                continue;
            }
        };
        if data.len() > IMPORT_MAX_BYTES {
            out.push(ImportOutcome { file: f.name.clone(), ok: false, msg: "文件过大（>64MB），请改用拖入导入".into() });
            continue;
        }
        let p = dir.join(&safe);
        match std::fs::write(&p, &data) {
            Ok(()) => paths.push(p),
            Err(e) => out.push(ImportOutcome { file: f.name.clone(), ok: false, msg: format!("暂存失败: {e}") }),
        }
    }
    out.extend(import_service(db_dir, paths, tone, strength, bw));
    Ok(out)
}

#[tauri::command]
async fn import_bytes(
    session: tauri::State<'_, Session>,
    files: Vec<UploadFile>,
    tone: Option<String>,
    strength: Option<f32>,
    bw: Option<bool>,
) -> Result<Vec<ImportOutcome>, String> {
    let (tone, strength, bw) = session_defaults(&session, tone, strength, bw);
    tauri::async_runtime::spawn_blocking(move || {
        import_uploaded(&Store::default_db_dir(), files, &tone, strength, bw)
    })
    .await
    .map_err(|e| format!("导入线程失败: {e}"))?
}

/// 外部拖放落文件后由前端回传路径导入（与选择器同服务）。
#[tauri::command]
async fn import_paths(
    session: tauri::State<'_, Session>,
    paths: Vec<String>,
    tone: Option<String>,
    strength: Option<f32>,
    bw: Option<bool>,
) -> Result<Vec<ImportOutcome>, String> {
    let (tone, strength, bw) = session_defaults(&session, tone, strength, bw);
    let paths: Vec<PathBuf> = paths.into_iter().map(PathBuf::from).collect();
    tauri::async_runtime::spawn_blocking(move || {
        import_service(&Store::default_db_dir(), paths, &tone, strength, bw)
    })
    .await
    .map_err(|e| format!("导入线程失败: {e}"))
}

/// 「存为导入默认」：仅影响之后导入的新文件，仅本次会话。
#[tauri::command]
fn set_defaults(session: tauri::State<'_, Session>, tone: String, strength: f32, bw: bool) -> Result<(), String> {
    if !matches!(tone.as_str(), "pass" | "neutral" | "blend") {
        return Err(format!("无效 tone: {tone}"));
    }
    if !(0.0..=1.0).contains(&strength) {
        return Err("强度须在 0..1".into());
    }
    *session.defaults.lock().unwrap() = Defaults { tone, strength, bw };
    Ok(())
}

// ── 参数编辑与保存闭环（稳定 ID，成功才清 dirty） ─────────

/// 校验 + 重算，生成暂存编辑。锁定项仅允许重命名。
fn stage_edit(
    store: &Store,
    id: i64,
    name: &str,
    tone: &str,
    strength: f32,
    bw: bool,
) -> Result<PendingEdit, String> {
    let name = lut_persist::sanitize_name(name).map_err(|e| e.to_string())?;
    if !matches!(tone, "pass" | "neutral" | "blend") {
        return Err(format!("无效 tone: {tone}"));
    }
    if !(0.0..=1.0).contains(&strength) {
        return Err("强度须在 0..1".into());
    }
    let rec = store.get_record(id).map_err(|e| e.to_string())?;
    if rec.is_deleted() {
        return Err("该项已在回收站，不能编辑".into());
    }
    let params_changed = tone != rec.tone || (strength - rec.strength).abs() > 1e-6 || bw != rec.bw;
    if rec.locked && params_changed {
        return Err("卡载滤镜参数已固定，仅可重命名".into());
    }
    let cc = if params_changed {
        let ch = rec.cube_hash.as_ref().ok_or("缺少源 LUT，无法重算")?;
        let blob = store.load_cube_blob(ch).map_err(|e| e.to_string())?;
        let lut = CubeLUT::from_bytes(&blob).ok_or("源 LUT blob 损坏")?;
        let stock = lut_core::load_stock_bytes()?;
        let (header, stock_cube, tail) = lut_core::decode_table_bytes(&stock);
        let cc = lut_core::compose_with_strength(Some(&lut), &stock_cube, tone, strength, bw, &header, &tail);
        if cc.len() != lut_persist::CC_TABLE_LEN {
            return Err(format!("复合结果长度异常 {} B", cc.len()));
        }
        cc
    } else {
        store.load_bytes(id).map_err(|e| e.to_string())?
    };
    Ok(PendingEdit { name, tone: tone.into(), strength, bw, cc })
}

#[derive(Serialize, Clone)]
struct FailedItem {
    id: i64,
    msg: String,
}

#[derive(Serialize, Default)]
struct FlushReport {
    saved: Vec<i64>,
    failed: Vec<FailedItem>,
    missing: Vec<i64>,
}

/// 把暂存编辑写回本地库：仅成功项移除；失败保留供重试。
fn flush_pending(store: &mut Store, pending: &mut HashMap<i64, PendingEdit>) -> FlushReport {
    let mut report = FlushReport::default();
    let mut ids: Vec<i64> = pending.keys().copied().collect();
    ids.sort();
    for id in ids {
        let Some(ed) = pending.get(&id).cloned() else { continue };
        match store.get_record(id) {
            Ok(rec) if !rec.is_deleted() => {}
            _ => {
                pending.remove(&id);
                report.missing.push(id);
                continue;
            }
        }
        match store.update(id, &ed.name, &ed.tone, ed.strength, ed.bw, &ed.cc) {
            Ok(_) => {
                pending.remove(&id);
                report.saved.push(id);
            }
            Err(e) => report.failed.push(FailedItem { id, msg: e.to_string() }),
        }
    }
    report
}

/// 编辑选中项：即时重算并暂存（dirty），flush 后才入库。
#[tauri::command]
fn edit_lut(
    session: tauri::State<'_, Session>,
    writes: tauri::State<'_, cardops::WriteSession>,
    id: i64,
    name: String,
    tone: String,
    strength: f32,
    bw: bool,
) -> Result<serde_json::Value, String> {
    let store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    if writes.busy.load(Ordering::SeqCst) { return Err("正在写卡，暂不能修改参数".into()); }
    let ed = stage_edit(&store, id, &name, &tone, strength, bw)?;
    let hash = Store::hash_bytes(&ed.cc);
    session.pending.lock().unwrap().insert(id, ed);
    Ok(serde_json::json!({ "id": id, "hash": hash }))
}

/// 保存全部暂存编辑。失败项保留，前端 dirty 仅清 saved。
#[tauri::command]
fn flush_edits(session: tauri::State<'_, Session>) -> Result<FlushReport, String> {
    let mut pending = session.pending.lock().unwrap();
    let mut store = match Store::open(Store::default_db_dir()) {
        Ok(s) => s,
        Err(e) => {
            let msg = format!("打开本地库失败: {e}");
            return Ok(FlushReport {
                failed: pending.keys().map(|id| FailedItem { id: *id, msg: msg.clone() }).collect(),
                ..Default::default()
            });
        }
    };
    Ok(flush_pending(&mut store, &mut pending))
}

/// 放弃未保存改动（不写库）。
#[tauri::command]
fn discard_edits(session: tauri::State<'_, Session>) -> usize {
    let mut p = session.pending.lock().unwrap();
    let n = p.len();
    p.clear();
    n
}

/// 当前未保存的稳定 ID 列表。
#[tauri::command]
fn pending_ids(session: tauri::State<'_, Session>) -> Vec<i64> {
    let mut v: Vec<i64> = session.pending.lock().unwrap().keys().copied().collect();
    v.sort();
    v
}

// ── 库管理：排序 / 勾选 / 回收站 ─────────────────────────

/// 校验 id 序列与活跃库一致后事务化重写 position。
fn reorder_checked(store: &mut Store, ids: &[i64]) -> Result<(), String> {
    let mut have: Vec<i64> = store.list().map_err(|e| e.to_string())?.iter().map(|r| r.id).collect();
    let mut want = ids.to_vec();
    have.sort();
    want.sort();
    if have != want {
        return Err("排序列表与库不一致，请刷新后重试".into());
    }
    store.reorder(ids).map_err(|e| e.to_string())
}

/// 拖拽排序 / 上下移共用：前端给出完整新顺序（稳定 ID）。
#[tauri::command]
fn reorder_luts(ids: Vec<i64>) -> Result<(), String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    reorder_checked(&mut store, &ids)
}

/// 导出勾选（与选中独立，立即落库）。
#[tauri::command]
fn set_export(id: i64, on: bool) -> Result<FilterRecord, String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    store.set_export(id, on).map_err(|e| e.to_string())?;
    store.get_record(id).map_err(|e| e.to_string())
}

/// 移除到回收站（可恢复）。调用前前端须先 flush。
#[tauri::command]
fn trash_remove(session: tauri::State<'_, Session>, id: i64) -> Result<FilterRecord, String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    session.pending.lock().unwrap().remove(&id);
    store.soft_delete(id).map_err(|e| e.to_string())
}

/// 从回收站恢复（排到库末尾，导出开关复原）。
#[tauri::command]
fn trash_restore(id: i64) -> Result<FilterRecord, String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    store.restore(id).map_err(|e| e.to_string())
}

/// 永久删除单条（前端二次确认后才调用）。
#[tauri::command]
fn trash_purge(id: i64) -> Result<(), String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    store.purge(id).map_err(|e| e.to_string())
}

/// 清空回收站（前端二次确认后才调用）。
#[tauri::command]
fn trash_empty() -> Result<usize, String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    store.purge_all().map_err(|e| e.to_string())
}

/// 清空库 = 全部移入回收站（可整体恢复）。
#[tauri::command]
fn lib_clear(session: tauri::State<'_, Session>) -> Result<usize, String> {
    let mut store = Store::open(Store::default_db_dir()).map_err(|e| e.to_string())?;
    session.pending.lock().unwrap().clear();
    store.soft_delete_all().map_err(|e| e.to_string())
}

// ── 关闭闭环 ────────────────────────────────────────────

/// 前端 flush 成功后调用：放行关闭。
#[tauri::command]
fn confirm_close(session: tauri::State<'_, Session>, window: tauri::Window) -> Result<(), String> {
    session.allow_close.store(true, Ordering::SeqCst);
    window.close().map_err(|e| e.to_string())
}

fn main() {
    log_line(&format!(
        "startup version={} db_dir={} os={}",
        env!("CARGO_PKG_VERSION"),
        Store::default_db_dir().to_string_lossy(),
        std::env::consts::OS
    ));
    tauri::Builder::default()
        .manage(Session::new())
        .manage(cardops::WriteSession::default())
        .manage(preview::PreviewState::default())
        .invoke_handler(tauri::generate_handler![
            list_luts, list_trash, app_info, card_status,
            import_bytes, import_paths, set_defaults,
            edit_lut, flush_edits, discard_edits, pending_ids,
            reorder_luts, set_export,
            trash_remove, trash_restore, trash_purge, trash_empty, lib_clear,
            confirm_close,
            cardops::list_cards, cardops::card_probe,
            cardops::card_list_get, cardops::card_list_reorder, cardops::card_list_move,
            cardops::card_list_remove, cardops::card_list_append_local,
            cardops::card_list_set_menu_name,
            cardops::card_set_preload,
            cardops::card_write_plan, cardops::card_apply,
            patch::patch_info, patch::patch_plan, patch::patch_install,
            preview::preview_request, preview::preview_set_image_bytes,
            preview::preview_reset_image, preview::preview_cancel, preview::preview_image_info
        ])
        .setup(|app| {
            #[cfg(not(target_os = "macos"))]
            let _ = app;
            // F-13a：WKWebView 须显式允许 magnify 手势，trackpad 捏合才会派发
            // gesturestart/change 到 JS；页面级原生放大由前端 preventDefault 接管，
            // 缩放只作用于照片内框（见 ui 缩放块）。
            #[cfg(target_os = "macos")]
            {
                use tauri::Manager;
                if let Some(win) = app.get_webview_window("main") {
                    let _ = win.with_webview(|wv| unsafe {
                        let wk = wv.inner() as *mut objc2::runtime::AnyObject;
                        if !wk.is_null() {
                            let _: () = objc2::msg_send![wk, setAllowsMagnification: true];
                        }
                    });
                }
            }
            Ok(())
        })
        .on_window_event(|window, event| match event {
            tauri::WindowEvent::CloseRequested { api, .. } => {
                let session = window.state::<Session>();
                if session.allow_close.load(Ordering::SeqCst) {
                    return;
                }
                api.prevent_close();
                let _ = window.emit("close-requested", ());
            }
            tauri::WindowEvent::DragDrop(ev) => match ev {
                tauri::DragDropEvent::Enter { .. } | tauri::DragDropEvent::Over { .. } => {
                    let _ = window.emit("drop-hover", true);
                }
                tauri::DragDropEvent::Leave => {
                    let _ = window.emit("drop-hover", false);
                }
                tauri::DragDropEvent::Drop { paths, .. } => {
                    let _ = window.emit("drop-hover", false);
                    let session = window.state::<Session>();
                    let d = session.defaults.lock().unwrap().clone();
                    let handle = window.app_handle().clone();
                    let paths = paths.clone();
                    tauri::async_runtime::spawn_blocking(move || {
                        let out = import_service(&Store::default_db_dir(), paths, &d.tone, d.strength, d.bw);
                        let _ = handle.emit("import-results", &out);
                    });
                }
                _ => {}
            },
            _ => {}
        })
        .build(tauri::generate_context!())
        .expect("tauri 应用启动失败")
        .run(|app, event| {
            if let tauri::RunEvent::ExitRequested { api, .. } = event {
                if !app.state::<Session>().allow_close.load(Ordering::SeqCst) {
                    api.prevent_exit();
                    let _ = app.emit("close-requested", ());
                }
            }
        });
}

// ── 针对性测试（R2：排序×dirty、保存失败保留、拖放与点击同服务） ──

#[cfg(test)]
mod tests {
    use super::*;

    fn cube_blob() -> Vec<u8> {
        lut_core::cube::cube_lut_from_flat(2, &[[0.2, 0.4, 0.6]; 8]).unwrap().to_bytes()
    }

    fn cc(seed: u8) -> Vec<u8> {
        let mut v = vec![0xAB; lut_persist::CC_TABLE_LEN];
        v[100] = seed;
        v
    }

    fn insert(store: &mut Store, name: &str, seed: u8, locked: bool, with_cube: bool) -> FilterRecord {
        let blob = cube_blob();
        store
            .insert(NewFilter {
                name,
                tone: "pass",
                strength: 0.65,
                bw: false,
                cc_bytes: &cc(seed),
                src: "t.cube",
                format: "cube",
                source_size: 2,
                locked,
                cube_blob: if with_cube { Some(&blob) } else { None },
            })
            .unwrap()
    }

    fn write_cube(path: &Path) {
        let mut s = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    s += &format!("{r} {g} {b}\n");
                }
            }
        }
        std::fs::write(path, s).unwrap();
    }

    #[test]
    fn test_defaults_initial_web_baseline() {
        let d = Defaults::initial();
        assert_eq!(d.tone, "pass");
        assert!((d.strength - 0.65).abs() < 1e-6);
        assert!(!d.bw);
    }

    /// 排序 × dirty 交叉：暂存编辑按稳定 ID 落回本人，排序不串项。
    #[test]
    fn test_sort_x_dirty_cross() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = insert(&mut store, "A", 1, false, true);
        let b = insert(&mut store, "B", 2, false, true);

        let mut pending = HashMap::new();
        pending.insert(a.id, stage_edit(&store, a.id, "A2", "neutral", 0.5, true).unwrap());
        pending.insert(b.id, stage_edit(&store, b.id, "B2", "blend", 1.0, false).unwrap());

        reorder_checked(&mut store, &[b.id, a.id]).unwrap();
        let rep = flush_pending(&mut store, &mut pending);
        assert_eq!(rep.saved.len(), 2);
        assert!(rep.failed.is_empty());
        assert!(pending.is_empty(), "成功项必须清 dirty");

        let list = store.list().unwrap();
        assert_eq!(list[0].id, b.id);
        assert_eq!(list[1].id, a.id);
        let ga = store.get_record(a.id).unwrap();
        let gb = store.get_record(b.id).unwrap();
        assert_eq!(ga.name, "A2");
        assert_eq!(ga.tone, "neutral");
        assert!(ga.bw);
        assert_eq!(gb.name, "B2");
        assert_eq!(gb.tone, "blend");
        assert!(!gb.bw);
    }

    /// 保存失败保留 dirty，修复后重试成功（unix：只读目录注入写失败）。
    #[test]
    #[cfg(unix)]
    fn test_flush_failure_retains_and_retry() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = insert(&mut store, "A", 1, false, true);
        let mut pending = HashMap::new();
        pending.insert(a.id, stage_edit(&store, a.id, "A", "neutral", 0.5, false).unwrap());

        let store_dir = dir.path().join("store");
        std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o555)).unwrap();
        let rep = flush_pending(&mut store, &mut pending);
        assert_eq!(rep.failed.len(), 1, "写失败必须报告");
        assert!(pending.contains_key(&a.id), "失败项 dirty 必须保留");
        assert_eq!(store.get_record(a.id).unwrap().tone, "pass", "失败不得写库");

        std::fs::set_permissions(&store_dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        let rep = flush_pending(&mut store, &mut pending);
        assert_eq!(rep.saved, vec![a.id]);
        assert!(pending.is_empty());
        assert_eq!(store.get_record(a.id).unwrap().tone, "neutral");
    }

    /// 拖放与点击同服务：同一 import_service，逐项结果、失败不污染库、上限 100。
    #[test]
    fn test_drop_and_click_same_service() {
        let dir = tempfile::tempdir().unwrap();
        let good = dir.path().join("good.cube");
        let good2 = dir.path().join("good2.cube");
        let bad = dir.path().join("bad.txt");
        write_cube(&good);
        write_cube(&good2);
        std::fs::write(&bad, "not a lut").unwrap();

        // 点击入口与拖放入口调用同一服务，结果一致
        let click = import_service(dir.path(), vec![good.clone()], "pass", 0.65, false);
        let drop = import_service(dir.path(), vec![good2.clone()], "pass", 0.65, false);
        assert!(click[0].ok && drop[0].ok);
        assert_eq!(click[0].msg, drop[0].msg);

        // 混合：有效 + 损坏 + 不支持，逐项报告，失败不产生半条记录
        let mixed = import_service(dir.path(), vec![good.clone(), bad.clone()], "pass", 0.65, false);
        assert!(mixed[0].ok);
        assert!(!mixed[1].ok);
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.count().unwrap(), 3);

        // 上限：补到 100 后再导 → 逐项报上限，库不变
        let mut store = store;
        while store.count().unwrap() < LIB_MAX {
            insert(&mut store, "X", 9, false, false);
        }
        let over = import_service(dir.path(), vec![good.clone()], "pass", 0.65, false);
        assert!(!over[0].ok);
        assert!(over[0].msg.contains("上限"));
        assert_eq!(store.count().unwrap(), LIB_MAX);
    }

    /// F-15：导入默认自定义名仅英文与数字、≤64；中文/符号过滤，全过滤回退 LUT。
    #[test]
    fn test_import_default_name_alnum_only() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let mut cube_text = String::from("LUT_3D_SIZE 2\n");
        for r in 0..2 {
            for g in 0..2 {
                for b in 0..2 {
                    cube_text += &format!("{r} {g} {b}\n");
                }
            }
        }
        let data = base64::engine::general_purpose::STANDARD.encode(&cube_text);
        let files = vec![
            UploadFile { name: "温暖K12_测试.cube".into(), data: data.clone() },
            UploadFile { name: "———.cube".into(), data: data.clone() },
            UploadFile { name: format!("{}.cube", "x".repeat(80)), data },
        ];
        let out = import_uploaded(dir.path(), files, "pass", 0.65, false).unwrap();
        assert!(out.iter().all(|o| o.ok), "{out:?}");
        let store = Store::open(dir.path()).unwrap();
        let names: Vec<String> = store.list().unwrap().iter().map(|r| r.name.clone()).collect();
        assert!(names.contains(&"K12".to_string()), "{names:?}");
        assert!(names.contains(&"LUT".to_string()), "{names:?}");
        assert!(names.iter().any(|n| n.len() == 64 && n.chars().all(|c| c == 'X')), "{names:?}");
    }

    /// F-14：Web 选择器字节流导入——合法 cube 入库；路径穿越/坏 base64 逐项拒绝；暂存目录用后即清。
    #[test]
    fn test_import_uploaded_bytes() {
        use base64::Engine as _;
        let dir = tempfile::tempdir().unwrap();
        let mut cube_text = String::from("LUT_3D_SIZE 2\n");
        for b in 0..2 {
            for g in 0..2 {
                for r in 0..2 {
                    cube_text += &format!("{r} {g} {b}\n");
                }
            }
        }
        let enc = |s: &str| base64::engine::general_purpose::STANDARD.encode(s);
        let files = vec![
            UploadFile { name: "goodup.cube".into(), data: enc(&cube_text) },
            UploadFile { name: "../evil.cube".into(), data: enc(&cube_text) },
            UploadFile { name: "badb64.cube".into(), data: "!!!not-base64!!!".into() },
        ];
        let out = import_uploaded(dir.path(), files, "pass", 0.65, false).unwrap();
        assert_eq!(out.len(), 3);
        let get = |n: &str| out.iter().find(|o| o.file == n).unwrap();
        assert!(get("goodup.cube").ok, "{:?}", get("goodup.cube"));
        assert!(!get("../evil.cube").ok, "路径穿越必须拒绝");
        assert!(!get("badb64.cube").ok, "坏 base64 必须拒绝");
        let store = Store::open(dir.path()).unwrap();
        assert_eq!(store.count().unwrap(), 1, "失败项不得入库");
        assert_eq!(store.list().unwrap()[0].name, "GOODUP");
        let prefix = format!("dxo-import-{}-", std::process::id());
        // 同进程其它导入测试并行运行时会有短暂在途暂存目录；泄漏的目录则永不消失
        let mut leftovers = Vec::new();
        for _ in 0..50 {
            leftovers = std::fs::read_dir(std::env::temp_dir())
                .unwrap()
                .filter_map(|e| e.ok())
                .filter(|e| e.file_name().to_string_lossy().starts_with(&prefix))
                .collect();
            if leftovers.is_empty() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        assert!(leftovers.is_empty(), "暂存目录必须清理: {leftovers:?}");
    }

    /// 锁定项：参数编辑被拒，重命名允许。
    #[test]
    fn test_locked_edit_rules() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = insert(&mut store, "CARD", 1, true, false);
        assert!(stage_edit(&store, a.id, "CARD", "neutral", 0.5, false).is_err());
        let ed = stage_edit(&store, a.id, "RENAMED", "pass", 0.65, false).unwrap();
        let mut pending = HashMap::new();
        pending.insert(a.id, ed);
        let rep = flush_pending(&mut store, &mut pending);
        assert_eq!(rep.saved, vec![a.id]);
        assert_eq!(store.get_record(a.id).unwrap().name, "RENAMED");
    }

    /// 排序校验：id 集合与库不一致必须拒绝。
    #[test]
    fn test_reorder_rejects_mismatch() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = insert(&mut store, "A", 1, false, false);
        let b = insert(&mut store, "B", 2, false, false);
        assert!(reorder_checked(&mut store, &[a.id]).is_err());
        assert!(reorder_checked(&mut store, &[a.id, b.id, 999]).is_err());
        assert!(reorder_checked(&mut store, &[b.id, a.id]).is_ok());
    }

    /// 编辑重算确实改变 CC 表 hash（参数即时作用）。
    #[test]
    fn test_edit_recompute_changes_hash() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = insert(&mut store, "A", 1, false, true);
        let before = store.get_record(a.id).unwrap().hash;
        let ed = stage_edit(&store, a.id, "A", "blend", 1.0, true).unwrap();
        assert_ne!(Store::hash_bytes(&ed.cc), before);
        let mut pending = HashMap::new();
        pending.insert(a.id, ed);
        flush_pending(&mut store, &mut pending);
        assert_ne!(store.get_record(a.id).unwrap().hash, before);
    }
}
