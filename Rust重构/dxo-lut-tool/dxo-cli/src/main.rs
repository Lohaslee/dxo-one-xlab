//! dxo-cli — DXO-ONE-XLAB 命令行验证入口。
//!
//! 子命令：
//!   convert <cube> <tone> <strength> [--bw] [-o <out>]   单次转换
//!   selftest                                            自检
//!   pack <in.bin> [-o <out>]                           铺满五槽
//!   save <cube> <name> <tone> <strength> [--bw]         保存滤镜到持久化
//!   list                                                列出已保存滤镜
//!   load <name> [-o <out>]                              加载滤镜 CC 表
//!   delete <name>                                       删除滤镜
//!   dbinfo                                              数据库信息
//!   cardinfo                                            相机卡信息
//!   deploy <packed.bin> <table_path> [--dry-run]        部署到相机卡（--dry-run 只读）
//!   readcard <table_path>                               读取相机卡当前表
//!   capabilities                                        输出只读能力标记

use std::fs;
use std::path::PathBuf;

use lut_core::table::decode_table;
use lut_core::pack_five_slots;
use lut_core::cube::read_cube;
use lut_core::compose_with_strength;
use lut_persist::CC_TABLE_LEN;
use lut_camera;

// 原厂基线由 lut_core::load_stock_bytes() 提供：编译期打包，DXO_STOCK_FILE 可覆盖。

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 2 {
        print_usage();
        return;
    }

    let result = match args[1].as_str() {
        "convert" => cmd_convert(&args[2..]),
        "selftest" => cmd_selftest(&args[2..]),
        "pack" => cmd_pack(&args[2..]),
        "save" => cmd_save(&args[2..]),
        "list" => cmd_list(&args[2..]),
        "load" => cmd_load(&args[2..]),
        "delete" => cmd_delete(&args[2..]),
        "dbinfo" => cmd_dbinfo(&args[2..]),
        "cardinfo" => cmd_cardinfo(&args[2..]),
        "deploy" => cmd_deploy(&args[2..]),
        "capabilities" => cmd_capabilities(),
        "readcard" => cmd_readcard(&args[2..]),
        "card" => cmd_card(&args[2..]),
        "export" => cmd_export(&args[2..]),
        other => {
            eprintln!("未知子命令: {other}");
            print_usage();
            std::process::exit(1);
        }
    };

    if let Err(e) = result {
        eprintln!("错误: {e}");
        std::process::exit(1);
    }
}

fn print_usage() {
    eprintln!("用法: dxo-cli <子命令> [参数]");
    eprintln!("  convert <cube> <tone> <strength> [--bw] [-o <out>]   单次转换");
    eprintln!("  selftest                                            自检");
    eprintln!("  pack <in.bin> [-o <out>]                            铺满五槽");
    eprintln!("  save <cube> <name> <tone> <strength> [--bw]         保存滤镜");
    eprintln!("  list                                                列出滤镜");
    eprintln!("  load <name> [-o <out>]                              加载滤镜");
    eprintln!("  delete <name>                                       删除滤镜");
    eprintln!("  dbinfo                                              数据库信息");
    eprintln!("  cardinfo                                            相机卡信息");
    eprintln!("  deploy <packed.bin> <table_path> [--dry-run]        部署；--dry-run 只读不写");
    eprintln!("  readcard <table_path> [-o <out>]                    读取相机卡当前表");
    eprintln!("  export <目录> [--no-original]                       导出勾选滤镜到 <目录>/DXO_FILTERS");
    eprintln!("  capabilities                                        输出只读能力标记");
}

/// 只读能力标记：供部署脚本确认 CLI 与脚本版本匹配，
/// 防止「新脚本 + 旧二进制」把 --dry-run 当未知位置参数后误写入。
fn cmd_capabilities() -> Result<(), String> {
    println!("deploy_dry_run=1");
    Ok(())
}

/// 解析 -o <out> 参数，返回 (剩余参数, output_path)。
///
/// 缺值、重复或以选项冒充输出路径一律报错，避免参数被静默忽略后
/// 把只读意图误当作写入意图执行。
fn parse_output(args: &[String]) -> Result<(Vec<String>, Option<PathBuf>), String> {
    let mut out = Vec::new();
    let mut output: Option<PathBuf> = None;
    let mut i = 0;
    while i < args.len() {
        if args[i] == "-o" {
            if i + 1 >= args.len() {
                return Err("-o 缺少输出路径".into());
            }
            let value = &args[i + 1];
            if value.starts_with('-') {
                return Err(format!("-o 缺少合法输出路径，得到选项: {value}"));
            }
            if output.is_some() {
                return Err("重复的 -o 参数".into());
            }
            output = Some(PathBuf::from(value));
            i += 2;
        } else {
            out.push(args[i].clone());
            i += 1;
        }
    }
    Ok((out, output))
}

/// deploy 专用严格解析：恰好两个位置参数，只允许 --dry-run 一个选项。
///
/// 历史上脚本传入裸 `-o` 被本解析静默保留，导致 `output.is_some()` 为 false、
/// 只读预演实际执行写入。此函数拒绝任何未知/缺值/多余参数，杜绝该歧义。
fn parse_deploy_args(args: &[String]) -> Result<(String, String, bool), String> {
    let mut pos: Vec<String> = Vec::new();
    let mut dry_run = false;
    for a in args {
        match a.as_str() {
            "--dry-run" => {
                if dry_run {
                    return Err("重复的 --dry-run".into());
                }
                dry_run = true;
            }
            "-o" => return Err("deploy 不支持 -o（历史上被误用作 dry-run）；请使用 --dry-run".into()),
            s if s.starts_with('-') => return Err(format!("未知选项: {s}")),
            _ => pos.push(a.clone()),
        }
    }
    if pos.len() != 2 {
        return Err(format!(
            "deploy 需要恰好 2 个位置参数 <packed.bin> <table_path>，实得 {}",
            pos.len()
        ));
    }
    let table_path = pos.pop().unwrap();
    let packed_path = pos.pop().unwrap();
    Ok((packed_path, table_path, dry_run))
}

/// 解析 --bw 标志，返回 (剩余参数, bw)
fn parse_bw(args: &[String]) -> (Vec<String>, bool) {
    let mut out = Vec::new();
    let mut bw = false;
    for a in args {
        if a == "--bw" {
            bw = true;
        } else {
            out.push(a.clone());
        }
    }
    (out, bw)
}

/// 严格位置参数校验：数量精确，且不允许残留未知选项。
fn require_pos(pos: &[String], n: usize, usage: &str) -> Result<(), String> {
    if let Some(bad) = pos.iter().find(|s| s.starts_with('-')) {
        return Err(format!("{usage}：未知选项 {bad}"));
    }
    if pos.len() != n {
        return Err(format!("{usage}：需要 {n} 个位置参数，实得 {}", pos.len()));
    }
    Ok(())
}

/// save 专用：一次解析 --bw（旧版双重消费导致 bw 恒为 false），并拒绝多余参数。
fn parse_save_args(args: &[String]) -> Result<(String, String, String, f32, bool), String> {
    let (pos, bw) = parse_bw(args);
    require_pos(&pos, 4, "save <cube> <name> <tone> <strength> [--bw]")?;
    let strength: f32 = pos[3]
        .parse()
        .map_err(|_| format!("strength 必须是数字: {}", pos[3]))?;
    Ok((pos[0].clone(), pos[1].clone(), pos[2].clone(), strength, bw))
}

fn db_dir() -> PathBuf {
    lut_persist::Store::default_db_dir()
}

// ── convert ──────────────────────────────────────────────

fn cmd_convert(args: &[String]) -> Result<(), String> {
    let (raw, output) = parse_output(args)?;
    let (pos, bw) = parse_bw(&raw);
    require_pos(&pos, 3, "convert <cube> <tone> <strength> [--bw] [-o <out>]")?;
    let cube_path = &pos[0];
    let tone = &pos[1];
    let strength: f32 = pos[2]
        .parse()
        .map_err(|_| format!("strength 必须是数字: {}", pos[2]))?;

    let stock_bytes =
        lut_core::load_stock_bytes()?;
    let (header, stock_cube, _tail) = decode_table(&stock_bytes);

    let (_size, lut) = read_cube(cube_path)
        .map_err(|e| format!("读 cube 失败: {e}"))?;

    let out = compose_with_strength(Some(&lut), &stock_cube, tone, strength, bw, &header, &_tail);

    match output {
        Some(path) => {
            fs::write(&path, &out).map_err(|e| format!("写输出失败: {e}"))?;
            println!("已写入 {} ({} B)", path.display(), out.len());
        }
        None => {
            println!("CC 表 ({} B):", out.len());
            println!("  sha256={}", lut_persist::Store::hash_bytes(&out));
        }
    }
    Ok(())
}

// ── selftest ─────────────────────────────────────────────

fn cmd_selftest(args: &[String]) -> Result<(), String> {
    require_pos(args, 0, "selftest")?;
    println!("== Rust lut-core 自检 ==");

    // 1. 原厂表解码
    let stock_bytes = lut_core::load_stock_bytes()?;
    let (header, stock_cube, _tail) = decode_table(&stock_bytes);
    println!("  原厂表解码 OK：{} B", stock_bytes.len());
    // 可校验基线：使用打包资源时两行应一致；DXO_STOCK_FILE 覆盖时不一致属预期。
    println!("  当前基线 SHA256: {}", lut_persist::Store::hash_bytes(&stock_bytes));
    println!("  打包基线 SHA256: {}", lut_core::STOCK_TABLE_SHA256);

    // 2. cube 转换：自带 fixture 写临时目录，不依赖项目目录或 CWD
    let fixture = std::env::temp_dir().join(format!(
        "dxo-selftest-{}-{}.cube",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    let mut body = String::from("TITLE selftest\nLUT_3D_SIZE 4\n");
    for r in 0..4 {
        for g in 0..4 {
            for b in 0..4 {
                body.push_str(&format!(
                    "{} {} {}\n",
                    r as f32 / 3.0,
                    g as f32 / 3.0,
                    b as f32 / 3.0
                ));
            }
        }
    }
    fs::write(&fixture, &body).map_err(|e| format!("写自检 fixture 失败: {e}"))?;
    let fixture_str = fixture.to_string_lossy().to_string();
    let cube_result = read_cube(&fixture_str);
    let _ = fs::remove_file(&fixture);
    let (_size, lut) = cube_result.map_err(|e| format!("读 cube 失败: {e}"))?;
    let out = compose_with_strength(Some(&lut), &stock_cube, "pass", 1.0, false, &header, &_tail);
    println!("  cube 转换 OK：{} B（自带 fixture）", out.len());

    // 3. 铺满五槽包
    let packed = pack_five_slots(&out);
    println!("  铺满五槽包 OK：{} B", packed.len());

    // 4. 持久化存储
    let dir = db_dir();
    let store = lut_persist::Store::open(&dir)
        .map_err(|e| format!("打开存储失败: {e}"))?;
    println!("  持久化存储 OK：{} 个滤镜", store.count().map_err(|e| e.to_string())?);

    println!("全部自检通过 ✅");
    Ok(())
}

// ── pack ────────────────────────────────────────────────

fn cmd_pack(args: &[String]) -> Result<(), String> {
    let (pos, output) = parse_output(args)?;
    // 旧版宣称支持多输入却只读第一个；现在明确只接受一张表铺满五槽。
    require_pos(&pos, 1, "pack <in.bin> [-o <out>]（把一张表铺满五槽）")?;

    let first = &pos[0];
    let bytes = fs::read(first).map_err(|e| format!("读取 {first} 失败: {e}"))?;
    if bytes.len() != CC_TABLE_LEN {
        return Err(format!(
            "{} 大小 {} B ≠ CC 表 {} B",
            first,
            bytes.len(),
            CC_TABLE_LEN
        ));
    }

    let packed = pack_five_slots(&bytes);

    match output {
        Some(path) => {
            fs::write(&path, &packed).map_err(|e| format!("写输出失败: {e}"))?;
            println!("已写入 {} ({} B)", path.display(), packed.len());
        }
        None => {
            println!("packed ({} B):", packed.len());
        }
    }
    Ok(())
}

// ── save ────────────────────────────────────────────────

fn cmd_save(args: &[String]) -> Result<(), String> {
    save_with_dir(args, &db_dir())
}

/// 保存契约可注入目录，便于用临时资源验证干净账户可运行。
fn save_with_dir(args: &[String], dir: &std::path::Path) -> Result<(), String> {
    let (cube_path, name, tone, strength, bw) = parse_save_args(args)?;

    let stock_bytes = lut_core::load_stock_bytes()?;
    let (header, stock_cube, _tail) = decode_table(&stock_bytes);

    let (_internal_size, lut) = read_cube(&cube_path)
        .map_err(|e| format!("读 cube 失败: {e}"))?;

    let cc_bytes = compose_with_strength(
        Some(&lut), &stock_cube, &tone, strength, bw, &header, &_tail,
    );

    let mut store = lut_persist::Store::open(dir)
        .map_err(|e| format!("打开存储失败: {e}"))?;

    let cube_blob = lut.to_bytes();
    // source_size 必须是源 .cube 声明的网格边长，不是内部恒定的 16。
    let rec = store
        .insert(lut_persist::NewFilter {
            name: &name,
            tone: &tone,
            strength,
            bw,
            cc_bytes: &cc_bytes,
            src: &cube_path,
            format: "cube",
            source_size: lut.source_size as i64,
            locked: false,
            cube_blob: Some(&cube_blob),
        })
        .map_err(|e| format!("保存失败: {e}"))?;

    println!(
        "✅ 已保存滤镜 \"{}\" (id={}, hash={}...)",
        rec.name,
        rec.id,
        &rec.hash[..16]
    );
    Ok(())
}

// ── list ────────────────────────────────────────────────

fn cmd_list(args: &[String]) -> Result<(), String> {
    let (rest, _) = parse_output(args)?;
    if let Some(bad) = rest.iter().find(|s| s.starts_with('-')) {
        return Err(format!("list 不接受选项 {bad}"));
    }
    if rest.len() > 1 {
        return Err(format!("list [tone]：最多 1 个位置参数，实得 {}", rest.len()));
    }
    let tone_filter = if rest.is_empty() {
        None
    } else {
        Some(rest[0].as_str())
    };

    let dir = db_dir();
    let store = lut_persist::Store::open(&dir)
        .map_err(|e| format!("打开存储失败: {e}"))?;

    let records = if let Some(tone) = tone_filter {
        store.list_by_tone(tone).map_err(|e| e.to_string())?
    } else {
        store.list().map_err(|e| e.to_string())?
    };

    if records.is_empty() {
        println!("（无滤镜）");
    } else {
        println!("共 {} 个滤镜:", records.len());
        println!(
            "{:>4}  {:<20}  {:<10}  {:>6}  {:>5}  {:>64}",
            "ID", "名称", "tone", "strength", "bw", "hash"
        );
        println!(
            "{:>4}  {:<20}  {:<10}  {:>6}  {:>5}  {:>64}",
            "---", "----", "----", "--------", "--", "----"
        );
        for r in &records {
            println!(
                "{:>4}  {:<20}  {:<10}  {:>6.2}  {:>5}  {}",
                r.id, r.name, r.tone, r.strength, if r.bw { "✓" } else { "" }, r.hash
            );
        }
    }
    Ok(())
}

// ── load ────────────────────────────────────────────────

fn cmd_load(args: &[String]) -> Result<(), String> {
    let (pos, output) = parse_output(args)?;
    require_pos(&pos, 1, "load <name> [-o <out>]")?;
    let name = &pos[0];

    let dir = db_dir();
    let store = lut_persist::Store::open(&dir)
        .map_err(|e| format!("打开存储失败: {e}"))?;

    let rec = store
        .get_by_name(name)
        .map_err(|e| format!("滤镜不存在: {e}"))?;
    let cc_bytes = store
        .load_bytes(rec.id)
        .map_err(|e| format!("加载失败: {e}"))?;

    match output {
        Some(path) => {
            fs::write(&path, &cc_bytes).map_err(|e| format!("写输出失败: {e}"))?;
            println!("已写入 {} ({} B)", path.display(), cc_bytes.len());
        }
        None => {
            println!("CC 表 ({} B):", cc_bytes.len());
            println!("  sha256={}", lut_persist::Store::hash_bytes(&cc_bytes));
        }
    }
    Ok(())
}

// ── delete ──────────────────────────────────────────────

fn cmd_delete(args: &[String]) -> Result<(), String> {
    require_pos(args, 1, "delete <name>")?;
    let name = &args[0];

    let dir = db_dir();
    let mut store = lut_persist::Store::open(&dir)
        .map_err(|e| format!("打开存储失败: {e}"))?;

    store
        .delete_by_name(name)
        .map_err(|e| format!("删除失败: {e}"))?;

    println!("✅ 已删除滤镜 \"{}\"", name);
    Ok(())
}

// ── dbinfo ──────────────────────────────────────────────

fn cmd_dbinfo(args: &[String]) -> Result<(), String> {
    require_pos(args, 0, "dbinfo")?;
    let dir = db_dir();
    let store = lut_persist::Store::open(&dir)
        .map_err(|e| format!("打开存储失败: {e}"))?;

    println!("数据库目录: {}", dir.display());
    println!("  滤镜总数: {}", store.count().map_err(|e| e.to_string())?);
    println!("  存储目录: {}", dir.join("store").display());

    // 列出 store 目录内容
    let store_dir = dir.join("store");
    if store_dir.exists() {
        let mut entries: Vec<_> = fs::read_dir(&store_dir)
            .map_err(|e| e.to_string())?
            .filter_map(|e| e.ok())
            .collect();
        entries.sort_by_key(|e| e.file_name());
        println!("  内容分片:");
        for entry in &entries {
            let sub_dir = entry.path();
            if sub_dir.is_dir() {
                let count = fs::read_dir(&sub_dir)
                    .map(|r| r.count())
                    .unwrap_or(0);
                println!("    {}/  ({} 个文件)", entry.file_name().to_string_lossy(), count);
            }
        }
    }
    Ok(())
}

// ── cardinfo ─────────────────────────────────────────────

fn card_flag(args: &[String], flag: &str) -> Option<String> {
    args.iter()
        .position(|a| a == flag)
        .and_then(|i| args.get(i + 1))
        .cloned()
}

/// R4-01 只读相机服务（模拟卷 / 参考模板），不写卡。
fn cmd_card(args: &[String]) -> Result<(), String> {
    match args.first().map(|s| s.as_str()).unwrap_or("") {
        "probe" => {
            let root = card_flag(args, "--root").ok_or("card probe 需 --root <模拟卷根>")?;
            let p = lut_camera::probe(std::path::Path::new(&root)).map_err(|e| format!("{e}"))?;
            println!("{}", serde_json::to_string_pretty(&p).map_err(|e| e.to_string())?);
        }
        "profile" => {
            let ph = card_flag(args, "--ph-menu").ok_or("card profile 需 --ph-menu <path>")?;
            let data = fs::read(&ph).map_err(|e| format!("读取 {ph} 失败: {e}"))?;
            let (_segs, entries, preload, magic) =
                lut_camera::card::parse_ph_menu(&data).map_err(|e| format!("{e}"))?;
            let tag = magic.and_then(lut_camera::card::build_tag);
            let out = serde_json::json!({
                "build_tag": tag,
                "known_firmware": tag.is_some(),
                "read_only": tag.is_none(),
                "n_slots": entries.len(),
                "max_slots": lut_camera::card::max_slots(),
                "preload": preload,
                "entries": entries,
            });
            println!("{}", serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?);
        }
        "defaults" => {
            println!(
                "{}",
                serde_json::to_string_pretty(&lut_camera::card::tool_defaults())
                    .map_err(|e| e.to_string())?
            );
        }
        "readbundle" => {
            let root = card_flag(args, "--root").ok_or("card readbundle 需 --root <DXO_FILTERS 目录>")?;
            let p = std::path::Path::new(&root);
            let dxo = if p.file_name().map(|n| n == "DXO_FILTERS").unwrap_or(false) {
                p.to_path_buf()
            } else {
                p.join("DXO_FILTERS")
            };
            let r = lut_camera::read_bundle(&dxo).map_err(|e| format!("{e}"))?;
            let out = serde_json::json!({
                "ok": true,
                "schema_version": r.schema_version,
                "keep_original": r.keep_original,
                "files_verified": r.files.len(),
                "items": r.items.len(),
                "order": r.order,
                "files": r.files,
            });
            println!("{}", serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?);
        }
        "faultmatrix" => {
            use lut_camera::{DisabledFingerprintRegistrar, FaultPoint, FingerprintRegistrar, InjectFault, NoFault};
            let root = card_flag(args, "--root").ok_or("card faultmatrix 需 --root <模拟卷根>")?;
            let mount = std::path::Path::new(&root);
            let rel = std::path::Path::new("filters").join("Imx183_Cc_Still1.bin");
            let path = mount.join(&rel);
            fs::create_dir_all(path.parent().unwrap()).map_err(|e| e.to_string())?;
            let old = b"OLD_STOCK_TABLE";
            let new = b"NEW_FILTER_TABLE";
            let no_tmp = || -> bool {
                fs::read_dir(path.parent().unwrap()).unwrap().flatten()
                    .all(|e| !e.file_name().to_string_lossy().contains(".tmp"))
            };
            let mut rows = Vec::new();
            let mut all_pass = true;
            for point in [FaultPoint::Backup, FaultPoint::StageWrite, FaultPoint::StageFsync, FaultPoint::Rename, FaultPoint::Readback] {
                fs::write(&path, old).map_err(|e| e.to_string())?;
                let res = lut_camera::controlled_write(&path, new, &InjectFault(point));
                let rolled_back = fs::read(&path).map(|d| d == old).unwrap_or(false);
                let clean = no_tmp();
                let pass = res.is_err() && rolled_back && clean;
                all_pass &= pass;
                rows.push(serde_json::json!({
                    "fault": format!("{point:?}"),
                    "errored": res.is_err(),
                    "rolled_back_to_old": rolled_back,
                    "no_tmp_leftover": clean,
                    "pass": pass,
                }));
            }
            fs::write(&path, old).map_err(|e| e.to_string())?;
            let ok = lut_camera::controlled_write(&path, new, &NoFault);
            let success_path = ok.is_ok() && fs::read(&path).map(|d| d == new).unwrap_or(false);
            let gate_disabled = lut_camera::ensure_write_gate().is_err();
            let fp_disabled = DisabledFingerprintRegistrar.register(&rel, "deadbeef").is_err();
            all_pass &= success_path && gate_disabled && fp_disabled;
            let out = serde_json::json!({
                "volume": root,
                "note": "模拟卷故障矩阵（离线，不接触真实相机卡）",
                "matrix": rows,
                "success_path_writes_new": success_path,
                "write_gate_disabled": gate_disabled,
                "fingerprint_registrar_disabled": fp_disabled,
                "all_pass": all_pass,
            });
            println!("{}", serde_json::to_string_pretty(&out).map_err(|e| e.to_string())?);
        }
        _ => return Err("card 子命令: probe --root <dir> | profile --ph-menu <path> | defaults | readbundle --root <dir> | faultmatrix --root <dir>".into()),
    }
    Ok(())
}

fn cmd_cardinfo(args: &[String]) -> Result<(), String> {
    require_pos(args, 0, "cardinfo")?;
    match lut_camera::find_card() {
        Ok(card) => {
            println!("相机存储卡:");
            println!("  设备: {}", card.device);
            println!("  UUID: {}", card.uuid);
            if let Some(ref mp) = card.mount_point {
                println!("  挂载点: {}", mp.display());
                println!("  已挂载: {}", if mp.exists() { "是" } else { "否" });
            } else {
                println!("  挂载点: 未挂载");
            }
            if let Some(ref vn) = card.volume_name {
                println!("  卷名: {vn}");
            }
            Ok(())
        }
        Err(e) => Err(format!("查找相机卡失败: {e}")),
    }
}

// ── deploy ───────────────────────────────────────────────

fn cmd_deploy(args: &[String]) -> Result<(), String> {
    let (packed_path, table_path, dry_run) = parse_deploy_args(args)?;
    let data = fs::read(&packed_path).map_err(|e| format!("读取 {packed_path} 失败: {e}"))?;
    println!("数据大小: {} B", data.len());

    let table = std::path::Path::new(&table_path);
    let plan = lut_camera::build_plan(table, &data)
        .map_err(|e| format!("生成写入计划失败: {e}"))?;

    if dry_run {
        // 只读：完全不调用 deploy，不查找设备，不触碰文件系统。
        lut_camera::print_plan(&plan);
        return Ok(());
    }

    // 真实写入前再次拒绝未经确认的路径，并明确提示这是写卡动作。
    lut_camera::ensure_write_gate().map_err(|e| format!("{e}"))?;
    eprintln!("[WRITE] 即将写入相机卡: {} ({} B)", table.display(), data.len());
    let card = lut_camera::deploy(table, &data)
        .map_err(|e| format!("部署失败: {e}"))?;
    println!("✅ 部署完成: device={}", card.device);
    Ok(())
}

// ── readcard ─────────────────────────────────────────────

fn cmd_readcard(args: &[String]) -> Result<(), String> {
    let (pos, output) = parse_output(args)?;
    require_pos(&pos, 1, "readcard <table_path> [-o <out>]")?;
    let table_path = &pos[0];

    let card = lut_camera::find_card().map_err(|e| format!("查找相机卡失败: {e}"))?;
    let data = lut_camera::read_current_table(&card, std::path::Path::new(table_path))
        .map_err(|e| format!("读取失败: {e}"))?;

    match output {
        Some(path) => {
            fs::write(&path, &data).map_err(|e| format!("写输出失败: {e}"))?;
            println!("已写入 {} ({} B)", path.display(), data.len());
        }
        None => {
            println!("CC 表 ({} B):", data.len());
            println!("  sha256={}", lut_persist::Store::hash_bytes(&data));
        }
    }
    Ok(())
}

// ── export ───────────────────────────────────────────────

fn cmd_export(args: &[String]) -> Result<(), String> {
    let mut target: Option<PathBuf> = None;
    let mut keep_original = true;
    for a in args {
        if a == "--no-original" {
            keep_original = false;
        } else if a.starts_with('-') {
            return Err(format!("export 未知选项: {a}"));
        } else if target.is_some() {
            return Err("export 只接受一个目标目录".into());
        } else {
            target = Some(PathBuf::from(a));
        }
    }
    let target = target.ok_or_else(|| "export 需要 <目录>".to_string())?;
    let dxo = if target
        .file_name()
        .map(|n| n == "DXO_FILTERS")
        .unwrap_or(false)
    {
        target
    } else {
        target.join("DXO_FILTERS")
    };

    let store = lut_persist::Store::open(db_dir()).map_err(|e| format!("打开存储失败: {e}"))?;
    let recs = store.list().map_err(|e| format!("读取列表失败: {e}"))?;
    let picked: Vec<_> = recs.into_iter().filter(|r| r.export).collect();
    if picked.is_empty() {
        return Err("本地库没有勾选导出的滤镜".into());
    }

    let mut tables: Vec<(Vec<u8>, String)> = Vec::with_capacity(picked.len());
    for r in &picked {
        let cc = store
            .load_bytes(r.id)
            .map_err(|e| format!("读取 \"{}\" 失败: {e}", r.name))?;
        let sha = lut_persist::Store::hash_bytes(&cc);
        tables.push((cc, sha));
    }

    let refs: Vec<lut_camera::ExportItem<'_>> = picked
        .iter()
        .zip(tables.iter())
        .map(|(r, (cc, sha))| lut_camera::ExportItem {
            name: &r.name,
            src: &r.src,
            format: &r.format,
            tone: &r.tone,
            strength: r.strength,
            bw: r.bw,
            locked: r.locked,
            source_size: r.source_size,
            cc,
            table_sha256: sha,
        })
        .collect();

    let rep = lut_camera::export_bundle(&dxo, keep_original, &refs, &now_iso())
        .map_err(|e| format!("导出失败: {e}"))?;

    println!(
        "✅ 已导出 {} 个滤镜到 {}（{} 个文件）",
        rep.names.len(),
        rep.root.display(),
        rep.written.len()
    );
    for w in &rep.written {
        println!("  · {}", w.display());
    }
    Ok(())
}

fn now_iso() -> String {
    let secs = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    format!("unix:{secs}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|x| x.to_string()).collect()
    }

    /// D01 回归：历史 deploy.sh 传入裸 `-o`，旧解析把它当位置参数、
    /// output=None → dry_run=false → 预演变真写。现在必须直接报错。
    #[test]
    fn test_deploy_rejects_bare_output_flag() {
        assert!(parse_deploy_args(&s(&["a.bin", "filters/x.bin", "-o"])).is_err());
    }

    #[test]
    fn test_deploy_rejects_unknown_and_duplicate_flags() {
        assert!(parse_deploy_args(&s(&["a.bin", "filters/x.bin", "--nope"])).is_err());
        assert!(parse_deploy_args(&s(&["a.bin", "filters/x.bin", "--dry-run", "--dry-run"])).is_err());
    }

    #[test]
    fn test_deploy_requires_exactly_two_positionals() {
        assert!(parse_deploy_args(&s(&["a.bin"])).is_err());
        assert!(parse_deploy_args(&s(&["a.bin", "filters/x.bin", "extra"])).is_err());
    }

    #[test]
    fn test_deploy_dry_run_is_explicit() {
        let (packed, table, dry) =
            parse_deploy_args(&s(&["a.bin", "filters/x.bin", "--dry-run"])).unwrap();
        assert_eq!(packed, "a.bin");
        assert_eq!(table, "filters/x.bin");
        assert!(dry);
        let (_, _, dry) = parse_deploy_args(&s(&["a.bin", "filters/x.bin"])).unwrap();
        assert!(!dry, "不带 --dry-run 时只允许走真实写入路径");
    }

    #[test]
    fn test_parse_output_rejects_missing_value() {
        assert!(parse_output(&s(&["a", "-o"])).is_err());
        assert!(parse_output(&s(&["a", "-o", "--bw"])).is_err());
        assert!(parse_output(&s(&["a", "-o", "x", "-o", "y"])).is_err());
        let (pos, out) = parse_output(&s(&["a", "-o", "x.bin"])).unwrap();
        assert_eq!(pos, s(&["a"]));
        assert_eq!(out, Some(PathBuf::from("x.bin")));
    }

    /// D08：旧版 save 两次 parse_bw，--bw 被第一次消费后 bw 恒为 false。
    #[test]
    fn test_parse_save_args_bw_is_effective() {
        let (cube, name, tone, strength, bw) =
            parse_save_args(&s(&["a.cube", "N", "pass", "0.5", "--bw"])).unwrap();
        assert_eq!(cube, "a.cube");
        assert_eq!(name, "N");
        assert_eq!(tone, "pass");
        assert!((strength - 0.5).abs() < 1e-6);
        assert!(bw, "--bw 必须生效");
        let (_, _, _, _, bw) = parse_save_args(&s(&["a.cube", "N", "pass", "0.5"])).unwrap();
        assert!(!bw);
    }

    #[test]
    fn test_require_pos_rejects_extra_and_unknown() {
        assert!(require_pos(&s(&["a", "b"]), 1, "pack").is_err());
        assert!(require_pos(&s(&["a", "--nope"]), 1, "pack").is_err());
        assert!(require_pos(&s(&["a"]), 1, "pack").is_ok());
        assert!(require_pos(&[], 0, "dbinfo").is_ok());
    }

    /// 用临时目录跑完整 save 链：验证 --bw 与 source_size 记录的是源网格而非内部 16。
    #[test]
    fn test_save_with_dir_records_bw_and_source_size() {
        let root = std::env::temp_dir().join(format!(
            "dxo-r004-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&root).unwrap();
        let cube_path = root.join("tint4.cube");
        let mut body = String::from("TITLE test\nLUT_3D_SIZE 4\n");
        for r in 0..4 {
            for g in 0..4 {
                for b in 0..4 {
                    body.push_str(&format!(
                        "{} {} {}\n",
                        r as f32 / 3.0,
                        g as f32 / 3.0,
                        b as f32 / 3.0
                    ));
                }
            }
        }
        std::fs::write(&cube_path, body).unwrap();

        let data_dir = root.join("data");
        let args = s(&[
            cube_path.to_str().unwrap(),
            "R004",
            "pass",
            "0.5",
            "--bw",
        ]);
        save_with_dir(&args, &data_dir).unwrap();

        let store = lut_persist::Store::open(&data_dir).unwrap();
        let recs = store.list().unwrap();
        assert_eq!(recs.len(), 1);
        assert!(recs[0].bw, "--bw 必须真实写入库");
        assert_eq!(recs[0].source_size, 4, "应记录源 cube 边长 4，不是内部网格 16");
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn test_packaged_stock_table_is_valid() {
        let bytes = lut_core::load_stock_bytes().unwrap();
        assert_eq!(bytes.len(), lut_core::TABLE_LEN);
        assert_eq!(
            lut_persist::Store::hash_bytes(&bytes),
            lut_core::STOCK_TABLE_SHA256,
            "打包基线必须与声明的 SHA256 一致"
        );
    }
}
