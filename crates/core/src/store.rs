//! SQLite 存储：设置 / 已装套件 / 站点 / 证书 / 代理订阅 / 端口分配。
//! 所有敏感信息（root 密码等）只存本机。

use crate::error::{AppError, Result};
use crate::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

// 损坏的持久化站点不能被悄悄转换成默认静态站点。
fn decode_site_json<T: serde::de::DeserializeOwned>(column: usize, value: &str) -> rusqlite::Result<T> {
    serde_json::from_str(value).map_err(|error| rusqlite::Error::FromSqlConversionFailure(
        column, rusqlite::types::Type::Text, Box::new(error),
    ))
}

pub struct Store {
    pub(crate) path: PathBuf,
    conn: parking_lot::Mutex<Connection>,
}

impl Store {
    pub fn open(path: PathBuf) -> Result<Self> {
        if let Some(p) = path.parent() {
            std::fs::create_dir_all(p)?;
        }
        let conn = Connection::open(&path).map_err(AppError::from)?;
        conn.execute_batch(
            r#"
            PRAGMA journal_mode = WAL;
            CREATE TABLE IF NOT EXISTS settings(
                key TEXT PRIMARY KEY, value TEXT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS installed(
                key TEXT PRIMARY KEY,           -- "{id}@{version}"
                id TEXT NOT NULL, version TEXT NOT NULL,
                category TEXT NOT NULL,
                install_path TEXT NOT NULL, config_path TEXT NOT NULL,
                installed_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS sites(
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                domains TEXT NOT NULL,          -- JSON array
                root_dir TEXT NOT NULL,
                runtime TEXT NOT NULL,          -- JSON
                https INTEGER NOT NULL DEFAULT 0,
                rewrite TEXT NOT NULL,
                db TEXT,                        -- JSON or NULL
                php_overrides TEXT,             -- JSON or NULL（站点级 PHP 覆盖）
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS certs(
                id TEXT PRIMARY KEY, kind TEXT NOT NULL,
                subject TEXT NOT NULL, sans TEXT NOT NULL,
                not_before INTEGER NOT NULL, not_after INTEGER NOT NULL,
                cert_path TEXT NOT NULL, key_path TEXT
            );
            CREATE TABLE IF NOT EXISTS proxy_profiles(
                id TEXT PRIMARY KEY, name TEXT NOT NULL, url TEXT NOT NULL,
                active INTEGER NOT NULL DEFAULT 0, added_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS port_assign(
                service_id TEXT PRIMARY KEY, base_port INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS stacks(
                id TEXT PRIMARY KEY, name TEXT NOT NULL,
                description TEXT NOT NULL DEFAULT '',
                items TEXT NOT NULL,            -- JSON array
                builtin INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cert_automations(
                id TEXT PRIMARY KEY, data TEXT NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cert_monitors(
                id TEXT PRIMARY KEY, data TEXT NOT NULL, updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS cron_jobs(
                id TEXT PRIMARY KEY, name TEXT NOT NULL, command TEXT NOT NULL,
                interval_min INTEGER NOT NULL, enabled INTEGER NOT NULL DEFAULT 1,
                created_at INTEGER NOT NULL,
                last_run_at INTEGER, last_exit TEXT, last_output TEXT
            );
            "#,
        )?;
        // 轻量迁移：旧库补列（已存在则报错被忽略）
        let _ = conn.execute("ALTER TABLE sites ADD COLUMN php_overrides TEXT", []);
        Ok(Self {
            path: path.canonicalize()?,
            conn: parking_lot::Mutex::new(conn),
        })
    }

    /// 独立只读事务使跨表导出来自同一快照；不执行初始化 SQL，也不重入原连接锁。
    pub(crate) fn read_snapshot<T>(&self, read: impl FnOnce(&Store) -> Result<T>) -> Result<T> {
        let conn = Connection::open_with_flags(&self.path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        conn.execute_batch("BEGIN DEFERRED;")?;
        let snapshot = Self { path: self.path.clone(), conn: parking_lot::Mutex::new(conn) };
        // 连接 Drop 会结束只读事务，包括闭包失败的情况。
        read(&snapshot)
    }

    /* ---------- settings ---------- */

    pub fn get_setting(&self, key: &str) -> Option<String> {
        self.get_setting_checked(key).ok().flatten()
    }

    /// 写入前读取旧设置时，必须区分「没有设置」与读取失败。
    pub fn get_setting_checked(&self, key: &str) -> Result<Option<String>> {
        let conn = self.conn.lock();
        Ok(conn.query_row(
            "SELECT value FROM settings WHERE key=?1",
            params![key],
            |r| r.get(0),
        )
        .optional()?)
    }

    pub fn set_setting(&self, key: &str, value: &str) -> Result<()> {
        let conn = self.conn.lock();
        Self::write_setting(&conn, key, value)
    }

    fn write_setting(conn: &Connection, key: &str, value: &str) -> Result<()> {
        conn.execute(
            "INSERT INTO settings(key,value) VALUES(?1,?2)
             ON CONFLICT(key) DO UPDATE SET value=?2",
            params![key, value],
        )?;
        Ok(())
    }

    /// 在复制数据目录前把 WAL 合并回主数据库，避免迁移时遗漏最近写入。
    pub fn checkpoint(&self) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
        Ok(())
    }

    /// 在副本中修正路径；源数据库、密码及历史记录保持原样。
    pub(crate) fn snapshot_for_data_dir(source: &std::path::Path, target: &std::path::Path, rebase: &crate::paths::DataPathRebase) -> Result<()> {
        let source = Connection::open_with_flags(source, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        source.execute("VACUUM main INTO ?1", params![target.to_string_lossy()])?;
        let mut copy = Connection::open_with_flags(target, rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE)?;
        let tx = copy.transaction()?;
        fn rewrite(
            conn: &Connection, table: &str, key: &str, column: &str,
            transform: impl Fn(&str) -> Result<String>,
        ) -> Result<()> {
            // 标识符只来自下方固定列表，路径和值全部参数化。
            let values = conn.prepare(&format!("SELECT {key},{column} FROM {table} WHERE {column} IS NOT NULL"))?
                .query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for (id, value) in values {
                let updated = transform(&value)?;
                if updated != value { conn.execute(&format!("UPDATE {table} SET {column}=?1 WHERE {key}=?2"), params![updated,id])?; }
            }
            Ok(())
        }
        for (table,key,column) in [("installed","key","install_path"),("installed","key","config_path"),("sites","id","root_dir"),("certs","id","cert_path"),("certs","id","key_path")] {
            rewrite(&tx, table, key, column, |value| Ok(rebase.path(value)))?;
        }
        rewrite(&tx, "sites", "id", "runtime", |value| {
            serde_json::from_str::<SiteRuntime>(value).map_err(|e| AppError::internal("校验站点运行配置",e.to_string()))?;
            let mut data: serde_json::Value = serde_json::from_str(value).map_err(|e| AppError::internal("读取站点运行配置", e.to_string()))?;
            if let Some(cwd) = data.get_mut("cwd").and_then(|v| v.as_str().map(str::to_string)) { data["cwd"] = rebase.path(&cwd).into(); }
            if let Some(command) = data.get("command").and_then(|v|v.as_str()) { data["command"] = rebase.text(command)?.into(); }
            if let Some(application) = data.get_mut("application") {
                if let Some(cwd) = application.get("cwd").and_then(|v| v.as_str()).map(str::to_owned) { application["cwd"] = rebase.path(&cwd).into(); }
                if let Some(args) = application.get_mut("args").and_then(|v| v.as_array_mut()) {
                    for arg in args { if let Some(value) = arg.as_str() { *arg = rebase.path(value).into(); } }
                }
            }
            Ok(data.to_string())
        })?;
        rewrite(&tx, "sites", "id", "php_overrides", |value| {
            let mut data: std::collections::BTreeMap<String,String> = serde_json::from_str(value).map_err(|e|AppError::internal("读取站点 PHP 配置", e.to_string()))?;
            for value in data.values_mut() { *value = rebase.path(value); }
            serde_json::to_string(&data).map_err(|e|AppError::internal("保存站点 PHP 配置",e.to_string()))
        })?;
        rewrite(&tx, "cron_jobs", "id", "command", |value| rebase.text(value))?;
        rewrite(&tx, "cert_automations", "id", "data", |value| {
            serde_json::from_str::<CertAutomation>(value).map_err(|e| AppError::internal("校验证书自动化",e.to_string()))?;
            let mut data: serde_json::Value = serde_json::from_str(value).map_err(|e|AppError::internal("读取证书自动化",e.to_string()))?;
            if data["state"] == "issuing" || data["state"] == "manual_wait" || data["state"] == "deploying" {
                return Err(AppError::new("DATA_DIR_BUSY", "证书自动化仍在执行或等待验证，请完成后再迁移"));
            }
            if let Some(targets) = data["targets"].as_array_mut() {
                for target in targets {
                    let keys: &[&str] = match target["kind"].as_str() {
                        Some("local") => &["certPath", "keyPath", "script"],
                        // SSH 登录密钥在本机；远程输出路径和远程脚本不可改写。
                        Some("ssh") => &["identityFile", "privateKey"],
                        _ => continue,
                    };
                    if let Some(config) = target["config"].as_object_mut() {
                        for &key in keys {
                            if let Some(value) = config.get_mut(key) {
                                if let Some(text) = value.as_str() { *value = if key == "script" { rebase.text(text)? } else { rebase.path(text) }.into(); }
                            }
                        }
                    }
                }
            }
            Ok(data.to_string())
        })?;
        let running: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM cron_jobs WHERE last_exit='running')", [], |r|r.get(0))?;
        if running { return Err(AppError::new("DATA_DIR_BUSY", "仍有计划任务在运行，请完成或停止后再迁移")); }
        // pathEnvDirs 是系统 PATH 旧条目的清理凭据，必须保留，不能重写为尚未应用的目录。
        tx.commit()?;
        copy.close().map_err(|(_,error)| AppError::from(error))?;
        Ok(())
    }

    /// 便捷：读取 JSON 序列化设置，缺失时用默认
    pub fn get_setting_or<T: serde::de::DeserializeOwned + Default>(&self, key: &str) -> T {
        match self.get_setting(key) {
            Some(s) => serde_json::from_str(&s).unwrap_or_default(),
            None => T::default(),
        }
    }

    pub fn set_setting_json<T: serde::Serialize>(&self, key: &str, value: &T) -> Result<()> {
        self.set_setting(
            key,
            &serde_json::to_string(value)
                .map_err(|e| AppError::internal("序列化设置", e.to_string()))?,
        )
    }

    /// 全量设置（导出配置用）
    pub fn all_settings(&self) -> Result<Vec<(String, String)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT key, value FROM settings ORDER BY key")?;
        let rows = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .filter_map(|e| e.ok())
            .collect();
        Ok(rows)
    }

    /* ---------- installed packages ---------- */

    pub fn upsert_installed(&self, p: &InstalledPackage) -> Result<()> {
        let conn = self.conn.lock();
        Self::write_installed(&conn, p)
    }

    /// 安装成功只增加可用版本；保存当前实际默认选择，不能在安装新版时隐式切换。
    /// 在提交时读取选择，保留下载期间用户的新选择；任一写入失败均整体回滚。
    pub(crate) fn complete_install(&self, p: &InstalledPackage) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let key = format!("active{}Version", p.id);
        let active: Option<String> = tx.query_row(
            "SELECT value FROM settings WHERE key=?1", params![key], |r| r.get(0),
        ).optional()?;
        let installed = {
            let mut stmt = tx.prepare("SELECT version FROM installed WHERE id=?1")?;
            let rows = stmt.query_map(params![p.id], |r| r.get::<_, String>(0))?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let chosen = active.as_deref().and_then(|version| {
            installed.iter().find(|v| {
                v.trim_start_matches(['v', 'V']) == version.trim_start_matches(['v', 'V'])
            }).map(String::as_str)
        })
            .or_else(|| installed.iter().min_by(|a, b| crate::versions::cmp_version_desc(a, b)).map(String::as_str))
            .unwrap_or(&p.version);
        Self::write_installed(&tx, p)?;
        if active.as_deref() != Some(chosen) {
            Self::write_setting(&tx, &key, chosen)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn write_installed(conn: &Connection, p: &InstalledPackage) -> Result<()> {
        conn.execute(
            "INSERT INTO installed(key,id,version,category,install_path,config_path,installed_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(key) DO UPDATE SET
               install_path=?5, config_path=?6, installed_at=?7",
            params![
                format!("{}@{}", p.id, p.version),
                p.id,
                p.version,
                p.category,
                p.install_path,
                p.config_path,
                p.installed_at
            ],
        )?;
        Ok(())
    }

    pub fn remove_installed(&self, id: &str, version: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "DELETE FROM installed WHERE id=?1 AND version=?2",
            params![id, version],
        )?;
        Ok(())
    }

    pub fn list_installed(&self) -> Result<Vec<InstalledPackage>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id,version,category,install_path,config_path,installed_at FROM installed ORDER BY id, version",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok(InstalledPackage {
                id: r.get(0)?,
                version: r.get(1)?,
                category: r.get(2)?,
                install_path: r.get(3)?,
                config_path: r.get(4)?,
                installed_at: r.get(5)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn find_installed(&self, id: &str, version: Option<&str>) -> Option<InstalledPackage> {
        self.list_installed()
            .ok()?
            .into_iter()
            .find(|p| {
                p.id == id
                    && version.map_or(true, |v| {
                        p.version.trim_start_matches(['v', 'V'])
                            == v.trim_start_matches(['v', 'V'])
                    })
            })
    }

    /* ---------- sites ---------- */

    pub fn save_site(&self, s: &Site) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO sites(id,name,domains,root_dir,runtime,https,rewrite,db,php_overrides,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11)
             ON CONFLICT(id) DO UPDATE SET
               name=?2, domains=?3, root_dir=?4, runtime=?5, https=?6,
               rewrite=?7, db=?8, php_overrides=?9, updated_at=?11",
            params![
                s.id,
                s.name,
                serde_json::to_string(&s.domains).unwrap(),
                s.root_dir,
                serde_json::to_string(&s.runtime).unwrap(),
                s.https as i32,
                serde_json::to_string(&s.rewrite).unwrap(),
                // IPC 响应隐藏密码，本机持久化仍需保留凭据供 .env 补全和重启后使用。
                s.db.as_ref().map(|d| serde_json::json!({
                    "enabled": d.enabled,
                    "database": d.database,
                    "username": d.username,
                    "password": d.password,
                    "version": d.version,
                    "port": d.port,
                }).to_string()),
                s.php_overrides.as_ref().map(|o| serde_json::to_string(o).unwrap()),
                s.created_at,
                s.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn delete_site(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM sites WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn list_sites(&self) -> Result<Vec<Site>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id,name,domains,root_dir,runtime,https,rewrite,db,php_overrides,created_at,updated_at
             FROM sites ORDER BY updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let domains: String = r.get(2)?;
            let runtime: String = r.get(4)?;
            let rewrite: String = r.get(6)?;
            let db: Option<String> = r.get(7)?;
            let php_overrides: Option<String> = r.get(8)?;
            Ok(Site {
                access_url: None,
                id: r.get(0)?,
                name: r.get(1)?,
                domains: decode_site_json(2, &domains)?,
                root_dir: r.get(3)?,
                runtime: decode_site_json(4, &runtime)?,
                https: r.get::<_, i32>(5)? != 0,
                rewrite: decode_site_json(6, &rewrite)?,
                db: db.as_deref().map(|value| decode_site_json(7, value)).transpose()?,
                php_overrides: php_overrides.as_deref().map(|value| decode_site_json(8, value)).transpose()?,
                status: "running".into(),
                created_at: r.get(9)?,
                updated_at: r.get(10)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /* ---------- certs ---------- */

    pub fn save_cert(&self, c: &CertRecord) -> Result<()> {
        let conn = self.conn.lock();
        Self::write_cert(&conn, c)
    }

    /// 一组实际证书输出与记录一起提交，同路径不保留失效的自签/ACME 元数据。
    pub(crate) fn replace_managed_certs(&self, records: &[CertRecord]) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        for record in records {
            if !matches!(record.kind.as_str(), "site" | "acme") {
                return Err(AppError::new("CERT_KIND", "仅可替换本地站点或 ACME 证书记录"));
            }
            tx.execute("DELETE FROM certs WHERE cert_path=?1 AND kind IN ('site','acme')", params![record.cert_path])?;
        }
        for record in records {
            Self::write_cert(&tx, record)?;
        }
        tx.commit()?;
        Ok(())
    }

    fn write_cert(conn: &Connection, c: &CertRecord) -> Result<()> {
        conn.execute(
            "INSERT INTO certs(id,kind,subject,sans,not_before,not_after,cert_path,key_path)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET
               kind=?2, subject=?3, sans=?4, not_before=?5, not_after=?6, cert_path=?7, key_path=?8",
            params![
                c.id,
                c.kind,
                c.subject,
                serde_json::to_string(&c.sans).unwrap(),
                c.not_before,
                c.not_after,
                c.cert_path,
                c.key_path
            ],
        )?;
        Ok(())
    }

    pub fn delete_cert(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM certs WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn list_certs(&self) -> Result<Vec<CertRecord>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id,kind,subject,sans,not_before,not_after,cert_path,key_path FROM certs",
        )?;
        let rows = stmt.query_map([], |r| {
            let sans: String = r.get(3)?;
            Ok(CertRecord {
                id: r.get(0)?,
                kind: r.get(1)?,
                subject: r.get(2)?,
                sans: serde_json::from_str(&sans).unwrap_or_default(),
                not_before: r.get(4)?,
                not_after: r.get(5)?,
                cert_path: r.get(6)?,
                key_path: r.get(7)?,
                trusted: None,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /* ---------- proxy profiles ---------- */

    pub fn save_proxy_profile(&self, id: &str, name: &str, url: &str, active: bool) -> Result<()> {
        let conn = self.conn.lock();
        if active {
            conn.execute("UPDATE proxy_profiles SET active=0", [])?;
        }
        conn.execute(
            "INSERT INTO proxy_profiles(id,name,url,active,added_at)
             VALUES(?1,?2,?3,?4,?5)
             ON CONFLICT(id) DO UPDATE SET name=?2, url=?3, active=?4",
            params![
                id,
                name,
                url,
                active as i32,
                chrono::Utc::now().timestamp_millis()
            ],
        )?;
        Ok(())
    }

    pub fn delete_proxy_profile(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        let active: Option<i32> = conn
            .query_row(
                "SELECT active FROM proxy_profiles WHERE id=?1",
                params![id],
                |row| row.get(0),
            )
            .optional()?;
        if active.is_none() {
            return Err(AppError::new("PROFILE_NOT_FOUND", "代理订阅不存在"));
        }
        if active == Some(1) {
            return Err(AppError::new(
                "PROFILE_ACTIVE",
                "当前订阅正在使用，请先切换到其它订阅",
            ));
        }
        conn.execute("DELETE FROM proxy_profiles WHERE id=?1", params![id])?;
        Ok(())
    }

    pub fn set_active_proxy_profile(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        let exists: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM proxy_profiles WHERE id=?1)", params![id], |r| r.get(0))?;
        if !exists {
            return Err(AppError::new("PROFILE_NOT_FOUND", "代理订阅不存在"));
        }
        tx.execute("UPDATE proxy_profiles SET active=0", [])?;
        tx.execute(
            "UPDATE proxy_profiles SET active=1 WHERE id=?1",
            params![id],
        )?;
        tx.commit()?;
        Ok(())
    }

    /* ---------- cron（计划任务，任务本体在 cron 模块） ---------- */

    pub fn list_cron_jobs(&self) -> Result<Vec<crate::cron::CronJob>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id,name,command,interval_min,enabled,created_at,last_run_at,last_exit,last_output FROM cron_jobs ORDER BY created_at DESC")?;
        let rows = stmt.query_map([], Self::cron_row)?;
        Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
    }

    fn cron_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<crate::cron::CronJob> {
        Ok(crate::cron::CronJob {
            id: r.get(0)?,
            name: r.get(1)?,
            command: r.get(2)?,
            interval_min: r.get(3)?,
            enabled: r.get::<_, i64>(4)? != 0,
            created_at: r.get(5)?,
            last_run_at: r.get(6)?,
            last_exit: r.get(7)?,
            last_output: r.get(8)?,
        })
    }

    pub fn get_cron_job(&self, id: &str) -> Result<Option<crate::cron::CronJob>> {
        Ok(self.conn.lock().query_row(
            "SELECT id,name,command,interval_min,enabled,created_at,last_run_at,last_exit,last_output FROM cron_jobs WHERE id=?1",
            params![id], Self::cron_row,
        ).optional()?)
    }

    /// 新建或编辑定义；执行中的任务不允许修改，历史结果只由执行器写入。
    pub fn save_cron_job(&self, job: &crate::cron::CronJob, create: bool) -> Result<()> {
        crate::cron::validate_job(job)?;
        let conn = self.conn.lock();
        let changed = if create {
            conn.execute("INSERT INTO cron_jobs(id,name,command,interval_min,enabled,created_at) VALUES(?1,?2,?3,?4,?5,?6)",
                params![job.id, job.name, job.command, job.interval_min, job.enabled as i32, job.created_at])?
        } else {
            conn.execute("UPDATE cron_jobs SET name=?2,command=?3,interval_min=?4 WHERE id=?1 AND last_exit IS NOT 'running'",
                params![job.id, job.name, job.command, job.interval_min])?
        };
        if changed == 0 {
            return Err(AppError::new(
                "CRON_NOT_EDITABLE",
                "任务不存在或正在运行，请刷新列表或停止后再编辑",
            ));
        }
        Ok(())
    }

    pub fn delete_cron_job(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        let changed = conn.execute(
            "DELETE FROM cron_jobs WHERE id=?1 AND last_exit IS NOT 'running'",
            params![id],
        )?;
        if changed == 0 {
            return Err(AppError::new(
                "CRON_NOT_REMOVABLE",
                "任务不存在或仍在运行，请刷新列表或先停止任务",
            ));
        }
        Ok(())
    }

    pub fn set_cron_enabled(&self, id: &str, enabled: bool) -> Result<()> {
        let conn = self.conn.lock();
        let changed = conn.execute(
            "UPDATE cron_jobs SET enabled=?2 WHERE id=?1",
            params![id, enabled as i32],
        )?;
        if changed == 0 {
            return Err(AppError::new(
                "CRON_NOT_FOUND",
                "计划任务不存在，请刷新列表",
            ));
        }
        Ok(())
    }

    /// 在同一写事务内读定义、复核到期条件并占用任务，阻止手动/自动/跨连接重复执行。
    pub(crate) fn claim_cron_run(
        &self,
        id: &str,
        manual: bool,
        now: i64,
    ) -> Result<Option<crate::cron::CronJob>> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let mut job = tx.query_row(
            "SELECT id,name,command,interval_min,enabled,created_at,last_run_at,last_exit,last_output FROM cron_jobs WHERE id=?1",
            params![id], Self::cron_row,
        ).optional()?.ok_or_else(|| AppError::new("CRON_NOT_FOUND", "计划任务不存在，请刷新列表"))?;
        crate::cron::validate_job(&job)?;
        if job.last_exit.as_deref() == Some(crate::cron::RUNNING) {
            return Err(AppError::new(
                "CRON_BUSY",
                "该计划任务正在运行，请等待或停止当前任务",
            ));
        }
        if !manual && (!job.enabled || !crate::cron::is_due(&job, now)) {
            return Ok(None);
        }
        tx.execute("UPDATE cron_jobs SET last_run_at=?2, last_exit='running', last_output=NULL WHERE id=?1", params![id, now])?;
        tx.commit()?;
        job.last_run_at = Some(now);
        job.last_exit = Some(crate::cron::RUNNING.into());
        job.last_output = None;
        Ok(Some(job))
    }

    pub(crate) fn finish_cron_run(
        &self,
        id: &str,
        started: i64,
        exit: &str,
        output: &str,
    ) -> Result<()> {
        let changed = self.conn.lock().execute(
            "UPDATE cron_jobs SET last_exit=?3,last_output=?4 WHERE id=?1 AND last_run_at=?2 AND last_exit='running'",
            params![id, started, exit, output],
        )?;
        if changed == 0 {
            return Err(AppError::new(
                "CRON_RESULT_CHANGED",
                "任务结果已变化，未覆盖其它执行结果",
            ));
        }
        Ok(())
    }

    /// 仅在执行锁确认无人持有后恢复；暂停自动调度，避免不确定的上次命令被重复执行。
    pub(crate) fn recover_cron_run(&self, id: &str, started: Option<i64>) -> Result<()> {
        self.conn.lock().execute(
            "UPDATE cron_jobs SET last_exit='interrupted', enabled=0, last_output=?3
             WHERE id=?1 AND last_run_at IS ?2 AND last_exit='running'",
            params![id, started, "上次执行因应用退出或异常而中断，无法确认是否完成。自动调度已暂停，请检查命令影响后重新启用。"],
        )?;
        Ok(())
    }

    pub fn list_proxy_profiles(&self) -> Result<Vec<(String, String, String, bool, i64)>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id,name,url,active,added_at FROM proxy_profiles ORDER BY added_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get::<_, i32>(3)? != 0,
                r.get(4)?,
            ))
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    /* ---------- 端口分配（php-cgi 池） ---------- */

    pub fn get_port_assign(&self, service_id: &str) -> Option<u16> {
        self.get_port_assign_checked(service_id).ok().flatten()
    }

    pub(crate) fn get_port_assign_checked(&self, service_id: &str) -> Result<Option<u16>> {
        let conn = self.conn.lock();
        let value = conn.query_row(
            "SELECT base_port FROM port_assign WHERE service_id=?1",
            params![service_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()?;
        value.map(|value| u16::try_from(value).ok().filter(|port| *port > 0)
            .ok_or_else(|| AppError::new("BAD_PORT", format!("服务 {service_id} 的分配端口无效"))))
            .transpose()
    }

    pub fn set_port_assign(&self, service_id: &str, base_port: u16) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO port_assign(service_id,base_port) VALUES(?1,?2)
             ON CONFLICT(service_id) DO UPDATE SET base_port=?2",
            params![service_id, base_port as i64],
        )?;
        Ok(())
    }

    pub fn all_port_assigns(&self) -> Vec<(String, u16)> {
        let conn = match self.conn.try_lock_for(std::time::Duration::from_secs(2)) {
            Some(guard) => guard,
            None => return Vec::new(),
        };
        let Ok(mut stmt) = conn.prepare("SELECT service_id,base_port FROM port_assign") else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? as u16))
        }) else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok()).collect()
    }

    /// 通用服务完成配置准备后再保存端口；回落覆盖与分配记录必须一致。
    pub(crate) fn save_generic_port(&self, service_id: &str, port: u16, fallback: bool) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction()?;
        tx.execute("INSERT INTO port_assign(service_id,base_port) VALUES(?1,?2) ON CONFLICT(service_id) DO UPDATE SET base_port=?2", params![service_id, port])?;
        if fallback {
            tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2",
                params![format!("portOverride.{service_id}"), port.to_string()])?;
        }
        tx.commit()?;
        Ok(())
    }

    /* ---------- 端口覆盖（设置 → 端口） ---------- */

    /// 用户在设置里逐个改过的端口。存成 `portOverride.{key}` 单键，
    /// 键名与 PortsProfile 的字段一一对应（http/https/mysql/redis/…）。
    pub fn port_overrides(&self) -> Vec<(String, String)> {
        let conn = self.conn.lock();
        let Ok(mut stmt) =
            conn.prepare("SELECT key, value FROM settings WHERE key LIKE 'portOverride.%'")
        else {
            return Vec::new();
        };
        let Ok(rows) = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        else {
            return Vec::new();
        };
        rows.filter_map(|r| r.ok())
            .map(|(k, v)| (k.trim_start_matches("portOverride.").to_string(), v))
            .collect()
    }

    pub fn set_port_override(&self, key: &str, port: Option<u16>) -> Result<()> {
        match port {
            Some(p) => self.set_setting(&format!("portOverride.{key}"), &p.to_string()),
            None => {
                let conn = self.conn.lock();
                conn.execute(
                    "DELETE FROM settings WHERE key=?1",
                    params![format!("portOverride.{key}")],
                )?;
                Ok(())
            }
        }
    }

    /* ---------- 服务栈 ---------- */

    pub fn list_stacks(&self) -> Result<Vec<Stack>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare(
            "SELECT id,name,description,items,builtin,created_at,updated_at FROM stacks ORDER BY builtin DESC, updated_at DESC",
        )?;
        let rows = stmt.query_map([], |r| {
            let items: String = r.get(3)?;
            Ok(Stack {
                id: r.get(0)?,
                name: r.get(1)?,
                description: r.get(2)?,
                items: serde_json::from_str(&items).unwrap_or_default(),
                builtin: r.get::<_, i32>(4)? != 0,
                created_at: r.get(5)?,
                updated_at: r.get(6)?,
            })
        })?;
        let mut out = Vec::new();
        for row in rows {
            out.push(row?);
        }
        Ok(out)
    }

    pub fn get_stack(&self, id: &str) -> Result<Option<Stack>> {
        Ok(self.list_stacks()?.into_iter().find(|s| s.id == id))
    }

    pub fn save_stack(&self, s: &Stack) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO stacks(id,name,description,items,builtin,created_at,updated_at)
             VALUES(?1,?2,?3,?4,?5,?6,?7)
             ON CONFLICT(id) DO UPDATE SET name=?2, description=?3, items=?4, updated_at=?7",
            params![
                s.id,
                s.name,
                s.description,
                serde_json::to_string(&s.items).unwrap_or_else(|_| "[]".into()),
                s.builtin as i32,
                s.created_at,
                s.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn delete_stack(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let removed = tx.execute("DELETE FROM stacks WHERE id=?1 AND builtin=0", params![id])?;
        if removed > 0 {
            tx.execute("UPDATE settings SET value='' WHERE key='startStackOnLaunch' AND value=?1", params![id])?;
        }
        tx.commit()?;
        Ok(())
    }

    /// 选择与删除共用事务边界，避免保存刚被删除的启动服务栈。
    pub fn set_start_stack_on_launch(&self, id: &str) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        if !id.is_empty() && !tx.query_row("SELECT EXISTS(SELECT 1 FROM stacks WHERE id=?1)", params![id], |row| row.get::<_, bool>(0))? {
            return Err(AppError::new("STACK_NOT_FOUND", "所选服务栈已删除，请重新选择"));
        }
        Self::write_setting(&tx, "startStackOnLaunch", id)?;
        tx.commit()?;
        Ok(())
    }

    /* ---------- 证书自动化（ACME 签发/续签/部署） ---------- */

    pub fn save_cert_automation(&self, a: &CertAutomation) -> Result<()> {
        let data = serde_json::to_string(a).map_err(|e| AppError::internal("保存证书自动化", e.to_string()))?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO cert_automations(id,data,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET data=?2, updated_at=?3",
            params![
                a.id,
                data,
                a.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn list_cert_automations(&self) -> Result<Vec<CertAutomation>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT id,data FROM cert_automations ORDER BY updated_at DESC")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        rows.into_iter().map(|(id, data)| Self::decode_cert_automation(&id, &data)).collect()
    }

    fn decode_cert_automation(id: &str, data: &str) -> Result<CertAutomation> {
        let a: CertAutomation = serde_json::from_str(data).map_err(|_| AppError::new(
            "CERT_AUTO_CORRUPT", format!("证书自动化 {id} 的保存数据损坏，未隐藏或覆盖该记录")))?;
        if a.id != id { return Err(AppError::new("CERT_AUTO_CORRUPT", "证书自动化记录标识不一致，请检查备份或修复记录")); }
        Ok(a)
    }

    pub fn get_cert_automation(&self, id: &str) -> Result<Option<CertAutomation>> {
        let data: Option<String> = self.conn.lock().query_row("SELECT data FROM cert_automations WHERE id=?1", params![id], |r| r.get(0)).optional()?;
        data.map(|data| Self::decode_cert_automation(id, &data)).transpose()
    }

    pub fn delete_cert_automation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        if conn.execute("DELETE FROM cert_automations WHERE id=?1", params![id])? == 0 {
            return Err(AppError::new("NOT_FOUND", "证书自动化不存在，未删除其它数据"));
        }
        Ok(())
    }

    /* ---------- 网站证书监控 ---------- */

    pub fn save_cert_monitor(&self, m: &CertMonitor) -> Result<()> {
        let data = serde_json::to_string(m).map_err(|e| AppError::internal("保存证书监控", e.to_string()))?;
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO cert_monitors(id,data,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET data=?2, updated_at=?3",
            params![
                m.id,
                data,
                m.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn list_cert_monitors(&self) -> Result<Vec<CertMonitor>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT id,data FROM cert_monitors ORDER BY id")?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut list = rows.into_iter().map(|(id, data)| Self::decode_cert_monitor(&id, &data)).collect::<Result<Vec<_>>>()?;
        list.sort_by_key(|m| std::cmp::Reverse(m.created_at));
        Ok(list)
    }

    pub fn get_cert_monitor(&self, id: &str) -> Result<Option<CertMonitor>> {
        let data: Option<String> = self.conn.lock().query_row("SELECT data FROM cert_monitors WHERE id=?1", params![id], |r| r.get(0)).optional()?;
        data.map(|data| Self::decode_cert_monitor(id, &data)).transpose()
    }

    fn decode_cert_monitor(id: &str, data: &str) -> Result<CertMonitor> {
        let m: CertMonitor = serde_json::from_str(data).map_err(|_| AppError::new("MONITOR_CORRUPT", format!("证书监控 {id} 的保存数据损坏，请检查备份")))?;
        if m.id != id { return Err(AppError::new("MONITOR_CORRUPT", "证书监控标识不一致，未隐藏或覆盖该记录")); }
        Ok(m)
    }

    /// 在写事务内按规范化端点去重；不同窗口/进程同时新增也只保留一条。
    pub(crate) fn create_cert_monitor(&self, m: &CertMonitor) -> Result<()> {
        let mut conn = self.conn.lock();
        let tx = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
        let rows = tx.prepare("SELECT id,data FROM cert_monitors")?.query_map([], |r|
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
        for (id, data) in rows {
            let old = Self::decode_cert_monitor(&id, &data)?;
            if crate::certmonitor::normalize_endpoint(&old.host, old.port).ok() == Some((m.host.clone(), m.port)) {
                return Err(AppError::new("MONITOR_EXISTS", "此地址和端口已在监控列表中，请使用原条目的立即检查"));
            }
        }
        let data = serde_json::to_string(m).map_err(|e| AppError::internal("保存证书监控", e.to_string()))?;
        tx.execute("INSERT INTO cert_monitors(id,data,updated_at) VALUES(?1,?2,?3)", params![m.id,data,m.updated_at])?;
        tx.commit()?;
        Ok(())
    }

    /// 网络探测完成后只更新原版本，删除或编辑之后的迟到结果不能复活/覆盖记录。
    pub(crate) fn complete_cert_monitor(&self, m: &CertMonitor, expected: i64) -> Result<bool> {
        let data = serde_json::to_string(m).map_err(|e| AppError::internal("保存证书监控结果", e.to_string()))?;
        Ok(self.conn.lock().execute("UPDATE cert_monitors SET data=?1,updated_at=?2 WHERE id=?3 AND updated_at=?4",
            params![data,m.updated_at,m.id,expected])? == 1)
    }

    pub(crate) fn save_monitor_notifications(&self, kind: &str, url: &str) -> Result<()> {
        let mut conn = self.conn.lock(); let tx = conn.transaction()?;
        for (key, value) in [("monitorNotifyKind",kind),("monitorNotifyUrl",url)] {
            tx.execute("INSERT INTO settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=?2", params![key,value])?;
        }
        tx.commit()?; Ok(())
    }

    pub fn delete_cert_monitor(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        if conn.execute("DELETE FROM cert_monitors WHERE id=?1", params![id])? == 0 {
            return Err(AppError::new("NOT_FOUND", "监控已不存在，请刷新列表"));
        }
        Ok(())
    }
}
