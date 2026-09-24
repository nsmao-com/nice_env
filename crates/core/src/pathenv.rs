//! 运行时 bin 目录注入系统 PATH —— 让 `php`、`mysql`、`node` 这些命令在终端里直接用。
//!
//! 设计要点：
//!
//! 1. **只碰我们自己写进去的目录**。改动前先把上次写入的目录列表读出来
//!    （`pathEnvDirs` 设置项），合并时精确移除这些条目，其余 PATH 原样保留。
//!    绝不按「看起来像我们的路径」去猜着删——用户手写的路径必须毫发无伤。
//! 2. **多版本只放一个**。同一 id 装了多个版本时，只把「使用中版本」的 bin 放进去
//!    （复用 `ops::installed_by_choice`），否则 `php` 指向哪个版本全凭 PATH 顺序，不可控。
//! 3. **纯函数负责合并**（`merge_win_path` / 平台层的 `merge_profile_content`），
//!    写盘只在 platform 层发生，便于测试与审查。
//! 4. Windows 写 HKCU（用户级，无需管理员），写完广播变更让新终端立刻生效；
//!    macOS 写 `~/.zshrc` 托管块，与 hosts 同一套「标记块可整块回滚」的思路。

use crate::error::{AppError, Result};
use crate::model::{Manifest, PackageManifestEntry, PathEnvEntry, PathEnvStatus};
use crate::paths::Paths;
use crate::store::Store;

/// 总开关（"1" 为开）
const ENABLED_KEY: &str = "pathEnvEnabled";
/// 我们写入过（因而有责任清理）的目录列表，JSON 数组
const DIRS_KEY: &str = "pathEnvDirs";
/// 用户勾选要注入的包 id 列表，JSON 数组；缺省表示「全部」
const SELECTED_KEY: &str = "pathEnvSelected";

/* ================= bin 目录推导 ================= */

/// 入口程序不该注入 PATH 的扩展名：这些不是可直接执行的命令，
/// 混进 PATH 只会让 `composer` 之类指向一个无法执行的文件。
fn is_non_executable_entry(entry: &str) -> bool {
    let lower = entry.to_ascii_lowercase();
    matches!(
        lower.rsplit_once('.').map(|(_, ext)| ext),
        Some("phar" | "php" | "jar" | "txt" | "json" | "toml" | "yaml" | "yml" | "md" | "ini")
    )
}

/// 由「安装目录 + 清单 entry」推出应注入 PATH 的目录 = 入口程序所在目录。
/// 这是通用规则，无需为每个包写特例：`go/bin/go.exe` → `{root}/go/bin`，
/// `php-cgi.exe` → `{root}`，`nginx-1.26.3/nginx.exe` → `{root}/nginx-1.26.3`。
pub fn bin_dir_for(install_path: &str, entry: &str) -> Option<String> {
    if entry.trim().is_empty() || is_non_executable_entry(entry) {
        return None;
    }
    let rel = entry.replace('\\', "/");
    let parent = match rel.rsplit_once('/') {
        Some((p, _)) => p,
        None => "",
    };
    let base = install_path.trim_end_matches(['/', '\\']);
    let joined = if parent.is_empty() {
        base.to_string()
    } else {
        format!("{base}/{parent}")
    };
    Some(joined)
}

/// 扫描 bin 目录里可用的命令名（Windows 去掉 .exe/.bat/.cmd 扩展名）。
/// 以入口程序优先排序，最多返回 6 个，避免 `mysql/bin` 这种把 UI 塞爆。
fn commands_in_dir(bin_dir: &std::path::Path, prefer: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    let Ok(read) = std::fs::read_dir(bin_dir) else {
        return names;
    };
    for e in read.flatten() {
        let path = e.path();
        if !path.is_file() {
            continue;
        }
        let fname = e.file_name().to_string_lossy().to_string();
        let lower = fname.to_ascii_lowercase();
        let stem = if cfg!(windows) {
            match lower.rsplit_once('.') {
                Some((s, "exe" | "bat" | "cmd" | "ps1")) => s.to_string(),
                _ => continue,
            }
        } else {
            fname.clone()
        };
        if !names.contains(&stem) {
            names.push(stem);
        }
    }
    let prefer_stem = prefer
        .replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or(prefer)
        .to_string();
    let prefer_stem = if cfg!(windows) {
        prefer_stem
            .rsplit_once('.')
            .map(|(s, _)| s.to_string())
            .unwrap_or(prefer_stem)
    } else {
        prefer_stem
    };
    // 入口程序同名命令排最前，其余按字母序，保证展示稳定
    names.sort_by(|a, b| {
        let pa = a.eq_ignore_ascii_case(&prefer_stem);
        let pb = b.eq_ignore_ascii_case(&prefer_stem);
        pb.cmp(&pa).then_with(|| a.cmp(b))
    });
    names.truncate(6);
    names
}

/* ================= 选择集与状态 ================= */

fn selected_ids(store: &Store) -> Option<Vec<String>> {
    store.get_setting_or::<Option<Vec<String>>>(SELECTED_KEY)
}

fn set_selected_ids(store: &Store, ids: &[String]) -> Result<()> {
    store.set_setting_json(SELECTED_KEY, &ids.to_vec())
}

fn managed_dirs(store: &Store) -> Vec<String> {
    store.get_setting_or::<Vec<String>>(DIRS_KEY)
}

pub fn is_enabled(store: &Store) -> bool {
    store.get_setting(ENABLED_KEY).as_deref() == Some("1")
}

/// 某个包是否处于「用户想要注入」的状态（未设置过则默认想要）
fn wants(sel: &Option<Vec<String>>, id: &str) -> bool {
    match sel {
        Some(list) => list.iter().any(|s| s == id),
        None => true,
    }
}

/// 清单里某 id 的模板条目（用于拿 entry 字段）
fn entry_of(manifest: &Manifest, id: &str) -> Option<PackageManifestEntry> {
    manifest.packages.iter().find(|p| p.id == id).cloned()
}

/// 计算当前「应该」注入的目录集合（不受总开关影响，供 UI 预览与实际写入共用）。
///
/// 多版本包只取使用中版本：同一 id 装了两个版本时，如果不做选择，
/// `php` 最终指向哪个版本取决于 PATH 顺序，行为不可预测。
pub fn desired_dirs(store: &Store, manifest: &Manifest) -> Vec<String> {
    let installed = store.list_installed().unwrap_or_default();
    let sel = selected_ids(store);

    // 按 id 归并，只保留使用中版本
    let mut by_id: Vec<String> = Vec::new();
    for p in &installed {
        if !by_id.contains(&p.id) {
            by_id.push(p.id.clone());
        }
    }
    let mut dirs: Vec<(String, String)> = Vec::new();
    for id in by_id {
        if !wants(&sel, &id) {
            continue;
        }
        let Some(chosen) = crate::ops::installed_by_choice(store, &id) else {
            continue;
        };
        let Some(entry) = entry_of(manifest, &id) else {
            continue;
        };
        if let Some(dir) = bin_dir_for(&chosen.install_path, &entry.entry) {
            if std::path::Path::new(&dir).is_dir() {
                dirs.push((id.clone(), dir));
            }
        }
    }
    // 按 id 排序，保证多次调用结果稳定
    dirs.sort_by(|a, b| a.0.cmp(&b.0));
    dirs.into_iter().map(|(_, d)| d).collect()
}

/// 组装完整状态（不写盘），供前端展示
pub fn status(store: &Store, manifest: &Manifest) -> PathEnvStatus {
    let enabled = is_enabled(store);
    let sel = selected_ids(store);
    let applied = managed_dirs(store);
    let installed = store.list_installed().unwrap_or_default();

    let mut ids: Vec<String> = Vec::new();
    for p in &installed {
        if !ids.contains(&p.id) {
            ids.push(p.id.clone());
        }
    }
    ids.sort();

    let current_path = read_current_path_entries(store);
    let mut entries = Vec::new();
    for id in ids {
        let Some(chosen) = crate::ops::installed_by_choice(store, &id) else {
            continue;
        };
        let Some(meta) = entry_of(manifest, &id) else {
            continue;
        };
        let bin_dir = bin_dir_for(&chosen.install_path, &meta.entry);
        let exists = bin_dir
            .as_deref()
            .map(|d| std::path::Path::new(d).is_dir())
            .unwrap_or(false);
        let commands = bin_dir
            .as_deref()
            .map(|d| commands_in_dir(std::path::Path::new(d), &meta.entry))
            .unwrap_or_default();
        let in_path = bin_dir
            .as_deref()
            .map(|d| current_path.iter().any(|p| same_path(p, d)))
            .unwrap_or(false);
        let usable = bin_dir.is_some() && exists && !commands.is_empty();
        if !usable && bin_dir.is_some() {
            // 目录在但扫不到可执行文件（如纯数据包）：不进列表，避免噪音
            continue;
        }
        if bin_dir.is_none() {
            continue;
        }
        entries.push(PathEnvEntry {
            id: id.clone(),
            label: meta.display_name.clone(),
            version: chosen.version.clone(),
            bin_dir: bin_dir.unwrap_or_default(),
            exists,
            selected: wants(&sel, &id),
            in_path,
            commands,
        });
    }

    // 漂移检测：开关开着，但磁盘上的实际内容与「应该注入的」不符。
    // 判据用真实 PATH（而不是我们的记录）：记录只能说明上次写了什么，
    // 用户手动删掉条目、或另一个程序覆盖了 PATH，都只有看磁盘才发现。
    let desired = if enabled { desired_dirs(store, manifest) } else { Vec::new() };
    let drift = enabled
        && desired.iter().any(|d| !current_path.iter().any(|p| same_path(p, d)));

    PathEnvStatus {
        enabled,
        managed_dirs: applied,
        entries,
        note: platform_note(),
        drift,
    }
}

fn platform_note() -> String {
    if cfg!(windows) {
        "写入的是当前用户的环境变量（无需管理员）。已打开的终端不会自动更新，请新开一个终端窗口。".to_string()
    } else {
        "写入 ~/.zshrc 的托管块。请新开终端窗口，或执行 source ~/.zshrc 立即生效。".to_string()
    }
}

/* ================= PATH 读取与合并（Windows） ================= */

/// 当前系统 PATH 的条目列表（Windows: 用户 Path；macOS: 进程环境变量）
fn read_current_path_entries(store: &Store) -> Vec<String> {
    let _ = store;
    #[cfg(windows)]
    {
        platform::pathenv::read_user_path()
            .map(|p| split_win_path(&p.value))
            .unwrap_or_default()
    }
    #[cfg(not(windows))]
    {
        std::env::var("PATH")
            .map(|p| p.split(':').map(|s| s.trim().to_string()).filter(|s| !s.is_empty()).collect())
            .unwrap_or_default()
    }
}

/// 按 `;` 切分 Windows PATH，去掉空段与首尾空白
pub fn split_win_path(value: &str) -> Vec<String> {
    value
        .split(';')
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .collect()
}

/// `merge_win_path` / `strip_win_path` 内部用的路径比较：大小写不敏感，忽略结尾分隔符。
///
/// 大小写不敏感性是 **Windows PATH 格式本身**的属性（这些函数处理的都是注册表里的
/// PATH 文本），不是运行平台的属性 —— 因此不能按 `cfg!(windows)` 分支：那样在
/// Linux/macOS 上跑测试或做交叉验证时，`d:\RT\PHP` 与 `D:\rt\php` 会被当成两条
/// 不同路径，托管条目清不掉。这里始终按 Windows 语义比较。
fn same_path(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        s.trim()
            .trim_end_matches(['\\', '/'])
            .replace('/', "\\")
            .to_ascii_lowercase()
    };
    norm(a) == norm(b)
}

/// 纯函数：把托管目录合并进 Windows PATH 文本。
///
/// - 精确移除 `previously_managed` 里的条目（那是我们写的，有责任清理）
/// - 移除即将重新加入的 `new_dirs`（避免重复，随后统一放到最前）
/// - 其余条目**原样保留**（包括 `%SystemRoot%` 这类未展开的变量引用）
///
/// 前置而非追加：用户输入 `php` 时应该命中我们管理的版本，
/// 而不是系统里可能存在的另一个 PHP。
pub fn merge_win_path(existing: &str, previously_managed: &[String], new_dirs: &[String]) -> String {
    let mut kept: Vec<String> = Vec::new();
    for item in split_win_path(existing) {
        let owned = previously_managed.iter().any(|m| same_path(m, &item));
        let replaced = new_dirs.iter().any(|n| same_path(n, &item));
        if owned || replaced {
            continue;
        }
        if kept.iter().any(|k| same_path(k, &item)) {
            continue; // 顺手去重，避免 PATH 越用越长
        }
        kept.push(item);
    }
    let mut out: Vec<String> = Vec::new();
    for d in new_dirs {
        if !out.iter().any(|k| same_path(k, d)) {
            out.push(d.clone());
        }
    }
    out.extend(kept);
    out.join(";")
}

/// 从 Windows PATH 文本里移除我们托管过的条目（关闭开关 / 卸载时用）
pub fn strip_win_path(existing: &str, previously_managed: &[String]) -> String {
    merge_win_path(existing, previously_managed, &[])
}

/* ================= 应用（写盘） ================= */

/// 按当前状态把托管目录写入系统 PATH。
/// 幂等：内容不变就不写盘，也不广播（避免无意义地惊动系统）。
pub fn apply(store: &Store, paths: &Paths, manifest: &Manifest) -> Result<PathEnvStatus> {
    let _ = paths;
    let enabled = is_enabled(store);
    let desired = if enabled { desired_dirs(store, manifest) } else { Vec::new() };

    #[cfg(windows)]
    {
        // 上次写入的托管目录：先从 PATH 里摘掉再放新的
        let prev = managed_dirs(store);
        let raw = platform::pathenv::read_user_path().map_err(AppError::from)?;
        let merged = merge_win_path(&raw.value, &prev, &desired);
        if merged != raw.value {
            platform::pathenv::write_user_path(&merged, raw.reg_type).map_err(AppError::from)?;
        }
    }
    #[cfg(not(windows))]
    {
        // macOS：托管块写入 shell 启动文件；块本身就是记录，无需额外比对
        let dirs = desired.clone();
        let profiles = platform::pathenv::shell_profiles();
        if profiles.is_empty() {
            return Err(AppError::new("NO_SHELL_PROFILE", "找不到用户 HOME 目录，无法写入 shell 配置")
                .with_hint("请确认 HOME 环境变量已设置"));
        }
        for p in &profiles {
            platform::pathenv::write_profile_managed_block(p, &dirs).map_err(AppError::from)?;
        }
    }

    store.set_setting_json(DIRS_KEY, &desired)?;
    Ok(status(store, manifest))
}

/// 开/关总开关
pub fn set_enabled(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
    enabled: bool,
) -> Result<PathEnvStatus> {
    store.set_setting(ENABLED_KEY, if enabled { "1" } else { "0" })?;
    apply(store, paths, manifest)
}

/// 设置要注入的包集合（传 None 恢复「全部」）
pub fn set_selected(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
    ids: &[String],
) -> Result<PathEnvStatus> {
    set_selected_ids(store, ids)?;
    apply(store, paths, manifest)
}

/// 安装/卸载/切换版本后自动同步（开关没开就只更新一次状态，不写盘）。
/// 这样用户装完 PHP 不用再手动回来点一次。
pub fn sync(store: &Store, paths: &Paths, manifest: &Manifest) -> Result<()> {
    if !is_enabled(store) {
        return Ok(());
    }
    apply(store, paths, manifest).map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn bin_dir_is_entrys_parent() {
        assert_eq!(
            bin_dir_for("D:/rt/php/8.3.33", "php-cgi.exe").unwrap(),
            "D:/rt/php/8.3.33"
        );
        assert_eq!(
            bin_dir_for("D:/rt/go/1.24.1", "go/bin/go.exe").unwrap(),
            "D:/rt/go/1.24.1/go/bin"
        );
        assert_eq!(
            bin_dir_for("D:/rt/mysql/8.0.46", "mysql-8.0.46-winx64/bin/mysqld.exe").unwrap(),
            "D:/rt/mysql/8.0.46/mysql-8.0.46-winx64/bin"
        );
        // 结尾分隔符不该导致双斜杠
        assert_eq!(
            bin_dir_for("D:/rt/nginx/1.26.3/", "nginx-1.26.3/nginx.exe").unwrap(),
            "D:/rt/nginx/1.26.3/nginx-1.26.3"
        );
    }

    #[test]
    fn non_executable_entries_are_skipped() {
        assert!(bin_dir_for("D:/rt/composer/2.8.5", "composer.phar").is_none());
        assert!(bin_dir_for("D:/rt/adminer/4.8.1", "adminer-4.8.1.php").is_none());
        assert!(bin_dir_for("D:/rt/geoip", "GeoLite2-City.mmdb").is_some());
    }

    #[test]
    fn merge_prepends_and_preserves_foreign_entries() {
        let existing = "%SystemRoot%\\system32;C:\\Other\\bin";
        let out = merge_win_path(existing, &[], &v(&["D:\\rt\\php\\8.3.33"]));
        assert_eq!(out, "D:\\rt\\php\\8.3.33;%SystemRoot%\\system32;C:\\Other\\bin");
    }

    #[test]
    fn merge_removes_previously_managed_dirs() {
        let existing = "D:\\rt\\php\\8.3.33;C:\\Other\\bin";
        let out = merge_win_path(existing, &v(&["D:\\rt\\php\\8.3.33"]), &v(&["D:\\rt\\php\\8.4.25"]));
        assert_eq!(out, "D:\\rt\\php\\8.4.25;C:\\Other\\bin");
    }

    #[test]
    fn merge_is_idempotent() {
        let dirs = v(&["D:\\rt\\php\\8.3.33", "D:\\rt\\go\\1.24.1"]);
        let once = merge_win_path("%SystemRoot%\\system32", &[], &dirs);
        let twice = merge_win_path(&once, &dirs, &dirs);
        assert_eq!(once, twice, "重复应用不应产生重复条目");
        assert_eq!(twice.matches("D:\\rt\\php").count(), 1);
    }

    #[test]
    fn merge_dedupes_within_existing_path() {
        let existing = "C:\\a;C:\\a;C:\\b";
        let out = merge_win_path(existing, &[], &[]);
        assert_eq!(out, "C:\\a;C:\\b");
    }

    #[test]
    fn strip_removes_only_managed() {
        let existing = "D:\\rt\\php\\8.3.33;C:\\Other\\bin;D:\\rt\\go\\1.24.1";
        let out = strip_win_path(
            existing,
            &v(&["D:\\rt\\php\\8.3.33", "D:\\rt\\go\\1.24.1"]),
        );
        assert_eq!(out, "C:\\Other\\bin");
    }

    #[test]
    fn disabled_leaves_no_trace_in_utility() {
        // 关掉时 merge 传空 new_dirs：应等价于「清掉我们的，保留别人的」
        let existing = "D:\\rt\\php\\8.3.33;%SystemRoot%\\system32";
        let out = merge_win_path(existing, &v(&["D:\\rt\\php\\8.3.33"]), &[]);
        assert_eq!(out, "%SystemRoot%\\system32");
    }

    #[test]
    fn case_insensitive_matching_on_windows_semantics() {
        // 大小写不同视为同一条目（Windows 路径不区分大小写）
        let existing = "d:\\RT\\PHP\\8.3.33;C:\\keep";
        let out = merge_win_path(existing, &v(&["D:\\rt\\php\\8.3.33"]), &[]);
        assert_eq!(out, "C:\\keep");
    }

    #[test]
    fn trailing_slash_treated_as_same_path() {
        let out = merge_win_path("D:\\rt\\php\\8.3.33\\;C:\\keep", &[], &v(&["D:\\rt\\php\\8.3.33"]));
        assert_eq!(out, "D:\\rt\\php\\8.3.33;C:\\keep");
    }
}
