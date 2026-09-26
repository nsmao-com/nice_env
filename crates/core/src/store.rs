//! SQLite 存储：设置 / 已装套件 / 站点 / 证书 / 代理订阅 / 端口分配。
//! 所有敏感信息（root 密码等）只存本机。

use crate::error::{AppError, Result};
use crate::model::*;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::PathBuf;

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
            .find(|p| p.id == id && version.map_or(true, |v| p.version == v))
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
            let php_overrides: Option<String> = r.get(8).ok().flatten();
            Ok(Site {
                id: r.get(0)?,
                name: r.get(1)?,
                domains: serde_json::from_str(&domains).unwrap_or_default(),
                root_dir: r.get(3)?,
                runtime: serde_json::from_str(&runtime).unwrap_or(SiteRuntime {
                    imported_cert_id: None,
                    web_server: "nginx".into(),
                    kind: SiteKind::Static,
                    php_version: None,
                    proxy_target: None,
                    command: None,
                    cwd: None,
                }),
                https: r.get::<_, i32>(5)? != 0,
                rewrite: serde_json::from_str(&rewrite).unwrap_or_default(),
                db: db.and_then(|d| serde_json::from_str(&d).ok()),
                php_overrides: php_overrides.and_then(|o| serde_json::from_str(&o).ok()),
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
        conn.execute(
            "INSERT INTO certs(id,kind,subject,sans,not_before,not_after,cert_path,key_path)
             VALUES(?1,?2,?3,?4,?5,?6,?7,?8)
             ON CONFLICT(id) DO UPDATE SET
               subject=?3, sans=?4, not_before=?5, not_after=?6, cert_path=?7, key_path=?8",
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
        let conn = self.conn.lock();
        conn.query_row(
            "SELECT base_port FROM port_assign WHERE service_id=?1",
            params![service_id],
            |r| r.get::<_, i64>(0),
        )
        .optional()
        .ok()
        .flatten()
        .map(|v| v as u16)
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
        let conn = self.conn.lock();
        conn.execute("DELETE FROM stacks WHERE id=?1 AND builtin=0", params![id])?;
        Ok(())
    }

    /* ---------- 证书自动化（ACME 签发/续签/部署） ---------- */

    pub fn save_cert_automation(&self, a: &CertAutomation) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO cert_automations(id,data,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET data=?2, updated_at=?3",
            params![
                a.id,
                serde_json::to_string(a).unwrap_or_else(|_| "{}".into()),
                a.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn list_cert_automations(&self) -> Result<Vec<CertAutomation>> {
        let conn = self.conn.lock();
        let mut stmt =
            conn.prepare("SELECT data FROM cert_automations ORDER BY updated_at DESC")?;
        let list = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        Ok(list)
    }

    pub fn get_cert_automation(&self, id: &str) -> Result<Option<CertAutomation>> {
        Ok(self
            .list_cert_automations()?
            .into_iter()
            .find(|a| a.id == id))
    }

    pub fn delete_cert_automation(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM cert_automations WHERE id=?1", params![id])?;
        Ok(())
    }

    /* ---------- 网站证书监控 ---------- */

    pub fn save_cert_monitor(&self, m: &CertMonitor) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute(
            "INSERT INTO cert_monitors(id,data,updated_at) VALUES(?1,?2,?3)
             ON CONFLICT(id) DO UPDATE SET data=?2, updated_at=?3",
            params![
                m.id,
                serde_json::to_string(m).unwrap_or_else(|_| "{}".into()),
                m.updated_at
            ],
        )?;
        Ok(())
    }

    pub fn list_cert_monitors(&self) -> Result<Vec<CertMonitor>> {
        let conn = self.conn.lock();
        let mut stmt = conn.prepare("SELECT data FROM cert_monitors ORDER BY updated_at DESC")?;
        let list = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .filter_map(|r| r.ok())
            .filter_map(|s| serde_json::from_str(&s).ok())
            .collect();
        Ok(list)
    }

    pub fn get_cert_monitor(&self, id: &str) -> Result<Option<CertMonitor>> {
        Ok(self.list_cert_monitors()?.into_iter().find(|m| m.id == id))
    }

    pub fn delete_cert_monitor(&self, id: &str) -> Result<()> {
        let conn = self.conn.lock();
        conn.execute("DELETE FROM cert_monitors WHERE id=?1", params![id])?;
        Ok(())
    }
}
