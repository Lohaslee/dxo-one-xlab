//! 已确认写卡操作的整批提交。授权由 App 的一次确认票据控制，不修改进程环境。
use crate::{CameraError, FaultInjector, FaultPoint};
use lut_persist::Store;
use serde::{Deserialize, Serialize};
use std::{collections::HashSet, fs, io::Write, path::{Path, PathBuf}};

pub struct BatchFile {
    pub path: String,
    pub bytes: Vec<u8>,
    pub before_sha: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct BackupEntry {
    path: String,
    backup: Option<String>,
    before_sha: Option<String>,
    after_sha: String,
}

fn write_synced(path: &Path, bytes: &[u8]) -> Result<(), CameraError> {
    let mut f = fs::File::create(path)?;
    f.write_all(bytes)?;
    f.sync_all()?;
    Ok(())
}

/// Lightning 完整包的固定目标清单，独立于滤镜编辑的写入范围。
pub const PATCH_PATHS: &[&str] = &[
    "autoexec.ash", "ph_probe.ash", "ph_menu.bin", "touch_prefs.bin",
    "dxot/a.bin", "dxot/b.bin", "dxot/m01.ash", "dxot/m02.ash",
    "DXO_FILTERS/lut_orig.bin", "DXO_FILTERS/lut_vel.bin", "DXO_FILTERS/lut_gold.bin",
    "DXO_FILTERS/lut_port.bin", "DXO_FILTERS/lut_hp5.bin", "DXO_FILTERS/lut_posi.bin",
    "DXO_FILTERS/lut_hibw.bin", "DXO_FILTERS/lut_fuyu.bin", "DXO_FILTERS/lut_kyus.bin",
    "DXO_FILTERS/lut_sato.bin", "DXO_FILTERS/lut_insta.bin",
];
const PATCH_DIRS: &[&str] = &["DXO_FILTERS", "dxot", "PH_DIAG"];

/// 安装只开放固定整包路径；照片、开机记忆及未知文件永远不在白名单内。
pub fn validate_patch_layout(root: &Path) -> Result<(), CameraError> {
    let canonical = root.canonicalize()?;
    let root = canonical.as_path();
    for dir in PATCH_DIRS {
        match fs::symlink_metadata(root.join(dir)) {
            Ok(m) if !m.file_type().is_dir() => return Err(CameraError::BadInput(format!("目录无效: {dir}"))),
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
            _ => {}
        }
    }
    for rel in PATCH_PATHS { target(root, rel, false, true)?; }
    Ok(())
}

fn target(root: &Path, rel: &str, include_touch: bool, patch: bool) -> Result<PathBuf, CameraError> {
    let package = rel.strip_prefix("DXO_FILTERS/").is_some_and(|f| {
        f.ends_with(".bin") && !f.is_empty()
            && f.chars().all(|c| c.is_ascii_alphanumeric() || "_.-".contains(c))
    });
    let allowed = if patch { PATCH_PATHS.contains(&rel) } else {
        package || rel == "ph_menu.bin" || rel == "ph_last.bin" || (include_touch && rel == "dxot/a.bin")
    };
    if !allowed {
        return Err(CameraError::ProtectedPath(rel.into()));
    }
    let full = root.join(rel);
    let parent = full.parent().ok_or_else(|| CameraError::BadInput(rel.into()))?;
    // 不沿符号链接写到卡外；整包仅允许稍后创建固定父目录。
    let missing_patch_dir = patch && PATCH_DIRS.iter().any(|d| parent == root.join(d))
        && matches!(fs::symlink_metadata(parent), Err(e) if e.kind() == std::io::ErrorKind::NotFound);
    if !missing_patch_dir && (parent.canonicalize()? != parent || !parent.is_dir()) {
        return Err(CameraError::BadInput(format!("目标目录无效: {rel}")));
    }
    match fs::symlink_metadata(&full) {
        Ok(m) if !m.file_type().is_file() => return Err(CameraError::BadInput(format!("目标不是普通文件: {rel}"))),
        Err(e) if e.kind() != std::io::ErrorKind::NotFound => return Err(e.into()),
        _ => {}
    }
    Ok(full)
}

pub fn before_sha(path: &Path) -> Result<Option<String>, CameraError> {
    match fs::read(path) {
        Ok(b) => Ok(Some(Store::hash_bytes(&b))),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// 全量预检及本地持久备份完成后才写卡。任何文件失败都逆序恢复本批已尝试文件。
/// 备份和 journal 留在本地，拔卡或恢复失败后仍可据此恢复；不宣称断电时具备多文件原子性。
pub fn write_batch(
    root: &Path,
    files: &[BatchFile],
    backup_dir: &Path,
    include_touch: bool,
    fault: &dyn FaultInjector,
) -> Result<(), String> {
    write_batch_inner(root, files, backup_dir, include_touch, false, fault)
}

pub fn write_patch_batch(root: &Path, files: &[BatchFile], backup_dir: &Path, fault: &dyn FaultInjector) -> Result<(), String> {
    write_batch_inner(root, files, backup_dir, false, true, fault)
}

fn write_batch_inner(root: &Path, files: &[BatchFile], backup_dir: &Path, include_touch: bool, patch: bool, fault: &dyn FaultInjector) -> Result<(), String> {
    let root = root.canonicalize().map_err(|e| format!("未写入：存储卡不可用: {e}"))?;
    let ancestor = backup_dir.ancestors().find(|p| p.exists()).ok_or("本地备份目录无效")?;
    if ancestor.canonicalize().map_err(|e| e.to_string())?.starts_with(&root) {
        return Err("未写入：备份必须保存在存储卡以外的本机目录".into());
    }
    if patch { validate_patch_layout(&root).map_err(|e| format!("未写入：{e}"))?; }
    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    let mut originals = Vec::new();
    for f in files {
        if !seen.insert(f.path.to_uppercase()) { return Err("未写入：目标路径重复".into()); }
        if patch && f.path == "touch_prefs.bin" && f.before_sha.is_some() {
            return Err("未写入：已有触屏设置禁止覆盖".into());
        }
        let p = target(&root, &f.path, include_touch, patch).map_err(|e| format!("未写入：{e}"))?;
        let old = match fs::read(&p) {
            Ok(b) => Some(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
            Err(e) => return Err(format!("未写入：读取 {} 失败: {e}", f.path)),
        };
        if old.as_ref().map(|b| Store::hash_bytes(b)) != f.before_sha {
            return Err(format!("未写入：{} 已变化，请重新预演确认", f.path));
        }
        originals.push(old);
        targets.push(p);
    }
    let prepare = (|| -> Result<(), CameraError> {
        fs::create_dir_all(backup_dir.parent().ok_or_else(|| CameraError::BadInput("备份目录无效".into()))?)?;
        fs::create_dir(backup_dir)?;
        let mut entries = Vec::new();
        for (i, (f, old)) in files.iter().zip(&originals).enumerate() {
            let backup = if let Some(bytes) = old {
                let name = format!("{i}.bin");
                write_synced(&backup_dir.join(&name), bytes)?;
                if fs::read(backup_dir.join(&name))? != *bytes { return Err(CameraError::ContentMismatch); }
                Some(name)
            } else { None };
            entries.push(BackupEntry { path: f.path.clone(), backup, before_sha: f.before_sha.clone(), after_sha: Store::hash_bytes(&f.bytes) });
        }
        let journal = serde_json::to_vec_pretty(&entries).map_err(|e| CameraError::BadInput(e.to_string()))?;
        write_synced(&backup_dir.join("journal.json"), &journal)?;
        if patch {
            let dirs: Vec<_> = PATCH_DIRS.iter().filter(|d| !root.join(d).exists()).collect();
            write_synced(&backup_dir.join("created-dirs.json"), &serde_json::to_vec(&dirs).map_err(|e| CameraError::BadInput(e.to_string()))?)?;
        }
        write_synced(&backup_dir.join("status"), b"prepared")?;
        crate::fsync_parent(&backup_dir.join("status"));
        Ok(())
    })();
    prepare.map_err(|e| format!("未写入：本地备份失败: {e}"))?;
    let mut created_dirs = Vec::new();
    if patch {
        for dir in PATCH_DIRS {
            let p = root.join(dir);
            if !p.exists() {
                if let Err(e) = fs::create_dir(&p) {
                    for d in created_dirs.iter().rev() { let _ = fs::remove_dir(d); }
                    let _ = write_synced(&backup_dir.join("status"), b"rolled_back");
                    return Err(format!("创建 {dir} 失败: {e}；未写入补丁文件。备份：{}", backup_dir.display()));
                }
                created_dirs.push(p);
                crate::fsync_parent(&root.join(dir));
            }
        }
    }
    for (i, f) in files.iter().enumerate() {
        if let Err(e) = crate::replace_staged(&targets[i], &f.bytes, fault) {
            let mut failures = Vec::new();
            for j in (0..=i).rev() {
                let restore = (|| -> Result<(), CameraError> {
                    fault.check(FaultPoint::Restore)?;
                    if originals[j].is_some() {
                        crate::restore_from(&backup_dir.join(format!("{j}.bin")), &targets[j])?;
                    } else {
                        match fs::remove_file(&targets[j]) {
                            Ok(()) => {},
                            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {},
                            Err(e) => return Err(e.into()),
                        }
                    }
                    crate::fsync_parent(&targets[j]);
                    Ok(())
                })();
                if let Err(re) = restore { failures.push(format!("{}: {re}", files[j].path)); }
            }
            for dir in created_dirs.iter().rev() {
                if let Err(e) = fs::remove_dir(dir) { failures.push(format!("{}: {e}", dir.display())); }
            }
            let status = if failures.is_empty() { "rolled_back" } else { "recovery_required" };
            let _ = write_synced(&backup_dir.join("status"), status.as_bytes());
            return Err(if failures.is_empty() {
                format!("写入 {} 失败: {e}；本批目标已全部回滚。备份：{}", f.path, backup_dir.display())
            } else {
                format!("写入 {} 失败: {e}；回滚未完成，卡可能部分更新：{}。保留存储卡并检查本地备份：{}", f.path, failures.join("；"), backup_dir.display())
            });
        }
    }
    // 目标已全部校验成功，状态记录失败不能误报为零写入。
    let _ = write_synced(&backup_dir.join("status"), b"committed");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct FailNth { count: AtomicUsize, n: usize, restore: bool }
    impl FaultInjector for FailNth {
        fn check(&self, p: FaultPoint) -> Result<(), CameraError> {
            if (p == FaultPoint::Readback && self.count.fetch_add(1, Ordering::SeqCst) == self.n)
                || (p == FaultPoint::Restore && self.restore) {
                Err(CameraError::InjectedFault(p))
            } else { Ok(()) }
        }
    }
    #[test]
    fn batch_failure_restores_old_files_and_removes_new_files() {
        for n in 0..4 {
            let vol = tempfile::tempdir().unwrap(); let backup = tempfile::tempdir().unwrap();
            fs::create_dir(vol.path().join("DXO_FILTERS")).unwrap();
            fs::create_dir(vol.path().join("dxot")).unwrap();
            fs::write(vol.path().join("ph_menu.bin"), b"old-menu").unwrap();
            fs::write(vol.path().join("dxot/a.bin"), b"old-touch").unwrap();
            let paths = ["DXO_FILTERS/lut_NEW.bin", "ph_menu.bin", "ph_last.bin", "dxot/a.bin"];
            let files: Vec<_> = paths.iter().map(|p| BatchFile { path: (*p).into(), bytes: b"new".to_vec(), before_sha: before_sha(&vol.path().join(p)).unwrap() }).collect();
            let fault = FailNth { count: AtomicUsize::new(0), n, restore: false };
            let err = write_batch(vol.path(), &files, &backup.path().join("tx"), true, &fault).unwrap_err();
            assert!(err.contains("全部回滚"), "{err}");
            assert_eq!(fs::read(vol.path().join("ph_menu.bin")).unwrap(), b"old-menu");
            assert_eq!(fs::read(vol.path().join("dxot/a.bin")).unwrap(), b"old-touch");
            assert!(!vol.path().join(paths[0]).exists()); assert!(!vol.path().join(paths[2]).exists());
        }
    }
    #[test]
    fn batch_preflight_and_recovery_failure_are_explicit() {
        let vol = tempfile::tempdir().unwrap(); let backup = tempfile::tempdir().unwrap();
        fs::write(vol.path().join("ph_menu.bin"), b"old").unwrap();
        let mut files = vec![BatchFile { path: "ph_menu.bin".into(), bytes: b"new".to_vec(), before_sha: Some("stale".into()) }];
        assert!(write_batch(vol.path(), &files, &backup.path().join("tx"), false, &crate::NoFault).unwrap_err().contains("已变化"));
        assert!(!backup.path().join("tx").exists());
        files[0].before_sha = before_sha(&vol.path().join("ph_menu.bin")).unwrap();
        let fault = FailNth { count: AtomicUsize::new(0), n: 0, restore: true };
        let err = write_batch(vol.path(), &files, &backup.path().join("tx"), false, &fault).unwrap_err();
        assert!(err.contains("回滚未完成"));
        assert_eq!(fs::read(backup.path().join("tx/0.bin")).unwrap(), b"old");
    }
}
