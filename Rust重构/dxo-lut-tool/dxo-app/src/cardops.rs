//! F-13 TF 卡区域：选卡 / 只读探测 / 卡内工作清单（增删排序）。
//!
//! 红线对齐：读卡一律只读；工作清单落在本地数据目录（card_plans/<卷>.json），
//! 不接触卡上文件。ph_menu 由 app 内置重建器生成（lut-camera::rebuild，与
//! 管理滤镜.py rebuild 逐字节一致）；真实写卡由 App 内一次性确认票据控制，
//! card_write_plan 仅做只读预演。

use lut_persist::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, atomic::{AtomicBool, AtomicU64, Ordering}};
use std::time::{Instant, Duration};
use tauri::Manager;

#[derive(Default)]
pub struct WriteSession {
    pub busy: AtomicBool,
    serial: AtomicU64,
    ticket: Mutex<Option<WriteTicket>>,
}

struct WriteTicket {
    token: String,
    mount: String,
    digest: String,
    issued: Instant,
}

impl WriteSession {
    pub(crate) fn issue(&self, mount: &str, digest: &str) -> String {
        let token = format!("{}-{}", std::process::id(), self.serial.fetch_add(1, Ordering::SeqCst));
        *self.ticket.lock().unwrap() = Some(WriteTicket { token: token.clone(), mount: mount.into(), digest: digest.into(), issued: Instant::now() });
        token
    }

    pub(crate) fn consume(&self, mount: &str, token: &str) -> Result<String, String> {
        let ticket = self.ticket.lock().unwrap().take().ok_or("请先预演并确认本次写入")?;
        if ticket.token != token || ticket.mount != mount || ticket.issued.elapsed() > Duration::from_secs(300) {
            return Err("写入确认已过期或目标卡不匹配，请重新预演".into());
        }
        Ok(ticket.digest)
    }
}


/// 卡清单里的一项：card=卡上既有档（只读快照），local=本地导入待写入档。
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CardItem {
    pub key: String,
    pub kind: String,
    pub name: String,
    pub local_id: Option<i64>,
    pub card_path: Option<String>,
    pub fp1: Option<u32>,
    pub fp5: Option<u32>,
    /// 用户定的菜单替代名；None = 沿用卡上名称或按库原始名推导。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub menu_name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CardPlan {
    base_sha: String,
    items: Vec<CardItem>,
    /// 用户选定的默认（开机预装）项 key；None = 沿用卡上预装档映射。
    #[serde(default)]
    preload: Option<String>,
}

/// 默认档在工作清单中的 idx：选定 key 优先；否则按卡上预装档指纹映射到保留项。
fn effective_preload(plan: &CardPlan, profile: &lut_camera::CardProfile) -> usize {
    if let Some(key) = &plan.preload {
        if let Some(i) = plan.items.iter().position(|x| &x.key == key) {
            return i;
        }
    }
    if let Some(e) = profile.entries.get(profile.preload) {
        if let Some(i) = plan.items.iter().position(|x| x.kind == "card" && x.fp1 == Some(e.fp1)) {
            return i;
        }
    }
    0
}

fn plan_path(db_dir: &Path, mount: &str) -> PathBuf {
    let key: String = mount
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() || c == '-' || c == '_' { c } else { '_' })
        .collect();
    db_dir.join("card_plans").join(format!("{key}.json"))
}

pub(crate) fn reset_plan(db: &Path, mount: &str) -> Result<(), String> {
    match std::fs::remove_file(plan_path(db, mount)) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(format!("补丁已安装，但本地卡清单重置失败: {e}")),
    }
}

fn read_ph_menu(root: &Path) -> Result<Vec<u8>, String> {
    std::fs::read(root.join("ph_menu.bin")).map_err(|e| format!("读取 ph_menu.bin 失败: {e}"))
}

fn card_item(e: &lut_camera::SlotEntry) -> CardItem {
    CardItem {
        key: format!("card:{:08x}", e.fp1),
        kind: "card".into(),
        name: e.name.clone(),
        local_id: None,
        card_path: Some(e.path.clone()),
        fp1: Some(e.fp1),
        fp5: Some(e.fp5),
        menu_name: None,
    }
}

/// 载入工作清单；卡上 ph_menu 变化（base_sha 不符）时按卡重建。
fn load_or_init(db_dir: &Path, mount: &str) -> Result<(CardPlan, lut_camera::CardProfile, Vec<u8>), String> {
    let root = PathBuf::from(mount);
    let ph = read_ph_menu(&root)?;
    let profile = lut_camera::probe(&root).map_err(|e| e.to_string())?;
    let base_sha = Store::hash_bytes(&ph);
    let pp = plan_path(db_dir, mount);
    if let Ok(txt) = std::fs::read_to_string(&pp) {
        if let Ok(mut plan) = serde_json::from_str::<CardPlan>(&txt) {
            if plan.base_sha == base_sha {
                // 名字是卡侧属性（MENU 段解码），旧版保存清单可能存的是路径反推名：按指纹回刷。
                for it in plan.items.iter_mut().filter(|i| i.kind == "card") {
                    if let Some(e) = profile.entries.iter().find(|e| it.fp1 == Some(e.fp1)) {
                        it.name = e.name.clone();
                    }
                }
                return Ok((plan, profile, ph));
            }
        }
    }
    let plan = CardPlan { base_sha, items: profile.entries.iter().map(card_item).collect(), preload: None };
    save_plan(db_dir, mount, &plan)?;
    Ok((plan, profile, ph))
}

fn save_plan(db_dir: &Path, mount: &str, plan: &CardPlan) -> Result<(), String> {
    let pp = plan_path(db_dir, mount);
    if let Some(parent) = pp.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let txt = serde_json::to_string_pretty(plan).map_err(|e| e.to_string())?;
    std::fs::write(&pp, txt).map_err(|e| e.to_string())
}

/// 工作清单 vs 卡上快照的差异（新增/删除/顺序）。
fn diff_counts(items: &[CardItem], profile: &lut_camera::CardProfile) -> (usize, usize, bool) {
    let add = items.iter().filter(|i| i.kind == "local").count();
    let plan_fps: Vec<u32> = items.iter().filter_map(|i| if i.kind == "card" { i.fp1 } else { None }).collect();
    let card_fps: Vec<u32> = profile.entries.iter().map(|e| e.fp1).collect();
    let del = card_fps.iter().filter(|f| !plan_fps.contains(f)).count();
    let kept: Vec<u32> = card_fps.iter().copied().filter(|f| plan_fps.contains(f)).collect();
    (add, del, plan_fps != kept)
}

fn list_reply(plan: &CardPlan, profile: &lut_camera::CardProfile) -> serde_json::Value {
    let (add, del, order) = diff_counts(&plan.items, profile);
    let items: Vec<serde_json::Value> = plan
        .items
        .iter()
        .map(|it| {
            let mut v = serde_json::json!({
                "key": it.key, "kind": it.kind, "name": it.name,
                "local_id": it.local_id, "card_path": it.card_path,
                "fp1": it.fp1, "fp5": it.fp5, "menu_name": it.menu_name,
            });
            let src = it.menu_name.as_deref().unwrap_or(&it.name);
            let (m, ok) = propose_menu_name(src);
            v["menu"] = serde_json::json!(m);
            v["menu_ok"] = serde_json::json!(ok);
            v
        })
        .collect();
    serde_json::json!({
        "items": items,
        "build_tag": profile.build_tag,
        "known_firmware": profile.known_firmware,
        "n_slots": profile.n_slots,
        "max_slots": profile.max_slots,
        "issues": profile.issues,
        "changed": { "add": add, "del": del, "order": order,
            "rename": plan.items.iter().filter(|it| it.kind == "card" && it.menu_name.as_ref().is_some_and(|n| n != &it.name)).count() },
        "preload": effective_preload(plan, profile),
        "gate_enabled": lut_camera::ensure_write_gate().is_ok(),
    })
}

/// 卷是否进入候选列表：外置介质 / 卡上有 ph_menu.bin / 用户显式配置的相机卷。
/// 两套平台共用同一条规则，避免 Windows 分支自行放宽。
fn keep_card(external: bool, has_ph_menu: bool, is_known: bool) -> bool {
    external || has_ph_menu || is_known
}

/// 已知相机卷判定：与 DXO_CARD_UUID 比对（Windows 的卷 GUID 同样按该变量配置）。
fn is_known_card(uuid: &str) -> bool {
    lut_camera::configured_card_uuid().is_some_and(|configured| uuid == configured)
}

fn card_row(mount: &Path, volume_name: String, device: String, uuid: String) -> serde_json::Value {
    let has_ph = mount.join("ph_menu.bin").is_file();
    let known = is_known_card(&uuid);
    serde_json::json!({
        "mount_point": mount.to_string_lossy(),
        "volume_name": volume_name,
        "device": device,
        "uuid": uuid,
        "has_ph_menu": has_ph,
        "is_known": known,
    })
}

fn sort_cards(mut out: Vec<serde_json::Value>) -> Vec<serde_json::Value> {
    out.sort_by(|a, b| {
        let ka = a["is_known"].as_bool().unwrap_or(false);
        let kb = b["is_known"].as_bool().unwrap_or(false);
        kb.cmp(&ka).then_with(|| a["volume_name"].as_str().cmp(&b["volume_name"].as_str()))
    });
    out
}

/// 卷枚举的入口目录：macOS 是 `/Volumes`；Windows 按盘符查询，参数不使用。
fn volumes_dir() -> &'static Path {
    if cfg!(windows) { Path::new("") } else { Path::new("/Volumes") }
}

/// 枚举已挂载卷（只读）：外置卷 / 含 ph_menu.bin 的卷 / 显式配置的相机 UUID。
fn enumerate_cards(volumes_dir: &Path) -> Vec<serde_json::Value> {
    #[cfg(windows)]
    {
        let _ = volumes_dir;
        sort_cards(enumerate_drive_cards())
    }
    #[cfg(not(windows))]
    sort_cards(enumerate_volumes_dir(volumes_dir))
}

/// macOS：遍历 `/Volumes` 子目录并用 `diskutil info` 取卷属性。
#[cfg(not(windows))]
fn enumerate_volumes_dir(volumes_dir: &Path) -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    let Ok(rd) = std::fs::read_dir(volumes_dir) else { return out };
    for e in rd.flatten() {
        let p = e.path();
        if !p.is_dir() {
            continue;
        }
        let mut device = String::new();
        let mut uuid = String::new();
        let mut vname = e.file_name().to_string_lossy().to_string();
        let mut external = false;
        if let Ok(o) = std::process::Command::new("diskutil").arg("info").arg(&p).output() {
            if o.status.success() {
                for line in String::from_utf8_lossy(&o.stdout).lines() {
                    let line = line.trim();
                    if let Some(v) = line.strip_prefix("Device Identifier:") {
                        device = v.trim().to_string();
                    }
                    if let Some(v) = line.strip_prefix("Volume UUID:") {
                        uuid = v.trim().to_string();
                    }
                    if let Some(v) = line.strip_prefix("Volume Name:") {
                        let v = v.trim();
                        if !v.is_empty() && v != "null" {
                            vname = v.to_string();
                        }
                    }
                    if let Some(v) = line.strip_prefix("Device Location:") {
                        external = v.trim() == "External";
                    }
                }
            }
        }
        if !keep_card(external, p.join("ph_menu.bin").is_file(), is_known_card(&uuid)) {
            continue;
        }
        out.push(card_row(&p, vname, device, uuid));
    }
    out
}

/// Windows：遍历盘符卷，用卷/存储查询取同一组属性；查询失败的卷直接跳过（不放行）。
#[cfg(windows)]
fn enumerate_drive_cards() -> Vec<serde_json::Value> {
    let mut out = Vec::new();
    for root in lut_camera::volume::volume_roots() {
        let Ok(f) = lut_camera::volume::query_volume(&root) else { continue };
        // 内置盘且卷信息不全时不作为候选，避免把系统盘/普通目录列成相机卡
        let external = f.internal == Some(false);
        if !keep_card(external, root.join("ph_menu.bin").is_file(), is_known_card(&f.volume_uuid)) {
            continue;
        }
        let name = if f.label.is_empty() { root.to_string_lossy().into_owned() } else { f.label };
        out.push(card_row(&root, name, f.device, f.volume_uuid));
    }
    out
}

// ── 核心操作（数据目录可注入，便于测试） ────────────────

fn op_get(db: &Path, mount: &str) -> Result<serde_json::Value, String> {
    let (plan, profile, _) = load_or_init(db, mount)?;
    Ok(list_reply(&plan, &profile))
}

fn op_reorder(db: &Path, mount: &str, keys: &[String]) -> Result<serde_json::Value, String> {
    let (mut plan, profile, _) = load_or_init(db, mount)?;
    let have: HashSet<&str> = plan.items.iter().map(|i| i.key.as_str()).collect();
    let want: HashSet<&str> = keys.iter().map(|s| s.as_str()).collect();
    if have != want || keys.len() != plan.items.len() {
        return Err("排序列表与卡清单不一致，请刷新后重试".into());
    }
    plan.items.sort_by_key(|i| keys.iter().position(|k| k == &i.key).unwrap_or(usize::MAX));
    save_plan(db, mount, &plan)?;
    Ok(list_reply(&plan, &profile))
}

fn op_move(db: &Path, mount: &str, key: &str, dir: i32) -> Result<serde_json::Value, String> {
    let (mut plan, profile, _) = load_or_init(db, mount)?;
    let i = plan.items.iter().position(|x| x.key == key).ok_or("该项不在卡清单中")?;
    let j = i as i32 + dir;
    if j < 0 || j >= plan.items.len() as i32 {
        return Ok(list_reply(&plan, &profile));
    }
    plan.items.swap(i, j as usize);
    save_plan(db, mount, &plan)?;
    Ok(list_reply(&plan, &profile))
}

fn op_remove(db: &Path, mount: &str, key: &str) -> Result<serde_json::Value, String> {
    let (mut plan, profile, _) = load_or_init(db, mount)?;
    let before = plan.items.len();
    plan.items.retain(|i| i.key != key);
    if plan.items.len() == before {
        return Err("该项不在卡清单中".into());
    }
    if plan.preload.as_deref() == Some(key) {
        plan.preload = None;
    }
    save_plan(db, mount, &plan)?;
    Ok(list_reply(&plan, &profile))
}

/// 选定卡清单默认（开机预装）项：仅存本地工作清单；真实写入须写卡授权后经内置重建器执行。
fn op_set_preload(db: &Path, mount: &str, key: &str) -> Result<serde_json::Value, String> {
    let (mut plan, profile, _) = load_or_init(db, mount)?;
    if !plan.items.iter().any(|i| i.key == key) {
        return Err("该项不在卡清单中".into());
    }
    plan.preload = Some(key.to_string());
    save_plan(db, mount, &plan)?;
    Ok(list_reply(&plan, &profile))
}

/// 用户定菜单替代名：字模/≤6/重名校验；空串=清除替代名回退原始名推导。
/// 只存本地工作清单，不改库原始名；真实写入仍须写卡授权。
fn op_set_menu_name(db: &Path, mount: &str, key: &str, name: &str) -> Result<serde_json::Value, String> {
    let (mut plan, profile, _) = load_or_init(db, mount)?;
    let i = plan.items.iter().position(|x| x.key == key).ok_or("该项不在卡清单中")?;
    let up = name.trim().to_uppercase();
    let (menu, ok) = propose_menu_name(if up.is_empty() { &plan.items[i].name } else { &up });
    if !up.is_empty() && (!ok || menu != up) {
        return Err(format!("菜单名不合规：字模限 {} 内 ≤6 字符", lut_camera::card::CHARSET));
    }
    for (j, it) in plan.items.iter().enumerate() {
        if j == i {
            continue;
        }
        let other = propose_menu_name(it.menu_name.as_deref().unwrap_or(&it.name)).0;
        if ok && other == menu {
            return Err(format!("菜单名重复：「{menu}」已被占用"));
        }
    }
    plan.items[i].menu_name = if up.is_empty() { None } else { Some(menu) };
    save_plan(db, mount, &plan)?;
    Ok(list_reply(&plan, &profile))
}

fn op_append_local(db: &Path, mount: &str, local_id: i64) -> Result<serde_json::Value, String> {
    let (mut plan, profile, _) = load_or_init(db, mount)?;
    let key = format!("local:{local_id}");
    if plan.items.iter().any(|i| i.key == key) {
        return Ok(list_reply(&plan, &profile));
    }
    if plan.items.len() >= profile.max_slots {
        return Err(format!("卡清单已满（{} 档上限），未追加", profile.max_slots));
    }
    let store = Store::open(db).map_err(|e| e.to_string())?;
    let rec = store.get_record(local_id).map_err(|e| e.to_string())?;
    if rec.is_deleted() {
        return Err("该项已在回收站，不能置入卡清单".into());
    }
    plan.items.push(CardItem {
        key,
        kind: "local".into(),
        name: rec.name.clone(),
        local_id: Some(local_id),
        card_path: None,
        fp1: None,
        fp5: None,
        menu_name: None,
    });
    save_plan(db, mount, &plan)?;
    Ok(list_reply(&plan, &profile))
}

/// 菜单名预演：字模字符集（-025ABEFGHIKLNOPRSTUVWY）≤6 字符；
/// 不合规时如实标记，替代名由用户定（与 管理滤镜.py 同约束）。
fn propose_menu_name(name: &str) -> (String, bool) {
    let filtered: String = name
        .to_uppercase()
        .chars()
        .filter(|c| lut_camera::card::CHARSET.contains(*c))
        .take(lut_camera::card::MAX_NAME_LEN)
        .collect();
    let ok = !filtered.is_empty()
        && name.chars().count() <= lut_camera::card::MAX_NAME_LEN
        && name.to_uppercase().chars().all(|c| lut_camera::card::CHARSET.contains(c));
    (filtered, ok)
}

/// 写入收集器：工作清单 → 预演行 / 不合规项 / rebuild 清单 / 重建 entries / 待写包字节。
struct WriteCollect {
    rows: Vec<serde_json::Value>,
    blocked: Vec<String>,
    filters: Vec<serde_json::Value>,
    entries: Vec<lut_camera::rebuild::RebuildEntry>,
    pkgs: Vec<(String, Vec<u8>)>,
}

fn collect_write(plan: &CardPlan, store: &Store, root: &Path) -> Result<WriteCollect, String> {
    // 先预留卡上全部已有文件，包括清单后面的保留项和未引用包。
    let mut used: HashSet<String> = std::fs::read_dir(root.join("DXO_FILTERS"))
        .map_err(|e| format!("读取滤镜目录失败: {e}"))?
        .map(|e| e.map(|e| e.file_name().to_string_lossy().to_uppercase()))
        .collect::<Result<_, _>>().map_err(|e| e.to_string())?;
    let mut c = WriteCollect { rows: Vec::new(), blocked: Vec::new(), filters: Vec::new(), entries: Vec::new(), pkgs: Vec::new() };
    let mut seen_menu: HashSet<String> = HashSet::new();
    for it in &plan.items {
        if it.kind == "card" {
            let (menu, menu_ok) = propose_menu_name(it.menu_name.as_deref().unwrap_or(&it.name));
            let fname = it.card_path.as_deref().unwrap_or("")
                .rsplit(['\\', '/']).next().unwrap_or("").to_string();
            used.insert(fname.to_uppercase());
            c.rows.push(serde_json::json!({
                "name": it.name, "action": if menu != it.name { "rename" } else { "keep" }, "file": fname,
                "menu_name": menu, "menu_ok": menu_ok, "size": lut_camera::card::PKG_SIZE,
            }));
            if !menu_ok {
                c.blocked.push(format!("「{}」菜单名不合规，需修改为字模内 ≤6 字符", it.menu_name.as_deref().unwrap_or(&it.name)));
            } else if !seen_menu.insert(menu.clone()) {
                c.blocked.push(format!("菜单名重复：「{menu}」（内置重建器拒绝重名）"));
            }
            c.filters.push(serde_json::json!({ "name": menu, "pkg": fname }));
            c.entries.push(lut_camera::rebuild::RebuildEntry {
                name: menu,
                card_path: it.card_path.clone().unwrap_or_default(),
                fp1: it.fp1.unwrap_or(0),
                fp5: it.fp5.unwrap_or(0),
            });
        } else {
            let id = it.local_id.ok_or("本地项缺少库 ID")?;
            let rec = store.get_record(id).map_err(|e| e.to_string())?;
            if rec.is_deleted() { return Err(format!("「{}」已在回收站，请先从卡清单移除", rec.name)); }
            let cc = store.load_bytes(id).map_err(|e| e.to_string())?;
            if cc.len() != lut_persist::CC_TABLE_LEN { return Err("滤镜表长度异常，未写入".into()); }
            let pkg = lut_core::pack_five_slots(&cc);
            let safe = lut_camera::card_safe_name(&rec.name, &mut HashSet::new());
            let mut fname = format!("lut_{safe}.bin");
            let mut suffix = 1;
            while used.contains(&fname.to_uppercase()) {
                fname = format!("lut_{safe}_{suffix}.bin"); suffix += 1;
            }
            used.insert(fname.to_uppercase());
            let (menu, menu_ok) = propose_menu_name(it.menu_name.as_deref().unwrap_or(&rec.name));
            if !menu_ok {
                c.blocked.push(format!(
                    "「{}」菜单名不合规（字模限 {} 内 ≤6 字符），需用户定替代名",
                    it.menu_name.as_deref().unwrap_or(&rec.name), lut_camera::card::CHARSET
                ));
            } else if !seen_menu.insert(menu.clone()) {
                c.blocked.push(format!("菜单名重复：「{menu}」（内置重建器拒绝重名）"));
            }
            let (fp1, fp5) = lut_camera::card::package_fingerprints(&pkg).map_err(|e| e.to_string())?;
            c.rows.push(serde_json::json!({
                "name": rec.name, "action": "add", "file": fname,
                "menu_name": menu, "menu_ok": menu_ok,
                "size": pkg.len(), "table_sha": Store::hash_bytes(&cc),
            }));
            c.filters.push(serde_json::json!({ "name": menu, "pkg": fname }));
            c.entries.push(lut_camera::rebuild::RebuildEntry {
                name: menu,
                card_path: format!("{}{}", lut_camera::rebuild::CARD_PREFIX, fname),
                fp1,
                fp5,
            });
            c.pkgs.push((fname, pkg));
        }
    }
    Ok(c)
}

/// F-19：读卡上 dxot/a.bin 并按档数同步触屏横滑循环常量；缺文件或无需变化返回 None。
/// 只读模板 + 离线 patch；真实落盘由整批事务处理，App 确认需独立勾选 dxot/a.bin。
fn touch_sync(root: &Path, n: usize) -> Result<Option<(PathBuf, Vec<u8>, Vec<u8>)>, String> {
    let p = root.join("dxot").join("a.bin");
    if !p.is_file() {
        return Ok(None);
    }
    let old = std::fs::read(&p).map_err(|e| format!("读取 dxot/a.bin 失败: {e}"))?;
    let new = lut_camera::rebuild::patch_touch(&old, n).map_err(|e| format!("触屏循环同步拒绝：{e}"))?;
    if new == old {
        Ok(None)
    } else {
        Ok(Some((p, old, new)))
    }
}

struct PreparedWrite {
    preview: serde_json::Value,
    files: Vec<lut_camera::batch::BatchFile>,
}

/// 生成冻结的目标字节及现状指纹；预演与执行使用同一条生成路径。
fn prepare_write(db: &Path, mount: &str) -> Result<PreparedWrite, String> {
    let (plan, profile, ph) = load_or_init(db, mount)?;
    let root = Path::new(mount);
    let store = Store::open(db).map_err(|e| e.to_string())?;
    let mut c = collect_write(&plan, &store, root)?;
    let preload = effective_preload(&plan, &profile);
    let mut files = Vec::new();
    let mut preview = serde_json::Value::Null;
    let mut touch_preview = serde_json::Value::Null;
    let mut phls_preview = serde_json::Value::Null;
    if c.blocked.is_empty() {
        match lut_camera::rebuild::rebuild_ph_menu(&ph, &c.entries, Some(preload), false, None) {
            Ok(bytes) => {
                preview = serde_json::json!({ "ok": true, "bytes": bytes.len(), "sha": Store::hash_bytes(&bytes) });
                for (fname, bytes) in &c.pkgs {
                    files.push((format!("DXO_FILTERS/{fname}"), bytes.clone()));
                }
                files.push(("ph_menu.bin".into(), bytes));
                let phls = lut_camera::rebuild::ph_last_bytes(preload as u32);
                phls_preview = serde_json::json!({ "file": "ph_last.bin", "bytes": phls.len(), "idx": preload, "sha": Store::hash_bytes(&phls) });
                files.push(("ph_last.bin".into(), phls));
                match touch_sync(root, c.entries.len()) {
                    Ok(Some((_, old, new))) => {
                        touch_preview = serde_json::json!({ "file": "dxot/a.bin", "bytes": new.len(), "sha": Store::hash_bytes(&new), "from_sha": Store::hash_bytes(&old) });
                        files.push(("dxot/a.bin".into(), new));
                    }
                    Ok(None) => {},
                    Err(e) => c.blocked.push(e),
                }
            }
            Err(e) => c.blocked.push(format!("内置重建器拒绝：{e}")),
        }
    }
    let files: Vec<lut_camera::batch::BatchFile> = files.into_iter().map(|(path, bytes)| {
        let before_sha = lut_camera::batch::before_sha(&root.join(&path)).map_err(|e| e.to_string())?;
        Ok(lut_camera::batch::BatchFile { path, bytes, before_sha })
    }).collect::<Result<_, String>>()?;
    // 含卡身份、工作清单、全部原包内容、写入前后 SHA，确认后任何变化均须重新预演。
    let mut source = Vec::new();
    for e in &profile.entries {
        let name = e.path.rsplit(['\\', '/']).next().unwrap_or("");
        let sha = lut_camera::batch::before_sha(&root.join("DXO_FILTERS").join(name)).map_err(|e| e.to_string())?;
        source.push(serde_json::json!([name, sha]));
    }
    // 卷身份参与指纹：Windows 上换卡即使盘符相同，卷 GUID/序列号也会变化，须重新确认
    let identity = lut_camera::volume::card_identity(root).map_err(|e| e.to_string())?;
    let targets: Vec<_> = files.iter().map(|f| serde_json::json!({ "file": f.path, "sha": Store::hash_bytes(&f.bytes), "from_sha": f.before_sha, "bytes": f.bytes.len() })).collect();
    let digest = Store::hash_bytes(&serde_json::to_vec(&serde_json::json!([identity, plan, source, targets])).map_err(|e| e.to_string())?);
    let reply = serde_json::json!({
        "gate_enabled": lut_camera::ensure_write_gate().is_ok(),
        "touch_gate_enabled": lut_camera::ensure_touch_write_gate().is_ok(),
        "confirmation_required": true,
        "ph_menu_action": "以卡上当前 ph_menu 为模板，由 app 内置重建器全量重建（MENU/CORE/STR 三段，其余段逐字节保留）",
        "touch_preview": touch_preview, "phls_preview": phls_preview,
        "items": c.rows, "blocked": c.blocked, "max_slots": profile.max_slots,
        "preload": preload, "preload_name": plan.items.get(preload).map(|i| i.menu_name.as_ref().unwrap_or(&i.name).clone()),
        "rebuild_preview": preview, "digest": digest, "files": targets,
        "rebuild_manifest": if c.blocked.is_empty() { serde_json::json!({ "filters": c.filters, "preload": preload }) } else { serde_json::Value::Null },
    });
    Ok(PreparedWrite { preview: reply, files })
}

fn op_write_plan(db: &Path, mount: &str) -> Result<serde_json::Value, String> {
    Ok(prepare_write(db, mount)?.preview)
}

fn apply_prepared(db: &Path, mount: &str, prepared: PreparedWrite, include_touch: bool) -> Result<serde_json::Value, String> {
    let p = &prepared.preview;
    let blocked = p["blocked"].as_array().ok_or("预演无效")?;
    if !blocked.is_empty() { return Err(format!("存在不合规项，未写入：{blocked:?}")); }
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let backup = db.join("card_backups").join(format!("{nanos}-{}", std::process::id()));
    lut_camera::batch::write_batch(Path::new(mount), &prepared.files, &backup, include_touch, &lut_camera::NoFault)?;
    let _ = std::fs::remove_file(plan_path(db, mount));
    Ok(serde_json::json!({
        "ok": true, "written": prepared.files.iter().map(|f| f.path.strip_prefix("DXO_FILTERS/").unwrap_or(&f.path)).collect::<Vec<_>>(),
        "ph_menu_sha": p["rebuild_preview"]["sha"], "ph_menu_bytes": p["rebuild_preview"]["bytes"],
        "touch_sha": p["touch_preview"]["sha"], "phls_sha": p["phls_preview"]["sha"],
        "phls_idx": p["preload"], "preload": p["preload"], "backup_dir": backup.to_string_lossy(),
    }))
}

fn apply_confirmed(db: &Path, mount: &str, digest: &str, include_touch: bool) -> Result<serde_json::Value, String> {
    let prepared = prepare_write(db, mount)?;
    if prepared.preview["digest"].as_str() != Some(digest) { return Err("未写入：滤镜、卡清单或卡内容已变化，请重新预演确认".into()); }
    apply_prepared(db, mount, prepared, include_touch)
}

// 保留 CLI/历史授权闸门测试；App 的生产入口改用一次性确认票据。
#[cfg(test)]
fn apply_inner(db: &Path, mount: &str) -> Result<serde_json::Value, String> {
    let prepared = prepare_write(db, mount)?;
    let touch = !prepared.preview["touch_preview"].is_null();
    if touch { lut_camera::ensure_touch_write_gate().map_err(|e| e.to_string())?; }
    apply_prepared(db, mount, prepared, touch)
}

#[cfg(test)]
fn op_apply(db: &Path, mount: &str) -> Result<serde_json::Value, String> {
    lut_camera::ensure_write_gate().map_err(|e| e.to_string())?;
    apply_inner(db, mount)
}

// ── IPC 命令 ────────────────────────────────────────────

#[tauri::command]
pub async fn list_cards() -> Result<Vec<serde_json::Value>, String> {
    tauri::async_runtime::spawn_blocking(|| enumerate_cards(volumes_dir()))
        .await
        .map_err(|e| format!("枚举线程失败: {e}"))
}

/// 只读探测所选卷的 ph_menu / 五槽包（不写任何文件）。
#[tauri::command]
pub async fn card_probe(mount: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || {
        let root = PathBuf::from(&mount);
        match lut_camera::probe(&root) {
            Ok(p) => Ok(serde_json::json!({ "ok": true, "profile": p })),
            Err(e) => Ok(serde_json::json!({ "ok": false, "msg": e.to_string() })),
        }
    })
    .await
    .map_err(|e| format!("探测线程失败: {e}"))?
}

#[tauri::command]
pub async fn card_list_get(mount: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_get(&Store::default_db_dir(), &mount))
        .await
        .map_err(|e| format!("读卡线程失败: {e}"))?
}

#[tauri::command]
pub async fn card_list_reorder(mount: String, keys: Vec<String>) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_reorder(&Store::default_db_dir(), &mount, &keys))
        .await
        .map_err(|e| format!("排序线程失败: {e}"))?
}

#[tauri::command]
pub async fn card_list_move(mount: String, key: String, dir: i32) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_move(&Store::default_db_dir(), &mount, &key, dir))
        .await
        .map_err(|e| format!("移动线程失败: {e}"))?
}

#[tauri::command]
pub async fn card_list_remove(mount: String, key: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_remove(&Store::default_db_dir(), &mount, &key))
        .await
        .map_err(|e| format!("移除线程失败: {e}"))?
}

/// 选定卡清单默认（开机预装）项：仅本地工作清单；真实写入由内置重建器在 App 确认后执行。
#[tauri::command]
pub async fn card_set_preload(mount: String, key: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_set_preload(&Store::default_db_dir(), &mount, &key))
        .await
        .map_err(|e| format!("默认档线程失败: {e}"))?
}

/// 本地导入同步：把库内滤镜追加到卡清单末位（待写入，不写卡）。
#[tauri::command]
pub async fn card_list_append_local(mount: String, local_id: i64) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_append_local(&Store::default_db_dir(), &mount, local_id))
        .await
        .map_err(|e| format!("追加线程失败: {e}"))?
}

/// 菜单替代名（仅本地项）：校验后存工作清单；不写卡、不改库原始名。
#[tauri::command]
pub async fn card_list_set_menu_name(mount: String, key: String, name: String) -> Result<serde_json::Value, String> {
    tauri::async_runtime::spawn_blocking(move || op_set_menu_name(&Store::default_db_dir(), &mount, &key, &name))
        .await
        .map_err(|e| format!("菜单名线程失败: {e}"))?
}

#[tauri::command]
pub async fn card_write_plan(app: tauri::AppHandle, mount: String) -> Result<serde_json::Value, String> {
    if app.state::<WriteSession>().busy.load(Ordering::SeqCst) { return Err("正在写卡，请稍候".into()); }
    let m = mount.clone();
    let mut plan = tauri::async_runtime::spawn_blocking(move || op_write_plan(&Store::default_db_dir(), &m))
        .await.map_err(|e| format!("预演线程失败: {e}"))??;
    let digest = plan["digest"].as_str().ok_or("预演指纹缺失")?;
    let token = app.state::<WriteSession>().issue(&mount, digest);
    plan["confirmation_token"] = serde_json::json!(token);
    Ok(plan)
}

/// 票据只能消费一次，绑定本次预演、挂载点及五分钟有效期；不修改进程环境。
#[tauri::command]
pub async fn card_apply(app: tauri::AppHandle, mount: String, confirmation_token: String, include_touch: bool) -> Result<serde_json::Value, String> {
    let state = app.state::<WriteSession>();
    if state.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() { return Err("正在写卡，请勿重复提交".into()); }
    let result = async {
        if !app.state::<crate::Session>().pending.lock().unwrap().is_empty() { return Err("未写入：仍有未保存参数，请先保存再确认".into()); }
        let digest = state.consume(&mount, &confirmation_token)?;
        tauri::async_runtime::spawn_blocking(move || apply_confirmed(&Store::default_db_dir(), &mount, &digest, include_touch))
            .await.map_err(|e| format!("写卡线程异常，写入状态需检查本地备份: {e}"))?
    }.await;
    state.busy.store(false, Ordering::SeqCst);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use lut_camera::card::{C_BOOT_CMP_69, C_PRELOAD_MOV_69, CORE_BASE, LOG_HDR_BASE, MAX_NAME_LEN, PATH_STRIDE, SLOT_ADDRS, SLOT_SIZE, STR_BASE, FP_OFFSET};

    fn seg(addr: u32, payload: &[u8]) -> Vec<u8> {
        let mut v = Vec::new();
        v.extend_from_slice(&addr.to_le_bytes());
        v.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        v.extend_from_slice(payload);
        v
    }

    fn str_body(entries: &[(String, u32, u32)]) -> Vec<u8> {
        let n = entries.len();
        let mut v = vec![0u8; n * (PATH_STRIDE + 8)];
        for (i, (path, f1, f5)) in entries.iter().enumerate() {
            v[i * PATH_STRIDE..i * PATH_STRIDE + path.len()].copy_from_slice(path.as_bytes());
            let off = n * PATH_STRIDE + i * 8;
            v[off..off + 4].copy_from_slice(&f1.to_le_bytes());
            v[off + 4..off + 8].copy_from_slice(&f5.to_le_bytes());
        }
        v
    }

    fn package(fp1: u32, fp5: u32) -> Vec<u8> {
        let mut v = Vec::new();
        for (i, &addr) in SLOT_ADDRS.iter().enumerate() {
            v.extend_from_slice(&addr.to_le_bytes());
            v.extend_from_slice(&(SLOT_SIZE as u32).to_le_bytes());
            let mut body = vec![0u8; SLOT_SIZE];
            let fp = if i == 1 { fp1 } else if i == 4 { fp5 } else { 0 };
            body[FP_OFFSET..FP_OFFSET + 4].copy_from_slice(&fp.to_le_bytes());
            v.extend_from_slice(&body);
        }
        v
    }

    fn ph_menu(entries: &[(String, u32, u32)], magic: u32) -> Vec<u8> {
        let n = entries.len();
        let mut core = vec![0u8; C_PRELOAD_MOV_69 + 8];
        core[C_BOOT_CMP_69..C_BOOT_CMP_69 + 4]
            .copy_from_slice(&(0xE350_0000 | n as u32 - 1).to_le_bytes());
        core[C_PRELOAD_MOV_69..C_PRELOAD_MOV_69 + 4].copy_from_slice(&0xE3A0_0002u32.to_le_bytes());
        let mut log = vec![0u8; 16];
        log[12..16].copy_from_slice(&magic.to_le_bytes());
        let mut v = Vec::new();
        v.extend(seg(CORE_BASE, &core));
        v.extend(seg(LOG_HDR_BASE, &log));
        v.extend(seg(STR_BASE, &str_body(entries)));
        v
    }

    fn sim_volume(entries: &[(String, u32, u32)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ph_menu.bin"), ph_menu(entries, 0x5047_3546)).unwrap();
        std::fs::create_dir_all(dir.path().join("DXO_FILTERS")).unwrap();
        for (path, f1, f5) in entries {
            let file = path.rsplit('\\').next().unwrap();
            std::fs::write(dir.path().join("DXO_FILTERS").join(file), package(*f1, *f5)).unwrap();
        }
        dir
    }

    fn two_entries() -> Vec<(String, u32, u32)> {
        vec![
            ("c:\\DXO_FILTERS\\lut_a.bin".to_string(), 0x11, 0x21),
            ("c:\\DXO_FILTERS\\lut_b.bin".to_string(), 0x12, 0x22),
        ]
    }

    fn insert_local(db: &Path, name: &str) -> i64 {
        let mut store = Store::open(db).unwrap();
        let cc = vec![0xAB; lut_persist::CC_TABLE_LEN];
        let rec = store.insert(lut_persist::NewFilter {
            name, tone: "pass", strength: 0.65, bw: false,
            cc_bytes: &cc, src: "t.cube", format: "cube", source_size: 2,
            locked: false, cube_blob: None,
        }).unwrap();
        rec.id
    }

    fn items_of(v: &serde_json::Value) -> Vec<CardItem> {
        serde_json::from_value(v["items"].clone()).unwrap()
    }

    #[test]
    fn test_init_from_card_and_persist() {
        let vol = sim_volume(&two_entries());
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let v = op_get(db.path(), &mount).unwrap();
        let items = items_of(&v);
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].key, "card:00000011");
        assert_eq!(v["n_slots"], 2);
        assert_eq!(v["gate_enabled"], false);
        assert!(plan_path(db.path(), &mount).exists());
        let v2 = op_get(db.path(), &mount).unwrap();
        assert_eq!(items_of(&v2).len(), 2);
    }

    #[test]
    fn test_saved_plan_card_names_refresh_from_card() {
        let vol = sim_volume(&two_entries());
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let fresh: Vec<String> = items_of(&op_get(db.path(), &mount).unwrap())
            .iter().map(|i| i.name.clone()).collect();
        let pp = plan_path(db.path(), &mount);
        let mut plan: CardPlan = serde_json::from_str(&std::fs::read_to_string(&pp).unwrap()).unwrap();
        for it in plan.items.iter_mut() {
            it.name = "STALE".into();
        }
        std::fs::write(&pp, serde_json::to_string(&plan).unwrap()).unwrap();
        let items = items_of(&op_get(db.path(), &mount).unwrap());
        assert_eq!(items.iter().map(|i| i.name.clone()).collect::<Vec<_>>(), fresh);
    }

    #[test]
    fn test_preload_select_follows_reorder_and_remove() {
        let vol = sim_volume(&two_entries());
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let v = op_get(db.path(), &mount).unwrap();
        assert_eq!(v["preload"], 0, "卡预装 idx 越界回退 0");
        let v = op_set_preload(db.path(), &mount, "card:00000012").unwrap();
        assert_eq!(v["preload"], 1);
        let v = op_reorder(db.path(), &mount, &["card:00000012".into(), "card:00000011".into()]).unwrap();
        assert_eq!(v["preload"], 0, "默认跟随项而非 idx");
        let v = op_remove(db.path(), &mount, "card:00000012").unwrap();
        assert_eq!(v["preload"], 0, "移除默认项后回退卡映射");
        assert!(op_set_preload(db.path(), &mount, "card:deadbeef").is_err());
    }

    #[test]
    fn test_preload_default_maps_card_preload_by_fp() {
        let entries = vec![
            ("c:\\DXO_FILTERS\\lut_a.bin".to_string(), 0x11, 0x21),
            ("c:\\DXO_FILTERS\\lut_b.bin".to_string(), 0x12, 0x22),
            ("c:\\DXO_FILTERS\\lut_c.bin".to_string(), 0x13, 0x23),
        ];
        let vol = sim_volume(&entries);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let v = op_get(db.path(), &mount).unwrap();
        assert_eq!(v["preload"], 2, "沿用卡上预装档");
        let v = op_remove(db.path(), &mount, "card:00000011").unwrap();
        assert_eq!(v["preload"], 1, "按指纹映射到保留项新 idx");
    }

    #[test]
    fn test_write_plan_manifest_and_preload() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        op_set_preload(db.path(), &mount, &format!("local:{id}")).unwrap();
        let p = op_write_plan(db.path(), &mount).unwrap();
        assert_eq!(p["preload"], 2);
        assert_eq!(p["preload_name"], "VELVIA");
        let m = &p["rebuild_manifest"];
        assert_eq!(m["filters"].as_array().unwrap().len(), 3);
        assert_eq!(m["preload"], 2);
        assert_eq!(m["filters"][2]["name"], "VELVIA");
        assert_eq!(m["filters"][0]["name"], "ORIG", "卡项名用相机菜单名解码");
        assert!(p["blocked"].as_array().unwrap().is_empty());
        let id2 = insert_local(db.path(), "温暖");
        op_append_local(db.path(), &mount, id2).unwrap();
        let p2 = op_write_plan(db.path(), &mount).unwrap();
        assert!(p2["rebuild_manifest"].is_null(), "不合规时不给 rebuild 清单");
        assert!(!p2["blocked"].as_array().unwrap().is_empty());
    }

    #[test]
    fn test_set_menu_name_override_clears_blocked() {
        let vol = sim_volume_full(&["ORIG", "G200"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "GOLD");
        let v = op_append_local(db.path(), &mount, id).unwrap();
        let key = format!("local:{id}");
        let raw_of = |v: &serde_json::Value| -> serde_json::Value {
            v["items"].as_array().unwrap().iter().find(|x| x["key"] == key).unwrap().clone()
        };
        let raw = raw_of(&v);
        assert_eq!(raw["menu"], "GOL", "D 不在字模被过滤");
        assert_eq!(raw["menu_ok"], false);
        assert!(raw["menu_name"].is_null());
        let p = op_write_plan(db.path(), &mount).unwrap();
        assert!(!p["blocked"].as_array().unwrap().is_empty());
        // 不合规 / 超长 / 重名 / 不存在项均拒
        assert!(op_set_menu_name(db.path(), &mount, &key, "GOLD").is_err());
        assert!(op_set_menu_name(db.path(), &mount, &key, "TOOLONG").is_err());
        assert!(op_set_menu_name(db.path(), &mount, &key, "G200").is_err());
        assert!(op_set_menu_name(db.path(), &mount, "card:deadbeef", "GOL2").is_err());
        assert!(op_set_menu_name(db.path(), &mount, "local:999", "GOL2").is_err());
        // 合规替代名（小写输入归一大写）清除 blocked，库原始名不变
        let v = op_set_menu_name(db.path(), &mount, &key, "gol2").unwrap();
        let raw = raw_of(&v);
        assert_eq!(raw["menu"], "GOL2");
        assert_eq!(raw["menu_ok"], true);
        assert_eq!(raw["menu_name"], "GOL2");
        assert_eq!(raw["name"], "GOLD");
        let p = op_write_plan(db.path(), &mount).unwrap();
        assert!(p["blocked"].as_array().unwrap().is_empty(), "{:?}", p["blocked"]);
        assert_eq!(p["rebuild_manifest"]["filters"][2]["name"], "GOL2");
        // 空串清除替代名 → 回退原始名推导（再不合规）
        let v = op_set_menu_name(db.path(), &mount, &key, "  ").unwrap();
        let raw = raw_of(&v);
        assert_eq!(raw["menu_ok"], false);
        assert!(raw["menu_name"].is_null());
    }

    #[test]
    fn card_rename_persists_validates_and_confirmed_write_keeps_packages() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 0);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let initial = op_get(db.path(), &mount).unwrap();
        let key = initial["items"][0]["key"].as_str().unwrap();
        let other = initial["items"][1]["key"].as_str().unwrap();
        let ph_before = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        let packages: Vec<_> = std::fs::read_dir(vol.path().join("DXO_FILTERS")).unwrap()
            .map(|e| { let p = e.unwrap().path(); let bytes = std::fs::read(&p).unwrap(); (p, bytes) }).collect();
        let v = op_set_menu_name(db.path(), &mount, key, " flat ").unwrap();
        assert_eq!(v["items"][0]["menu"], "FLAT");
        assert_eq!(v["items"][0]["name"], "ORIG");
        assert_eq!(v["changed"]["rename"], 1);
        let saved = std::fs::read(plan_path(db.path(), &mount)).unwrap();
        for bad in ["GOL2", "GOLD", "FLAT 2", "中文", "TOOLONG"] {
            assert!(op_set_menu_name(db.path(), &mount, key, bad).is_err(), "{bad}");
            assert_eq!(std::fs::read(plan_path(db.path(), &mount)).unwrap(), saved);
        }
        let id = insert_local(db.path(), "VELVIA");
        let local = format!("local:{id}");
        op_append_local(db.path(), &mount, id).unwrap();
        assert!(op_set_menu_name(db.path(), &mount, &local, "FLAT").is_err());
        assert!(op_set_menu_name(db.path(), &mount, key, "VELVIA").is_err());
        op_remove(db.path(), &mount, &local).unwrap();
        op_set_menu_name(db.path(), &mount, other, "ORIG").unwrap();
        // 清空替代名也要检查重名：原名已被其他项占用。
        assert!(op_set_menu_name(db.path(), &mount, key, "").is_err());
        op_set_menu_name(db.path(), &mount, other, "").unwrap();
        let reset = op_set_menu_name(db.path(), &mount, key, "").unwrap();
        assert_eq!(reset["items"][0]["menu"], "ORIG");
        assert_eq!(reset["changed"]["rename"], 0);
        op_set_menu_name(db.path(), &mount, key, "FLAT").unwrap();
        let restored = op_get(db.path(), &mount).unwrap();
        assert_eq!(restored["items"][0]["menu"], "FLAT");
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), ph_before);
        let plan = op_write_plan(db.path(), &mount).unwrap();
        assert!(plan["blocked"].as_array().unwrap().is_empty());
        assert_eq!(plan["items"][0]["action"], "rename");
        assert_eq!(plan["items"][0]["menu_name"], "FLAT");
        assert_eq!(plan["preload_name"], "FLAT");
        assert_eq!(plan["rebuild_manifest"]["filters"][0]["name"], "FLAT");
        assert_eq!(plan["files"].as_array().unwrap().len(), 2, "仅菜单和开机记忆，不重写滤镜包");
        apply_confirmed(db.path(), &mount, plan["digest"].as_str().unwrap(), false).unwrap();
        let profile = lut_camera::probe(vol.path()).unwrap();
        assert_eq!(profile.entries[0].name, "FLAT");
        assert_eq!(profile.entries[1].name, "GOL2");
        assert_eq!(profile.preload, 0);
        for (path, bytes) in packages { assert_eq!(std::fs::read(path).unwrap(), bytes); }
        let fresh = op_get(db.path(), &mount).unwrap();
        assert_eq!(fresh["items"][0]["name"], "FLAT");
        assert!(fresh["items"][0]["menu_name"].is_null());
        assert_eq!(fresh["changed"]["rename"], 0);
    }

    #[test]
    fn card_rename_after_confirmation_requires_new_preview() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 0);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let initial = op_get(db.path(), &mount).unwrap();
        let key = initial["items"][0]["key"].as_str().unwrap();
        let plan = op_write_plan(db.path(), &mount).unwrap();
        let before = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        op_set_menu_name(db.path(), &mount, key, "FLAT").unwrap();
        assert!(apply_confirmed(db.path(), &mount, plan["digest"].as_str().unwrap(), false).unwrap_err().contains("已变化"));
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), before);
    }

    #[test]
    fn test_append_local_tail_dedupe_and_capacity() {
        let entries: Vec<(String, u32, u32)> = (0..12)
            .map(|i| (format!("c:\\DXO_FILTERS\\lut_f{i}.bin"), 0x100 + i, 0x200 + i))
            .collect();
        let vol = sim_volume(&entries);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "NEWLOOK");
        // 满容量（12 档）拒绝追加
        let err = op_append_local(db.path(), &mount, id).unwrap_err();
        assert!(err.contains("已满"), "{err}");
        // 删一档后追加到末位
        op_remove(db.path(), &mount, "card:00000100").unwrap();
        let v = op_append_local(db.path(), &mount, id).unwrap();
        let items = items_of(&v);
        assert_eq!(items.len(), 12);
        assert_eq!(items.last().unwrap().key, format!("local:{id}"));
        assert_eq!(items.last().unwrap().name, "NEWLOOK");
        // 去重：再追加同一 id 不产生第二行
        let v2 = op_append_local(db.path(), &mount, id).unwrap();
        assert_eq!(items_of(&v2).len(), 12);
        assert_eq!(v["changed"]["add"], 1);
        assert_eq!(v["changed"]["del"], 1);
        // 回收站项拒绝
        let id2 = insert_local(db.path(), "TRASHED");
        Store::open(db.path()).unwrap().soft_delete(id2).unwrap();
        op_remove(db.path(), &mount, "card:00000101").unwrap();
        assert!(op_append_local(db.path(), &mount, id2).is_err());
    }

    #[test]
    fn test_reorder_move_remove_and_mismatch() {
        let vol = sim_volume(&two_entries());
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let ka = "card:00000011".to_string();
        let kb = "card:00000012".to_string();
        // 下移首项 = 交换
        let v = op_move(db.path(), &mount, &ka, 1).unwrap();
        assert_eq!(items_of(&v)[0].key, kb);
        assert_eq!(v["changed"]["order"], true);
        // 边界移动不报错、不改序
        let v = op_move(db.path(), &mount, &kb, -1).unwrap();
        assert_eq!(items_of(&v)[0].key, kb);
        // 拖拽全量排序
        let v = op_reorder(db.path(), &mount, &[ka.clone(), kb.clone()]).unwrap();
        assert_eq!(items_of(&v)[0].key, ka);
        assert_eq!(v["changed"]["order"], false);
        // key 集合不一致必须拒绝
        assert!(op_reorder(db.path(), &mount, &[ka.clone()]).is_err());
        assert!(op_reorder(db.path(), &mount, &[ka.clone(), kb.clone(), "card:ffffffff".into()]).is_err());
        // 删除
        let v = op_remove(db.path(), &mount, &ka).unwrap();
        assert_eq!(items_of(&v).len(), 1);
        assert_eq!(v["changed"]["del"], 1);
        assert!(op_remove(db.path(), &mount, &ka).is_err());
        // 持久化：重新载入仍是删后状态
        let v = op_get(db.path(), &mount).unwrap();
        assert_eq!(items_of(&v).len(), 1);
    }

    #[test]
    fn test_card_change_reinitializes_plan() {
        let vol = sim_volume(&two_entries());
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        op_remove(db.path(), &mount, "card:00000011").unwrap();
        assert_eq!(items_of(&op_get(db.path(), &mount).unwrap()).len(), 1);
        // 卡上 ph_menu 变化（换模板）→ 工作清单按卡重建
        std::fs::write(vol.path().join("ph_menu.bin"), ph_menu(&two_entries(), 0x5047_3545)).unwrap();
        let v = op_get(db.path(), &mount).unwrap();
        assert_eq!(items_of(&v).len(), 2, "卡内容变化后必须重读，不得沿用旧清单");
    }

    #[test]
    fn test_menu_name_charset_rules() {
        let (m, ok) = propose_menu_name("FROST");
        assert_eq!(m, "FROST");
        assert!(ok);
        let (m, ok) = propose_menu_name("GOLD");
        assert!(!ok, "D 不在字模字符集");
        assert_eq!(m, "GOL");
        let (m, ok) = propose_menu_name("K12_SATOBI");
        assert!(!ok, "下划线/超长/数字 1 不合规");
        assert_eq!(m, "K2SATO");
        let (m, ok) = propose_menu_name("MIYABI");
        assert!(!ok, "M 不在字模字符集");
        assert_eq!(m, "IYABI");
        let (_, ok) = propose_menu_name("___");
        assert!(!ok);
        assert_eq!(lut_camera::card::CHARSET.len(), 23);
        assert_eq!(MAX_NAME_LEN, 6);
    }

    #[test]
    fn test_write_plan_readonly_and_flags() {
        let vol = sim_volume(&two_entries());
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "K12_SATOBI");
        op_append_local(db.path(), &mount, id).unwrap();
        let v = op_write_plan(db.path(), &mount).unwrap();
        assert_eq!(v["gate_enabled"], false);
        assert_eq!(v["items"].as_array().unwrap().len(), 3);
        assert_eq!(v["items"][2]["action"], "add");
        assert_eq!(v["items"][2]["menu_ok"], false);
        assert_eq!(v["blocked"].as_array().unwrap().len(), 1);
        assert_eq!(v["items"][2]["size"], lut_camera::card::PKG_SIZE);
        // 预演不写卡：卡上文件指纹不变
        let before = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        op_write_plan(db.path(), &mount).unwrap();
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), before);
    }

    #[test]
    fn test_write_gate_stays_disabled() {
        assert!(!lut_camera::WRITE_GATE_ENABLED);
        assert!(lut_camera::ensure_write_gate().is_err());
    }

    /// 带完整 MENU 段的模拟卷（内置重建器合成模板，不含固件版权字节）。
    fn sim_volume_full(names: &[&str], preload: usize) -> tempfile::TempDir {
        let ns: Vec<String> = names.iter().map(|s| s.to_string()).collect();
        let ph = lut_camera::rebuild::synth::synth_ph_menu(&ns, "72", preload).unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("ph_menu.bin"), ph).unwrap();
        std::fs::create_dir_all(dir.path().join("DXO_FILTERS")).unwrap();
        for (i, nm) in names.iter().enumerate() {
            let f1 = 0x11 + i as u32;
            let f5 = 0x21 + i as u32;
            std::fs::write(dir.path().join("DXO_FILTERS").join(format!("lut_{nm}.bin")), package(f1, f5)).unwrap();
        }
        dir
    }

    fn touch_fixture(vol: &tempfile::TempDir) -> Vec<u8> {
        // 真实 a.bin 为记录流（seg1 头 8 B + 载荷）；T_* 为文件偏移，载荷内偏移 = off-8。
        let mut payload = vec![0u8; 0x2000];
        for (off, w) in [
            (0x04B4usize, 0xE350_0012u32),
            (0x04BC, 0xE35B_0000),
            (0x04C0, 0x0A00_0008),
            (0x04C4, 0xE350_0012),
            (0x04C8, 0x0A00_0023),
            (0x04D4, 0x0A00_000B),
            (0x04E4, 0xEA00_0007),
            (0x0508, 0xE1A0_000A),
            (0x050C, 0xEBFC_3964),
            (0x04CC, 0xE350_0000),
            (0x04D0, 0x03A0_100A),
            (0x04D8, 0xE350_0009),
            (0x04DC, 0x03A0_1000),
            (0x04E0, 0x1280_1001),
            (0x04E8, 0xE350_0001),
            (0x04EC, 0x0A00_001A),
            (0x04F0, 0xE350_0000),
            (0x04F4, 0x03A0_1009),
            (0x04F8, 0x0A00_0002),
            (0x04FC, 0xE350_000A),
            (0x0500, 0x03A0_1000),
            (0x0504, 0x1240_1001),
        ] {
            payload[off - 8..off - 8 + 4].copy_from_slice(&w.to_le_bytes());
        }
        let mut a = Vec::new();
        a.extend_from_slice(&0x08EE_5080u32.to_le_bytes());
        a.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        a.extend_from_slice(&payload);
        std::fs::create_dir_all(vol.path().join("dxot")).unwrap();
        std::fs::write(vol.path().join("dxot").join("a.bin"), &a).unwrap();
        a
    }

    static TOUCH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TouchEnvGuard;
    impl Drop for TouchEnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(lut_camera::TOUCH_WRITE_AUTH_ENV);
        }
    }

    /// F-19：预演给 touch_preview、apply 落盘 dxot/a.bin 且幂等（常量吻合后不重写）。
    #[test]
    fn test_touch_sync_preview_apply_and_idempotent() {
        let _lock = TOUCH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let a = touch_fixture(&vol);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        let p = op_write_plan(db.path(), &mount).unwrap();
        assert!(p["blocked"].as_array().unwrap().is_empty());
        let tp = &p["touch_preview"];
        assert_eq!(tp["file"], "dxot/a.bin");
        assert_eq!(tp["bytes"].as_u64().unwrap(), (8 + 0x2000) as u64);
        assert_eq!(tp["sha"].as_str().unwrap().len(), 64);
        assert_ne!(tp["sha"], tp["from_sha"]);
        assert_eq!(p["touch_gate_enabled"], false, "默认启动触屏点名闸门须恒禁用");
        assert_eq!(std::fs::read(vol.path().join("dxot").join("a.bin")).unwrap(), a, "预演不写卡");
        let _guard = TouchEnvGuard;
        std::env::set_var(lut_camera::TOUCH_WRITE_AUTH_ENV, "1");
        let res = apply_inner(db.path(), &mount).unwrap();
        let written: Vec<String> = res["written"].as_array().unwrap().iter()
            .map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(written.contains(&"dxot/a.bin".to_string()), "{written:?}");
        let after = std::fs::read(vol.path().join("dxot").join("a.bin")).unwrap();
        assert_eq!(after, lut_camera::rebuild::patch_touch(&a, 3).unwrap());
        assert_eq!(res["touch_sha"].as_str().unwrap(), Store::hash_bytes(&after));
        // 幂等：apply 后清单按新卡重建（仍 n=3）→ 常量已吻合，touch_preview null、不再重写 a.bin
        let p2 = op_write_plan(db.path(), &mount).unwrap();
        assert!(p2["touch_preview"].is_null());
        let res2 = apply_inner(db.path(), &mount).unwrap();
        let w2: Vec<String> = res2["written"].as_array().unwrap().iter()
            .map(|v| v.as_str().unwrap().to_string()).collect();
        assert!(!w2.contains(&"dxot/a.bin".to_string()), "{w2:?}");
        assert!(res2["touch_sha"].is_null());
    }

    #[test]
    fn test_touch_sync_absent_volume_unchanged() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        let p = op_write_plan(db.path(), &mount).unwrap();
        assert!(p["touch_preview"].is_null());
        let res = apply_inner(db.path(), &mount).unwrap();
        assert_eq!(res["written"].as_array().unwrap().len(), 3, "缺 a.bin：新包 + ph_menu + ph_last");
        assert!(res["touch_sha"].is_null());
    }

    /// F-19：需触屏同步但未获点名授权 → 整体保护性拒写，ph_menu 与 a.bin 均零变化。
    #[test]
    fn test_touch_sync_unauthorized_rejects_whole_write() {
        let _lock = TOUCH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(lut_camera::TOUCH_WRITE_AUTH_ENV);
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let a = touch_fixture(&vol);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        let ph_before = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        let err = apply_inner(db.path(), &mount).unwrap_err();
        assert!(err.contains("dxot/a.bin 写入禁用"), "{err}");
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), ph_before, "拒写须卡零变化");
        assert_eq!(std::fs::read(vol.path().join("dxot").join("a.bin")).unwrap(), a);
        assert!(!vol.path().join("ph_last.bin").exists(), "整体拒写不得创建 ph_last");
    }

    #[test]
    fn test_touch_sync_gate_rejection_keeps_a_bin() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let a = touch_fixture(&vol);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        let err = op_apply(db.path(), &mount).unwrap_err();
        assert!(err.contains("写卡入口禁用"), "{err}");
        assert_eq!(std::fs::read(vol.path().join("dxot").join("a.bin")).unwrap(), a, "闸门拒绝不得动 a.bin");
    }

    #[test]
    fn test_apply_rejected_by_gate() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let before = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        let err = op_apply(db.path(), &mount).unwrap_err();
        assert!(err.contains("写卡入口禁用"), "{err}");
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), before, "闸门拒绝不得写卡");
    }

    #[test]
    fn test_write_plan_rebuild_preview_offline() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        op_set_preload(db.path(), &mount, &format!("local:{id}")).unwrap();
        let before = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        let p = op_write_plan(db.path(), &mount).unwrap();
        assert!(p["blocked"].as_array().unwrap().is_empty());
        let pv = &p["rebuild_preview"];
        assert_eq!(pv["ok"], true);
        assert!(pv["bytes"].as_u64().unwrap() > 0);
        assert_eq!(pv["sha"].as_str().unwrap().len(), 64);
        assert_eq!(p["rebuild_manifest"]["filters"].as_array().unwrap().len(), 3);
        assert!(p["ph_menu_action"].as_str().unwrap().contains("内置重建器"));
        // F-20：预演给 ph_last 重置（idx=preload=2），且不写卡
        let lp = &p["phls_preview"];
        assert_eq!(lp["file"], "ph_last.bin");
        assert_eq!(lp["bytes"].as_u64().unwrap(), 8);
        assert_eq!(lp["idx"], 2);
        assert_eq!(lp["sha"].as_str().unwrap(), Store::hash_bytes(&lut_camera::rebuild::ph_last_bytes(2)));
        assert!(!vol.path().join("ph_last.bin").exists(), "预演不得创建 ph_last");
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), before, "预演不写卡");
        // 不合规清单：不给 preview / manifest
        let id2 = insert_local(db.path(), "温暖");
        op_append_local(db.path(), &mount, id2).unwrap();
        let p2 = op_write_plan(db.path(), &mount).unwrap();
        assert!(p2["rebuild_preview"].is_null());
        assert!(p2["rebuild_manifest"].is_null());
        assert!(p2["phls_preview"].is_null());
    }

    /// F-20：写卡闸门拒绝时 ph_last 亦零变化（不创建、不覆盖）。
    #[test]
    fn test_gate_rejection_keeps_ph_last() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        std::fs::write(vol.path().join("ph_last.bin"), lut_camera::rebuild::ph_last_bytes(1)).unwrap();
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        let before = std::fs::read(vol.path().join("ph_last.bin")).unwrap();
        let err = op_apply(db.path(), &mount).unwrap_err();
        assert!(err.contains("写卡入口禁用"), "{err}");
        assert_eq!(std::fs::read(vol.path().join("ph_last.bin")).unwrap(), before, "闸门拒绝不得动 ph_last");
    }

    /// 写入管线端到端（仅模拟卷）：新包 + ph_menu 落盘、菜单名/默认档可被只读探测解回、
    /// 保留档包字节不变、工作清单按新卡重置。生产入口仍被闸门恒拒绝。
    #[test]
    fn test_apply_inner_sim_volume_end_to_end() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let keep_before = std::fs::read(vol.path().join("DXO_FILTERS").join("lut_ORIG.bin")).unwrap();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        op_set_preload(db.path(), &mount, &format!("local:{id}")).unwrap();
        let res = apply_inner(db.path(), &mount).unwrap();
        assert_eq!(res["ok"], true);
        assert_eq!(res["written"].as_array().unwrap().len(), 3, "新包 + ph_menu + ph_last");
        assert_eq!(res["preload"], 2);
        // F-20：ph_last 记忆重置为开机默认档（preload=2）
        let phls = std::fs::read(vol.path().join("ph_last.bin")).unwrap();
        assert_eq!(phls, lut_camera::rebuild::ph_last_bytes(2));
        assert_eq!(res["phls_idx"], 2);
        assert_eq!(res["phls_sha"].as_str().unwrap(), Store::hash_bytes(&phls));
        let profile = lut_camera::probe(vol.path()).unwrap();
        let names: Vec<&str> = profile.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, vec!["ORIG", "GOL2", "VELVIA"]);
        assert_eq!(profile.preload, 2);
        assert_eq!(profile.build_tag.as_deref(), Some("72"));
        assert_eq!(std::fs::read(vol.path().join("DXO_FILTERS").join("lut_ORIG.bin")).unwrap(), keep_before);
        let newpkg = std::fs::read(vol.path().join("DXO_FILTERS").join("lut_VELVIA.bin")).unwrap();
        assert_eq!(newpkg.len(), lut_camera::card::PKG_SIZE);
        // 工作清单已重置：再读按新卡初始化（3 档全 card）
        let v = op_get(db.path(), &mount).unwrap();
        let items = items_of(&v);
        assert_eq!(items.len(), 3);
        assert!(items.iter().all(|i| i.kind == "card"));
        assert_eq!(v["preload"], 2);
        // 二次 no-op 写入：ph_menu 逐字节不变
        let ph1 = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        apply_inner(db.path(), &mount).unwrap();
        assert_eq!(std::fs::read(vol.path().join("ph_menu.bin")).unwrap(), ph1);
    }

    /// 目录式卷枚举是 macOS/Linux 路径；Windows 走盘符查询，另由 volume 模块的策略测试覆盖。
    #[cfg(not(windows))]
    #[test]
    fn test_enumerate_cards_filters_internal_volumes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("RandomUSB")).unwrap();
        std::fs::create_dir(dir.path().join("DXOCard")).unwrap();
        std::fs::write(dir.path().join("DXOCard").join("ph_menu.bin"), b"x").unwrap();
        // 无 diskutil 信息时：含 ph_menu 的卷入选，普通目录不入选
        let out = enumerate_cards(dir.path());
        assert_eq!(out.len(), 1);
        assert_eq!(out[0]["volume_name"], "DXOCard");
        assert_eq!(out[0]["has_ph_menu"], true);
    }

    #[test]
    fn card_candidate_rule_is_shared_across_platforms() {
        assert!(keep_card(true, false, false), "外置卷入选");
        assert!(keep_card(false, true, false), "含 ph_menu 的卷入选");
        assert!(keep_card(false, false, true), "显式配置的相机卷入选");
        assert!(!keep_card(false, false, false), "内置盘上的普通目录不得入选");
    }

    /// 真实写卡入口：默认忽略；仅用户逐次点名授权后显式运行：
    /// DXO_CARD_WRITE_AUTH=1 DXO_REAL_CARD_MOUNT="/Volumes/DXO ONE" \
    ///   cargo test -p dxo-app --bin dxo-app real_card_apply_authorized -- --ignored --nocapture
    #[test]
    #[ignore]
    fn real_card_apply_authorized() {
        assert_eq!(
            std::env::var("DXO_CARD_WRITE_AUTH").ok().as_deref(),
            Some("1"),
            "未获逐次授权：须显式 DXO_CARD_WRITE_AUTH=1"
        );
        let mount = std::env::var("DXO_REAL_CARD_MOUNT").expect("DXO_REAL_CARD_MOUNT 未设置");
        let rep = apply_inner(&Store::default_db_dir(), &mount).expect("apply_inner 失败");
        eprintln!("REAL_WRITE_OK {rep}");
    }

    /// 实卡只读取证（手动运行，屏幕锁定时亦可）：
    /// cargo test -p dxo-app realcard_readonly_evidence -- --ignored --nocapture
    /// 工作清单落临时目录；断言卡上 ph_menu 与 DXO_FILTERS 字节全程不变。
    #[test]
    #[ignore]
    fn realcard_readonly_evidence() {
        let mount = "/Volumes/DXO ONE";
        let root = Path::new(mount);
        if !root.join("ph_menu.bin").is_file() {
            eprintln!("SKIP: 相机卡未挂载");
            return;
        }
        let ph_before = std::fs::read(root.join("ph_menu.bin")).unwrap();
        let filters_before: Vec<_> = std::fs::read_dir(root.join("DXO_FILTERS"))
            .unwrap()
            .flatten()
            .map(|e| (e.file_name(), std::fs::read(e.path()).unwrap()))
            .collect();

        let cards = enumerate_cards(volumes_dir());
        eprintln!("== 卷枚举 ==");
        for c in &cards {
            eprintln!("  {} · {} · known={} ph={}", c["volume_name"], c["device"], c["is_known"], c["has_ph_menu"]);
        }
        assert!(cards.iter().any(|c| c["mount_point"] == mount), "相机卡必须在枚举结果中");

        let db = tempfile::tempdir().unwrap();
        let v = op_get(db.path(), mount).unwrap();
        eprintln!("== 卡清单（工作清单初始化） ==");
        eprintln!("  build_tag={} n_slots={} max_slots={} gate={}", v["build_tag"], v["n_slots"], v["max_slots"], v["gate_enabled"]);
        for it in v["items"].as_array().unwrap() {
            eprintln!("  [{}] {} fp1={:08x}", it["kind"], it["name"], it["fp1"].as_u64().unwrap_or(0));
        }
        assert_eq!(v["gate_enabled"], false);

        // 增删排序只作用于临时工作清单
        let first_key = v["items"][0]["key"].as_str().unwrap().to_string();
        let v = op_move(db.path(), mount, &first_key, 1).unwrap();
        assert_eq!(v["changed"]["order"], true);
        let v = op_remove(db.path(), mount, &first_key).unwrap();
        assert_eq!(v["changed"]["del"], 1);
        let plan = op_write_plan(db.path(), mount).unwrap();
        eprintln!("== 写入预演（只读） ==");
        eprintln!("  {} 档 · blocked={} · {}", plan["items"].as_array().unwrap().len(), plan["blocked"].as_array().unwrap().len(), plan["ph_menu_action"]);
        assert_eq!(plan["gate_enabled"], false);

        // 红线断言：卡上字节全程不变
        assert_eq!(std::fs::read(root.join("ph_menu.bin")).unwrap(), ph_before, "ph_menu 不得变化");
        let filters_after: Vec<_> = std::fs::read_dir(root.join("DXO_FILTERS"))
            .unwrap()
            .flatten()
            .map(|e| (e.file_name(), std::fs::read(e.path()).unwrap()))
            .collect();
        assert_eq!(filters_after.len(), filters_before.len(), "DXO_FILTERS 文件数不得变化");
        for (name, bytes) in &filters_before {
            let after = filters_after.iter().find(|(n, _)| n == name).unwrap();
            assert_eq!(&after.1, bytes, "DXO_FILTERS/{name:?} 字节不得变化");
        }
        eprintln!("== 红线校验通过：卡上 ph_menu + DXO_FILTERS 字节零变化 ==");
    }

    #[test]
    fn confirmed_write_keeps_existing_package_even_when_new_item_is_first() {
        for first in [false, true] {
            let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
            let db = tempfile::tempdir().unwrap();
            let mount = vol.path().to_string_lossy().to_string();
            let id = insert_local(db.path(), "GOL2");
            let key = format!("local:{id}");
            op_append_local(db.path(), &mount, id).unwrap();
            op_set_menu_name(db.path(), &mount, &key, "GOLW").unwrap();
            if first { op_move(db.path(), &mount, &key, -1).unwrap(); op_move(db.path(), &mount, &key, -1).unwrap(); }
            let old = std::fs::read(vol.path().join("DXO_FILTERS/lut_GOL2.bin")).unwrap();
            let plan = op_write_plan(db.path(), &mount).unwrap();
            let new_file = plan["items"].as_array().unwrap().iter().find(|i| i["action"] == "add").unwrap()["file"].as_str().unwrap().to_string();
            assert_ne!(new_file.to_uppercase(), "LUT_GOL2.BIN");
            let reply = apply_confirmed(db.path(), &mount, plan["digest"].as_str().unwrap(), false).unwrap();
            assert!(Path::new(reply["backup_dir"].as_str().unwrap()).join("journal.json").is_file());
            assert_eq!(old, std::fs::read(vol.path().join("DXO_FILTERS/lut_GOL2.bin")).unwrap());
        }
    }

    #[test]
    fn confirmation_rejects_changed_source_plan_and_reused_ticket() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let plan = op_write_plan(db.path(), &mount).unwrap();
        let session = WriteSession::default();
        let token = session.issue(&mount, plan["digest"].as_str().unwrap());
        let digest = session.consume(&mount, &token).unwrap();
        assert!(session.consume(&mount, &token).is_err());
        let old = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        op_move(db.path(), &mount, "card:00000011", 1).unwrap();
        assert!(apply_confirmed(db.path(), &mount, &digest, false).unwrap_err().contains("已变化"));
        assert_eq!(old, std::fs::read(vol.path().join("ph_menu.bin")).unwrap());
        let token = session.issue(&mount, &digest);
        assert!(session.consume("wrong-card", &token).is_err());
    }

    #[test]
    fn confirmed_touch_requires_explicit_checkbox_and_supports_normal_launch() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let old_touch = touch_fixture(&vol);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let old = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        let plan = op_write_plan(db.path(), &mount).unwrap();
        let digest = plan["digest"].as_str().unwrap();
        assert!(apply_confirmed(db.path(), &mount, digest, false).is_err());
        assert_eq!(old, std::fs::read(vol.path().join("ph_menu.bin")).unwrap());
        apply_confirmed(db.path(), &mount, digest, true).unwrap();
        assert_ne!(std::fs::read(vol.path().join("dxot/a.bin")).unwrap(), old_touch);
    }

    #[test]
    fn invalid_late_target_fails_before_any_card_file_is_written() {
        let vol = sim_volume_full(&["ORIG", "GOL2"], 1);
        let db = tempfile::tempdir().unwrap();
        let mount = vol.path().to_string_lossy().to_string();
        let id = insert_local(db.path(), "VELVIA");
        op_append_local(db.path(), &mount, id).unwrap();
        let old = std::fs::read(vol.path().join("ph_menu.bin")).unwrap();
        let prepared = prepare_write(db.path(), &mount).unwrap();
        std::fs::create_dir(vol.path().join("ph_last.bin")).unwrap();
        assert!(apply_prepared(db.path(), &mount, prepared, false).is_err());
        assert_eq!(old, std::fs::read(vol.path().join("ph_menu.bin")).unwrap());
        assert!(!vol.path().join("DXO_FILTERS/lut_VELVIA.bin").exists());
    }
}
