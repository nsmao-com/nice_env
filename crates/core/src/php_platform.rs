//! 只读 Composer 平台检查：仅在临时目录处理元数据，不加载项目代码。
use crate::{
    error::{AppError, Result},
    paths::Paths,
    sites::ProjectPhp,
    store::Store,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    io::Read,
    path::Path,
    process::Stdio,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlatformRequirement {
    pub name: String,
    pub version: String,
    pub status: String,
    #[serde(alias = "failed_requirement")]
    pub failed_requirement: Option<FailedRequirement>,
    pub provider: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FailedRequirement {
    pub source: String,
    pub constraint: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectPlatformReport {
    pub project: String,
    pub php_version: String,
    pub ini: String,
    pub source: String,
    pub include_dev: bool,
    pub dev_incomplete: bool,
    pub lock_fresh: Option<bool>,
    pub autoload_present: bool,
    pub requirements: Vec<PlatformRequirement>,
    pub diagnostics: Option<String>,
}

pub fn site_project_root(web_root: &Path) -> std::path::PathBuf {
    crate::envfile::project_root(web_root)
}

fn invalid(detail: impl ToString) -> AppError {
    AppError::new("PHP_PLATFORM_METADATA", "项目依赖清单无法用于环境检查")
        .with_hint("请修复 composer.json、composer.lock 或已安装依赖的元数据后重试")
        .with_detail(detail.to_string())
}

fn read_json(root: &Path, relative: &str, limit: u64) -> Result<Option<(Value, Vec<u8>)>> {
    let path = crate::paths::checked_data_path(root, relative)
        .map_err(|e| invalid(format!("{relative}: {e}")))?;
    // 先检查文件类型，避免在 Unix 上打开 FIFO 等特殊文件时阻塞。
    match std::fs::symlink_metadata(&path) {
        Ok(metadata) if !metadata.is_file() => {
            return Err(invalid(format!("{relative} 不是普通文件")))
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::io(&format!("读取 {relative}"), e)),
    }
    let file = match std::fs::File::open(&path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(AppError::io(&format!("读取 {relative}"), e)),
    };
    if !file.metadata()?.is_file() {
        return Err(invalid(format!("{relative} 不是普通文件")));
    }
    let mut bytes = Vec::new();
    file.take(limit + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > limit {
        return Err(invalid(format!("{relative} 超出只读检查的大小限制")));
    }
    let value = serde_json::from_slice(&bytes).map_err(|e| invalid(format!("{relative}: {e}")))?;
    Ok(Some((value, bytes)))
}

fn links(value: &Value, key: &str) -> Result<Value> {
    match value.get(key) {
        None => Ok(json!({})),
        Some(Value::Object(map))
            if map.iter().all(|(name, constraint)| {
                !name.is_empty()
                    && name.len() <= 256
                    && constraint
                        .as_str()
                        .is_some_and(|s| !s.is_empty() && s.len() <= 4096)
            }) =>
        {
            Ok(Value::Object(map.clone()))
        }
        _ => Err(invalid(format!("{key} 必须是名称与版本约束的映射"))),
    }
}

// 仅保留 Composer 解析平台约束所需的字段；不复制安装器、脚本、路径仓库或 autoload。
fn packages(value: Option<&Value>) -> Result<Vec<Value>> {
    let input = value
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("缺少依赖 packages 数组"))?;
    if input.len() > 10000 {
        return Err(invalid("依赖数量超出检查限制"));
    }
    input
        .iter()
        .map(|package| {
            let name = package
                .get("name")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or_else(|| invalid("依赖缺少名称"))?;
            let version = package
                .get("version")
                .and_then(Value::as_str)
                .filter(|s| !s.is_empty() && s.len() <= 256)
                .ok_or_else(|| invalid(format!("{name} 缺少版本")))?;
            let mut result = json!({"name":name,"version":version});
            for key in ["require", "provide", "replace"] {
                result[key] = links(package, key)?;
            }
            Ok(result)
        })
        .collect()
}

const PROBE: &str = r#"
require 'phar://'.$argv[1].'/vendor/autoload.php';
$original = file_get_contents('original.json');
$lock = json_decode(file_get_contents('composer.lock'), true, 512, JSON_THROW_ON_ERROR);
$fresh = isset($lock['content-hash']) ? hash_equals($lock['content-hash'], Composer\Package\Locker::getContentHash($original)) : null;
file_put_contents('runtime.json', json_encode(['php'=>PHP_VERSION,'ini'=>php_ini_loaded_file(),'fresh'=>$fresh], JSON_THROW_ON_ERROR));
$args = ['command'=>'check-platform-reqs', '--lock'=>true, '--format'=>'json', '--no-plugins'=>true, '--no-scripts'=>true, '--no-interaction'=>true, '--no-ansi'=>true, '--no-cache'=>true];
if ($argv[2] === 'no-dev') $args['--no-dev'] = true;
$app = new Composer\Console\Application();
$app->setAutoExit(false);
exit($app->run(new Symfony\Component\Console\Input\ArrayInput($args)));
"#;

/// 保留 Composer 的非零退出及 JSON，stderr 单独展示；限制时间和输出，清理自有进程组。
fn capture(command: &mut std::process::Command) -> Result<(i32, Vec<u8>, String)> {
    use std::io::{Seek, SeekFrom};
    let mut stdout = tempfile::tempfile()?;
    let mut stderr = tempfile::tempfile()?;
    command
        .stdin(Stdio::null())
        .stdout(stdout.try_clone()?)
        .stderr(stderr.try_clone()?);
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(platform::spawn_pre_exec);
        }
    }
    let mut group = platform::ProcessGroup::new()?;
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("启动 PHP 环境检查", e))?;
    if let Err(e) = group.attach(child.id()) {
        let _ = child.kill();
        let _ = child.wait();
        return Err(e.into());
    }
    let start = Instant::now();
    let result = (|| -> Result<std::process::ExitStatus> {
        loop {
            if stdout.metadata()?.len() > 1024 * 1024 || stderr.metadata()?.len() > 1024 * 1024 {
                return Err(AppError::new(
                    "PHP_PLATFORM_LIMIT",
                    "环境检查输出过多，请修复 PHP 配置后重试",
                ));
            }
            if let Some(status) = child.try_wait()? {
                return Ok(status);
            }
            if start.elapsed() > Duration::from_secs(15) {
                return Err(AppError::new(
                    "PHP_PLATFORM_TIMEOUT",
                    "环境检查超过 15 秒，请修复 PHP 或 Composer 后重试",
                ));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
    })();
    let cleanup = group.terminate(true);
    if result.is_err() || cleanup.is_err() {
        let _ = child.kill();
    }
    let _ = child.wait();
    stderr.seek(SeekFrom::Start(0))?;
    let mut detail = Vec::new();
    stderr.take(65536).read_to_end(&mut detail)?;
    let detail = String::from_utf8_lossy(&detail).into_owned();
    let status = result.map_err(|e| e.with_detail(&detail))?;
    cleanup?;
    stdout.seek(SeekFrom::Start(0))?;
    let mut output = Vec::new();
    stdout.take(1024 * 1024 + 1).read_to_end(&mut output)?;
    if output.len() > 1024 * 1024 {
        return Err(invalid("Composer 输出超出检查限制"));
    }
    Ok((status.code().unwrap_or(-1), output, detail))
}

pub fn check(
    paths: &Paths,
    store: &Store,
    project: &Path,
    version: &str,
    include_dev: bool,
) -> Result<ProjectPlatformReport> {
    static CHECK_RUNNING: parking_lot::Mutex<()> = parking_lot::Mutex::new(());
    let _guard = CHECK_RUNNING.try_lock().ok_or_else(|| {
        AppError::new("PHP_PLATFORM_BUSY", "已有项目环境检查正在运行，请稍后重试")
    })?;
    // binaries 先校验已安装版本，避免用户输入被用于拼接配置路径。
    let php = ProjectPhp::resolve(paths, store, version)?;
    let project = project
        .canonicalize()
        .map_err(|e| AppError::io("读取项目目录", e))?;
    if !project.is_dir() {
        return Err(invalid("项目路径不是目录"));
    }
    let (manifest, original) =
        read_json(&project, "composer.json", 512 * 1024)?.ok_or_else(|| {
            AppError::new(
                "PHP_PLATFORM_NO_MANIFEST",
                "此目录没有 composer.json，无法自动确定项目需要的 PHP 扩展",
            )
            .with_hint("请选择项目根目录，或按项目文档在扩展管理中核对")
        })?;
    if !manifest.is_object() {
        return Err(invalid("composer.json 必须是对象"));
    }
    let require = links(&manifest, "require")?;
    let require_dev = links(&manifest, "require-dev")?;
    let provides = links(&manifest, "provide")?;
    let replaces = links(&manifest, "replace")?;
    if manifest.get("version").and_then(Value::as_str).is_none()
        && [provides.as_object(), replaces.as_object()]
            .into_iter()
            .flatten()
            .any(|map| {
                map.values()
                    .any(|value| value.as_str() == Some("self.version"))
            })
    {
        return Err(invalid(
            "项目根包使用 self.version 提供能力，但清单未声明根包版本；本次无法只读确认此能力版本",
        ));
    }
    let mut lock =
        json!({"packages":[],"packages-dev":[],"aliases":[],"platform":{},"platform-dev":{}});
    let source;
    let mut dev_incomplete = false;
    if let Some((existing, _)) = read_json(&project, "composer.lock", 8 * 1024 * 1024)? {
        lock["packages"] = json!(packages(existing.get("packages"))?);
        lock["packages-dev"] = json!(packages(existing.get("packages-dev"))?);
        for key in ["platform", "platform-dev"] {
            lock[key] = links(&existing, key)?;
        }
        if let Some(hash) = existing.get("content-hash") {
            if !hash
                .as_str()
                .is_some_and(|s| s.len() == 32 && s.bytes().all(|c| c.is_ascii_hexdigit()))
            {
                return Err(invalid("composer.lock 的 content-hash 无效"));
            }
            lock["content-hash"] = hash.clone();
        }
        source = "lock";
    } else {
        let vendor = match manifest.get("config").and_then(|v| v.get("vendor-dir")) {
            None => "vendor",
            Some(Value::String(path)) if !path.is_empty() => path.as_str(),
            _ => return Err(invalid("config.vendor-dir 无效")),
        };
        let relative = format!(
            "{}/composer/installed.json",
            vendor
                .replace('\\', "/")
                .trim_start_matches("./")
                .trim_end_matches('/')
        );
        // 自定义 vendor 仅支持项目内的普通目录，不读取外部目录或链接。
        if let Some((installed, _)) = read_json(&project, &relative, 8 * 1024 * 1024)? {
            let all = packages(if installed.is_array() {
                Some(&installed)
            } else {
                installed.get("packages")
            })?;
            let dev_names = installed.get("dev-package-names").and_then(Value::as_array);
            if !include_dev && dev_names.is_none() {
                return Err(invalid(
                    "已安装依赖未记录开发依赖分组；请选择包含开发依赖，或提供 composer.lock",
                ));
            }
            if dev_names.is_some_and(|names| names.iter().any(|name| !name.is_string())) {
                return Err(invalid("开发依赖分组无效"));
            }
            let (dev, regular): (Vec<_>, Vec<_>) = all
                .into_iter()
                .partition(|p| dev_names.is_some_and(|names| names.contains(&p["name"])));
            lock["packages"] = json!(regular);
            lock["packages-dev"] = json!(dev);
            dev_incomplete =
                include_dev && installed.get("dev").and_then(Value::as_bool) != Some(true);
            source = "installed";
        } else {
            source = "manifest";
        }
    }
    let vendor = manifest
        .get("config")
        .and_then(|v| v.get("vendor-dir"))
        .and_then(Value::as_str)
        .unwrap_or("vendor");
    let autoload_present = crate::paths::checked_data_path(
        &project,
        &format!(
            "{}/autoload.php",
            vendor
                .replace('\\', "/")
                .trim_start_matches("./")
                .trim_end_matches('/')
        ),
    )
    .ok()
    .is_some_and(|p| p.is_file());
    let temp = tempfile::tempdir()?;
    let home = temp.path().join("home");
    std::fs::create_dir(&home)?;
    std::fs::write(temp.path().join("original.json"), original)?;
    // 固定根包元数据，禁止从项目 config、repositories、scripts 或全局 Composer 配置加载行为。
    std::fs::write(temp.path().join("composer.json"), serde_json::to_vec(&json!({
        "name":manifest.get("name").and_then(Value::as_str).unwrap_or("niceenv/platform-check"),
        "version":manifest.get("version").and_then(Value::as_str).unwrap_or("1.0.0"),
        "require":require,"require-dev":require_dev,"provide":provides,"replace":replaces,
        "config":{"allow-plugins":false,"disable-tls":true},"repositories":[{"packagist.org":false}]
    })).map_err(invalid)?)?;
    std::fs::write(
        temp.path().join("composer.lock"),
        serde_json::to_vec(&lock).map_err(invalid)?,
    )?;
    let mut command = php.command(temp.path());
    // managed ini 的扩展会真实加载；仅禁用可能执行用户 PHP 代码的启动项。
    command
        .args([
            "-d",
            "memory_limit=128M",
            "-d",
            "auto_prepend_file=",
            "-d",
            "auto_append_file=",
            "-d",
            "opcache.preload=",
            "-d",
            "display_errors=stderr",
            "-d",
            "log_errors=0",
            "-r",
            PROBE,
            "--",
        ])
        .arg(crate::paths::portable_path_text(&php.composer))
        .arg(if include_dev { "dev" } else { "no-dev" });
    for (key, _) in std::env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("COMPOSER_")
            || key == "COMPOSER"
        {
            command.env_remove(key);
        }
    }
    command
        .env("COMPOSER_HOME", &home)
        .env("COMPOSER_CACHE_DIR", temp.path().join("cache"))
        .env("COMPOSER_DISABLE_NETWORK", "1")
        .env("COMPOSER_NO_INTERACTION", "1")
        .env("COMPOSER_ALLOW_SUPERUSER", "1")
        .env("COMPOSER_MAX_PARALLEL_PROCESSES", "1");
    let (code, output, diagnostics) = capture(&mut command)?;
    // 诊断不联网，因此临时 Composer 配置允许无 OpenSSL 启动；仍由平台检查报告缺失的 ext-openssl。
    let diagnostics = diagnostics
        .lines()
        .filter(|line| {
            !matches!(
                line.trim(),
                "You are running Composer with SSL/TLS protection disabled."
                    | "Checking non-dev platform requirements using the lock file"
                    | "Checking platform requirements using the lock file"
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let requirements: Vec<PlatformRequirement> = serde_json::from_slice(&output).map_err(|e| {
        AppError::new(
            "PHP_PLATFORM_FAILED",
            "Composer 未能完成环境检查，请检查 PHP 和 Composer 配置",
        )
        .with_detail(format!("{diagnostics}\n{e}"))
    })?;
    let expected_code = requirements
        .iter()
        .map(|r| match r.status.as_str() {
            "success" => 0,
            "failed" => 1,
            "missing" => 2,
            _ => -1,
        })
        .max()
        .unwrap_or(0);
    if code != expected_code
        || requirements
            .iter()
            .any(|r| !["success", "failed", "missing"].contains(&r.status.as_str()))
    {
        return Err(AppError::new(
            "PHP_PLATFORM_FAILED",
            "环境检查结果不完整，请修复 PHP 或 Composer 后重试",
        )
        .with_detail(diagnostics));
    }
    #[derive(Deserialize)]
    struct Runtime {
        php: String,
        ini: String,
        fresh: Option<bool>,
    }
    let runtime: Runtime =
        serde_json::from_slice(&std::fs::read(temp.path().join("runtime.json"))?)
            .map_err(invalid)?;
    Ok(ProjectPlatformReport {
        project: crate::paths::portable_path_text(&project),
        php_version: runtime.php,
        ini: runtime.ini,
        source: source.into(),
        include_dev,
        dev_incomplete,
        lock_fresh: if source == "lock" {
            runtime.fresh
        } else {
            None
        },
        autoload_present,
        requirements,
        diagnostics: if diagnostics.trim().is_empty() {
            None
        } else {
            Some(diagnostics.trim().into())
        },
    })
}
