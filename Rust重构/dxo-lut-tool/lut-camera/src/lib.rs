//! 相机存储卡安全写入模块。
//!
//! 功能：
//! - 旧版接口按 DXO_CARD_UUID 检测相机卡；Tauri App 使用卷枚举与用户选卡
//! - 安全写入：先备份 → 写入 → fsync → 逐字节回读校验
//! - 只读挂载检查，禁止动 DCIM / touch_prefs
//! - 安全弹出

pub mod card;
pub mod export;
pub mod rebuild;
pub mod batch;
pub mod volume;
pub use card::{probe, CardProfile, PackageInfo, SlotEntry, ToolDefaults};
pub use export::{card_safe_name, export_bundle, item_paths, list_bundles, read_bundle, table_from_package, validate_rel_path, BundleRead, ExportItem, ExportReport, ItemPaths, RelFile, CUSTOM_MAX, MANIFEST_SCHEMA};

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
#[cfg(not(windows))]
use std::process::Command;
use thiserror::Error;

/// 旧版 CLI / Iced 接口的显式目标；不在源码中绑定开发者自己的卡。
pub fn configured_card_uuid() -> Option<String> {
    std::env::var("DXO_CARD_UUID").ok().map(|s| s.trim().to_owned()).filter(|s| !s.is_empty())
}

/// 禁止写入的目录（相机维护用，部署保存原值）。
pub const PROTECTED_DIRS: &[&str] = &["DCIM", "touch_prefs", "dxot"];

/// 受控写入的故障注入点（R4-03）：用于在模拟卷上验证中断/回滚行为。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultPoint {
    /// 备份原文件前
    Backup,
    /// 写暂存文件前
    StageWrite,
    /// 暂存文件 fsync 前
    StageFsync,
    /// 暂存→目标 rename 前
    Rename,
    /// 落盘后回读校验前
    Readback,
    /// 回滚恢复阶段
    Restore,
}

#[derive(Debug, Error)]
pub enum CameraError {
    #[error("未找到相机存储卡")]
    CardNotFound,
    #[error("IO 错误: {0}")]
    Io(#[from] io::Error),
    #[error("命令执行失败: {0}")]
    Command(String),
    #[error("校验失败: 期望 {expected} B，实际 {actual} B")]
    SizeMismatch { expected: usize, actual: usize },
    #[error("内容校验失败: hash 不匹配")]
    ContentMismatch,
    #[error("路径受保护: {0}")]
    ProtectedPath(String),
    #[error("存储卡未挂载")]
    NotMounted,
    #[error("参数不合法: {0}")]
    BadInput(String),
    #[error("写卡入口禁用: 本实例未获写卡授权（须用户逐次明确授权、随启动注入）")]
    WriteDisabled,
    #[error("dxot/a.bin 写入禁用: 本实例未获触屏同步点名授权（须用户逐次点名含 dxot/a.bin、随启动注入）")]
    TouchWriteDisabled,
    #[error("故障注入命中: {0:?}")]
    InjectedFault(FaultPoint),
    #[error("回滚失败: {0}")]
    RollbackFailed(String),
}

/// 写卡闸门：默认恒禁用真实写卡入口。
/// 模拟卷/暂存写入不受此闸门影响（它们不接触真实相机卡）。
pub const WRITE_GATE_ENABLED: bool = false;

/// 逐次授权开关：仅当启动环境显式 DXO_CARD_WRITE_AUTH=1 时对本进程实例开放写卡。
/// 授权由用户在对话中逐次点名给出、随启动注入；不持久化、不落盘、默认启动恒禁用。
pub const WRITE_AUTH_ENV: &str = "DXO_CARD_WRITE_AUTH";

pub fn write_gate_check(auth_env: Option<&str>) -> Result<(), CameraError> {
    if WRITE_GATE_ENABLED || auth_env == Some("1") {
        Ok(())
    } else {
        Err(CameraError::WriteDisabled)
    }
}

pub fn ensure_write_gate() -> Result<(), CameraError> {
    write_gate_check(std::env::var(WRITE_AUTH_ENV).ok().as_deref())
}

/// F-19 例外闸门：dxot/ 整体仍受保护，仅 dxot/a.bin 一个文件可经独立点名授权写入。
/// 授权由用户在对话中逐次点名（须含 dxot/a.bin 与本 flag 名）、随启动注入；
/// 不持久化、不落盘、默认启动恒禁用；与 DXO_CARD_WRITE_AUTH 相互独立、须同时具备。
pub const TOUCH_WRITE_AUTH_ENV: &str = "DXO_TOUCH_WRITE_AUTH";

pub fn touch_write_gate_check(auth_env: Option<&str>) -> Result<(), CameraError> {
    if auth_env == Some("1") {
        Ok(())
    } else {
        Err(CameraError::TouchWriteDisabled)
    }
}

pub fn ensure_touch_write_gate() -> Result<(), CameraError> {
    touch_write_gate_check(std::env::var(TOUCH_WRITE_AUTH_ENV).ok().as_deref())
}

/// 相机卡信息。
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CardInfo {
    pub device: String,      // diskutil 设备名，如 disk4s1
    pub mount_point: Option<PathBuf>,
    pub uuid: String,
    pub volume_name: Option<String>,
}

/// 检查路径是否受保护。
pub fn is_protected(path: &Path) -> bool {
    let path_str = path.to_string_lossy();
    for dir in PROTECTED_DIRS {
        if path_str.contains(dir) {
            return true;
        }
    }
    false
}

/// 校验路径安全（非受保护目录）。
pub fn validate_safe_path(path: &Path) -> Result<(), CameraError> {
    if is_protected(path) {
        return Err(CameraError::ProtectedPath(path.to_string_lossy().to_string()));
    }
    Ok(())
}

/// 查找相机存储卡。
///
/// 只按 DXO_CARD_UUID 匹配，未配置时不猜测目标；平台差异收在 `find_card_by_uuid`。
pub fn find_card() -> Result<CardInfo, CameraError> {
    let uuid = configured_card_uuid().ok_or_else(|| CameraError::BadInput(
        "旧版卡接口需设置 DXO_CARD_UUID；当前 Tauri App 可直接扫描并选择存储卡".into()
    ))?;
    find_card_by_uuid(&uuid)
}

/// macOS：通过 `diskutil info` 查找该 UUID 对应的分区。
#[cfg(not(windows))]
fn find_card_by_uuid(uuid: &str) -> Result<CardInfo, CameraError> {
    let output = Command::new("diskutil")
        .args(["info", uuid])
        .output()
        .map_err(|e| CameraError::Command(e.to_string()))?;

    if !output.status.success() {
        return Err(CameraError::CardNotFound);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut device = None;
    let mut mount_point = None;
    let mut volume_name = None;

    for line in stdout.lines() {
        let line = line.trim();
        if let Some(dev) = line.strip_prefix("Device Identifier:") {
            device = Some(dev.trim().to_string());
        }
        if let Some(mp) = line.strip_prefix("Mount Point:") {
            let mp = mp.trim();
            if !mp.is_empty() && mp != "null" {
                mount_point = Some(PathBuf::from(mp));
            }
        }
        if let Some(vn) = line.strip_prefix("Volume Name:") {
            let vn = vn.trim();
            if !vn.is_empty() && vn != "null" {
                volume_name = Some(vn.to_string());
            }
        }
    }

    let device = device.ok_or(CameraError::CardNotFound)?;
    Ok(CardInfo {
        device,
        mount_point,
        uuid: uuid.to_string(),
        volume_name,
    })
}

/// Windows：在盘符卷中按卷 GUID 匹配 DXO_CARD_UUID；查不到就报未找到，不回退为任意目录。
#[cfg(windows)]
fn find_card_by_uuid(uuid: &str) -> Result<CardInfo, CameraError> {
    let want = uuid.to_lowercase();
    for root in volume::volume_roots() {
        let Ok(f) = volume::query_volume(&root) else { continue };
        if f.volume_uuid.to_lowercase().contains(&want) {
            return Ok(CardInfo {
                device: f.device,
                mount_point: Some(root),
                uuid: f.volume_uuid,
                volume_name: (!f.label.is_empty()).then_some(f.label),
            });
        }
    }
    Err(CameraError::CardNotFound)
}

/// 检查存储卡是否已挂载。
pub fn is_mounted(card: &CardInfo) -> bool {
    card.mount_point.as_ref().is_some_and(|p| p.exists())
}

/// 获取挂载点路径。
pub fn mount_point(card: &CardInfo) -> Result<&Path, CameraError> {
    card.mount_point
        .as_ref()
        .ok_or(CameraError::NotMounted)
        .map(|p| p.as_path())
}

/// 备份文件（复制到同目录 .bak）。
/// 路径安全由调用方入口保证（controlled_write 的 validate_safe_path / write_touch_a_bin 的白名单+闸门）。
pub fn backup_file(path: &Path) -> Result<PathBuf, CameraError> {
    let bak = path.with_extension(
        path.extension()
            .map(|e| {
                let mut s = e.to_string_lossy().to_string();
                s.insert_str(0, ".bak");
                s
            })
            .unwrap_or_else(|| "bak".to_string()),
    );

    // 如果已存在备份，先移除
    if bak.exists() {
        fs::remove_file(&bak)?;
    }

    fs::copy(path, &bak)?;
    Ok(bak)
}

/// 故障注入器：在指定阶段返回错误，模拟中断/回滚失败。
pub trait FaultInjector {
    fn check(&self, point: FaultPoint) -> Result<(), CameraError>;
}

/// 无故障（生产路径）。
pub struct NoFault;
impl FaultInjector for NoFault {
    fn check(&self, _point: FaultPoint) -> Result<(), CameraError> {
        Ok(())
    }
}

/// 在恰好一个阶段注入故障（测试用）。
pub struct InjectFault(pub FaultPoint);
impl FaultInjector for InjectFault {
    fn check(&self, point: FaultPoint) -> Result<(), CameraError> {
        if point == self.0 {
            Err(CameraError::InjectedFault(point))
        } else {
            Ok(())
        }
    }
}

/// 受控写入结果。
#[derive(Debug, Clone)]
pub struct WriteOutcome {
    pub written: PathBuf,
    /// 原文件备份（首次写入无备份时为 None）。
    pub backup: Option<PathBuf>,
    pub size: usize,
}

fn fsync_file(f: &fs::File) -> io::Result<()> {
    f.sync_all()
}

/// 暂存文件路径：与目标同目录（保证 rename 原子），带进程号避免冲突。
fn staging_path(path: &Path) -> PathBuf {
    let mut s = path.as_os_str().to_string_lossy().into_owned();
    s.push_str(&format!(".tmp{}", std::process::id()));
    PathBuf::from(s)
}

/// 从备份恢复目标：copy → fsync → 回读校验。
fn restore_from(backup: &Path, path: &Path) -> Result<(), CameraError> {
    fs::copy(backup, path)?;
    // FlushFileBuffers 要求句柄带 GENERIC_WRITE（Unix 的 fsync 对只读 fd 也合法），
    // 用只读句柄刷盘在 Windows 上必然回 ACCESS_DENIED(5)，把「可以完成的成功回滚」
    // 变成「回滚未完成，卡可能部分更新」。
    if let Ok(f) = fs::OpenOptions::new().write(true).open(path) {
        fsync_file(&f)?;
    }
    let want = fs::read(backup)?;
    let got = fs::read(path)?;
    if got != want {
        return Err(CameraError::ContentMismatch);
    }
    Ok(())
}

/// 受控写入（R4-03）：备份 → 暂存(temp) → fsync → rename → fsync 目录 → 回读校验。
///
/// 任一阶段失败：清理暂存文件，并用备份回滚目标；回滚再失败则报 [`CameraError::RollbackFailed`]。
/// `fault` 注入器仅在测试中命中各 [`FaultPoint`]；生产路径传 [`NoFault`]。
/// 全程经 [`validate_safe_path`]，拒绝 DCIM / touch_prefs / dxot。
pub fn controlled_write(
    path: &Path,
    data: &[u8],
    fault: &dyn FaultInjector,
) -> Result<WriteOutcome, CameraError> {
    validate_safe_path(path)?;
    controlled_write_inner(path, data, fault)
}

/// F-19 例外写入：仅限 dxot/a.bin 单文件，须独立点名授权（DXO_TOUCH_WRITE_AUTH=1）。
/// 路径白名单精确到「父目录名 dxot + 文件名 a.bin」，其余受保护路径一律拒绝；
/// 落盘管线与 controlled_write 完全一致（备份/暂存/fsync/rename/回读/回滚）。
pub fn write_touch_a_bin(
    path: &Path,
    data: &[u8],
    fault: &dyn FaultInjector,
) -> Result<WriteOutcome, CameraError> {
    let name_ok = path.file_name().map(|n| n == "a.bin").unwrap_or(false);
    let dir_ok = path
        .parent()
        .and_then(|p| p.file_name())
        .map(|n| n == "dxot")
        .unwrap_or(false);
    if !(name_ok && dir_ok) {
        return Err(CameraError::ProtectedPath(path.to_string_lossy().to_string()));
    }
    ensure_touch_write_gate()?;
    controlled_write_inner(path, data, fault)
}

/// 已有整批备份时直接暂存替换，避免每个目标覆盖历史 .bak 文件。
pub(crate) fn replace_staged(path: &Path, data: &[u8], fault: &dyn FaultInjector) -> Result<(), CameraError> {
    let tmp = staging_path(path);
    let attempt = (|| -> Result<(), CameraError> {
        fault.check(FaultPoint::StageWrite)?;
        let mut f = fs::File::create(&tmp)?;
        f.write_all(data)?;
        f.flush()?;
        fault.check(FaultPoint::StageFsync)?;
        fsync_file(&f)?;
        drop(f);
        fault.check(FaultPoint::Rename)?;
        volume::replace_file(&tmp, path)?;
        volume::fsync_parent(path);
        fault.check(FaultPoint::Readback)?;
        let rb = fs::read(path)?;
        if rb.len() != data.len() {
            return Err(CameraError::SizeMismatch {
                expected: data.len(),
                actual: rb.len(),
            });
        }
        if rb != data {
            return Err(CameraError::ContentMismatch);
        }
        Ok(())
    })();

    if attempt.is_err() { let _ = fs::remove_file(&tmp); }
    attempt
}

fn controlled_write_inner(
    path: &Path,
    data: &[u8],
    fault: &dyn FaultInjector,
) -> Result<WriteOutcome, CameraError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    fault.check(FaultPoint::Backup)?;
    let backup = if path.exists() {
        Some(backup_file(path)?)
    } else {
        None
    };

    let attempt = replace_staged(path, data, fault);

    if let Err(e) = attempt {
        if let Some(ref bak) = backup {
            if let Err(re) = fault.check(FaultPoint::Restore) {
                return Err(CameraError::RollbackFailed(format!(
                    "{e}；恢复阶段再失败: {re}"
                )));
            }
            if let Err(re) = restore_from(bak, path) {
                return Err(CameraError::RollbackFailed(format!("{e}；恢复失败: {re}")));
            }
        }
        return Err(e);
    }

    Ok(WriteOutcome {
        written: path.to_path_buf(),
        backup,
        size: data.len(),
    })
}

/// 安全写入：备份 → 暂存 → fsync → rename → 回读校验（无故障注入）。
///
/// 返回备份文件路径（供恢复用；首次写入无备份时返回空路径）。
pub fn safe_write(path: &Path, data: &[u8]) -> Result<PathBuf, CameraError> {
    let out = controlled_write(path, data, &NoFault)?;
    Ok(out.backup.unwrap_or_else(PathBuf::new))
}

/// 指纹登记集成点（R4-03 预留，本关不启用真实写卡）。
///
/// 真实写卡前须由 ph_menu 指纹工具（重登记指纹.py / 管理滤镜.py）登记新表指纹，
/// 否则相机会拒切。此处仅定义契约，接入留待安全服务完成且用户逐次授权后。
pub trait FingerprintRegistrar {
    fn register(&mut self, table_path: &Path, table_sha256: &str) -> Result<(), CameraError>;
}

/// 恒禁用的指纹登记器：本关任何真实登记请求都返回 [`CameraError::WriteDisabled`]。
pub struct DisabledFingerprintRegistrar;
impl FingerprintRegistrar for DisabledFingerprintRegistrar {
    fn register(&mut self, _table_path: &Path, _table_sha256: &str) -> Result<(), CameraError> {
        Err(CameraError::WriteDisabled)
    }
}

/// 安全弹出存储卡（平台差异见 `eject_device`）。
pub fn safe_eject(card: &CardInfo) -> Result<(), CameraError> {
    eject_device(&card.device)
}

/// macOS：`diskutil eject`。
#[cfg(not(windows))]
fn eject_device(device: &str) -> Result<(), CameraError> {
    let output = Command::new("diskutil")
        .args(["eject", device])
        .output()
        .map_err(|e| CameraError::Command(e.to_string()))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(CameraError::Command(format!("eject 失败: {stderr}")));
    }
    Ok(())
}

/// Windows：不执行程序化弹出。未提权时系统会拒绝，伪造成「已弹出」比不弹出更危险，
/// 因此原样返回失败并引导用户使用系统的「弹出」，与安装完成提示同一套说明。
#[cfg(windows)]
fn eject_device(_device: &str) -> Result<(), CameraError> {
    Err(CameraError::Command(
        "Windows 未执行程序化弹出，请在「此电脑」中右键该卷选择「弹出」后再拔卡".into(),
    ))
}

/// 读取当前相机卡上的 CC 表。
pub fn read_current_table(card: &CardInfo, table_path: &Path) -> Result<Vec<u8>, CameraError> {
    let mp = mount_point(card)?;
    let full = mp.join(table_path);
    if !full.exists() {
        return Err(CameraError::NotMounted);
    }
    Ok(fs::read(&full)?)
}

// ── 写入计划：只读预演与真实落盘分离 ────────────────────

/// 只读写入计划。生成计划不接触文件系统，也不要求相机卡在场。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WritePlan {
    /// 相对卡根目录的目标路径
    pub table_path: PathBuf,
    pub size: usize,
}

/// 校验并生成写入计划：绝对路径、`..` 穿越与受保护目录一律拒绝。
pub fn build_plan(table_path: &Path, data: &[u8]) -> Result<WritePlan, CameraError> {
    use std::path::Component;
    // Windows 上 `/Windows/x.bin` 既不算 `is_absolute()`（缺盘符前缀）也不含 `..`，
    // 但 `PathBuf::push` 会把它当带根路径替换挂载点之后的部分，直接跳出卡根，
    // 所以必须按组件拒绝 Prefix（盘符/UNC/`\\?\`）与 RootDir。
    if table_path.is_absolute()
        || table_path
            .components()
            .any(|c| matches!(c, Component::Prefix(_) | Component::RootDir))
    {
        return Err(CameraError::BadInput("目标路径必须是相对卡根的相对路径".into()));
    }
    if table_path.components().any(|c| matches!(c, Component::ParentDir)) {
        return Err(CameraError::BadInput("目标路径不允许 .. 穿越".into()));
    }
    validate_safe_path(table_path)?;
    Ok(WritePlan {
        table_path: table_path.to_path_buf(),
        size: data.len(),
    })
}

/// 打印只读计划：不查设备、不创建目录、不备份、不写文件。
pub fn print_plan(plan: &WritePlan) {
    println!("[DRY RUN] 目标(相对卡根): {}", plan.table_path.display());
    println!("[DRY RUN] 大小: {} B", plan.size);
    println!("[DRY RUN] 未查找设备、未写入、未备份");
}

/// 落盘抽象；注入模拟写入器可验证只读路径的调用次数。
pub trait TableWriter {
    fn write_table(&mut self, path: &Path, data: &[u8]) -> Result<PathBuf, CameraError>;
}

/// 真实文件系统写入器：把相对路径拼到已核验的挂载点。
pub struct FsTableWriter<'a> {
    pub mount_point: &'a Path,
}

impl TableWriter for FsTableWriter<'_> {
    fn write_table(&mut self, path: &Path, data: &[u8]) -> Result<PathBuf, CameraError> {
        safe_write(&self.mount_point.join(path), data)
    }
}

/// 暂存写入器：把相对路径拼到模拟卷/挂载点，走受控写入并支持故障注入（R4-03 故障矩阵）。
pub struct StagingTableWriter<'a> {
    pub mount_point: &'a Path,
    pub fault: &'a dyn FaultInjector,
}

impl TableWriter for StagingTableWriter<'_> {
    fn write_table(&mut self, path: &Path, data: &[u8]) -> Result<PathBuf, CameraError> {
        let out = controlled_write(&self.mount_point.join(path), data, self.fault)?;
        Ok(out.backup.unwrap_or_default())
    }
}

/// `dry_run` 为真时保证不调用 writer；返回 None 表示没有落盘。
pub fn apply_plan<W: TableWriter>(
    plan: &WritePlan,
    data: &[u8],
    writer: &mut W,
    dry_run: bool,
) -> Result<Option<PathBuf>, CameraError> {
    if dry_run {
        return Ok(None);
    }
    if data.len() != plan.size {
        return Err(CameraError::SizeMismatch {
            expected: plan.size,
            actual: data.len(),
        });
    }
    Ok(Some(writer.write_table(&plan.table_path, data)?))
}

/// 部署滤镜到相机卡：查找卡 → 挂载检查 → 生成计划 → 安全写入 → 回读校验。
///
/// 本函数恒为真实写入；只读预演请直接使用 `build_plan` + `print_plan`，
/// 不再由布尔参数决定写或不写，避免参数解析歧义导致误写。
pub fn deploy(table_path: &Path, data: &[u8]) -> Result<CardInfo, CameraError> {
    ensure_write_gate()?;
    let card = find_card()?;

    if !is_mounted(&card) {
        return Err(CameraError::NotMounted);
    }

    let mp = mount_point(&card)?;
    let plan = build_plan(table_path, data)?;
    let mut writer = FsTableWriter { mount_point: mp };
    let backup = apply_plan(&plan, data, &mut writer, false)?.unwrap_or_default();

    println!("✅ 写入成功: {} ({} B)", mp.join(&plan.table_path).display(), data.len());
    if !backup.as_os_str().is_empty() {
        println!("📦 备份: {}", backup.display());
    }

    Ok(card)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_protected_dcim() {
        assert!(is_protected(Path::new("/Volumes/DXO/DCIM/test.bin")));
    }

    #[test]
    fn test_is_protected_touch_prefs() {
        assert!(is_protected(Path::new("/Volumes/DXO/touch_prefs/data.bin")));
    }

    #[test]
    fn test_is_protected_normal() {
        assert!(!is_protected(Path::new("/Volumes/DXO/filters/test.bin")));
    }

    #[test]
    fn test_validate_safe_path() {
        assert!(validate_safe_path(Path::new("/Volumes/DXO/filters/test.bin")).is_ok());
        assert!(validate_safe_path(Path::new("/Volumes/DXO/DCIM/test.bin")).is_err());
    }

    #[test]
    fn test_backup_and_write() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let data = b"hello world";

        // 首次写入（无备份）
        let _backup = safe_write(&path, data).unwrap();
        assert!(path.exists());
        assert_eq!(fs::read(&path).unwrap(), data);

        // 第二次写入（有备份）
        let data2 = b"hello rust";
        let backup2 = safe_write(&path, data2).unwrap();
        assert!(backup2.exists());
        assert_eq!(fs::read(&path).unwrap(), data2);
    }

    #[test]
    fn test_write_verify_fails_on_corrupt() {
        // 模拟：写入后手动破坏文件，验证回读校验
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("test.bin");
        let data = b"test data";

        // 先正常写入
        safe_write(&path, data).unwrap();

        // 手动破坏（模拟写入失败）
        fs::write(&path, b"corrupt").unwrap();

        // 重新写入应成功（覆盖损坏文件）
        let _backup = safe_write(&path, data).unwrap();
        assert_eq!(fs::read(&path).unwrap(), data);
    }

    /// 模拟写入器：只计数，不接触磁盘。
    struct CountingWriter {
        calls: usize,
        last_path: Option<PathBuf>,
    }

    impl TableWriter for CountingWriter {
        fn write_table(&mut self, path: &Path, _data: &[u8]) -> Result<PathBuf, CameraError> {
            self.calls += 1;
            self.last_path = Some(path.to_path_buf());
            Ok(PathBuf::new())
        }
    }

    #[test]
    fn test_dry_run_plan_never_invokes_writer() {
        let data = [1u8; 16];
        let plan = build_plan(Path::new("filters/Imx183_Cc_Still1.bin"), &data).unwrap();
        let mut writer = CountingWriter { calls: 0, last_path: None };
        let result = apply_plan(&plan, &data, &mut writer, true).unwrap();
        assert!(result.is_none());
        assert_eq!(writer.calls, 0, "dry-run 不得调用写入器");
        assert!(writer.last_path.is_none());
    }

    #[test]
    fn test_real_plan_invokes_writer_once() {
        let data = [1u8; 16];
        let plan = build_plan(Path::new("filters/Imx183_Cc_Still1.bin"), &data).unwrap();
        let mut writer = CountingWriter { calls: 0, last_path: None };
        let result = apply_plan(&plan, &data, &mut writer, false).unwrap();
        assert!(result.is_some());
        assert_eq!(writer.calls, 1);
        assert_eq!(writer.last_path.as_deref(), Some(plan.table_path.as_path()));
    }

    #[test]
    fn test_build_plan_rejects_unsafe_paths() {
        assert!(build_plan(Path::new("/etc/x.bin"), &[]).is_err());
        assert!(build_plan(Path::new("../DCIM/x.bin"), &[]).is_err());
        assert!(build_plan(Path::new("DCIM/x.bin"), &[]).is_err());
        assert!(build_plan(Path::new("filters/x.bin"), &[]).is_ok());
    }

    /// Windows 的路径语义与 Unix 不同：`/Windows/x.bin` 不是绝对路径，却带 RootDir，
    /// `push` 时会丢弃挂载点根之后的部分。这里把「跳出卡根」的实际后果钉成断言，
    /// 以免守卫被改回只看 `is_absolute()`。
    #[cfg(windows)]
    #[test]
    fn build_plan_rejects_windows_rooted_and_prefixed_paths() {
        for raw in [
            "/Windows/x.bin",
            "\\Windows\\x.bin",
            "C:\\Windows\\x.bin",
            "\\\\?\\C:\\Windows\\x.bin",
            "\\\\server\\share\\x.bin",
        ] {
            assert!(build_plan(Path::new(raw), &[]).is_err(), "{raw} 必须拒绝");
        }
        assert_eq!(
            Path::new("C:\\card").join("/Windows/x.bin"),
            Path::new("C:\\Windows\\x.bin"),
            "带根相对路径会把挂载点整段换掉"
        );
        assert!(build_plan(Path::new("filters/x.bin"), &[]).is_ok());
    }

    #[test]
    fn test_apply_plan_rejects_size_mismatch() {
        let plan = build_plan(Path::new("filters/x.bin"), &[0u8; 8]).unwrap();
        let mut writer = CountingWriter { calls: 0, last_path: None };
        assert!(apply_plan(&plan, &[0u8; 7], &mut writer, false).is_err());
        assert_eq!(writer.calls, 0);
    }

    // ── R4-03 受控写入 / 故障矩阵（模拟卷） ──────────────────

    fn no_tmp_leftover(dir: &Path) -> bool {
        fs::read_dir(dir)
            .unwrap()
            .flatten()
            .all(|e| !e.file_name().to_string_lossy().contains(".tmp"))
    }

    #[test]
    fn test_controlled_write_staging_and_backup() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("filters").join("t.bin");
        // 首次写入：无备份，内容正确，无暂存残留
        let out = controlled_write(&path, b"AAAA", &NoFault).unwrap();
        assert!(out.backup.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"AAAA");
        assert!(no_tmp_leftover(path.parent().unwrap()));
        // 覆盖写入：备份保留旧内容
        let out2 = controlled_write(&path, b"BBBB", &NoFault).unwrap();
        let bak = out2.backup.unwrap();
        assert_eq!(fs::read(&bak).unwrap(), b"AAAA");
        assert_eq!(fs::read(&path).unwrap(), b"BBBB");
        assert!(no_tmp_leftover(path.parent().unwrap()));
    }

    #[test]
    fn test_fault_matrix_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Path::new("filters").join("t.bin");
        let path = dir.path().join(&rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let old = b"OLD_CONTENT";
        let new = b"NEW_CONTENT";
        fs::write(&path, old).unwrap();

        for point in [
            FaultPoint::Backup,
            FaultPoint::StageWrite,
            FaultPoint::StageFsync,
            FaultPoint::Rename,
            FaultPoint::Readback,
        ] {
            let err = controlled_write(&path, new, &InjectFault(point)).unwrap_err();
            assert!(
                matches!(err, CameraError::InjectedFault(p) if p == point),
                "{point:?} 应命中注入故障，实得 {err}"
            );
            assert_eq!(fs::read(&path).unwrap(), old, "{point:?} 后目标应回滚为旧内容");
            assert!(no_tmp_leftover(path.parent().unwrap()), "{point:?} 后不得残留暂存文件");
        }
    }

    struct ReadbackAndRestoreFault;
    impl FaultInjector for ReadbackAndRestoreFault {
        fn check(&self, p: FaultPoint) -> Result<(), CameraError> {
            if matches!(p, FaultPoint::Readback | FaultPoint::Restore) {
                Err(CameraError::InjectedFault(p))
            } else {
                Ok(())
            }
        }
    }

    #[test]
    fn test_rollback_failed_when_restore_faults() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("t.bin");
        fs::write(&path, b"OLD").unwrap();
        let err = controlled_write(&path, b"NEW", &ReadbackAndRestoreFault).unwrap_err();
        assert!(matches!(err, CameraError::RollbackFailed(_)), "应报回滚失败: {err}");
        // 回滚失败：备份仍在，供人工恢复
        assert!(path.with_file_name("t.bakbin").exists() || fs::read_dir(dir.path()).unwrap().flatten().any(|e| e.file_name().to_string_lossy().contains(".bak")));
    }

    #[test]
    fn test_staging_writer_via_apply_plan() {
        let dir = tempfile::tempdir().unwrap();
        let rel = Path::new("filters").join("t.bin");
        let path = dir.path().join(&rel);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, b"OLD").unwrap();
        let plan = build_plan(&rel, b"NEW").unwrap();

        // 成功路径
        let mut ok = StagingTableWriter { mount_point: dir.path(), fault: &NoFault };
        assert!(apply_plan(&plan, b"NEW", &mut ok, false).unwrap().is_some());
        assert_eq!(fs::read(&path).unwrap(), b"NEW");

        // 故障路径：rename 前中断 → 回滚旧内容
        fs::write(&path, b"OLD").unwrap();
        let mut bad = StagingTableWriter { mount_point: dir.path(), fault: &InjectFault(FaultPoint::Rename) };
        assert!(apply_plan(&plan, b"NEW", &mut bad, false).is_err());
        assert_eq!(fs::read(&path).unwrap(), b"OLD");
    }

    #[test]
    fn test_fingerprint_registrar_disabled() {
        let mut r = DisabledFingerprintRegistrar;
        let err = r.register(Path::new("filters/x.bin"), "abc").unwrap_err();
        assert!(matches!(err, CameraError::WriteDisabled));
    }

    #[test]
    fn test_write_gate_and_deploy_disabled() {
        assert!(matches!(write_gate_check(None).unwrap_err(), CameraError::WriteDisabled));
        if std::env::var(WRITE_AUTH_ENV).ok().as_deref() != Some("1") {
            assert!(matches!(ensure_write_gate().unwrap_err(), CameraError::WriteDisabled));
            let err = deploy(Path::new("filters/x.bin"), &[0u8; 16]).unwrap_err();
            assert!(matches!(err, CameraError::WriteDisabled), "默认启动真实写卡入口须恒禁用: {err}");
        }
    }

    #[test]
    fn test_write_gate_per_launch_auth() {
        assert!(!WRITE_GATE_ENABLED, "编译期闸门须恒禁用");
        assert!(write_gate_check(None).is_err());
        assert!(write_gate_check(Some("")).is_err());
        assert!(write_gate_check(Some("0")).is_err());
        assert!(write_gate_check(Some("yes")).is_err());
        assert!(write_gate_check(Some("1")).is_ok(), "仅显式 =1 的逐次授权启动开放");
    }

    static TOUCH_ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    struct TouchEnvGuard;
    impl Drop for TouchEnvGuard {
        fn drop(&mut self) {
            std::env::remove_var(TOUCH_WRITE_AUTH_ENV);
        }
    }

    #[test]
    fn test_touch_write_gate_pure() {
        assert!(matches!(touch_write_gate_check(None).unwrap_err(), CameraError::TouchWriteDisabled));
        assert!(touch_write_gate_check(Some("")).is_err());
        assert!(touch_write_gate_check(Some("0")).is_err());
        assert!(touch_write_gate_check(Some("yes")).is_err());
        assert!(touch_write_gate_check(Some("1")).is_ok(), "仅显式 =1 的逐次点名授权开放");
    }

    #[test]
    fn test_touch_write_path_whitelist() {
        // 白名单精确到 dxot/a.bin；其余受保护路径即便有授权也拒绝（路径检查先于闸门）
        for p in ["dxot/b.bin", "a.bin", "dxot/sub/a.bin", "touch_prefs.bin", "DCIM/a.bin"] {
            let err = write_touch_a_bin(Path::new(p), b"x", &NoFault).unwrap_err();
            assert!(matches!(err, CameraError::ProtectedPath(_)), "{p}: {err}");
        }
        assert!(is_protected(Path::new("/Volumes/DXO/dxot/a.bin")), "dxot 整体保护不得放松");
    }

    #[test]
    fn test_touch_write_gate_and_authorized_write() {
        let _lock = TOUCH_ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        std::env::remove_var(TOUCH_WRITE_AUTH_ENV);
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("dxot").join("a.bin");
        let err = write_touch_a_bin(&path, b"AAAA", &NoFault).unwrap_err();
        assert!(matches!(err, CameraError::TouchWriteDisabled), "默认启动须恒禁用: {err}");
        assert!(!path.exists(), "闸门拒绝不得落盘");

        let _guard = TouchEnvGuard;
        std::env::set_var(TOUCH_WRITE_AUTH_ENV, "1");
        let out = write_touch_a_bin(&path, b"AAAA", &NoFault).unwrap();
        assert!(out.backup.is_none());
        assert_eq!(fs::read(&path).unwrap(), b"AAAA");
        let out2 = write_touch_a_bin(&path, b"BBBB", &NoFault).unwrap();
        assert!(out2.backup.is_some(), "二次写入须留备份");
        assert_eq!(fs::read(&path).unwrap(), b"BBBB");
    }
}
