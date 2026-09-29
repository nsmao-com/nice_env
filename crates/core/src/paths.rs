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
    /// 4. 安装版：{exe 所在目录}/nsb-data
    /// 5. 开发环境（cargo target 下运行）：LocalAppData，避免 cargo clean 清掉数据
    /// 相对路径统一锚定到当前目录（子进程 cwd 各异，绝不能把相对路径写进配置/参数）
    pub fn resolve(base: Option<PathBuf>) -> crate::error::Result<PathBuf> {
        let absolutize = |p: PathBuf| {
            if p.is_absolute() {
                p
            } else {
                std::env::current_dir().unwrap_or_default().join(p)
            }
        };
        if let Some(p) = base {
            return Ok(absolutize(p));
        }
        if let Ok(env) = std::env::var("NSB_HOME") {
            if !env.trim().is_empty() {
                return Ok(absolutize(PathBuf::from(env)));
            }
        }
        if let Some(selected) = read_data_dir_selection(&data_dir_selection_file()?)? {
            return Ok(selected);
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
                            return Ok(candidate);
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
                    return Ok(legacy); // 迁移失败（如被占用）：沿用旧目录，保证还能读到数据
                }
            }
        }
        Ok(base)
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
    replacements: Vec<String>,
}

pub(crate) fn portable_path_text(path: &Path) -> String {
    let text = path.to_string_lossy().replace('\\', "/");
    if let Some(unc) = text.strip_prefix("//?/UNC/") { format!("//{unc}") }
    else { text.strip_prefix("//?/").unwrap_or(&text).to_string() }
}

impl DataPathRebase {
    pub(crate) fn new(source: &Path, target: &Path) -> crate::error::Result<Self> {
        let canonical = std::fs::canonicalize(source).ok().map(|path|portable_path_text(&path));
        let source = portable_path_text(source).trim_end_matches('/').to_string();
        let mut sources = vec![source.clone()];
        if let Some(canonical) = canonical { if !sources.contains(&canonical) { sources.push(canonical); } }
        sources.sort_by_key(|s|std::cmp::Reverse(s.len()));
        let target = portable_path_text(target).trim_end_matches('/').to_string();
        if source.is_empty() || source.ends_with(':') {
            return Err(crate::error::AppError::new("DATA_DIR_INVALID", "不能将磁盘根目录作为迁移源"));
        }
        // 这些字符会改变现有 shell/配置文件的引号语义，不能直接代入旧模板。
        if target.chars().any(|c| c.is_control() || "\"'`$%&|<>^;(){}".contains(c)) {
            return Err(crate::error::AppError::new("DATA_DIR_INVALID", "目标目录包含无法安全写入服务配置的字符，请选择其它目录"));
        }
        let mut forms = Vec::new();
        for source in &sources {
            forms.push((source.clone(), target.clone()));
            if cfg!(windows) {
                let extended = if let Some(unc) = source.strip_prefix("//") { format!("//?/UNC/{unc}") } else { format!("//?/{source}") };
                forms.push((extended.clone(), target.clone()));
                for form in [source, &extended] {
                    let old = form.replace('/', "\\");
                    let new = target.replace('/', "\\");
                    forms.push((old.replace('\\', "\\\\"), new.replace('\\', "\\\\")));
                    forms.push((old, new));
                }
            }
        }
        forms.sort_by(|a,b| b.0.len().cmp(&a.0.len()));
        forms.dedup_by(|a,b| a.0 == b.0);
        let pattern = forms.iter().map(|(old,_)| format!("({})", regex::escape(old))).collect::<Vec<_>>().join("|");
        let patterns = regex::RegexBuilder::new(&pattern).case_insensitive(cfg!(windows)).build()
            .map_err(|e| crate::error::AppError::internal("准备目录路径替换", e.to_string()))?;
        Ok(Self { sources, target, patterns, replacements: forms.into_iter().map(|(_,new)|new).collect() })
    }

    pub(crate) fn path(&self, value: &str) -> String {
        // 先消除 . / ..，避免把 source/../external 误当作目录内文件。
        let mut lexical = PathBuf::new();
        for component in Path::new(value).components() {
            match component {
                Component::CurDir => {},
                Component::ParentDir if lexical.file_name().is_some() => { lexical.pop(); },
                other => lexical.push(other.as_os_str()),
            }
        }
        let normalized = portable_path_text(&lexical);
        let source = self.sources.iter().find(|source| {
            normalized.get(..source.len()).is_some_and(|prefix| if cfg!(windows) { prefix.eq_ignore_ascii_case(source) } else { prefix == *source })
                && normalized.get(source.len()..).is_some_and(|suffix|suffix.is_empty() || suffix.starts_with('/'))
        });
        if let Some(source) = source {
            let suffix = &normalized[source.len()..];
            let rebased = format!("{}{suffix}", self.target);
            if value.contains('\\') { rebased.replace('/', "\\") } else { rebased }
        } else { value.to_string() }
    }

    pub(crate) fn text(&self, value: &str) -> crate::error::Result<String> {
        let mut out = String::new();
        let mut cursor = 0;
        for captures in self.patterns.captures_iter(value) {
            let Some(found) = captures.get(0) else { continue; };
            let before = value[..found.start()].chars().next_back();
            let after = value[found.end()..].chars().next();
            let boundary = |c: char| c.is_whitespace() || "\"'=;:,()[]{}".contains(c);
            if !before.is_none_or(boundary) || !after.is_none_or(|c| boundary(c) || c == '/' || c == '\\') { continue; }
            let suffix = value[found.end()..].split(|c: char| boundary(c)).next().unwrap_or("");
            if suffix.split(['/', '\\']).any(|part| part == "..") {
                return Err(crate::error::AppError::new("DATA_DIR_PATH_AMBIGUOUS", "配置中的旧路径包含上级目录引用，无法自动修正")
                    .with_hint("请先将相关路径改为完整绝对路径后重试"));
            }
            let Some(replacement) = captures.iter().skip(1).position(|item| item.is_some()).map(|i| &self.replacements[i]) else { continue; };
            if replacement.contains(' ') && !found.as_str().contains(' ') {
                let line = value[..found.start()].rsplit('\n').next().unwrap_or("");
                if line.matches('"').count() % 2 == 0 && line.matches('\'').count() % 2 == 0 {
                    return Err(crate::error::AppError::new("DATA_DIR_PATH_QUOTING", "配置中存在未加引号的旧路径，无法安全迁移到含空格的目录")
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
        let rewritten = rebase_config_files(&staging, &staging, &rebase)?;
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
        if root && matches!(entry.file_name().to_str(), Some("nsb.sqlite" | "nsb.sqlite-wal" | "nsb.sqlite-shm" | "nsb.sqlite-journal" | ".data-dir-activity.lock" | ".data-dir-activation.json" | ".data-dir-authority.json")) { continue; }
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

fn rebase_config_files(root: &Path, directory: &Path, rebase: &DataPathRebase) -> crate::error::Result<u64> {
    let mut rewritten = 0;
    for entry in std::fs::read_dir(directory)? {
        let entry = entry?;
        let path = entry.path();
        let relative = path.strip_prefix(root).map_err(|_| crate::error::AppError::new("DATA_DIR_INVALID", "配置文件超出迁移目录"))?;
        let first = relative.components().next().map(|c| c.as_os_str().to_string_lossy().into_owned()).unwrap_or_default();
        // 历史、下载及数据库业务内容保持原样，不做全盘字符串替换。
        if matches!(first.as_str(), "backup" | "logs" | "downloads" | "certs" | "cron-locks") { continue; }
        let metadata = entry.metadata()?;
        if metadata.is_dir() {
            rewritten += rebase_config_files(root, &path, rebase)?;
            continue;
        }
        let name = entry.file_name().to_string_lossy().to_lowercase();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("").to_lowercase();
        let config = first == "etc" || first == "user-modules" || name == ".user.ini"
            || ((first == "runtimes" || first == "data") && matches!(ext.as_str(), "conf" | "cnf" | "ini" | "cfg" | "properties" | "cmd" | "bat" | "ps1" | "sh"))
            || name == ".niceenv-package.json";
        if !config { continue; }
        if metadata.len() > 16 * 1024 * 1024 {
            return Err(crate::error::AppError::new("DATA_DIR_CONFIG_SIZE", format!("配置文件过大，无法自动检查：{}", relative.display())));
        }
        let bytes = std::fs::read(&path)?;
        let Ok(text) = std::str::from_utf8(&bytes) else {
            // 二进制凭据/缓存保持字节不变；旧路径出现在非 UTF-8 配置时不能假装完成。
            if rebase.patterns.is_match(&String::from_utf8_lossy(&bytes)) {
                return Err(crate::error::AppError::new("DATA_DIR_CONFIG_ENCODING", format!("配置不是 UTF-8，无法修正旧路径：{}", relative.display())));
            }
            continue;
        };
        let rebased = rebase.text(text).map_err(|e| e.with_detail(relative.display().to_string()))?;
        if rebased != text {
            std::fs::write(&path, rebased)?;
            rewritten += 1;
        }
    }
    Ok(rewritten)
}

fn read_path_history(base: &Path) -> io::Result<Vec<PathBuf>> {
    let Some(bytes) = read_optional(&base.join(".data-dir-history.json"))? else { return Ok(Vec::new()); };
    let history: Vec<PathBuf> = serde_json::from_slice(&bytes).map_err(io::Error::other)?;
    if history.len() > 100 || history.iter().any(|p| !p.is_absolute()) {
        return Err(backup_error("数据目录历史记录无效，无法安全转换备份路径"));
    }
    Ok(history)
}

pub(crate) fn rebase_backup_content(base: &Path, content: Vec<u8>) -> io::Result<Vec<u8>> {
    let mut history = read_path_history(base)?;
    // 长路径优先，避免多次迁移的相邻目录前缀互相覆盖。
    history.sort_by_key(|path|std::cmp::Reverse(path.as_os_str().len()));
    if history.is_empty() { return Ok(content); }
    let mut text = String::from_utf8(content).map_err(io::Error::other)?;
    for source in history {
        let rebase = DataPathRebase::new(&source,base).map_err(io::Error::other)?;
        text = rebase.text(&text).map_err(io::Error::other)?;
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
    let relative = nginx_path(relative);
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
    p.to_string_lossy().replace('\\', "/")
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
        match std::fs::symlink_metadata(&result) {
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
    pub path: String,
    pub size_bytes: u64,
    pub modified_at: i64,
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
                found.push(nginx_path(
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
    let content = rebase_backup_content(base, content)?;
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
        let target = temp.path().join("target");

        let result = copy_data_dir(&source, &target).unwrap();
        assert_eq!(result.files, 2);
        let copied_store = crate::store::Store::open(target.join("nsb.sqlite")).unwrap();
        assert_eq!(copied_store.get_setting("snapshot-fixture").as_deref(), Some("includes-wal"));
        assert_eq!(std::fs::read(target.join("etc/marker.ini")).unwrap(), b"preserve me");

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
            let error = guard.select_with_file(&file, &target, || {
                let rollback = crate::pathenv::MigrationActivationRollback::capture(&target)?;
                let mut command = platform::command(std::env::current_exe().unwrap());
                command.args(["--exact", "restart::tests::restart_child_probe", "--nocapture"])
                    .env("NSB_RESTART_PROBE_MODE", mode).env("NSB_RESTART_PROBE_ROOT", &target)
                    .stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                let result = crate::restart::launch_and_wait(&mut command, &target, std::time::Duration::from_secs(2));
                // 模拟新进程激活副本后出现后续初始化错误；仅写临时副本。
                store.set_setting_json("pathEnvDirs", &vec!["changed-path"])?;
                std::fs::remove_file(&marker_file)?;
                rollback.restore()?;
                result
            }).unwrap_err();
            assert_eq!(error.code, if mode=="fail" {"FIXTURE_INIT_FAILED"} else {"RESTART_TIMEOUT"});
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
        let old = if cfg!(windows) { "C:/old-data" } else { "/old-data" };
        let new = if cfg!(windows) { "D:/new-data" } else { "/new-data" };
        let rebase = DataPathRebase::new(Path::new(old),Path::new(new)).unwrap();
        assert_eq!(rebase.path(&format!("{old}/etc/a.ini")), format!("{new}/etc/a.ini"));
        assert_eq!(rebase.path(&format!("{old}-external/a")), format!("{old}-external/a"));
        let value = format!("include \"{old}/etc/*.conf\";\nroot {old}-external/www;\nurl https://example.test{old}/assets;");
        assert_eq!(rebase.text(&value).unwrap(), format!("include \"{new}/etc/*.conf\";\nroot {old}-external/www;\nurl https://example.test{old}/assets;"));
        if cfg!(windows) {
            assert_eq!(rebase.path("c:\\OLD-data\\runtime"), "D:\\new-data\\runtime");
            assert_eq!(rebase.text(r#"{"path":"C:\\old-data\\etc"}"#).unwrap(), r#"{"path":"D:\\new-data\\etc"}"#);
            assert_eq!(rebase.text(r#"root "\\?\C:\old-data\www";"#).unwrap(), r#"root "D:\new-data\www";"#);
            let unc = DataPathRebase::new(Path::new(r"\\?\UNC\server\share\old"),Path::new("D:/new-data")).unwrap();
            assert_eq!(unc.text(r#"root "\\?\UNC\server\share\old\www";"#).unwrap(),r#"root "D:\new-data\www";"#);
        }
        assert_eq!(rebase.path(&format!("{old}/../external")),format!("{old}/../external"));
        assert!(rebase.text(&format!("root \"{old}/../external\";")).is_err());
        let spaced = DataPathRebase::new(Path::new(old),Path::new(&format!("{new} with space"))).unwrap();
        assert!(spaced.text(&format!("command {old}/tool")).is_err());
        assert_eq!(spaced.text(&format!("command \"{old}/tool\"")).unwrap(), format!("command \"{new} with space/tool\""));
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
        store.set_setting("mysqlRootPassword", &format!("{old}/secret")).unwrap();
        store.set_setting("pathEnvDirs", &format!(r#"["{old}/runtimes/fixture/1"]"#)).unwrap();
        let conn = rusqlite::Connection::open(paths.db()).unwrap();
        conn.execute("INSERT INTO sites(id,name,domains,root_dir,runtime,https,rewrite,created_at,updated_at) VALUES('site','site','[]',?1,?2,0,'\"none\"',1,2)", rusqlite::params![format!("{old}/www"),serde_json::json!({"kind":"node","cwd":format!("{old}/www"),"command":format!("\"{old}/runtimes/fixture/1/{binary}\""),"custom":"preserve","application":{"version":"1","cwd":format!("{old}/app"),"args":[format!("{old}/app/server.js"),format!("{external}/file"),"literal & ; argument"]}}).to_string()]).unwrap();
        conn.execute("INSERT INTO certs(id,kind,subject,sans,not_before,not_after,cert_path,key_path) VALUES('cert','imported','test','[]',0,1,?1,?2)", rusqlite::params![format!("{old}/certs/site.pem"),format!("{external}/key.pem")]).unwrap();
        let target = temp.path().join("target");
        let result = copy_data_dir(&source, &target).unwrap();
        assert_eq!(result.rewritten_files,1);
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
        let paths = Paths::new(source.clone()); paths.ensure_dirs().unwrap();
        let _store = crate::store::Store::open(paths.db()).unwrap();
        let old = portable_path_text(&source);
        let content = format!("include {old}/etc/*.conf;");
        std::fs::write(paths.nginx_conf(), &content).unwrap();
        let target = temp.path().join("contains space");
        std::fs::create_dir(&target).unwrap();
        assert_eq!(copy_data_dir(&source, &target).unwrap_err().code, "DATA_DIR_PATH_QUOTING");
        assert!(std::fs::read_dir(&target).unwrap().next().is_none());
        assert_eq!(std::fs::read_to_string(paths.nginx_conf()).unwrap(), content);
        assert!(!std::fs::read_dir(temp.path()).unwrap().any(|entry|entry.unwrap().file_name().to_string_lossy().contains("-migrating-")));
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
