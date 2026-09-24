//! 应用数据目录布局：
//! {base}/runtimes/{id}/{version}/   运行时（解压产物）
//! {base}/etc/{id}/{version}/        每版本配置
//! {base}/data/{id}/                 服务数据（MySQL datadir、Redis RDB）
//! {base}/logs/{service}/out.log     服务日志
//! {base}/certs/                     CA 与站点证书
//! {base}/downloads/                 下载缓存（断点续传）
//! {base}/backup/                    配置修改前的自动备份
//! {base}/nsb.sqlite                 站点/套件/证书/设置

use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
pub struct Paths {
    pub base: PathBuf,
}

impl Paths {
    /// 数据目录解析优先级：
    /// 1. 显式 base（冒烟测试等）
    /// 2. NSB_HOME 环境变量（便携化/调试覆盖）
    /// 3. 安装版：{exe 所在目录}/nsb-data —— 数据跟随安装位置，可整目录迁移
    /// 4. 开发环境（cargo target 下运行）：LocalAppData，避免 cargo clean 清掉数据
    /// 相对路径统一锚定到当前目录（子进程 cwd 各异，绝不能把相对路径写进配置/参数）
    pub fn resolve(base: Option<PathBuf>) -> PathBuf {
        let absolutize = |p: PathBuf| {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            }
        };
        if let Some(p) = base {
            return absolutize(p);
        }
        if let Ok(env) = std::env::var("NSB_HOME") {
            if !env.trim().is_empty() {
                return absolutize(PathBuf::from(env));
            }
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                let s = dir.to_string_lossy().to_lowercase().replace('\\', "/");
                let is_dev = s.contains("/target/debug") || s.contains("/target/release");
                if !is_dev {
                    let candidate = absolutize(dir.join("nsb-data"));
                    // 写权限探测（用户可能装到 Program Files 等只读位置）
                    if std::fs::create_dir_all(&candidate).is_ok() {
                        let probe = candidate.join(".write-probe");
                        if std::fs::write(&probe, b"ok").is_ok() {
                            let _ = std::fs::remove_file(&probe);
                            return candidate;
                        }
                    }
                }
            }
        }
        // 产品改名（NiceServBay → NiceEnv）：把旧数据目录整体迁过来，
        // 设置/已装套件/站点注册全都无缝带走；迁移失败（如被占用）就沿用旧目录
        let data_local = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        let base = data_local.join("NiceEnv");
        if !base.exists() {
            // "niceEnv" 是改名中途短暂的拼写，一并兼容
            for legacy_name in ["NiceServBay", "niceEnv"] {
                let legacy = data_local.join(legacy_name);
                if legacy.exists() {
                    if std::fs::rename(&legacy, &base).is_ok() {
                        break;
                    }
                    return legacy; // 迁移失败（如被占用）：沿用旧目录，保证还能读到数据
                }
            }
        }
        base
    }

    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    pub fn ensure_dirs(&self) -> std::io::Result<()> {
        for d in [
            self.base.clone(),
            self.runtimes(),
            self.etc(),
            self.data(),
            self.logs(),
            self.certs(),
            self.downloads(),
            self.backup(),
            self.etc().join("nginx").join("sites"),
            self.etc().join("nginx").join("rewrites"),
            self.etc().join("nginx").join("temp"),
            self.etc().join("php"),
            self.etc().join("mysql"),
            self.etc().join("redis"),
            self.etc().join("mihomo"),
            self.certs().join("sites"),
            self.etc().join("apache").join("sites"),
            self.etc().join("apache").join("run"),
            self.etc().join("apache").join("logs"),
        ] {
            std::fs::create_dir_all(d)?;
        }
        Ok(())
    }

    pub fn runtimes(&self) -> PathBuf {
        self.base.join("runtimes")
    }
    pub fn etc(&self) -> PathBuf {
        self.base.join("etc")
    }
    pub fn data(&self) -> PathBuf {
        self.base.join("data")
    }
    pub fn logs(&self) -> PathBuf {
        self.base.join("logs")
    }
    pub fn certs(&self) -> PathBuf {
        self.base.join("certs")
    }
    pub fn downloads(&self) -> PathBuf {
        self.base.join("downloads")
    }
    pub fn backup(&self) -> PathBuf {
        self.base.join("backup")
    }
    pub fn db(&self) -> PathBuf {
        self.base.join("nsb.sqlite")
    }

    pub fn runtime_dir(&self, id: &str, version: &str) -> PathBuf {
        self.runtimes().join(id).join(version)
    }
    pub fn etc_dir(&self, id: &str, version: &str) -> PathBuf {
        self.etc().join(id).join(version)
    }

    pub fn nginx_conf(&self) -> PathBuf {
        self.etc().join("nginx").join("nginx.conf")
    }
    pub fn nginx_sites_dir(&self) -> PathBuf {
        self.etc().join("nginx").join("sites")
    }
    pub fn php_ini(&self, version: &str) -> PathBuf {
        self.etc().join("php").join(version).join("php.ini")
    }
    pub fn mysql_ini(&self, version: &str) -> PathBuf {
        self.etc().join("mysql").join(version).join("my.ini")
    }
    pub fn mysql_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("mysql").join(version)
    }
    pub fn redis_conf(&self, version: &str) -> PathBuf {
        self.etc().join("redis").join(version).join("redis.conf")
    }
    pub fn redis_data_dir(&self) -> PathBuf {
        self.data().join("redis")
    }
    pub fn mihomo_dir(&self) -> PathBuf {
        self.etc().join("mihomo")
    }
    pub fn mihomo_config(&self) -> PathBuf {
        self.mihomo_dir().join("config.yaml")
    }
    /* ---------- Apache ---------- */
    pub fn apache_conf(&self) -> PathBuf {
        self.etc().join("apache").join("httpd.conf")
    }
    pub fn apache_sites_dir(&self) -> PathBuf {
        self.etc().join("apache").join("sites")
    }
    pub fn apache_run_dir(&self) -> PathBuf {
        self.etc().join("apache").join("run")
    }
    /* ---------- PostgreSQL / MongoDB（按版本隔离：跨大版本数据文件不兼容） ---------- */
    pub fn postgres_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("postgresql").join(version)
    }
    pub fn mongo_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("mongodb").join(version)
    }
    pub fn service_log(&self, service_id: &str) -> PathBuf {
        self.logs().join(service_id.replace(['@', ':'], "_")).join("out.log")
    }
}

/// 写文件前把旧内容备份到 {base}/backup/
pub fn write_with_backup(path: &Path, content: &str, backup_dir: &Path) -> std::io::Result<()> {
    if let Some(name) = path.file_name().and_then(|n| n.to_str()) {
        if path.exists() {
            let ts = chrono::Local::now().format("%Y%m%d-%H%M%S");
            let bak = backup_dir.join(format!("{name}.{ts}.bak"));
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let _ = std::fs::copy(path, bak);
        }
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, content)
}

/// Windows 路径转 nginx 正斜杠形式
pub fn nginx_path(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// 列出备份目录里的备份文件（新→旧）。返回 (文件名, 完整路径, 字节数, 修改时间 ms)
pub fn list_backups(base: &Path) -> Vec<(String, String, u64, i64)> {
    let dir = base.join("backup");
    let mut out: Vec<(String, String, u64, i64)> = Vec::new();
    let Ok(rd) = std::fs::read_dir(&dir) else {
        return out;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if !p.is_file() {
            continue;
        }
        let Some(name) = p.file_name().map(|n| n.to_string_lossy().to_string()) else {
            continue;
        };
        let meta = e.metadata().ok();
        let size = meta.as_ref().map(|m| m.len()).unwrap_or(0);
        let mtime = meta
            .and_then(|m| m.modified().ok())
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_millis() as i64)
            .unwrap_or(0);
        out.push((name, p.to_string_lossy().to_string(), size, mtime));
    }
    out.sort_by(|a, b| b.3.cmp(&a.3));
    out
}

/// 从备份文件恢复。备份名形如 `{原文件名}.{时间戳}.bak`。
/// 恢复目标在当前配置树（etc/）里按文件名查找，避免被诱导写到任意路径。
pub fn restore_backup(base: &Path, backup_name: &str) -> std::io::Result<PathBuf> {
    let src = base.join("backup").join(backup_name);
    if !src.is_file() {
        return Err(std::io::Error::new(
            std::io::ErrorKind::NotFound,
            "备份文件不存在",
        ));
    }
    // foo.conf.20260101-120000.bak → foo.conf
    let orig = backup_name
        .strip_suffix(".bak")
        .and_then(|s| s.rsplit_once('.').map(|(a, _)| a))
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::InvalidInput, "备份文件名无法解析")
        })?;

    // 在 etc/ 下递归查找目标配置文件
    let etc = base.join("etc");
    let mut target: Option<PathBuf> = None;
    let mut stack = vec![etc.clone()];
    while let Some(dir) = stack.pop() {
        let Ok(rd) = std::fs::read_dir(&dir) else { continue };
        for e in rd.filter_map(|e| e.ok()) {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else if p.file_name().map(|n| n.to_string_lossy() == orig).unwrap_or(false) {
                target = Some(p);
                break;
            }
        }
        if target.is_some() {
            break;
        }
    }
    let target = target.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::NotFound,
            format!("找不到 {orig} 对应的当前配置文件，无法恢复"),
        )
    })?;
    // 恢复前把当前版本再备份一次，保证可回退
    let content = std::fs::read_to_string(&src)?;
    write_with_backup(&target, &content, &base.join("backup"))?;
    Ok(target)
}
