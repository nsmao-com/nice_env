//! 应用数据目录布局：
//! {base}/runtimes/{id}/{version}/   运行时（解压产物）
//! {base}/etc/{id}/{version}/        每版本配置
//! {base}/data/{id}/                 服务数据（MySQL datadir、Redis RDB）
//! {base}/logs/{service}/out.log     服务日志
//! {base}/certs/                     CA 与站点证书
//! {base}/downloads/                 下载缓存（断点续传）
//! {base}/backup/                    配置修改前的自动备份
//! {base}/nsb.sqlite                 站点/套件/证书/设置

use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug)]
pub struct Paths {
    pub base: PathBuf,
}

impl Paths {
    /// 数据目录解析优先级：
    /// 1. 显式 base（冒烟测试等）
    /// 2. NSB_HOME 环境变量（便携化/调试覆盖）
    /// 3. 用户在设置中确认的数据目录（独立于数据目录保存）
    /// 4. Windows 安装版：{exe 所在目录}/nsb-data；其它平台只兼容已有的邻接数据
    /// 5. macOS / 开发环境：用户应用数据目录，避免应用更新或 cargo clean 清掉数据
    /// 相对路径统一锚定到当前目录（子进程 cwd 各异，绝不能把相对路径写进配置/参数）
    pub fn resolve(base: Option<PathBuf>) -> crate::error::Result<PathBuf> {
        let absolutize = |p: PathBuf| {
            std::path::absolute(p).map_err(|e| crate::error::AppError::io("解析数据目录", e))
        };
        if let Some(p) = base {
            return absolutize(p);
        }
        if let Ok(env) = std::env::var("NSB_HOME") {
            if !env.trim().is_empty() {
                return absolutize(PathBuf::from(env));
            }
        }
        if let Some(selected) = read_data_dir_selection(&data_dir_selection_file()?)? {
            return Ok(selected);
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(candidate) = adjacent_data_dir(&exe, crate::install::current_os())? {
                return absolutize(candidate);
            }
        }
        // 产品改名（NiceServBay → NiceEnv）：把旧数据目录整体迁过来，
        // 设置/已装套件/站点注册全都无缝带走；迁移失败（如被占用）就沿用旧目录
        let data_local = dirs::data_local_dir().ok_or_else(|| {
            crate::error::AppError::new("DATA_DIR_UNAVAILABLE", "无法确定用户数据目录")
                .with_hint("请恢复系统用户目录，或通过 NSB_HOME 指定数据目录。")
        })?;
        let base = data_local.join("NiceEnv");
        if !base.exists() {
            // "niceEnv" 是改名中途短暂的拼写，一并兼容
            for legacy_name in ["NiceServBay", "niceEnv"] {
                let legacy = data_local.join(legacy_name);
                if legacy.exists() {
                    if std::fs::rename(&legacy, &base).is_ok() {
                        break;
                    }
                    return Ok(legacy); // 迁移失败（如被占用）：沿用旧目录，保证还能读到数据
                }
            }
        }
        Ok(base)
    }

    pub fn new(base: PathBuf) -> Self {
        Self { base }
    }

    /// 更新会替换 macOS 应用包；旧版包内的数据必须先通过设置中的迁移流程移出。
    pub fn ensure_safe_app_update(&self) -> crate::error::Result<()> {
        validate_update_data_location(&self.base, crate::install::current_os())
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
            self.etc().join("postgresql"),
            self.etc().join("caddy").join("sites"),
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
    pub fn caddy_sites_dir(&self) -> PathBuf {
        self.etc().join("caddy").join("sites")
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
    pub fn mariadb_ini(&self, version: &str) -> PathBuf {
        self.etc().join("mariadb").join(version).join("my.ini")
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
    pub fn postgres_conf(&self, version: &str) -> PathBuf {
        self.postgres_data_dir(version).join("postgresql.conf")
    }
    /// PostgreSQL Unix sockets are runtime state, not Apache state or database data.
    /// Keep the directory version-scoped so side-by-side major versions cannot
    /// collide when one is being upgraded or inspected.
    pub fn postgres_run_dir(&self, version: &str) -> PathBuf {
        self.etc().join("postgresql").join(version).join("run")
    }
    pub fn mongo_data_dir(&self, version: &str) -> PathBuf {
        self.data().join("mongodb").join(version)
    }
    pub fn mongo_conf(&self, version: &str) -> PathBuf {
        self.etc().join("mongodb").join(version).join("mongod.conf")
    }
    pub fn service_log(&self, service_id: &str) -> PathBuf {
        self.logs()
            .join(service_id.replace(['@', ':'], "_"))
            .join("out.log")
    }
}

/// Windows 保留便携目录；macOS 不能在 .app/Contents/MacOS 内创建新数据。
/// 旧版已在程序旁保存数据时继续读取，交由设置页的受锁迁移流程搬迁，避免静默丢失。
fn adjacent_data_dir(executable: &Path, os: &str) -> crate::error::Result<Option<PathBuf>> {
    let Some(directory) = executable.parent() else {
        return Ok(None);
    };
    let is_dev = directory.ancestors().any(|parent| {
        matches!(
            parent.file_name().and_then(|name| name.to_str()),
            Some("debug" | "release")
        ) && parent
            .ancestors()
            .any(|ancestor| ancestor.file_name().is_some_and(|name| name == "target"))
    });
    if is_dev {
        return Ok(None);
    }
    let candidate = directory.join("nsb-data");
    if os != "windows" {
        return candidate
            .join("nsb.sqlite")
            .try_exists()
            .map(|exists| exists.then_some(candidate))
            .map_err(|e| crate::error::AppError::io("检查旧数据目录", e));
    }
    if std::fs::create_dir_all(&candidate).is_ok()
        && tempfile::Builder::new()
            .prefix(".niceenv-write-probe-")
            .tempfile_in(&candidate)
            .is_ok()
    {
        return Ok(Some(candidate));
    }
    Ok(None)
}

#[derive(Serialize, Deserialize)]
struct DataDirSelection {
    version: u8,
    path: PathBuf,
    #[serde(default)]
    retired: Vec<PathBuf>,
}

#[derive(Serialize, Deserialize)]
struct DataDirAuthority {
    version: u8,
    selection_file: PathBuf,
}

fn data_dir_selection_file() -> crate::error::Result<PathBuf> {
    dirs::config_dir().map(|root| root.join("com.niceservbay.app").join("data-directory.json"))
        .ok_or_else(|| crate::error::AppError::new("DATA_DIR_SETTINGS", "无法确定应用配置目录，未切换数据目录"))
}

/// 普通操作持共享锁，复制和等待迁移重启期间持独占锁；Drop 自动恢复。
pub struct DataDirActivity { _file: std::fs::File, base: PathBuf, exclusive: bool }
impl DataDirActivity {
    pub fn shared(base: &Path) -> crate::error::Result<Self> { Self::acquire(base, false) }
    pub fn exclusive(base: &Path) -> crate::error::Result<Self> { Self::acquire(base, true) }
    fn acquire(base: &Path, exclusive: bool) -> crate::error::Result<Self> {
        let file = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true)
            .open(base.join(".data-dir-activity.lock"))?;
        let result = if exclusive { file.try_lock() } else { file.try_lock_shared() };
        result.map_err(|error| match error {
            std::fs::TryLockError::WouldBlock => crate::error::AppError::new("DATA_DIR_BUSY", if exclusive {
                "仍有操作或后台任务正在使用数据目录，请完成后再迁移"
            } else { "数据目录正在迁移，请先完成重启或取消迁移" }),
            std::fs::TryLockError::Error(error) => crate::error::AppError::io("锁定数据目录",error),
        })?;
        ensure_data_dir_current(base)?;
        Ok(Self { _file: file, base: std::fs::canonicalize(base)?, exclusive })
    }

    /// 持源目录独占锁时提交选择及旧目录失效记录；二者使用同一个原子文件。
    pub fn with_selected_data_dir<T>(&self, target: &Path, launch: impl FnOnce() -> crate::error::Result<T>) -> crate::error::Result<T> {
        self.select_with_file(&data_dir_selection_file()?, target, launch)
    }

    pub(crate) fn select_with_file<T>(&self, file: &Path, target: &Path, launch: impl FnOnce() -> crate::error::Result<T>) -> crate::error::Result<T> {
        if !self.exclusive {
            return Err(crate::error::AppError::new("DATA_DIR_BUSY", "切换数据目录需要持有源目录独占锁"));
        }
        ensure_data_dir_current(&self.base)?;
        with_selection_and_source(file, target, Some(&self.base), launch)
    }
}

fn parse_data_dir_selection(bytes: &[u8]) -> crate::error::Result<DataDirSelection> {
    let selection: DataDirSelection = serde_json::from_slice(bytes)
        .map_err(|_| crate::error::AppError::new("DATA_DIR_SETTINGS", "保存的数据目录选择已损坏，未使用其它目录启动"))?;
    if selection.version != 1 || !selection.path.is_absolute() || selection.retired.iter().any(|path| !path.is_absolute()) {
        return Err(crate::error::AppError::new("DATA_DIR_SETTINGS", "保存的数据目录选择无效，未使用其它目录启动"));
    }
    Ok(selection)
}

/// 旧实例每次操作重新检查已提交的目录选择；连续迁移返回当前的最终目录。
pub fn redirected_data_dir(base: &Path) -> crate::error::Result<Option<PathBuf>> {
    let Some(bytes) = read_optional(&base.join(".data-dir-authority.json"))? else { return Ok(None); };
    let authority: DataDirAuthority = serde_json::from_slice(&bytes)
        .map_err(|e| crate::error::AppError::internal("读取数据目录交接记录",e.to_string()))?;
    if authority.version != 1 || !authority.selection_file.is_absolute() {
        return Err(crate::error::AppError::new("DATA_DIR_SETTINGS", "数据目录交接记录无效，未继续操作"));
    }
    // 首次提交前进程终止时，选择文件还不存在，原目录仍可使用。
    let Some(bytes) = read_optional(&authority.selection_file)? else { return Ok(None); };
    let selection = parse_data_dir_selection(&bytes)?;
    let current = portable_path_text(&std::fs::canonicalize(base)?);
    let retired = selection.retired.iter().any(|path| {
        let path = portable_path_text(path);
        if cfg!(windows) { path.eq_ignore_ascii_case(&current) } else { path == current }
    });
    Ok(retired.then_some(selection.path))
}

pub fn ensure_data_dir_current(base: &Path) -> crate::error::Result<()> {
    if let Some(target) = redirected_data_dir(base)? {
        return Err(crate::error::AppError::new("DATA_DIR_RELOCATED", "数据目录已在其它进程中迁移，当前实例已停止数据操作")
            .with_hint(format!("请退出这个旧窗口并重新打开 NiceEnv；当前数据目录：{}。使用 MCP 的客户端请重新连接", target.display())));
    }
    Ok(())
}

fn read_data_dir_selection(file: &Path) -> crate::error::Result<Option<PathBuf>> {
    let Some(bytes) = read_optional(file).map_err(|e| crate::error::AppError::io("读取数据目录选择", e))? else { return Ok(None); };
    let selection = parse_data_dir_selection(&bytes)
        .map_err(|error|error.with_hint(format!("请检查 {}，恢复有效的数据目录选择后重新打开应用",file.display())))?;
    validate_migrated_root(&selection.path).map_err(|e| e.with_hint(format!(
        "请连接原磁盘并检查 {}；未创建空数据库或回退到旧目录。可用 NSB_HOME 指定可用的数据目录", selection.path.display())))?;
    Ok(Some(selection.path))
}

pub(crate) fn write_atomic(path: &Path, content: &[u8]) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| backup_error("目标文件没有父目录"))?;
    std::fs::create_dir_all(parent)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(content)?;
    pending.as_file().sync_all()?;
    pending.persist(path).map_err(|e| e.error)?;
    Ok(())
}

/// 新目录首次启动时更新原本启用的 PATH；失败保留标记，下次可重试。
pub(crate) fn finish_data_dir_activation(paths: &Paths, sync_path: impl FnOnce() -> crate::error::Result<()>) -> crate::error::Result<()> {
    let marker = paths.base.join(".data-dir-activation.json");
    let Some(bytes) = read_optional(&marker)? else { return Ok(()); };
    let expected: PathBuf = serde_json::from_slice(&bytes).map_err(|e|crate::error::AppError::internal("读取迁移完成标记",e.to_string()))?;
    if std::fs::canonicalize(&paths.base)? != std::fs::canonicalize(&expected)? {
        return Err(crate::error::AppError::new("DATA_DIR_MOVED", "迁移副本的位置已变化，请重新从原目录执行迁移"));
    }
    sync_path().map_err(|error| error.with_hint("新目录已保留，但环境变量更新未完成。请检查目录权限后重新打开应用；原数据目录仍保留"))?;
    std::fs::remove_file(marker)?;
    Ok(())
}

#[cfg(test)]
fn with_selection_file<T>(file: &Path, target: &Path, launch: impl FnOnce() -> crate::error::Result<T>) -> crate::error::Result<T> {
    with_selection_and_source(file, target, None, launch)
}

/// 选择文件同时提交旧目录列表，启动失败原文回滚即可重新启用源目录。
fn with_selection_and_source<T>(file: &Path, target: &Path, source: Option<&Path>, launch: impl FnOnce() -> crate::error::Result<T>) -> crate::error::Result<T> {
    use crate::error::AppError;
    if !target.is_absolute() { return Err(AppError::new("DATA_DIR_INVALID", "数据目录必须是绝对路径")); }
    if !file.is_absolute() { return Err(AppError::new("DATA_DIR_SETTINGS", "目录选择文件必须是绝对路径")); }
    validate_migrated_root(target)?;
    let parent = file.parent().ok_or_else(|| AppError::new("DATA_DIR_SETTINGS", "应用配置目录无效"))?;
    std::fs::create_dir_all(parent)?;
    let lock = std::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(file.with_extension("lock"))?;
    lock.try_lock().map_err(|_| AppError::new("DATA_DIR_BUSY", "其它应用进程正在切换数据目录，请稍后重试"))?;
    let previous = read_optional(file)?;
    let mut retired = previous.as_deref().map(parse_data_dir_selection).transpose()?.map(|selection|selection.retired).unwrap_or_default();
    if let Some(source) = source {
        let source = std::fs::canonicalize(source)?;
        let target = std::fs::canonicalize(target)?;
        if source == target { return Err(AppError::new("DATA_DIR_INVALID", "不能将当前目录标记为已迁移")); }
        // 在选择文件提交前准备引用；若提交失败或回滚，旧选择不含此源目录，引用保持无效。
        let authority = DataDirAuthority {version:1,selection_file:file.to_path_buf()};
        write_atomic(&source.join(".data-dir-authority.json"), &serde_json::to_vec(&authority)
            .map_err(|e|AppError::internal("保存目录交接记录",e.to_string()))?)?;
        if !retired.contains(&source) { retired.push(source); }
    }
    let selection = serde_json::to_vec(&DataDirSelection { version: 1, path: target.to_path_buf(), retired })
        .map_err(|e| AppError::internal("保存数据目录选择", e.to_string()))?;
    write_atomic(file, &selection).map_err(|e| AppError::io("保存数据目录选择", e))?;
    match launch() {
        Ok(value) => Ok(value),
        Err(error) => {
            if error.code == "RESTART_CHILD_CLEANUP_FAILED" { return Err(error); }
            let restored = match previous {
                Some(bytes) => write_atomic(file, &bytes),
                None => std::fs::remove_file(file),
            };
            if let Err(restore) = restored {
                return Err(AppError::new("DATA_DIR_ROLLBACK_FAILED", "重启失败，恢复原目录选择也失败；当前应用仍使用原目录")
                    .with_hint(format!("请检查 {} 的权限，下次启动前修复目录选择", file.display()))
                    .with_detail(format!("{}; {}", error.message, restore)));
            }
            Err(error)
        }
    }
}

fn validate_update_data_location(base: &Path, os: &str) -> crate::error::Result<()> {
    if os != "macos" {
        return Ok(());
    }
    let inside_bundle = |path: &Path| {
        path.ancestors().any(|parent| {
            parent
                .extension()
                .and_then(|value| value.to_str())
                .is_some_and(|extension| extension.eq_ignore_ascii_case("app"))
        })
    };
    // 同时检查原路径和真实路径：符号链接不能隐藏包内位置，也不能依赖即将被删除的包内入口。
    let canonical = std::fs::canonicalize(base)
        .map_err(|error| crate::error::AppError::io("检查更新前的数据目录", error))?;
    if inside_bundle(base) || inside_bundle(&canonical) {
        return Err(crate::error::AppError::new(
            "DATA_DIR_IN_APP_BUNDLE",
            "当前数据仍保存在 macOS 应用包内，请先迁移数据目录再更新",
        ).with_hint("请到设置中的数据目录选项，将数据迁移到 .app 之外的文件夹；迁移完成并重启后再安装更新，以免替换应用时丢失站点、数据库和证书。")
            .with_detail(portable_path_text(base)));
    }
    Ok(())
}

/// 数据目录迁移结果。迁移成功后桌面端会重启应用，让新进程从目标目录打开数据库和运行时。
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DataDirMigration {
    pub path: String,
    pub files: u64,
    pub bytes: u64,
    pub rewritten_files: u64,
}

/// 只替换完整路径前缀，不误改同名前缀的其它目录；历史日志、密码等不参与重写。
pub(crate) struct DataPathRebase {
    sources: Vec<String>,
    target: String,
    patterns: regex::Regex,
    forms: Vec<(String, String)>,
}

/// 把 Windows 扩展路径前缀从面向用户的文本中移除。
///
/// Windows 为了支持超过 MAX_PATH 的路径会使用 `\\?\`（或斜杠形式
/// `//?/`）前缀。这个前缀适合传给文件 API，但直接显示在界面或错误信息
/// 中会变成用户无法理解的 `//?/D:/...`。这里只处理前缀，不改动路径主体。
pub fn portable_text(value: &str) -> String {
    value
        .replace("\\\\?\\UNC\\", "\\\\")
        .replace("\\\\?\\", "")
        .replace("//?/UNC/", "//")
        .replace("//?/", "")
}

pub fn portable_path_text(path: &Path) -> String {
    let text = path.to_string_lossy();
    #[cfg(windows)]
    {
        portable_text(&text.replace('\\', "/"))
    }
    // Unix 的反斜杠是合法文件名字符，不是目录分隔符。
    #[cfg(not(windows))]
    {
        text.into_owned()
    }
}

/// 仅用于判断托管配置类型；不改变实际文件名或写入配置的路径。
pub(crate) fn config_path_key(path: &Path) -> String {
    let text = portable_path_text(path);
    if cfg!(windows) {
        text.to_ascii_lowercase()
    } else {
        text
    }
}

/// Nginx、PHP ini、MySQL ini 与 Redis 双引号路径的内部文本（不含外层引号）。
/// Windows 先使用普通正斜杠路径；Unix 的字面反斜杠必须转义，不能改成目录分隔符。
/// 不用于命令参数、Caddyfile、正则或 URL；这些用途有各自的编码规则。
pub(crate) fn quoted_config_path(path: &Path) -> String {
    portable_path_text(path)
        .replace('\\', "\\\\")
        .replace('"', "\\\"")
}

/// 文件系统 glob 的字面目录部分；调用方再追加自己的通配符并按配置格式引用。
pub(crate) fn escaped_glob_path(path: &Path) -> String {
    let text = portable_path_text(path);
    if !cfg!(unix) {
        return text;
    }
    escaped_posix_glob_text(&text)
}

/// POSIX glob；Redis 7+ 的 Windows MSYS2 发行也使用同一套规则。
pub(crate) fn escaped_posix_glob_text(text: &str) -> String {
    let mut escaped = String::new();
    for character in text.chars() {
        if matches!(character, '\\' | '*' | '?' | '[' | ']') {
            escaped.push('\\');
        }
        escaped.push(character);
    }
    escaped
}

impl DataPathRebase {
    pub(crate) fn new(source: &Path, target: &Path) -> crate::error::Result<Self> {
        let canonical = std::fs::canonicalize(source)
            .ok()
            .map(|path| portable_path_text(&path));
        let source = portable_path_text(source).trim_end_matches('/').to_string();
        let mut sources = vec![source.clone()];
        if let Some(canonical) = canonical {
            if !sources.contains(&canonical) {
                sources.push(canonical);
            }
        }
        sources.sort_by_key(|s| std::cmp::Reverse(s.len()));
        let target = portable_path_text(target).trim_end_matches('/').to_string();
        if source.is_empty() || source.ends_with(':') {
            return Err(crate::error::AppError::new(
                "DATA_DIR_INVALID",
                "不能将磁盘根目录作为迁移源",
            ));
        }
        // 这些字符会改变现有 shell/配置文件的引号语义，不能直接代入旧模板。
        if target
            .chars()
            .any(|c| c.is_control() || "\"'`$%&|<>^;(){}".contains(c))
        {
            return Err(crate::error::AppError::new(
                "DATA_DIR_INVALID",
                "目标目录包含无法安全写入服务配置的字符，请选择其它目录",
            ));
        }
        let mut forms = Vec::new();
        for source in &sources {
            forms.push((source.clone(), target.clone()));
            if cfg!(windows) {
                let extended = if let Some(unc) = source.strip_prefix("//") {
                    format!("//?/UNC/{unc}")
                } else {
                    format!("//?/{source}")
                };
                forms.push((extended.clone(), target.clone()));
                for form in [source, &extended] {
                    let old = form.replace('/', "\\");
                    let new = target.replace('/', "\\");
                    forms.push((old.replace('\\', "\\\\"), new.replace('\\', "\\\\")));
                    forms.push((old, new));
                }
            }
        }
        forms.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        forms.dedup_by(|a, b| a.0 == b.0);
        let pattern = forms
            .iter()
            .map(|(old, _)| format!("({})", regex::escape(old)))
            .collect::<Vec<_>>()
            .join("|");
        let patterns = regex::RegexBuilder::new(&pattern)
            .case_insensitive(cfg!(windows))
            .build()
            .map_err(|e| crate::error::AppError::internal("准备目录路径替换", e.to_string()))?;
        Ok(Self {
            sources,
            target,
            patterns,
            forms,
        })
    }

    pub(crate) fn path(&self, value: &str) -> String {
        // 先消除 . / ..，避免把 source/../external 误当作目录内文件。
        let mut lexical = PathBuf::new();
        for component in Path::new(value).components() {
            match component {
                Component::CurDir => {}
                Component::ParentDir if lexical.file_name().is_some() => {
                    lexical.pop();
                }
                other => lexical.push(other.as_os_str()),
            }
        }
        let normalized = portable_path_text(&lexical);
        let source = self.sources.iter().find(|source| {
            normalized.get(..source.len()).is_some_and(|prefix| {
                if cfg!(windows) {
                    prefix.eq_ignore_ascii_case(source)
                } else {
                    prefix == *source
                }
            }) && normalized
                .get(source.len()..)
                .is_some_and(|suffix| suffix.is_empty() || suffix.starts_with('/'))
        });
        if let Some(source) = source {
            let suffix = &normalized[source.len()..];
            let rebased = format!("{}{suffix}", self.target);
            if cfg!(windows) && value.contains('\\') {
                rebased.replace('/', "\\")
            } else {
                rebased
            }
        } else {
            value.to_string()
        }
    }

    pub(crate) fn text(&self, value: &str) -> crate::error::Result<String> {
        self.replace_text(value, &self.patterns, &self.forms, false, false)
    }

    /// 配置词法解析后的参数；调用方负责按原配置格式重新引用修改后的完整参数。
    pub(crate) fn config_value(
        &self,
        value: &str,
        encode: impl Fn(&str) -> String,
    ) -> crate::error::Result<String> {
        self.encoded_value(value, encode, false)
    }

    pub(crate) fn config_pattern(&self, value: &str) -> crate::error::Result<String> {
        self.encoded_value(value, regex::escape, true)
    }

    fn encoded_value(
        &self,
        value: &str,
        encode: impl Fn(&str) -> String,
        regex_path: bool,
    ) -> crate::error::Result<String> {
        let mut forms: Vec<_> = self
            .forms
            .iter()
            .map(|(old, new)| (encode(old), encode(new)))
            .collect();
        forms.sort_by(|a, b| b.0.len().cmp(&a.0.len()));
        forms.dedup_by(|a, b| a.0 == b.0);
        let pattern = forms
            .iter()
            .map(|(old, _)| format!("({})", regex::escape(old)))
            .collect::<Vec<_>>()
            .join("|");
        let patterns = regex::RegexBuilder::new(&pattern)
            .case_insensitive(cfg!(windows))
            .build()
            .map_err(|e| crate::error::AppError::internal("转换配置路径编码", e.to_string()))?;
        self.replace_text(value, &patterns, &forms, true, regex_path)
    }

    fn config_text(&self, relative: &Path, value: &str) -> crate::error::Result<String> {
        if let Some((service, json)) = crate::configpaths::service(relative) {
            return crate::configpaths::rebase(value, service, json, self);
        }
        match web_config_service(relative) {
            Some("nginx") => return crate::configgen::rebase_nginx_config(value, self),
            Some("apache") => return crate::configgen::rebase_httpd_config(value, self),
            _ => {}
        }
        let relative = config_path_key(relative);
        let name = relative.rsplit('/').next().unwrap_or("");
        let name = name.strip_suffix(".disabled").unwrap_or(name);
        let conf = name.ends_with(".conf");
        if (relative.starts_with("etc/caddy/") || relative.starts_with("runtimes/caddy/"))
            && (conf || name == "Caddyfile" || (cfg!(windows) && name == "caddyfile"))
        {
            return crate::caddy::rebase_config(value, self, Path::new(&self.target));
        }
        let ini = name.ends_with(".ini") || name.ends_with(".cnf");
        if name == ".user.ini"
            || (ini && (relative.starts_with("etc/php/") || relative.starts_with("runtimes/php/")))
        {
            return crate::configgen::rebase_ini_config(
                value,
                self,
                crate::configgen::IniDialect::Php,
            );
        }
        if ini
            && [
                "etc/mysql/",
                "runtimes/mysql/",
                "etc/mariadb/",
                "runtimes/mariadb/",
            ]
            .iter()
            .any(|prefix| relative.starts_with(prefix))
        {
            return crate::configgen::rebase_ini_config(
                value,
                self,
                crate::configgen::IniDialect::Mysql,
            );
        }
        if conf && (relative.starts_with("etc/redis/") || relative.starts_with("runtimes/redis/")) {
            let major = relative.split('/').nth(2).and_then(|version| {
                version
                    .trim_start_matches('v')
                    .split('.')
                    .next()?
                    .parse::<u32>()
                    .ok()
            });
            return crate::configgen::rebase_redis_config(
                value,
                self,
                major.is_some_and(|major| major >= 7),
            );
        }
        self.text(value)
    }

    fn replace_text(
        &self,
        value: &str,
        patterns: &regex::Regex,
        forms: &[(String, String)],
        quoted: bool,
        pattern: bool,
    ) -> crate::error::Result<String> {
        let mut out = String::new();
        let mut cursor = 0;
        for captures in patterns.captures_iter(value) {
            let Some(found) = captures.get(0) else {
                continue;
            };
            let before = value[..found.start()].chars().next_back();
            let after = value[found.end()..].chars().next();
            let boundary = |c: char| c.is_whitespace() || "\"'=;:,()[]{}".contains(c);
            if quoted {
                // 参数内部的空格、方括号等都是文件名字符，不能当作路径边界。
                // 正则只接受明确的起始锚点/分组；字面反斜杠转义也不能误认成子目录。
                let prefix = &value[..found.start()];
                let prefix_ok = prefix.is_empty()
                    || (pattern
                        && matches!(
                            prefix,
                            "^" | "(?i)" | "(?i)^" | "(?i:" | "^(?i:" | "(?:" | "^(?:"
                        ));
                let suffix_ok = after.is_none_or(|c| {
                    c == '/'
                        || (pattern && matches!(c, ')' | '$'))
                        || (!pattern && cfg!(windows) && c == '\\')
                });
                if !prefix_ok || !suffix_ok {
                    continue;
                }
            } else if !before.is_none_or(boundary)
                || !after.is_none_or(|c| boundary(c) || c == '/' || (cfg!(windows) && c == '\\'))
            {
                continue;
            }
            let suffix = if quoted {
                &value[found.end()..]
            } else {
                value[found.end()..].split(boundary).next().unwrap_or("")
            };
            if suffix
                .split(|c| c == '/' || (!pattern && cfg!(windows) && c == '\\'))
                .any(|part| part == ".." || (pattern && part == r"\.\."))
            {
                return Err(crate::error::AppError::new(
                    "DATA_DIR_PATH_AMBIGUOUS",
                    "配置中的旧路径包含上级目录引用，无法自动修正",
                )
                .with_hint("请先将相关路径改为完整绝对路径后重试"));
            }
            let Some(replacement) = captures
                .iter()
                .skip(1)
                .position(|item| item.is_some())
                .map(|i| &forms[i].1)
            else {
                continue;
            };
            if !quoted && replacement.contains(' ') && !found.as_str().contains(' ') {
                let line = value[..found.start()].rsplit('\n').next().unwrap_or("");
                if line.matches('"').count() % 2 == 0 && line.matches('\'').count() % 2 == 0 {
                    return Err(crate::error::AppError::new(
                        "DATA_DIR_PATH_QUOTING",
                        "配置中存在未加引号的旧路径，无法安全迁移到含空格的目录",
                    )
                    .with_hint("请选择不含空格的目录，或先为相关配置中的完整路径添加引号后重试"));
                }
            }
            out.push_str(&value[cursor..found.start()]);
            out.push_str(replacement);
            cursor = found.end();
        }
        out.push_str(&value[cursor..]);
        Ok(out)
    }
}

/// 将整个 NiceEnv 数据目录复制到一个新的空目录。
///
/// 复制使用同父目录的临时目录，完成后再一次性改名，避免目标目录只复制了一半就被下次启动
/// 选中。拒绝把目标放进源目录，也拒绝跟随软链接/目录联接，避免迁移时越出用户明确选择的范围。
pub fn copy_data_dir(
    source: &Path,
    requested_target: &Path,
) -> crate::error::Result<DataDirMigration> {
    let source_alias = source.to_path_buf();
    if let Ok(metadata) = std::fs::symlink_metadata(source) {
        if linked(&metadata) {
            return Err(crate::error::AppError::new(
                "DATA_DIR_INVALID",
                "当前数据目录是软链接或目录联接，无法安全迁移",
            ));
        }
    }
    let source = std::fs::canonicalize(source)
        .map_err(|e| crate::error::AppError::io("读取当前数据目录", e))?;
    if !source.is_dir() {
        return Err(crate::error::AppError::new(
            "DATA_DIR_INVALID",
            "当前数据目录不是文件夹",
        ));
    }

    let requested_target = if requested_target.is_absolute() {
        requested_target.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|e| crate::error::AppError::io("解析目标数据目录", e))?
            .join(requested_target)
    };
    let name = requested_target
        .file_name()
        .filter(|name| !name.is_empty())
        .ok_or_else(|| crate::error::AppError::new("DATA_DIR_INVALID", "请选择一个具体的数据目录"))?;
    let parent = requested_target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .ok_or_else(|| crate::error::AppError::new("DATA_DIR_INVALID", "目标数据目录路径无效"))?;
    std::fs::create_dir_all(parent)
        .map_err(|e| crate::error::AppError::io("创建目标数据目录的父目录", e))?;
    let parent = std::fs::canonicalize(parent)
        .map_err(|e| crate::error::AppError::io("解析目标数据目录的父目录", e))?;
    let target = parent.join(name);
    let rebase = DataPathRebase::new(&source_alias, &target)?;

    if let Ok(metadata) = std::fs::symlink_metadata(&target) {
        if linked(&metadata) {
            return Err(crate::error::AppError::new(
                "DATA_DIR_INVALID",
                "目标目录不能是软链接或目录联接",
            ));
        }
    }

    let source_text = source.to_string_lossy();
    let target_text = target.to_string_lossy();
    let same_path = if cfg!(windows) {
        source_text.eq_ignore_ascii_case(&target_text)
    } else {
        source_text == target_text
    };
    if same_path || target.starts_with(&source) {
        return Err(crate::error::AppError::new(
            "DATA_DIR_INVALID",
            "目标目录不能与当前目录相同，也不能放在当前目录里面",
        ));
    }
    if target.exists() {
        if !target.is_dir() {
            return Err(crate::error::AppError::new(
                "DATA_DIR_NOT_EMPTY",
                "目标路径已经是一个文件",
            ));
        }
        let mut entries = std::fs::read_dir(&target)
            .map_err(|e| crate::error::AppError::io("检查目标数据目录", e))?;
        let has_entry = entries
            .next()
            .transpose()
            .map_err(|e| crate::error::AppError::io("检查目标数据目录", e))?
            .is_some();
        if has_entry {
            return Err(crate::error::AppError::new(
                "DATA_DIR_NOT_EMPTY",
                "目标数据目录必须为空",
            ));
        }
    }

    let staging = parent.join(format!(
        ".{}-migrating-{}",
        name.to_string_lossy(),
        crate::services::now_ms()
    ));
    if staging.exists() {
        return Err(crate::error::AppError::new(
            "DATA_DIR_BUSY",
            "目标目录正在进行另一次迁移，请稍后重试",
        ));
    }
    std::fs::create_dir(&staging)
        .map_err(|e| crate::error::AppError::io("创建迁移暂存目录", e))?;

    let mut stats = (0_u64, 0_u64);
    let copy_result = copy_tree(&source, &staging, &mut stats, true);
    if let Err(error) = copy_result {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(crate::error::AppError::io("复制数据目录", error));
    }
    let prepared = (|| {
        crate::store::Store::snapshot_for_data_dir(&source.join("nsb.sqlite"), &staging.join("nsb.sqlite"), &rebase)?;
        stats.0 += 1;
        stats.1 += std::fs::metadata(staging.join("nsb.sqlite"))?.len();
        let rewritten = rebase_config_files(&staging, &source, &rebase)?;
        validate_migrated_root(&staging)?;
        let history_file = staging.join(".data-dir-history.json");
        let mut history = read_path_history(&staging)?;
        history.extend(rebase.sources.iter().map(PathBuf::from));
        history.sort(); history.dedup();
        write_atomic(&history_file,&serde_json::to_vec(&history).map_err(|e|crate::error::AppError::internal("保存目录历史",e.to_string()))?)?;
        let marker = serde_json::to_vec(&PathBuf::from(portable_path_text(&target)))
            .map_err(|e|crate::error::AppError::internal("记录迁移目标",e.to_string()))?;
        write_atomic(&staging.join(".data-dir-activation.json"), &marker)?;
        Ok::<_, crate::error::AppError>(rewritten)
    })();
    let rewritten_files = match prepared {
        Ok(count) => count,
        Err(error) => {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(error);
        }
    };
    if target.exists() {
        if let Err(error) = std::fs::remove_dir(&target) {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(crate::error::AppError::io("替换空目标数据目录", error));
        }
    }
    if let Err(error) = std::fs::rename(&staging, &target) {
        let _ = std::fs::remove_dir_all(&staging);
        return Err(crate::error::AppError::io("提交新的数据目录", error));
    }
    Ok(DataDirMigration {
        path: portable_path_text(&target),
        files: stats.0,
        bytes: stats.1,
        rewritten_files,
    })
}

fn copy_tree(source: &Path, target: &Path, stats: &mut (u64, u64), root: bool) -> io::Result<()> {
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        // 活跃 SQLite 文件不能逐个复制；随后用 VACUUM INTO 创建一致快照。
        if root && matches!(entry.file_name().to_str(), Some("nsb.sqlite" | "nsb.sqlite-wal" | "nsb.sqlite-shm" | "nsb.sqlite-journal" | ".data-dir-activity.lock" | ".data-dir-activation.json" | ".data-dir-authority.json" | ".niceenv-control.lock" | ".niceenv-control.json")) { continue; }
        let from = entry.path();
        let to = target.join(entry.file_name());
        let metadata = std::fs::symlink_metadata(&from)?;
        if linked(&metadata) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("数据目录包含不支持迁移的链接：{}", from.display()),
            ));
        }
        if metadata.is_dir() {
            std::fs::create_dir(&to)?;
            copy_tree(&from, &to, stats, false)?;
        } else if metadata.is_file() {
            std::fs::copy(&from, &to)?;
            stats.0 = stats.0.saturating_add(1);
            stats.1 = stats.1.saturating_add(metadata.len());
        } else {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("数据目录包含不支持迁移的文件：{}", from.display()),
            ));
        }
    }
    Ok(())
}

fn web_config_service(relative: &Path) -> Option<&'static str> {
    let relative = config_path_key(relative);
    let name = relative.rsplit('/').next().unwrap_or("");
    let name = name.strip_suffix(".disabled").unwrap_or(name);
    if (relative.starts_with("etc/nginx/") || relative.starts_with("runtimes/nginx/"))
        && (name.ends_with(".conf")
            || [
                "mime.types",
                "fastcgi_params",
                "uwsgi_params",
                "scgi_params",
            ]
            .contains(&name))
    {
        Some("nginx")
    } else if (relative.starts_with("etc/apache/") || relative.starts_with("runtimes/apache/"))
        && name.ends_with(".conf")
    {
        Some("apache")
    } else {
        None
    }
}

fn rebase_config_files(
    root: &Path,
    source: &Path,
    rebase: &DataPathRebase,
) -> crate::error::Result<u64> {
    let mut files = Vec::new();
    migration_config_files(root, root, &mut files)?;
    let resources = migration_resources(root, source, &files, rebase)?;
    let mut rewritten = 0;
    let mut web_configs = std::collections::HashMap::<&str, String>::new();
    for path in files {
        if resources
            .paths
            .iter()
            .any(|resource| resource_contains(resource, &config_path_key(&path)))
            || resources
                .env_dirs
                .iter()
                .any(|directory| resource_contains(directory, &config_path_key(&path)))
            || resources.files.contains_key(&path)
        {
            continue;
        }
        let relative = path.strip_prefix(root).unwrap();
        if std::fs::metadata(&path)?.len() > 16 * 1024 * 1024 {
            return Err(crate::error::AppError::new(
                "DATA_DIR_CONFIG_SIZE",
                format!("配置文件过大，无法自动检查：{}", relative.display()),
            ));
        }
        let bytes = std::fs::read(&path)?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            // 二进制凭据/缓存保持字节不变；旧路径出现在非 UTF-8 配置时不能假装完成。
            if rebase.patterns.is_match(&String::from_utf8_lossy(&bytes)) {
                return Err(crate::error::AppError::new(
                    "DATA_DIR_CONFIG_ENCODING",
                    format!("配置不是 UTF-8，无法修正旧路径：{}", relative.display()),
                ));
            }
            continue;
        };
        if let Some(service) = web_config_service(relative) {
            let combined = web_configs.entry(service).or_default();
            combined.push_str(text);
            combined.push('\n');
        }
        let rebased = rebase
            .config_text(relative, text)
            .map_err(|e| e.with_detail(relative.display().to_string()))?;
        if rebased != text {
            std::fs::write(&path, rebased)?;
            rewritten += 1;
        }
    }
    // 定义和使用可以分散在多个 include 文件中。提交迁移目录之前联合检查，
    // 不能因为各文件单独转换成功，就留下仍指向旧目录的变量引用。
    for (service, content) in web_configs {
        let result = if service == "nginx" {
            crate::configgen::rebase_nginx_config(&content, rebase)
        } else {
            crate::configgen::rebase_httpd_config(&content, rebase)
        };
        result.map_err(|error| error.with_detail(format!("{service} 配置及其 include 文件")))?;
    }
    for (path, bytes) in resources.files {
        let existing = match std::fs::read(&path) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == io::ErrorKind::NotFound => None,
            Err(error) => return Err(error.into()),
        };
        if existing.as_deref() != Some(bytes.as_slice()) {
            std::fs::write(path, bytes)?;
            rewritten += 1;
        }
    }
    for provider in resources.providers.values() {
        if crate::sftpgo_data::rebase_provider(provider, rebase)? {
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

fn resource_contains(resource: &str, path: &str) -> bool {
    path == resource
        || path
            .strip_prefix(resource)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

#[derive(Default)]
struct MigrationResources {
    paths: Vec<String>,
    files: std::collections::HashMap<PathBuf, Vec<u8>>,
    env_dirs: Vec<String>,
    providers: std::collections::HashMap<String, crate::sftpgo_data::Provider>,
}

fn migration_resources(
    root: &Path,
    source: &Path,
    files: &[PathBuf],
    rebase: &DataPathRebase,
) -> crate::error::Result<MigrationResources> {
    // 使用复制后的文件做只读规划，所有规划成功后才改写。不能按 read_dir 顺序边读边改。
    let root_key = config_path_key(root);
    let target_key = config_path_key(Path::new(&rebase.target));
    let mut plans = Vec::new();
    for file in files {
        let relative = file.strip_prefix(root).unwrap();
        let Some(("sftpgo", json)) = crate::configpaths::service(relative) else {
            continue;
        };
        let result = (|| {
            if std::fs::metadata(file)?.len() > 16 * 1024 * 1024 {
                return Err(crate::error::AppError::new("DATA_DIR_CONFIG_SIZE", "配置文件过大，无法自动检查"));
            }
            let text = std::fs::read_to_string(file)?;
            let mut resources = MigrationResources::default();
            let mut references = crate::configpaths::sftpgo_resources(&text, json)?;
            let directory = source.join(relative).parent().unwrap().to_path_buf();
            let mut cwd;
            {
                let store = crate::store::Store::open_read_only(source.join("nsb.sqlite"))?;
                let planned = crate::generic::sftpgo_migrate_environment(&store, &Paths::new(source.to_path_buf()), directory, &text, json, rebase)?;
                cwd = planned.cwd;
                references.extend(planned.resources);
                if let Some(mut provider) = planned.provider {
                    if let Some(executable) = &mut provider.executable {
                        let mapped = portable_path_text(Path::new(&rebase.path(&portable_path_text(executable))));
                        if !resource_contains(&target_key, &config_path_key(Path::new(&mapped))) {
                            return Err(crate::error::AppError::new("DATA_DIR_SFTPGO_RUN",
                                "SFTPGo 程序不在托管数据目录中，无法验证迁移副本"));
                        }
                        *executable = root.join(mapped[target_key.len()..].trim_start_matches('/'));
                    }
                    let mapped = rebase.path(&portable_path_text(&provider.path));
                    let key = config_path_key(Path::new(&mapped));
                    if resource_contains(&target_key, &key) {
                        provider.path = root.join(mapped[target_key.len()..].trim_start_matches(['/', '\\']));
                        // 默认文件名以及 SQLite 日志也属于账号库，不按文本配置改写。
                        let key = config_path_key(&provider.path);
                        resources.paths.push(key.clone());
                        for suffix in ["-wal", "-shm", "-journal"] {
                            resources.paths.push(format!("{key}{suffix}"));
                        }
                    } else {
                        provider.external = true;
                    }
                    for directory in crate::sftpgo_data::data_directories(&provider, rebase)? {
                        let mapped = rebase.path(&portable_path_text(&directory));
                        let key = config_path_key(Path::new(&mapped));
                        if resource_contains(&target_key, &key) {
                            resources.paths.push(format!("{root_key}{}", &key[target_key.len()..]));
                        }
                    }
                    resources.providers.insert(config_path_key(&provider.path), provider);
                }
                resources.env_dirs.push(config_path_key(&file.parent().unwrap().join("env.d")));
                for (path, bytes) in planned.files {
                    let mapped = portable_path_text(Path::new(&rebase.path(&portable_path_text(&path))));
                    if !resource_contains(&target_key, &config_path_key(Path::new(&mapped))) {
                        return Err(crate::error::AppError::new("DATA_DIR_ENV_PATH", "环境配置文件超出数据目录，未修改原文件"));
                    }
                    let path = root.join(mapped[target_key.len()..].trim_start_matches('/'));
                    resources.files.insert(path, bytes);
                }
            }
            for resource in references {
                let resource_path = if resource.path.is_absolute() || resource.relative_to_config {
                    resource.path
                } else {
                    if cwd.is_none() {
                        let store = crate::store::Store::open_read_only(source.join("nsb.sqlite"))?;
                        cwd = Some(crate::generic::sftpgo_migration_cwd(&store,
                            &Paths::new(source.to_path_buf()), source.join(relative).parent().unwrap().to_path_buf())
                            .map_err(|_| crate::error::AppError::new("DATA_DIR_RESOURCE_BASE",
                                "无法从 SFTPGo 安装记录确认资源的工作目录，未切换数据目录")
                                .with_hint("请检查当前 SFTPGo 安装是否完整；也可将资源配置改为绝对路径。原文件已保留。"))?);
                    }
                    cwd.as_ref().unwrap().join(resource.path)
                };
                let key = if resource_path.is_absolute() {
                    let mapped = rebase.path(&portable_path_text(&resource_path));
                    let mapped = config_path_key(Path::new(&mapped));
                    if !resource_contains(&target_key, &mapped) { continue; }
                    format!("{root_key}{}", &mapped[target_key.len()..])
                } else {
                    let mut normalized = PathBuf::new();
                    for part in file.parent().unwrap().join(resource_path).components() {
                        match part {
                            Component::CurDir => {},
                            Component::ParentDir => { normalized.pop(); },
                            part => normalized.push(part.as_os_str()),
                        }
                    }
                    let key = config_path_key(&normalized);
                    if !resource_contains(&root_key, &key) { continue; }
                    key
                };
                resources.paths.push(key.trim_end_matches('/').to_string());
            }
            Ok::<_, crate::error::AppError>(resources)
        })().map_err(|error| error.with_detail(relative.display().to_string()));
        plans.push((config_path_key(file), result));
    }
    let mut pending = (0..plans.len()).collect::<Vec<_>>();
    let mut resources = MigrationResources::default();
    while !pending.is_empty() {
        pending.retain(|index| {
            !resources
                .paths
                .iter()
                .any(|resource| resource_contains(resource, &plans[*index].0))
        });
        if pending.is_empty() {
            break;
        }
        let roots = pending
            .iter()
            .copied()
            .filter(|index| {
                !pending.iter().any(|other| {
                    plans[*other].1.as_ref().is_ok_and(|references| {
                        references
                            .paths
                            .iter()
                            .any(|resource| resource_contains(resource, &plans[*index].0))
                    })
                })
            })
            .collect::<Vec<_>>();
        if roots.is_empty() {
            return Err(crate::error::AppError::new(
                "DATA_DIR_RESOURCE_CONFLICT",
                "同一文件或目录同时被用作配置和资源，无法安全迁移",
            )
            .with_hint("请将密钥、模板或提示文本放在独立资源文件中；原配置和数据均已保留。"));
        }
        for index in &roots {
            let plan = plans[*index].1.as_ref().map_err(Clone::clone)?;
            resources.paths.extend(plan.paths.iter().cloned());
            resources.env_dirs.extend(plan.env_dirs.iter().cloned());
            for (key, provider) in &plan.providers {
                if resources.providers.insert(key.clone(), provider.clone())
                    .is_some_and(|previous| previous != *provider)
                {
                    return Err(crate::error::AppError::new("DATA_DIR_SFTPGO_STATE",
                        "多份 SFTPGo 配置对同一账号库使用不同的驱动或表前缀，未切换数据目录"));
                }
            }
            for (path, bytes) in &plan.files {
                if resources
                    .files
                    .insert(path.clone(), bytes.clone())
                    .is_some_and(|previous| previous != *bytes)
                {
                    return Err(crate::error::AppError::new(
                        "DATA_DIR_ENV_CONFLICT",
                        "多份 SFTPGo 配置要求不同的运行环境，未自动覆盖共享配置",
                    )
                    .with_hint("请先确认各配置目录的环境变量；源文件和目标目录均保持原样。"));
                }
            }
        }
        pending.retain(|index| !roots.contains(index));
    }
    resources.paths.sort();
    resources.paths.dedup();
    for path in resources.files.keys() {
        if resources
            .paths
            .iter()
            .any(|resource| resource_contains(resource, &config_path_key(path)))
        {
            return Err(crate::error::AppError::new(
                "DATA_DIR_RESOURCE_CONFLICT",
                "同一文件同时被用作环境配置和资源，无法安全迁移",
            ));
        }
    }
    Ok(resources)
}

fn migration_config_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> crate::error::Result<()> {
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path
            .strip_prefix(root)
            .map_err(|_| crate::error::AppError::new("DATA_DIR_INVALID", "配置文件超出迁移目录"))?;
        let relative_text = config_path_key(relative);
        let first = relative_text.split('/').next().unwrap_or("");
        // 历史、下载及数据库业务内容保持原样，不做全盘字符串替换。
        if matches!(
            first,
            "backup" | "logs" | "downloads" | "certs" | "cron-locks"
        ) {
            continue;
        }
        if [
            "etc/apache/logs",
            "etc/apache/run",
            "etc/nginx/run",
            "etc/nginx/temp",
        ]
        .iter()
        .any(|prefix| relative_text == *prefix || relative_text.starts_with(&format!("{prefix}/")))
        {
            continue;
        }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            migration_config_files(root, &path, files)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let active_name = name.strip_suffix(".disabled").unwrap_or(&name);
        let ext = active_name
            .rsplit_once('.')
            .map(|(_, ext)| ext)
            .unwrap_or("");
        let config = first == "etc"
            || crate::configpaths::service(relative).is_some()
            || first == "user-modules"
            || name == ".user.ini"
            || (relative_text.starts_with("runtimes/caddy/") && active_name == "caddyfile")
            || ((first == "runtimes" || first == "data")
                && matches!(
                    ext,
                    "conf" | "cnf" | "ini" | "cfg" | "properties" | "cmd" | "bat" | "ps1" | "sh"
                ))
            || name == ".niceenv-package.json";
        if config {
            files.push(path);
        }
    }
    Ok(())
}

fn read_path_history(base: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(bytes) = read_optional(&base.join(".data-dir-history.json"))? else { return Ok(Vec::new()); };
    let history: Vec<PathBuf> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if history.len() > 100 || history.iter().any(|p| !p.is_absolute()) {
        return Err(backup_error("数据目录历史记录无效，无法安全转换备份路径"));
    }
    Ok(history)
}

pub(crate) fn rebase_backup_content(
    base: &Path,
    target: &Path,
    content: Vec<u8>,
) -> io::Result<Vec<u8>> {
    let relative = target
        .strip_prefix(base)
        .map_err(|_| backup_error("备份目标超出数据目录"))?;
    let mut history = read_path_history(base)?;
    // 长路径优先，避免多次迁移的相邻目录前缀互相覆盖。
    history.sort_by_key(|path| std::cmp::Reverse(path.as_os_str().len()));
    if history.is_empty() {
        return Ok(content);
    }
    let mut text = String::from_utf8(content).map_err(io::Error::other)?;
    for source in history {
        let rebase = DataPathRebase::new(&source, base).map_err(io::Error::other)?;
        text = rebase
            .config_text(relative, &text)
            .map_err(io::Error::other)?;
    }
    Ok(text.into_bytes())
}

fn validate_migrated_root(root: &Path) -> crate::error::Result<()> {
    ensure_data_dir_current(root)?;
    if !root.join("nsb.sqlite").is_file() {
        return Err(crate::error::AppError::new(
            "DATA_DIR_INVALID",
            "迁移源缺少 NiceEnv 数据库，未切换目录",
        ));
    }
    let conn = rusqlite::Connection::open_with_flags(root.join("nsb.sqlite"), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let check: String = conn.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
    if check != "ok" { return Err(crate::error::AppError::new("DATA_DIR_INVALID", "数据目录中的数据库校验失败，未切换目录")); }
    conn.prepare("SELECT key,value FROM settings LIMIT 0")?;
    conn.prepare("SELECT install_path,config_path FROM installed LIMIT 0")?;
    Ok(())
}

/// 写文件前把旧内容备份到 {base}/backup/
pub fn write_with_backup(path: &Path, content: &str, backup_dir: &Path) -> std::io::Result<()> {
    write_with_backup_expected(path, content, backup_dir, None)
}

pub(crate) fn write_with_backup_expected(
    path: &Path,
    content: &str,
    backup_dir: &Path,
    expected: Option<Option<&[u8]>>,
) -> io::Result<()> {
    let base = backup_dir
        .parent()
        .ok_or_else(|| backup_error("备份目录无效"))?;
    let relative = path
        .strip_prefix(base)
        .map_err(|_| backup_error("配置不在应用数据目录内"))?;
    let relative = portable_path_text(relative);
    let target = checked_data_path(base, &relative)?;
    let previous = read_optional(&target)?;
    if expected.is_some_and(|bytes| previous.as_deref() != bytes) {
        return Err(backup_error("配置已变化，请重新预览后恢复"));
    }
    if previous.as_deref() == Some(content.as_bytes()) {
        return Ok(());
    }
    let parent = target
        .parent()
        .ok_or_else(|| backup_error("配置目录无效"))?;
    std::fs::create_dir_all(parent)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent)?;
    pending.write_all(content.as_bytes())?;
    pending.as_file().sync_all()?;
    if let Ok(metadata) = std::fs::metadata(&target) {
        pending.as_file().set_permissions(metadata.permissions())?;
    }
    if let Some(previous) = &previous {
        let directory = checked_data_path(base, "backup/files")?;
        std::fs::create_dir_all(&directory)?;
        let pending_backup = tempfile::Builder::new()
            .prefix(".pending-")
            .tempdir_in(&directory)?;
        let payload = pending_backup.path().join("content.bak");
        let mut file = std::fs::File::create(&payload)?;
        file.write_all(&previous)?;
        file.sync_all()?;
        drop(file);
        let metadata = BackupMetadata {
            version: 1,
            target: relative.clone(),
            sha256: hex::encode(Sha256::digest(previous)),
        };
        let mut file = std::fs::File::create(pending_backup.path().join("metadata.json"))?;
        file.write_all(&serde_json::to_vec(&metadata).map_err(io::Error::other)?)?;
        file.sync_all()?;
        drop(file);
        let suffix = pending_backup
            .path()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .replace(".pending-", "");
        let name = format!(
            "cfg-{}-{suffix}",
            chrono::Utc::now().timestamp_nanos_opt().unwrap_or_default()
        );
        std::fs::rename(pending_backup.path(), directory.join(name))?;
    }
    checked_data_path(base, &relative)?;
    if read_optional(&target)? != previous {
        return Err(backup_error(
            "配置在写入前已变化，未覆盖当前文件，请重新预览",
        ));
    }
    pending.persist(&target).map_err(|e| e.error)?;
    Ok(())
}

/// Windows 路径转 nginx 正斜杠形式
pub fn nginx_path(p: &Path) -> String {
    // `portable_path_text` 只在 Windows 把反斜杠转换为分隔符；Unix 上反斜杠
    // 可以是合法文件名字符，不能在写入 nginx 配置时被误改成目录层级；
    // 写入配置时要再转义一次，避免 Nginx 把它当作控制字符。
    let text = portable_path_text(p);
    #[cfg(not(windows))]
    let text = text.replace('\\', "\\\\");
    // Nginx 配置文件不需要 Windows 文件 API 使用的 `\\?\` 长路径前缀。
    // 如果直接写入，用户会看到 `//?/D:/...`，并且旧配置同步时会和普通路径
    // 被识别成两条不同的 include。
    portable_text(&text)
}

/// 列出备份目录里的备份文件（新→旧）。返回 (文件名, 完整路径, 字节数, 修改时间 ms)
pub fn list_backups(base: &Path) -> Vec<(String, String, u64, i64)> {
    list_backup_files(base)
        .unwrap_or_default()
        .into_iter()
        .map(|file| (file.name, file.path, file.size_bytes, file.modified_at))
        .collect()
}

/// 从已记录准确目标的备份恢复；兼容目标唯一且无版本歧义的旧备份。
pub fn restore_backup(base: &Path, backup_name: &str) -> std::io::Result<PathBuf> {
    restore_backup_checked(base, backup_name, None)
}

fn backup_error(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}

fn linked(metadata: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if metadata.file_attributes() & 0x400 != 0 {
            return true;
        }
    }
    metadata.file_type().is_symlink()
}

/// macOS 的根目录别名由系统维护；仅展开指向预期位置的别名，保留其余链接供安全检查拒绝。
pub(crate) fn system_path(path: &Path) -> PathBuf {
    #[cfg(target_os = "macos")]
    for (alias, target) in [
        ("/var", "/private/var"),
        ("/tmp", "/private/tmp"),
        ("/etc", "/private/etc"),
    ] {
        if let Ok(relative) = path.strip_prefix(alias) {
            if let Ok(link) = std::fs::read_link(alias) {
                let resolved = if link.is_absolute() {
                    link
                } else {
                    Path::new("/").join(link)
                };
                if resolved == Path::new(target) {
                    return Path::new(target).join(relative);
                }
            }
        }
    }
    path.to_path_buf()
}

/// 相对路径必须由普通组件构成；拒绝盘符、ADS、尾点、软链接和目录联接。
pub(crate) fn checked_data_path(base: &Path, relative: &str) -> io::Result<PathBuf> {
    if relative.is_empty() || relative.contains(['\\', ':', '\0', '<', '>', '"', '|', '?', '*']) {
        return Err(backup_error("配置路径无效"));
    }
    let mut result = base.to_path_buf();
    for part in relative.split('/') {
        let device = part.split('.').next().unwrap_or("").to_ascii_uppercase();
        let numbered_device = device
            .strip_prefix("COM")
            .or_else(|| device.strip_prefix("LPT"))
            .is_some_and(|suffix| {
                matches!(
                    suffix,
                    "0" | "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" | "¹" | "²" | "³"
                )
            });
        if part.is_empty()
            || part == "."
            || part == ".."
            || part.ends_with(['.', ' '])
            || part.chars().any(|c| c.is_control())
            || ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str())
            || numbered_device
            || Path::new(part)
                .components()
                .any(|c| !matches!(c, Component::Normal(_)))
        {
            return Err(backup_error("配置路径无效"));
        }
        result.push(part);
        // 只在检查时解析系统别名；返回值保留 base 的写法，供后续 strip_prefix 比较。
        match std::fs::symlink_metadata(system_path(&result)) {
            Ok(metadata) if linked(&metadata) => {
                return Err(backup_error("配置路径包含软链接或目录联接，无法自动恢复"))
            }
            Ok(_) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
    }
    Ok(result)
}

fn read_optional(path: &Path) -> io::Result<Option<Vec<u8>>> {
    match std::fs::read(path) {
        Ok(bytes) => Ok(Some(bytes)),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Serialize, Deserialize)]
struct BackupMetadata {
    version: u8,
    target: String,
    sha256: String,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupFile {
    pub name: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: i64,
    #[serde(serialize_with = "crate::model::serialize_optional_path")]
    pub target_path: Option<String>,
    pub restorable: bool,
    pub reason: Option<String>,
    #[serde(skip)]
    sort_time: u128,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupPreview {
    pub name: String,
    #[serde(serialize_with = "crate::model::serialize_path")]
    pub target_path: String,
    pub target_relative: String,
    pub current_exists: bool,
    pub revision: String,
    pub backup_content: Option<String>,
    pub current_content: Option<String>,
    pub backup_size_bytes: u64,
    pub current_size_bytes: u64,
    pub changed: bool,
}

fn restorable_config_target(relative: &str) -> bool {
    relative.starts_with("etc/") || crate::cfgeditor::config_key_for_relative(relative).is_some()
}

fn backup_preview_text(content: &[u8]) -> Option<String> {
    // 只在内存预览有限大小的完整文本；不把截断内容误当作将恢复的完整文件。
    if content.len() > 256 * 1024 || content.contains(&0) { return None; }
    std::str::from_utf8(content).ok().map(str::to_owned)
}

fn legacy_target(base: &Path, name: &str) -> io::Result<String> {
    let original = name
        .strip_suffix(".bak")
        .and_then(|s| s.rsplit_once('.').map(|(name, _)| name))
        .ok_or_else(|| backup_error("旧备份文件名无法解析"))?;
    if ["php.ini", "my.ini", "redis.conf"].contains(&original) {
        return Err(backup_error(
            "旧备份没有记录所属版本，请打开备份目录确认，不能自动恢复",
        ));
    }
    let etc = checked_data_path(base, "etc")?;
    let mut stack = vec![etc];
    let mut found = Vec::new();
    while let Some(directory) = stack.pop() {
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if linked(&metadata) {
                continue;
            }
            if metadata.is_dir() {
                stack.push(entry.path());
            } else if metadata.is_file() && entry.file_name().to_string_lossy() == original {
                found.push(portable_path_text(
                    entry
                        .path()
                        .strip_prefix(base)
                        .map_err(|_| backup_error("配置路径无效"))?,
                ));
            }
        }
    }
    match found.len() {
        1 => Ok(found.remove(0)),
        0 => Err(backup_error(
            "找不到旧备份对应的配置文件，请打开备份目录确认",
        )),
        _ => Err(backup_error(
            "存在多个同名配置，旧备份无法确定目标，不能自动恢复",
        )),
    }
}

fn backup_source(base: &Path, name: &str) -> io::Result<(PathBuf, String, Option<String>)> {
    let parts: Vec<_> = name.split('/').collect();
    if parts.len() == 2 && parts[0] == "files" && parts[1].starts_with("cfg-") {
        let directory = checked_data_path(base, &format!("backup/{name}"))?;
        let meta = checked_data_path(base, &format!("backup/{name}/metadata.json"))?;
        if std::fs::metadata(&meta)?.len() > 65536 {
            return Err(backup_error("备份元信息过大"));
        }
        let metadata: BackupMetadata =
            serde_json::from_slice(&std::fs::read(meta)?).map_err(io::Error::other)?;
        if metadata.version != 1 {
            return Err(backup_error("暂不支持此备份格式"));
        }
        checked_data_path(base, &metadata.target)?;
        let file = directory.join("content.bak");
        checked_data_path(base, &format!("backup/{name}/content.bak"))?;
        return Ok((file, metadata.target, Some(metadata.sha256)));
    }
    if parts.len() == 2 && parts[0] == "config" && parts[1].ends_with(".bak") {
        let source = checked_data_path(base, &format!("backup/{name}"))?;
        let target = crate::cfgeditor::backup_relative_target(parts[1])
            .ok_or_else(|| backup_error("旧备份没有准确的版本信息，不能自动恢复"))?;
        return Ok((source, target, None));
    }
    if parts.len() == 1 && name.ends_with(".bak") {
        let source = checked_data_path(base, &format!("backup/{name}"))?;
        return Ok((source, legacy_target(base, name)?, None));
    }
    Err(backup_error("备份标识无效"))
}

pub fn list_backup_files(base: &Path) -> io::Result<Vec<BackupFile>> {
    let mut output = Vec::new();
    for area in ["", "config", "files"] {
        let directory = checked_data_path(
            base,
            &if area.is_empty() {
                "backup".into()
            } else {
                format!("backup/{area}")
            },
        )?;
        let entries = match std::fs::read_dir(directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            Err(error) => return Err(error),
        };
        for entry in entries {
            let entry = entry?;
            let metadata = std::fs::symlink_metadata(entry.path())?;
            if linked(&metadata) {
                continue;
            }
            let filename = entry.file_name().to_string_lossy().to_string();
            if area == "files" {
                if !metadata.is_dir() || !filename.starts_with("cfg-") {
                    continue;
                }
            } else if !metadata.is_file() || !filename.ends_with(".bak") {
                continue;
            }
            let name = if area.is_empty() {
                filename
            } else {
                format!("{area}/{filename}")
            };
            let path = if area == "files" {
                entry.path().join("content.bak")
            } else {
                entry.path()
            };
            if std::fs::symlink_metadata(&path).is_ok_and(|meta| linked(&meta)) {
                continue;
            }
            let (metadata, payload_error) = match std::fs::metadata(&path) {
                Ok(meta) if meta.is_file() => (meta, None),
                Ok(_) => (metadata, Some("备份内容不是普通文件".to_string())),
                Err(error) => (metadata, Some(format!("无法读取备份内容：{error}"))),
            };
            let (target_path, reason) = match backup_source(base, &name) {
                Ok((_, relative, _)) if restorable_config_target(&relative) => {
                    match checked_data_path(base, &relative) {
                        Ok(_) => (Some(relative), None),
                        Err(error) => (Some(relative), Some(error.to_string())),
                    }
                }
                Ok((_, relative, _)) => (
                    Some(relative),
                    Some("此备份不是服务配置，请在对应功能页处理".into()),
                ),
                Err(error) => (None, Some(error.to_string())),
            };
            let size_bytes = if payload_error.is_some() {
                0
            } else {
                metadata.len()
            };
            let reason = payload_error.or(reason);
            output.push(BackupFile {
                name,
                path: path.to_string_lossy().into(),
                size_bytes,
                modified_at: metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_millis() as i64,
                target_path,
                restorable: reason.is_none(),
                reason,
                sort_time: metadata
                    .modified()?
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos(),
            });
        }
    }
    output.sort_by(|a, b| {
        b.sort_time
            .cmp(&a.sort_time)
            .then_with(|| b.name.cmp(&a.name))
    });
    Ok(output)
}

fn read_backup_snapshot(
    base: &Path,
    name: &str,
) -> io::Result<(BackupPreview, Vec<u8>, Option<Vec<u8>>)> {
    let (source, relative, expected_hash) = backup_source(base, name)?;
    if !restorable_config_target(&relative) {
        return Err(backup_error("只能从此入口恢复服务配置"));
    }
    let target = checked_data_path(base, &relative)?;
    let content = std::fs::read(source)?;
    let digest = hex::encode(Sha256::digest(&content));
    if expected_hash.is_some_and(|hash| hash != digest) {
        return Err(backup_error("备份内容校验失败，原配置未改动"));
    }
    let content = rebase_backup_content(base, &target, content)?;
    let relocated_digest = hex::encode(Sha256::digest(&content));
    let current = read_optional(&target)?;
    let revision = hex::encode(Sha256::digest(
        serde_json::to_vec(&(
            name,
            &relative,
            &digest,
            &relocated_digest,
            current.as_ref().map(|v| hex::encode(Sha256::digest(v))),
        ))
        .map_err(io::Error::other)?,
    ));
    Ok((
        BackupPreview {
            name: name.into(),
            target_path: target.to_string_lossy().into(),
            target_relative: relative,
            current_exists: current.is_some(),
            revision,
            backup_content: backup_preview_text(&content),
            current_content: current.as_deref().and_then(backup_preview_text),
            backup_size_bytes: content.len() as u64,
            current_size_bytes: current.as_ref().map_or(0, |bytes| bytes.len() as u64),
            changed: current.as_deref() != Some(content.as_slice()),
        },
        content,
        current,
    ))
}

pub fn preview_backup(base: &Path, name: &str) -> io::Result<BackupPreview> {
    read_backup_snapshot(base, name).map(|(preview, _, _)| preview)
}

pub fn restore_backup_checked(
    base: &Path,
    name: &str,
    expected_revision: Option<&str>,
) -> io::Result<PathBuf> {
    let (preview, content, current) = read_backup_snapshot(base, name)?;
    if expected_revision.is_some_and(|revision| preview.revision != revision) {
        return Err(backup_error("配置或备份已变化，请重新预览后恢复"));
    }
    let content = String::from_utf8(content).map_err(io::Error::other)?;
    let target = checked_data_path(base, &preview.target_relative)?;
    write_with_backup_expected(
        &target,
        &content,
        &base.join("backup"),
        Some(current.as_deref()),
    )?;
    Ok(target)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "macos")]
    #[test]
    fn system_aliases_allow_real_files_but_still_reject_nested_links() {
        for (alias, target) in [
            ("/var", "/private/var"),
            ("/tmp", "/private/tmp"),
            ("/etc", "/private/etc"),
        ] {
            assert_eq!(system_path(Path::new(alias)), Path::new(target));
            assert_eq!(
                checked_data_path(Path::new("/"), &alias[1..]).unwrap(),
                Path::new(alias)
            );
        }
        let temp = tempfile::tempdir_in("/tmp").unwrap();
        let original = temp.path().join("original");
        std::fs::create_dir(&original).unwrap();
        std::fs::write(original.join("cert.pem"), "certificate").unwrap();
        let linked = temp.path().join("linked");
        std::os::unix::fs::symlink(&original, &linked).unwrap();
        let relative = original
            .join("cert.pem")
            .strip_prefix("/")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        let actual = checked_data_path(Path::new("/"), &relative).unwrap();
        assert_eq!(std::fs::read_to_string(actual).unwrap(), "certificate");
        let relative = linked
            .join("cert.pem")
            .strip_prefix("/")
            .unwrap()
            .to_str()
            .unwrap()
            .to_owned();
        assert!(checked_data_path(Path::new("/"), &relative).is_err());
        assert!(checked_data_path(temp.path(), "linked/cert.pem").is_err());
        assert_eq!(
            system_path(Path::new("/tmp-extra/cert.pem")),
            Path::new("/tmp-extra/cert.pem")
        );
    }

    #[test]
    fn platform_data_defaults_preserve_legacy_data_and_never_write_inside_new_mac_apps() {
        let temp = tempfile::tempdir().unwrap();
        let executable = temp.path().join("NiceEnv.app/Contents/MacOS/niceservbay");
        assert!(adjacent_data_dir(&executable, "macos").unwrap().is_none());
        assert!(!temp.path().join("NiceEnv.app").exists());

        let legacy = executable.parent().unwrap().join("nsb-data");
        std::fs::create_dir_all(&legacy).unwrap();
        std::fs::write(legacy.join("nsb.sqlite"), "existing data").unwrap();
        assert_eq!(
            adjacent_data_dir(&executable, "macos").unwrap(),
            Some(legacy.clone())
        );
        assert_eq!(
            std::fs::read_to_string(legacy.join("nsb.sqlite")).unwrap(),
            "existing data"
        );
        assert_eq!(
            validate_update_data_location(&legacy, "macos")
                .unwrap_err()
                .code,
            "DATA_DIR_IN_APP_BUNDLE"
        );
        assert!(validate_update_data_location(&legacy, "windows").is_ok());
        let migrated = temp.path().join("Application Support/NiceEnv");
        std::fs::create_dir_all(&migrated).unwrap();
        assert!(validate_update_data_location(&migrated, "macos").is_ok());
        #[cfg(unix)]
        {
            let alias = temp.path().join("data-alias");
            std::os::unix::fs::symlink(&legacy, &alias).unwrap();
            assert_eq!(
                validate_update_data_location(&alias, "macos")
                    .unwrap_err()
                    .code,
                "DATA_DIR_IN_APP_BUNDLE"
            );
        }

        let installed = temp.path().join("Program Files/NiceEnv/niceservbay.exe");
        let portable = installed.parent().unwrap().join("nsb-data");
        std::fs::create_dir_all(&portable).unwrap();
        std::fs::write(portable.join(".write-probe"), "user contents").unwrap();
        assert_eq!(
            adjacent_data_dir(&installed, "windows").unwrap(),
            Some(portable.clone())
        );
        assert_eq!(
            std::fs::read_to_string(portable.join(".write-probe")).unwrap(),
            "user contents"
        );
        assert_eq!(std::fs::read_dir(portable).unwrap().count(), 1);
    }

    #[test]
    fn native_and_cross_target_builds_do_not_store_user_data_under_target() {
        let temp = tempfile::tempdir().unwrap();
        for relative in [
            "target/debug/niceservbay.exe",
            "target/release/niceservbay.exe",
            "target/debug/deps/nsbctl.exe",
            "target/x86_64-pc-windows-msvc/release/nsbctl.exe",
            "target/aarch64-apple-darwin/release/niceservbay",
        ] {
            for os in ["windows", "macos"] {
                assert!(
                    adjacent_data_dir(&temp.path().join(relative), os)
                        .unwrap()
                        .is_none(),
                    "{os}: {relative}"
                );
            }
        }
        assert!(!temp.path().join("target").exists());
    }

    #[cfg(unix)]
    #[test]
    fn unix_service_paths_preserve_literal_backslashes_in_filenames() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join(r"config\archive.json");
        std::fs::write(&path, "unchanged").unwrap();
        let argument = portable_path_text(&path);
        assert_eq!(std::fs::read_to_string(&argument).unwrap(), "unchanged");
        assert_eq!(argument, path.to_string_lossy());
        assert_eq!(
            nginx_path(&path),
            path.to_string_lossy().replace('\\', "\\\\")
        );
    }

    #[test]
    fn postgres_runtime_directory_is_version_scoped_and_separate_from_apache() {
        let paths = Paths::new(PathBuf::from("/tmp/niceenv-paths"));
        assert_ne!(paths.postgres_run_dir("16.6"), paths.apache_run_dir());
        assert_eq!(
            paths.postgres_run_dir("16.6"),
            PathBuf::from("/tmp/niceenv-paths/etc/postgresql/16.6/run")
        );
        assert_ne!(paths.postgres_run_dir("16.6"), paths.postgres_run_dir("17.6"));
    }

    fn fixture() -> (tempfile::TempDir, Paths) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("app data"));
        paths.ensure_dirs().unwrap();
        (temp, paths)
    }

    #[test]
    fn data_dir_copy_is_atomic_and_requires_an_empty_target() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        let source_store = crate::store::Store::open(paths.db()).unwrap();
        source_store.set_setting("snapshot-fixture", "includes-wal").unwrap();
        std::fs::write(paths.etc().join("marker.ini"), b"preserve me").unwrap();
        std::fs::write(source.join(".niceenv-control.lock"), b"live lock").unwrap();
        std::fs::write(source.join(".niceenv-control.json"), b"private endpoint").unwrap();
        let target = temp.path().join("target");

        let result = copy_data_dir(&source, &target).unwrap();
        assert_eq!(result.files, 2);
        let copied_store = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        assert_eq!(copied_store.get_setting("snapshot-fixture").as_deref(), Some("includes-wal"));
        assert_eq!(std::fs::read(target.join("etc/marker.ini")).unwrap(), b"preserve me");
        assert!(!target.join(".niceenv-control.lock").exists());
        assert!(!target.join(".niceenv-control.json").exists());
        assert_eq!(std::fs::read(source.join(".niceenv-control.json")).unwrap(), b"private endpoint");

        std::fs::write(target.join("keep.txt"), b"do not overwrite").unwrap();
        let error = copy_data_dir(&source, &target).unwrap_err();
        assert_eq!(error.code, "DATA_DIR_NOT_EMPTY");
        assert_eq!(std::fs::read(target.join("keep.txt")).unwrap(), b"do not overwrite");
    }

    #[test]
    fn data_dir_selection_persists_and_failed_launch_restores_previous_choice() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("application-config/data-directory.json");
        let source = temp.path().join("original");
        let target = temp.path().join("relocated");
        let _source = crate::store::Store::open(source.join("nsb.sqlite")).unwrap();
        let _target = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        assert!(read_data_dir_selection(&file).unwrap().is_none());
        let fail = || Err::<(),_>(crate::error::AppError::new("LAUNCH_FAILED", "fixture"));
        assert_eq!(with_selection_file(&file, &target, fail).unwrap_err().code, "LAUNCH_FAILED");
        assert!(!file.exists());
        with_selection_file(&file, &source, || Ok(())).unwrap();
        let old = std::fs::read(&file).unwrap();
        with_selection_file(&file, &target, fail).unwrap_err();
        assert_eq!(std::fs::read(&file).unwrap(), old);
        with_selection_file(&file, &target, || {
            assert_eq!(read_data_dir_selection(&file).unwrap().as_deref(), Some(target.as_path()));
            Ok(())
        }).unwrap();
        assert_eq!(read_data_dir_selection(&file).unwrap(), Some(target));
    }

    #[test]
    fn data_dir_selection_rejects_unavailable_corrupt_and_concurrent_choices() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("data-directory.json");
        let missing = temp.path().join("missing");
        std::fs::write(&file, serde_json::to_vec(&DataDirSelection {version:1,path:missing.clone(),retired:Vec::new()}).unwrap()).unwrap();
        assert!(read_data_dir_selection(&file).is_err());
        assert!(!missing.exists());
        std::fs::write(&file, "broken").unwrap();
        assert_eq!(read_data_dir_selection(&file).unwrap_err().code, "DATA_DIR_SETTINGS");
        let root = temp.path().join("data");
        let _store = crate::store::Store::open(root.join("nsb.sqlite")).unwrap();
        assert_eq!(with_selection_file(&file, &root, || Ok(())).unwrap_err().code, "DATA_DIR_SETTINGS");
        std::fs::remove_file(&file).unwrap();
        with_selection_file(&file, &root, || {
            assert_eq!(with_selection_file(&file, &root, || Ok(())).unwrap_err().code, "DATA_DIR_BUSY");
            Ok(())
        }).unwrap();
        let invalid = temp.path().join("invalid");
        std::fs::create_dir(&invalid).unwrap();
        std::fs::write(invalid.join("nsb.sqlite"), "not a database").unwrap();
        let old = std::fs::read(&file).unwrap();
        assert!(with_selection_file(&file, &invalid, || Ok(())).is_err());
        assert_eq!(std::fs::read(&file).unwrap(), old);
    }

    #[test]
    fn data_dir_handoff_retires_old_roots_and_rolls_back_failed_launch() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("config/selection.json");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        let final_root = temp.path().join("final");
        let original = crate::store::Store::open(source.join("nsb.sqlite")).unwrap();
        original.set_setting("handoff-value", "original").unwrap();
        let _target = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        let _final = crate::store::Store::open(final_root.join("nsb.sqlite")).unwrap();
        with_selection_file(&file,&source,||Ok(())).unwrap();
        let before = std::fs::read(&file).unwrap();
        let shared = DataDirActivity::shared(&source).unwrap();
        assert_eq!(shared.select_with_file(&file,&target,||Ok(())).unwrap_err().code,"DATA_DIR_BUSY");
        drop(shared);
        let guard = DataDirActivity::exclusive(&source).unwrap();
        guard.select_with_file(&file,&target,|| {
            assert_eq!(redirected_data_dir(&source).unwrap(),Some(target.clone()));
            Err::<(),_>(crate::error::AppError::new("LAUNCH_FAILED","fixture"))
        }).unwrap_err();
        assert_eq!(std::fs::read(&file).unwrap(),before);
        assert!(redirected_data_dir(&source).unwrap().is_none());
        guard.select_with_file(&file,&target,||Ok(())).unwrap();
        drop(guard);
        assert!(matches!(DataDirActivity::shared(&source),Err(error) if error.code=="DATA_DIR_RELOCATED"));
        assert!(matches!(DataDirActivity::exclusive(&source),Err(error) if error.code=="DATA_DIR_RELOCATED"));
        assert!(DataDirActivity::shared(&target).is_ok());
        assert!(crate::CoreState::init(Some(source.clone()),std::sync::Arc::new(|_|{})).is_err());
        let output = std::process::Command::new(std::env::current_exe().unwrap())
            .args(["--exact","paths::tests::data_dir_handoff_child_probe","--nocapture"])
            .env("NSB_HANDOFF_PROBE_ROOT",&source).output().unwrap();
        assert!(output.status.success(),"{}\n{}",String::from_utf8_lossy(&output.stdout),String::from_utf8_lossy(&output.stderr));
        let next = DataDirActivity::exclusive(&target).unwrap();
        next.select_with_file(&file,&final_root,||Ok(())).unwrap();
        drop(next);
        assert_eq!(redirected_data_dir(&source).unwrap(),Some(final_root.clone()));
        assert_eq!(redirected_data_dir(&target).unwrap(),Some(final_root));
        assert_eq!(original.get_setting("handoff-value").as_deref(),Some("original"));
    }

    #[test]
    fn data_dir_handoff_restores_selection_after_real_child_failure_but_not_uncertain_cleanup() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("config/selection.json");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        let _source = crate::store::Store::open(source.join("nsb.sqlite")).unwrap();
        let store = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        store.set_setting("pathEnvEnabled", "0").unwrap();
        store.set_setting_json("pathEnvDirs", &vec!["original-path"]).unwrap();
        let marker_file = target.join(".data-dir-activation.json");
        let marker = serde_json::to_vec(&source).unwrap();
        std::fs::write(&marker_file, &marker).unwrap();
        with_selection_file(&file, &source, || Ok(())).unwrap();
        let previous = std::fs::read(&file).unwrap();
        let guard = DataDirActivity::exclusive(&source).unwrap();
        for mode in ["fail", "hang"] {
            let error = guard
                .select_with_file(&file, &target, || {
                    let rollback = crate::pathenv::MigrationActivationRollback::capture(&target)?;
                    let mut command = platform::command(std::env::current_exe().unwrap());
                    command
                        .args([
                            "--exact",
                            "restart::tests::restart_child_probe",
                            "--nocapture",
                        ])
                        .env("NSB_RESTART_PROBE_MODE", mode)
                        .env("NSB_RESTART_PROBE_ROOT", &target)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null());
                    let result = crate::restart::launch_and_wait(
                        &mut command,
                        &target,
                        std::time::Duration::from_secs(2),
                    );
                    // 模拟新进程激活副本后出现后续初始化错误；仅写临时副本。
                    store.set_setting_json("pathEnvDirs", &vec!["changed-path"])?;
                    std::fs::remove_file(&marker_file)?;
                    rollback.restore()?;
                    result
                })
                .unwrap_err();
            assert_eq!(
                error.code,
                if mode == "fail" {
                    "FIXTURE_INIT_FAILED"
                } else {
                    "RESTART_TIMEOUT"
                },
                "{error:?}"
            );
            assert_eq!(std::fs::read(&file).unwrap(), previous);
            assert!(redirected_data_dir(&source).unwrap().is_none());
            assert_eq!(std::fs::read(&marker_file).unwrap(), marker);
            assert_eq!(store.get_setting("pathEnvDirs").as_deref(), Some("[\"original-path\"]"));
            let pid: u32 = std::fs::read_to_string(target.join("child-pid")).unwrap().parse().unwrap();
            assert!(!platform::process_alive(pid));
        }
        let error = guard.select_with_file(&file, &target, || Err::<(), _>(crate::error::AppError::new(
            "RESTART_CHILD_CLEANUP_FAILED", "fixture: child cleanup could not be verified"))).unwrap_err();
        assert_eq!(error.code, "RESTART_CHILD_CLEANUP_FAILED");
        assert_eq!(redirected_data_dir(&source).unwrap(), Some(target));
        drop(guard);
        assert!(matches!(DataDirActivity::shared(&source), Err(error) if error.code=="DATA_DIR_RELOCATED"));
    }

    #[test]
    fn data_dir_handoff_child_probe() {
        let Some(root) = std::env::var_os("NSB_HANDOFF_PROBE_ROOT") else { return; };
        let root = PathBuf::from(root);
        assert!(matches!(DataDirActivity::shared(&root),Err(error) if error.code=="DATA_DIR_RELOCATED"));
    }

    #[test]
    fn data_dir_handoff_uncommitted_reference_and_corruption_are_distinguished() {
        let temp = tempfile::tempdir().unwrap();
        let file = temp.path().join("selection.json");
        let source = temp.path().join("source");
        let target = temp.path().join("target");
        let _source = crate::store::Store::open(source.join("nsb.sqlite")).unwrap();
        let _target = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        let guard = DataDirActivity::exclusive(&source).unwrap();
        guard.select_with_file(&file,&target,||Err::<(),_>(crate::error::AppError::new("LAUNCH_FAILED","fixture"))).unwrap_err();
        drop(guard);
        assert!(!file.exists());
        assert!(DataDirActivity::shared(&source).is_ok());
        let copied = temp.path().join("copied");
        copy_data_dir(&source,&copied).unwrap();
        assert!(!copied.join(".data-dir-authority.json").exists());
        std::fs::write(&file,"broken").unwrap();
        assert!(matches!(DataDirActivity::shared(&source),Err(error) if error.code=="DATA_DIR_SETTINGS"));
        std::fs::remove_file(&file).unwrap();
        std::fs::write(source.join(".data-dir-authority.json"),"broken").unwrap();
        assert!(DataDirActivity::shared(&source).is_err());
    }

    #[test]
    fn data_dir_path_rebase_preserves_boundaries_and_path_styles() {
        let old = if cfg!(windows) {
            "C:/old-data"
        } else {
            "/old-data"
        };
        let new = if cfg!(windows) {
            "D:/new-data"
        } else {
            "/new-data"
        };
        let rebase = DataPathRebase::new(Path::new(old), Path::new(new)).unwrap();
        let structured_target = format!("{new} with spaces #资料");
        let structured =
            DataPathRebase::new(Path::new(old), Path::new(&structured_target)).unwrap();
        let nginx = format!(
            "# keep {old}\r\nhttp {{\r\n server {{\r\n  root \"{old}/www\";\r\n  add_header X-Audit \"{old}/literal\";\r\n  proxy_set_header X-Secret \"{old}/literal\";\r\n  fastcgi_param APP_SECRET \"{old}/literal\";\r\n  fastcgi_param SCRIPT_FILENAME \"{old}/www/index.php\";\r\n  set $secret \"{old}/literal\";\r\n  location \"{old}/url\" {{ return 200 \"{old}/literal\"; }}\r\n }}\r\n}}\r\n"
        );
        let httpd = format!(
            "# keep {old}\r\nDocumentRoot \"{old}/www\"\r\nHeader set X-Audit \"{old}/literal\"\r\nRequestHeader set X-Secret \"{old}/literal\"\r\nSetEnv APP_SECRET \"{old}/literal\"\r\nDefine APP_SECRET \"{old}/literal\"\r\nAlias \"{old}/url\" \"{old}/www/assets\"\r\n<Location \"{old}/url\">\r\nErrorDocument 404 \"{old}/literal\"\r\n</Location>\r\n"
        );
        let expected_nginx = nginx
            .replace(
                &format!("root \"{old}/www\""),
                &format!("root \"{structured_target}/www\""),
            )
            .replace(
                &format!("SCRIPT_FILENAME \"{old}/www/index.php\""),
                &format!("SCRIPT_FILENAME \"{structured_target}/www/index.php\""),
            );
        let expected_httpd = httpd
            .replace(
                &format!("DocumentRoot \"{old}/www\""),
                &format!("DocumentRoot \"{structured_target}/www\""),
            )
            .replace(
                &format!("\"{old}/www/assets\""),
                &format!("\"{structured_target}/www/assets\""),
            );
        assert_eq!(
            (
                structured
                    .config_text(Path::new("etc/nginx/nginx.conf"), &nginx)
                    .unwrap(),
                structured
                    .config_text(Path::new("etc/apache/httpd.conf"), &httpd)
                    .unwrap(),
            ),
            (expected_nginx, expected_httpd),
            "physical paths must migrate without changing headers, URLs or application secrets"
        );
        let mapped_literal = format!("map $host $secret {{ root \"{old}/literal\"; default \"{old}/literal\"; }} add_header X-Secret $secret;");
        assert_eq!(
            crate::configgen::rebase_nginx_config(&mapped_literal, &structured).unwrap(),
            mapped_literal
        );
        let generated = crate::configgen::render_httpd_conf(
            &Paths::new(PathBuf::from(old)),
            &PathBuf::from(format!("{old}/Apache24")),
            &[],
            8080,
            8443,
        );
        let generated = crate::configgen::rebase_httpd_config(&generated, &structured).unwrap();
        assert!(generated.contains(&format!(
            "Define NSB_ETC \"{structured_target}/etc/apache\""
        )));
        assert!(generated.contains("SSLSessionCache \"shmcb:${NSB_ETC}/logs/ssl_scache(512000)\""));
        let cache = format!("SSLSessionCache \"shmcb:{old}/cache(512000)\"\n");
        assert_eq!(
            crate::configgen::rebase_httpd_config(&cache, &structured).unwrap(),
            format!("SSLSessionCache \"shmcb:{structured_target}/cache(512000)\"\n")
        );
        let aliases = format!("<Location /files>\r\nAlias \"{old}/www\"\r\n</Location>\r\nAlias \"{old}/url\" \\\r\n \"{old}/www\"\r\nLoadFile \"{old}/one.so\" \"{old}/two.so\"\r\n");
        let expected = aliases
            .replace(&format!("{old}/www"), &format!("{structured_target}/www"))
            .replace(
                &format!("{old}/one.so"),
                &format!("{structured_target}/one.so"),
            )
            .replace(
                &format!("{old}/two.so"),
                &format!("{structured_target}/two.so"),
            );
        assert_eq!(
            crate::configgen::rebase_httpd_config(&aliases, &structured).unwrap(),
            expected
        );
        for nginx in [
            format!("set $base \"{old}\"; set $nested $base/www; root $nested;"),
            format!("map $host $base {{ default \"{old}\"; }} root $base;"),
            format!("geo $base {{ default \"{old}\"; }} root ${{base}};"),
            format!("split_clients $request_id $base {{ * \"{old}\"; }} root $base;"),
        ] {
            assert_eq!(
                crate::configgen::rebase_nginx_config(&nginx, &structured)
                    .unwrap_err()
                    .code,
                "DATA_DIR_CONFIG_VARIABLE"
            );
        }
        let macro_path = format!("Define BASE \"{old}\"\nDefine NESTED \"${{BASE}}/www\"\nDocumentRoot \"${{NESTED}}\"\n");
        assert_eq!(
            crate::configgen::rebase_httpd_config(&macro_path, &structured)
                .unwrap_err()
                .code,
            "DATA_DIR_CONFIG_VARIABLE"
        );
        let mixed_macro = format!("Define NSB_ETC \"{old}/etc/apache\"\nDocumentRoot \"${{NSB_ETC}}/htdocs\"\nHeader set X-Literal \"${{NSB_ETC}}\"\n");
        assert_eq!(
            crate::configgen::rebase_httpd_config(&mixed_macro, &structured)
                .unwrap_err()
                .code,
            "DATA_DIR_CONFIG_VARIABLE"
        );
        let yaml = format!("# 中文注释 {old}/unchanged\r\nshared: &shared '{old}/shared'\r\nsecret: *shared\r\nstorage: {{dbPath: *shared}} # keep flow\r\nsystemLog:\r\n  path: &log \"{old}/mongo.log\" # keep log comment\r\nnet:\r\n  tls:\r\n    certificateKeyFile: |- # keep block comment\r\n      {old}/tls.pem\r\ncustom:\r\n  password: *log\r\n  external: '{old} sibling/file'\r\n");
        let updated = structured
            .config_text(Path::new("etc/mongodb/8.0/mongod.conf"), &yaml)
            .unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
        assert_eq!(
            decoded["storage"]["dbPath"].as_str(),
            Some(format!("{structured_target}/shared").as_str())
        );
        assert_eq!(
            decoded["systemLog"]["path"].as_str(),
            Some(format!("{structured_target}/mongo.log").as_str())
        );
        assert_eq!(
            decoded["net"]["tls"]["certificateKeyFile"].as_str(),
            Some(format!("{structured_target}/tls.pem").as_str())
        );
        assert_eq!(
            decoded["secret"].as_str(),
            Some(format!("{old}/shared").as_str())
        );
        assert_eq!(
            decoded["custom"]["password"].as_str(),
            Some(format!("{old}/mongo.log").as_str())
        );
        for comment in [
            "# 中文注释",
            "# keep flow",
            "# keep log comment",
            "# keep block comment",
        ] {
            assert!(updated.contains(comment), "{updated}");
        }
        assert!(updated.contains(&format!("external: '{old} sibling/file'")));
        assert!(!updated.replace("\r\n", "").contains('\n'));
        assert_eq!(
            structured
                .config_text(Path::new("etc/mongodb/8.0/mongod.conf"), &updated)
                .unwrap(),
            updated
        );
        let merged = format!("defaults: &defaults {{dbPath: '{old}/db', enabled: true, count: 4, blank: null}}\nstorage: {{<<: [*defaults]}}\ncustom: *defaults\n");
        let updated = structured
            .config_text(Path::new("etc/mongodb/8.0/mongod.conf"), &merged)
            .unwrap();
        let mut decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
        decoded.apply_merge().unwrap();
        assert_eq!(
            decoded["storage"]["dbPath"].as_str(),
            Some(format!("{structured_target}/db").as_str())
        );
        assert_eq!(
            decoded["custom"]["dbPath"].as_str(),
            Some(format!("{old}/db").as_str())
        );
        assert_eq!(decoded["storage"]["enabled"].as_bool(), Some(true));
        assert_eq!(decoded["storage"]["count"].as_i64(), Some(4));
        let original = format!("{{\n  \"data_provider\": {{\"driver\": \"sqlite\", \"name\": \"{old}/users.db\", \"password\": \"{old}/secret\"}},\n  \"sftpd\": {{\"host_keys\": [\"{old}/key\", \"{old} sibling/key\"]}},\n  \"httpd\": {{\"templates_path\": \"{old}/templates\"}}\n}}\n");
        let updated = structured
            .config_text(Path::new("etc/sftpgo/2.6/sftpgo.json"), &original)
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            decoded["data_provider"]["name"],
            format!("{structured_target}/users.db")
        );
        assert_eq!(
            decoded["data_provider"]["password"],
            format!("{old}/secret")
        );
        assert_eq!(
            decoded["sftpd"]["host_keys"][0],
            format!("{structured_target}/key")
        );
        assert_eq!(
            decoded["sftpd"]["host_keys"][1],
            format!("{old} sibling/key")
        );
        assert_eq!(updated.lines().count(), original.lines().count());
        let remote = original.replace("sqlite", "mysql");
        let updated = structured
            .config_text(Path::new("etc/sftpgo/2.6/sftpgo.json"), &remote)
            .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&updated).unwrap()["data_provider"]["name"],
            format!("{old}/users.db")
        );
        let qdrant = format!("service: {{api_key: '{old}/credential'}}\nstorage: {{storage_path: '{old}/storage', snapshots_path: '{old}/snapshots'}}\n");
        let updated = structured
            .config_text(Path::new("runtimes/qdrant/1.0/config/local.yaml"), &qdrant)
            .unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
        assert_eq!(
            decoded["service"]["api_key"].as_str(),
            Some(format!("{old}/credential").as_str())
        );
        assert_eq!(
            decoded["storage"]["snapshots_path"].as_str(),
            Some(format!("{structured_target}/snapshots").as_str())
        );
        let mihomo = format!("secret: '{old}/credential'\nproxy-providers:\n  local: {{type: file, path: '{old}/providers/local.yaml'}}\nproxies:\n  - {{name: preserved, password: '{old}/credential'}}\n");
        let updated = structured
            .config_text(Path::new("etc/mihomo/profiles/local.yaml"), &mihomo)
            .unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
        assert_eq!(
            decoded["secret"].as_str(),
            Some(format!("{old}/credential").as_str())
        );
        assert_eq!(
            decoded["proxy-providers"]["local"]["path"].as_str(),
            Some(format!("{structured_target}/providers/local.yaml").as_str())
        );
        assert_eq!(
            decoded["proxies"][0]["password"].as_str(),
            Some(format!("{old}/credential").as_str())
        );
        let secret_only = format!("secret: '{old}/do-not-edit' # keep bytes\r\n");
        assert_eq!(
            structured
                .config_text(Path::new("etc/mihomo/config.yaml"), &secret_only)
                .unwrap(),
            secret_only
        );
        let literal_merge = serde_json::json!({"data_provider": {"driver": "sqlite", "name": format!("{old}/accounts.db"), "<<": {"name": format!("{old}/literal")}}}).to_string();
        let updated = structured
            .config_text(Path::new("etc/sftpgo/1/sftpgo.json"), &literal_merge)
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            decoded["data_provider"]["name"],
            format!("{structured_target}/accounts.db")
        );
        assert_eq!(
            decoded["data_provider"]["<<"]["name"],
            format!("{old}/literal")
        );
        let connected = serde_json::json!({"data_provider": {"driver": "sqlite", "connection_string": "file:accounts.db", "name": format!("{old}/literal")}}).to_string();
        assert_eq!(
            structured
                .config_text(Path::new("etc/sftpgo/1/sftpgo.json"), &connected)
                .unwrap(),
            connected
        );
        for driver in ["sqlite", "bolt", "mysql"] {
            let mixed = serde_json::json!({"Data_Provider":{"DRİVER":driver,"NaMe":format!("{old}/accounts.db"),"PASSWORD":format!("{old}/secret")}}).to_string();
            for name in ["etc/sftpgo/sftpgo.json", "etc/sftpgo/sftpgo.yaml"] {
                let changed = structured.config_text(Path::new(name), &mixed).unwrap();
                let decoded: serde_json::Value = serde_json::from_str(&changed).unwrap();
                assert_eq!(
                    decoded["Data_Provider"]["NaMe"],
                    format!(
                        "{}/accounts.db",
                        if driver == "mysql" {
                            old
                        } else {
                            &structured_target
                        }
                    )
                );
                assert_eq!(
                    decoded["Data_Provider"]["PASSWORD"],
                    format!("{old}/secret")
                );
                assert!(decoded.get("data_provider").is_none());
                assert_eq!(
                    structured.config_text(Path::new(name), &changed).unwrap(),
                    changed
                );
            }
        }
        let mixed = format!("# preserve casing and comment\nshared: &provider\n  DRIVER: sqlite\n  NaMe: '{old}/accounts.db'\n  Password: '{old}/secret'\nData_Provider:\n  <<: *provider\nHTTPD:\n  BINDINGS:\n    - Certificate_File: '{old}/tls.pem'\n");
        let changed = structured
            .config_text(Path::new("etc/sftpgo/sftpgo.yaml"), &mixed)
            .unwrap();
        let mut decoded: yaml_serde::Value = yaml_serde::from_str(&changed).unwrap();
        decoded.apply_merge().unwrap();
        assert_eq!(
            decoded["Data_Provider"]["NaMe"].as_str(),
            Some(format!("{structured_target}/accounts.db").as_str())
        );
        assert_eq!(
            decoded["shared"]["NaMe"].as_str(),
            Some(format!("{old}/accounts.db").as_str())
        );
        assert_eq!(
            decoded["Data_Provider"]["Password"].as_str(),
            Some(format!("{old}/secret").as_str())
        );
        assert_eq!(
            decoded["HTTPD"]["BINDINGS"][0]["Certificate_File"].as_str(),
            Some(format!("{structured_target}/tls.pem").as_str())
        );
        assert!(changed.starts_with("# preserve casing and comment\n"));
        assert_eq!(
            structured
                .config_text(Path::new("etc/sftpgo/sftpgo.yaml"), &changed)
                .unwrap(),
            changed
        );
        for duplicate in [
            r#"{"Data_Provider":{"NAME":"private"},"data_provider":{"name":"other"}}"#,
            r#"{"HTTPD":{"BINDINGS":[{"PORT":8080,"port":8081}]}}"#,
            "DATA_PROVIDER:\n  Name: private\n  name: other\n",
        ] {
            let error = structured
                .config_text(Path::new("etc/sftpgo/sftpgo.yaml"), duplicate)
                .unwrap_err();
            assert_eq!(error.code, "SFTPGO_CONFIG_AMBIGUOUS");
            assert!(!format!("{error:?}").contains("private"));
        }
        let tagged = format!("systemLog:\n  path: &log !!str\n    # preserve tagged scalar comment\n    '{old}/mongo.log'\ncustom: *log\n");
        let updated = structured
            .config_text(Path::new("etc/mongodb/mongod.conf"), &tagged)
            .unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
        assert_eq!(
            decoded["systemLog"]["path"].as_str(),
            Some(format!("{structured_target}/mongo.log").as_str())
        );
        assert_eq!(
            decoded["custom"].as_str(),
            Some(format!("{old}/mongo.log").as_str())
        );
        assert!(updated.contains("&log !!str\n    # preserve tagged scalar comment"));
        #[cfg(unix)]
        {
            assert!(
                crate::configpaths::service(Path::new(r"etc/sftpgo/1/other\sftpgo.json")).is_none()
            );
            assert!(crate::configpaths::service(Path::new(r"etc\qdrant/1/config.yaml")).is_none());
            let old = r"/old-data/literal\old";
            let new = r"/new-data/literal\new #资料";
            let rebase = DataPathRebase::new(Path::new(old), Path::new(new)).unwrap();
            let config = serde_json::json!({"data_provider": {"driver": "sqlite", "name": format!("{old}/accounts.db"), "password": format!("{old}/secret")}}).to_string();
            for name in ["etc/sftpgo/1/sftpgo.json", "etc/sftpgo/1/sftpgo.yaml"] {
                let updated = rebase.config_text(Path::new(name), &config).unwrap();
                let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
                assert_eq!(
                    decoded["data_provider"]["name"].as_str(),
                    Some(format!("{new}/accounts.db").as_str())
                );
                assert_eq!(
                    decoded["data_provider"]["password"].as_str(),
                    Some(format!("{old}/secret").as_str())
                );
            }
            let config = format!(
                "storage:\n  storage_path: '{old}/storage'\nservice:\n  api_key: '{old}/secret'\n"
            );
            let updated = rebase
                .config_text(Path::new("etc/qdrant/config.yaml"), &config)
                .unwrap();
            let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
            assert_eq!(
                decoded["storage"]["storage_path"].as_str(),
                Some(format!("{new}/storage").as_str())
            );
            assert_eq!(
                decoded["service"]["api_key"].as_str(),
                Some(format!("{old}/secret").as_str())
            );
        }
        assert!(structured
            .config_text(
                Path::new("etc/sftpgo/1/sftpgo.json"),
                "{\"data_provider\": {\"name\": \"unterminated}"
            )
            .is_err());
        for invalid in [
            "storage: [",
            "storage: {dbPath: /old}\n---\nsecret: private",
            "{\"data_provider\": {\"name\": \"unterminated}",
        ] {
            assert!(structured
                .config_text(Path::new("etc/mongodb/mongod.conf"), invalid)
                .is_err());
        }
        let caddy_target = format!("{new} with spaces");
        for name in [
            "etc/mihomo/config.yaml.disabled",
            "runtimes/mihomo/config.yaml.disabled",
        ] {
            let updated = structured.config_text(Path::new(name), &mihomo).unwrap();
            let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
            assert_eq!(
                decoded["secret"].as_str(),
                Some(format!("{old}/credential").as_str())
            );
            assert_eq!(
                decoded["proxy-providers"]["local"]["path"].as_str(),
                Some(format!("{structured_target}/providers/local.yaml").as_str())
            );
        }
        #[cfg(windows)]
        {
            let updated = structured
                .config_text(Path::new(r"EtC\MiHoMo\CONFIG.YAML.DISABLED"), &mihomo)
                .unwrap();
            assert_eq!(
                yaml_serde::from_str::<yaml_serde::Value>(&updated).unwrap()["secret"].as_str(),
                Some(format!("{old}/credential").as_str())
            );
        }
        let uri = crate::configpaths::sqlite_file_uri(&Path::new(old).join("data/accounts #1.db"));
        assert!(
            crate::configpaths::sqlite_connection_path("file:temporary?mode=memory&mode=rw")
                .is_err()
        );
        assert!(crate::configpaths::sqlite_connection_path("file:/bad%00name.db").is_err());
        let opaque = format!("{uri}?private=%FF&mode=rw");
        assert_eq!(
            crate::configpaths::sqlite_connection_path(&opaque).unwrap(),
            Some(Path::new(old).join("data/accounts #1.db"))
        );
        let dsn = format!("{uri}?mode=rw&_auth_pass={old}/secret&cache=shared#unchanged");
        let config = serde_json::json!({"data_provider":{"driver":"sqlite", "connection_string":dsn, "name":format!("{old}/unused.db"), "password":dsn}}).to_string();
        let mixed_dsn =
            serde_json::json!({"DATA_PROVIDER":{"DRIVER":"sqlite","CONNECTION_STRING":dsn}})
                .to_string();
        let changed = structured
            .config_text(Path::new("etc/sftpgo/sftpgo.json"), &mixed_dsn)
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&changed).unwrap();
        assert_eq!(
            crate::configpaths::sqlite_connection_path(
                decoded["DATA_PROVIDER"]["CONNECTION_STRING"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            Some(Path::new(&structured_target).join("data/accounts #1.db"))
        );
        let updated = structured
            .config_text(Path::new("etc/sftpgo/1/sftpgo.json"), &config)
            .unwrap();
        let decoded: serde_json::Value = serde_json::from_str(&updated).unwrap();
        assert_eq!(
            decoded["data_provider"]["connection_string"],
            format!(
                "{}?mode=rw&_auth_pass={old}/secret&cache=shared#unchanged",
                crate::configpaths::sqlite_file_uri(
                    &Path::new(&structured_target).join("data/accounts #1.db")
                )
            )
        );
        assert_eq!(decoded["data_provider"]["password"], dsn);
        assert_eq!(decoded["data_provider"]["name"], format!("{old}/unused.db"));
        let file = format!("{old}/资料 #1.pem");
        let remote = format!("https://example.invalid/{old}/unchanged");
        let sftpgo_paths = serde_json::json!({
            "common":{"temp_path":file,"server_version":file},
            "data_provider":{"driver":"postgresql","name":file,"password":file,
                "connection_string":file,"root_cert":file,"client_cert":file,"client_key":file,"users_base_dir":file},
            "sftpd":{"login_banner_file":file,"host_certificates":[file,"relative-cert.pub"],"opkssh_path":file,"opkssh_checksum":file},
            "ftpd":{"banner_file":file,"bindings":[{"certificate_file":file,"certificate_key_file":file}],
                "ca_certificates":[file],"ca_revocation_lists":[file]},
            "webdavd":{"bindings":[{"certificate_file":file,"certificate_key_file":file,"prefix":file}],
                "ca_certificates":[file],"ca_revocation_lists":[file]},
            "httpd":{"signing_passphrase_file":file,"signing_passphrase":file,"ca_revocation_lists":[file],
                "bindings":[{"oidc":{"client_secret_file":file,"client_secret":file,"config_url":remote},
                    "branding":{"web_admin":{"logo_path":file,"favicon_path":file,"disclaimer_path":file}}}]},
            "http":{"ca_certificates":[file],"certificates":[{"cert":file,"key":file}],
                "headers":[{"key":"Authorization","value":file,"url":remote}]},
            "telemetry":{"auth_user_file":file,"certificate_file":file,"certificate_key_file":file},
            "kms":{"secrets":{"master_key_path":file,"master_key":file,"url":remote}},
            "acme":{"http01_challenge":{"webroot":file},"ca_endpoint":remote},
            "custom":{"login_banner_file":file}
        });
        let mut expected = sftpgo_paths.clone();
        for pointer in [
            "/common/temp_path",
            "/data_provider/root_cert",
            "/data_provider/client_cert",
            "/data_provider/client_key",
            "/data_provider/users_base_dir",
            "/sftpd/login_banner_file",
            "/sftpd/host_certificates/0",
            "/sftpd/opkssh_path",
            "/ftpd/banner_file",
            "/ftpd/bindings/0/certificate_file",
            "/ftpd/bindings/0/certificate_key_file",
            "/ftpd/ca_certificates/0",
            "/ftpd/ca_revocation_lists/0",
            "/webdavd/bindings/0/certificate_file",
            "/webdavd/bindings/0/certificate_key_file",
            "/webdavd/ca_certificates/0",
            "/webdavd/ca_revocation_lists/0",
            "/httpd/signing_passphrase_file",
            "/httpd/ca_revocation_lists/0",
            "/httpd/bindings/0/oidc/client_secret_file",
            "/http/ca_certificates/0",
            "/http/certificates/0/cert",
            "/http/certificates/0/key",
            "/telemetry/auth_user_file",
            "/telemetry/certificate_file",
            "/telemetry/certificate_key_file",
            "/kms/secrets/master_key_path",
            "/acme/http01_challenge/webroot",
        ] {
            *expected.pointer_mut(pointer).unwrap() =
                format!("{structured_target}/资料 #1.pem").into();
        }
        for (filename, config) in [
            (
                "sftpgo.json",
                serde_json::to_string_pretty(&sftpgo_paths).unwrap(),
            ),
            ("sftpgo.yaml", yaml_serde::to_string(&sftpgo_paths).unwrap()),
        ] {
            let relative = PathBuf::from("etc/sftpgo/1").join(filename);
            let updated = structured.config_text(&relative, &config).unwrap();
            let decoded: serde_json::Value = yaml_serde::from_str(&updated).unwrap();
            assert_eq!(decoded, expected, "{filename}");
            assert_eq!(
                structured.config_text(&relative, &updated).unwrap(),
                updated
            );
        }
        let encoded_old = format!("{old} #中文");
        let encoded_rebase =
            DataPathRebase::new(Path::new(&encoded_old), Path::new(&structured_target)).unwrap();
        let encoded_uri =
            crate::configpaths::sqlite_file_uri(&Path::new(&encoded_old).join("accounts.db"));
        let yaml = format!("shared: &dsn '{encoded_uri}?mode=rw'\ndata_provider:\n  driver: sqlite\n  connection_string: *dsn\n  password: *dsn\n");
        let updated = encoded_rebase
            .config_text(Path::new("etc/sftpgo/sftpgo.yaml"), &yaml)
            .unwrap();
        let decoded: yaml_serde::Value = yaml_serde::from_str(&updated).unwrap();
        assert_eq!(
            crate::configpaths::sqlite_connection_path(
                decoded["data_provider"]["connection_string"]
                    .as_str()
                    .unwrap()
            )
            .unwrap(),
            Some(Path::new(&structured_target).join("accounts.db"))
        );
        assert_eq!(
            decoded["data_provider"]["password"].as_str(),
            Some(format!("{encoded_uri}?mode=rw").as_str())
        );
        for connection in [
            ":memory:",
            "file::memory:?cache=shared",
            "file:temporary?mode=memory",
            "file:relative.db?mode=rw",
        ] {
            let config = serde_json::json!({"data_provider":{"driver":"sqlite","connection_string":connection}}).to_string();
            assert_eq!(
                structured
                    .config_text(Path::new("etc/sftpgo/sftpgo.json"), &config)
                    .unwrap(),
                config
            );
        }
        for name in ["data #资料.db", "literal%23name.db", r"literal\name.db"] {
            let path = Path::new(old).join(name);
            assert_eq!(
                crate::configpaths::sqlite_connection_path(&crate::configpaths::sqlite_file_uri(
                    &path
                ))
                .unwrap(),
                Some(PathBuf::from(portable_path_text(&path)))
            );
        }
        let caddy_rebase = DataPathRebase::new(Path::new(old), Path::new(&caddy_target)).unwrap();
        let caddy = format!("# preserve {old}/comment\n{{\n storage file_system {{\n root {old}/storage\n }}\n}}\nimport {old}/etc/caddy/sites/*.conf\nhttp://:8080 {{\n root * {old}/www\n tls {old}/cert.pem {old}/key.pem\n log {{\n output file {old}/logs/caddy.log\n }}\n respond \"{old}/literal body\"\n header X-External \"{old} sibling/file\"\n basic_auth {{\n user {old}/credential\n }}\n}}\n");
        let changed = caddy_rebase
            .config_text(Path::new("etc/caddy/2.11.4/Caddyfile"), &caddy)
            .unwrap();
        for suffix in [
            "storage",
            "www",
            "cert.pem",
            "key.pem",
            "logs/caddy.log",
            "etc/caddy/sites/*.conf",
        ] {
            assert!(
                changed.contains(&format!("\"{caddy_target}/{suffix}\"")),
                "{changed}"
            );
        }
        for line in [
            format!("# preserve {old}/comment"),
            format!("respond \"{old}/literal body\""),
            format!("header X-External \"{old} sibling/file\""),
            format!("user {old}/credential"),
        ] {
            assert!(changed.contains(&line), "{changed}");
        }
        assert_eq!(
            caddy_rebase
                .config_text(Path::new("etc/caddy/2.11.4/Caddyfile"), &changed)
                .unwrap(),
            changed
        );
        assert!(caddy_rebase
            .config_text(Path::new("etc/caddy/sites/a.conf"), "root * \"unterminated")
            .is_err());
        let unsafe_target =
            DataPathRebase::new(Path::new(old), Path::new(&format!("{new}[1]"))).unwrap();
        assert_eq!(
            unsafe_target
                .config_text(Path::new("etc/caddy/2.11.4/Caddyfile"), &caddy)
                .unwrap_err()
                .code,
            "CADDY_IMPORT_PATH"
        );
        #[cfg(unix)]
        {
            let unix_source = Path::new(r"/tmp/data\old");
            let unix_target = Path::new(r"/tmp/new\data with spaces");
            let rebase = DataPathRebase::new(unix_source, unix_target).unwrap();
            let original = format!(
                "import {}\nhttp://:8080 {{\n root * {}\n}}\n",
                crate::caddy::quoted_path_text(&format!(
                    "{}/*.conf",
                    escaped_glob_path(unix_source)
                )),
                crate::caddy::quoted_path_text(r"/tmp/data\old/www")
            );
            let updated = rebase
                .config_text(Path::new("etc/caddy/Caddyfile"), &original)
                .unwrap();
            assert!(updated.contains(&crate::caddy::quoted_path_text(&format!(
                "{}/*.conf",
                escaped_glob_path(unix_target)
            ))));
            assert!(updated.contains(&crate::caddy::quoted_path_text(
                r"/tmp/new\data with spaces/www"
            )));
        }
        for suffix in [
            " sibling/file",
            "[other]/file",
            ".external/file",
            "-external/file",
        ] {
            let external = format!("{old}{suffix}");
            assert_eq!(
                rebase.config_value(&external, str::to_owned).unwrap(),
                external
            );
            let pattern = format!("^{}", regex::escape(&external));
            assert_eq!(rebase.config_pattern(&pattern).unwrap(), pattern);
        }
        assert_eq!(
            rebase
                .config_pattern(&format!("^{}$", regex::escape(old)))
                .unwrap(),
            format!("^{}$", regex::escape(new))
        );
        assert!(rebase
            .config_value(
                &format!("{old}/folder with spaces/../../external"),
                str::to_owned
            )
            .is_err());
        assert!(rebase
            .config_pattern(&format!("^{}/\\.\\./external", regex::escape(old)))
            .is_err());
        let continued = format!("# 中文注释 {old}\\\r\nDocumentRoot {old}/comment\r\nDocumentRoot {old}/www\\\r\n/site\r\nAlias /external \"{old} sibling/site\"\r\n");
        let expected = format!("# 中文注释 {old}\\\r\nDocumentRoot {old}/comment\r\nDocumentRoot \"{new}/www/site\"\r\nAlias /external \"{old} sibling/site\"\r\n");
        assert_eq!(
            crate::configgen::rebase_httpd_config(&continued, &rebase).unwrap(),
            expected
        );
        assert_eq!(
            rebase.path(&format!("{old}/etc/a.ini")),
            format!("{new}/etc/a.ini")
        );
        assert_eq!(
            rebase.path(&format!("{old}-external/a")),
            format!("{old}-external/a")
        );
        let value = format!("include \"{old}/etc/*.conf\";\nroot {old}-external/www;\nurl https://example.test{old}/assets;");
        assert_eq!(rebase.text(&value).unwrap(), format!("include \"{new}/etc/*.conf\";\nroot {old}-external/www;\nurl https://example.test{old}/assets;"));
        if cfg!(windows) {
            assert_eq!(
                crate::configgen::rebase_nginx_config(
                    r#"include "//?/C:/old-data/etc/*.conf";"#,
                    &rebase
                )
                .unwrap(),
                r#"include "D:/new-data/etc/*.conf";"#
            );
            assert_eq!(
                rebase.path("c:\\OLD-data\\runtime"),
                "D:\\new-data\\runtime"
            );
            assert_eq!(
                rebase.text(r#"{"path":"C:\\old-data\\etc"}"#).unwrap(),
                r#"{"path":"D:\\new-data\\etc"}"#
            );
            assert_eq!(
                rebase.text(r#"root "\\?\C:\old-data\www";"#).unwrap(),
                r#"root "D:\new-data\www";"#
            );
            let unc = DataPathRebase::new(
                Path::new(r"\\?\UNC\server\share\old"),
                Path::new("D:/new-data"),
            )
            .unwrap();
            assert_eq!(
                unc.text(r#"root "\\?\UNC\server\share\old\www";"#).unwrap(),
                r#"root "D:\new-data\www";"#
            );
        } else {
            // Unix 文件名的反斜杠不能触发整条路径的 Windows 样式转换。
            assert_eq!(
                rebase.path(&format!("{old}/etc/config\\archive.json")),
                format!("{new}/etc/config\\archive.json")
            );
            assert_eq!(
                rebase
                    .text(&format!("root \"{old}\\external/file\";"))
                    .unwrap(),
                format!("root \"{old}\\external/file\";")
            );
        }
        assert_eq!(
            rebase.path(&format!("{old}/../external")),
            format!("{old}/../external")
        );
        assert!(rebase
            .text(&format!("root \"{old}/../external\";"))
            .is_err());
        let spaced =
            DataPathRebase::new(Path::new(old), Path::new(&format!("{new} with space"))).unwrap();
        assert!(spaced.text(&format!("command {old}/tool")).is_err());
        assert_eq!(
            spaced.text(&format!("command \"{old}/tool\"")).unwrap(),
            format!("command \"{new} with space/tool\"")
        );
        let source = PathBuf::from(format!("{old}/source [old]"));
        let target = PathBuf::from(format!("{new}/target [new]"));
        #[cfg(unix)]
        let (source, target) = (source.join(r"literal\old"), target.join(r"literal\new"));
        let from = Paths::new(source.clone());
        let to = Paths::new(target.clone());
        let rebase = DataPathRebase::new(&source, &target).unwrap();
        let nginx = |paths: &Paths| {
            crate::configgen::render_nginx_conf(
                paths,
                &paths.base.join("runtimes/nginx"),
                8080,
                8443,
                &[],
                None,
            )
        };
        assert_eq!(
            rebase
                .config_text(Path::new("etc/nginx/nginx.conf"), &nginx(&from))
                .unwrap(),
            nginx(&to)
        );
        #[cfg(unix)]
        for (old_name, new_name) in [("plain", "bracket[1]"), ("bracket[1]", "plain")] {
            let from = Paths::new(
                PathBuf::from("/old-data")
                    .join(old_name)
                    .join(r"literal\old"),
            );
            let to = Paths::new(
                PathBuf::from("/new-data")
                    .join(new_name)
                    .join(r"literal\new"),
            );
            let rebase = DataPathRebase::new(&from.base, &to.base).unwrap();
            let render = |paths: &Paths| {
                crate::configgen::render_nginx_conf(
                    paths,
                    &paths.base.join(r"runtime\nginx"),
                    8080,
                    8443,
                    &[],
                    None,
                )
            };
            assert_eq!(
                rebase
                    .config_text(Path::new("etc/nginx/nginx.conf"), &render(&from))
                    .unwrap(),
                render(&to)
            );
        }
        let apache = |paths: &Paths| {
            crate::configgen::render_httpd_conf(
                paths,
                &paths.base.join("runtimes/apache"),
                &[],
                8080,
                8443,
            )
        };
        assert_eq!(
            rebase
                .config_text(Path::new("etc/apache/httpd.conf"), &apache(&from))
                .unwrap(),
            apache(&to)
        );
        let mut site: crate::model::Site = serde_json::from_value(serde_json::json!({
            "id":"migration", "name":"migration", "domains":["migration.test"], "rootDir":source.join("www/project [root]"),
            "runtime":{"kind":"php","webServer":"apache"}, "https":false, "rewrite":"none", "createdAt":1,"updatedAt":1
        })).unwrap();
        let old_vhost =
            crate::configgen::render_httpd_vhost(&site, 8080, 8443, &from.certs(), Some(19000));
        site.root_dir = portable_path_text(&target.join("www/project [root]"));
        let new_vhost =
            crate::configgen::render_httpd_vhost(&site, 8080, 8443, &to.certs(), Some(19000));
        // Apache 接受 \. 和 \\. 两种配置写法，比较解码后的语义。
        let normalize = |text: &str| {
            text.lines()
                .map(crate::configgen::httpd_argument)
                .collect::<Vec<_>>()
        };
        assert_eq!(
            normalize(
                &rebase
                    .config_text(Path::new("etc/apache/sites/migration.conf"), &old_vhost)
                    .unwrap()
            ),
            normalize(&new_vhost)
        );
        let php = |paths: &Paths| {
            crate::configgen::render_php_ini(paths, "8.4", &paths.base.join("runtimes/php/8.4"))
        };
        assert_eq!(
            rebase
                .config_text(Path::new("etc/php/8.4/php.ini"), &php(&from))
                .unwrap(),
            php(&to)
        );
        let mysql = |paths: &Paths| {
            crate::configgen::render_mysql_ini(
                paths,
                "8.4",
                &paths.base.join("runtimes/mysql/8.4"),
                3306,
            )
        };
        assert_eq!(
            rebase
                .config_text(Path::new("etc/mysql/8.4/my.ini"), &mysql(&from))
                .unwrap(),
            mysql(&to)
        );
        let old = quoted_config_path(&source);
        let new = quoted_config_path(&target);
        let sep = if cfg!(windows) { ';' } else { ':' };
        let php_custom = format!("; keep {old}\r\n[PHP]\r\ninclude_path=\"{old}/lib{sep}{old}/shared{sep}{old} sibling\" ; note\r\nsession.save_path=\"2;0600;{old}/session\"\r\npdo_password=\"{old}/secret\"\r\n");
        let expected = format!("; keep {old}\r\n[PHP]\r\ninclude_path=\"{new}/lib{sep}{new}/shared{sep}{old} sibling\" ; note\r\nsession.save_path=\"2;0600;{new}/session\"\r\npdo_password=\"{old}/secret\"\r\n");
        assert_eq!(
            rebase
                .config_text(Path::new("www/.user.ini"), &php_custom)
                .unwrap(),
            expected
        );
        let mysql_custom = format!("# keep {old}\n[mysqld]\nloose-log-error = \"{old}/logs/error.log\" # note\n[client]\npassword=\"{old}/secret\"\n");
        let expected = format!("# keep {old}\n[mysqld]\nloose-log-error = \"{new}/logs/error.log\" # note\n[client]\npassword=\"{old}/secret\"\n");
        assert_eq!(
            rebase
                .config_text(Path::new("etc/mariadb/11/my.ini"), &mysql_custom)
                .unwrap(),
            expected
        );
        let redis = |paths: &Paths| crate::configgen::render_redis_conf(paths, "7", 6379);
        assert_eq!(
            rebase
                .config_text(Path::new("etc/redis/7/redis.conf"), &redis(&from))
                .unwrap(),
            redis(&to)
        );
        let redis_custom = format!("# keep {old}\r\ndir \"{old}/data\"\r\nrequirepass \"{old}/secret\"\r\nloadmodule \"{old}/module.so\" password \"{old}/secret\"\r\n");
        let expected = format!("# keep {old}\r\ndir \"{new}/data\"\r\nrequirepass \"{old}/secret\"\r\nloadmodule \"{new}/module.so\" password \"{old}/secret\"\r\n");
        assert_eq!(
            rebase
                .config_text(Path::new("etc/redis/7/redis.conf"), &redis_custom)
                .unwrap(),
            expected
        );
        let include = |base: &Path| {
            format!(
                "include \"{}/*.conf\"\n",
                escaped_posix_glob_text(&portable_path_text(&base.join("conf.d")))
                    .replace('\\', "\\\\")
                    .replace('"', "\\\"")
            )
        };
        assert_eq!(
            rebase
                .config_text(Path::new("etc/redis/7/redis.conf"), &include(&source))
                .unwrap(),
            include(&target)
        );
        assert_eq!(
            rebase
                .config_text(
                    Path::new("etc/redis/5/redis.conf"),
                    &format!("include \"{old}/extra.conf\"\n")
                )
                .unwrap(),
            format!("include \"{new}/extra.conf\"\n")
        );
        let php_single = format!(
            "error_log='{}'\n",
            portable_path_text(&source.join("logs/error.log"))
        );
        let expected = format!(
            "error_log='{}'\n",
            portable_path_text(&target.join("logs/error.log"))
        );
        assert_eq!(
            rebase
                .config_text(Path::new("etc/php/8.4/php.ini"), &php_single)
                .unwrap(),
            expected
        );
        let php_dynamic = format!("error_log=\"{old}/logs/${{NAME}}.log\"\n");
        assert_eq!(
            rebase
                .config_text(Path::new("etc/php/8.4/php.ini"), &php_dynamic)
                .unwrap(),
            format!("error_log=\"{new}/logs/${{NAME}}.log\"\n")
        );
        let redis_hex = format!("dir \"\\x{:02x}{}/data\"\n", old.as_bytes()[0], &old[1..]);
        assert_eq!(
            rebase
                .config_text(Path::new("etc/redis/7/redis.conf"), &redis_hex)
                .unwrap(),
            format!("dir \"{new}/data\"\n")
        );
        let mysql_unquoted = format!("[mysqld]\ntmpdir={old}/data{sep}/external\\\\folder\n");
        assert_eq!(
            rebase
                .config_text(Path::new("etc/mysql/8.4/my.ini"), &mysql_unquoted)
                .unwrap(),
            format!("[mysqld]\ntmpdir=\"{new}/data{sep}/external\\\\folder\"\n")
        );
        #[cfg(unix)]
        {
            // PHP 未知转义保留反斜杠，用户手写的单反斜杠也指向同一真实目录。
            let php = format!(
                "error_log=\"{}/logs/error.log\"\n",
                portable_path_text(&source)
            );
            assert_eq!(
                rebase
                    .config_text(Path::new("etc/php/8.4/php.ini"), &php)
                    .unwrap(),
                format!("error_log=\"{new}/logs/error.log\"\n")
            );
        }
    }

    #[test]
    fn data_dir_copy_rebases_live_records_configs_and_keeps_external_and_historical_data() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        let store = crate::store::Store::open(paths.db()).unwrap();
        let runtime = paths.runtime_dir("fixture", "1");
        std::fs::create_dir_all(&runtime).unwrap();
        let binary = if cfg!(windows) { "fixture.cmd" } else { "fixture.sh" };
        std::fs::write(runtime.join(binary), if cfg!(windows) { "@echo fixture-runnable\r\n" } else { "#!/bin/sh\necho fixture-runnable\n" }).unwrap();
        #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(runtime.join(binary), std::fs::Permissions::from_mode(0o755)).unwrap(); }
        store.upsert_installed(&crate::model::InstalledPackage {id:"fixture".into(),version:"1".into(),category:"runtime".into(),install_path:runtime.to_string_lossy().into_owned(),config_path:paths.etc().to_string_lossy().into_owned(),installed_at:123}).unwrap();
        let old = portable_path_text(&source);
        let external = format!("{old}-external");
        let config = format!("root \"{old}/www\";\ninclude \"{old}/etc/*.conf\";\nexternal \"{external}/etc\";\n");
        std::fs::write(paths.nginx_conf(), &config).unwrap();
        std::fs::write(paths.backup().join("original.conf"), &config).unwrap();
        #[cfg(windows)]
        {
            std::fs::rename(paths.etc(), source.join("EtC")).unwrap();
            std::fs::rename(paths.backup(), source.join("BACKUP")).unwrap();
            let directory = source.join("EtC/MiHoMo");
            std::fs::create_dir_all(&directory).unwrap();
            std::fs::write(
                directory.join("CONFIG.YAML.DISABLED"),
                format!("external-ui: '{old}/ui'\nsecret: '{old}/private'\n"),
            )
            .unwrap();
        }
        store.set_setting("mysqlRootPassword", &format!("{old}/secret")).unwrap();
        store.set_setting("pathEnvDirs", &format!(r#"["{old}/runtimes/fixture/1"]"#)).unwrap();
        let sftpgo = source.join("etc/sftpgo/1");
        std::fs::create_dir_all(sftpgo.join("templates/invalid")).unwrap();
        let opaque = format!("{old}/private-value");
        let fake_config = serde_json::json!({"kms":{"secrets":{"master_key_path":format!("{old}/etc/nginx/nginx.conf")}}}).to_string();
        for (name, bytes) in [
            ("templates/banner.conf", opaque.as_bytes()),
            ("templates/sftpgo.json", fake_config.as_bytes()),
            (
                "templates/invalid/sftpgo.json",
                b"not a configuration".as_slice(),
            ),
            ("key.conf", opaque.as_bytes()),
        ] {
            std::fs::write(sftpgo.join(name), bytes).unwrap();
        }
        let opaque_binary = sftpgo.join("large.pem");
        let mut binary_resource = std::fs::File::create(&opaque_binary).unwrap();
        binary_resource
            .write_all(format!("{old}/opaque\0").as_bytes())
            .unwrap();
        binary_resource.set_len(17 * 1024 * 1024).unwrap();
        drop(binary_resource);
        let resource_config = format!("shared: &resource\n  Templates_Path: '{old}/etc/sftpgo/1/templates'\nHTTPD:\n  <<: *resource\n  Signing_Passphrase_File: 'templates/../key.conf'\nSFTPD:\n  Host_Keys: ['large.pem']\n");
        std::fs::write(sftpgo.join("sftpgo.yaml"), &resource_config).unwrap();
        let conn = rusqlite::Connection::open(paths.db()).unwrap();
        conn.execute("INSERT INTO sites(id,name,domains,root_dir,runtime,https,rewrite,created_at,updated_at) VALUES('site','site','[]',?1,?2,0,'\"none\"',1,2)", rusqlite::params![format!("{old}/www"),serde_json::json!({"kind":"node","cwd":format!("{old}/www"),"command":format!("\"{old}/runtimes/fixture/1/{binary}\""),"custom":"preserve","application":{"version":"1","cwd":format!("{old}/app"),"args":[format!("{old}/app/server.js"),format!("{external}/file"),"literal & ; argument"]}}).to_string()]).unwrap();
        conn.execute("INSERT INTO certs(id,kind,subject,sans,not_before,not_after,cert_path,key_path) VALUES('cert','imported','test','[]',0,1,?1,?2)", rusqlite::params![format!("{old}/certs/site.pem"),format!("{external}/key.pem")]).unwrap();
        let target = temp.path().join("target");
        let result = copy_data_dir(&source, &target).unwrap();
        assert_eq!(result.rewritten_files, if cfg!(windows) { 3 } else { 2 });
        for name in [
            "templates/banner.conf",
            "templates/sftpgo.json",
            "templates/invalid/sftpgo.json",
            "key.conf",
            "large.pem",
        ] {
            assert_eq!(
                std::fs::read(target.join("etc/sftpgo/1").join(name)).unwrap(),
                std::fs::read(sftpgo.join(name)).unwrap(),
                "{name}"
            );
        }
        let new = result.path.clone();
        let copied = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        let installed = copied.list_installed().unwrap().remove(0);
        assert_eq!(portable_path_text(Path::new(&installed.install_path)),format!("{new}/runtimes/fixture/1"));
        assert_eq!(copied.list_sites().unwrap()[0].root_dir,format!("{new}/www"));
        let application = copied.list_sites().unwrap()[0].runtime.application.clone().unwrap();
        assert_eq!(application.cwd.as_deref(), Some(format!("{new}/app").as_str()));
        assert_eq!(application.args, vec![format!("{new}/app/server.js"), format!("{external}/file"), "literal & ; argument".into()]);
        assert_eq!(store.list_sites().unwrap()[0].runtime.application.as_ref().unwrap().args[0], format!("{old}/app/server.js"));
        assert_eq!(copied.list_certs().unwrap()[0].cert_path,format!("{new}/certs/site.pem"));
        assert_eq!(copied.list_certs().unwrap()[0].key_path.as_deref(),Some(format!("{external}/key.pem").as_str()));
        assert_eq!(copied.get_setting("mysqlRootPassword"),store.get_setting("mysqlRootPassword"));
        assert_eq!(copied.get_setting("pathEnvDirs"),store.get_setting("pathEnvDirs"));
        assert_eq!(std::fs::read_to_string(paths.nginx_conf()).unwrap(), config);
        assert_eq!(std::fs::read_to_string(target.join("backup/original.conf")).unwrap(), config);
        let migrated = std::fs::read_to_string(target.join("etc/nginx/nginx.conf")).unwrap();
        assert!(migrated.contains(&format!("{new}/etc")) && migrated.contains(&external));
        #[cfg(windows)]
        {
            let content =
                std::fs::read_to_string(target.join("EtC/MiHoMo/CONFIG.YAML.DISABLED")).unwrap();
            let value: yaml_serde::Value = yaml_serde::from_str(&content).unwrap();
            assert_eq!(
                value["external-ui"].as_str(),
                Some(format!("{new}/ui").as_str())
            );
            assert_eq!(
                value["secret"].as_str(),
                Some(format!("{old}/private").as_str())
            );
        }
        drop(conn); drop(store);
        std::fs::rename(&source,temp.path().join("unavailable-original")).unwrap();
        let output = platform::command(Path::new(&installed.install_path).join(binary)).output().unwrap();
        assert!(output.status.success());
        assert!(String::from_utf8_lossy(&output.stdout).contains("fixture-runnable"));
    }

    #[test]
    fn data_dir_copy_failure_leaves_source_and_target_untouched() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        let _store = crate::store::Store::open(paths.db()).unwrap();
        let old = portable_path_text(&source);
        let content = format!("include \"{old}/etc/*.conf;");
        std::fs::write(paths.nginx_conf(), &content).unwrap();
        let target = temp.path().join("contains space");
        std::fs::create_dir(&target).unwrap();
        assert_eq!(
            copy_data_dir(&source, &target).unwrap_err().code,
            "CONFIG_STRUCTURE"
        );
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        assert_eq!(
            std::fs::read_to_string(paths.nginx_conf()).unwrap(),
            content
        );
        assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry| entry
            .unwrap()
            .file_name()
            .to_string_lossy()
            .contains("-migrating-")));
        std::fs::write(paths.nginx_conf(), "# valid configuration\n").unwrap();
        for (main, include, definition, reference) in [
            (
                paths.nginx_conf(),
                paths.nginx_sites_dir().join("vars.conf"),
                format!("set $base \"{old}\";"),
                "root $base;",
            ),
            (
                paths.apache_conf(),
                paths.apache_sites_dir().join("vars.conf"),
                format!("Define BASE \"{old}\"\n"),
                "DocumentRoot \"${BASE}\"\n",
            ),
        ] {
            std::fs::create_dir_all(include.parent().unwrap()).unwrap();
            std::fs::write(&main, &definition).unwrap();
            std::fs::write(&include, reference).unwrap();
            assert_eq!(
                copy_data_dir(&source, &target).unwrap_err().code,
                "DATA_DIR_CONFIG_VARIABLE"
            );
            assert_eq!(std::fs::read_to_string(&main).unwrap(), definition);
            assert_eq!(std::fs::read_to_string(&include).unwrap(), reference);
            assert!(std::fs::read_dir(&target).unwrap().next().is_none());
            std::fs::write(main, "# valid configuration\n").unwrap();
            std::fs::remove_file(include).unwrap();
        }
        let mongo = source.join("etc/mongodb/8.0/mongod.conf");
        std::fs::create_dir_all(mongo.parent().unwrap()).unwrap();
        std::fs::write(&mongo, "storage: [ invalid\n").unwrap();
        assert_eq!(
            copy_data_dir(&source, &target).unwrap_err().code,
            "DATA_DIR_CONFIG_FORMAT"
        );
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        assert_eq!(
            std::fs::read_to_string(&mongo).unwrap(),
            "storage: [ invalid\n"
        );
        std::fs::write(&mongo, "storage: {}\n").unwrap();
        let sftpgo = source.join("etc/sftpgo/1/sftpgo.json");
        std::fs::create_dir_all(sftpgo.parent().unwrap()).unwrap();
        // 同一文件既是配置又是提示文本时，不能为了通过迁移而改写其中一种用途。
        let content = serde_json::json!({"sftpd":{"login_banner_file":"sftpgo.json"}}).to_string();
        std::fs::write(&sftpgo, &content).unwrap();
        assert_eq!(
            copy_data_dir(&source, &target).unwrap_err().code,
            "DATA_DIR_RESOURCE_CONFLICT"
        );
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        assert_eq!(std::fs::read_to_string(&sftpgo).unwrap(), content);
    }

    #[test]
    fn sftpgo_account_paths_migrate_without_changing_credentials_or_virtual_paths() {
        for prefix in ["", "custom_\""] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source data");
            let target = temp.path().join("新 data # [1]");
            let paths = Paths::new(source.clone());
            paths.ensure_dirs().unwrap();
            drop(crate::store::Store::open(paths.db()).unwrap());
            let directory = source.join("etc/sftpgo/1");
            std::fs::create_dir_all(&directory).unwrap();
            let old = portable_path_text(&source);
            let quote = |name: &str| format!("\"{}{}\"", prefix.replace('"', "\"\""), name);
            let users = quote("users");
            let folders = quote("folders");
            let groups = quote("groups");
            let database = directory.join("accounts #1.db");
            let conn = rusqlite::Connection::open(&database).unwrap();
            conn.execute_batch(&format!(
                "CREATE TABLE {users}(id INTEGER PRIMARY KEY, home_dir TEXT NOT NULL, password TEXT, permissions TEXT, filesystem TEXT);
                 CREATE TABLE {folders}(id INTEGER PRIMARY KEY, path TEXT, filesystem TEXT);
                 CREATE TABLE {groups}(id INTEGER PRIMARY KEY, user_settings TEXT, description TEXT);"
            )).unwrap();
            let secret = format!("{old}/literal-password");
            let permissions = serde_json::json!({format!("{old}/virtual"): ["list"]}).to_string();
            let filesystem = serde_json::json!({"provider": 5, "sftpconfig": {"prefix": format!("{old}/remote")}}).to_string();
            conn.execute(&format!("INSERT INTO {users} VALUES(1,?1,?2,?3,?4)"),
                rusqlite::params![format!("{old}/data/sftpgo/home"), secret, permissions, filesystem]).unwrap();
            conn.execute(&format!("INSERT INTO {users} VALUES(2,?1,?2,?3,?4)"),
                rusqlite::params![format!("{old} sibling/home"), secret, permissions, filesystem]).unwrap();
            conn.execute(&format!("INSERT INTO {folders} VALUES(1,?1,?2)"),
                rusqlite::params![format!("{old}/data/sftpgo/shared"), filesystem]).unwrap();
            let settings = serde_json::json!({"home_dir": format!("{old}/data/sftpgo/%username%"),
                "filesystem": {"s3config": {"key_prefix": format!("{old}/remote-prefix")}},
                "filters": {"file_patterns": [{"path": format!("{old}/virtual"),"allowed_patterns":["*"]}]}});
            conn.execute(&format!("INSERT INTO {groups} VALUES(1,?1,?2)"),
                rusqlite::params![settings.to_string(), secret]).unwrap();
            conn.execute(&format!("INSERT INTO {groups} VALUES(2,?1,?2)"),
                rusqlite::params![settings.to_string().into_bytes(), secret]).unwrap();
            drop(conn);
            let uploads = source.join("data/sftpgo/home");
            std::fs::create_dir_all(&uploads).unwrap();
            std::fs::write(uploads.join("uploaded.ini"), format!("path={old}/literal-upload")).unwrap();
            std::fs::write(directory.join("sftpgo.json"), serde_json::json!({
                "data_provider":{"driver":"sqlite","name":"accounts #1.db","sql_tables_prefix":prefix}
            }).to_string()).unwrap();
            let before = std::fs::read(&database).unwrap();
            let migrated = copy_data_dir(&source, &target).unwrap();
            let target = PathBuf::from(migrated.path);
            assert_eq!(std::fs::read(&database).unwrap(), before);
            assert_eq!(std::fs::read_to_string(target.join("data/sftpgo/home/uploaded.ini")).unwrap(), format!("path={old}/literal-upload"));
            let copied = target.join("etc/sftpgo/1/accounts #1.db");
            let conn = rusqlite::Connection::open(&copied).unwrap();
            let row: (String, String, String, String) = conn.query_row(
                &format!("SELECT home_dir,password,permissions,filesystem FROM {users} WHERE id=1"),
                [], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?))).unwrap();
            assert_eq!(row, (portable_path_text(&target.join("data/sftpgo/home")), secret.clone(), permissions, filesystem.clone()));
            assert_eq!(conn.query_row(&format!("SELECT home_dir FROM {users} WHERE id=2"), [], |r|r.get::<_,String>(0)).unwrap(), format!("{old} sibling/home"));
            assert_eq!(conn.query_row(&format!("SELECT path FROM {folders} WHERE id=1"), [], |r|r.get::<_,String>(0)).unwrap(), portable_path_text(&target.join("data/sftpgo/shared")));
            let (actual, description): (String, String) = conn.query_row(
                &format!("SELECT user_settings,description FROM {groups} WHERE id=1"), [], |r|Ok((r.get(0)?,r.get(1)?))).unwrap();
            let mut expected = settings;
            expected["home_dir"] = portable_path_text(&target.join("data/sftpgo/%username%")).into();
            assert_eq!(serde_json::from_str::<serde_json::Value>(&actual).unwrap(), expected);
            let binary: Vec<u8> = conn.query_row(&format!("SELECT user_settings FROM {groups} WHERE id=2"),
                [], |r|r.get(0)).unwrap();
            assert_eq!(serde_json::from_slice::<serde_json::Value>(&binary).unwrap(), expected);
            assert_eq!(description, secret);
            assert_eq!(conn.query_row(&format!("SELECT filesystem FROM {folders}"), [], |r|r.get::<_,String>(0)).unwrap(), filesystem);
            drop(conn);
            let stable = std::fs::read(&copied).unwrap();
            assert!(!crate::sftpgo_data::rebase_provider(
                &crate::sftpgo_data::Provider {
                    path: copied.clone(),
                    driver: "sqlite".into(),
                    prefix: prefix.into(),
                    external: false,
                    executable: None,
                },
                &DataPathRebase::new(&source, &target).unwrap()
            )
            .unwrap());
            assert_eq!(std::fs::read(copied).unwrap(), stable);
        }
    }

    #[test]
    fn sftpgo_account_migration_rejects_invalid_and_external_databases_without_switching() {
        for case in [
            "invalid-group",
            "external",
            "bolt",
            "provider-conflict",
            "mysql",
            "postgresql",
            "cockroachdb",
            "memory",
            "unknown-driver",
        ] {
            let temp = tempfile::tempdir().unwrap();
            let source = temp.path().join("source");
            let target = temp.path().join("destination");
            let paths = Paths::new(source.clone());
            paths.ensure_dirs().unwrap();
            drop(crate::store::Store::open(paths.db()).unwrap());
            let directory = source.join("etc/sftpgo/1");
            std::fs::create_dir_all(&directory).unwrap();
            let database = if case == "external" { temp.path().join("outside.db") } else { directory.join("accounts.db") };
            let conn = rusqlite::Connection::open(&database).unwrap();
            conn.execute_batch("CREATE TABLE users(id INTEGER PRIMARY KEY,home_dir TEXT);
                CREATE TABLE folders(id INTEGER PRIMARY KEY,path TEXT);
                CREATE TABLE groups(id INTEGER PRIMARY KEY,user_settings TEXT);").unwrap();
            conn.execute("INSERT INTO users VALUES(1,?1)", [portable_path_text(&source.join("data/home"))]).unwrap();
            conn.execute("INSERT INTO groups VALUES(1,?1)", [if case == "invalid-group" {"broken JSON"} else {"{}"}]).unwrap();
            drop(conn);
            let unsupported = matches!(
                case,
                "mysql" | "postgresql" | "cockroachdb" | "memory" | "unknown-driver"
            );
            let remote = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
            remote.set_nonblocking(true).unwrap();
            let config = serde_json::json!({"data_provider":{
                "driver":if unsupported {case} else if case == "bolt" {"bolt"} else {"sqlite"},
                "name":portable_path_text(&database), "host":"127.0.0.1", "port":remote.local_addr().unwrap().port(),
                "password":"fixture-only-remote-secret"}});
            let config_path = directory.join("sftpgo.json");
            std::fs::write(&config_path, config.to_string()).unwrap();
            if case == "provider-conflict" {
                let mut second = config.clone();
                second["data_provider"]["sql_tables_prefix"] = "different_".into();
                std::fs::write(directory.join("sftpgo.yaml"), second.to_string()).unwrap();
            }
            let before = std::fs::read(&database).unwrap();
            let error = copy_data_dir(&source, &target).unwrap_err();
            assert_eq!(
                error.code,
                if unsupported {
                    "DATA_DIR_SFTPGO_PROVIDER"
                } else if case == "bolt" {
                    "DATA_DIR_SFTPGO_BOLT"
                } else {
                    "DATA_DIR_SFTPGO_STATE"
                },
                "{case}"
            );
            assert!(!format!("{error:?}").contains("fixture-only-remote-secret"));
            assert_eq!(
                std::fs::read_to_string(&config_path).unwrap(),
                config.to_string()
            );
            assert!(
                matches!(remote.accept(), Err(error) if error.kind() == std::io::ErrorKind::WouldBlock)
            );
            assert_eq!(std::fs::read(&database).unwrap(), before);
            assert!(!target.exists());
            assert!(!std::fs::read_dir(temp.path()).unwrap().any(|e| e.unwrap().file_name().to_string_lossy().contains("-migrating-")));
        }
    }

    #[test]
    #[ignore = "requires NSB_SFTPGO_NATIVE pointing to an official SFTPGo binary; offline initialization only"]
    fn native_sftpgo_accounts_survive_data_directory_migration() {
        let program = PathBuf::from(std::env::var_os("NSB_SFTPGO_NATIVE").expect("NSB_SFTPGO_NATIVE"));
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source data");
        let target = temp.path().canonicalize().unwrap().join("新 data # [1]");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        drop(crate::store::Store::open(paths.db()).unwrap());
        let directory = source.join("etc/sftpgo/2.7.6");
        std::fs::create_dir_all(&directory).unwrap();
        let home = source.join("data/sftpgo/users/migration-fixture");
        let shared = source.join("data/sftpgo/shared");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        std::fs::write(home.join("kept.txt"), "user contents").unwrap();
        std::fs::write(shared.join("shared.txt"), "shared contents").unwrap();
        std::fs::write(directory.join("sftpgo.json"), serde_json::json!({
            "data_provider": {"driver":"sqlite","name":"accounts.db","sql_tables_prefix":"fixture_"}
        }).to_string()).unwrap();
        let seed = temp.path().join("accounts.json");
        std::fs::write(&seed, serde_json::json!({"version":16,
            "folders":[{"name":"shared","mapped_path":portable_path_text(&shared)}],
            "groups":[{"name":"fixture-group","user_settings":{"home_dir":portable_path_text(&source.join("data/sftpgo/group/%username%"))}}],
            "users":[{"username":"migration-fixture","password":"Fixture-Only-Password-123!","status":1,
                "home_dir":portable_path_text(&home),"permissions":{"/":["*"]},
                "virtual_folders":[{"name":"shared","mapped_path":portable_path_text(&shared),"virtual_path":"/shared","quota_size":-1,"quota_files":-1}]}]
        }).to_string()).unwrap();
        let initialize = |directory: &Path, seed: Option<&Path>| {
            let mut command = platform::command(&program);
            command
                .current_dir(temp.path())
                .args(["initprovider", "--config-dir"])
                .arg(portable_path_text(directory));
            if let Some(seed) = seed {
                command.arg("--loaddata-from").arg(seed);
            }
            for (key, _) in std::env::vars_os() {
                if key.to_string_lossy().to_ascii_uppercase().starts_with("SFTPGO_") { command.env_remove(key); }
            }
            let output = command.output().unwrap();
            assert!(output.status.success(), "{}{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
        };
        initialize(&directory, Some(&seed));
        // 使用官方建库结果核对每张表的所有值，避免只验证路径而遗漏账号权限/凭据。
        fn snapshot(path: &Path) -> std::collections::BTreeMap<String, (Vec<String>, Vec<Vec<rusqlite::types::Value>>)> {
            let conn = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
            let tables: Vec<String> = conn.prepare("SELECT name FROM sqlite_master WHERE type='table' ORDER BY name").unwrap()
                .query_map([], |r|r.get(0)).unwrap().collect::<std::result::Result<_,_>>().unwrap();
            tables.into_iter().map(|name| {
                let mut statement = conn.prepare(&format!("SELECT * FROM \"{}\" ORDER BY rowid",name.replace('"',"\"\""))).unwrap();
                let columns = statement.column_names().iter().map(|s| s.to_string()).collect::<Vec<_>>();
                let rows = statement.query_map([], |row| (0..columns.len()).map(|i|row.get(i)).collect::<rusqlite::Result<Vec<_>>>()).unwrap()
                    .collect::<std::result::Result<Vec<_>,_>>().unwrap();
                (name, (columns, rows))
            }).collect()
        }
        let database = directory.join("accounts.db");
        let mut expected = snapshot(&database);
        let rebase = DataPathRebase::new(&source, &target).unwrap();
        for (name, field) in [("fixture_users","home_dir"),("fixture_folders","path"),("fixture_groups","user_settings")] {
            let (columns, rows) = expected.get_mut(name).expect("official provider table");
            assert!(!rows.is_empty(), "native import must create {name}");
            let index = columns.iter().position(|c|c==field).unwrap();
            for row in rows {
                use rusqlite::types::Value;
                let mut value = match &row[index] {
                    Value::Text(text) => text.clone(),
                    Value::Blob(bytes) => String::from_utf8(bytes.clone()).unwrap(),
                    _ => panic!("physical path must be text or UTF-8 bytes in {name}"),
                };
                if field == "user_settings" {
                    let mut settings: serde_json::Value = serde_json::from_str(&value).unwrap();
                    settings["home_dir"] = rebase.path(settings["home_dir"].as_str().unwrap()).into();
                    value = settings.to_string();
                } else { value = rebase.path(&value); }
                row[index] = if matches!(row[index], Value::Blob(_)) { Value::Blob(value.into_bytes()) } else { Value::Text(value) };
            }
        }
        let before = std::fs::read(&database).unwrap();
        copy_data_dir(&source, &target).unwrap();
        assert_eq!(std::fs::read(&database).unwrap(), before);
        let migrated = target.join("etc/sftpgo/2.7.6/accounts.db");
        assert_eq!(snapshot(&migrated), expected);
        // 原路径消失后让官方程序再次打开目标库，防止目标依然依赖源目录。
        std::fs::rename(&source, temp.path().join("retained source")).unwrap();
        initialize(&target.join("etc/sftpgo/2.7.6"), None);
        assert_eq!(snapshot(&migrated), expected);
        assert_eq!(std::fs::read_to_string(target.join("data/sftpgo/users/migration-fixture/kept.txt")).unwrap(), "user contents");
        assert_eq!(std::fs::read_to_string(target.join("data/sftpgo/shared/shared.txt")).unwrap(), "shared contents");
    }

    #[test]
    fn sftpgo_bolt_reader_rejects_truncation_invalid_extents_and_page_cycles() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("fixture.db");
        for page_size in [4096, 16384] {
            let root = page_size * 2;
            let mut bytes = vec![0u8; page_size * 4];
            let put = |bytes: &mut [u8], start: usize, value: u64, size: usize| {
                bytes[start..start + size].copy_from_slice(&value.to_le_bytes()[..size]);
            };
            for id in 0..2 {
                let page = &mut bytes[id * page_size..(id + 1) * page_size];
                put(page, 0, id as u64, 8);
                put(page, 8, 4, 2);
                put(page, 16, 0xed0cdaed, 4);
                put(page, 20, 2, 4);
                put(page, 24, page_size as u64, 4);
                put(page, 32, 2, 8);
                put(page, 48, 3, 8);
                put(page, 56, 4, 8);
                put(page, 64, id as u64, 8);
                let checksum = page[16..72]
                    .iter()
                    .fold(0xcbf29ce484222325_u64, |hash, byte| {
                        (hash ^ *byte as u64).wrapping_mul(0x100000001b3)
                    });
                put(page, 72, checksum, 8);
            }
            put(&mut bytes, root, 2, 8);
            put(&mut bytes, root + 8, 2, 2);
            std::fs::write(&path, &bytes).unwrap();
            assert!(crate::sftpgo_bolt::read(&path).unwrap().buckets.is_empty());
            for length in [0, 16, 79, page_size, root, page_size * 3 - 1] {
                std::fs::write(&path, &bytes[..length]).unwrap();
                assert!(
                    crate::sftpgo_bolt::read(&path).is_err(),
                    "truncated at {length}"
                );
            }
            for case in [
                "checksum", "page-id", "overflow", "flags", "cycle", "offset",
            ] {
                let mut damaged = bytes.clone();
                match case {
                    "checksum" => {
                        damaged[72] ^= 1;
                        damaged[page_size + 72] ^= 1;
                    }
                    "page-id" => put(&mut damaged, root, 3, 8),
                    "overflow" => put(&mut damaged, root + 12, u32::MAX as u64, 4),
                    "flags" => put(&mut damaged, root + 8, 0, 2),
                    "cycle" => {
                        put(&mut damaged, root + 8, 1, 2);
                        put(&mut damaged, root + 10, 1, 2);
                        put(&mut damaged, root + 16, 16, 4);
                        put(&mut damaged, root + 20, 1, 4);
                        put(&mut damaged, root + 24, 2, 8);
                        damaged[root + 32] = b'x';
                    }
                    _ => {
                        put(&mut damaged, root + 10, 1, 2);
                        put(&mut damaged, root + 20, u32::MAX as u64, 4);
                        put(&mut damaged, root + 24, 1, 4);
                    }
                }
                std::fs::write(&path, damaged).unwrap();
                assert!(crate::sftpgo_bolt::read(&path).is_err(), "{case}");
            }
        }
    }

    #[test]
    #[ignore = "requires NSB_SFTPGO_NATIVE; reads an official Bolt fixture and verifies offline imports"]
    fn native_sftpgo_bolt_accounts_keep_state_during_offline_import() {
        let program =
            PathBuf::from(std::env::var_os("NSB_SFTPGO_NATIVE").expect("NSB_SFTPGO_NATIVE"));
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source data");
        let target = temp.path().canonicalize().unwrap().join("new 中文 data");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        let store = crate::store::Store::open(paths.db()).unwrap();
        let manifest: crate::model::Manifest =
            serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let mut entry = manifest
            .packages
            .iter()
            .find(|entry| entry.id == "sftpgo" && entry.version == "2.7.6")
            .unwrap()
            .clone();
        entry.entry = format!("sftpgo{}", std::env::consts::EXE_SUFFIX);
        let runtime = paths.runtime_dir("sftpgo", &entry.version);
        std::fs::create_dir_all(&runtime).unwrap();
        std::fs::copy(&program, runtime.join(&entry.entry)).unwrap();
        std::fs::write(
            runtime.join(".niceenv-package.json"),
            serde_json::to_vec(&entry).unwrap(),
        )
        .unwrap();
        store
            .upsert_installed(&crate::model::InstalledPackage {
                id: "sftpgo".into(),
                version: entry.version.clone(),
                category: entry.category.clone(),
                install_path: portable_path_text(&runtime),
                config_path: String::new(),
                installed_at: 0,
            })
            .unwrap();
        drop(store);
        let directory = paths.etc_dir("sftpgo", "2.7.6");
        std::fs::create_dir_all(&directory).unwrap();
        let config = directory.join("sftpgo.json");
        std::fs::write(
            &config,
            r#"{"data_provider":{"driver":"bolt","name":"accounts.db"}}"#,
        )
        .unwrap();
        let home = source.join("users");
        let shared = source.join("shared");
        let password = bcrypt::hash("Fixture-Only-Password!", 4).unwrap();
        let mut users = (0..48).map(|i| serde_json::json!({
            "username":format!("fixture-{i:03}"),"password":password,"status":1,
            "home_dir":portable_path_text(&home.join(format!("user-{i}"))),
            "permissions":{"/":["*"]},"description":if i == 0 {"x".repeat(12000)} else {String::new()},
            "groups":[{"name":"fixture-group","type":2}],
            "virtual_folders":[{"name":"shared","mapped_path":portable_path_text(&shared),"virtual_path":"/shared","quota_size":-1,"quota_files":-1}]
        })).collect::<Vec<_>>();
        users[47]["filesystem"] = serde_json::json!({"provider":5,"sftpconfig":{
            "endpoint":"127.0.0.1:9","username":"fixture","prefix":"/remote-only",
            "password":{"status":"Plain","payload":"fixture-only-encryption"}}});
        let seed = temp.path().join("accounts.json");
        std::fs::write(&seed,serde_json::json!({"version":16,"users":users,
            "folders":[{"name":"shared","mapped_path":portable_path_text(&shared)}],
            "groups":[{"name":"fixture-group","user_settings":{"home_dir":portable_path_text(&home.join("%username%"))}}]
        }).to_string()).unwrap();
        let initialize = |directory: &Path, seed: &Path| {
            let mut command = platform::command(&program);
            command
                .current_dir(temp.path())
                .args(["initprovider", "--config-dir"])
                .arg(portable_path_text(directory))
                .arg("--loaddata-from")
                .arg(seed)
                .args(["--loaddata-mode", "0"]);
            for (key, _) in std::env::vars_os() {
                if key
                    .to_string_lossy()
                    .to_ascii_uppercase()
                    .starts_with("SFTPGO_")
                {
                    command.env_remove(key);
                }
            }
            let (success, output) = crate::cfgeditor::run_validator_with_timeout(
                &mut command,
                std::time::Duration::from_secs(30),
            )
            .unwrap();
            assert!(success, "{output}");
        };
        initialize(&directory, &seed);
        let database = directory.join("accounts.db");
        let before = crate::sftpgo_bolt::read(&database).unwrap();
        assert_eq!(before.buckets[b"users".as_slice()].values.len(), 48);
        assert_eq!(before.buckets[b"groups".as_slice()].values.len(), 1);
        let unchanged = std::fs::read(&database).unwrap();
        let remote: serde_json::Value = serde_json::from_slice(
            &before.buckets[b"users".as_slice()].values[b"fixture-047".as_slice()],
        )
        .unwrap();
        let encrypted_status = remote["filesystem"]["sftpconfig"]["password"]["status"]
            .as_str()
            .unwrap();
        assert!(!encrypted_status.is_empty() && encrypted_status != "Plain");
        let rebase = DataPathRebase::new(&source, &target).unwrap();
        let uploaded = home.join("user-0");
        std::fs::create_dir_all(&uploaded).unwrap();
        std::fs::write(
            uploaded.join("uploaded.ini"),
            format!("keep={}", portable_path_text(&source)),
        )
        .unwrap();
        copy_data_dir(&source, &target).unwrap();
        let migrated_db = target.join("etc/sftpgo/2.7.6/accounts.db");
        let after = crate::sftpgo_bolt::read(&migrated_db).unwrap();
        assert_eq!(std::fs::read(&database).unwrap(), unchanged);
        assert_eq!(
            std::fs::read(target.join("users/user-0/uploaded.ini")).unwrap(),
            std::fs::read(uploaded.join("uploaded.ini")).unwrap()
        );
        assert!(!crate::sftpgo_bolt::migrate(&migrated_db, None, &rebase).unwrap());
        assert_eq!(before.sequence, after.sequence);
        assert_eq!(
            before.buckets.keys().collect::<Vec<_>>(),
            after.buckets.keys().collect::<Vec<_>>()
        );
        for (name, bucket) in &before.buckets {
            let updated = &after.buckets[name];
            assert_eq!(bucket.sequence, updated.sequence);
            assert_eq!(
                bucket.values.keys().collect::<Vec<_>>(),
                updated.values.keys().collect::<Vec<_>>()
            );
            for (key, original) in &bucket.values {
                let mut expected: serde_json::Value = serde_json::from_slice(original).unwrap();
                let mut actual: serde_json::Value =
                    serde_json::from_slice(&updated.values[key]).unwrap();
                let name = std::str::from_utf8(name).unwrap();
                if ["users", "groups", "folders"].contains(&name) {
                    let field = match name {
                        "folders" => "/mapped_path",
                        "groups" => "/user_settings/home_dir",
                        _ => "/home_dir",
                    };
                    if let Some(path) = expected.pointer_mut(field) {
                        *path = rebase.path(path.as_str().unwrap()).into();
                    }
                    if let Some(values) = expected.as_object_mut() {
                        values.remove("updated_at");
                    }
                    if let Some(values) = actual.as_object_mut() {
                        values.remove("updated_at");
                    }
                }
                assert_eq!(
                    actual,
                    expected,
                    "bucket {name}, record {}",
                    String::from_utf8_lossy(key)
                );
            }
        }
        let mut broken = unchanged.clone();
        let page_size = u32::from_le_bytes(broken[24..28].try_into().unwrap()) as usize;
        broken[72] ^= 1;
        broken[page_size + 72] ^= 1;
        let damaged = temp.path().join("damaged.db");
        std::fs::write(&damaged, broken).unwrap();
        assert!(crate::sftpgo_bolt::read(&damaged).is_err());
        let failed_target = temp.path().join("must not switch");
        std::fs::copy(&damaged, &database).unwrap();
        let damaged_before = std::fs::read(&database).unwrap();
        assert!(copy_data_dir(&source, &failed_target).is_err());
        assert!(!failed_target.exists());
        assert_eq!(std::fs::read(&database).unwrap(), damaged_before);
        std::fs::write(&database, unchanged).unwrap();
        std::fs::write(runtime.join(&entry.entry), b"invalid executable").unwrap();
        let original = std::fs::read(&database).unwrap();
        let error = copy_data_dir(&source, &failed_target).unwrap_err();
        assert_eq!(error.code, "DATA_DIR_SFTPGO_IMPORT");
        assert_eq!(std::fs::read(&database).unwrap(), original);
        assert!(!failed_target.exists());
    }

    #[test]
    #[ignore = "requires NSB_STRUCTURED_NATIVE pointing to verified mihomo/sftpgo/qdrant binaries; isolated migration and short-lived loopback services"]
    fn native_structured_configs_keep_credentials_and_use_migrated_data() {
        let binaries = PathBuf::from(
            std::env::var_os("NSB_STRUCTURED_NATIVE").expect("NSB_STRUCTURED_NATIVE"),
        );
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source data");
        let target = temp.path().join("新 data # [1]");
        let paths = Paths::new(source.clone());
        paths.ensure_dirs().unwrap();
        let store = crate::store::Store::open(paths.db()).unwrap();
        let old = portable_path_text(&source);
        let secret = format!("{old}/literal-secret");
        let manifest: crate::model::Manifest =
            serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap();
        let mut entry = manifest
            .packages
            .iter()
            .find(|entry| entry.id == "sftpgo" && entry.version == "2.7.6")
            .unwrap()
            .clone();
        entry.entry = format!("sftpgo{}", std::env::consts::EXE_SUFFIX);
        let runtime = paths.runtime_dir("sftpgo", &entry.version);
        std::fs::create_dir_all(&runtime).unwrap();
        copy_tree(&binaries.join("sftpgo"), &runtime, &mut (0, 0), false).unwrap();
        std::fs::write(
            runtime.join(".niceenv-package.json"),
            serde_json::to_vec(&entry).unwrap(),
        )
        .unwrap();
        store
            .upsert_installed(&crate::model::InstalledPackage {
                id: entry.id.clone(),
                version: entry.version.clone(),
                category: entry.category.clone(),
                install_path: portable_path_text(&runtime),
                config_path: String::new(),
                installed_at: 0,
            })
            .unwrap();
        let qdrant_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let qdrant_port = qdrant_listener.local_addr().unwrap().port();
        let mihomo_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let mihomo_port = mihomo_listener.local_addr().unwrap().port();
        let ssh_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ssh_port = ssh_listener.local_addr().unwrap().port();
        let ftp_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let ftp_port = ftp_listener.local_addr().unwrap().port();
        let dav_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let dav_port = dav_listener.local_addr().unwrap().port();
        for directory in [
            "etc/qdrant/1",
            "etc/mihomo/providers",
            "etc/sftpgo/1",
            "etc/sftpgo/1/resources",
            "data/sftpgo",
        ] {
            std::fs::create_dir_all(source.join(directory)).unwrap();
        }
        let qdrant = format!("# preserve path-shaped API key\nservice:\n  host: 127.0.0.1\n  http_port: {qdrant_port}\n  grpc_port: null\n  api_key: '{secret}'\nstorage:\n  storage_path: '{old}/data/qdrant/storage'\n  snapshots_path: '{old}/data/qdrant/snapshots'\ntelemetry_disabled: true\n");
        let qdrant_config = source.join("etc/qdrant/1/config.yaml");
        std::fs::write(&qdrant_config, &qdrant).unwrap();
        write_with_backup(
            &qdrant_config,
            &format!("{qdrant}# changed\n"),
            &paths.backup(),
        )
        .unwrap();
        let mut backups = list_backup_files(&source).unwrap();
        assert_eq!(backups.len(), 1);
        let backup = backups.remove(0);
        std::fs::write(
            source.join("etc/mihomo/providers/local.yaml"),
            "proxies:\n  - {name: fixture, type: socks5, server: 127.0.0.1, port: 9}\n",
        )
        .unwrap();
        let mihomo = format!("mixed-port: 0\nexternal-controller: 127.0.0.1:{mihomo_port}\nsecret: '{secret}'\nallow-lan: false\nmode: rule\nproxy-providers:\n  local:\n    type: file\n    path: '{old}/etc/mihomo/providers/local.yaml'\nproxy-groups:\n  - {{name: fixture-group, type: select, use: [local]}}\nrules: ['MATCH,DIRECT']\n");
        std::fs::write(source.join("etc/mihomo/config.yaml"), &mihomo).unwrap();
        let certificate = rcgen::generate_simple_self_signed(vec!["127.0.0.1".into()]).unwrap();
        std::fs::write(source.join("data/sftpgo/tls.pem"), certificate.cert.pem()).unwrap();
        std::fs::write(
            source.join("data/sftpgo/tls.key"),
            certificate.key_pair.serialize_pem(),
        )
        .unwrap();
        std::fs::write(
            source.join("etc/sftpgo/1/resources/banner.txt"),
            format!("migrated native banner {old}/literal-text"),
        )
        .unwrap();
        std::fs::write(source.join("etc/sftpgo/1/resources/master.key"), &secret).unwrap();
        std::fs::create_dir(source.join("data/sftpgo/templates")).unwrap();
        copy_tree(
            &binaries.join("sftpgo/templates"),
            &source.join("data/sftpgo/templates"),
            &mut (0, 0),
            false,
        )
        .unwrap();
        let mut sftpgo = serde_json::json!({
            "data_provider":{"driver":"sqlite","name":format!("{old}/data/sftpgo/accounts.db"),"password":secret,"credentials_path":format!("{old}/data/sftpgo/credentials")},
            "sftpd":{"bindings":[{"address":"127.0.0.1","port":ssh_port}]},
            "ftpd":{"bindings":[{"address":"127.0.0.1","port":ftp_port}],"banner_file":"resources/banner.txt"},
            "webdavd":{"bindings":[{"address":"127.0.0.1","port":dav_port,"enable_https":true,
                "certificate_file":format!("{old}/data/sftpgo/tls.pem"),"certificate_key_file":format!("{old}/data/sftpgo/tls.key")}]},
            "httpd":{"bindings":[{"port":0}]},
            "smtp":{"templates_path":format!("{old}/data/sftpgo/templates")},
            "http":{"ca_certificates":[format!("{old}/data/sftpgo/tls.pem")],
                "certificates":[{"cert":format!("{old}/data/sftpgo/tls.pem"),"key":format!("{old}/data/sftpgo/tls.key")}]},
            "kms":{"secrets":{}}
        });
        fn uppercase_keys(value: &mut serde_json::Value) {
            match value {
                serde_json::Value::Object(values) => {
                    *values = std::mem::take(values)
                        .into_iter()
                        .map(|(key, mut value)| {
                            uppercase_keys(&mut value);
                            let key = if key == "driver" {
                                "DRİVER".into()
                            } else {
                                key.to_uppercase()
                            };
                            (key, value)
                        })
                        .collect();
                }
                serde_json::Value::Array(values) => values.iter_mut().for_each(uppercase_keys),
                _ => {}
            }
        }
        uppercase_keys(&mut sftpgo);
        let env_dir = source.join("etc/sftpgo/1/env.d");
        std::fs::create_dir(&env_dir).unwrap();
        let env = format!("# native environment migration\r\nBASE='{old}'\r\nSFTPGO_DATA_PROVIDER__CONNECTION_STRING='{}'\r\nSFTPGO_KMS__SECRETS__MASTER_KEY_PATH=\"${{BASE}}/etc/sftpgo/1/resources/master.key\"\r\nSFTPGO_SFTPD__LOGIN_BANNER_FILE=\"${{BASE}}/etc/sftpgo/1/resources/banner.txt\"\r\nPASSWORD=${{SFTPGO_KMS__SECRETS__MASTER_KEY_PATH}}\r\n", crate::configpaths::sftpgo_sqlite_dsn(&source, "data/sftpgo/accounts.db"));
        let mut env_bytes = vec![0xff, 0xfe];
        for word in env.encode_utf16() {
            env_bytes.extend(word.to_le_bytes());
        }
        std::fs::write(env_dir.join("native.env"), &env_bytes).unwrap();
        std::fs::write(
            source.join("etc/sftpgo/1/sftpgo.json"),
            serde_json::to_vec_pretty(&sftpgo).unwrap(),
        )
        .unwrap();
        let executable = |id: &str, name: &str| {
            binaries
                .join(id)
                .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
        };
        let initialize = |base: &Path| {
            let mut command = platform::command(executable("sftpgo", "sftpgo"));
            for (key, _) in
                std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("SFTPGO_"))
            {
                command.env_remove(key);
            }
            command
                .current_dir(base)
                .args(["initprovider", "--config-dir"])
                .arg(base.join("etc/sftpgo/1"))
                .args(["--config-file", "sftpgo.json"]);
            let output = command.output().unwrap();
            assert!(
                output.status.success(),
                "{}{}",
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        };
        initialize(&source);
        let original_db = std::fs::read(source.join("data/sftpgo/accounts.db")).unwrap();
        assert!(original_db.len() > 4096);
        drop(store);
        copy_data_dir(&source, &target).unwrap();
        assert_eq!(
            std::fs::read(env_dir.join("native.env")).unwrap(),
            env_bytes
        );
        let migrated_bytes = std::fs::read(target.join("etc/sftpgo/1/env.d/native.env")).unwrap();
        assert_eq!(&migrated_bytes[..2], &[0xff, 0xfe]);
        let migrated_env = String::from_utf16(
            &migrated_bytes[2..]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>(),
        )
        .unwrap();
        assert!(migrated_env.contains(&format!(
            "PASSWORD='{old}/etc/sftpgo/1/resources/master.key'"
        )));
        assert_eq!(
            std::fs::read(target.join("etc/sftpgo/1/resources/master.key")).unwrap(),
            secret.as_bytes()
        );
        assert_eq!(
            std::fs::read(target.join("data/sftpgo/accounts.db")).unwrap(),
            original_db
        );
        std::fs::rename(&source, temp.path().join("unavailable-source")).unwrap();
        let preview = preview_backup(&target, &backup.name).unwrap();
        restore_backup_checked(&target, &backup.name, Some(&preview.revision)).unwrap();
        initialize(&target);
        let config: serde_json::Value = serde_json::from_slice(
            &std::fs::read(target.join("etc/sftpgo/1/sftpgo.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(config["DATA_PROVIDER"]["PASSWORD"], secret);
        struct Child(std::process::Child);
        impl Drop for Child {
            fn drop(&mut self) {
                let _ = self.0.kill();
                let _ = self.0.wait();
            }
        }
        let log = temp.path().join("sftpgo.log");
        let output = std::fs::File::create(&log).unwrap();
        let mut command = platform::command(executable("sftpgo", "sftpgo"));
        for (key, _) in
            std::env::vars_os().filter(|(key, _)| key.to_string_lossy().starts_with("SFTPGO_"))
        {
            command.env_remove(key);
        }
        command
            .current_dir(&target)
            .args(["serve", "--config-dir"])
            .arg(target.join("etc/sftpgo/1"))
            .args(["--config-file", "sftpgo.json", "--log-file-path", ""])
            .stdout(output.try_clone().unwrap())
            .stderr(output);
        drop(ssh_listener);
        drop(ftp_listener);
        drop(dav_listener);
        let mut child = Child(command.spawn().unwrap());
        let tls = reqwest::blocking::Client::builder()
            .no_proxy()
            .add_root_certificate(
                reqwest::Certificate::from_pem(certificate.cert.pem().as_bytes()).unwrap(),
            )
            .timeout(std::time::Duration::from_secs(2))
            .build()
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
        loop {
            assert!(
                child.0.try_wait().unwrap().is_none(),
                "{}",
                std::fs::read_to_string(&log).unwrap()
            );
            if let Ok(response) = tls.get(format!("https://127.0.0.1:{dav_port}/")).send() {
                assert_eq!(response.status(), reqwest::StatusCode::UNAUTHORIZED);
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "{}",
                std::fs::read_to_string(&log).unwrap()
            );
            std::thread::sleep(std::time::Duration::from_millis(100));
        }
        let ftp = std::net::TcpStream::connect(("127.0.0.1", ftp_port)).unwrap();
        ftp.set_read_timeout(Some(std::time::Duration::from_secs(5)))
            .unwrap();
        let mut ftp = std::io::BufReader::new(ftp);
        let mut greeting = String::new();
        for _ in 0..10 {
            use std::io::BufRead;
            let mut line = String::new();
            assert!(ftp.read_line(&mut line).unwrap() > 0);
            greeting.push_str(&line);
            if line.starts_with("220 ") {
                break;
            }
        }
        assert!(
            greeting.contains(&format!("migrated native banner {old}/literal-text")),
            "{greeting}"
        );
        let banner = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        struct Banner(std::sync::Arc<std::sync::Mutex<String>>, String);
        impl russh::client::Handler for Banner {
            type Error = russh::Error;
            async fn check_server_key(
                &mut self,
                key: &russh::keys::PublicKeyOrCertificate,
            ) -> std::result::Result<bool, Self::Error> {
                Ok(key
                    .public_key()
                    .fingerprint(russh::keys::HashAlg::Sha256)
                    .to_string()
                    == self.1)
            }
            async fn auth_banner(
                &mut self,
                value: &str,
                _: &mut russh::client::Session,
            ) -> std::result::Result<(), Self::Error> {
                *self.0.lock().unwrap() = value.into();
                Ok(())
            }
        }
        let fingerprint = crate::certdeploy::probe_ssh("127.0.0.1", ssh_port)
            .unwrap()
            .fingerprint;
        tokio::runtime::Runtime::new().unwrap().block_on(async {
            tokio::time::timeout(std::time::Duration::from_secs(10), async {
                let mut connection = russh::client::connect(
                    std::sync::Arc::new(russh::client::Config::default()),
                    ("127.0.0.1", ssh_port),
                    Banner(banner.clone(), fingerprint),
                )
                .await
                .unwrap();
                assert!(!connection
                    .authenticate_none("migration-probe")
                    .await
                    .unwrap()
                    .success());
                assert_eq!(
                    *banner.lock().unwrap(),
                    format!("migrated native banner {old}/literal-text")
                );
                connection
                    .disconnect(russh::Disconnect::ByApplication, "done", "en")
                    .await
                    .unwrap();
            })
            .await
            .unwrap();
        });
        drop(ftp);
        drop(child);
        let http = reqwest::blocking::Client::builder()
            .no_proxy()
            .timeout(std::time::Duration::from_millis(500))
            .build()
            .unwrap();
        drop(qdrant_listener);
        drop(mihomo_listener);
        for (id, binary, port, endpoint) in [
            ("qdrant", "qdrant", qdrant_port, "/collections"),
            (
                "mihomo",
                if cfg!(windows) {
                    "mihomo-windows-amd64"
                } else {
                    "mihomo"
                },
                mihomo_port,
                "/providers/proxies/local",
            ),
        ] {
            let log = temp.path().join(format!("{id}.log"));
            let output = std::fs::File::create(&log).unwrap();
            let mut command = platform::command(executable(id, binary));
            command
                .current_dir(&target)
                .stdout(output.try_clone().unwrap())
                .stderr(output);
            if id == "qdrant" {
                for (key, _) in std::env::vars_os()
                    .filter(|(key, _)| key.to_string_lossy().starts_with("QDRANT_"))
                {
                    command.env_remove(key);
                }
                command
                    .arg("--config-path")
                    .arg(target.join("etc/qdrant/1/config.yaml"))
                    .arg("--disable-telemetry");
            } else {
                command
                    .arg("-d")
                    .arg(target.join("etc/mihomo"))
                    .arg("-f")
                    .arg(target.join("etc/mihomo/config.yaml"));
            }
            let mut child = Child(command.spawn().unwrap());
            let url = format!("http://127.0.0.1:{port}{endpoint}");
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
            loop {
                assert!(
                    child.0.try_wait().unwrap().is_none(),
                    "{}",
                    std::fs::read_to_string(&log).unwrap()
                );
                let request = if id == "qdrant" {
                    http.get(&url).header("api-key", &secret)
                } else {
                    http.get(&url).bearer_auth(&secret)
                };
                if let Ok(response) = request.send() {
                    if response.status().is_success() {
                        let value: serde_json::Value = response.json().unwrap();
                        if id == "mihomo" {
                            assert!(
                                value["proxies"]
                                    .as_array()
                                    .unwrap()
                                    .iter()
                                    .any(|proxy| proxy["name"] == "fixture"),
                                "{value}"
                            );
                        }
                        break;
                    }
                }
                assert!(
                    std::time::Instant::now() < deadline,
                    "{}",
                    std::fs::read_to_string(&log).unwrap()
                );
                std::thread::sleep(std::time::Duration::from_millis(100));
            }
            assert!(!http.get(&url).send().unwrap().status().is_success());
            if id == "qdrant" {
                let collection = format!("http://127.0.0.1:{port}/collections/migration_probe");
                http.put(&collection)
                    .header("api-key", &secret)
                    .json(&serde_json::json!({"vectors":{"size":1,"distance":"Dot"}}))
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .unwrap()
                    .error_for_status()
                    .unwrap();
                let snapshot: serde_json::Value = http
                    .post(format!("{collection}/snapshots"))
                    .header("api-key", &secret)
                    .timeout(std::time::Duration::from_secs(5))
                    .send()
                    .unwrap()
                    .error_for_status()
                    .unwrap()
                    .json()
                    .unwrap();
                assert!(target
                    .join("data/qdrant/snapshots/migration_probe")
                    .join(snapshot["result"]["name"].as_str().unwrap())
                    .is_file());
            }
            drop(child);
        }
        assert!(target.join("data/qdrant/storage").is_dir());
        assert!(target.join("data/qdrant/snapshots").is_dir());
        assert!(!source.exists());
        println!("Qdrant/mihomo authenticated HTTP; SFTPGo provider reopen, SSH/FTP banners and verified WebDAV TLS passed with the original directory unavailable");
    }

    #[test]
    fn data_dir_activity_blocks_overlap_and_releases_after_cancel() {
        let temp = tempfile::tempdir().unwrap();
        let first = DataDirActivity::shared(temp.path()).unwrap();
        let second = DataDirActivity::shared(temp.path()).unwrap();
        assert!(matches!(DataDirActivity::exclusive(temp.path()), Err(error) if error.code == "DATA_DIR_BUSY"));
        drop(first); drop(second);
        let exclusive = DataDirActivity::exclusive(temp.path()).unwrap();
        let root = temp.path().to_path_buf();
        std::thread::spawn(move || {
            assert!(matches!(DataDirActivity::shared(&root), Err(error) if error.code == "DATA_DIR_BUSY"));
            assert!(matches!(DataDirActivity::exclusive(&root), Err(error) if error.code == "DATA_DIR_BUSY"));
        }).join().unwrap();
        drop(exclusive);
        assert!(DataDirActivity::shared(temp.path()).is_ok());
    }

    #[test]
    fn data_dir_activation_retries_failure_and_runs_once() {
        let (temp, paths) = fixture();
        let marker = paths.base.join(".data-dir-activation.json");
        std::fs::write(&marker,serde_json::to_vec(&paths.base).unwrap()).unwrap();
        assert_eq!(finish_data_dir_activation(&paths,||Err(crate::error::AppError::new("PATH_FAILED","fixture"))).unwrap_err().code,"PATH_FAILED");
        assert!(marker.exists());
        let mut count = 0;
        finish_data_dir_activation(&paths,|| { count += 1; Ok(()) }).unwrap();
        finish_data_dir_activation(&paths,|| { count += 1; Ok(()) }).unwrap();
        assert_eq!(count,1);
        assert!(!marker.exists());
        std::fs::write(&marker,serde_json::to_vec(&temp.path()).unwrap()).unwrap();
        assert_eq!(finish_data_dir_activation(&paths,||panic!("must not sync moved copy")).unwrap_err().code,"DATA_DIR_MOVED");
    }

    #[test]
    fn data_dir_backup_restore_rebases_history_without_altering_original_backup() {
        let temp = tempfile::tempdir().unwrap();
        let source = Paths::new(temp.path().join("source")); source.ensure_dirs().unwrap();
        let _store = crate::store::Store::open(source.db()).unwrap();
        let old = portable_path_text(&source.base);
        let original = format!("root \"{old}/www\";\n");
        std::fs::write(source.nginx_conf(),&original).unwrap();
        write_with_backup(&source.nginx_conf(),"changed",&source.backup()).unwrap();
        let backup = list_backup_files(&source.base).unwrap().remove(0);
        let target = temp.path().join("destination"); copy_data_dir(&source.base,&target).unwrap();
        let preview = preview_backup(&target,&backup.name).unwrap();
        restore_backup_checked(&target,&backup.name,Some(&preview.revision)).unwrap();
        assert_eq!(std::fs::read_to_string(target.join("etc/nginx/nginx.conf")).unwrap(),format!("root \"{}/www\";\n",portable_path_text(&target)));
        assert_eq!(std::fs::read_to_string(&backup.path).unwrap(),original);
        let second = temp.path().join("second"); copy_data_dir(&target,&second).unwrap();
        let preview = preview_backup(&second,&backup.name).unwrap();
        restore_backup_checked(&second,&backup.name,Some(&preview.revision)).unwrap();
        assert_eq!(std::fs::read_to_string(second.join("etc/nginx/nginx.conf")).unwrap(),format!("root \"{}/www\";\n",portable_path_text(&second)));
        std::fs::write(second.join(".data-dir-history.json"),"[]").unwrap();
        assert!(restore_backup_checked(&second,&backup.name,Some(&preview.revision)).is_err());
    }

    #[test]
    fn exact_targets_survive_missing_files_and_rapid_writes_keep_every_backup() {
        let (_temp, paths) = fixture();
        let target = paths.php_ini("8.2.0");
        write_with_backup(&target, "original", &paths.backup()).unwrap();
        write_with_backup(&paths.php_ini("8.4.0"), "other version", &paths.backup()).unwrap();
        for value in 0..8 {
            write_with_backup(&target, &value.to_string(), &paths.backup()).unwrap();
        }
        write_with_backup(&target, "7", &paths.backup()).unwrap();
        let history = list_backup_files(&paths.base).unwrap();
        assert_eq!(history.len(), 8);
        let names: std::collections::HashSet<_> = history.iter().map(|b| &b.name).collect();
        assert_eq!(names.len(), 8);
        assert!(history
            .iter()
            .all(|b| b.target_path.as_deref() == Some("etc/php/8.2.0/php.ini") && b.restorable));
        let original = history
            .iter()
            .find(|b| std::fs::read_to_string(&b.path).unwrap() == "original")
            .unwrap();
        std::fs::remove_file(&target).unwrap();
        let preview = preview_backup(&paths.base, &original.name).unwrap();
        assert!(!preview.current_exists);
        assert_eq!(
            restore_backup_checked(&paths.base, &original.name, Some(&preview.revision)).unwrap(),
            target
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "original");
        assert_eq!(
            std::fs::read_to_string(paths.php_ini("8.4.0")).unwrap(),
            "other version"
        );
    }

    #[test]
    fn restore_conflicts_tampering_and_backup_failures_preserve_current_content() {
        let (_temp, paths) = fixture();
        let target = paths.nginx_conf();
        write_with_backup(&target, "v1", &paths.backup()).unwrap();
        write_with_backup(&target, "v2", &paths.backup()).unwrap();
        let backup = list_backup_files(&paths.base).unwrap().remove(0);
        let preview = preview_backup(&paths.base, &backup.name).unwrap();
        std::fs::write(&target, "external").unwrap();
        assert!(
            restore_backup_checked(&paths.base, &backup.name, Some(&preview.revision)).is_err()
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external");
        std::fs::write(&backup.path, "tampered").unwrap();
        assert!(preview_backup(&paths.base, &backup.name).is_err());
        assert!(restore_backup(&paths.base, &backup.name).is_err());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "external");

        let (_temp2, other) = fixture();
        write_with_backup(&other.nginx_conf(), "keep", &other.backup()).unwrap();
        std::fs::write(other.backup().join("files"), "blocked directory").unwrap();
        assert!(write_with_backup(&other.nginx_conf(), "lost", &other.backup()).is_err());
        assert_eq!(std::fs::read_to_string(other.nginx_conf()).unwrap(), "keep");
        assert!(list_backup_files(&other.base).is_err());
    }

    #[test]
    fn malformed_bundles_are_disabled_without_hiding_valid_history() {
        let (_temp, paths) = fixture();
        write_with_backup(&paths.nginx_conf(), "v1", &paths.backup()).unwrap();
        write_with_backup(&paths.nginx_conf(), "v2", &paths.backup()).unwrap();
        std::fs::create_dir_all(paths.backup().join("files/cfg-incomplete")).unwrap();
        std::fs::create_dir_all(paths.backup().join("files/.pending-unpublished")).unwrap();
        let history = list_backup_files(&paths.base).unwrap();
        assert_eq!(history.len(), 2);
        assert!(history
            .iter()
            .any(|b| b.name == "files/cfg-incomplete" && !b.restorable && b.reason.is_some()));
        assert_eq!(history.iter().filter(|b| b.restorable).count(), 1);
    }

    #[test]
    fn legacy_ambiguity_non_config_backups_and_unsafe_paths_cannot_be_restored() {
        let (_temp, paths) = fixture();
        write_with_backup(&paths.php_ini("8.4.0"), "keep", &paths.backup()).unwrap();
        std::fs::write(paths.backup().join("php.ini.20260101.bak"), "old").unwrap();
        write_with_backup(&paths.nginx_conf(), "nginx", &paths.backup()).unwrap();
        std::fs::write(paths.etc().join("nginx.conf"), "duplicate").unwrap();
        std::fs::write(paths.backup().join("nginx.conf.20260101.bak"), "old").unwrap();
        write_with_backup(&paths.certs().join("secret.key"), "key1", &paths.backup()).unwrap();
        write_with_backup(&paths.certs().join("secret.key"), "key2", &paths.backup()).unwrap();
        for backup in list_backup_files(&paths.base).unwrap() {
            assert!(!backup.restorable, "{}", backup.name);
            assert!(restore_backup(&paths.base, &backup.name).is_err());
        }
        for path in [
            "../escape",
            "etc/../escape",
            "etc//file",
            "etc/./file",
            "/etc/file",
            "C:/file",
            "etc/a:stream",
            "etc/a.",
            "etc/a ",
            "etc/NUL.ini",
            "etc/COM1/x",
            "etc/LPT²/x",
            "etc/a?b",
            "etc\\file",
        ] {
            assert!(checked_data_path(&paths.base, path).is_err(), "{path}");
        }
        for name in [
            "../secret.bak",
            "config/../../secret.bak",
            "files/cfg-a/../b",
            "C:/file.bak",
        ] {
            assert!(restore_backup(&paths.base, name).is_err(), "{name}");
        }
    }

    #[test]
    fn altered_metadata_cannot_write_outside_the_config_tree() {
        let (_temp, paths) = fixture();
        write_with_backup(&paths.nginx_conf(), "v1", &paths.backup()).unwrap();
        write_with_backup(&paths.nginx_conf(), "v2", &paths.backup()).unwrap();
        let backup = list_backup_files(&paths.base).unwrap().remove(0);
        let metadata = paths.backup().join(&backup.name).join("metadata.json");
        for target in [
            "../escape",
            "etc/../secret",
            "etc/file:stream",
            "certs/root.key",
        ] {
            std::fs::write(
                &metadata,
                serde_json::to_vec(&BackupMetadata {
                    version: 1,
                    target: target.into(),
                    sha256: hex::encode(Sha256::digest(b"v1")),
                })
                .unwrap(),
            )
            .unwrap();
            assert!(
                restore_backup(&paths.base, &backup.name).is_err(),
                "{target}"
            );
        }
        assert_eq!(std::fs::read_to_string(paths.nginx_conf()).unwrap(), "v2");
        assert!(!paths.base.parent().unwrap().join("escape").exists());
    }

    #[test]
    fn directory_links_cannot_redirect_restores_or_backup_writes() {
        let (temp, paths) = fixture();
        let outside = temp.path().join("outside");
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(outside.join("file"), "keep outside").unwrap();
        let link = paths.etc().join("linked");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            let result = std::process::Command::new("cmd.exe")
                .args(["/C", "mklink", "/J"])
                .arg(&link)
                .arg(&outside)
                .creation_flags(0x08000000)
                .output()
                .unwrap();
            assert!(
                result.status.success(),
                "{}",
                String::from_utf8_lossy(&result.stderr)
            );
        }
        #[cfg(unix)]
        std::os::unix::fs::symlink(&outside, &link).unwrap();
        assert!(checked_data_path(&paths.base, "etc/linked/file").is_err());
        assert!(write_with_backup(&link.join("file"), "lost", &paths.backup()).is_err());
        std::fs::write(paths.backup().join("file.20260101.bak"), "lost").unwrap();
        assert!(restore_backup(&paths.base, "file.20260101.bak").is_err());
        assert_eq!(
            std::fs::read_to_string(outside.join("file")).unwrap(),
            "keep outside"
        );
        #[cfg(windows)]
        std::fs::remove_dir(&link).unwrap();
        #[cfg(unix)]
        std::fs::remove_file(&link).unwrap();
    }
}
