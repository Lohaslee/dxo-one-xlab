//! DXO-ONE-XLAB · 原生 GUI（Iced 0.13）
//!
//! 交互模型对齐网页版（`工具/index.html` + `工具/filt_server.py`）：
//! - 导入即入库、自动选中、按「导入默认参数」初值
//! - 库顺序 = 菜单环序；每行有「导出」开关与行内删除
//! - 参数改动直接作用于选中项（没有「保存」这一步）
//! - 预览支持并排 / 拖动对比，可换任意预览图
//! - 导出到存储卡 `DXO_FILTERS/<名>/`（.bin + 前后预览 + manifest）
//! - 可读取卡上已有滤镜并设为「默认开机」
//!
//! 现行视觉规范见 `docs/UI-设计规范.md`（嘻哈视觉规范 v1.7）；本文件已按该规范重绘：
//! 近黑画布 + 浅色白卡 + 单一 DxO 蓝绿 accent、零圆角、2px 硬边、无阴影。
//! 交互状态机（①–⑦ / 库即真相 / 回收站 / 写卡）不受视觉迁移影响；本文件只做装配与状态机。

mod theme;

use iced::alignment::Horizontal;
use iced::widget::image as img_widget;
use iced::widget::image::Handle;
use iced::widget::{button, column, container, horizontal_space, row, scrollable, slider, text, text_input};
use iced::{Alignment, Background, Border, Element, Length, Shadow, Subscription, Task, Theme};
use lut_core::cube::{read_cube, CubeLUT};
use lut_core::table::decode_table;
use lut_core::{compose_with_strength, render, N};
use lut_persist::{NewFilter, Store};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use theme::{color, size, space, text as t, Kind};

/// 数据目录：`DXO_DATA_DIR` 可覆盖；默认在操作系统应用数据目录。
/// 原厂基线已编译期打包进 lut-core，不再引用开发机绝对路径。
fn db_dir() -> PathBuf {
    lut_persist::Store::default_db_dir()
}

/// 默认仍是导入滤镜；`DXO_START_PAGE` 仅用于同尺寸截图验收时指定启动页。
fn start_page() -> AppPage {
    match std::env::var("DXO_START_PAGE").as_deref() {
        Ok("patch") => AppPage::Patch,
        Ok("about") => AppPage::About,
        _ => AppPage::Import,
    }
}

/// 单个滤镜可写卡的容量上限（与相机自定义档位一致）
const CUSTOM_MAX: usize = 5;
/// 库软上限（预览可很多，导出才受 CUSTOM_MAX 限制）
const LIB_MAX: usize = 100;

/// 并排模式单帧图像目标像素宽
const SPLIT_IMG_W: u32 = 340;
/// 拖动对比模式图像目标像素宽
const SLIDER_IMG_W: u32 = 640;
/// 预览图最长边上限（与网页版一致，避免大图每次改参数都重算千万像素）
const TEST_IMG_MAX: u32 = 1024;

// ---------------------------------------------------------------- 数据

#[derive(Default, Clone)]
struct TestImage {
    w: u32,
    h: u32,
    rgb: Vec<u8>,
    label: String,
}

#[derive(Clone)]
struct LibItem {
    id: i64,
    name: String,
    fmt: String,
    source_size: i64,
    src: String,
    tone: String,
    strength: f32,
    bw: bool,
    export: bool,
    locked: bool,
    cube: Option<CubeLUT>,
    cc: Vec<u8>,
}

impl LibItem {
    fn param_text(&self) -> String {
        let mut s = format!("{} · {:.2}", tone_label(&self.tone), self.strength);
        if self.bw {
            s.push_str(" · 黑白");
        }
        s
    }
}

/// 回收站条目（软删除记录，内容文件仍在，可恢复）。
#[derive(Clone)]
struct TrashItem {
    id: i64,
    name: String,
    src: String,
    tone: String,
    strength: f32,
    /// RFC3339 删除时间
    deleted_at: String,
}

impl TrashItem {
    fn sub_text(&self) -> String {
        format!(
            "{} · {} · 删除于 {}",
            tone_label(&self.tone),
            truncate(&self.src, 22),
            short_time(&self.deleted_at)
        )
    }
}

/// 把 RFC3339（`2026-11-12T09:07:03.123+00:00`）压成 `11-12 17:07`（本地时区）。
fn short_time(rfc3339: &str) -> String {
    match chrono::DateTime::parse_from_rfc3339(rfc3339) {
        Ok(dt) => dt.with_timezone(&chrono::Local).format("%m-%d %H:%M").to_string(),
        Err(_) => rfc3339.chars().take(16).collect(),
    }
}

#[derive(Clone)]
struct CardFilter {
    dir: String,
    name: String,
    source: String,
    format: String,
    tone: String,
    strength: f32,
    bw: bool,
    bin_size: u64,
}

struct Stock {
    header: [u8; 128],
    cube: [[[[f32; 3]; N]; N]; N],
    tail: [u8; 1024],
}

impl Stock {
    fn load() -> Option<Self> {
        let bytes = lut_core::load_stock_bytes().ok()?;
        let (header, cube, tail) = decode_table(&bytes);
        Some(Stock { header, cube, tail })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ViewMode {
    Split,
    Slider,
}

/// 三页固定导航；默认进入导入滤镜。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AppPage {
    Import,
    Patch,
    About,
}

impl AppPage {
    fn label(self) -> &'static str {
        match self {
            AppPage::Import => "导入滤镜",
            AppPage::Patch => "更新补丁",
            AppPage::About => "关于我们",
        }
    }

    fn index_label(self) -> &'static str {
        match self {
            AppPage::Import => "01",
            AppPage::Patch => "02",
            AppPage::About => "03",
        }
    }

    const ALL: [AppPage; 3] = [AppPage::Import, AppPage::Patch, AppPage::About];
}

#[derive(Debug, Clone)]
enum Message {
    PickFiles,
    ClearLib,
    Select(usize),
    ToggleExport(usize),
    DeleteItem(usize),
    ToggleTrash,
    RestoreItem(i64),
    PurgeItem(i64),
    PurgeAll,
    MoveUp,
    MoveDown,
    NameEdited(String),
    ToneSet(&'static str),
    StrengthSet(f32),
    BwSet(bool),
    SaveDefault,
    PickTestImage,
    ResetTestImage,
    ViewModeChanged(ViewMode),
    PosChanged(f32),
    RefreshCard,
    ToggleKeepOriginal,
    ReadCard,
    ToggleCardDefault(String),
    LoadCardItem(String),
    AskExport,
    ConfirmExport,
    CancelExport,
    /// 用户请求关闭窗口：先保存，失败则取消关闭
    CloseRequested(iced::window::Id),
    /// 保存失败后重试并继续关闭
    RetrySaveAndClose,
    /// 明确放弃未保存改动后退出
    DiscardAndClose,
    /// 三页导航切换
    SwitchPage(AppPage),
}

/// 一次批量保存的结果；failed 非空表示仍有未落盘改动。
#[derive(Default)]
struct FlushReport {
    saved: usize,
    failed: Vec<(i64, String)>,
    missing: Vec<i64>,
}

impl FlushReport {
    fn ok(&self) -> bool {
        self.failed.is_empty()
    }
}

/// 按稳定 ID 把改动写回本地库：仅成功项从 dirty 移除。
///
/// 抽成纯函数便于用临时数据库测试「保存失败仍保留」「排序后不串项」。
fn flush_items(store: &mut Store, items: &[LibItem], dirty: &mut HashSet<i64>) -> FlushReport {
    let mut report = FlushReport::default();
    let ids: Vec<i64> = dirty.iter().copied().collect();
    for id in ids {
        let Some(it) = items.iter().find(|x| x.id == id) else {
            // 记录已被删除，没有可保存对象
            dirty.remove(&id);
            report.missing.push(id);
            continue;
        };
        if it.cc.len() != lut_persist::CC_TABLE_LEN {
            report.failed.push((id, format!("CC 表长度 {} 不合法", it.cc.len())));
            continue;
        }
        match store.update(it.id, &it.name, &it.tone, it.strength, it.bw, &it.cc) {
            Ok(_) => {
                dirty.remove(&id);
                report.saved += 1;
            }
            Err(e) => report.failed.push((id, e.to_string())),
        }
    }
    report
}

struct App {
    page: AppPage,
    items: Vec<LibItem>,
    sel: Option<usize>,
    name_buf: String,
    /// 按数据库记录 ID 记录未保存改动，排序/删除不会串项
    dirty: HashSet<i64>,
    /// 最近一次保存失败提示（供状态栏显示）
    save_error: Option<String>,
    /// 因保存失败而挂起的关闭请求
    pending_close: Option<iced::window::Id>,

    /// 回收站（软删除）
    trash: Vec<TrashItem>,
    show_trash: bool,
    purge_armed: bool,
    /// 单条「彻底删除」的二次确认目标
    purge_armed_id: Option<i64>,

    def_tone: String,
    def_strength: f32,
    def_bw: bool,

    stock: Option<Stock>,
    test_img: Option<TestImage>,

    before_rgb: Vec<u8>,
    after_rgb: Vec<u8>,
    preview_w: u32,
    preview_h: u32,

    view_mode: ViewMode,
    pos: f32,
    split_w: u32,
    split_h: u32,
    slider_size: (u32, u32),
    before_handle: Option<Handle>,
    after_handle: Option<Handle>,
    slider_handle: Option<Handle>,
    slider_before: Vec<u8>,
    slider_after: Vec<u8>,

    card_mount: Option<PathBuf>,
    card_status: String,
    card_filters: Vec<CardFilter>,
    card_default: Option<String>,
    card_note: String,
    keep_original: bool,

    export_confirm: bool,
    clear_armed: bool,

    status: String,
    log: Vec<String>,
}

impl App {
    fn new() -> Self {
        let stock = Stock::load();
        let (w, h, chart) = render::build_test_chart();
        let mut app = Self {
            page: start_page(),
            items: Vec::new(),
            sel: None,
            name_buf: String::new(),
            dirty: HashSet::new(),
            save_error: None,
            pending_close: None,
            trash: Vec::new(),
            show_trash: false,
            purge_armed: false,
            purge_armed_id: None,
            def_tone: "pass".into(),
            def_strength: 0.65,
            def_bw: false,
            stock,
            test_img: None,
            before_rgb: chart.clone(),
            after_rgb: chart,
            preview_w: w,
            preview_h: h,
            view_mode: ViewMode::Split,
            pos: 0.5,
            split_w: SPLIT_IMG_W,
            split_h: (SPLIT_IMG_W as f32 * h as f32 / w as f32) as u32,
            slider_size: (SLIDER_IMG_W, 1),
            before_handle: None,
            after_handle: None,
            slider_handle: None,
            slider_before: Vec::new(),
            slider_after: Vec::new(),
            card_mount: None,
            card_status: "检测中…".into(),
            card_filters: Vec::new(),
            card_default: None,
            card_note: String::new(),
            keep_original: true,
            export_confirm: false,
            clear_armed: false,
            status: "就绪 · 导入 LUT 开始".into(),
            log: vec!["就绪 · 导入 LUT 开始".into()],
        };
        app.refresh_card();
        app.load_lib();
        if app.sel.is_none() && !app.items.is_empty() {
            app.sel = Some(0);
        }
        app.sync_editor_from_sel();
        app.recompute_preview();
        app
    }

    // ------------------------------------------------------------ 日志

    fn push_log(&mut self, s: impl Into<String>) {
        let s = s.into();
        self.status = s.clone();
        self.log.push(s);
        if self.log.len() > 200 {
            self.log.drain(0..self.log.len() - 200);
        }
    }

    // ------------------------------------------------------------ 库

    fn load_lib(&mut self) {
        let store = match Store::open(db_dir()) {
            Ok(s) => s,
            Err(e) => {
                self.push_log(format!("打开本地库失败：{e}"));
                return;
            }
        };
        let records = match store.list() {
            Ok(r) => r,
            Err(e) => {
                self.push_log(format!("读取本地库失败：{e}"));
                return;
            }
        };
        let mut items: Vec<LibItem> = Vec::with_capacity(records.len());
        for r in records {
            let cc = store.load_bytes(r.id).unwrap_or_default();
            let cube = r
                .cube_hash
                .as_ref()
                .and_then(|h| store.load_cube_blob(h).ok())
                .and_then(|b| CubeLUT::from_bytes(&b));
            items.push(LibItem {
                id: r.id,
                name: r.name,
                fmt: r.format,
                source_size: r.source_size,
                src: r.src,
                tone: r.tone,
                strength: r.strength,
                bw: r.bw,
                export: r.export,
                locked: r.locked,
                cube,
                cc,
            });
        }
        self.items = items;
        if self.sel.is_some() && self.sel.unwrap() >= self.items.len() {
            self.sel = self.items.len().checked_sub(1);
        }
        self.load_trash();
    }

    /// 重新读取回收站（软删除条目，按删除时间倒序）。
    fn load_trash(&mut self) {
        let store = match Store::open(db_dir()) {
            Ok(s) => s,
            Err(e) => {
                self.push_log(format!("打开本地库失败：{e}"));
                return;
            }
        };
        match store.list_deleted() {
            Ok(recs) => {
                self.trash = recs
                    .into_iter()
                    .map(|r| TrashItem {
                        id: r.id,
                        name: r.name,
                        src: r.src,
                        tone: r.tone,
                        strength: r.strength,
                        deleted_at: r.deleted_at.unwrap_or_default(),
                    })
                    .collect();
            }
            Err(e) => self.push_log(format!("读取回收站失败：{e}")),
        }
    }

    /// 把内存里改过的参数写回本地库；失败项的 dirty 必须保留。
    fn flush_dirty(&mut self) -> FlushReport {
        if self.dirty.is_empty() {
            self.save_error = None;
            return FlushReport::default();
        }
        let mut store = match Store::open(db_dir()) {
            Ok(s) => s,
            Err(e) => {
                let n = self.dirty.len();
                let msg = format!("打开本地库失败：{e}；{n} 项未保存");
                self.save_error = Some(msg.clone());
                self.push_log(msg);
                return FlushReport {
                    failed: self.dirty.iter().map(|id| (*id, "打开本地库失败".into())).collect(),
                    ..Default::default()
                };
            }
        };
        let report = flush_items(&mut store, &self.items, &mut self.dirty);
        if report.ok() {
            self.save_error = None;
            self.push_log(format!("保存完成：{} 项已落盘，未保存 0 项", report.saved));
        } else {
            let msg = format!("{} 项保存失败，改动仍保留", report.failed.len());
            self.save_error = Some(msg.clone());
            self.push_log(format!("{msg}；可点「重试保存」"));
            for (id, e) in &report.failed {
                self.push_log(format!("  id={id}: {e}"));
            }
        }
        report
    }

    /// 窗口关闭请求订阅：保存成功才允许关闭。
    fn subscription(&self) -> Subscription<Message> {
        iced::window::close_requests().map(Message::CloseRequested)
    }

    fn compose_for(
        &self,
        lut: Option<&CubeLUT>,
        tone: &str,
        strength: f32,
        bw: bool,
    ) -> Vec<u8> {
        match &self.stock {
            Some(s) => compose_with_strength(lut, &s.cube, tone, strength, bw, &s.header, &s.tail),
            None => Vec::new(),
        }
    }

    /// 当前应显示的成品表：有选中项用该项，否则原厂直出。
    fn current_cc(&self) -> Vec<u8> {
        match self.sel.and_then(|i| self.items.get(i)) {
            Some(it) => it.cc.clone(),
            None => self.compose_for(None, "pass", 1.0, false),
        }
    }

    fn export_count(&self) -> usize {
        self.items.iter().filter(|i| i.export).count()
    }

    // ------------------------------------------------------------ 导入

    fn import_files(&mut self, paths: Vec<PathBuf>) {
        if paths.is_empty() {
            return;
        }
        self.flush_dirty();
        let mut added = 0usize;
        for path in paths {
            if self.items.len() >= LIB_MAX {
                self.push_log(format!("预览库已达上限 {LIB_MAX}"));
                break;
            }
            let ext = path
                .extension()
                .map(|e| e.to_string_lossy().to_lowercase())
                .unwrap_or_default();
            // R2-03：按格式分派（对齐 Web 版 filt_core.load_lut 支持面）。
            let loaded: Result<(usize, lut_core::cube::CubeLUT), String> = match ext.as_str() {
                "cube" => read_cube(&path.to_string_lossy()),
                "3dl" => std::fs::read_to_string(&path)
                    .map_err(|e| format!("读取失败: {e}"))
                    .and_then(|t| {
                        lut_core::formats::parse_3dl_text(&t)
                            .and_then(lut_core::formats::grid_to_cube)
                            .map(|c| (c.source_size, c))
                    }),
                "csp" => std::fs::read_to_string(&path)
                    .map_err(|e| format!("读取失败: {e}"))
                    .and_then(|t| {
                        lut_core::formats::parse_csp_text(&t)
                            .and_then(lut_core::formats::grid_to_cube)
                            .map(|c| (c.source_size, c))
                    }),
                "png" | "jpg" | "jpeg" | "tif" | "tiff" => image::open(&path)
                    .map_err(|e| format!("图像解码失败: {e}"))
                    .and_then(|img| {
                        let rgb = img.to_rgb8();
                        lut_core::formats::hald_from_pixels(rgb.width(), rgb.height(), rgb.as_raw())
                            .and_then(lut_core::formats::grid_to_cube)
                            .map(|c| (c.source_size, c))
                    }),
                _ => Err("暂不支持该格式（支持 .cube/.3dl/.csp/Hald 图像）".into()),
            };
            let (_src_size, lut) = match loaded {
                Ok(v) => v,
                Err(e) => {
                    self.push_log(format!("解析失败 {}：{e}", file_name(&path)));
                    continue;
                }
            };
            let lut = lut;

            let tone = self.def_tone.clone();
            let strength = self.def_strength;
            let bw = self.def_bw;
            let cc = self.compose_for(Some(&lut), &tone, strength, bw);
            if cc.len() != lut_persist::CC_TABLE_LEN {
                self.push_log("复合失败：原厂表不可用".to_string());
                continue;
            }
            let raw_name = path
                .file_stem()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_else(|| "LUT".into());
            let name: String = if raw_name.trim().is_empty() {
                "LUT".into()
            } else {
                raw_name.trim().to_uppercase().chars().take(20).collect()
            };
            let src = file_name(&path);
            let blob = lut.to_bytes();

            let mut store = match Store::open(db_dir()) {
                Ok(s) => s,
                Err(e) => {
                    self.push_log(format!("打开本地库失败：{e}"));
                    return;
                }
            };
            match store.insert(NewFilter {
                name: &name,
                tone: &tone,
                strength,
                bw,
                cc_bytes: &cc,
                src: &src,
                format: &ext,
                source_size: lut.source_size as i64,
                locked: false,
                cube_blob: Some(&blob),
            }) {
                Ok(_) => {
                    added += 1;
                    self.push_log(format!(
                        "✓ 导入 {src}（{ext} · {}³ → 16³ · {}）",
                        lut.source_size,
                        tone_label(&tone)
                    ));
                }
                Err(e) => self.push_log(format!("入库失败 {src}：{e}")),
            }
        }
        if added > 0 {
            self.load_lib();
            self.sel = self.items.len().checked_sub(1);
            self.sync_editor_from_sel();
            self.recompute_preview();
        }
    }

    // ------------------------------------------------------------ 选中 / 编辑

    fn sync_editor_from_sel(&mut self) {
        self.name_buf = self
            .sel
            .and_then(|i| self.items.get(i))
            .map(|it| it.name.clone())
            .unwrap_or_default();
    }

    fn select(&mut self, i: usize) {
        if i >= self.items.len() || self.sel == Some(i) {
            return;
        }
        self.flush_dirty();
        self.sel = Some(i);
        self.sync_editor_from_sel();
        if let Some(it) = self.items.get(i) {
            self.push_log(format!("已选中「{}」", it.name));
        }
        self.recompute_preview();
    }

    /// 参数改动后重算该滤镜的成品表，并刷新预览。
    fn reapply_params(&mut self) {
        let Some(i) = self.sel.filter(|i| *i < self.items.len()) else {
            return;
        };
        let Some(it) = self.items.get(i) else { return };
        if it.locked {
            return;
        }
        let Some(lut) = it.cube.clone() else { return };
        let (tone, strength, bw) = (it.tone.clone(), it.strength, it.bw);
        let cc = self.compose_for(Some(&lut), &tone, strength, bw);
        if cc.len() != lut_persist::CC_TABLE_LEN {
            return;
        }
        let id = self.items[i].id;
        self.items[i].cc = cc;
        self.dirty.insert(id);
        self.recompute_preview();
    }

    // ------------------------------------------------------------ 预览

    fn recompute_preview(&mut self) {
        let cc = self.current_cc();
        let (w, h, rgb) = match &self.test_img {
            Some(img) if !img.rgb.is_empty() => (img.w, img.h, img.rgb.clone()),
            _ => render::build_test_chart(),
        };
        let after = if cc.len() >= lut_core::TABLE_LEN {
            render::render_rgb(&cc, w, h, &rgb)
        } else {
            rgb.clone()
        };
        self.before_rgb = rgb;
        self.after_rgb = after;
        self.preview_w = w;
        self.preview_h = h;
        self.rebuild_images();
    }

    fn aspect(&self) -> f32 {
        self.preview_h.max(1) as f32 / self.preview_w.max(1) as f32
    }

    fn rebuild_images(&mut self) {
        let aspect = self.aspect();
        let sw = SPLIT_IMG_W;
        let sh = ((sw as f32 * aspect).round() as u32).max(1);
        self.split_w = sw;
        self.split_h = sh;
        self.before_handle = Some(Handle::from_rgba(
            sw,
            sh,
            resize_rgba(&self.before_rgb, self.preview_w, self.preview_h, sw, sh),
        ));
        self.after_handle = Some(Handle::from_rgba(
            sw,
            sh,
            resize_rgba(&self.after_rgb, self.preview_w, self.preview_h, sw, sh),
        ));

        let tw = SLIDER_IMG_W;
        let th = ((tw as f32 * aspect).round() as u32).max(1);
        self.slider_size = (tw, th);
        self.slider_before = resize_rgba(&self.before_rgb, self.preview_w, self.preview_h, tw, th);
        self.slider_after = resize_rgba(&self.after_rgb, self.preview_w, self.preview_h, tw, th);
        self.rebuild_slider_image();
    }

    fn rebuild_slider_image(&mut self) {
        let (w, h) = self.slider_size;
        if self.slider_before.len() != (w * h * 4) as usize {
            return;
        }
        let rgba = composite_slider(&self.slider_before, &self.slider_after, w, h, self.pos);
        self.slider_handle = Some(Handle::from_rgba(w, h, rgba));
    }

    fn set_test_image(&mut self, path: Option<PathBuf>) {
        match path {
            None => {
                self.test_img = None;
                self.push_log("已重置为默认色卡");
            }
            Some(p) => match load_test_image(&p) {
                Ok(img) => {
                    self.push_log(format!(
                        "预览图已更换：{}（{}×{}）",
                        img.label, img.w, img.h
                    ));
                    self.test_img = Some(img);
                }
                Err(e) => self.push_log(format!("预览图读取失败：{e}")),
            },
        }
        self.recompute_preview();
    }

    // ------------------------------------------------------------ 卡

    fn refresh_card(&mut self) {
        match lut_camera::find_card() {
            Ok(card) => {
                self.card_mount = card.mount_point.clone();
                self.card_status = match &card.mount_point {
                    Some(p) => format!(
                        "{} · {}",
                        card.volume_name.clone().unwrap_or_else(|| card.device.clone()),
                        p.display()
                    ),
                    None => format!("{} · 未挂载", card.device),
                };
            }
            Err(_) => {
                self.card_mount = None;
                self.card_status = "未检测到相机卡".into();
            }
        }
    }

    fn dxo_dir(&self) -> Option<PathBuf> {
        self.card_mount.as_ref().map(|m| m.join("DXO_FILTERS"))
    }

    fn read_card(&mut self) {
        let Some(dxo) = self.dxo_dir() else {
            self.push_log("请先插入并挂载相机存储卡");
            return;
        };
        if !dxo.is_dir() {
            self.card_filters.clear();
            self.card_default = None;
            self.card_note = "卡上未找到 DXO_FILTERS 目录（尚无本工具导出的滤镜）".into();
            self.push_log(self.card_note.clone());
            return;
        }
        let mut out: Vec<CardFilter> = Vec::new();
        let Ok(rd) = std::fs::read_dir(&dxo) else {
            self.push_log("读取 DXO_FILTERS 失败");
            return;
        };
        for e in rd.flatten() {
            let sub = e.path();
            if !sub.is_dir() {
                continue;
            }
            let dir = e.file_name().to_string_lossy().to_string();
            let bin = sub.join(format!("{dir}.bin"));
            if !bin.is_file() {
                continue;
            }
            let man: serde_json::Value = std::fs::read_to_string(sub.join("manifest.json"))
                .ok()
                .and_then(|s| serde_json::from_str(&s).ok())
                .unwrap_or(serde_json::Value::Null);
            let gs = |k: &str| man.get(k).and_then(|v| v.as_str()).unwrap_or("").to_string();
            out.push(CardFilter {
                name: {
                    let n = gs("display_name");
                    if n.is_empty() {
                        dir.clone()
                    } else {
                        n
                    }
                },
                source: gs("source"),
                format: {
                    let f = gs("format");
                    if f.is_empty() {
                        "bin".into()
                    } else {
                        f
                    }
                },
                tone: {
                    let v = gs("tone");
                    if v.is_empty() {
                        "pass".into()
                    } else {
                        v
                    }
                },
                strength: man
                    .get("strength")
                    .and_then(|v| v.as_f64())
                    .unwrap_or(1.0) as f32,
                bw: man.get("bw").and_then(|v| v.as_bool()).unwrap_or(false),
                bin_size: std::fs::metadata(&bin).map(|m| m.len()).unwrap_or(0),
                dir,
            });
        }
        out.sort_by(|a, b| a.dir.cmp(&b.dir));
        self.card_default = std::fs::read_to_string(dxo.join("_default.json"))
            .ok()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok())
            .and_then(|v| v.get("dir").and_then(|d| d.as_str()).map(|s| s.to_string()));
        self.card_note = if out.is_empty() {
            "卡上暂无本工具导出的滤镜".into()
        } else {
            match &self.card_default {
                Some(d) => format!("卡上 {} 个滤镜 · 默认开机：{d}", out.len()),
                None => format!("卡上 {} 个滤镜", out.len()),
            }
        };
        self.push_log(self.card_note.clone());
        self.card_filters = out;
    }

    fn toggle_card_default(&mut self, dir: String) {
        let Some(dxo) = self.dxo_dir() else {
            self.push_log("请先插入并挂载相机存储卡");
            return;
        };
        let clearing = self.card_default.as_deref() == Some(dir.as_str());
        if clearing {
            let p = dxo.join("_default.json");
            if p.exists() {
                match std::fs::remove_file(&p) {
                    Ok(_) => {
                        self.card_default = None;
                        self.push_log("已取消默认开机滤镜");
                    }
                    Err(e) => self.push_log(format!("取消失败：{e}")),
                }
            } else {
                self.card_default = None;
            }
            return;
        }
        let sub = dxo.join(&dir);
        if !sub.join(format!("{dir}.bin")).is_file() {
            self.push_log(format!("卡上不存在滤镜 {dir}"));
            return;
        }
        let body = serde_json::json!({
            "dir": dir,
            "set_at": chrono_now(),
        });
        let bytes = serde_json::to_vec_pretty(&body).unwrap_or_default();
        match lut_camera::safe_write(&dxo.join("_default.json"), &bytes) {
            Ok(_) => {
                self.push_log(format!("已将「{dir}」设为默认开机滤镜（工具侧偏好）"));
                self.card_default = Some(dir);
            }
            Err(e) => self.push_log(format!("设置失败：{e}")),
        }
    }

    fn load_card_item(&mut self, dir: String) {
        let Some(dxo) = self.dxo_dir() else {
            self.push_log("请先插入并挂载相机存储卡");
            return;
        };
        if self.items.len() >= LIB_MAX {
            self.push_log(format!("预览库已达上限 {LIB_MAX}"));
            return;
        }
        let bin = dxo.join(&dir).join(format!("{dir}.bin"));
        let Ok(bytes) = std::fs::read(&bin) else {
            self.push_log(format!("读取 {dir}.bin 失败"));
            return;
        };
        const HEAD: usize = 8;
        if bytes.len() < HEAD + lut_persist::CC_TABLE_LEN {
            self.push_log(format!("bin 长度异常：{} B", bytes.len()));
            return;
        }
        let cc = bytes[HEAD..HEAD + lut_persist::CC_TABLE_LEN].to_vec();
        let name = self
            .card_filters
            .iter()
            .find(|f| f.dir == dir)
            .map(|f| f.name.clone())
            .unwrap_or_else(|| dir.clone());
        let mut store = match Store::open(db_dir()) {
            Ok(s) => s,
            Err(e) => {
                self.push_log(format!("打开本地库失败：{e}"));
                return;
            }
        };
        match store.insert(NewFilter {
            name: &name,
            tone: "pass",
            strength: 1.0,
            bw: false,
            cc_bytes: &cc,
            src: &format!("卡上 {dir}"),
            format: "bin",
            source_size: 0,
            locked: true,
            cube_blob: None,
        }) {
            Ok(_) => {
                self.load_lib();
                self.sel = self.items.len().checked_sub(1);
                self.sync_editor_from_sel();
                self.recompute_preview();
                self.push_log(format!("已载入「{name}」到库（参数固定，可改名 / 重导出）"));
            }
            Err(e) => self.push_log(format!("载入失败：{e}")),
        }
    }

    // ------------------------------------------------------------ 导出

    fn validate_export(&mut self) -> bool {
        if self.items.iter().filter(|i| i.export).count() == 0 {
            self.push_log("请先在库里勾选要导出的滤镜");
            return false;
        }
        if self.export_count() > CUSTOM_MAX {
            self.push_log(format!("最多勾选 {CUSTOM_MAX} 个导出（相机上限）"));
            return false;
        }
        if self.card_mount.is_none() {
            self.push_log("请先选择存储卡（未检测到相机卡）");
            return false;
        }
        true
    }

    fn do_export(&mut self) {
        self.export_confirm = false;
        let Some(dxo) = self.dxo_dir() else {
            self.push_log("请先插入并挂载相机存储卡");
            return;
        };
        let rep = self.flush_dirty();
        if !rep.ok() {
            self.push_log("导出已取消：存在未保存改动，请先重试保存");
            return;
        }

        // 快照待导出项（借用局部数据，避免与 self 的日志写入冲突）
        let picked: Vec<LibItem> = self
            .items
            .iter()
            .filter(|i| i.export)
            .cloned()
            .collect();
        if picked.is_empty() {
            self.push_log("导出清单为空，已取消");
            return;
        }
        let shas: Vec<String> = picked.iter().map(|i| Store::hash_bytes(&i.cc)).collect();
        let refs: Vec<lut_camera::ExportItem<'_>> = picked
            .iter()
            .zip(shas.iter())
            .map(|(it, h)| lut_camera::ExportItem {
                name: &it.name,
                src: &it.src,
                format: &it.fmt,
                tone: &it.tone,
                strength: it.strength,
                bw: it.bw,
                locked: it.locked,
                source_size: it.source_size,
                cc: &it.cc,
                table_sha256: h,
            })
            .collect();

        match lut_camera::export_bundle(
            &dxo,
            self.keep_original,
            &refs,
            &chrono_now(),
        ) {
            Ok(rep) => {
                for n in &rep.names {
                    self.push_log(format!("  · {n}/{n}.bin（87720 B，回读校验通过）"));
                }
                self.push_log(format!(
                    "✓ 已导出 {} 个滤镜 · {} 个文件（备份 + fsync + sync）",
                    rep.names.len(),
                    rep.written.len()
                ));
                self.push_log("提示：仅卡上资产。写完请在访达里正常推出存储卡。");
            }
            Err(e) => self.push_log(format!("导出失败：{e}")),
        }
    }

    // ------------------------------------------------------------ 更新

    fn update(&mut self, msg: Message) -> Task<Message> {
        match msg {
            Message::SwitchPage(page) => {
                if self.page != page {
                    // 切页不丢未保存参数；失败则留在当前页。
                    let rep = self.flush_dirty();
                    if !rep.ok() {
                        self.push_log("切换页面前保存失败，请先重试保存");
                        return Task::none();
                    }
                    self.page = page;
                    self.push_log(format!("已切换到「{}」", page.label()));
                }
                return Task::none();
            }
            Message::CloseRequested(id) => {
                let rep = self.flush_dirty();
                if rep.ok() {
                    self.pending_close = None;
                    return iced::window::close(id);
                }
                self.pending_close = Some(id);
                self.push_log("关闭已取消：仍有未保存改动，可点「重试保存」或「放弃改动并退出」");
                return Task::none();
            }
            Message::RetrySaveAndClose => {
                let rep = self.flush_dirty();
                if rep.ok() {
                    if let Some(id) = self.pending_close.take() {
                        return iced::window::close(id);
                    }
                    self.save_error = None;
                }
                return Task::none();
            }
            Message::DiscardAndClose => {
                let n = self.dirty.len();
                self.dirty.clear();
                self.save_error = None;
                self.push_log(format!("已放弃 {n} 项未保存改动（未写入本地库）"));
                if let Some(id) = self.pending_close.take() {
                    return iced::window::close(id);
                }
                return Task::none();
            }
            Message::PickFiles => {
                // 只列实际支持的格式：后端当前仅解析 .cube（方案 §12 格式矩阵）。
                // .3dl/.csp/HaldCLUT/.xmp 待 lut-core 增加解析器后再放开。
                let picked = rfd::FileDialog::new()
                    .add_filter(
                        "LUT / HaldCLUT",
                        &["cube", "3dl", "csp", "png", "jpg", "jpeg", "tif", "tiff"],
                    )
                    .add_filter("Cube LUT", &["cube"])
                    .set_title("选择 LUT 文件（可多选）")
                    .pick_files();
                if let Some(paths) = picked {
                    self.import_files(paths);
                }
            }
            Message::ClearLib => {
                if !self.clear_armed {
                    self.clear_armed = true;
                    self.push_log("再点一次「确认清空」将把全部滤镜移入回收站（可恢复）");
                } else {
                    self.clear_armed = false;
                    let rep = self.flush_dirty();
                    if !rep.ok() {
                        self.push_log("清空已取消：存在未保存改动，请先重试保存");
                        return Task::none();
                    }
                    match Store::open(db_dir()).and_then(|mut s| s.soft_delete_all()) {
                        Ok(n) => {
                            self.items.clear();
                            self.sel = None;
                            self.name_buf.clear();
                            self.dirty.clear();
                            self.load_trash();
                            self.recompute_preview();
                            self.push_log(format!("已把 {n} 个滤镜移入回收站 · 可在②的回收站里恢复"));
                        }
                        Err(e) => self.push_log(format!("清空失败：{e}")),
                    }
                }
            }
            Message::ToggleTrash => {
                self.show_trash = !self.show_trash;
                self.purge_armed = false;
                self.purge_armed_id = None;
                if self.show_trash {
                    self.load_trash();
                    self.push_log(format!("回收站：{} 条（删除只是移入这里，可恢复）", self.trash.len()));
                } else {
                    self.push_log("返回滤镜库");
                }
            }
            Message::RestoreItem(id) => {
                let name = self
                    .trash
                    .iter()
                    .find(|t| t.id == id)
                    .map(|t| t.name.clone())
                    .unwrap_or_default();
                let mut store = match Store::open(db_dir()) {
                    Ok(s) => s,
                    Err(e) => {
                        self.push_log(format!("打开本地库失败：{e}"));
                        return Task::none();
                    }
                };
                match store.restore(id) {
                    Ok(_) => {
                        self.load_lib();
                        if !self.items.is_empty() {
                            self.sel = Some(self.items.len() - 1);
                        }
                        self.sync_editor_from_sel();
                        self.recompute_preview();
                        self.push_log(format!("已恢复「{name}」到库末尾"));
                    }
                    Err(e) => self.push_log(format!("恢复失败：{e}")),
                }
            }
            Message::PurgeItem(id) => {
                if self.purge_armed_id != Some(id) {
                    self.purge_armed_id = Some(id);
                    self.push_log("再点一次「确认删除」将永久删除该条（内容文件一并回收）");
                } else {
                    self.purge_armed_id = None;
                    let mut store = match Store::open(db_dir()) {
                        Ok(s) => s,
                        Err(e) => {
                            self.push_log(format!("打开本地库失败：{e}"));
                            return Task::none();
                        }
                    };
                    match store.purge(id) {
                        Ok(_) => {
                            self.load_trash();
                            self.push_log("已永久删除 1 条");
                        }
                        Err(e) => self.push_log(format!("删除失败：{e}")),
                    }
                }
            }
            Message::PurgeAll => {
                if !self.purge_armed {
                    self.purge_armed = true;
                    self.purge_armed_id = None;
                    self.push_log("再点一次「确认清空回收站」将永久删除全部条目");
                } else {
                    self.purge_armed = false;
                    let mut store = match Store::open(db_dir()) {
                        Ok(s) => s,
                        Err(e) => {
                            self.push_log(format!("打开本地库失败：{e}"));
                            return Task::none();
                        }
                    };
                    match store.purge_all() {
                        Ok(n) => {
                            self.load_trash();
                            self.push_log(format!("回收站已清空 · 永久删除 {n} 条"));
                        }
                        Err(e) => self.push_log(format!("清空回收站失败：{e}")),
                    }
                }
            }
            Message::Select(i) => self.select(i),
            Message::ToggleExport(i) => {
                if i < self.items.len() {
                    self.items[i].export = !self.items[i].export;
                    let (id, on, name) = (
                        self.items[i].id,
                        self.items[i].export,
                        self.items[i].name.clone(),
                    );
                    let mut store = match Store::open(db_dir()) {
                        Ok(s) => s,
                        Err(e) => {
                            self.push_log(format!("打开本地库失败：{e}"));
                            return Task::none();
                        }
                    };
                    if let Err(e) = store.set_export(id, on) {
                        self.push_log(format!("保存导出开关失败：{e}"));
                    }
                    self.push_log(format!(
                        "「{name}」{}导出",
                        if on { "已加入" } else { "已移出" }
                    ));
                }
            }
            Message::DeleteItem(i) => {
                if i >= self.items.len() {
                    return Task::none();
                }
                let rep = self.flush_dirty();
                if !rep.ok() {
                    self.push_log("删除已取消：存在未保存改动，请先重试保存");
                    return Task::none();
                }
                let (id, name) = (self.items[i].id, self.items[i].name.clone());
                let mut store = match Store::open(db_dir()) {
                    Ok(s) => s,
                    Err(e) => {
                        self.push_log(format!("打开本地库失败：{e}"));
                        return Task::none();
                    }
                };
                match store.soft_delete(id) {
                    Ok(_) => {
                        self.items.remove(i);
                        self.dirty.remove(&id);
                        if let Some(s) = self.sel {
                            self.sel = if s == i {
                                None
                            } else if s > i {
                                Some(s - 1)
                            } else {
                                Some(s)
                            };
                        }
                        self.sync_editor_from_sel();
                        self.recompute_preview();
                        self.load_trash();
                        self.push_log(format!("「{name}」已移入回收站（可恢复）"));
                    }
                    Err(e) => self.push_log(format!("删除失败：{e}")),
                }
            }
            Message::MoveUp | Message::MoveDown => {
                let up = matches!(msg, Message::MoveUp);
                if let Some(i) = self.sel {
                    let target = if up {
                        i.checked_sub(1)
                    } else if i + 1 < self.items.len() {
                        Some(i + 1)
                    } else {
                        None
                    };
                    if let Some(j) = target {
                        self.flush_dirty();
                        self.items.swap(i, j);
                        self.sel = Some(j);
                        // dirty 按记录 ID 保存，排序不影响身份；顺序单独落库
                        self.persist_order();
                        self.push_log(format!("顺序已调整 · 第 {} 位", j + 1));
                    }
                } else {
                    self.push_log("先选择一个滤镜，再调整顺序");
                }
            }
            Message::NameEdited(s) => {
                self.name_buf = s;
                if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                    let name = self.name_buf.clone();
                    let id = self.items[i].id;
                    self.items[i].name = name.clone();
                    self.dirty.insert(id);
                    // 名称立即落库（参数可能还没动）；失败仍由 dirty 统一补写
                    if let Ok(mut store) = Store::open(db_dir()) {
                        if let Err(e) = store.rename(id, &name) {
                            self.push_log(format!("名称暂存失败，将在保存时重试：{e}"));
                        }
                    }
                }
            }
            Message::ToneSet(tone) => {
                if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                    if self.items[i].locked {
                        self.push_log("卡载滤镜参数已固定");
                    } else {
                        self.items[i].tone = tone.to_string();
                        self.reapply_params();
                    }
                }
            }
            Message::StrengthSet(v) => {
                if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                    let locked = self.items[i].locked;
                    if locked {
                        return Task::none();
                    }
                    self.items[i].strength = v;
                    self.reapply_params();
                }
            }
            Message::BwSet(b) => {
                if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                    if self.items[i].locked {
                        self.push_log("卡载滤镜参数已固定");
                    } else {
                        self.items[i].bw = b;
                        self.reapply_params();
                    }
                }
            }
            Message::SaveDefault => {
                if let Some(i) = self.sel.filter(|i| *i < self.items.len()) {
                    let it = &self.items[i];
                    if it.locked {
                        self.push_log("卡载滤镜参数已固定，不作为导入默认");
                    } else {
                        self.def_tone = it.tone.clone();
                        self.def_strength = it.strength;
                        self.def_bw = it.bw;
                        self.push_log(format!(
                            "已存为导入默认值：{} · {:.2}{}",
                            tone_label(&self.def_tone),
                            self.def_strength,
                            if self.def_bw { " · 黑白" } else { "" }
                        ));
                    }
                } else {
                    self.push_log("先选择一个滤镜");
                }
            }
            Message::PickTestImage => {
                let picked = rfd::FileDialog::new()
                    .add_filter("图片", &["png", "jpg", "jpeg", "tif", "tiff", "bmp", "webp"])
                    .set_title("选择预览图")
                    .pick_file();
                if let Some(p) = picked {
                    self.set_test_image(Some(p));
                }
            }
            Message::ToggleKeepOriginal => {
                self.keep_original = !self.keep_original;
                self.push_log(if self.keep_original {
                    "导出将包含 ORIGINAL（原厂档）"
                } else {
                    "导出不含 ORIGINAL，自定义从第 1 档开始"
                });
            }
            Message::ResetTestImage => self.set_test_image(None),
            Message::ViewModeChanged(m) => {
                self.view_mode = m;
            }
            Message::PosChanged(p) => {
                self.pos = p;
                self.rebuild_slider_image();
            }
            Message::RefreshCard => {
                self.refresh_card();
                let s = self.card_status.clone();
                self.push_log(format!("存储卡：{s}"));
            }
            Message::ReadCard => self.read_card(),
            Message::ToggleCardDefault(dir) => self.toggle_card_default(dir),
            Message::LoadCardItem(dir) => self.load_card_item(dir),
            Message::AskExport => {
                if self.validate_export() {
                    self.export_confirm = true;
                    let n = self.export_count();
                    self.push_log(format!("待确认：将写入 {n} 个滤镜到卡上 DXO_FILTERS/"));
                }
            }
            Message::ConfirmExport => self.do_export(),
            Message::CancelExport => {
                self.export_confirm = false;
                self.push_log("已取消导出");
            }
        }

        Task::none()
    }

    fn persist_order(&mut self) {
        let ids: Vec<i64> = self.items.iter().map(|i| i.id).collect();
        if let Ok(mut store) = Store::open(db_dir()) {
            let _ = store.reorder(&ids);
        }
    }

    // ------------------------------------------------------------ 视图

    fn view(&self) -> Element<Message> {
        // 单边分隔线：Iced Border 是四边统一，竖线用 2px 窄容器实现。
        let vline = container(horizontal_space())
            .width(2.0)
            .height(Length::Fill)
            .style(|_: &Theme| container::Style {
                background: Some(Background::Color(color::BORDER_HAIRLINE)),
                ..container::Style::default()
            });

        // 固定壳体：导航｜当前页操作｜当前页结果；状态栏常驻底部。
        let body = row![self.view_nav(), vline, self.view_page()]
            .spacing(space::S5)
            .height(Length::Fill);

        container(column![body, self.view_statusbar()].spacing(space::S5))
            .padding(size::CONTENT_PAD)
            .width(Length::Fill)
            .height(Length::Fill)
            .style(|_: &Theme| container::Style {
                text_color: Some(color::CANVAS_TEXT),
                background: Some(Background::Color(color::CANVAS_BASE)),
                border: Border::default(),
                shadow: Shadow::default(),
            })
            .into()
    }

    // 自定义顶栏已移除：品牌（DXO-ONE-XLAB）移到左侧栏顶部，符合规范「不另画标题栏」。
    // 窗口标题由 OS 原生标题栏承载（`application("DXO-ONE-XLAB", …)`）。

    /// 固定导航：品牌 + MENU / 03 + 三页菜单 + 全局卡/库状态。
    fn view_nav(&self) -> Element<Message> {
        let brand = column![
            // 16px 保证 "DXO-ONE-XLAB" 在 208px 导航内单行（22px 会拆行）
            text("DXO-ONE-XLAB")
                .size(16.0)
                .font(t::DISPLAY)
                .color(color::CANVAS_TEXT),
            text("MENU / 03")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
        ]
        .spacing(space::S2);

        let menu = column![
            self.nav_item(AppPage::Import),
            self.nav_item(AppPage::Patch),
            self.nav_item(AppPage::About),
        ]
        .spacing(space::S2);

        let foot = column![
            text(format!(
                "卡：{}",
                if self.card_mount.is_some() { "已挂载" } else { "未挂载" }
            ))
            .size(t::CAPTION_SIZE)
            .font(t::CAPTION)
            .color(color::CANVAS_TEXT_DIM),
            text(format!("本地库：{} 项", self.items.len()))
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
        ]
        .spacing(space::S2);

        container(
            column![brand, menu, foot]
                .spacing(space::S6)
                .padding(space::S5)
                .width(Length::Fill),
        )
        .width(Length::Fixed(208.0))
        .height(Length::Fill)
        .style(theme::panel)
        .into()
    }

    fn nav_item(&self, page: AppPage) -> Element<Message> {
        let selected = self.page == page;
        button(
            container(
                text(format!("{}  {}", page.index_label(), page.label()))
                    .size(t::BODY_SIZE)
                    .font(if selected { t::BODY_STRONG } else { t::BODY })
                    .color(if selected {
                        color::CANVAS_BASE
                    } else {
                        color::CANVAS_TEXT
                    }),
            )
            .padding([8.0, 10.0])
            .width(Length::Fill),
        )
        .on_press(Message::SwitchPage(page))
        .style(theme::nav_item_style(selected))
        .width(Length::Fill)
        .into()
    }

    fn view_page(&self) -> Element<Message> {
        match self.page {
            AppPage::Import => row![self.view_sidebar(), self.view_content()]
                .spacing(space::S5)
                .height(Length::Fill)
                .into(),
            AppPage::Patch => self.view_patch_page(),
            AppPage::About => self.view_about_page(),
        }
    }

    /// 更新补丁页：全部取真实状态，不含演示数据；补丁部署入口明确禁用。
    fn view_patch_page(&self) -> Element<Message> {
        let ops = column![
            text("更新补丁")
                .size(t::DISPLAY_SIZE)
                .font(t::DISPLAY)
                .color(color::CANVAS_TEXT),
            text("操作类型")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
            text("资产导出 · 可用（只生成工具资产包）")
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text("补丁部署 · 禁用（R4 写入服务未完成）")
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT_DIM),
            button(text("导出所选资产").size(t::BODY_SIZE))
                .on_press(Message::AskExport)
                .width(Length::Fill),
            button(text("刷新卡状态").size(t::BODY_SIZE))
                .on_press(Message::RefreshCard)
                .width(Length::Fill),
        ]
        .spacing(space::S4)
        .padding(space::S5);

        let target = self
            .card_mount
            .as_ref()
            .map(|p| p.display().to_string())
            .unwrap_or_else(|| "未挂载".into());
        let result = column![
            text("本次状态")
                .size(t::DISPLAY_SIZE)
                .font(t::DISPLAY)
                .color(color::CANVAS_TEXT),
            text(format!("目标卷：{target}"))
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text(format!("已勾选导出：{} 项", self.export_count()))
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text(format!("卡状态：{}", self.card_status))
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text(format!("卡上滤镜：{} 条", self.card_filters.len()))
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text("资产导出不等于相机补丁已更新；写卡入口保持不可用。")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
        ]
        .spacing(space::S4)
        .padding(space::S5);

        // R2-01：导出与卡上滤镜归位到本页结果区
        let result = column![
            result,
            separator(),
            self.view_export_section(),
            separator(),
            self.view_card_section(),
        ]
        .spacing(space::S4);

        row![
            container(scrollable(ops))
                .width(Length::FillPortion(1))
                .height(Length::Fill)
                .style(theme::panel),
            container(scrollable(result))
                .width(Length::FillPortion(1))
                .height(Length::Fill)
                .style(theme::panel),
        ]
        .spacing(space::S5)
        .height(Length::Fill)
        .into()
    }

    /// 关于我们页：版本来自构建元数据，路径与边界均为真实值。
    fn view_about_page(&self) -> Element<Message> {
        let left = column![
            text("关于我们")
                .size(t::DISPLAY_SIZE)
                .font(t::DISPLAY)
                .color(color::CANVAS_TEXT),
            text("DXO-ONE-XLAB · 本地滤镜工具")
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text("把第三方 LUT 转成相机 CC 表，生成工具资产包，并管理本地滤镜库。")
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT_DIM),
        ]
        .spacing(space::S4)
        .padding(space::S5);

        let right = column![
            text("版本与边界")
                .size(t::DISPLAY_SIZE)
                .font(t::DISPLAY)
                .color(color::CANVAS_TEXT),
            text(format!("应用版本：{}", env!("CARGO_PKG_VERSION")))
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT),
            text(format!("数据目录：{}", db_dir().display()))
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT),
            text(format!("原厂基线 SHA256：{}", &lut_core::STOCK_TABLE_SHA256[..16]))
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
            text("相机兼容版本：未实机验收")
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::CANVAS_TEXT_DIM),
            text("许可与第三方声明见项目 LICENSE、THIRD_PARTY.md")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
        ]
        .spacing(space::S4)
        .padding(space::S5);

        row![
            container(scrollable(left))
                .width(Length::FillPortion(1))
                .height(Length::Fill)
                .style(theme::panel),
            container(scrollable(right))
                .width(Length::FillPortion(1))
                .height(Length::Fill)
                .style(theme::panel),
        ]
        .spacing(space::S5)
        .height(Length::Fill)
        .into()
    }

    fn view_sidebar(&self) -> Element<Message> {
        // ① 批量导入
        let drop_inner = column![
            text("拖入滤镜文件，或点击选择")
                .size(t::BODY_SIZE)
                .font(t::BODY_STRONG)
                .color(color::LABEL_PRIMARY),
            text("支持 .cube / .3dl / .csp / Hald 图像（png·jpg·tif），可多选")
                .size(t::FOOTNOTE_SIZE)
                .font(t::FOOTNOTE)
                .color(color::LABEL_SECONDARY),
        ]
        .spacing(space::S2)
        .align_x(Alignment::Center);

        let drop = button(container(drop_inner).width(Length::Fill).align_x(Alignment::Center))
            .on_press(Message::PickFiles)
            .style(theme::drop_zone)
            .padding([14.0, 12.0])
            .width(Length::Fill);

        let def_hint = text(format!(
            "导入默认：{} · {:.2}{}",
            tone_label(&self.def_tone),
            self.def_strength,
            if self.def_bw { " · 黑白" } else { "" }
        ))
        .size(t::CAPTION_SIZE)
        .font(t::CAPTION)
        .color(color::LABEL_TERTIARY);

        // ② 滤镜库 / 回收站
        let trash_n = self.trash.len();

        let trash_toggle: Element<Message> = button(
            text(if self.show_trash {
                "← 返回库".to_string()
            } else if trash_n > 0 {
                format!("回收站 {trash_n}")
            } else {
                "回收站".to_string()
            })
            .size(t::CAPTION_SIZE)
            .font(t::CAPTION),
        )
        .on_press(Message::ToggleTrash)
        .style(theme::export_chip(self.show_trash))
        .padding([3.0, 7.0])
        .into();

        let lib_header = row![
            text("②")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY),
            text(if self.show_trash { "回收站" } else { "滤镜库" })
                .size(t::TITLE_SIZE)
                .font(t::TITLE)
                .color(color::LABEL_PRIMARY),
            text(if self.show_trash {
                format!("{trash_n} 条 · 可恢复")
            } else {
                format!("{} 个 · 顺序 = 菜单环序", self.items.len())
            })
            .size(t::CAPTION_SIZE)
            .font(t::CAPTION)
            .color(color::LABEL_TERTIARY),
            horizontal_space(),
            trash_toggle,
        ]
        .spacing(space::S3)
        .align_y(Alignment::Center);

        let list: Element<Message> = if self.show_trash {
            if self.trash.is_empty() {
                container(
                    text("（回收站是空的）")
                        .size(t::FOOTNOTE_SIZE)
                        .font(t::FOOTNOTE)
                        .color(color::LABEL_TERTIARY),
                )
                .width(Length::Fill)
                .height(Length::Fixed(84.0))
                .align_x(Alignment::Center)
                .align_y(Alignment::Center)
                .style(theme::invisible)
                .into()
            } else {
                let rows: Vec<Element<Message>> =
                    (0..self.trash.len()).map(|i| self.view_trash_row(i)).collect();
                let n = self.trash.len();
                let h = (n as f32 * 52.0 + n.saturating_sub(1) as f32 * space::S3)
                    .min(size::LIB_MAX_H)
                    .max(58.0);
                scrollable(column(rows).spacing(space::S3))
                    .style(theme::scrollable_style)
                    .height(Length::Fixed(h))
                    .into()
            }
        } else if self.items.is_empty() {
            container(
                text("（空 · 导入后出现在这里）")
                    .size(t::FOOTNOTE_SIZE)
                    .font(t::FOOTNOTE)
                    .color(color::LABEL_TERTIARY),
            )
            .width(Length::Fill)
            .height(Length::Fixed(84.0))
            .align_x(Alignment::Center)
            .align_y(Alignment::Center)
            .style(theme::invisible)
            .into()
        } else {
            let rows: Vec<Element<Message>> =
                (0..self.items.len()).map(|i| self.view_row(i)).collect();
            let n = self.items.len();
            let h = (n as f32 * 52.0 + n.saturating_sub(1) as f32 * space::S3)
                .min(size::LIB_MAX_H)
                .max(58.0);
            scrollable(column(rows).spacing(space::S3))
                .style(theme::scrollable_style)
                .height(Length::Fixed(h))
                .into()
        };

        let lib_tools: Element<Message> = if self.show_trash {
            let purge_btn: Element<Message> = if trash_n == 0 {
                button(text("清空回收站").size(t::FOOTNOTE_SIZE).font(t::BODY))
                    .style(theme::button_style(Kind::Ghost))
                    .padding([0.0, 8.0])
                    .height(size::CONTROL_SM)
                    .into()
            } else if self.purge_armed {
                button(text("确认清空回收站？").size(t::FOOTNOTE_SIZE).font(t::BODY_STRONG))
                    .on_press(Message::PurgeAll)
                    .style(theme::button_style(Kind::Danger))
                    .padding([0.0, 8.0])
                    .height(size::CONTROL_SM)
                    .into()
            } else {
                button(text("清空回收站").size(t::FOOTNOTE_SIZE).font(t::BODY))
                    .on_press(Message::PurgeAll)
                    .style(theme::button_style(Kind::Ghost))
                    .padding([0.0, 8.0])
                    .height(size::CONTROL_SM)
                    .into()
            };
            row![
                text("删除的滤镜留在这里，内容文件尚未回收")
                    .size(t::CAPTION_SIZE)
                    .font(t::CAPTION)
                    .color(color::LABEL_TERTIARY)
                    .width(Length::Fill),
                purge_btn,
            ]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                small_ghost("↑ 上移", Message::MoveUp),
                small_ghost("↓ 下移", Message::MoveDown),
                horizontal_space(),
                if self.clear_armed {
                    button(text("确认移入回收站？").size(t::FOOTNOTE_SIZE).font(t::BODY_STRONG))
                        .on_press(Message::ClearLib)
                        .style(theme::button_style(Kind::Danger))
                        .padding([0.0, 8.0])
                        .height(size::CONTROL_SM)
                } else {
                    button(text("清空").size(t::FOOTNOTE_SIZE).font(t::BODY))
                        .on_press(Message::ClearLib)
                        .style(theme::button_style(Kind::Ghost))
                        .padding([0.0, 8.0])
                        .height(size::CONTROL_SM)
                },
            ]
            .spacing(space::S2)
            .align_y(Alignment::Center)
            .into()
        };

        // ③ 编辑选中滤镜
        let locked = self.sel.and_then(|i| self.items.get(i)).map(|i| i.locked);
        let has_sel = self.sel.is_some();

        let name_field = row![
            field_label("显示名"),
            text_input("选一个滤镜后改名", &self.name_buf)
                .on_input(Message::NameEdited)
                .style(theme::field)
                .padding([6.0, 10.0])
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .width(Length::Fill),
        ]
        .spacing(space::S3)
        .align_y(Alignment::Center);

        let tone_now = self
            .sel
            .and_then(|i| self.items.get(i))
            .map(|i| i.tone.clone())
            .unwrap_or_else(|| "pass".into());

        let tone_row: Element<Message> = if locked == Some(true) {
            row![
                field_label("影调"),
                text(format!("{}（卡载固定）", tone_label(&tone_now)))
                    .size(t::BODY_SIZE)
                    .font(t::BODY)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                field_label("影调"),
                container(row![
                    seg("直出", tone_now == "pass", Message::ToneSet("pass")),
                    seg("中灰", tone_now == "neutral", Message::ToneSet("neutral")),
                    seg("混合", tone_now == "blend", Message::ToneSet("blend")),
                ]
                .spacing(2.0))
                .padding(3.0)
                .style(theme::segmented_shell),
            ]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .into()
        };

        let strength_now = self
            .sel
            .and_then(|i| self.items.get(i))
            .map(|i| i.strength)
            .unwrap_or(self.def_strength);

        let strength: Element<Message> = if locked == Some(true) {
            row![
                field_label("强度"),
                text(format!("{:.2}（卡载固定）", strength_now))
                    .size(t::BODY_SIZE)
                    .font(t::NUM)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .into()
        } else {
            column![
                row![
                    text("强度")
                        .size(t::BODY_SIZE)
                        .font(t::BODY)
                        .color(color::LABEL_SECONDARY),
                    horizontal_space(),
                    text(format!("{:.2}", strength_now))
                        .size(t::BODY_SIZE)
                        .font(t::NUM)
                        .color(color::LABEL_PRIMARY),
                ],
                slider(0.0..=1.0, strength_now, Message::StrengthSet)
                    .step(0.05_f32)
                    .style(theme::slider_style),
                text("0 = 原厂直出，1 = 满套")
                    .size(t::CAPTION_SIZE)
                    .font(t::CAPTION)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S2)
            .into()
        };

        let bw_on = self
            .sel
            .and_then(|i| self.items.get(i))
            .map(|i| i.bw)
            .unwrap_or(false);

        let bw_row: Element<Message> = if locked == Some(true) {
            row![
                field_label("黑白"),
                text(if bw_on { "是（卡载固定）" } else { "否（卡载固定）" })
                    .size(t::BODY_SIZE)
                    .font(t::BODY)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .into()
        } else {
            row![
                field_label("黑白"),
                toggle(bw_on, Message::BwSet(!bw_on)),
                text("套 LUT 后转灰度")
                    .size(t::CAPTION_SIZE)
                    .font(t::CAPTION)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S3)
            .align_y(Alignment::Center)
            .into()
        };

        let save_def: Element<Message> = {
            let b = button(text("存为导入默认").size(t::FOOTNOTE_SIZE).font(t::BODY))
                .style(theme::button_style(Kind::Ghost))
                .padding([0.0, 8.0])
                .height(size::CONTROL_SM);
            if has_sel {
                b.on_press(Message::SaveDefault).into()
            } else {
                b.into()
            }
        };

        // 左栏顶部品牌（规范：mono 小字，非大标题）+ 右侧短徽章。
        // 侧栏窄（212px，内宽约 172px），品牌 mono 11px + 短徽章才放得下。
        let menu_head = row![
            text("DXO-ONE-XLAB")
                .size(t::FOOTNOTE_SIZE)
                .font(t::FOOTNOTE)
                .color(color::LABEL_PRIMARY),
            horizontal_space(),
            container(
                text(format!("导出 {}/{}", self.export_count(), CUSTOM_MAX))
                    .size(t::CAPTION_SIZE)
                    .font(t::CAPTION)
                    .color(color::LABEL_SECONDARY),
            )
            .padding([4.0, 9.0])
            .style(theme::pill),
        ]
        .spacing(space::S4)
        .align_y(Alignment::Center);

        let body = column![
            menu_head,
            separator(),
            section_header("①", "批量导入第三方滤镜", None),
            drop,
            def_hint,
            separator(),
            lib_header,
            list,
            lib_tools,
            separator(),
            section_header("③", "编辑选中滤镜", None),
            name_field,
            tone_row,
            strength,
            bw_row,
            save_def,
        ]
        .spacing(space::S4);

        container(
            scrollable(body.width(Length::Fill).padding(iced::Padding {
                top: 0.0,
                right: 16.0,
                bottom: 0.0,
                left: 0.0,
            }))
            .style(theme::scrollable_style)
            .width(Length::Fill),
        )
        .style(theme::sidebar)
        .padding(size::CONTENT_PAD)
        .width(Length::Fixed(size::SIDEBAR_W))
        .height(Length::Fill)
        .into()
    }

    fn view_row(&self, i: usize) -> Element<Message> {
        let item = &self.items[i];
        let selected = self.sel == Some(i);

        let idx = container(
            text(format!("{}", i + 1))
                .size(t::FOOTNOTE_SIZE)
                .font(t::NUM)
                .color(color::LABEL_SECONDARY),
        )
        .width(22.0)
        .height(22.0)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(theme::chip);

        let meta = column![
            text(item.name.clone())
                .size(t::BODY_SIZE)
                .font(if selected { t::BODY_STRONG } else { t::BODY })
                .color(color::LABEL_PRIMARY),
            text(format!("{} · {}", truncate(&item.src, 26), item.param_text()))
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY)
                .width(Length::Fill),
        ]
        .spacing(1.0)
        .width(Length::Fill);

        let click = button(meta)
            .on_press(Message::Select(i))
            .style(theme::click_zone)
            .padding(0.0)
            .width(Length::Fill);

        let fmt = container(
            text(if item.locked { "卡" } else { "CUBE" })
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_SECONDARY),
        )
        .padding([2.0, 5.0])
        .style(theme::chip);

        let exp = button(
            text(if item.export { "导出" } else { "不导出" })
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION),
        )
        .on_press(Message::ToggleExport(i))
        .style(theme::export_chip(item.export))
        .padding([3.0, 7.0]);

        let del = button(text("×").size(14.0).font(t::BODY))
            .on_press(Message::DeleteItem(i))
            .style(theme::icon_danger)
            .padding([2.0, 6.0]);

        container(row![idx, click, fmt, exp, del].spacing(space::S2).align_y(Alignment::Center))
            .style(theme::row_shell(selected, false))
            .padding([6.0, 8.0])
            .width(Length::Fill)
            .into()
    }

    /// 回收站一行：名称 + 参数/删除时间 + 「恢复」+「彻底删除」（二次确认）
    fn view_trash_row(&self, i: usize) -> Element<Message> {
        let it = &self.trash[i];
        let armed = self.purge_armed_id == Some(it.id);

        let meta = column![
            text(it.name.clone())
                .size(t::BODY_SIZE)
                .font(t::BODY)
                .color(color::LABEL_PRIMARY),
            text(it.sub_text())
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY)
                .width(Length::Fill),
        ]
        .spacing(1.0)
        .width(Length::Fill);

        let restore = button(text("恢复").size(t::CAPTION_SIZE).font(t::CAPTION))
            .on_press(Message::RestoreItem(it.id))
            .style(theme::export_chip(true))
            .padding([3.0, 7.0]);

        let purge: Element<Message> = if armed {
            button(text("确认删除").size(t::CAPTION_SIZE).font(t::BODY_STRONG))
                .on_press(Message::PurgeItem(it.id))
                .style(theme::button_style(Kind::Danger))
                .padding([3.0, 7.0])
                .into()
        } else {
            button(text("×").size(14.0).font(t::BODY))
                .on_press(Message::PurgeItem(it.id))
                .style(theme::icon_danger)
                .padding([2.0, 6.0])
                .into()
        };

        container(row![meta, restore, purge].spacing(space::S2).align_y(Alignment::Center))
            .style(theme::row_shell(false, false))
            .padding([6.0, 8.0])
            .width(Length::Fill)
            .into()
    }

    fn view_content(&self) -> Element<Message> {
        let mode_seg = container(
            row![
                seg(
                    "并排",
                    self.view_mode == ViewMode::Split,
                    Message::ViewModeChanged(ViewMode::Split)
                ),
                seg(
                    "拖动对比",
                    self.view_mode == ViewMode::Slider,
                    Message::ViewModeChanged(ViewMode::Slider)
                ),
            ]
            .spacing(2.0),
        )
        .padding(3.0)
        .style(theme::segmented_shell);

        let preview_header = row![
            text("④")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY),
            text("预览").size(t::TITLE_SIZE).font(t::TITLE).color(color::LABEL_PRIMARY),
            horizontal_space(),
            mode_seg,
        ]
        .spacing(space::S3)
        .align_y(Alignment::Center);

        let preview_body: Element<Message> = match self.view_mode {
            ViewMode::Split => row![
                frame("原图", self.before_handle.clone(), self.split_w, self.split_h),
                frame(
                    "加滤镜后",
                    self.after_handle.clone(),
                    self.split_w,
                    self.split_h
                ),
            ]
            .spacing(space::S5)
            .into(),
            ViewMode::Slider => {
                let (w, h) = self.slider_size;
                let img: Element<Message> = match &self.slider_handle {
                    Some(hd) => img_widget::Image::new(hd.clone())
                        .width(Length::Fixed(w as f32))
                        .height(Length::Fixed(h as f32))
                        .content_fit(iced::ContentFit::Contain)
                        .into(),
                    None => container("")
                        .width(Length::Fixed(w as f32))
                        .height(Length::Fixed(h as f32))
                        .into(),
                };

                column![
                    container(img).padding(2.0).style(theme::media),
                    slider(0.0..=1.0, self.pos, Message::PosChanged)
                        .step(0.01_f32)
                        .style(theme::slider_style),
                    row![
                        text("原图")
                            .size(t::CAPTION_SIZE)
                            .font(t::CAPTION)
                            .color(color::LABEL_SECONDARY),
                        horizontal_space(),
                        text(format!("{:.0}%", self.pos * 100.0))
                            .size(t::CAPTION_SIZE)
                            .font(t::NUM)
                            .color(color::LABEL_TERTIARY),
                        horizontal_space(),
                        text("加滤镜后")
                            .size(t::CAPTION_SIZE)
                            .font(t::CAPTION)
                            .color(color::LABEL_SECONDARY),
                    ]
                    .width(Length::Fixed(w as f32)),
                ]
                .spacing(space::S3)
                .align_x(Alignment::Center)
                .into()
            }
        };

        let img_hint = match &self.test_img {
            Some(img) => format!("当前预览图：{}（{}×{}）", img.label, img.w, img.h),
            None => "当前预览图：默认色卡".to_string(),
        };

        let img_tools = row![
            small_ghost("选择预览图…", Message::PickTestImage),
            small_ghost("重置为默认色卡", Message::ResetTestImage),
            horizontal_space(),
            text(img_hint)
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY)
                .width(Length::Fill),
        ]
        .spacing(space::S3)
        .align_y(Alignment::Center);

        // ⑤ 导出到存储卡 / ⑥ 卡上已有滤镜：R2-01 起移至「更新补丁」页，
        // 见 view_export_section / view_card_section。

        // ⑤ 日志
        // 最新一条在最上面：不依赖滚动补偿，导入/导出后立即可见
        let log_lines: Vec<Element<Message>> = self
            .log
            .iter()
            .rev()
            .map(|s| {
                text(s.clone())
                    .size(t::FOOTNOTE_SIZE)
                    .font(t::FOOTNOTE)
                    .color(color::LABEL_SECONDARY)
                    .into()
            })
            .collect();

        let log_card = container(
            scrollable(column(log_lines).spacing(2.0))
                .style(theme::scrollable_style)
                .height(Length::Fixed(104.0)),
        )
        .padding([8.0, 10.0])
        .width(Length::Fill)
        .style(theme::log_panel);

        let body = column![
            preview_header,
            preview_body,
            img_tools,
            separator(),
            section_header("⑤", "日志", None),
            log_card,
        ]
        .spacing(space::S4);

        container(
            scrollable(body.width(Length::Fill).padding(iced::Padding {
                top: 0.0,
                right: 16.0,
                bottom: 0.0,
                left: 0.0,
            }))
            .style(theme::scrollable_style)
            .width(Length::Fill),
        )
        .style(theme::panel)
        .padding(size::CONTENT_PAD)
        .width(Length::Fill)
        .height(Length::Fill)
        .into()
    }

    /// ⑤ 导出到存储卡（R2-01 起归位到「更新补丁」页结果区）。
    fn view_export_section(&self) -> Element<Message> {
        let card_row = row![
            field_label("存储卡"),
            text(self.card_status.clone())
                .size(t::FOOTNOTE_SIZE)
                .font(t::FOOTNOTE)
                .color(color::LABEL_SECONDARY)
                .width(Length::Fill),
            small_ghost("刷新", Message::RefreshCard),
        ]
        .spacing(space::S3)
        .align_y(Alignment::Center);

        let keep_row = row![
            field_label("保留原厂"),
            toggle(self.keep_original, Message::ToggleKeepOriginal),
            text("含 ORIGINAL 档；自定义最多 5 个，不论是否保留原厂")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY)
                .width(Length::Fill),
        ]
        .spacing(space::S3)
        .align_y(Alignment::Center);

        let export_btn = button(
            text(format!("导出 {} 个滤镜到存储卡…", self.export_count()))
                .size(t::BODY_SIZE)
                .font(t::BODY_STRONG),
        )
        .on_press(Message::AskExport)
        .style(theme::button_style(Kind::Primary))
        .padding([0.0, 16.0])
        .height(size::CONTROL_LG)
        .width(Length::Fill);

        let export: Element<Message> = if self.export_confirm {
            let list: String = self
                .items
                .iter()
                .filter(|i| i.export)
                .enumerate()
                .map(|(n, i)| format!("{}. {}", n + 1, i.name))
                .collect::<Vec<_>>()
                .join("\n");
            column![
                container(
                    column![
                        text("确认写入卡上 DXO_FILTERS/")
                            .size(t::BODY_SIZE)
                            .font(t::BODY_STRONG)
                            .color(color::LABEL_PRIMARY),
                        text(list)
                            .size(t::FOOTNOTE_SIZE)
                            .font(t::FOOTNOTE)
                            .color(color::LABEL_SECONDARY),
                        row![
                            button(text("确认写入").size(t::BODY_SIZE).font(t::BODY_STRONG))
                                .on_press(Message::ConfirmExport)
                                .style(theme::button_style(Kind::Primary))
                                .padding([0.0, 12.0])
                                .height(size::CONTROL_MD),
                            small_ghost("取消", Message::CancelExport),
                        ]
                        .spacing(space::S3)
                        .align_y(Alignment::Center),
                    ]
                    .spacing(space::S2),
                )
                .padding([10.0, 12.0])
                .width(Length::Fill)
                .style(theme::notice)
            ]
            .spacing(space::S3)
            .into()
        } else {
            column![
                card_row,
                keep_row,
                export_btn,
                text("每个滤镜写入：铺满五槽的 87720 B .bin + 前后预览 + manifest。不动 DCIM / touch_prefs，写后 fsync + sync。")
                    .size(t::CAPTION_SIZE)
                    .font(t::CAPTION)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S3)
            .into()
        };

        column![
            section_header("⑤", "导出到存储卡", Some(format!(
                "{} / {}",
                self.export_count(),
                CUSTOM_MAX
            ))),
            export,
        ]
        .spacing(space::S3)
        .into()
    }

    /// ⑥ 卡上已有滤镜（R2-01 起归位到「更新补丁」页结果区）。
    fn view_card_section(&self) -> Element<Message> {
        let card_list: Element<Message> = if self.card_filters.is_empty() {
            text(if self.card_note.is_empty() {
                "先点「读取卡上滤镜」".to_string()
            } else {
                self.card_note.clone()
            })
            .size(t::FOOTNOTE_SIZE)
            .font(t::FOOTNOTE)
            .color(color::LABEL_TERTIARY)
            .into()
        } else {
            let rows: Vec<Element<Message>> =
                self.card_filters.iter().map(|f| self.view_card_row(f)).collect();
            column(rows).spacing(space::S2).into()
        };

        column![
            section_header("⑥", "卡上已有滤镜", None),
            row![
                small_ghost("读取卡上滤镜", Message::ReadCard),
                text("每条可「设为默认」或「载入到库」")
                    .size(t::CAPTION_SIZE)
                    .font(t::CAPTION)
                    .color(color::LABEL_TERTIARY),
            ]
            .spacing(space::S3)
            .align_y(Alignment::Center),
            card_list,
        ]
        .spacing(space::S3)
        .into()
    }

    fn view_card_row(&self, f: &CardFilter) -> Element<Message> {
        let is_def = self.card_default.as_deref() == Some(f.dir.as_str());
        let dir = f.dir.clone();

        let idx = container(
            text(if is_def { "★" } else { "▦" })
                .size(t::FOOTNOTE_SIZE)
                .font(t::NUM)
                .color(if is_def {
                    color::WARNING
                } else {
                    color::LABEL_SECONDARY
                }),
        )
        .width(22.0)
        .height(22.0)
        .align_x(Alignment::Center)
        .align_y(Alignment::Center)
        .style(move |th: &Theme| {
            if is_def {
                theme::chip_gold(th)
            } else {
                theme::chip(th)
            }
        });

        let mut title = row![
            text(f.name.clone())
                .size(t::BODY_SIZE)
                .font(t::BODY_STRONG)
                .color(color::LABEL_PRIMARY)
        ]
        .spacing(space::S2)
        .align_y(Alignment::Center);
        if is_def {
            title = title.push(
                container(
                    text("默认开机")
                        .size(t::CAPTION_SIZE)
                        .font(t::CAPTION),
                )
                .padding([1.0, 5.0])
                .style(theme::chip_gold),
            );
        }

        let meta = column![
            title,
            text(format!(
                "{} · {} · {}/{:.2}{} · {} B",
                if f.source.is_empty() { &f.dir } else { &f.source },
                f.format,
                tone_label(&f.tone),
                f.strength,
                if f.bw { "/BW" } else { "" },
                f.bin_size
            ))
            .size(t::CAPTION_SIZE)
            .font(t::CAPTION)
            .color(color::LABEL_TERTIARY),
        ]
        .spacing(1.0)
        .width(Length::Fill);

        let def_btn = button(
            text(if is_def { "默认开机 ✓" } else { "设为默认" })
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION),
        )
        .on_press(Message::ToggleCardDefault(f.dir.clone()))
        .style(theme::export_chip(is_def))
        .padding([3.0, 7.0]);

        let load_btn = button(text("载入").size(t::CAPTION_SIZE).font(t::CAPTION))
            .on_press(Message::LoadCardItem(dir))
            .style(theme::export_chip(false))
            .padding([3.0, 7.0]);

        container(row![idx, meta, def_btn, load_btn].spacing(space::S2).align_y(Alignment::Center))
            .style(theme::row_shell(false, is_def))
            .padding([6.0, 8.0])
            .width(Length::Fill)
            .into()
    }

    fn view_statusbar(&self) -> Element<Message> {
        let mut inner = row![
            text(format!("DXO-ONE-XLAB · {}", self.status))
                .size(t::FOOTNOTE_SIZE)
                .font(t::FOOTNOTE)
                .color(color::CANVAS_TEXT),
            horizontal_space(),
            text("本地 · 无网络")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
            text("HIP-HOP V1.7 · 1180×720")
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::CANVAS_TEXT_DIM),
        ]
        .height(size::STATUSBAR_H)
        .align_y(Alignment::Center);

        // 仅在保存失败 / 关闭被拦截时出现，避免污染正常状态栏。
        if self.save_error.is_some() || self.pending_close.is_some() {
            inner = inner.push(
                button(text("重试保存").size(t::CAPTION_SIZE))
                    .on_press(Message::RetrySaveAndClose)
                    .padding([2, 8]),
            );
            if self.pending_close.is_some() {
                inner = inner.push(
                    button(text("放弃改动并退出").size(t::CAPTION_SIZE))
                        .on_press(Message::DiscardAndClose)
                        .padding([2, 8]),
                );
            }
        }

        container(inner)
            .style(|_: &Theme| container::Style {
                text_color: None,
                background: Some(Background::Color(color::CANVAS_BASE)),
                border: theme::bd(color::CANVAS_TEXT_DIM, 2.0, 0.0),
                shadow: Shadow::default(),
            })
            .into()
    }
}

// ---------------------------------------------------------------- 无状态组件

fn section_header<'a>(
    index: &'a str,
    title: &'a str,
    right: Option<String>,
) -> Element<'a, Message> {
    let mut r = row![
        text(index)
            .size(t::CAPTION_SIZE)
            .font(t::CAPTION)
            .color(color::LABEL_TERTIARY),
        text(title)
            .size(t::TITLE_SIZE)
            .font(t::TITLE)
            .color(color::LABEL_PRIMARY),
    ]
    .spacing(space::S3)
    .align_y(Alignment::Center);

    if let Some(v) = right {
        r = r.push(horizontal_space()).push(
            text(v)
                .size(t::CAPTION_SIZE)
                .font(t::CAPTION)
                .color(color::LABEL_TERTIARY),
        );
    }

    r.into()
}

fn field_label<'a>(s: &'a str) -> Element<'a, Message> {
    text(s)
        .size(t::BODY_SIZE)
        .font(t::BODY)
        .color(color::LABEL_SECONDARY)
        .width(Length::Fixed(size::LABEL_COL))
        .into()
}

fn separator<'a>() -> Element<'a, Message> {
    container("")
        .height(1.0)
        .width(Length::Fill)
        .style(theme::separator)
        .into()
}

fn seg<'a>(label: &'a str, selected: bool, msg: Message) -> Element<'a, Message> {
    button(text(label).size(t::BODY_SIZE).font(t::BODY))
        .on_press(msg)
        .style(theme::segment_style(selected))
        .padding([4.0, 10.0])
        .into()
}

fn small_ghost<'a>(label: &'a str, msg: Message) -> Element<'a, Message> {
    button(text(label).size(t::FOOTNOTE_SIZE).font(t::BODY))
        .on_press(msg)
        .style(theme::button_style(Kind::Ghost))
        .padding([0.0, 8.0])
        .height(size::CONTROL_SM)
        .into()
}

fn toggle<'a>(on: bool, msg: Message) -> Element<'a, Message> {
    let knob = container(container("").width(21.0).height(21.0).style(theme::knob))
        .width(Length::Fill)
        .height(Length::Fill)
        .align_x(if on {
            Horizontal::Right
        } else {
            Horizontal::Left
        })
        .align_y(Alignment::Center)
        .padding([0.0, 3.0]);

    button(knob)
        .on_press(msg)
        .style(theme::toggle_style(on))
        .padding(0.0)
        .width(46.0)
        .height(27.0)
        .into()
}

fn frame<'a>(cap: &'a str, handle: Option<Handle>, w: u32, h: u32) -> Element<'a, Message> {
    let img: Element<Message> = match handle {
        Some(hd) => img_widget::Image::new(hd)
            .width(Length::Fixed(w as f32))
            .height(Length::Fixed(h as f32))
            .content_fit(iced::ContentFit::Contain)
            .into(),
        None => container("")
            .width(Length::Fixed(w as f32))
            .height(Length::Fixed(h as f32))
            .into(),
    };

    container(
        column![
            text(cap)
                .size(t::FOOTNOTE_SIZE)
                .font(t::FOOTNOTE)
                .color(color::CANVAS_TEXT_DIM),
            img,
        ]
        .spacing(space::S3)
        .align_x(Alignment::Center),
    )
    .padding(10.0)
    .style(theme::frame)
    .into()
}

fn tone_label(tone: &str) -> &'static str {
    match tone {
        "neutral" => "中灰",
        "blend" => "混合",
        _ => "直出",
    }
}

fn file_name(p: &Path) -> String {
    p.file_name()
        .map(|s| s.to_string_lossy().to_string())
        .unwrap_or_else(|| p.display().to_string())
}

fn truncate(s: &str, max_chars: usize) -> String {
    let n = s.chars().count();
    if n <= max_chars {
        s.to_string()
    } else {
        let head: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{head}…")
    }
}

fn chrono_now() -> String {
    chrono::Local::now().format("%Y-%m-%dT%H:%M:%S").to_string()
}

// ---------------------------------------------------------------- 图像工具

fn load_test_image(path: &Path) -> Result<TestImage, String> {
    let img = image::open(path).map_err(|e| e.to_string())?;
    let mut rgb = img.to_rgb8();
    let (w0, h0) = rgb.dimensions();
    if w0.max(h0) > TEST_IMG_MAX {
        let scale = TEST_IMG_MAX as f32 / w0.max(h0) as f32;
        let nw = ((w0 as f32 * scale).round() as u32).max(1);
        let nh = ((h0 as f32 * scale).round() as u32).max(1);
        rgb = image::imageops::resize(&rgb, nw, nh, image::imageops::FilterType::Lanczos3);
    }
    let (w, h) = rgb.dimensions();
    Ok(TestImage {
        w,
        h,
        rgb: rgb.into_raw(),
        label: file_name(path),
    })
}

fn encode_jpeg(w: u32, h: u32, rgb: &[u8], quality: u8) -> Option<Vec<u8>> {
    if rgb.len() < (w * h * 3) as usize {
        return None;
    }
    let mut buf = Vec::new();
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, quality);
    enc.encode(rgb, w, h, image::ExtendedColorType::Rgb8).ok()?;
    Some(buf)
}

/// 拖动对比合成：左半原图、右半滤镜后，并在分割处画 2px 白线。
fn composite_slider(before: &[u8], after: &[u8], w: u32, h: u32, pos: f32) -> Vec<u8> {
    let mut out = after.to_vec();
    let split = (pos.clamp(0.0, 1.0) * w as f32).round() as usize;
    let split = split.min(w as usize);

    for y in 0..h as usize {
        let base = y * w as usize * 4;
        let n = split * 4;
        out[base..base + n].copy_from_slice(&before[base..base + n]);

        for dx in 0..2usize {
            let x = split + dx;
            if x < w as usize {
                let i = base + x * 4;
                out[i] = 255;
                out[i + 1] = 255;
                out[i + 2] = 255;
                out[i + 3] = 255;
            }
        }
    }

    out
}

/// RGB → RGBA，双线性缩放到目标尺寸。
fn resize_rgba(src: &[u8], sw: u32, sh: u32, tw: u32, th: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((tw * th * 4) as usize);
    if sw == 0 || sh == 0 || src.len() < (sw * sh * 3) as usize {
        out.resize((tw * th * 4) as usize, 0);
        return out;
    }

    let scale_x = sw as f32 / tw as f32;
    let scale_y = sh as f32 / th as f32;

    for ty in 0..th {
        let sy = (ty as f32 + 0.5) * scale_y - 0.5;
        let y0 = sy.floor().clamp(0.0, (sh - 1) as f32) as u32;
        let y1 = (y0 + 1).min(sh - 1);
        let fy = (sy - y0 as f32).clamp(0.0, 1.0);

        for tx in 0..tw {
            let sx = (tx as f32 + 0.5) * scale_x - 0.5;
            let x0 = sx.floor().clamp(0.0, (sw - 1) as f32) as u32;
            let x1 = (x0 + 1).min(sw - 1);
            let fx = (sx - x0 as f32).clamp(0.0, 1.0);

            let p00 = ((y0 * sw + x0) * 3) as usize;
            let p01 = ((y0 * sw + x1) * 3) as usize;
            let p10 = ((y1 * sw + x0) * 3) as usize;
            let p11 = ((y1 * sw + x1) * 3) as usize;

            for c in 0..3 {
                let top = src[p00 + c] as f32 * (1.0 - fx) + src[p01 + c] as f32 * fx;
                let bot = src[p10 + c] as f32 * (1.0 - fx) + src[p11 + c] as f32 * fx;
                let v = top * (1.0 - fy) + bot * fy;
                out.push(v.round().clamp(0.0, 255.0) as u8);
            }
            out.push(255);
        }
    }

    out
}

fn main() -> iced::Result {
    let mut win = iced::window::Settings::default();
    win.size = iced::Size::new(1180.0, 720.0);
    win.min_size = Some(iced::Size::new(1024.0, 660.0));
    win.icon = load_window_icon();
    // 由 update 先执行 flush_dirty 再决定是否 window::close，不能在事件到达前退出。
    win.exit_on_close_request = false;

    iced::application("DXO-ONE-XLAB", App::update, App::view)
        .subscription(App::subscription)
        .theme(|_| Theme::Dark)
        .default_font(t::BODY)
        .antialiasing(true)
        .window(win)
        .run_with(|| (App::new(), Task::none()))
}

/// 编译期嵌入窗口图标（像素相机 + DXO-ONE-XLAB），运行时解码为 RGBA。
/// 失败返回 None —— 图标是加分项，不影响启动。
fn load_window_icon() -> Option<iced::window::icon::Icon> {
    iced::window::icon::from_file_data(include_bytes!("resources/icon-1024.png"), None).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc() -> Vec<u8> {
        vec![7u8; lut_persist::CC_TABLE_LEN]
    }

    fn make_item(id: i64, name: &str) -> LibItem {
        LibItem {
            id,
            name: name.into(),
            fmt: "cube".into(),
            source_size: 4,
            src: "tint.cube".into(),
            tone: "pass".into(),
            strength: 0.5,
            bw: false,
            export: false,
            locked: false,
            cube: None,
            cc: cc(),
        }
    }

    fn tmp_store(tag: &str) -> (Store, PathBuf) {
        let dir = std::env::temp_dir().join(format!(
            "dxo-r003-{}-{}-{}",
            std::process::id(),
            tag,
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let store = Store::open(&dir).unwrap();
        (store, dir)
    }

    fn insert(store: &mut Store, name: &str) -> i64 {
        store
            .insert(NewFilter {
                name,
                tone: "pass",
                strength: 0.5,
                bw: false,
                cc_bytes: &cc(),
                src: "tint.cube",
                format: "cube",
                source_size: 4,
                locked: false,
                cube_blob: None,
            })
            .unwrap()
            .id
    }

    #[test]
    fn flush_saves_and_clears_dirty() {
        let (mut store, dir) = tmp_store("save");
        let id = insert(&mut store, "A");
        let mut items = vec![make_item(id, "A-edited")];
        items[0].strength = 0.9;
        let mut dirty: HashSet<i64> = [id].into_iter().collect();
        let rep = flush_items(&mut store, &items, &mut dirty);
        assert!(rep.ok(), "{:?}", rep.failed);
        assert_eq!(rep.saved, 1);
        assert!(dirty.is_empty());
        let got = store.get_record(id).unwrap();
        assert_eq!(got.name, "A-edited");
        assert!((got.strength - 0.9).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(dir);
    }

    /// D03 核心：排序后仍按记录 ID 保存，不按数组下标串项。
    #[test]
    fn flush_after_reorder_keeps_identity() {
        let (mut store, dir) = tmp_store("reorder");
        let id_a = insert(&mut store, "A");
        let id_b = insert(&mut store, "B");
        let mut items = vec![make_item(id_a, "A"), make_item(id_b, "B-edited")];
        items[1].strength = 0.33;
        let mut dirty: HashSet<i64> = [id_b].into_iter().collect();
        items.reverse(); // 模拟库重排
        let rep = flush_items(&mut store, &items, &mut dirty);
        assert!(rep.ok(), "{:?}", rep.failed);
        assert!(dirty.is_empty());
        let a = store.get_record(id_a).unwrap();
        let b = store.get_record(id_b).unwrap();
        assert_eq!(a.name, "A");
        assert!((a.strength - 0.5).abs() < 1e-6, "未改动的 A 不应被写入 B 的参数");
        assert_eq!(b.name, "B-edited");
        assert!((b.strength - 0.33).abs() < 1e-6);
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn flush_keeps_dirty_on_bad_table() {
        let (mut store, dir) = tmp_store("badtable");
        let mut item = make_item(1, "bad");
        item.cc = vec![0u8; 10];
        let mut dirty: HashSet<i64> = [1].into_iter().collect();
        let rep = flush_items(&mut store, &[item], &mut dirty);
        assert!(!rep.ok());
        assert!(dirty.contains(&1), "保存失败必须保留 dirty");
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn flush_keeps_dirty_on_store_error() {
        let (mut store, dir) = tmp_store("storeerr");
        // 记录不存在：update 的 get_record 会失败
        let mut dirty: HashSet<i64> = [999_999].into_iter().collect();
        let rep = flush_items(&mut store, &[make_item(999_999, "ghost")], &mut dirty);
        assert!(!rep.ok());
        assert!(dirty.contains(&999_999));
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn flush_drops_deleted_items_from_dirty() {
        let (mut store, dir) = tmp_store("missing");
        let mut dirty: HashSet<i64> = [987].into_iter().collect();
        let rep = flush_items(&mut store, &[], &mut dirty);
        assert!(rep.ok());
        assert_eq!(rep.missing, vec![987]);
        assert!(dirty.is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }
}
