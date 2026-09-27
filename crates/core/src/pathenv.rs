//! 运行时 bin 目录注入系统 PATH —— 让 `php`、`mysql`、`node` 这些命令在终端里直接用。
//!
//! 设计要点：
//!
//! 1. **只碰我们自己写进去的目录**。改动前先把上次写入的目录列表读出来
//!    （`pathEnvDirs` 设置项），合并时精确移除这些条目，其余 PATH 原样保留。
//!    绝不按「看起来像我们的路径」去猜着删——用户手写的路径必须毫发无伤。
//! 2. **多版本只放一个**。PATH 版本独立保存，不改变默认版本、服务进程或站点绑定。
//!    旧设置尚未指定 PATH 版本时，沿用原有默认版本。
//! 3. **纯函数负责合并**（`merge_win_path` / 平台层的 `merge_profile_content`），
//!    写盘只在 platform 层发生，便于测试与审查。
//! 4. Windows 写 HKCU（用户级，无需管理员），写完广播变更让新终端立刻生效；
//!    macOS 写 `~/.zshrc` 托管块，与 hosts 同一套「标记块可整块回滚」的思路。

use crate::error::{AppError, Result};
use crate::model::{Manifest, PathEnvEntry, PathEnvStatus};
use crate::paths::Paths;
use crate::store::Store;

/// 总开关（"1" 为开）
const ENABLED_KEY: &str = "pathEnvEnabled";
/// 我们写入过（因而有责任清理）的目录列表，JSON 数组
const DIRS_KEY: &str = "pathEnvDirs";
/// 用户勾选要注入的包 id 列表，JSON 数组；缺省表示「全部」
const SELECTED_KEY: &str = "pathEnvSelected";
/// 每个套件单独选择的 PATH 版本；与 activeVersion 设置独立。
const VERSIONS_KEY: &str = "pathEnvVersions";

fn chosen_version(store: &Store, id: &str) -> Option<crate::model::InstalledPackage> {
    let installed = store.list_installed().ok()?;
    let versions = read_selection(store, VERSIONS_KEY).ok()?;
    chosen_from(store, &installed, &versions, id)
        .ok()
        .flatten()
        .cloned()
}

fn read_selection<T: serde::de::DeserializeOwned + Default>(store: &Store, key: &str) -> Result<T> {
    match store.get_setting_checked(key)? {
        Some(value) => serde_json::from_str(&value)
            .map_err(|error| AppError::internal("读取 PATH 版本选择", error.to_string())),
        None => Ok(T::default()),
    }
}

fn chosen_from<'a>(
    store: &Store,
    installed: &'a [crate::model::InstalledPackage],
    versions: &std::collections::BTreeMap<String, String>,
    id: &str,
) -> Result<Option<&'a crate::model::InstalledPackage>> {
    match versions.get(id) {
        // 已选版本卸载后不擅自换成其它版本，sync 会清除原有托管路径。
        Some(version) => Ok(installed
            .iter()
            .find(|p| p.id == id && p.version == *version)),
        None => {
            let active = store.get_setting_checked(&format!("active{id}Version"))?;
            Ok(installed
                .iter()
                .find(|p| p.id == id && Some(&p.version) == active.as_ref())
                .or_else(|| {
                    installed
                        .iter()
                        .filter(|p| p.id == id)
                        .min_by(|a, b| crate::versions::cmp_version_desc(&a.version, &b.version))
                }))
        }
    }
}

/// 当前终端使用的只读快照；总开关关闭也可生成脚本，不修改持久 PATH 或版本设置。
pub fn terminal_environment(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
) -> Result<crate::model::TerminalEnvironment> {
    terminal_environment_selected(store, paths, manifest, &Default::default())
}

/// 站点终端固定使用站点的 PHP；其余命令沿用 PATH 选择，不更改持久配置。
/// 调用方按 SITE_CHANGES -> lifecycle 的顺序锁定站点与安装版本。
pub fn site_terminal_environment(store: &Store, paths: &Paths, manifest: &Manifest, site_id: &str) -> Result<crate::model::TerminalEnvironment> {
    let site = crate::sites::get(store, site_id)?;
    let mut required = std::collections::BTreeMap::new();
    if site.runtime.kind == crate::model::SiteKind::Php {
        let version = site.runtime.php_version.as_ref().filter(|v| !v.is_empty()).ok_or_else(||
            AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", "该 PHP 站点尚未指定 PHP 版本，请先保存站点设置"))?;
        required.insert("php".into(), version.clone());
    }
    terminal_directory(&site.root_dir)?;
    let cwd = crate::envfile::project_root(std::path::Path::new(&site.root_dir)).canonicalize().map_err(|e| AppError::io("读取项目目录", e))?;
    let mut environment = terminal_environment_selected(store, paths, manifest, &required)?;
    environment.cwd = cwd.to_string_lossy().into_owned();
    environment.revision = terminal_revision(&environment, Some(site_id));
    Ok(environment)
}

pub fn terminal_directory(cwd: &str) -> Result<std::path::PathBuf> {
    if cwd.trim().is_empty() || cwd.chars().any(char::is_control) {
        return Err(AppError::new("TERMINAL_DIRECTORY_INVALID", "终端目录不能为空或包含控制字符"));
    }
    let directory = std::path::Path::new(cwd).canonicalize().map_err(|e| AppError::io("读取终端工作目录", e))?;
    if !directory.is_dir() { return Err(AppError::new("TERMINAL_DIRECTORY_INVALID", "终端工作目录不是文件夹")); }
    Ok(directory)
}

fn terminal_revision(environment: &crate::model::TerminalEnvironment, site_id: Option<&str>) -> String {
    use sha2::{Digest, Sha256};
    let payload = (&environment.shell, &environment.cwd, &environment.script, &environment.entries, &environment.warnings, site_id);
    format!("{:x}", Sha256::digest(serde_json::to_vec(&payload).expect("terminal snapshot contains only strings")))
}

fn terminal_environment_selected(store: &Store, paths: &Paths, manifest: &Manifest, required: &std::collections::BTreeMap<String, String>) -> Result<crate::model::TerminalEnvironment> {
    let installed = store.list_installed()?;
    let mut versions =
        read_selection::<std::collections::BTreeMap<String, String>>(store, VERSIONS_KEY)?;
    versions.extend(required.iter().map(|(id, version)| (id.clone(), version.clone())));
    let selected = read_selection::<Option<Vec<String>>>(store, SELECTED_KEY)?;
    let installer = crate::install::Installer {
        manifest: manifest.clone(),
    };
    let ids: std::collections::BTreeSet<_> = installed
        .iter()
        .map(|p| p.id.as_str())
        .chain(versions.keys().map(String::as_str))
        .collect();
    let mut entries = Vec::new();
    let mut warnings = Vec::new();
    for id in ids {
        if !wants(&selected, id) && !required.contains_key(id) {
            continue;
        }
        let Some(package) = chosen_from(store, &installed, &versions, id)? else {
            if let Some(version) = required.get(id) {
                return Err(AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", format!("站点指定的 {id} {version} 尚未安装，未打开终端"))
                    .with_hint("请先安装此版本，或在站点设置中选择已安装版本。"));
            }
            warnings.push(format!("{id}：所选 PATH 版本已卸载，请重新选择版本"));
            continue;
        };
        let meta = installer.installed_entry(package);
        if is_non_executable_entry(&meta.entry) {
            if required.contains_key(id) { return Err(AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", "站点指定版本没有可用的命令行入口")); }
            continue;
        }
        let check = (|| -> std::result::Result<String, String> {
            let entry = terminal_cli_entry(id, &meta.entry, cfg!(windows));
            let dir = bin_dir_for(&package.install_path, &entry)
                .ok_or_else(|| "无法确定安装入口，请重新安装该版本".to_string())?;
            let relative = entry.replace('\\', "/");
            if relative
                .split('/')
                .any(|part| part == ".." || part.contains(':'))
                || std::path::Path::new(&relative).is_absolute()
            {
                return Err("安装入口必须位于安装目录内".into());
            }
            if !std::path::Path::new(&package.install_path).is_absolute() {
                return Err("安装目录不是绝对路径，请重新安装该版本".into());
            }
            let root = std::path::Path::new(&package.install_path)
                .canonicalize()
                .map_err(|error| format!("无法读取安装目录：{error}"))?;
            let executable = root
                .join(relative)
                .canonicalize()
                .map_err(|error| format!("无法读取入口文件：{error}"))?;
            if !executable.starts_with(&root) || !executable.is_file() {
                return Err("入口文件不可用或指向安装目录外".into());
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if executable
                    .metadata()
                    .map_err(|e| e.to_string())?
                    .permissions()
                    .mode()
                    & 0o111
                    == 0
                {
                    return Err("入口文件没有执行权限".into());
                }
            }
            validate_terminal_dir(&dir, cfg!(windows)).map_err(|e| e.message)?;
            Ok(dir)
        })();
        match check {
            Ok(bin_dir) => entries.push(crate::model::TerminalEnvironmentEntry {
                id: id.into(),
                label: if id == "php" { "PHP".into() } else { meta.display_name },
                version: package.version.clone(),
                bin_dir,
            }),
            Err(reason) => {
                let message = format!("{} {}：{reason}", meta.display_name, package.version);
                if required.contains_key(id) { return Err(AppError::new("TERMINAL_RUNTIME_UNAVAILABLE", message).with_hint("站点指定版本不可用，请修复或重新安装后再打开终端。")); }
                warnings.push(message);
            }
        }
    }
    entries.sort_by_key(|entry| (!required.contains_key(&entry.id), entry.id.clone()));
    let dirs = entries
        .iter()
        .map(|entry| entry.bin_dir.clone())
        .collect::<Vec<_>>();
    let mut environment = crate::model::TerminalEnvironment {
        shell: if cfg!(windows) { "powershell" } else { "posix" }.into(),
        cwd: paths.base.to_string_lossy().into_owned(),
        revision: String::new(),
        script: render_terminal_script(&dirs, cfg!(windows))?,
        entries,
        warnings,
    };
    environment.revision = terminal_revision(&environment, None);
    Ok(environment)
}

fn terminal_cli_entry(id: &str, entry: &str, windows: bool) -> String {
    if id != "php" { return entry.into(); }
    let entry = entry.replace('\\', "/");
    let parent = entry.rsplit_once('/').map(|(parent, _)| parent).unwrap_or("");
    let parent = if !windows && (parent == "sbin" || parent.ends_with("/sbin")) {
        format!("{}bin", &parent[..parent.len() - 4])
    } else { parent.into() };
    let executable = if windows { "php.exe" } else { "php" };
    if parent.is_empty() { executable.into() } else { format!("{parent}/{executable}") }
}

/// 参数由后端快照生成，前端不能提交待执行脚本。
pub fn powershell_terminal_args(environment: &crate::model::TerminalEnvironment) -> Result<Vec<String>> {
    use base64::Engine;
    let script = format!("$ErrorActionPreference = 'Stop'\n{}", environment.script);
    let encoded = base64::engine::general_purpose::STANDARD.encode(script.encode_utf16().flat_map(u16::to_le_bytes).collect::<Vec<_>>());
    if encoded.len() > 24_000 { return Err(AppError::new("TERMINAL_ENV_TOO_LARGE", "终端环境过长，请减少 PATH 中选择的套件后重试")); }
    Ok(vec!["-NoLogo".into(), "-NoProfile".into(), "-NoExit".into(), "-EncodedCommand".into(), encoded])
}

pub fn posix_terminal_command(environment: &crate::model::TerminalEnvironment) -> Result<String> {
    if environment.cwd.chars().any(char::is_control) { return Err(AppError::new("TERMINAL_DIRECTORY_INVALID", "终端目录包含控制字符")); }
    let quote = |text: &str| format!("'{}'", text.replace('\'', "'\"'\"'"));
    let script = format!("cd -- {} || exit\n{}\nexec /bin/zsh -f", quote(&environment.cwd), environment.script);
    Ok(format!("/bin/zsh -f -c {}", quote(&script)))
}

fn validate_terminal_dir(dir: &str, windows: bool) -> Result<()> {
    if dir.is_empty()
        || dir.chars().any(char::is_control)
        || dir.contains(if windows { ';' } else { ':' })
    {
        return Err(AppError::new(
            "TERMINAL_PATH_INVALID",
            "目录含 PATH 分隔符或控制字符，无法安全加入 PATH",
        ));
    }
    Ok(())
}

/// 只改变运行脚本的终端进程环境；重复粘贴不会累加所选目录，其余 PATH 项保持原样。
fn render_terminal_script(dirs: &[String], windows: bool) -> Result<String> {
    for dir in dirs {
        validate_terminal_dir(dir, windows)?;
    }
    if dirs.is_empty() {
        return Ok(String::new());
    }
    if windows {
        let quoted = dirs
            .iter()
            .map(|dir| {
                let mut literal = String::from("'");
                for ch in dir.chars() {
                    literal.push(ch);
                    // PowerShell 也将弯引号识别为单引号。
                    if matches!(ch, '\'' | '\u{2018}' | '\u{2019}' | '\u{201a}' | '\u{201b}') {
                        literal.push(ch);
                    }
                }
                literal.push('\'');
                literal
            })
            .collect::<Vec<_>>()
            .join(",\n    ");
        Ok(format!(
            r#"& {{
  $nsbDirs = @(
    {quoted}
  )
  $nsbKeys = @($nsbDirs | ForEach-Object {{ $_.Replace('/', '\').TrimEnd('\') }})
  $nsbRest = @()
  if ($env:PATH) {{
    $nsbRest = @($env:PATH.Split(';') | Where-Object {{
      $nsbKeys -notcontains $_.Replace('/', '\').TrimEnd('\')
    }})
  }}
  $env:PATH = (@($nsbDirs) + $nsbRest) -join ';'
}}"#
        ))
    } else {
        let quote = |text: &str| format!("'{}'", text.replace('\'', "'\"'\"'"));
        let joined = quote(&dirs.join(":"));
        let choices = dirs.iter().map(|d| quote(d)).collect::<Vec<_>>().join("|");
        Ok(format!(
            r#"PATH="$(
  nsb_result={joined}
  nsb_rest=${{PATH-}}
  if [ -n "$nsb_rest" ]; then
    while :; do
      nsb_item=${{nsb_rest%%:*}}
      case "$nsb_item" in
        {choices}) ;;
        *) nsb_result=$nsb_result:$nsb_item ;;
      esac
      case "$nsb_rest" in
        *:*) nsb_rest=${{nsb_rest#*:}} ;;
        *) break ;;
      esac
    done
  fi
  printf '%s.' "$nsb_result"
)"
PATH=${{PATH%.}}
export PATH"#
        ))
    }
}

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
    // Windows 扩展路径不接受混合分隔符，否则真实目录会被误判为不存在。
    Some(if cfg!(windows) && base.starts_with(r"\\?\") {
        joined.replace('/', "\\")
    } else {
        joined
    })
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

/// 计算当前「应该」注入的目录集合（不受总开关影响，供 UI 预览与实际写入共用）。
///
/// 多版本包只取独立选择的 PATH 版本：同一 id 装了两个版本时，如果不做选择，
/// `php` 最终指向哪个版本取决于 PATH 顺序，行为不可预测。
pub fn desired_dirs(store: &Store, manifest: &Manifest) -> Vec<String> {
    let installer = crate::install::Installer {
        manifest: manifest.clone(),
    };
    let installed = store.list_installed().unwrap_or_default();
    let sel = selected_ids(store);

    // 按 id 归并，每个套件只保留一个 PATH 版本。
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
        let Some(chosen) = chosen_version(store, &id) else {
            continue;
        };
        let entry = installer.installed_entry(&chosen);
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
    let installer = crate::install::Installer {
        manifest: manifest.clone(),
    };
    let enabled = is_enabled(store);
    let sel = selected_ids(store);
    let applied = managed_dirs(store);
    let installed = store.list_installed().unwrap_or_default();

    let current_path = read_current_path_entries(store);
    let mut entries = Vec::new();
    for package in &installed {
        let meta = installer.installed_entry(package);
        let bin_dir = bin_dir_for(&package.install_path, &meta.entry);
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
            id: package.id.clone(),
            label: meta.display_name.clone(),
            version: package.version.clone(),
            bin_dir: bin_dir.unwrap_or_default(),
            exists,
            selected: wants(&sel, &package.id)
                && chosen_version(store, &package.id)
                    .is_some_and(|chosen| chosen.version == package.version),
            in_path,
            commands,
        });
    }

    // 漂移检测：开关开着，但磁盘上的实际内容与「应该注入的」不符。
    // 判据用真实 PATH（而不是我们的记录）：记录只能说明上次写了什么，
    // 用户手动删掉条目、或另一个程序覆盖了 PATH，都只有看磁盘才发现。
    let desired = if enabled {
        desired_dirs(store, manifest)
    } else {
        Vec::new()
    };
    let drift = enabled
        && desired
            .iter()
            .any(|d| !current_path.iter().any(|p| same_path(p, d)));

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
        "写入的是当前用户的环境变量（无需管理员）。已打开的终端不会自动更新，请新开一个终端窗口。"
            .to_string()
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
            .map(|p| {
                p.split(':')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect()
            })
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
pub fn merge_win_path(
    existing: &str,
    previously_managed: &[String],
    new_dirs: &[String],
) -> String {
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
    let desired = if enabled {
        desired_dirs(store, manifest)
    } else {
        Vec::new()
    };
    for dir in &desired {
        validate_terminal_dir(dir, cfg!(windows))?;
    }

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
            return Err(AppError::new(
                "NO_SHELL_PROFILE",
                "找不到用户 HOME 目录，无法写入 shell 配置",
            )
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

/// 从任一已安装版本直接加入/移出 PATH，整个操作由 CoreState 的生命周期锁串行执行。
pub fn set_version(
    store: &Store,
    paths: &Paths,
    manifest: &Manifest,
    id: &str,
    version: &str,
    selected: bool,
) -> Result<PathEnvStatus> {
    let current = status(store, manifest);
    let entry = current
        .entries
        .iter()
        .find(|entry| entry.id == id && entry.version == version)
        .ok_or_else(|| {
            AppError::new(
                "PATH_VERSION_UNAVAILABLE",
                "该版本尚未安装，或没有可加入环境变量的命令",
            )
        })?;
    if !selected && !entry.selected {
        return Ok(current);
    }
    let previous_versions = store
        .get_setting(VERSIONS_KEY)
        .unwrap_or_else(|| "{}".into());
    let previous_selected = store
        .get_setting(SELECTED_KEY)
        .unwrap_or_else(|| "null".into());
    let previous_enabled = store.get_setting(ENABLED_KEY).unwrap_or_else(|| "0".into());
    let mut versions =
        store.get_setting_or::<std::collections::BTreeMap<String, String>>(VERSIONS_KEY);
    let mut ids: Vec<String> = if current.enabled {
        current
            .entries
            .iter()
            .filter(|entry| entry.selected)
            .map(|entry| entry.id.clone())
            .collect()
    } else {
        Vec::new()
    };
    ids.retain(|value| value != id);
    if selected {
        versions.insert(id.to_string(), version.to_string());
        ids.push(id.to_string());
    }
    let result = (|| {
        store.set_setting_json(VERSIONS_KEY, &versions)?;
        set_selected_ids(store, &ids)?;
        if selected {
            store.set_setting(ENABLED_KEY, "1")?;
        }
        apply(store, paths, manifest)
    })();
    match result {
        Ok(status) => Ok(status),
        Err(error) => {
            // 注册表/配置文件写入失败时恢复选择，避免界面显示成已经切换成功。
            let rollback = (|| -> Result<()> {
                let mut cleanup = current.managed_dirs.clone();
                cleanup.extend(desired_dirs(store, manifest).into_iter().filter(|dir| {
                    !current
                        .entries
                        .iter()
                        .any(|entry| entry.in_path && same_path(&entry.bin_dir, dir))
                }));
                store.set_setting_json(DIRS_KEY, &cleanup)?;
                store.set_setting(VERSIONS_KEY, &previous_versions)?;
                store.set_setting(SELECTED_KEY, &previous_selected)?;
                store.set_setting(ENABLED_KEY, &previous_enabled)?;
                apply(store, paths, manifest).map(|_| ())
            })();
            if let Err(rollback_error) = rollback {
                return Err(AppError::new(
                    "PATH_VERSION_UPDATE_FAILED",
                    "环境变量更新失败，恢复原设置时也遇到问题",
                )
                .with_hint("请到工具箱的环境变量卡片重新应用 PATH")
                .with_detail(format!("{}; {}", error.message, rollback_error.message)));
            }
            Err(error)
        }
    }
}

/// 安装/卸载/切换版本后自动同步（开关没开就只更新一次状态，不写盘）。
/// 这样用户装完 PHP 不用再手动回来点一次。
pub fn sync(store: &Store, paths: &Paths, manifest: &Manifest) -> Result<()> {
    if !is_enabled(store) {
        return Ok(());
    }
    apply(store, paths, manifest).map(|_| ())
}

enum MigrationSystemPath {
    Unchanged,
    #[cfg(windows)]
    Windows {
        previous: platform::pathenv::RawPath,
        expected: String,
    },
    #[cfg(not(windows))]
    Profiles(Vec<(std::path::PathBuf, Option<String>, String)>),
}

/// 只用于尚未确认接管的迁移子进程；失败后恢复其激活状态，供原窗口继续重试。
pub struct MigrationActivationRollback {
    paths: Paths,
    managed: Vec<String>,
    marker: Option<Vec<u8>>,
    system: MigrationSystemPath,
}

fn rollback_content_needed(
    current: Option<&str>,
    previous: Option<&str>,
    expected: &str,
) -> Result<bool> {
    if current == previous {
        return Ok(false);
    }
    if current == Some(expected) {
        return Ok(true);
    }
    Err(AppError::new(
        "PATH_ROLLBACK_CONFLICT",
        "启动过程中环境变量被其它操作修改，未覆盖这些改动",
    )
    .with_hint("请检查工具箱中的环境变量状态后重新应用 PATH"))
}

impl MigrationActivationRollback {
    pub fn capture(target: &std::path::Path) -> Result<Self> {
        let paths = Paths::new(target.to_path_buf());
        let store = Store::open(paths.db())?;
        let managed = read_selection::<Vec<String>>(&store, DIRS_KEY)?;
        let marker = match std::fs::read(paths.base.join(".data-dir-activation.json")) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::io("读取迁移激活状态", error)),
        };
        let enabled = store.get_setting_checked(ENABLED_KEY)?.as_deref() == Some("1");
        let system = if enabled && marker.is_some() {
            let manifest = crate::install::Installer::effective(&paths).manifest;
            let desired = desired_dirs(&store, &manifest);
            for dir in &desired {
                validate_terminal_dir(dir, cfg!(windows))?;
            }
            #[cfg(windows)]
            {
                let previous = platform::pathenv::read_user_path().map_err(AppError::from)?;
                let expected = merge_win_path(&previous.value, &managed, &desired);
                MigrationSystemPath::Windows { previous, expected }
            }
            #[cfg(not(windows))]
            {
                let mut snapshots = Vec::new();
                for path in platform::pathenv::shell_profiles() {
                    let previous = match std::fs::read_to_string(&path) {
                        Ok(value) => Some(value),
                        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                        Err(error) => return Err(AppError::io("保存 shell PATH 快照", error)),
                    };
                    let expected = platform::pathenv::merge_profile_content(
                        previous.as_deref().unwrap_or(""),
                        &desired,
                    );
                    snapshots.push((path, previous, expected));
                }
                MigrationSystemPath::Profiles(snapshots)
            }
        } else {
            MigrationSystemPath::Unchanged
        };
        Ok(Self {
            paths,
            managed,
            marker,
            system,
        })
    }

    pub fn restore(self) -> Result<()> {
        let mut failures = Vec::new();
        let restored = (|| -> Result<()> {
            match self.system {
                MigrationSystemPath::Unchanged => {}
                #[cfg(windows)]
                MigrationSystemPath::Windows { previous, expected } => {
                    let current = platform::pathenv::read_user_path().map_err(AppError::from)?;
                    if current.reg_type != previous.reg_type {
                        return Err(AppError::new(
                            "PATH_ROLLBACK_CONFLICT",
                            "用户 PATH 类型已改变，未覆盖其它操作",
                        ));
                    }
                    if rollback_content_needed(
                        Some(&current.value),
                        Some(&previous.value),
                        &expected,
                    )? {
                        platform::pathenv::write_user_path(&previous.value, previous.reg_type)
                            .map_err(AppError::from)?;
                    }
                }
                #[cfg(not(windows))]
                MigrationSystemPath::Profiles(snapshots) => {
                    for (path, previous, expected) in snapshots {
                        let attempt = (|| -> Result<()> {
                            let current = match std::fs::read_to_string(&path) {
                                Ok(value) => Some(value),
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
                                Err(error) => return Err(AppError::io("读取 shell PATH", error)),
                            };
                            if rollback_content_needed(
                                current.as_deref(),
                                previous.as_deref(),
                                &expected,
                            )? {
                                match previous {
                                    Some(content) => std::fs::write(&path, content)?,
                                    None => std::fs::remove_file(&path)?,
                                }
                            }
                            Ok(())
                        })();
                        if let Err(error) = attempt {
                            failures.push(format!("{}：{}", path.display(), error.message));
                        }
                    }
                }
            }
            Ok(())
        })();
        if let Err(error) = restored {
            failures.push(error.message);
        }
        let local = (|| -> Result<()> {
            Store::open(self.paths.db())?.set_setting_json(DIRS_KEY, &self.managed)?;
            if let Some(marker) = self.marker {
                crate::paths::write_atomic(
                    &self.paths.base.join(".data-dir-activation.json"),
                    &marker,
                )?;
            }
            Ok(())
        })();
        if let Err(error) = local {
            failures.push(error.message);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(AppError::new(
                "DATA_DIR_ACTIVATION_ROLLBACK_FAILED",
                "新进程未完成启动，恢复迁移激活状态时也遇到问题",
            )
            .with_hint("副本已保留。请检查环境变量状态与目录权限后再重试")
            .with_detail(failures.join("；")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn migration_activation_rollback_restores_copy_for_retry_without_system_writes() {
        let temp=tempfile::tempdir().unwrap();
        let paths=Paths::new(temp.path().to_path_buf());paths.ensure_dirs().unwrap();
        let store=Store::open(paths.db()).unwrap();
        store.set_setting_json(DIRS_KEY,&vec!["old-managed"]).unwrap();
        let marker=serde_json::to_vec(&paths.base).unwrap();
        let marker_file=paths.base.join(".data-dir-activation.json");
        std::fs::write(&marker_file,&marker).unwrap();
        // PATH 未启用：capture/restore 不读取或修改实际注册表/profile。
        let rollback=MigrationActivationRollback::capture(&paths.base).unwrap();
        store.set_setting_json(DIRS_KEY,&vec!["new-managed"]).unwrap();
        std::fs::remove_file(&marker_file).unwrap();
        rollback.restore().unwrap();
        assert_eq!(managed_dirs(&store),vec!["old-managed"]);
        assert_eq!(std::fs::read(marker_file).unwrap(),marker);
    }

    #[test]
    fn migration_path_rollback_preserves_concurrent_edits() {
        assert!(!rollback_content_needed(Some("original"),Some("original"),"migrated").unwrap());
        assert!(rollback_content_needed(Some("migrated"),Some("original"),"migrated").unwrap());
        assert!(rollback_content_needed(Some("new managed block"),None,"new managed block").unwrap());
        assert_eq!(rollback_content_needed(Some("external edit"),Some("original"),"migrated").unwrap_err().code,"PATH_ROLLBACK_CONFLICT");
    }

    fn v(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    fn terminal_fixture() -> (tempfile::TempDir, Store, Paths, Manifest) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        let store = Store::open(paths.db()).unwrap();
        let mut manifest = crate::install::Installer::bundled().manifest;
        let mut template = manifest.packages[0].clone();
        template.id = "terminal-fixture".into();
        template.entry = if cfg!(windows) {
            "nested/bin/tool.exe"
        } else {
            "nested/bin/tool"
        }
        .into();
        manifest.packages.clear();
        for version in ["1.0.0", "2.0.0"] {
            let mut entry = template.clone();
            entry.version = version.into();
            let root = paths.base.join(format!("custom-{version}"));
            let executable = root.join(&entry.entry);
            std::fs::create_dir_all(executable.parent().unwrap()).unwrap();
            std::fs::write(&executable, "fixture").unwrap();
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
                    .unwrap();
            }
            store
                .upsert_installed(&crate::model::InstalledPackage {
                    id: entry.id.clone(),
                    version: version.into(),
                    category: "runtime".into(),
                    install_path: root.to_string_lossy().into_owned(),
                    config_path: String::new(),
                    installed_at: 0,
                })
                .unwrap();
            manifest.packages.push(entry);
        }
        (temp, store, paths, manifest)
    }

    #[test]
    fn terminal_uses_selected_version_and_real_nested_entry_without_writes() {
        let (_temp, store, paths, manifest) = terminal_fixture();
        store.set_setting(ENABLED_KEY, "0").unwrap();
        store
            .set_setting("activeterminal-fixtureVersion", "2.0.0")
            .unwrap();
        store
            .set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"1.0.0"}"#)
            .unwrap();
        let result = terminal_environment(&store, &paths, &manifest).unwrap();
        assert_eq!(result.entries.len(), 1);
        assert_eq!(result.entries[0].version, "1.0.0");
        assert!(result.entries[0]
            .bin_dir
            .replace('\\', "/")
            .ends_with("custom-1.0.0/nested/bin"));
        assert!(result.warnings.is_empty());
        assert!(!is_enabled(&store));
        assert_eq!(
            store
                .get_setting("activeterminal-fixtureVersion")
                .as_deref(),
            Some("2.0.0")
        );
        assert!(store.get_setting(DIRS_KEY).is_none());
    }

    #[test]
    fn terminal_does_not_substitute_uninstalled_or_broken_selected_version() {
        let (_temp, store, paths, mut manifest) = terminal_fixture();
        store
            .set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"3.0.0"}"#)
            .unwrap();
        let missing = terminal_environment(&store, &paths, &manifest).unwrap();
        assert!(missing.entries.is_empty());
        assert!(missing.script.is_empty());
        assert_eq!(missing.warnings.len(), 1);
        store
            .set_setting(VERSIONS_KEY, r#"{"terminal-fixture":"2.0.0"}"#)
            .unwrap();
        manifest.packages[1].entry = "../outside/tool".into();
        let unsafe_entry = terminal_environment(&store, &paths, &manifest).unwrap();
        assert!(unsafe_entry.entries.is_empty());
        assert_eq!(unsafe_entry.warnings.len(), 1);
        manifest.packages[1].entry = "missing/tool".into();
        assert_eq!(
            terminal_environment(&store, &paths, &manifest)
                .unwrap()
                .warnings
                .len(),
            1
        );
    }

    #[test]
    fn terminal_respects_empty_selection_and_reports_corrupt_settings() {
        let (_temp, store, paths, manifest) = terminal_fixture();
        assert_eq!(
            terminal_environment(&store, &paths, &manifest)
                .unwrap()
                .entries[0]
                .version,
            "2.0.0"
        );
        store.set_setting(SELECTED_KEY, "[]").unwrap();
        assert!(terminal_environment(&store, &paths, &manifest)
            .unwrap()
            .entries
            .is_empty());
        store.set_setting(SELECTED_KEY, "broken").unwrap();
        assert!(terminal_environment(&store, &paths, &manifest).is_err());
    }

    #[test]
    fn terminal_rejects_path_separators_and_control_characters() {
        for (dir, windows) in [
            ("C:/bad;path", true),
            ("/bad:path", false),
            ("/bad\npath", false),
        ] {
            assert!(render_terminal_script(&v(&[dir]), windows).is_err());
        }
        assert!(render_terminal_script(&[], true).unwrap().is_empty());
    }

    fn site_terminal_fixture() -> (tempfile::TempDir, crate::CoreState, crate::model::Site) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().join("data"));
        paths.ensure_dirs().unwrap();
        let store = Store::open(paths.db()).unwrap();
        let mut manifest = crate::install::Installer::bundled().manifest;
        let mut template = manifest.packages.iter().find(|p| p.id == "php").cloned().unwrap_or_else(|| manifest.packages[0].clone());
        template.id = "php".into();
        template.entry = if cfg!(windows) { "nested/bin/php-cgi.exe" } else { "nested/sbin/php-fpm" }.into();
        manifest.packages.clear();
        for version in ["1.0.0", "2.0.0"] {
            let mut entry = template.clone(); entry.version = version.into();
            let root = paths.base.join(format!("PHP O'Brien $var`&()/{version}"));
            for relative in [entry.entry.clone(), terminal_cli_entry("php", &entry.entry, cfg!(windows))] {
                let file = root.join(relative); std::fs::create_dir_all(file.parent().unwrap()).unwrap(); std::fs::write(&file, "fixture").unwrap();
                #[cfg(unix)] { use std::os::unix::fs::PermissionsExt; std::fs::set_permissions(file, std::fs::Permissions::from_mode(0o755)).unwrap(); }
            }
            store.upsert_installed(&crate::model::InstalledPackage { id: "php".into(), version: version.into(), category: "runtime".into(), install_path: root.to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
            manifest.packages.push(entry);
        }
        let project = temp.path().join("Project 中文 O'Brien $var`&()");
        std::fs::create_dir_all(project.join("public")).unwrap(); std::fs::write(project.join("composer.json"), "{}").unwrap();
        let site: crate::model::Site = serde_json::from_value(serde_json::json!({"id":"terminal-site","name":"Terminal","domains":["terminal.test"],"rootDir":project.join("public"),"runtime":{"kind":"php","phpVersion":"1.0.0"},"https":false,"rewrite":"none","createdAt":1,"updatedAt":1})).unwrap();
        store.save_site(&site).unwrap();
        let state = crate::CoreState { paths, store, manager: std::sync::Arc::new(crate::services::ServiceManager::new()),
            downloader: std::sync::Arc::new(crate::download::Downloader::new()), installer: crate::install::Installer { manifest },
            emit: std::sync::Arc::new(|_| {}), watchdog: std::sync::Arc::new(crate::watchdog::Watchdog::new()) };
        (temp, state, site)
    }

    #[test]
    fn site_terminal_uses_php_cli_and_project_root_without_changing_global_selection() {
        let (_temp, state, site) = site_terminal_fixture();
        state.store.set_setting(VERSIONS_KEY, r#"{"php":"2.0.0"}"#).unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        state.store.set_setting("activephpVersion", "2.0.0").unwrap();
        let result = state.site_terminal_environment(&site.id).unwrap();
        assert_eq!(result.entries.len(), 1); assert_eq!(result.entries[0].version, "1.0.0");
        assert!(result.entries[0].bin_dir.replace('\\', "/").ends_with("1.0.0/nested/bin"));
        assert!(std::path::Path::new(&result.cwd).join("composer.json").is_file());
        assert!(state.terminal_environment().unwrap().entries.is_empty());
        assert_eq!(state.store.get_setting(VERSIONS_KEY).as_deref(), Some(r#"{"php":"2.0.0"}"#));
        assert_eq!(state.store.get_setting("activephpVersion").as_deref(), Some("2.0.0"));
        assert!(!is_enabled(&state.store)); assert!(state.store.get_setting(DIRS_KEY).is_none());
        let cli = std::path::Path::new(&result.entries[0].bin_dir).join(if cfg!(windows) { "php.exe" } else { "php" });
        std::fs::remove_file(cli).unwrap();
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
    }

    #[test]
    fn terminal_launch_rechecks_site_versions_directory_and_holds_lifecycle_locks() {
        let (temp, state, mut site) = site_terminal_fixture();
        let view = state.site_terminal_environment(&site.id).unwrap();
        let called = std::cell::Cell::new(false);
        state.with_terminal_environment(Some(&site.id), &view.revision, |actual| {
            assert_eq!(actual.revision, view.revision);
            std::thread::scope(|scope| scope.spawn(|| {
                assert!(state.manager.lifecycle.try_lock().is_none());
                assert!(crate::sites::SITE_CHANGES.try_lock().is_none());
            }).join().unwrap());
            called.set(true); Ok(())
        }).unwrap();
        assert!(called.get());
        site.runtime.php_version = Some("2.0.0".into()); state.store.save_site(&site).unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &view.revision, |_| panic!("stale PHP launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        let view = state.site_terminal_environment(&site.id).unwrap();
        let other = temp.path().join("other"); std::fs::create_dir(&other).unwrap(); site.root_dir = other.to_string_lossy().into_owned(); state.store.save_site(&site).unwrap();
        assert_eq!(state.with_terminal_environment::<()>(Some(&site.id), &view.revision, |_| panic!("stale directory launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        site.runtime.php_version = Some("missing".into()); state.store.save_site(&site).unwrap();
        assert_eq!(state.site_terminal_environment(&site.id).unwrap_err().code, "TERMINAL_RUNTIME_UNAVAILABLE");
        let global = state.terminal_environment().unwrap();
        state.store.set_setting(SELECTED_KEY, "[]").unwrap();
        assert_eq!(state.with_terminal_environment::<()>(None, &global.revision, |_| panic!("stale selection launched")).unwrap_err().code, "TERMINAL_ENV_CHANGED");
        for invalid in ["", "\n", "a\0b"] { assert_eq!(terminal_directory(invalid).unwrap_err().code, "TERMINAL_DIRECTORY_INVALID"); }
        assert!(terminal_directory(temp.path().join("absent").to_str().unwrap()).is_err());
    }

    #[cfg(windows)]
    #[test]
    #[ignore = "requires NSB_ENV_PHP; runs a short hidden PowerShell with real PHP, no interactive terminal"]
    fn native_site_terminal_resolves_real_php_and_literal_working_directory() {
        let php = std::path::PathBuf::from(std::env::var_os("NSB_ENV_PHP").expect("NSB_ENV_PHP")).canonicalize().unwrap();
        let (_temp, mut state, mut site) = site_terminal_fixture();
        let root = php.parent().unwrap();
        let mut entry = state.installer.manifest.packages[0].clone(); entry.version = "8.4.26".into(); entry.entry = "php-cgi.exe".into();
        state.installer.manifest.packages.push(entry);
        state.store.upsert_installed(&crate::model::InstalledPackage { id: "php".into(), version: "8.4.26".into(), category: "runtime".into(), install_path: root.to_string_lossy().into_owned(), config_path: String::new(), installed_at: 0 }).unwrap();
        site.runtime.php_version = Some("8.4.26".into()); state.store.save_site(&site).unwrap();
        let mut environment = state.site_terminal_environment(&site.id).unwrap();
        environment.script.push_str("\n[Console]::OutputEncoding = [Text.UTF8Encoding]::new($false)\n$resolved = (Get-Command php -CommandType Application).Source\n$version = & php -n -r 'echo PHP_VERSION;'\nif ($LASTEXITCODE -ne 0) { exit $LASTEXITCODE }\n[pscustomobject]@{ resolved=$resolved; version=$version; cwd=(Get-Location).ProviderPath } | ConvertTo-Json -Compress");
        let args = powershell_terminal_args(&environment).unwrap().into_iter().filter(|arg| arg != "-NoExit").collect::<Vec<_>>();
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap()).join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let global_path = std::env::var_os("PATH");
        let output = platform::command(powershell).args(args).current_dir(terminal_directory(&environment.cwd).unwrap()).env("PATH", "C:/not-the-selected-php").output().unwrap();
        assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
        let result: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(std::path::Path::new(result["resolved"].as_str().unwrap()).canonicalize().unwrap(), php);
        assert_eq!(result["version"], "8.4.26");
        assert_eq!(std::path::Path::new(result["cwd"].as_str().unwrap()).canonicalize().unwrap(), terminal_directory(&environment.cwd).unwrap());
        assert_eq!(std::env::var_os("PATH"), global_path);
    }

    #[cfg(windows)]
    #[test]
    fn terminal_powershell_is_literal_idempotent_and_preserves_other_entries() {
        use base64::Engine;
        let dirs = v(&[
            "C:/Nice Env/O'Brien/$nsbInjection`&()‘’‚‛/bin",
            "D:/second/bin",
        ]);
        let script = render_terminal_script(&dirs, true).unwrap();
        let program = format!("$nsbInjection = 'EXPANDED'\n{script}\n{script}\nif (Get-Variable nsbDirs -ErrorAction SilentlyContinue) {{ throw 'scope leak' }}\n[Convert]::ToBase64String([Text.Encoding]::UTF8.GetBytes($env:PATH))");
        let encoded = base64::engine::general_purpose::STANDARD.encode(
            program
                .encode_utf16()
                .flat_map(u16::to_le_bytes)
                .collect::<Vec<_>>(),
        );
        let powershell = std::path::PathBuf::from(std::env::var_os("SystemRoot").unwrap())
            .join("System32/WindowsPowerShell/v1.0/powershell.exe");
        let output = platform::command(powershell)
            .args(["-NoProfile", "-NonInteractive", "-EncodedCommand", &encoded])
            .env("PATH", "C:/Other;;d:/SECOND/bin/;C:/Other;")
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let actual = base64::engine::general_purpose::STANDARD
            .decode(String::from_utf8_lossy(&output.stdout).trim())
            .unwrap();
        assert_eq!(
            String::from_utf8(actual).unwrap(),
            format!("{};{};C:/Other;;C:/Other;", dirs[0], dirs[1])
        );
    }

    #[test]
    fn terminal_posix_is_literal_idempotent_and_preserves_other_entries() {
        // Windows 上用已有 Git Bash；无该工具的平台仍执行纯函数与 PowerShell 回归。
        #[cfg(windows)]
        let shell = std::env::var_os("PATH")
            .into_iter()
            .flat_map(|value| std::env::split_paths(&value).collect::<Vec<_>>())
            .filter(|dir| dir.join("git.exe").is_file())
            .flat_map(|dir| {
                dir.ancestors()
                    .take(3)
                    .map(|parent| parent.join("bin/bash.exe"))
                    .collect::<Vec<_>>()
            })
            .find(|path| path.is_file())
            .unwrap_or_default();
        #[cfg(not(windows))]
        let shell = std::path::PathBuf::from("/bin/sh");
        if !shell.is_file() {
            return;
        }
        let dirs = v(&["/Nice Env/O'Brien/$nsbInjection`&()\\/bin", "/second/bin"]);
        let script = render_terminal_script(&dirs, false).unwrap();
        let program = format!("PATH='/other::/second/bin:/other:'\nnsbInjection=EXPANDED\n{script}\n{script}\n[ -z \"${{nsb_result+x}}\" ] || exit 2\nprintf '%s' \"$PATH\"");
        // /bin/sh 不认识 Bash 启动参数。
        #[cfg(not(windows))]
        let output = platform::command("/bin/sh")
            .args(["-c", &program])
            .output()
            .unwrap();
        #[cfg(windows)]
        let output = platform::command(shell)
            .args(["--noprofile", "--norc", "-c", &program])
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert_eq!(
            String::from_utf8(output.stdout).unwrap(),
            format!("{}:{}:/other::/other:", dirs[0], dirs[1])
        );
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
        assert_eq!(
            out,
            "D:\\rt\\php\\8.3.33;%SystemRoot%\\system32;C:\\Other\\bin"
        );
    }

    #[test]
    fn merge_removes_previously_managed_dirs() {
        let existing = "D:\\rt\\php\\8.3.33;C:\\Other\\bin";
        let out = merge_win_path(
            existing,
            &v(&["D:\\rt\\php\\8.3.33"]),
            &v(&["D:\\rt\\php\\8.4.25"]),
        );
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
        let out = strip_win_path(existing, &v(&["D:\\rt\\php\\8.3.33", "D:\\rt\\go\\1.24.1"]));
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
        let out = merge_win_path(
            "D:\\rt\\php\\8.3.33\\;C:\\keep",
            &[],
            &v(&["D:\\rt\\php\\8.3.33"]),
        );
        assert_eq!(out, "D:\\rt\\php\\8.3.33;C:\\keep");
    }
}
