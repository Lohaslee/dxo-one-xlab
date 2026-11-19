//! Windows 真机取数证据：在真实 Win32 卷上跑 `volume::query_volume`，
//! 核对文件系统上报、卷身份、刷盘与「绝不能把非相机卡放行」这条底线。
//! 由 CI 的 windows-x64 作业执行（本机无 Windows 环境时该文件整体不参与编译）。

#![cfg(windows)]

use lut_camera::volume::{assess_camera_card, card_identity, query_volume, sync_volume, volume_roots};
use std::path::{Path, PathBuf};

fn root_of(drive: &str) -> PathBuf {
    PathBuf::from(format!("{}\\", drive.trim_end_matches(':')))
}

fn report(label: &str, root: &Path) {
    match query_volume(root) {
        Ok(f) => println!(
            "[win32-volume] {label} root={} mount_matches={} internal={:?} writable_volume={:?} \
             writable_media={:?} fs_type={} fs_name={} free_bytes={:?} device={} uuid={} label={}",
            root.display(), f.mount_matches, f.internal, f.writable_volume, f.writable_media,
            f.fs_type, f.fs_name, f.free_bytes, f.device, f.volume_uuid, f.label
        ),
        Err(e) => println!("[win32-volume] {label} root={} query_volume=Err({e})", root.display()),
    }
}

#[test]
fn system_drive_is_refused_before_any_card_assessment() {
    let drive = std::env::var("SystemDrive").unwrap_or_else(|_| "C:".into());
    let root = root_of(&drive);
    report("system", &root);
    let err = query_volume(&root).expect_err("系统卷必须被拒绝，不能进入写卡流程");
    assert!(err.contains("系统盘"), "系统卷的拒绝理由应指向系统盘，实际：{err}");
}

#[test]
fn every_enumerated_root_yields_a_volume_or_a_refusal() {
    let roots = volume_roots();
    assert!(!roots.is_empty(), "GetLogicalDrives 应至少返回一个卷根");
    for r in &roots {
        match query_volume(r) {
            Ok(f) => {
                assert!(f.volume_uuid.contains("Volume{") || f.device.is_empty(), "卷 GUID 取到才谈得上设备号");
                report("root", r);
            }
            Err(e) => println!("[win32-volume] root={} refused: {e}", r.display()),
        }
    }
}

/// CI 用 diskpart 造出的 FAT32 / exFAT 虚拟卷通过 DXO_TEST_VOLUME 传入。
/// 虚拟盘不是读卡器介质，因此判定必须是「拒绝」，但取数本身要真实可用。
#[test]
fn test_volume_reports_real_facts_and_is_not_mistaken_for_a_card() {
    let Some(drive) = std::env::var("DXO_TEST_VOLUME").ok().filter(|s| !s.is_empty()) else {
        println!("[win32-volume] DXO_TEST_VOLUME 未设置，跳过虚拟卷用例");
        return;
    };
    let want_fs = std::env::var("DXO_TEST_FSTYPE").unwrap_or_default();
    let root = root_of(&drive);
    let facts = query_volume(&root).unwrap_or_else(|e| panic!("{drive} 卷取数失败：{e}"));
    println!(
        "[win32-volume] test root={} mount_matches={} internal={:?} writable_volume={:?} \
         writable_media={:?} fs_type={} fs_name={} free_bytes={:?} device={} uuid={}",
        root.display(), facts.mount_matches, facts.internal, facts.writable_volume,
        facts.writable_media, facts.fs_type, facts.fs_name, facts.free_bytes, facts.device, facts.volume_uuid
    );
    assert!(facts.mount_matches, "{drive} 应被识别为卷根");
    if !want_fs.is_empty() {
        assert_eq!(facts.fs_type, want_fs, "{drive} 的文件系统归一结果");
        assert!(facts.fs_name.contains(if want_fs == "msdos" { "FAT32" } else { "exFAT" }),
            "fs_name 应区分具体格式，实际：{}", facts.fs_name);
    }
    assert!(facts.free_bytes.unwrap_or(0) > 0, "剩余空间应可读取");
    assert!(facts.volume_uuid.starts_with(r"\\?\Volume{"), "卷 GUID 应为 verbatim 卷路径，实际：{}", facts.volume_uuid);
    let identity = card_identity(&root).expect("卷身份串");
    assert!(!identity.is_empty(), "卷身份不能为空");
    println!("[win32-volume] card_identity={identity}");
    let verdict = assess_camera_card(&facts);
    println!("[win32-volume] assess_camera_card={verdict:?}");
    assert!(verdict.is_err(), "非外置介质（虚拟盘）绝不能判定为可写相机卡");
    match sync_volume(&root) {
        Ok(()) => println!("[win32-volume] sync_volume=Ok"),
        Err(e) => println!("[win32-volume] sync_volume=Err({e})"),
    }
}
