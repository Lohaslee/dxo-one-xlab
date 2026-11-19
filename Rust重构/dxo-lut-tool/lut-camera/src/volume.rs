//! 卷查询的平台边界。
//!
//! macOS 沿用 `diskutil` 输出；Windows 用 Win32 卷/存储查询取回同一组事实。
//! 「能不能当作相机卡」的判断集中在本模块的纯函数里，两套平台共用，
//! 因此在没有 Windows 机器的情况下也能回归；FFI 只负责取数，不做决策。

use std::io;
use std::path::{Component, Path, PathBuf};

/// 安装完成提示里「去哪里安全弹出」的系统说法（Windows 不得出现「访达」）。
pub const EJECT_UI_HINT: &str = if cfg!(windows) {
    "在「此电脑」中右键该卷选择「弹出」"
} else {
    "在访达中安全推出存储卡"
};

/// 一次卷查询取回的原始事实；`None` 表示系统未告知，按信息不完整处理。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeFacts {
    /// 所选路径确实就是该卷的挂载根
    pub mount_matches: bool,
    /// 系统内置磁盘（true 一律拒绝；None 表示查询失败）
    pub internal: Option<bool>,
    pub writable_volume: Option<bool>,
    pub writable_media: Option<bool>,
    /// 归一后的文件系统类型：`msdos` / `exfat` / 其它
    pub fs_type: String,
    /// 补充文件系统名，用于区分 FAT32 与 FAT16
    pub fs_name: String,
    pub free_bytes: Option<u64>,
    pub device: String,
    pub volume_uuid: String,
    /// 卷标（GetVolumeInformationW / diskutil Volume Name），仅用于列表展示
    pub label: String,
}

/// 只读预检的通用判定：任何一项不满足都不进入安装流程。
pub fn assess_camera_card(f: &VolumeFacts) -> Result<(), String> {
    if !f.mount_matches || f.internal != Some(false) {
        return Err("请选择已挂载的外置相机存储卡".into());
    }
    if f.writable_volume != Some(true) || f.writable_media == Some(false) {
        return Err("存储卡只读，请解锁后重试".into());
    }
    if !["msdos", "exfat"].contains(&f.fs_type.as_str()) {
        return Err("仅支持 FAT32 / exFAT 相机卡".into());
    }
    if f.fs_type == "msdos" && !f.fs_name.contains("FAT32") {
        return Err("FAT16 卡不受支持，请先在相机中格式化为 FAT32 / exFAT".into());
    }
    if f.free_bytes.is_none() {
        return Err("无法读取卡剩余空间".into());
    }
    if f.device.is_empty() || f.volume_uuid.is_empty() {
        return Err("存储卡身份信息不完整".into());
    }
    Ok(())
}

/// GetDriveTypeW 的盘符类型 + 存储总线类型 → 是否外置介质。
/// USB 读卡器常被上报为固定磁盘，因此不能只看「可移动」一位。
pub fn is_external_media(drive_type: u32, bus_type: Option<u16>) -> bool {
    const DRIVE_REMOVABLE: u32 = 2;
    // STORAGE_BUS_TYPE: FireWire=4, USB=7, SD=12, MMC=13
    match bus_type {
        Some(4) | Some(6) | Some(7) | Some(12) | Some(13) => true,
        // 总线信息缺失时只接受可换介质盘符，不回退为「允许任意目录」
        _ => drive_type == DRIVE_REMOVABLE,
    }
}

fn folded(component: &Component<'_>, case_insensitive: bool) -> String {
    let s = component.as_os_str().to_string_lossy().into_owned();
    if case_insensitive { s.to_lowercase() } else { s }
}

/// 按路径组件比较（Windows 大小写不敏感；分隔符由 Path 自身归一）。
pub fn paths_equal(a: &Path, b: &Path, case_insensitive: bool) -> bool {
    let mut ia = a.components();
    let mut ib = b.components();
    loop {
        match (ia.next(), ib.next()) {
            (Some(x), Some(y)) if folded(&x, case_insensitive) == folded(&y, case_insensitive) => {}
            (None, None) => return true,
            _ => return false,
        }
    }
}

/// `child` 是否位于 `parent` 之内（按组件判断，避免 `E:\CARD2` 被当成 `E:\CARD` 之内）。
pub fn is_within(child: &Path, parent: &Path, case_insensitive: bool) -> bool {
    let mut ci = child.components();
    for pc in parent.components() {
        match ci.next() {
            Some(cc) if folded(&cc, case_insensitive) == folded(&pc, case_insensitive) => {}
            _ => return false,
        }
    }
    true
}

/// 当前平台的组件比较是否大小写不敏感。
pub fn case_insensitive_paths() -> bool {
    cfg!(windows)
}

/// 卷身份串：参与预检指纹，换卡后即使盘符相同也会变化。
pub fn card_identity(root: &Path) -> io::Result<String> {
    #[cfg(windows)]
    {
        let guid = win::containing_volume_guid(root)
            .ok_or_else(|| io::Error::other("无法读取卷 GUID，不能确认卡身份"))?;
        let (serial, index) = win::volume_and_file_ids(root)?;
        Ok(format!("volume={guid} serial={serial:08X} index={index}"))
    }
    #[cfg(not(windows))]
    {
        use std::os::unix::fs::MetadataExt;
        let meta = std::fs::metadata(root)?;
        Ok(format!("dev={} ino={}", meta.dev(), meta.ino()))
    }
}

/// 平台刷盘：Unix 调 `/bin/sync`；Windows 刷该卷缓存。
/// 失败必须原样上报，调用方不得声称「可以安全拔卡」。
pub fn sync_volume(root: &Path) -> Result<(), String> {
    #[cfg(windows)]
    {
        win::flush_volume(root)
    }
    #[cfg(not(windows))]
    {
        let _ = root;
        if std::process::Command::new("/bin/sync").status().is_ok_and(|s| s.success()) {
            Ok(())
        } else {
            Err("/bin/sync 未成功".into())
        }
    }
}

/// 暂存文件 → 目标：Windows 用 write-through 重命名同步落目录项，
/// 这是「rename 后 fsync 父目录」在 Windows 上的等价保证。
pub fn replace_file(src: &Path, dst: &Path) -> io::Result<()> {
    #[cfg(windows)]
    {
        win::move_write_through(src, dst)
    }
    #[cfg(not(windows))]
    {
        std::fs::rename(src, dst)
    }
}

/// 同步父目录，确保目录项落盘。
/// Windows 没有目录 fsync：目录项的同步性由 [`replace_file`] 的 write-through 重命名
/// 与 [`sync_volume`] 的卷刷盘共同保证，因此这里不做无意义的伪调用。
pub fn fsync_parent(path: &Path) {
    #[cfg(unix)]
    {
        use std::os::unix::io::AsRawFd;
        if let Some(parent) = path.parent() {
            if let Ok(d) = std::fs::File::open(parent) {
                unsafe {
                    libc::fsync(d.as_raw_fd());
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = path;
    }
}

/// 路径是否指向符号链接 / junction / 其它 reparse point（不得沿它写到卡外）。
pub fn is_reparse_point(path: &Path) -> bool {
    let Ok(meta) = std::fs::symlink_metadata(path) else { return true };
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
    }
    #[cfg(not(windows))]
    {
        meta.file_type().is_symlink()
    }
}

/// 归一「卡根」路径写法。
///
/// Unix 用 `canonicalize`（顺带解析符号链接）；Windows 的 `canonicalize` 会加 `\\?\` 前缀
/// 并改写盘符大小写，与后续拼接出的目标比较时会误拒正常卷，因此这里只做卷根写法归一。
/// 「确实是已挂载的外置卷根」由入口点证明：`patch::mounted_disk` 与 `cardops` 选卡
/// 都要求卷 GUID 与卷属性，`card_identity` 取不到卷 GUID 时预检直接失败。
pub fn canonical_root(root: &Path) -> io::Result<PathBuf> {
    #[cfg(windows)]
    {
        Ok(PathBuf::from(win::as_root(root)))
    }
    #[cfg(not(windows))]
    {
        std::fs::canonicalize(root)
    }
}

/// 目录是否「就是它自己」：Unix 比较 canonicalize 结果；Windows 拒绝任何链接/重定向目录。
pub fn dir_is_self_same(dir: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        Ok(!is_reparse_point(dir))
    }
    #[cfg(not(windows))]
    {
        Ok(dir.canonicalize()? == dir)
    }
}

/// 本地备份是否落在卡内（落在卡内必须拒绝）。
///
/// Unix 保持原有 canonicalize + 前缀判断。Windows 上 `\?\` 前缀会让纯字符串比较失真，
/// 因此先确认两者是否同一卷根，再按路径组件判断包含：真卡（卷根）同卷即卡内，
/// 而本机测试用的同级临时目录不会被误判为卡内。
pub fn backup_inside_card(backup_ancestor: &Path, card_root: &Path) -> io::Result<bool> {
    #[cfg(windows)]
    {
        if !win::same_volume(backup_ancestor, card_root)? { return Ok(false); }
        Ok(is_within(backup_ancestor, card_root, true))
    }
    #[cfg(not(windows))]
    {
        Ok(backup_ancestor.canonicalize()?.starts_with(card_root))
    }
}

/// Windows：读取卷事实；其它平台由 diskutil 分支提供同名入口。
#[cfg(windows)]
pub fn query_volume(root: &Path) -> Result<VolumeFacts, String> {
    win::facts(root)
}

/// 枚举可能被当作相机卡的卷根：Windows 返回盘符根，其它平台返回空
/// （macOS 沿用 `/Volumes` 目录枚举，见 `dxo-app::cardops`）。
pub fn volume_roots() -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        win::drive_roots()
    }
    #[cfg(not(windows))]
    {
        Vec::new()
    }
}

/// Windows 实现：只做系统调用，判定一律交回上面的纯函数。
#[cfg(windows)]
mod win {
    use super::VolumeFacts;
    use std::ffi::c_void;
    use std::io;
    use std::path::{Path, PathBuf};
    use std::ptr;
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, HANDLE, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::Storage::FileSystem::{
        CreateFileW, FlushFileBuffers, GetDiskFreeSpaceExW, GetDriveTypeW, GetFileInformationByHandle,
        GetLogicalDrives, GetVolumeInformationW, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
        MoveFileExW, BY_HANDLE_FILE_INFORMATION, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    use windows_sys::Win32::System::IO::DeviceIoControl;

    const CAP: u32 = 256;
    const FILE_SHARE_ALL: u32 = 0x00000001 | 0x00000002 | 0x00000004;
    const OPEN_EXISTING: u32 = 3;
    const FILE_FLAG_BACKUP_SEMANTICS: u32 = 0x0200_0000;
    const GENERIC_READ: u32 = 0x8000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const FILE_READ_ONLY_VOLUME: u32 = 0x0008_0000;
    // CTL_CODE(IOCTL_STORAGE_BASE, 0x0280 / 0x0420, METHOD_BUFFERED, FILE_ANY_ACCESS)
    const IOCTL_STORAGE_QUERY_PROPERTY: u32 = 0x001D_0A00;
    const IOCTL_STORAGE_GET_DEVICE_NUMBER: u32 = 0x001D_1080;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn from_wide(buf: &[u16]) -> String {
        let end = buf.iter().position(|c| *c == 0).unwrap_or(buf.len());
        String::from_utf16_lossy(&buf[..end])
    }

    /// 归一为「卷根」写法：反斜杠、去尾分隔符后补一个，便于与系统返回值比较。
    pub fn as_root(path: &Path) -> String {
        let raw = path.to_string_lossy().replace('/', "\\");
        let trimmed = raw.trim_end_matches('\\');
        if trimmed.is_empty() { "\\".into() } else { format!("{trimmed}\\") }
    }

    /// 路径所属卷的挂载根（盘符根或挂载点目录）。
    fn volume_root_of(path: &Path) -> io::Result<String> {
        let mut buf = vec![0u16; CAP as usize];
        let ok = unsafe {
            GetVolumePathNameW(wide(&path.to_string_lossy()).as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
        };
        if ok == 0 { return Err(win_err("GetVolumePathNameW")); }
        Ok(as_root(Path::new(&from_wide(&buf))).to_uppercase())
    }

    pub fn same_volume(a: &Path, b: &Path) -> io::Result<bool> {
        Ok(volume_root_of(a)? == volume_root_of(b)?)
    }

    fn open(path: &str, access: u32) -> Result<HANDLE, u32> {
        let w = wide(path);
        let h = unsafe {
            CreateFileW(w.as_ptr(), access, FILE_SHARE_ALL, ptr::null(), OPEN_EXISTING,
                        FILE_FLAG_BACKUP_SEMANTICS, ptr::null_mut())
        };
        if h == INVALID_HANDLE_VALUE { Err(unsafe { GetLastError() }) } else { Ok(h) }
    }

    /// 只读 DeviceIoControl 查询（FILE_ANY_ACCESS，非管理员也可查询）。
    /// 存储类查询必须打在卷设备路径 `\\?\Volume{GUID}\` 上，而不是盘符路径。
    fn query(path: &str, code: u32, input: &[u8]) -> Result<Vec<u8>, u32> {
        let mut out = vec![0u8; 512];
        let mut done = 0u32;
        let h = open(path, 0)?;
        let input_ptr = if input.is_empty() { ptr::null_mut() } else { input.as_ptr() as *mut c_void };
        let ok = unsafe {
            DeviceIoControl(h, code, input_ptr, input.len() as u32,
                            out.as_mut_ptr() as *mut c_void, out.len() as u32, &mut done, ptr::null_mut())
        };
        unsafe { CloseHandle(h) };
        if ok == 0 { Err(unsafe { GetLastError() }) } else { out.truncate(done as usize); Ok(out) }
    }

    /// STORAGE_PROPERTY_QUERY：StorageDeviceProperty(0) + PropertyStandardQuery(0)。
    fn property_query() -> Vec<u8> {
        vec![0u8; std::mem::size_of::<u32>() * 3]
    }

    /// STORAGE_DEVICE_DESCRIPTOR.BusType（偏移 28）；查询失败返回 None 交由策略拒绝。
    fn bus_type(path: &str) -> Option<u16> {
        query(path, IOCTL_STORAGE_QUERY_PROPERTY, &property_query())
            .ok()
            .filter(|b| b.len() >= 32)
            .map(|b| u32::from_ne_bytes([b[28], b[29], b[30], b[31]]) as u16)
    }

    fn device_number(path: &str) -> Option<String> {
        query(path, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[])
            .ok()
            .filter(|b| b.len() >= 12)
            .map(|b| {
                let n = |i: usize| u32::from_ne_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
                format!("type{}-disk{}-part{}", n(0), n(4), n(8))
            })
    }

    pub fn drive_roots() -> Vec<PathBuf> {
        let mask = unsafe { GetLogicalDrives() };
        (0..26)
            .filter(|i| mask & (1 << i) != 0)
            .map(|i| PathBuf::from(format!("{}:\\", (b'A' + i as u8) as char)))
            .collect()
    }

    pub fn volume_guid_path(root: &Path) -> Option<String> {
        let mut buf = vec![0u16; CAP as usize];
        let ok = unsafe {
            GetVolumeNameForVolumeMountPointW(wide(&as_root(root)).as_ptr(), buf.as_mut_ptr(), buf.len() as u32)
        };
        (ok != 0).then(|| from_wide(&buf))
    }

    /// 路径所属卷的 GUID：先把它当挂载点直取，取不到再解析所属卷根后取。
    /// 只用于身份绑定；「所选路径必须是已挂载卷根」由 `assess_camera_card` 的
    /// `mount_matches` 判定，这里放宽不会让子目录通过写卡预检。
    pub fn containing_volume_guid(path: &Path) -> Option<String> {
        if let Some(guid) = volume_guid_path(path) {
            return Some(guid);
        }
        let root = volume_root_of(path).ok()?;
        volume_guid_path(Path::new(&root))
    }

    /// 卷序列号 + 目录 file index，用于跨插拔的身份指纹。
    /// `GetVolumeInformationW` 的入参按文档应是卷根，直接把普通子目录当根传会拿到
    /// ERROR_DIR_NOT_ROOT(144)（CI 的模拟卷就是这么失败的）；因此先解析所属卷根取序列号，
    /// 再对目标目录本身取 file index。每个失败点都带上 API 名，避免下次只剩裸 errno。
    pub fn volume_and_file_ids(root: &Path) -> io::Result<(u32, u64)> {
        let vol_root = volume_root_of(root)?;
        let mut serial = 0u32;
        let ok = unsafe {
            GetVolumeInformationW(wide(&vol_root).as_ptr(), ptr::null_mut(), 0, &mut serial,
                                  ptr::null_mut(), ptr::null_mut(), ptr::null_mut(), 0)
        };
        if ok == 0 { return Err(win_err("GetVolumeInformationW")); }
        let path = as_root(root);
        let h = open(&path, 0).map_err(|e| win_err_named("CreateFileW(目录句柄)", e))?;
        // windows-sys 的结构体没有 Default，零初始化后由系统填充
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        let got = unsafe { GetFileInformationByHandle(h, &mut info) };
        unsafe { CloseHandle(h) };
        if got == 0 { return Err(win_err("GetFileInformationByHandle")); }
        let index = ((info.nFileIndexHigh as u64) << 32) | info.nFileIndexLow as u64;
        Ok((serial, index))
    }

    /// 把 GetLastError 包成带 API 名的 io::Error（裸 os error 无法定位调用点）。
    fn win_err(api: &str) -> io::Error {
        win_err_named(api, unsafe { GetLastError() })
    }

    fn win_err_named(api: &str, code: u32) -> io::Error {
        io::Error::other(format!("{api} 失败（GetLastError={code}）：{}",
                                 io::Error::from_raw_os_error(code as i32)))
    }

    pub fn facts(root: &Path) -> Result<VolumeFacts, String> {
        if super::is_reparse_point(root) {
            return Err("所选路径是链接或重定向目标，不能作为相机卡".into());
        }
        let path = as_root(root);
        // 只有真正的卷挂载点才能取到 GUID：普通子目录与失效路径在这里就被挡下
        let guid = volume_guid_path(root).ok_or("所选路径不是已挂载的卷根，请重新选择存储卡")?;
        let mut vol_root = vec![0u16; CAP as usize];
        let got = unsafe { GetVolumePathNameW(wide(&path).as_ptr(), vol_root.as_mut_ptr(), vol_root.len() as u32) };
        let mount_matches = got != 0
            && super::paths_equal(Path::new(&path), Path::new(&as_root(Path::new(&from_wide(&vol_root)))), true);

        // 系统盘判定按卷 GUID 比对，避免把挂载在系统盘目录下的卷误判、也避免放行系统卷
        let system_drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
        let system_guid = volume_guid_path(Path::new(&format!("{}\\", system_drive.trim_end_matches(':'))));
        if system_guid.as_deref() == Some(guid.as_str()) {
            return Err("不能选择系统盘作为相机存储卡".into());
        }

        let mut fs_name = vec![0u16; CAP as usize];
        let mut label = vec![0u16; CAP as usize];
        let mut serial = 0u32;
        let mut flags = 0u32;
        let mut component = 0u32;
        let info_ok = unsafe {
            GetVolumeInformationW(wide(&path).as_ptr(), label.as_mut_ptr(), label.len() as u32,
                                  &mut serial, &mut component, &mut flags,
                                  fs_name.as_mut_ptr(), fs_name.len() as u32)
        };
        if info_ok == 0 {
            return Err(format!("无法读取卷信息（GetLastError={})", unsafe { GetLastError() }));
        }
        let win_fs = from_wide(&fs_name).to_uppercase();
        let readonly = flags & FILE_READ_ONLY_VOLUME != 0;
        // Windows 上报 "FAT32" / "FAT" / "exFAT"；映射到与 macOS diskutil 同一套判定输入
        let (fs_type, fs_label) = match win_fs.as_str() {
            "EXFAT" => ("exfat".to_string(), "exFAT".to_string()),
            "FAT32" => ("msdos".to_string(), "MS-DOS FAT32".to_string()),
            "FAT" => ("msdos".to_string(), "MS-DOS FAT16".to_string()),
            other => (other.to_lowercase(), other.to_string()),
        };

        let mut available = 0u64;
        let mut total = 0u64;
        let mut total_free = 0u64;
        let space_ok = unsafe {
            GetDiskFreeSpaceExW(wide(&path).as_ptr(), &mut available, &mut total, &mut total_free)
        };
        let drive_type = unsafe { GetDriveTypeW(wide(&path).as_ptr()) };
        // 总线与磁盘编号只能从卷设备路径查询（盘符路径上会失败）
        let bus = bus_type(&guid);
        let device = device_number(&guid).unwrap_or_default();

        Ok(VolumeFacts {
            mount_matches,
            internal: Some(!super::is_external_media(drive_type, bus)),
            writable_volume: Some(!readonly),
            // 未提权时无法确证介质写保护开关：只读卷按 false，其余保持未知交由实际写入判定
            writable_media: if readonly { Some(false) } else { None },
            fs_type,
            fs_name: fs_label,
            free_bytes: (space_ok != 0).then_some(available),
            device,
            volume_uuid: guid,
            label: from_wide(&label),
        })
    }

    pub fn move_write_through(src: &Path, dst: &Path) -> io::Result<()> {
        let (s, d) = (wide(&src.to_string_lossy()), wide(&dst.to_string_lossy()));
        if unsafe { MoveFileExW(s.as_ptr(), d.as_ptr(), MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH) } == 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(())
    }

    /// 刷该卷缓存。非管理员通常会被系统拒绝，此时原样返回失败，不声称已安全拔卡。
    pub fn flush_volume(root: &Path) -> Result<(), String> {
        let guid = volume_guid_path(root).ok_or("无法定位卷 GUID，刷盘未确认")?;
        let h = open(&guid, GENERIC_READ | GENERIC_WRITE)
            .map_err(|e| format!("打开卷失败，刷盘未确认（GetLastError={e}）"))?;
        let ok = unsafe { FlushFileBuffers(h) };
        unsafe { CloseHandle(h) };
        if ok == 0 {
            return Err(format!("卷刷盘被系统拒绝（GetLastError={})", unsafe { GetLastError() }));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn facts() -> VolumeFacts {
        VolumeFacts {
            mount_matches: true,
            internal: Some(false),
            writable_volume: Some(true),
            writable_media: Some(true),
            fs_type: "exfat".into(),
            fs_name: "exFAT".into(),
            free_bytes: Some(9_999_999),
            device: "disk-test".into(),
            volume_uuid: "volume-guid-test".into(),
            label: "DXO ONE".into(),
        }
    }

    #[test]
    fn card_policy_rejects_wrong_root_internal_readonly_fs_and_missing_identity() {
        assert!(assess_camera_card(&facts()).is_ok());

        let mut f = facts();
        f.mount_matches = false;
        assert!(assess_camera_card(&f).is_err(), "所选路径不是卷根");

        for internal in [Some(true), None] {
            let mut f = facts();
            f.internal = internal;
            assert!(assess_camera_card(&f).is_err(), "内置磁盘或介质信息缺失必须拒绝: {internal:?}");
        }

        for (field, value) in [("writable_volume", Some(false)), ("writable_media", Some(false))] {
            let mut f = facts();
            match field {
                "writable_volume" => f.writable_volume = value,
                _ => f.writable_media = value,
            }
            assert!(assess_camera_card(&f).is_err(), "{field}");
        }

        for fs in ["ntfs", "apfs", ""] {
            let mut f = facts();
            f.fs_type = fs.into();
            assert!(assess_camera_card(&f).is_err(), "文件系统 {fs}");
        }

        let mut f = facts();
        (f.fs_type, f.fs_name) = ("msdos".into(), "MS-DOS FAT16".into());
        assert!(assess_camera_card(&f).is_err(), "FAT16 拒绝");
        let mut f = facts();
        (f.fs_type, f.fs_name) = ("msdos".into(), "MS-DOS FAT32".into());
        assert!(assess_camera_card(&f).is_ok(), "FAT32 通过");

        let mut f = facts();
        f.free_bytes = None;
        assert!(assess_camera_card(&f).is_err(), "剩余空间未知");
        let mut f = facts();
        f.volume_uuid = String::new();
        assert!(assess_camera_card(&f).is_err(), "卷身份不完整");
        let mut f = facts();
        f.device = String::new();
        assert!(assess_camera_card(&f).is_err(), "设备标识不完整");
    }

    #[test]
    fn usb_card_reader_reported_as_fixed_disk_is_still_external() {
        for bus in [4u16, 6, 7, 12, 13] {
            assert!(is_external_media(3, Some(bus)), "USB/SD 读卡器上报固定磁盘仍为外置: bus {bus}");
        }
        for bus in [3u16, 8, 11, 14, 17] {
            assert!(!is_external_media(3, Some(bus)), "内置总线不算外置: bus {bus}");
        }
        assert!(is_external_media(2, None), "无总线信息时只认 removable");
        assert!(!is_external_media(3, None), "固定磁盘 + 总线未知必须拒绝");
        assert!(!is_external_media(1, None), "盘符不可用");
        assert!(!is_external_media(5, Some(11)), "光驱不因内置总线成为相机卡");
    }

    #[test]
    fn path_rules_use_components_and_case_folding() {
        let card = Path::new("/Volumes/DXO ONE");
        assert!(is_within(&card.join("DXO_FILTERS/a.bin"), card, false));
        assert!(!is_within(Path::new("/Volumes/DXO ONE2/a.bin"), card, false), "字符串前缀不得算卡内");
        assert!(!is_within(Path::new("/Volumes/Other/a.bin"), card, false));
        assert!(!is_within(Path::new("/Volumes/a.bin"), card, false));

        // 大小写折叠与「父级组件更多即不包含」在两平台语义一致
        assert!(paths_equal(Path::new("/E/DXO_FILTERS"), Path::new("/e/dxo_filters"), true));
        assert!(!paths_equal(Path::new("/E/DXO_FILTERS"), Path::new("/e/dxo_filters"), false), "Unix 大小写敏感");
        assert!(is_within(Path::new("/E/CARD/dxot/a.bin"), Path::new("/e/card"), true));
        assert!(!is_within(Path::new("/E/CARD2/a.bin"), Path::new("/e/card"), true));
        assert!(!is_within(Path::new("/E/a.bin"), Path::new("/e/card"), true));
        assert!(!is_within(Path::new("/C/Users/x/AppData"), Path::new("/c/users/x/appdata/local"), true));
        assert_eq!(case_insensitive_paths(), cfg!(windows));
    }

    #[test]
    fn reparse_and_missing_paths_are_treated_as_unsafe() {
        let dir = tempfile::tempdir().unwrap();
        assert!(!is_reparse_point(dir.path()), "普通目录不是链接");
        assert!(is_reparse_point(&dir.path().join("不存在")), "查询失败按不安全处理");
        #[cfg(unix)]
        {
            let outside = tempfile::tempdir().unwrap();
            let link = dir.path().join("link");
            std::os::unix::fs::symlink(outside.path(), &link).unwrap();
            assert!(is_reparse_point(&link), "符号链接必须被识别");
        }
    }

    #[test]
    fn sync_and_identity_report_real_results() {
        let dir = tempfile::tempdir().unwrap();
        let r = sync_volume(dir.path());
        if cfg!(unix) {
            assert!(r.is_ok(), "本机 /bin/sync 应成功: {r:?}");
        }
        let id = card_identity(dir.path()).unwrap();
        assert!(!id.is_empty() && id != "0", "身份串不得为空");
        assert_ne!(id, card_identity(tempfile::tempdir().unwrap().path()).unwrap(), "不同卷/目录身份不同");
        assert!(card_identity(Path::new("/不存在的路径")).is_err(), "路径缺失必须报错而不是伪造身份");
    }

    #[test]
    fn replace_file_roundtrips_on_host_and_overwrites_existing() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("staged.bin");
        let dst = dir.path().join("target.bin");
        std::fs::write(&src, b"new-content").unwrap();
        std::fs::write(&dst, b"old-content").unwrap();
        replace_file(&src, &dst).unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"new-content");
        assert!(!src.exists(), "暂存文件不得残留");
    }
}
