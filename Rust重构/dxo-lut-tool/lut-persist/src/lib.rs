//! 滤镜持久化层：SQLite 元数据 + 内容寻址文件存储。
//!
//! 设计要点：
//! - 每个滤镜以 CC 表字节（17536 B）为内容主体，SHA-256 做内容寻址
//! - 文件存储布局：`store/<hash[0:2]>/<hash>.bin`
//! - 源 LUT（16³ f32 网格）同样内容寻址存 `cubes/<hash[0:2]>/<hash>.f32`，
//!   使 tone / strength / bw 可随时重算——与网页版「内存里持有 LUT 对象」等价
//! - `position` 决定菜单环顺序（对应网页版的库顺序 / 导出顺序）
//! - `export` 对应网页版每行的「导出」开关
//! - **软删除（v3）**：`deleted_at IS NULL` 为活跃；非空表示在回收站里。
//!   `soft_delete` 只打时间戳、**不删内容文件**，`restore` 可原样恢复；
//!   只有 `purge` / `purge_all` 才真正删元数据并在无引用时回收 blob。

use chrono::Utc;
use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};
use std::fs;
use std::path::{Path, PathBuf};
use thiserror::Error;

pub const CC_TABLE_LEN: usize = 17536; // 单槽 CC 表长度

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FilterRecord {
    pub id: i64,
    pub name: String,
    pub tone: String,  // "pass" | "neutral" | "blend"
    pub strength: f32, // 0.0 = stock, 1.0 = full
    pub bw: bool,
    pub hash: String, // CC 表 SHA-256 hex
    pub created_at: String,
    /// 菜单环顺序（0 为首位）
    pub position: i64,
    /// 是否勾选导出
    pub export: bool,
    /// 来源文件名
    pub src: String,
    /// 源格式（cube / bin …）
    pub format: String,
    /// 源 LUT 网格尺寸（16 / 33 / 65 …）
    pub source_size: i64,
    /// 卡载滤镜：参数锁定，不可改 tone/strength/bw
    pub locked: bool,
    /// 源 LUT blob 的哈希（有则可重算；卡载 bin 为 None）
    pub cube_hash: Option<String>,
    /// 软删除时间戳（RFC3339）。`None` = 在库中；`Some` = 在回收站
    pub deleted_at: Option<String>,
}

impl FilterRecord {
    /// 是否在回收站里。
    pub fn is_deleted(&self) -> bool {
        self.deleted_at.is_some()
    }
}

/// 新增滤镜的入参。
pub struct NewFilter<'a> {
    pub name: &'a str,
    pub tone: &'a str,
    pub strength: f32,
    pub bw: bool,
    pub cc_bytes: &'a [u8],
    pub src: &'a str,
    pub format: &'a str,
    pub source_size: i64,
    pub locked: bool,
    /// 源 LUT 网格序列化字节（`CubeLUT::to_bytes`），卡载滤镜传 None
    pub cube_blob: Option<&'a [u8]>,
}

#[derive(Debug, Error)]
pub enum StoreError {
    #[error("IO 错误: {0}")]
    Io(#[from] std::io::Error),
    #[error("SQLite 错误: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("滤镜不存在: {0}")]
    NotFound(String),
    #[error("滤镜名称无效: {0}")]
    InvalidName(String),
    #[error("内容哈希不匹配")]
    HashMismatch,
}

pub struct Store {
    db_dir: PathBuf,
    conn: Connection,
}

const SELECT_COLS: &str = "id, name, tone, strength, bw, hash, created_at, \
                           position, export, src, format, source_size, locked, cube_hash, deleted_at";

fn row_to_record(r: &rusqlite::Row<'_>) -> rusqlite::Result<FilterRecord> {
    Ok(FilterRecord {
        id: r.get(0)?,
        name: r.get(1)?,
        tone: r.get(2)?,
        strength: r.get(3)?,
        bw: r.get::<_, i32>(4)? != 0,
        hash: r.get(5)?,
        created_at: r.get(6)?,
        position: r.get(7)?,
        export: r.get::<_, i32>(8)? != 0,
        src: r.get(9)?,
        format: r.get(10)?,
        source_size: r.get(11)?,
        locked: r.get::<_, i32>(12)? != 0,
        cube_hash: r.get(13)?,
        deleted_at: r.get(14)?,
    })
}

/// 平台数据目录规则（纯函数：在任意主机上都能单测三套平台的取值）。
/// Windows 走系统本地应用数据目录，macOS 保持原 ~/Library/Application Support 不变。
fn db_dir_for(os: &str, env: &dyn Fn(&str) -> Option<String>) -> PathBuf {
    match os {
        "windows" => {
            let base = env("LOCALAPPDATA")
                .or_else(|| env("USERPROFILE").map(|p| format!("{p}\\AppData\\Local")))
                .unwrap_or_default();
            PathBuf::from(base).join("DXO-ONE-XLAB")
        }
        "macos" => PathBuf::from(env("HOME").unwrap_or_default()).join("Library/Application Support/DXO-ONE-XLAB"),
        _ => PathBuf::from(env("HOME").unwrap_or_default()).join(".local/share/DXO-ONE-XLAB"),
    }
}

impl Store {
    /// 打开或创建持久化存储。
    ///
    /// `db_dir` 是存储根目录，将在其下创建 `dxo_filters.db`、`store/`、`cubes/`。
    /// 默认应用数据目录：优先 `DXO_DATA_DIR`，否则按平台约定。
    /// 测试可把 `DXO_DATA_DIR` 指向临时目录，不依赖开发机项目路径。
    pub fn default_db_dir() -> std::path::PathBuf {
        if let Some(p) = std::env::var_os("DXO_DATA_DIR") {
            return std::path::PathBuf::from(p);
        }
        let env = |k: &str| std::env::var_os(k).map(|v| v.to_string_lossy().into_owned());
        db_dir_for(std::env::consts::OS, &env)
    }

    pub fn open(db_dir: impl AsRef<Path>) -> Result<Self, StoreError> {
        let dir = db_dir.as_ref().to_path_buf();
        fs::create_dir_all(&dir)?;

        let db_path = dir.join("dxo_filters.db");
        let conn = Connection::open(&db_path)?;
        conn.execute_batch(
            "PRAGMA journal_mode=WAL;
             PRAGMA synchronous=NORMAL;
             CREATE TABLE IF NOT EXISTS filters (
                 id          INTEGER PRIMARY KEY AUTOINCREMENT,
                 name        TEXT    NOT NULL,
                 tone        TEXT    NOT NULL,
                 strength    REAL    NOT NULL,
                 bw          INTEGER NOT NULL DEFAULT 0,
                 hash        TEXT    NOT NULL,
                 created_at  TEXT    NOT NULL
             );
             CREATE INDEX IF NOT EXISTS idx_filters_name ON filters(name);",
        )?;

        // --- 迁移：补齐 v2 字段（position / export / src / format / source_size / locked / cube_hash）
        // --- 迁移：补齐 v3 字段（deleted_at，软删除时间戳）
        let mut existing: Vec<String> = Vec::new();
        {
            let mut stmt = conn.prepare("PRAGMA table_info(filters)")?;
            let rows = stmt.query_map([], |r| r.get::<_, String>(1))?;
            for r in rows {
                existing.push(r?);
            }
        }
        let wanted: &[(&str, &str)] = &[
            ("position", "INTEGER NOT NULL DEFAULT 0"),
            ("export", "INTEGER NOT NULL DEFAULT 1"),
            ("src", "TEXT NOT NULL DEFAULT ''"),
            ("format", "TEXT NOT NULL DEFAULT 'cube'"),
            ("source_size", "INTEGER NOT NULL DEFAULT 0"),
            ("locked", "INTEGER NOT NULL DEFAULT 0"),
            ("cube_hash", "TEXT"),
            ("deleted_at", "TEXT"),
        ];
        for (col, decl) in wanted {
            if !existing.iter().any(|c| c == col) {
                conn.execute_batch(&format!("ALTER TABLE filters ADD COLUMN {col} {decl};"))?;
            }
        }
        // 旧库 position 全为 0 → 用 id（创建顺序）铺开，保证顺序稳定
        let zero_pos: i64 = conn.query_row(
            "SELECT COUNT(*) FROM filters WHERE position = 0 AND deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        let total: i64 = conn.query_row(
            "SELECT COUNT(*) FROM filters WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        if zero_pos > 1 && zero_pos == total {
            conn.execute_batch("UPDATE filters SET position = id WHERE deleted_at IS NULL;")?;
        }
        conn.execute_batch("CREATE INDEX IF NOT EXISTS idx_filters_pos ON filters(position);")?;
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS idx_filters_deleted ON filters(deleted_at);",
        )?;

        fs::create_dir_all(dir.join("store"))?;
        fs::create_dir_all(dir.join("cubes"))?;

        Ok(Store { db_dir: dir, conn })
    }

    /// 计算字节的 SHA-256（hex）。
    pub fn hash_bytes(data: &[u8]) -> String {
        let mut h = Sha256::new();
        h.update(data);
        hex::encode(h.finalize())
    }

    fn content_path(&self, hash: &str) -> PathBuf {
        self.db_dir
            .join("store")
            .join(&hash[..2])
            .join(format!("{hash}.bin"))
    }

    fn cube_path(&self, hash: &str) -> PathBuf {
        self.db_dir
            .join("cubes")
            .join(&hash[..2])
            .join(format!("{hash}.f32"))
    }

    /// 写入内容文件（已存在则跳过），返回哈希。
    fn write_blob(&self, dir_kind: BlobKind, data: &[u8]) -> Result<String, StoreError> {
        let hash = Self::hash_bytes(data);
        let path = match dir_kind {
            BlobKind::Table => self.content_path(&hash),
            BlobKind::Cube => self.cube_path(&hash),
        };
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        if !path.exists() {
            fs::write(&path, data)?;
        }
        Ok(hash)
    }

    fn next_position(&self) -> i64 {
        self.conn
            .query_row("SELECT COALESCE(MAX(position), -1) + 1 FROM filters", [], |r| {
                r.get(0)
            })
            .unwrap_or(0)
    }

    /// 新增一条滤镜，自动排到库末尾。
    pub fn insert(&mut self, f: NewFilter<'_>) -> Result<FilterRecord, StoreError> {
        let name = sanitize_name(f.name)?;
        if f.cc_bytes.len() != CC_TABLE_LEN {
            return Err(StoreError::InvalidName(format!(
                "CC 表长度 {} ≠ {CC_TABLE_LEN}",
                f.cc_bytes.len()
            )));
        }
        let hash = self.write_blob(BlobKind::Table, f.cc_bytes)?;
        let cube_hash = match f.cube_blob {
            Some(b) => Some(self.write_blob(BlobKind::Cube, b)?),
            None => None,
        };
        let position = self.next_position();
        let created_at = Utc::now().to_rfc3339();

        self.conn.execute(
            "INSERT INTO filters
                (name, tone, strength, bw, hash, created_at, position, export, src, format, source_size, locked, cube_hash)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, 1, ?8, ?9, ?10, ?11, ?12)",
            rusqlite::params![
                name,
                f.tone,
                f.strength,
                f.bw as i32,
                hash,
                created_at,
                position,
                f.src,
                f.format,
                f.source_size,
                f.locked as i32,
                cube_hash,
            ],
        )?;
        let id = self.conn.last_insert_rowid();
        self.get_record(id)
    }

    /// 更新一条滤镜的参数与内容（名称可一并改）。
    pub fn update(
        &mut self,
        id: i64,
        name: &str,
        tone: &str,
        strength: f32,
        bw: bool,
        cc_bytes: &[u8],
    ) -> Result<FilterRecord, StoreError> {
        let name = sanitize_name(name)?;
        let hash = self.write_blob(BlobKind::Table, cc_bytes)?;
        self.conn.execute(
            "UPDATE filters SET name=?1, tone=?2, strength=?3, bw=?4, hash=?5 \
             WHERE id=?6 AND deleted_at IS NULL",
            rusqlite::params![name, tone, strength, bw as i32, hash, id],
        )?;
        self.get_record(id)
    }

    /// 只改名称。
    pub fn rename(&mut self, id: i64, name: &str) -> Result<(), StoreError> {
        let name = sanitize_name(name)?;
        self.conn.execute(
            "UPDATE filters SET name=?1 WHERE id=?2 AND deleted_at IS NULL",
            rusqlite::params![name, id],
        )?;
        Ok(())
    }

    /// 设置「导出」开关（回收站里的条目恒为不导出）。
    pub fn set_export(&mut self, id: i64, on: bool) -> Result<(), StoreError> {
        self.conn.execute(
            "UPDATE filters SET export=?1 WHERE id=?2 AND deleted_at IS NULL",
            rusqlite::params![on as i32, id],
        )?;
        Ok(())
    }

    /// 按给定 id 序列重写 position（列表顺序 = 菜单环序）。单事务，失败整体回滚。
    pub fn reorder(&mut self, ids: &[i64]) -> Result<(), StoreError> {
        let tx = self.conn.transaction()?;
        for (i, id) in ids.iter().enumerate() {
            tx.execute(
                "UPDATE filters SET position=?1 WHERE id=?2",
                rusqlite::params![i as i64, id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_record(&self, id: i64) -> Result<FilterRecord, StoreError> {
        let row = self
            .conn
            .query_row(
                &format!("SELECT {SELECT_COLS} FROM filters WHERE id = ?1"),
                [id],
                row_to_record,
            )
            .optional()?;
        row.ok_or_else(|| StoreError::NotFound(format!("id={id}")))
    }

    /// 按名称查活跃滤镜（回收站里的同名项不参与）。
    pub fn get_by_name(&self, name: &str) -> Result<FilterRecord, StoreError> {
        let row = self
            .conn
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM filters WHERE name = ?1 AND deleted_at IS NULL \
                     ORDER BY position ASC LIMIT 1"
                ),
                [name],
                row_to_record,
            )
            .optional()?;
        row.ok_or_else(|| StoreError::NotFound(name.to_string()))
    }

    /// 加载滤镜的 CC 表字节。
    pub fn load_bytes(&self, id: i64) -> Result<Vec<u8>, StoreError> {
        let rec = self.get_record(id)?;
        let path = self.content_path(&rec.hash);
        if !path.exists() {
            return Err(StoreError::NotFound(format!(
                "内容文件缺失: {}",
                path.display()
            )));
        }
        Ok(fs::read(&path)?)
    }

    /// 加载源 LUT 网格字节（供重算 tone/strength）。
    pub fn load_cube_blob(&self, cube_hash: &str) -> Result<Vec<u8>, StoreError> {
        let path = self.cube_path(cube_hash);
        if !path.exists() {
            return Err(StoreError::NotFound(format!("LUT 文件缺失: {}", path.display())));
        }
        Ok(fs::read(&path)?)
    }

    /// 列出所有活跃滤镜（按菜单环顺序）。回收站里的不返回。
    pub fn list(&self) -> Result<Vec<FilterRecord>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM filters WHERE deleted_at IS NULL \
             ORDER BY position ASC, id ASC"
        ))?;
        let rows = stmt.query_map([], row_to_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 按 tone 筛选列出活跃滤镜。
    pub fn list_by_tone(&self, tone: &str) -> Result<Vec<FilterRecord>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM filters WHERE tone = ?1 AND deleted_at IS NULL \
             ORDER BY position ASC, id ASC"
        ))?;
        let rows = stmt.query_map([tone], row_to_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    // ---------------------------------------------------------- 回收站

    /// 列出回收站内容（最近删除的在前）。
    pub fn list_deleted(&self) -> Result<Vec<FilterRecord>, StoreError> {
        let mut stmt = self.conn.prepare(&format!(
            "SELECT {SELECT_COLS} FROM filters WHERE deleted_at IS NOT NULL \
             ORDER BY deleted_at DESC, id DESC"
        ))?;
        let rows = stmt.query_map([], row_to_record)?;
        Ok(rows.collect::<Result<Vec<_>, _>>()?)
    }

    /// 回收站条目数。
    pub fn count_deleted(&self) -> Result<i64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM filters WHERE deleted_at IS NOT NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// **软删除**：打 `deleted_at` 时间戳并关掉导出开关。
    ///
    /// 不碰内容文件，也不改 position（`restore` 时要放回原位用的线索在 id 里）。
    pub fn soft_delete(&mut self, id: i64) -> Result<FilterRecord, StoreError> {
        let rec = self.get_record(id)?;
        if rec.deleted_at.is_some() {
            return Ok(rec); // 已在回收站，幂等
        }
        let now = Utc::now().to_rfc3339();
        self.conn.execute(
            "UPDATE filters SET deleted_at = ?1, export = 0 WHERE id = ?2",
            rusqlite::params![now, id],
        )?;
        self.get_record(id)
    }

    /// 把活跃滤镜全部移入回收站（GUI 的「清空」，可整体找回）。
    pub fn soft_delete_all(&mut self) -> Result<usize, StoreError> {
        let now = Utc::now().to_rfc3339();
        let n = self.conn.execute(
            "UPDATE filters SET deleted_at = ?1, export = 0 WHERE deleted_at IS NULL",
            rusqlite::params![now],
        )?;
        Ok(n)
    }

    /// 从回收站恢复，排到库末尾（避免与原 position 冲突）。
    pub fn restore(&mut self, id: i64) -> Result<FilterRecord, StoreError> {
        let rec = self.get_record(id)?;
        if rec.deleted_at.is_none() {
            return Ok(rec); // 本来就在库里
        }
        let pos = self.next_position();
        self.conn.execute(
            "UPDATE filters SET deleted_at = NULL, position = ?1, export = 1 WHERE id = ?2",
            rusqlite::params![pos, id],
        )?;
        self.get_record(id)
    }

    /// **彻底删除**单条：删元数据；内容文件在无任何引用（含回收站里的）时回收。
    pub fn purge(&mut self, id: i64) -> Result<(), StoreError> {
        let rec = self.get_record(id)?;

        let table_refs: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM filters WHERE hash = ?1 AND id != ?2",
            rusqlite::params![rec.hash, id],
            |r| r.get(0),
        )?;
        let cube_refs: i64 = match &rec.cube_hash {
            Some(ch) => self.conn.query_row(
                "SELECT COUNT(*) FROM filters WHERE cube_hash = ?1 AND id != ?2",
                rusqlite::params![ch, id],
                |r| r.get(0),
            )?,
            None => 0,
        };

        self.conn.execute("DELETE FROM filters WHERE id = ?1", [id])?;

        if table_refs == 0 {
            let p = self.content_path(&rec.hash);
            if p.exists() {
                let _ = fs::remove_file(p);
            }
        }
        if cube_refs == 0 {
            if let Some(ch) = &rec.cube_hash {
                let p = self.cube_path(ch);
                if p.exists() {
                    let _ = fs::remove_file(p);
                }
            }
        }
        Ok(())
    }

    /// 清空回收站：彻底删除所有已软删除条目，返回删除条数。
    pub fn purge_all(&mut self) -> Result<usize, StoreError> {
        let ids: Vec<i64> = self.list_deleted()?.into_iter().map(|r| r.id).collect();
        for id in &ids {
            self.purge(*id)?;
        }
        self.gc_orphans()?;
        Ok(ids.len())
    }

    /// 回收没有任何记录引用的内容文件（防止历史脏数据长期占空间）。
    pub fn gc_orphans(&mut self) -> Result<usize, StoreError> {
        let mut live: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut stmt = self.conn.prepare("SELECT hash, cube_hash FROM filters")?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
            ))
        })?;
        for r in rows {
            let (h, ch) = r?;
            live.insert(h);
            if let Some(c) = ch {
                live.insert(c);
            }
        }
        drop(stmt);

        let mut removed = 0usize;
        for (sub, ext) in [("store", "bin"), ("cubes", "f32")] {
            let root = self.db_dir.join(sub);
            let Ok(shards) = fs::read_dir(&root) else {
                continue;
            };
            for shard in shards.flatten() {
                if !shard.path().is_dir() {
                    continue;
                }
                if let Ok(files) = fs::read_dir(shard.path()) {
                    for f in files.flatten() {
                        let p = f.path();
                        if p.extension().and_then(|e| e.to_str()) != Some(ext) {
                            continue;
                        }
                        let stem = p
                            .file_stem()
                            .and_then(|s| s.to_str())
                            .unwrap_or_default()
                            .to_string();
                        if !live.contains(&stem) && fs::remove_file(&p).is_ok() {
                            removed += 1;
                        }
                    }
                }
            }
        }
        Ok(removed)
    }

    /// 按名称彻底删除（CLI 用）。
    pub fn delete_by_name(&mut self, name: &str) -> Result<(), StoreError> {
        let rec = self.get_by_name(name)?;
        self.purge(rec.id)
    }

    pub fn find_by_hash(&self, hash: &str) -> Result<Option<FilterRecord>, StoreError> {
        let row = self
            .conn
            .query_row(
                &format!(
                    "SELECT {SELECT_COLS} FROM filters WHERE hash = ?1 AND deleted_at IS NULL \
                     ORDER BY position ASC LIMIT 1"
                ),
                [hash],
                row_to_record,
            )
            .optional()?;
        Ok(row)
    }

    /// 活跃滤镜数量（不含回收站）。
    pub fn count(&self) -> Result<i64, StoreError> {
        let n: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM filters WHERE deleted_at IS NULL",
            [],
            |r| r.get(0),
        )?;
        Ok(n)
    }

    /// 清空所有滤镜（**硬删除**，含回收站；危险操作，需谨慎）。
    ///
    /// GUI 的「清空」走 `soft_delete_all`，本方法仅供测试 / CLI 重置。
    pub fn clear(&mut self) -> Result<(), StoreError> {
        for sub in ["store", "cubes"] {
            let d = self.db_dir.join(sub);
            if d.exists() {
                fs::remove_dir_all(&d)?;
            }
            fs::create_dir_all(&d)?;
        }
        self.conn.execute_batch("DELETE FROM filters;")?;
        Ok(())
    }
}

enum BlobKind {
    Table,
    Cube,
}

/// 校验滤镜名称：非空、长度 ≤ 64、不含路径分隔符。
pub fn sanitize_name(name: &str) -> Result<String, StoreError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StoreError::InvalidName("名称不能为空".into()));
    }
    if trimmed.chars().count() > 64 {
        return Err(StoreError::InvalidName("名称长度不能超过 64 字符".into()));
    }
    if trimmed.contains('/') || trimmed.contains('\\') {
        return Err(StoreError::InvalidName("名称不能包含 / 或 \\".into()));
    }
    Ok(trimmed.to_string())
}

/// 导出到卡时用的安全目录名：与相机菜单一致的 12 位大写字母数字（对齐 filt_core.sanitize_name）。
pub fn card_safe_name(name: &str, used: &mut std::collections::HashSet<String>) -> String {
    let mut s: String = name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect::<String>()
        .to_uppercase();
    if s.is_empty() {
        s = "LUT".into();
    }
    s.truncate(12);
    let base = s.clone();
    let mut i = 1;
    while used.contains(&s) {
        let keep: String = base.chars().take(10).collect();
        s = format!("{keep}{i}");
        i += 1;
    }
    used.insert(s.clone());
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cc(seed: u8) -> Vec<u8> {
        let mut v = vec![0xAB; CC_TABLE_LEN];
        v[100] = seed;
        v
    }

    #[test]
    fn default_data_dir_rules_per_platform() {
        let env = |k: &str| match k {
            "HOME" => Some("/home/张三".to_string()),
            "LOCALAPPDATA" => Some("C:\\Users\\张三\\AppData\\Local".to_string()),
            "USERPROFILE" => Some("C:\\Users\\张三".to_string()),
            _ => None,
        };
        assert_eq!(
            db_dir_for("macos", &env),
            Path::new("/home/张三").join("Library/Application Support/DXO-ONE-XLAB")
        );
        assert_eq!(
            db_dir_for("windows", &env),
            Path::new("C:\\Users\\张三\\AppData\\Local").join("DXO-ONE-XLAB")
        );
        assert_eq!(db_dir_for("linux", &env), Path::new("/home/张三").join(".local/share/DXO-ONE-XLAB"));
        // LOCALAPPDATA 缺失时按 USERPROFILE 推导，不留相对路径
        let no_local = |k: &str| if k == "USERPROFILE" { Some("C:\\Users\\李四".to_string()) } else { None };
        assert_eq!(db_dir_for("windows", &no_local), Path::new("C:\\Users\\李四\\AppData\\Local").join("DXO-ONE-XLAB"));
    }

    fn newf<'a>(name: &'a str, cc_bytes: &'a [u8]) -> NewFilter<'a> {
        NewFilter {
            name,
            tone: "pass",
            strength: 1.0,
            bw: false,
            cc_bytes,
            src: "t.cube",
            format: "cube",
            source_size: 33,
            locked: false,
            cube_blob: None,
        }
    }

    #[test]
    fn test_hash_deterministic() {
        let a = Store::hash_bytes(&cc(1));
        let b = Store::hash_bytes(&cc(1));
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
    }

    #[test]
    fn test_insert_and_load() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = cc(1);
        let rec = store.insert(newf("test", &bytes)).unwrap();
        assert_eq!(rec.name, "test");
        assert_eq!(rec.position, 0);
        assert!(rec.export);
        assert_eq!(store.load_bytes(rec.id).unwrap(), bytes);
    }

    #[test]
    fn test_same_content_two_entries_allowed() {
        // v2 语义：内容可以重复（同一 LUT 不同名称），不再按 hash 去重
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = cc(1);
        store.insert(newf("a", &bytes)).unwrap();
        store.insert(newf("b", &bytes)).unwrap();
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn test_update_params_and_reorder() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = store.insert(newf("a", &cc(1))).unwrap();
        let b = store.insert(newf("b", &cc(2))).unwrap();
        assert_eq!(store.list().unwrap()[0].name, "a");

        store.update(a.id, "a", "neutral", 0.5, true, &cc(3)).unwrap();
        let got = store.get_record(a.id).unwrap();
        assert_eq!(got.tone, "neutral");
        assert!((got.strength - 0.5).abs() < 1e-6);
        assert!(got.bw);

        store.reorder(&[b.id, a.id]).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list[0].name, "b");
        assert_eq!(list[1].name, "a");
    }

    #[test]
    fn test_export_flag_and_purge_reclaims() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let rec = store.insert(newf("a", &cc(1))).unwrap();
        store.set_export(rec.id, false).unwrap();
        assert!(!store.get_record(rec.id).unwrap().export);

        let p = store.content_path(&rec.hash);
        assert!(p.exists());
        store.purge(rec.id).unwrap();
        assert!(!p.exists());
        assert_eq!(store.count().unwrap(), 0);
    }

    #[test]
    fn test_soft_delete_keeps_blob_and_restore() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = store.insert(newf("a", &cc(1))).unwrap();
        let b = store.insert(newf("b", &cc(2))).unwrap();
        let pa = store.content_path(&a.hash);

        let del = store.soft_delete(a.id).unwrap();
        assert!(del.is_deleted());
        assert!(!del.export, "移入回收站应自动关掉导出开关");
        // 内容文件仍然在（这是可恢复的前提）
        assert!(pa.exists(), "软删除不得删内容文件");
        // 活跃列表不再包含它，回收站里能查到
        assert_eq!(store.list().unwrap().len(), 1);
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.count_deleted().unwrap(), 1);
        assert_eq!(store.list_deleted().unwrap()[0].name, "a");
        assert!(store.get_by_name("a").is_err(), "回收站里的名字不参与查找");
        assert!(store.find_by_hash(&a.hash).unwrap().is_none());

        // 恢复：回到库里、排到末尾、导出开关复原
        let back = store.restore(a.id).unwrap();
        assert!(!back.is_deleted());
        assert!(store.get_by_name("a").is_ok());
        let list = store.list().unwrap();
        assert_eq!(list.len(), 2);
        assert_eq!(list[1].name, "a", "恢复的条目排到末尾");
        assert_eq!(list[1].position, b.position + 1);
        assert_eq!(store.count_deleted().unwrap(), 0);
        // 可正常读回内容
        assert_eq!(store.load_bytes(a.id).unwrap(), cc(1));

        // 幂等：重复软删 / 重复恢复都不报错
        store.soft_delete(a.id).unwrap();
        store.soft_delete(a.id).unwrap();
        store.restore(a.id).unwrap();
        store.restore(a.id).unwrap();
        assert_eq!(store.count().unwrap(), 2);
    }

    #[test]
    fn test_purge_all_and_orphan_gc() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let a = store.insert(newf("a", &cc(1))).unwrap();
        let b = store.insert(newf("b", &cc(2))).unwrap();
        let keep = store.insert(newf("c", &cc(3))).unwrap();
        let (pa, pb, pk) = (
            store.content_path(&a.hash),
            store.content_path(&b.hash),
            store.content_path(&keep.hash),
        );

        store.soft_delete(a.id).unwrap();
        store.soft_delete(b.id).unwrap();
        let n = store.purge_all().unwrap();
        assert_eq!(n, 2);
        assert!(!pa.exists() && !pb.exists(), "彻底删除应回收无引用内容");
        assert!(pk.exists(), "活跃条目内容不得误删");
        assert_eq!(store.count().unwrap(), 1);
        assert_eq!(store.list_deleted().unwrap().len(), 0);

        // 共享同一内容的两条：删掉一条不能动文件
        let c1 = store.insert(newf("c1", &cc(9))).unwrap();
        let c2 = store.insert(newf("c2", &cc(9))).unwrap();
        assert_eq!(c1.hash, c2.hash);
        store.soft_delete(c1.id).unwrap();
        store.purge_all().unwrap();
        assert!(store.content_path(&c1.hash).exists(), "另一条仍引用，不可回收");
        store.purge(c2.id).unwrap();
        assert!(!store.content_path(&c1.hash).exists());

        // 孤儿文件（数据库里没有记录）由 gc 清掉
        let orphan = store.content_path(&"ab".repeat(32));
        fs::create_dir_all(orphan.parent().unwrap()).unwrap();
        fs::write(&orphan, b"x").unwrap();
        assert_eq!(store.gc_orphans().unwrap(), 1);
        assert!(!orphan.exists());
    }

    #[test]
    fn test_clear_all_moves_to_trash_keyword() {
        // GUI 的「清空」= soft_delete_all：全部进回收站，可整体找回
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        store.insert(newf("a", &cc(1))).unwrap();
        store.insert(newf("b", &cc(2))).unwrap();
        assert_eq!(store.soft_delete_all().unwrap(), 2);
        assert_eq!(store.count().unwrap(), 0);
        assert_eq!(store.count_deleted().unwrap(), 2);
        assert_eq!(store.list_deleted().unwrap().len(), 2);
        // 硬清空（CLI/测试）连回收站一起清
        store.clear().unwrap();
        assert_eq!(store.count_deleted().unwrap(), 0);
    }

    #[test]
    fn test_cube_blob_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let blob = vec![7u8; 49192];
        let bytes = cc(9);
        let mut f = newf("c", &bytes);
        f.cube_blob = Some(&blob);
        let rec = store.insert(f).unwrap();
        let ch = rec.cube_hash.unwrap();
        assert_eq!(store.load_cube_blob(&ch).unwrap(), blob);
    }

    #[test]
    fn test_invalid_name() {
        let dir = tempfile::tempdir().unwrap();
        let mut store = Store::open(dir.path()).unwrap();
        let bytes = cc(1);
        assert!(store.insert(newf("", &bytes)).is_err());
        assert!(store.insert(newf("a/b", &bytes)).is_err());
        let long = "a".repeat(65);
        assert!(store.insert(newf(&long, &bytes)).is_err());
    }

    #[test]
    fn test_card_safe_name() {
        let mut used = std::collections::HashSet::new();
        assert_eq!(card_safe_name("Ricoh Posi", &mut used), "RICOHPOSI");
        assert_eq!(card_safe_name("ricoh posi", &mut used), "RICOHPOSI1");
        assert_eq!(card_safe_name("冬野 FUYUNO", &mut used), "FUYUNO");
        assert_eq!(card_safe_name("名", &mut used), "LUT");
        // 超长截断到 12
        assert_eq!(card_safe_name("abcdefghijklmnop", &mut used), "ABCDEFGHIJKL");
    }

    #[test]
    fn test_legacy_schema_migration() {
        // 建一张 v1 表（无 position/export/…），确认 open 能补齐并给出正确默认值
        let dir = tempfile::tempdir().unwrap();
        {
            let conn = Connection::open(dir.path().join("dxo_filters.db")).unwrap();
            conn.execute_batch(
                "CREATE TABLE filters (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    name TEXT NOT NULL, tone TEXT NOT NULL, strength REAL NOT NULL,
                    bw INTEGER NOT NULL DEFAULT 0, hash TEXT NOT NULL UNIQUE,
                    created_at TEXT NOT NULL);
                 INSERT INTO filters (name,tone,strength,bw,hash,created_at)
                 VALUES ('old','pass',1.0,0,'h1','2026-01-01T00:00:00Z');",
            )
            .unwrap();
        }
        let store = Store::open(dir.path()).unwrap();
        let list = store.list().unwrap();
        assert_eq!(list.len(), 1);
        assert_eq!(list[0].name, "old");
        assert!(list[0].export);
        assert_eq!(list[0].format, "cube");
        // v3 迁移：老记录默认不在回收站
        assert!(list[0].deleted_at.is_none());
        assert_eq!(store.count_deleted().unwrap(), 0);
    }
}
