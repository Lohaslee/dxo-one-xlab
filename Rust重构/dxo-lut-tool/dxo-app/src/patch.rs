//! Lightning 完整包：内置校验、只读预检及一次确认安装。
use crate::cardops::WriteSession;
use lut_camera::batch::{self, BatchFile};
use lut_persist::Store;
use serde_json::{json, Value};
use std::{collections::HashSet, fs, path::Path, sync::atomic::Ordering};
use tauri::Manager;

include!(concat!(env!("OUT_DIR"), "/lightning_bundle.rs"));
const FIRMWARE: &str = "lightning-ecef5ef809";
const CREDITS: &str = include_str!("../../../../DEPENDENCIES.md");

fn validate_bundle() -> Result<(), String> {
    let paths: HashSet<_> = BUNDLE.iter().map(|(p, _, _)| *p).collect();
    if paths.len() != 19 || paths != batch::PATCH_PATHS.iter().copied().collect() {
        return Err("内置整包清单不完整，请重新安装 App".into());
    }
    for (path, sha, bytes) in BUNDLE {
        if Store::hash_bytes(bytes) != *sha { return Err(format!("内置文件校验失败: {path}")); }
    }
    Ok(())
}

#[derive(serde::Serialize)]
struct Disk {
    device: String,
    uuid: String,
    filesystem: String,
    #[serde(skip)]
    free: u64,
}

fn disk_from_plist(v: &plist::Value, mount: &str) -> Result<Disk, String> {
    let d = v.as_dictionary().ok_or("无法读取存储卡信息")?;
    let s = |k| d.get(k).and_then(plist::Value::as_string).unwrap_or("");
    let b = |k| d.get(k).and_then(plist::Value::as_boolean);
    if s("MountPoint") != mount || b("Internal") != Some(false) {
        return Err("请选择已挂载的外置相机存储卡".into());
    }
    if b("WritableVolume") != Some(true) || b("WritableMedia") == Some(false) {
        return Err("存储卡只读，请解锁后重试".into());
    }
    if !["msdos", "exfat"].contains(&s("FilesystemType").to_lowercase().as_str()) {
        return Err("仅支持 FAT32 / exFAT 相机卡".into());
    }
    if s("FilesystemType") == "msdos" && !s("FilesystemName").contains("FAT32") {
        return Err("FAT16 卡不受支持，请先在相机中格式化为 FAT32 / exFAT".into());
    }
    let free = d.get("VolumeFreeSpace").or_else(|| d.get("FreeSpace"))
        .and_then(plist::Value::as_unsigned_integer).ok_or("无法读取卡剩余空间")?;
    if s("DeviceIdentifier").is_empty() || s("VolumeUUID").is_empty() { return Err("存储卡身份信息不完整".into()); }
    Ok(Disk { device: s("DeviceIdentifier").into(), uuid: s("VolumeUUID").into(), filesystem: s("FilesystemType").into(), free })
}

fn mounted_disk(root: &Path) -> Result<Disk, String> {
    if root.parent() != Some(Path::new("/Volumes")) || root.canonicalize().map_err(|e| e.to_string())? != root {
        return Err("请选择 /Volumes 下的已挂载相机卡，不能选择目录或符号链接".into());
    }
    let o = std::process::Command::new("/usr/sbin/diskutil").args(["info", "-plist"]).arg(root)
        .output().map_err(|e| format!("读卡信息失败: {e}"))?;
    if !o.status.success() { return Err("存储卡已拔出或未挂载".into()); }
    let v = plist::Value::from_reader_xml(o.stdout.as_slice()).map_err(|e| format!("读卡信息无效: {e}"))?;
    disk_from_plist(&v, root.to_str().ok_or("挂载路径无效")?)
}

struct Prepared { reply: Value, files: Vec<BatchFile> }

fn prepare(root: &Path, disk: Disk) -> Result<Prepared, String> {
    validate_bundle()?;
    let dcim = root.join("DCIM");
    if !fs::symlink_metadata(&dcim).is_ok_and(|m| m.file_type().is_dir()) {
        return Err("未找到 DCIM 相机照片目录。请先在相机中格式化存储卡并拍照，再插卡安装".into());
    }
    batch::validate_patch_layout(root).map_err(|e| e.to_string())?;
    if disk.free < 5 * 1024 * 1024 { return Err("存储卡剩余空间不足 5 MB".into()); }
    // 不覆盖其它补丁的开机入口，也不把 USB-C 原生版误作 Lightning 升级。
    if batch::before_sha(&root.join("autoexec.ash")).map_err(|e| e.to_string())?.is_some() {
        let boot = fs::read_to_string(root.join("autoexec.ash")).map_err(|_| "无法识别已有启动补丁，请先检查并备份原补丁")?;
        if !boot.contains("c:\\ph_probe.ash") || !boot.contains("c:\\dxot\\m01.ash") {
            return Err("检测到其它或无法识别的启动补丁，不能混装。USB-C 原生机身不适用本套".into());
        }
    }
    let mut files = Vec::new();
    let mut rows = Vec::new();
    let mut snapshot = Vec::new();
    for (path, sha, bytes) in BUNDLE {
        let before = batch::before_sha(&root.join(path)).map_err(|e| e.to_string())?;
        let preserve = *path == "touch_prefs.bin" && before.is_some();
        let action = if preserve { "保留设置" } else if before.as_deref() == Some(*sha) { "已一致" }
            else if before.is_some() { "替换" } else { "新增" };
        rows.push(json!({"file": path, "bytes": bytes.len(), "sha": sha, "action": action}));
        snapshot.push(json!([path, before, sha]));
        if !preserve {
            files.push(BatchFile { path: (*path).into(), bytes: bytes.to_vec(), before_sha: before });
        }
    }
    // 最后发布启动入口；完整回读前不提示可拔卡。
    files.sort_by_key(|f| f.path == "autoexec.ash");
    let dirs: Vec<_> = ["DXO_FILTERS", "dxot", "PH_DIAG"].into_iter().filter(|d| !root.join(d).exists()).collect();
    let meta = fs::metadata(root).map_err(|e| e.to_string())?;
    #[cfg(unix)]
    let identity = { use std::os::unix::fs::MetadataExt; (meta.dev(), meta.ino()) };
    #[cfg(not(unix))]
    let identity = (meta.len(), 0u64);
    let current = lut_camera::probe(root).ok().and_then(|p| p.build_tag);
    let digest = Store::hash_bytes(&serde_json::to_vec(&json!(["lightning-full-72", identity, disk, snapshot, dirs])).map_err(|e| e.to_string())?);
    let reply = json!({"build": "72", "firmware": "3.2.0 (ecef5ef809)", "mount": root.to_string_lossy(),
        "device": disk.device, "filesystem": disk.filesystem, "free_mb": disk.free / 1024 / 1024,
        "current_build": current, "files": rows, "write_count": files.len(), "create_dirs": dirs,
        "preserve_prefs": root.join("touch_prefs.bin").exists(), "digest": digest});
    Ok(Prepared { reply, files })
}

fn install(db: &Path, root: &Path, p: Prepared, fault: &dyn lut_camera::FaultInjector) -> Result<Value, String> {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|e| e.to_string())?.as_nanos();
    let backup = db.join("card_backups").join(format!("patch72-{nanos}-{}", std::process::id()));
    batch::write_patch_batch(root, &p.files, &backup, fault)?;
    let warning = crate::cardops::reset_plan(db, &root.to_string_lossy()).err();
    Ok(json!({"ok": true, "build": "72", "written": p.files.iter().map(|f| &f.path).collect::<Vec<_>>(),
        "backup_dir": backup.to_string_lossy(), "warning": warning}))
}

fn prepare_confirmed(root: &Path, disk: Disk, digest: &str) -> Result<Prepared, String> {
    let p = prepare(root, disk)?;
    if p.reply["digest"].as_str() != Some(digest) { return Err("未写入：存储卡或目标文件已变化，请重新预检确认".into()); }
    Ok(p)
}

#[tauri::command]
pub fn patch_info() -> Result<Value, String> {
    validate_bundle()?;
    let filters: Vec<_> = BUNDLE.iter().filter_map(|(p, _, _)| p.strip_prefix("DXO_FILTERS/lut_").and_then(|p| p.strip_suffix(".bin"))).collect();
    Ok(json!({"build": "72", "date": "2026-11-17", "firmware": "3.2.0 (ecef5ef809)", "count": BUNDLE.len(), "credits": CREDITS, "filters": filters}))
}

#[tauri::command]
pub async fn patch_plan(app: tauri::AppHandle, mount: String) -> Result<Value, String> {
    if app.state::<WriteSession>().busy.load(Ordering::SeqCst) { return Err("正在写卡，请稍候".into()); }
    let m = mount.clone();
    let mut p = tauri::async_runtime::spawn_blocking(move || {
        let root = Path::new(&m); prepare(root, mounted_disk(root)?)
    }).await.map_err(|e| format!("预检线程失败: {e}"))??.reply;
    let digest = p["digest"].as_str().ok_or("预检指纹缺失")?;
    p["confirmation_token"] = json!(app.state::<WriteSession>().issue(&mount, digest));
    Ok(p)
}

#[tauri::command]
pub async fn patch_install(app: tauri::AppHandle, mount: String, confirmation_token: String, firmware: String) -> Result<Value, String> {
    let state = app.state::<WriteSession>();
    if state.busy.compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst).is_err() { return Err("正在写卡，请勿重复提交".into()); }
    let result = async {
        if firmware != FIRMWARE { return Err("请先确认 Lightning 机身及 ecef5ef809 固件".into()); }
        if !app.state::<crate::Session>().pending.lock().unwrap().is_empty() { return Err("请先保存未保存参数".into()); }
        let digest = state.consume(&mount, &confirmation_token)?;
        tauri::async_runtime::spawn_blocking(move || {
            let root = Path::new(&mount);
            let p = prepare_confirmed(root, mounted_disk(root)?, &digest)?;
            let mut res = install(&Store::default_db_dir(), root, p, &lut_camera::NoFault)?;
            if !std::process::Command::new("/bin/sync").status().is_ok_and(|s| s.success()) {
                res["warning"] = json!("文件已回读一致，但系统刷盘未确认；请通过访达安全弹出后再拔卡");
            }
            Ok(res)
        }).await.map_err(|e| format!("安装线程异常，请保留卡并检查本地备份: {e}"))?
    }.await;
    state.busy.store(false, Ordering::SeqCst);
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;
    fn disk() -> Disk { Disk { device: "disk-test".into(), uuid: "test-volume".into(), filesystem: "exfat".into(), free: 10 * 1024 * 1024 } }
    fn card() -> tempfile::TempDir {
        let v = tempfile::tempdir().unwrap();
        fs::create_dir(v.path().join("DCIM")).unwrap();
        fs::write(v.path().join("DCIM/photo.jpg"), b"precious-photo").unwrap();
        v
    }
    fn snapshot(root: &Path) -> std::collections::BTreeMap<String, String> {
        fn walk(root: &Path, dir: &Path, out: &mut std::collections::BTreeMap<String, String>) {
            for e in fs::read_dir(dir).unwrap() {
                let p = e.unwrap().path(); let rel = p.strip_prefix(root).unwrap().to_string_lossy().to_string();
                if p.is_dir() { out.insert(format!("{rel}/"), "dir".into()); walk(root, &p, out); }
                else { out.insert(rel, Store::hash_bytes(&fs::read(p).unwrap())); }
            }
        }
        let mut out = std::collections::BTreeMap::new(); walk(root, root, &mut out); out
    }
    #[test]
    fn full_install_and_upgrade_preserve_user_content_and_have_backups() {
        validate_bundle().unwrap();
        let v = card(); let db = tempfile::tempdir().unwrap();
        let before = snapshot(v.path());
        let fresh = prepare(v.path(), disk()).unwrap();
        assert_eq!(snapshot(v.path()), before); // 只读预检不创建目录
        assert_eq!(fresh.files.len(), 19);
        assert_eq!(fresh.files.last().unwrap().path, "autoexec.ash");
        let result = install(db.path(), v.path(), fresh, &lut_camera::NoFault).unwrap();
        assert!(Path::new(result["backup_dir"].as_str().unwrap()).join("journal.json").exists());
        for (p, sha, _) in BUNDLE { assert_eq!(Store::hash_bytes(&fs::read(v.path().join(p)).unwrap()), *sha); }
        let profile = lut_camera::probe(v.path()).unwrap();
        assert_eq!(profile.build_tag.as_deref(), Some("72")); assert_eq!(profile.n_slots, 11);
        assert!(v.path().join("PH_DIAG").is_dir());
        fs::write(v.path().join("touch_prefs.bin"), b"user-preferences").unwrap();
        fs::write(v.path().join("ph_last.bin"), b"last-filter-memory").unwrap();
        fs::write(v.path().join("DXO_FILTERS/lut_custom.bin"), b"custom-filter").unwrap();
        fs::write(v.path().join("DXO_FILTERS/lut_gold.bin"), b"custom-same-name").unwrap();
        let p = prepare(v.path(), disk()).unwrap(); assert_eq!(p.files.len(), 18);
        let result = install(db.path(), v.path(), p, &lut_camera::NoFault).unwrap();
        assert_eq!(fs::read(v.path().join("touch_prefs.bin")).unwrap(), b"user-preferences");
        assert_eq!(fs::read(v.path().join("ph_last.bin")).unwrap(), b"last-filter-memory");
        assert_eq!(fs::read(v.path().join("DXO_FILTERS/lut_custom.bin")).unwrap(), b"custom-filter");
        assert_eq!(fs::read(v.path().join("DCIM/photo.jpg")).unwrap(), b"precious-photo");
        let journal = fs::read_to_string(Path::new(result["backup_dir"].as_str().unwrap()).join("journal.json")).unwrap();
        assert!(journal.contains(&Store::hash_bytes(b"custom-same-name")));
        let before = snapshot(v.path());
        install(db.path(), v.path(), prepare(v.path(), disk()).unwrap(), &lut_camera::NoFault).unwrap();
        assert_eq!(before, snapshot(v.path())); // 重复安装字节不变
    }
    struct FailNth { count: AtomicUsize, n: usize }
    impl lut_camera::FaultInjector for FailNth {
        fn check(&self, p: lut_camera::FaultPoint) -> Result<(), lut_camera::CameraError> {
            if p == lut_camera::FaultPoint::Readback && self.count.fetch_add(1, Ordering::SeqCst) == self.n {
                Err(lut_camera::CameraError::InjectedFault(p))
            } else { Ok(()) }
        }
    }
    #[test]
    fn failure_at_each_file_restores_fresh_and_upgrade_cards() {
        for upgraded in [false, true] {
            for n in 0..19 {
                let v = card(); let db = tempfile::tempdir().unwrap();
                if upgraded {
                    install(db.path(), v.path(), prepare(v.path(), disk()).unwrap(), &lut_camera::NoFault).unwrap();
                    fs::write(v.path().join("ph_menu.bin"), b"old-custom-menu").unwrap();
                    fs::write(v.path().join("DXO_FILTERS/lut_gold.bin"), b"old-gold").unwrap();
                }
                let before = snapshot(v.path()); let p = prepare(v.path(), disk()).unwrap();
                if n >= p.files.len() { continue; }
                let err = install(db.path(), v.path(), p, &FailNth { count: AtomicUsize::new(0), n }).unwrap_err();
                assert!(err.contains("全部回滚"), "{err}"); assert_eq!(before, snapshot(v.path()), "file {n}");
            }
        }
    }
    #[test]
    fn stale_confirmation_wrong_card_and_unknown_boot_are_read_only() {
        let a = card(); let b = card();
        let p = prepare(a.path(), disk()).unwrap(); let digest = p.reply["digest"].as_str().unwrap();
        assert!(prepare_confirmed(b.path(), disk(), digest).is_err());
        fs::write(a.path().join("ph_menu.bin"), b"changed-after-confirmation").unwrap();
        assert!(prepare_confirmed(a.path(), disk(), digest).is_err());
        fs::write(a.path().join("autoexec.ash"), b"unknown-or-usbc-patch").unwrap();
        let before = snapshot(a.path()); assert!(prepare(a.path(), disk()).is_err());
        assert_eq!(before, snapshot(a.path()));
        let blank = tempfile::tempdir().unwrap(); assert!(prepare(blank.path(), disk()).is_err());
        let mut small = disk(); small.free = 1; assert!(prepare(b.path(), small).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn symlinks_protected_files_and_backup_on_card_are_rejected() {
        for name in ["DXO_FILTERS", "dxot", "PH_DIAG", "touch_prefs.bin", "ph_menu.bin"] {
            let v = card(); let outside = tempfile::tempdir().unwrap();
            std::os::unix::fs::symlink(outside.path(), v.path().join(name)).unwrap();
            assert!(prepare(v.path(), disk()).is_err(), "{name}");
        }
        let v = card(); let db = tempfile::tempdir().unwrap();
        for name in ["DCIM/photo.jpg", "ph_last.bin", "../escape", "dxot/unknown.bin"] {
            let f = BatchFile { path: name.into(), bytes: vec![1], before_sha: None };
            assert!(batch::write_patch_batch(v.path(), &[f], &db.path().join("backup"), &lut_camera::NoFault).is_err());
        }
        let p = prepare(v.path(), disk()).unwrap(); let before = snapshot(v.path());
        assert!(install(v.path(), v.path(), p, &lut_camera::NoFault).is_err());
        assert_eq!(before, snapshot(v.path()));
        fs::write(v.path().join("touch_prefs.bin"), b"prefs").unwrap();
        let f = BatchFile { path: "touch_prefs.bin".into(), bytes: vec![1], before_sha: batch::before_sha(&v.path().join("touch_prefs.bin")).unwrap() };
        assert!(batch::write_patch_batch(v.path(), &[f], &db.path().join("backup"), &lut_camera::NoFault).is_err());
    }
    #[test]
    fn disk_preflight_rejects_wrong_mount_fs_internal_and_readonly() {
        let mut v: plist::Value = plist::from_bytes(br#"<?xml version="1.0"?><plist version="1.0"><dict>
            <key>Internal</key><false/><key>WritableVolume</key><true/><key>WritableMedia</key><true/>
            <key>MountPoint</key><string>/Volumes/CARD</string><key>FilesystemType</key><string>exfat</string>
            <key>DeviceIdentifier</key><string>disk99s1</string><key>VolumeUUID</key><string>test</string>
            <key>VolumeFreeSpace</key><integer>9999999</integer></dict></plist>"#).unwrap();
        assert!(disk_from_plist(&v, "/Volumes/CARD").is_ok());
        assert!(disk_from_plist(&v, "/Volumes/OTHER").is_err());
        for (key, bad) in [("Internal", plist::Value::Boolean(true)), ("WritableVolume", plist::Value::Boolean(false)),
            ("FilesystemType", plist::Value::String("apfs".into())), ("WritableMedia", plist::Value::Boolean(false))] {
            let old = v.as_dictionary_mut().unwrap().insert(key.into(), bad).unwrap();
            assert!(disk_from_plist(&v, "/Volumes/CARD").is_err());
            v.as_dictionary_mut().unwrap().insert(key.into(), old);
        }
        v.as_dictionary_mut().unwrap().insert("FilesystemType".into(), plist::Value::String("msdos".into()));
        v.as_dictionary_mut().unwrap().insert("FilesystemName".into(), plist::Value::String("MS-DOS FAT32".into()));
        assert!(disk_from_plist(&v, "/Volumes/CARD").is_ok());
        v.as_dictionary_mut().unwrap().insert("FilesystemName".into(), plist::Value::String("MS-DOS FAT16".into()));
        assert!(disk_from_plist(&v, "/Volumes/CARD").is_err());
    }
}
