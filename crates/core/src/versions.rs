//! 远程版本目录：从上游 API 枚举套件的完整版本历史（含最新）。
//!
//! 设计要点：
//! - 清单只内置少量「常用版本」做离线兜底；其余版本按需从上游拉取
//! - 结果落 SQLite 缓存（默认 6 小时），避免 GitHub 60 次/时的匿名限流
//! - 拉取失败不报错：回退到缓存，再回退到清单内置版本（`online=false`）
//! - 远程版本与清单「模板条目」合并后可安装：安装流程与内置版本完全一致

use crate::error::{AppError, Result};
use crate::model::{PackageManifestEntry, RemoteVersion, VersionCatalog, VersionSource};
use crate::store::Store;
use futures_util::{stream, StreamExt};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, LazyLock};

static FETCH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);
// 同一个套件/平台的强制刷新可能同时来自套件页、设置页和安装器。
// 通过按缓存键串行化，避免并发重复请求上游并互相覆盖结果。
static FETCH_GATES: LazyLock<parking_lot::Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>> =
    LazyLock::new(|| parking_lot::Mutex::new(HashMap::new()));

/// 缓存有效期：6 小时（GitHub 匿名限流 60/h，多人/多次刷新也够用）
pub const CACHE_TTL_MS: i64 = 6 * 60 * 60 * 1000;

/// 内置版本源兜底表：清单里的 `versionSource` 优先；清单没声明时用这张表。
/// 这样即使清单被换成旧版（或远程清单尚未更新），版本枚举依然可用。
fn builtin_source(id: &str) -> Option<VersionSource> {
    let (kind, repo, tag_prefix, asset_match, entry_template) = match id {
        "caddy" => (
            "github",
            "caddyserver/caddy",
            "v",
            r"caddy_.*_windows_amd64\.zip$",
            "caddy.exe",
        ),
        "frankenphp" => (
            "github",
            "php/frankenphp",
            "v",
            r"frankenphp-windows-x86_64\.zip$",
            "frankenphp.exe",
        ),
        "mailpit" => (
            "github",
            "axllent/mailpit",
            "v",
            r"mailpit-windows-amd64\.zip$",
            "mailpit.exe",
        ),
        "meilisearch" => (
            "github",
            "meilisearch/meilisearch",
            "v",
            r"meilisearch-windows-amd64\.exe$",
            "meilisearch-windows-amd64.exe",
        ),
        "minio" => (
            "github",
            "minio/minio",
            "",
            r"minio\.windows-amd64\..*\.exe$",
            "minio.windows-amd64.{version}.exe",
        ),
        "rustfs" => (
            "github",
            "rustfs/rustfs",
            "",
            r"rustfs-windows-x86_64.*\.zip$",
            "rustfs.exe",
        ),
        "qdrant" => (
            "github",
            "qdrant/qdrant",
            "v",
            r"qdrant-x86_64-pc-windows-msvc\.zip$",
            "qdrant.exe",
        ),
        "memcached" => (
            "memcached",
            "nono303/memcached",
            "",
            "",
            "memcached-{version}/libevent-2.1/x64/memcached.exe",
        ),
        "etcd" => (
            "github",
            "etcd-io/etcd",
            "v",
            r"etcd-v?[\d.]+-windows-amd64\.zip$",
            "etcd-v{version}-windows-amd64/etcd.exe",
        ),
        "sftpgo" => (
            "github",
            "drakkan/sftpgo",
            "v",
            r"sftpgo_v[\d.]+_windows_portable\.zip$",
            "sftpgo.exe",
        ),
        "rnacos" => (
            "github",
            "nacos-group/r-nacos",
            "v",
            r"rnacos-x86_64-pc-windows-msvc.*\.zip$",
            "rnacos.exe",
        ),
        "temporal-cli" => (
            "github",
            "temporalio/cli",
            "v",
            r"temporal_cli_[\d.]+_windows_amd64\.zip$",
            "temporal.exe",
        ),
        "cloudflared" => (
            "github",
            "cloudflare/cloudflared",
            "",
            r"cloudflared-windows-amd64\.exe$",
            "cloudflared-windows-amd64.exe",
        ),
        "coredns" => (
            "github",
            "coredns/coredns",
            "v",
            r"coredns_[\d.]+_windows_amd64\.zip$",
            "coredns.exe",
        ),
        "zincsearch" => (
            "github",
            "zincsearch/zincsearch",
            "v",
            r"zincsearch_[\d.]+_windows_x86_64\.tar\.gz$",
            "zincsearch.exe",
        ),
        "rabbitmq" => (
            "github",
            "rabbitmq/rabbitmq-server",
            "v",
            r"rabbitmq-server-windows-[\d.]+\.zip$",
            "rabbitmq_server-{version}/sbin/rabbitmq-server.bat",
        ),
        "consul" => ("consul", "", "", "", "consul.exe"),
        "ollama" => (
            "github",
            "ollama/ollama",
            "v",
            r"ollama-windows-amd64\.zip$",
            "ollama.exe",
        ),
        "mihomo" => (
            "github",
            "MetaCubeX/mihomo",
            "v",
            r"^mihomo-windows-amd64-v[\d.]+\.zip$",
            "mihomo-windows-amd64.exe",
        ),
        "phpmyadmin" => ("phpmyadmin", "", "", "", "phpMyAdmin-{version}-all-languages/index.php"),
        "adminer" => (
            "github",
            "vrana/adminer",
            "v",
            r"adminer-[\d.]+\.php$",
            "adminer.php",
        ),
        "k6" => (
            "github",
            "grafana/k6",
            "v",
            r"k6-v[\d.]+-windows-amd64\.zip$",
            "k6-v{version}-windows-amd64/k6.exe",
        ),
        "bun" => (
            "github",
            "oven-sh/bun",
            "bun-v",
            r"^bun-windows-x64\.zip$",
            "bun-windows-x64/bun.exe",
        ),
        "deno" => (
            "github",
            "denoland/deno",
            "v",
            r"^deno-x86_64-pc-windows-msvc\.zip$",
            "deno.exe",
        ),
        "erlang" => (
            "github",
            "erlang/otp",
            "OTP-",
            r"^otp_win64_[\d.]+\.zip$",
            "bin/erl.exe",
        ),
        "roadrunner" => (
            "github",
            "roadrunner-server/roadrunner",
            "v",
            r"^roadrunner-[\d.]+-windows-amd64\.zip$",
            "roadrunner-{version}-windows-amd64/rr.exe",
        ),
        "ruby" => (
            "github",
            "oneclick/rubyinstaller2",
            "RubyInstaller-",
            r"^rubyinstaller-[\d.]+-\d+-x64\.7z$",
            "rubyinstaller-{version}-x64/bin/ruby.exe",
        ),
        "ruby-devkit" => (
            "github",
            "oneclick/rubyinstaller2",
            "RubyInstaller-",
            r"^rubyinstaller-devkit-[\d.]+-\d+-x64\.exe$",
            "{asset}",
        ),
        "temurin-jdk21" => (
            "github",
            "adoptium/temurin21-binaries",
            "jdk-",
            r"^OpenJDK21U-jdk_x64_windows_hotspot_.*\.zip$",
            "jdk-{version}/bin/java.exe",
        ),
        "redis" => (
            "github",
            "redis-windows/redis-windows",
            "",
            r"^Redis-[\d.]+-Windows-x64-msys2\.zip$",
            "Redis-{version}-Windows-x64-msys2/redis-server.exe",
        ),
        "strawberry-perl" => (
            "github",
            "StrawberryPerl/Perl-Dist-Strawberry",
            "",
            r"^strawberry-perl-[\d.]+-64bit-portable\.zip$",
            "perl/bin/perl.exe",
        ),
        "composer" => ("composer", "", "", "", "composer.phar"),
        "gradle" => ("gradle", "", "", "", "gradle-{version}/bin/gradle.bat"),
        "zig" => ("zig", "", "", "", ""),
        "dotnet-sdk8" => ("dotnet", "", "", "", "dotnet.exe"),
        "flutter" => ("flutter", "", "", "", "flutter/bin/flutter.bat"),
        "mongodb" => (
            "mongodb",
            "",
            "",
            "",
            "mongodb-win32-x86_64-windows-{version}/bin/mongod.exe",
        ),
        "mysql" => ("mysql", "", "", "", "mysql-{version}-winx64/bin/mysqld.exe"),
        "mariadb" => (
            "mariadb",
            "",
            "",
            "",
            "mariadb-{version}-winx64/bin/mysqld.exe",
        ),
        "postgresql" => ("postgresql", "", "", "", "pgsql/bin/pg_ctl.exe"),
        "apache" => ("apache", "", "", "", "Apache24/bin/httpd.exe"),
        "tomcat" => (
            "tomcat",
            "",
            "",
            "",
            "apache-tomcat-{version}/bin/startup.bat",
        ),
        "elasticsearch" => (
            "elasticsearch",
            "",
            "",
            "",
            "elasticsearch-{version}/bin/elasticsearch.bat",
        ),
        "neo4j" => (
            "neo4j",
            "",
            "",
            "",
            "neo4j-community-{version}/bin/neo4j.bat",
        ),
        "rust" => ("rustup", "", "", "", "rustup-init.exe"),
        // 官方索引/目录页
        "node" => ("nodejs", "", "", "", "node-v{version}-win-x64/node.exe"),
        "php" => ("php", "", "", "", "php-cgi.exe"),
        "go" => ("go", "", "", "", "go/bin/go.exe"),
        "nginx" => ("nginx", "", "", "", "nginx-{version}/nginx.exe"),
        "python" => ("python", "", "", "", "python.exe"),
        _ => return None,
    };
    Some(VersionSource {
        kind: kind.to_string(),
        repo: (!repo.is_empty()).then(|| repo.to_string()),
        asset_match: (!asset_match.is_empty()).then(|| asset_match.to_string()),
        tag_prefix: (!tag_prefix.is_empty()).then(|| tag_prefix.to_string()),
        url_template: None,
        entry_template: Some(entry_template.to_string()),
        checksum_url: None,
        version_filter: match id {
            "php" => Some(r"^(?:[7-9]|[1-9]\d+)\.".to_string()),
            "python" => Some(r"^3\.(?:9|[1-9]\d+)\.".to_string()),
            _ => None,
        },
        version_strip: None,
        max_versions: Some(
            if matches!(id, "php" | "node" | "python" | "go" | "nginx") {
                80
            } else {
                40
            },
        ),
        include_prerelease: Some(false),
    })
}

/// 解析某包的版本源：清单声明优先，缺失时回退到内置表
pub fn source_for(template: &PackageManifestEntry) -> Option<VersionSource> {
    if template.os.iter().any(|os| os == "macos") && !template.os.iter().any(|os| os == "windows") {
        // macOS 只使用该平台明确声明的规则，禁止回退到 Windows 的内置匹配器。
        return template
            .version_source
            .clone()
            .or_else(|| match template.id.as_str() {
                "composer" | "mongodb" | "mysql" => builtin_source(&template.id).map(|mut s| {
                    s.entry_template = None;
                    s
                }),
                _ => None,
            });
    }
    template
        .version_source
        .clone()
        .or_else(|| builtin_source(&template.id))
}

/// 拉取（或取缓存）某包的版本目录。
/// `force = true` 时忽略缓存（用户手动点「刷新版本列表」）。
pub async fn catalog(
    store: &Store,
    template: &PackageManifestEntry,
    force: bool,
) -> VersionCatalog {
    let Some(src) = source_for(template) else {
        // 未声明版本源：只能用清单内置版本
        return VersionCatalog {
            id: template.id.clone(),
            remote: vec![],
            online: false,
            cached_at: None,
            error: None,
        };
    };
    if src.kind == "static" {
        return VersionCatalog {
            id: template.id.clone(),
            remote: vec![],
            online: false,
            cached_at: None,
            error: None,
        };
    }

    // 源/平台/入口发生变化后不复用旧缓存，更不能跨平台使用下载地址。
    let signature = format!(
        "{src:?}:{:?}:{:?}:{}:{}",
        template.os, template.arch, template.entry, template.kind
    );
    // v5 废弃旧 macOS Intel 源误返回的 ARM64 下载记录，按目标架构重新枚举。
    let cache_revision = "v5";
    let cache_key = format!(
        "versionCatalog:{cache_revision}:{}:{:x}",
        template.id,
        Sha256::digest(signature)
    );
    if !force {
        if let Some(hit) = read_cache(store, &cache_key) {
            return hit;
        }
    }

    let requested_at = crate::services::now_ms();
    let gate = {
        let mut gates = FETCH_GATES.lock();
        gates
            .entry(cache_key.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone()
    };
    let _gate = gate.lock().await;
    // 排队期间其它调用可能已经填入缓存。强制刷新也复用本次等待期间
    // 刚写入的结果，但不会把请求开始前的旧缓存当成刷新成功。
    if let Some(hit) = if force {
        read_cache_any(store, &cache_key).filter(|cat| {
            cat.cached_at
                .is_some_and(|cached_at| cached_at >= requested_at)
        })
    } else {
        read_cache(store, &cache_key)
    } {
        return hit;
    }
    let _permit = FETCH_SLOTS.acquire().await;
    match fetch(store, &src, template).await {
        Ok(mut cat) => {
            cat.online = true;
            cat.cached_at = Some(crate::services::now_ms());
            write_cache(store, &cache_key, &cat);
            cat
        }
        Err(e) => {
            // 回退策略：先给过期缓存（比空列表有用），再给空列表 + 错误说明
            let mut fallback = read_cache_any(store, &cache_key).unwrap_or_default();
            fallback.id = template.id.clone();
            fallback.online = false;
            if fallback.remote.is_empty() {
                fallback.error = Some(format!("{}；当前只能用清单内置版本", e.message));
            } else {
                fallback.error = Some(format!("{}；显示上次缓存结果", e.message));
            }
            fallback
        }
    }
}

/* ================= 缓存 ================= */

fn read_cache(store: &Store, key: &str) -> Option<VersionCatalog> {
    let cat = read_cache_any(store, key)?;
    let age = crate::services::now_ms() - cat.cached_at.unwrap_or(0);
    (age >= 0 && age < CACHE_TTL_MS).then_some(cat)
}

fn read_cache_any(store: &Store, key: &str) -> Option<VersionCatalog> {
    let mut cat: VersionCatalog = serde_json::from_str(&store.get_setting(key)?).ok()?;
    cat.remote
        .sort_by(|a, b| cmp_version_desc(&a.version, &b.version));
    Some(cat)
}

fn write_cache(store: &Store, key: &str, cat: &VersionCatalog) {
    if let Ok(s) = serde_json::to_string(cat) {
        let _ = store.set_setting(key, &s);
    }
}

/// 清空全部版本目录缓存（「刷新」或远端清单更新后调用）
pub fn clear_cache(store: &Store) {
    for (k, _) in store.all_settings().unwrap_or_default() {
        if k.starts_with("versionCatalog:") {
            let _ = store.set_setting(&k, "");
        }
    }
}

/* ================= 各上游抓取 ================= */

async fn fetch(store: &Store, src: &VersionSource, template: &PackageManifestEntry) -> Result<VersionCatalog> {
    for pattern in [&src.asset_match, &src.version_filter, &src.version_strip]
        .into_iter()
        .flatten()
    {
        regex::Regex::new(pattern)
            .map_err(|e| AppError::new("BAD_VERSION_SOURCE", format!("版本源规则无效：{e}")))?;
    }
    let list = match src.kind.as_str() {
        "github" => fetch_github(src, template).await?,
        "nodejs" => fetch_nodejs(store, src, template).await?,
        "php" => fetch_php(src, template).await?,
        "go" => fetch_go(src, template).await?,
        "nginx" => fetch_nginx(src, template).await?,
        "python" => fetch_python(src, template).await?,
        _ => upstream::fetch(src, template).await?,
    };
    if list.is_empty() {
        return Err(AppError::new(
            "EMPTY_VERSION_CATALOG",
            "上游未返回适用于当前平台的版本，请稍后重试",
        ));
    }
    Ok(VersionCatalog {
        id: template.id.clone(),
        remote: list,
        online: true,
        cached_at: None,
        error: None,
    })
}

#[path = "version_upstreams.rs"]
mod upstream;

async fn get_text(client: &reqwest::Client, url: &str) -> Result<String> {
    client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| AppError::download(url, e.to_string()))?
        .text()
        .await
        .map_err(|e| AppError::download(url, e.to_string()))
}

async fn get_json(client: &reqwest::Client, url: &str) -> Result<serde_json::Value> {
    serde_json::from_str(&get_text(client, url).await?)
        .map_err(|e| AppError::internal("解析上游版本索引", e.to_string()))
}

fn http_with_proxy(no_proxy: bool) -> Result<reqwest::Client> {
    let mut builder = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent("NiceEnv/0.1 (+local dev env manager)");
    if no_proxy {
        builder = builder.no_proxy();
    }
    builder
        .build()
        .map_err(|e| AppError::internal("创建 HTTP 客户端", e.to_string()))
}

fn http() -> Result<reqwest::Client> {
    http_with_proxy(false)
}

fn http_direct() -> Result<reqwest::Client> {
    http_with_proxy(true)
}

fn github_is_rate_limited(response: &reqwest::Response) -> bool {
    response.status().as_u16() == 403
        || response.status().as_u16() == 429
        || response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.trim() == "0")
}

fn github_rate_limit_detail(response: &reqwest::Response) -> String {
    let remaining = response
        .headers()
        .get("x-ratelimit-remaining")
        .and_then(|value| value.to_str().ok())
        .unwrap_or("未知");
    let reset = response
        .headers()
        .get("x-ratelimit-reset")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<i64>().ok())
        .map(|timestamp| {
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|duration| duration.as_secs() as i64)
                .unwrap_or_default();
            let wait = (timestamp - now).max(0);
            if wait == 0 {
                "恢复时间已到".to_string()
            } else {
                format!("约 {} 分钟后恢复", (wait + 59) / 60)
            }
        })
        .unwrap_or_else(|| "恢复时间未知".to_string());
    format!("剩余请求 {remaining} 次，{reset}")
}

async fn github_send(client: &reqwest::Client, url: &str) -> reqwest::Result<reqwest::Response> {
    client
        .get(url)
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
}

/// GitHub Releases：一次请求拿到该 repo 最近 100 个 release（含 asset digest）
async fn fetch_github(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let repo = src.repo.as_deref().ok_or_else(|| {
        AppError::new("BAD_VERSION_SOURCE", "github 版本源缺少 repo（owner/name）")
    })?;
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
    let direct = http_direct().ok();
    let proxy = http()?;
    let direct_result = match direct.as_ref() {
        Some(client) => Some(github_send(client, &url).await),
        None => None,
    };
    let resp = match direct_result {
        Some(Ok(response)) if !github_is_rate_limited(&response) => response,
        Some(Ok(response)) => {
            let detail = github_rate_limit_detail(&response);
            match github_send(&proxy, &url).await {
                Ok(proxy_response) if !github_is_rate_limited(&proxy_response) => proxy_response,
                Ok(proxy_response) => {
                    return Err(AppError::new(
                        "GITHUB_RATE_LIMIT",
                        "GitHub 接口请求过于频繁，暂时无法读取版本列表",
                    )
                    .with_hint(format!(
                        "直连和系统代理出口都被 GitHub 限制（直连：{detail}；代理：{}）。稍后再试；已缓存或清单内置版本仍可安装。",
                        github_rate_limit_detail(&proxy_response)
                    )));
                }
                Err(proxy_error) => {
                    return Err(AppError::new(
                        "GITHUB_RATE_LIMIT",
                        "GitHub 接口请求过于频繁，暂时无法读取版本列表",
                    )
                    .with_hint(format!(
                        "直连出口已被 GitHub 限制（{detail}），系统代理也无法访问：{proxy_error}。稍后再试；已缓存或清单内置版本仍可安装。"
                    )));
                }
            }
        }
        Some(Err(direct_error)) => match github_send(&proxy, &url).await {
            Ok(proxy_response) => proxy_response,
            Err(proxy_error) => {
                return Err(AppError::download(
                    &url,
                    format!("直连失败：{direct_error}；系统代理失败：{proxy_error}"),
                ));
            }
        },
        None => github_send(&proxy, &url)
            .await
            .map_err(|e| AppError::download(&url, e.to_string()))?,
    };

    if !resp.status().is_success() {
        return Err(AppError::download(&url, format!("HTTP {}", resp.status())));
    }

    let releases: Vec<serde_json::Value> = resp
        .json()
        .await
        .map_err(|e| AppError::internal("解析 GitHub 响应", e.to_string()))?;

    let allow_pre = src.include_prerelease.unwrap_or(false);
    let asset_re = src
        .asset_match
        .as_ref()
        .map(|p| regex::Regex::new(p))
        .transpose()
        .map_err(|e| AppError::new("BAD_VERSION_SOURCE", format!("下载文件匹配规则无效：{e}")))?;
    let ver_filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());
    let strip = src.tag_prefix.as_deref().unwrap_or("");

    let mut out = Vec::new();
    for rel in releases {
        if rel["draft"].as_bool().unwrap_or(false) {
            continue;
        }
        let pre = rel["prerelease"].as_bool().unwrap_or(false);
        if pre && !allow_pre {
            continue;
        }
        let tag = rel["tag_name"].as_str().unwrap_or("");
        if tag.is_empty() {
            continue;
        }
        let mut version = tag.strip_prefix(strip).unwrap_or(tag).to_string();
        // 规范化（如 memcached 的 1.6.8_mingw_libressl → 1.6.8）
        if let Some(re) = src
            .version_strip
            .as_ref()
            .and_then(|p| regex::Regex::new(p).ok())
        {
            if let Some(c) = re.captures(&version) {
                if let Some(m) = c.get(1) {
                    version = m.as_str().to_string();
                }
            }
        }
        if ver_filter.as_ref().is_some_and(|re| !re.is_match(&version)) {
            continue;
        }

        let assets = rel["assets"].as_array().cloned().unwrap_or_default();
        let picked = match &asset_re {
            Some(re) => pick_github_asset(&assets, re, template),
            None => None,
        };
        let Some(asset) = picked else { continue };
        let Some(asset_url) = asset["browser_download_url"].as_str() else {
            continue;
        };
        let filename = asset["name"].as_str().unwrap_or("");
        // Strawberry Perl 使用 SP_54231_64bit 形式的 tag，实际版本在发行文件名中。
        if template.id == "strawberry-perl" {
            version = filename
                .trim_start_matches("strawberry-perl-")
                .trim_end_matches("-64bit-portable.zip")
                .to_string();
        }
        // digest 形如 "sha256:abcd..."；老 release 无此字段则留空（下载时跳过校验）
        let sha256 = asset["digest"]
            .as_str()
            .and_then(|d| d.strip_prefix("sha256:"))
            .map(|s| s.to_string());

        out.push(RemoteVersion {
            version: version.clone(),
            url: asset_url.to_string(),
            sha256,
            size_bytes: asset["size"].as_u64(),
            entry: render_template(src.entry_template.as_deref(), &version, template)
                .replace("{asset}", filename),
            kind: archive_kind(asset_url).to_string(),
            prerelease: pre,
            note: None,
            released_at: rel["published_at"].as_str().map(|s| s.to_string()),
        });
    }
    Ok(limit_and_sort(out, src))
}

/// 在同一 release 有多个匹配资产时按目标架构、校验信息和文件名稳定选择。
/// GitHub API 的返回顺序不是选择契约，不能直接取第一个。
fn pick_github_asset<'a>(
    assets: &'a [serde_json::Value],
    matcher: &regex::Regex,
    template: &PackageManifestEntry,
) -> Option<&'a serde_json::Value> {
    let mut matches: Vec<&serde_json::Value> = assets
        .iter()
        .filter(|asset| {
            let Some(name) = asset["name"].as_str() else { return false };
            matcher.is_match(name)
                && asset["browser_download_url"].as_str().is_some()
                && asset_name_matches_arch(name, template)
        })
        .collect();
    matches.sort_by(|a, b| {
        let a_digest = a["digest"].as_str().is_some();
        let b_digest = b["digest"].as_str().is_some();
        b_digest
            .cmp(&a_digest)
            .then_with(|| b["size"].as_u64().unwrap_or(0).cmp(&a["size"].as_u64().unwrap_or(0)))
            .then_with(|| a["name"].as_str().unwrap_or("").cmp(b["name"].as_str().unwrap_or("")))
    });
    matches.into_iter().next()
}

fn asset_name_matches_arch(name: &str, template: &PackageManifestEntry) -> bool {
    let name = name.to_ascii_lowercase();
    let arm = template.arch.iter().any(|arch| arch == "arm64");
    if arm {
        !["x86_64", "amd64", "x64", "i386", "i686", "386"]
            .iter()
            .any(|marker| name.contains(marker))
    } else {
        !["arm64", "aarch64", "armv7", "armhf"]
            .iter()
            .any(|marker| name.contains(marker))
    }
}

/// Node.js：官方 dist 索引；校验文件延迟到安装选中的版本时读取。
async fn fetch_nodejs(
    store: &Store,
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let client = http()?;
    let url = "https://nodejs.org/dist/index.json";
    let mac = template.os.iter().any(|os| os == "macos");
    let arch = if template.arch.iter().any(|a| a == "arm64") {
        "arm64"
    } else {
        "x64"
    };
    let platform = if mac {
        format!("darwin-{arch}")
    } else {
        format!("win-{arch}")
    };
    let extension = if mac { "tar.gz" } else { "zip" };
    let file_key = if mac {
        format!("osx-{arch}-tar")
    } else {
        format!("win-{arch}-zip")
    };
    let rows: Vec<serde_json::Value> = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| AppError::download(url, e.to_string()))?
        .json()
        .await
        .map_err(|e| AppError::internal("解析 Node 版本索引", e.to_string()))?;

    let ver_filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());

    let mut out = Vec::new();
    for row in &rows {
        let ver = row["version"].as_str().unwrap_or("");
        if ver.is_empty() {
            continue;
        }
        let bare = ver.trim_start_matches('v');
        if ver_filter.as_ref().is_some_and(|re| !re.is_match(bare)) {
            continue;
        }
        // 只保留带目标系统和架构构建的版本。
        let files = row["files"].as_array().cloned().unwrap_or_default();
        if !files.iter().any(|f| f.as_str() == Some(&file_key)) {
            continue;
        }
        let lts = row["lts"].as_str().map(|s| s.to_string());
        out.push(RemoteVersion {
            version: bare.to_string(),
            url: format!("https://nodejs.org/dist/{ver}/node-{ver}-{platform}.{extension}"),
            sha256: None, // 安装选定版本时读取官方校验值。
            size_bytes: None,
            entry: render_template(src.entry_template.as_deref(), bare, template),
            kind: if mac { "targz" } else { "archive" }.into(),
            prerelease: false,
            note: lts.map(|l| format!("{l} LTS")),
            released_at: row["date"].as_str().map(|s| s.to_string()),
        });
    }
    out = limit_and_sort(out, src);

    if !out.is_empty() { cache_node_lts_aliases(store, &rows)?; }

    // 校验值在选择安装版本时读取，避免版本列表多等 12 次网络请求。
    Ok(out)
}

// LTS 别名来自完整官方索引，独立于平台筛选、显示数量和普通版本目录的清理。
// 项目读取只查本地快照；无法联网刷新时保留之前已确认的别名。
const NODE_LTS_KEY: &str = "nodeLtsAliases:v1";

fn valid_node_release(version: &str) -> bool {
    let parts: Vec<_> = version.split('.').collect();
    parts.len() == 3 && parts.iter().all(|part| !part.is_empty()
        && part.bytes().all(|c| c.is_ascii_digit()) && !(part.len() > 1 && part.starts_with('0'))
        && part.parse::<u64>().is_ok())
}

pub(crate) fn cache_node_lts_aliases(store: &Store, rows: &[serde_json::Value]) -> Result<()> {
    let mut aliases = std::collections::BTreeMap::<String, String>::new();
    for row in rows {
        let Some(name) = row["lts"].as_str().filter(|name| !name.is_empty() && name.len() <= 64
            && name.bytes().all(|c| c.is_ascii_alphabetic() || c == b'-')) else { continue; };
        let Some(version) = row["version"].as_str().and_then(|v| v.strip_prefix('v')).filter(|v| valid_node_release(v)) else { continue; };
        for alias in ["*".to_string(), name.to_ascii_lowercase()] {
            if aliases.get(&alias).is_none_or(|previous| cmp_version_desc(version, previous).is_lt()) {
                aliases.insert(alias, version.into());
            }
        }
    }
    if aliases.is_empty() { return Err(AppError::new("NODE_LTS_CATALOG_INVALID", "Node.js 官方索引缺少有效 LTS 信息，已保留上次结果")); }
    let content = serde_json::to_string(&aliases).map_err(|e| AppError::internal("保存 Node.js LTS 信息", e.to_string()))?;
    // 与项目保存、卸载和终端启动串行，避免快照核对后又切换别名。
    let _guard = crate::sites::SITE_CHANGES.lock();
    store.set_setting(NODE_LTS_KEY, &content)
}

pub(crate) fn node_lts_version(store: &Store, alias: &str) -> Result<String> {
    let missing = || AppError::new("NODE_LTS_CATALOG_MISSING", "尚无可用的 Node.js LTS 信息，请点击刷新 Node.js 版本信息后重试");
    let content = store.get_setting_checked(NODE_LTS_KEY)?.ok_or_else(missing)?;
    let aliases: std::collections::BTreeMap<String, String> = serde_json::from_str(&content).map_err(|_| missing())?;
    aliases.get(&alias.to_ascii_lowercase()).filter(|version| valid_node_release(version)).cloned()
        .ok_or_else(|| AppError::new("NODE_LTS_ALIAS_UNKNOWN", format!("上次获取的官方索引中没有 lts/{alias}，请刷新 Node.js 版本信息或检查代号")))
}

/// 按实际下载文件名读取 Node 的官方校验值，支持 Windows 与 macOS。
pub async fn node_sha256(version: &str, download_url: &str) -> Result<String> {
    let client = http()?;
    let url = format!("https://nodejs.org/dist/v{version}/SHASUMS256.txt");
    let text = client
        .get(&url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| AppError::download(&url, e.to_string()))?
        .text()
        .await
        .map_err(|e| AppError::internal("读取 Node 校验文件", e.to_string()))?;
    let needle = download_url.rsplit('/').next().unwrap_or("");
    text.lines()
        .find_map(|line| {
            let mut it = line.split_whitespace();
            let hash = it.next()?;
            let name = it.next()?.trim_start_matches('*');
            (name == needle && hash.len() == 64 && hash.bytes().all(|b| b.is_ascii_hexdigit()))
                .then(|| hash.to_string())
        })
        .ok_or_else(|| AppError::new("CHECKSUM_NOT_FOUND", format!("校验文件里没有 {needle}")))
}

/// PHP（Windows 官方 builds）：releases + archives 两个目录页
async fn fetch_php(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let client = http()?;
    let ver_filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());
    // 匹配 php-8.3.33-Win32-vs16-x64.zip（TS 版），排除 nt/nts 变体
    let file_re = regex::Regex::new(r"php-(\d+\.\d+\.\d+)-Win32-(vs\d+|vc\d+)-x64\.zip").unwrap();

    let mut out = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for base in [
        "https://downloads.php.net/~windows/releases/",
        "https://downloads.php.net/~windows/releases/archives/",
    ] {
        let html = match get_text(&client, base).await {
            Ok(html) => html,
            Err(_) => continue,
        };
        for cap in file_re.captures_iter(&html) {
            let ver = cap[1].to_string();
            // 同一版本在 releases/archives 都可能出现，去重
            if !seen.insert(ver.clone()) {
                continue;
            }
            if ver_filter.as_ref().is_some_and(|re| !re.is_match(&ver)) {
                continue;
            }
            out.push(RemoteVersion {
                version: ver.clone(),
                url: format!("{base}{}", &cap[0]),
                sha256: None, // 目录页不含哈希；发行清单的已验证条目保留官方 SHA256。
                size_bytes: None,
                entry: render_template(src.entry_template.as_deref(), &ver, template),
                kind: "archive".into(),
                prerelease: false,
                note: None,
                released_at: None,
            });
        }
    }
    Ok(limit_and_sort(out, src))
}

/// Go：官方 dl 索引含 sha256，最省事
async fn fetch_go(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let client = http()?;
    let os = if template.os.iter().any(|os| os == "macos") {
        "darwin"
    } else {
        "windows"
    };
    let arch = if template.arch.iter().any(|a| a == "arm64") {
        "arm64"
    } else {
        "amd64"
    };
    let url = "https://go.dev/dl/?mode=json&include=all";
    let rows: Vec<serde_json::Value> = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| AppError::download(url, e.to_string()))?
        .json()
        .await
        .map_err(|e| AppError::internal("解析 Go 版本索引", e.to_string()))?;

    let ver_filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());
    let allow_pre = src.include_prerelease.unwrap_or(false);

    let mut out = Vec::new();
    for row in rows {
        let ver = row["version"].as_str().unwrap_or(""); // go1.27.1
        let Some(bare) = ver.strip_prefix("go") else {
            continue;
        };
        if ver_filter.as_ref().is_some_and(|re| !re.is_match(bare)) {
            continue;
        }
        let pre = !row["stable"].as_bool().unwrap_or(true);
        if pre && !allow_pre {
            continue;
        }
        let files = row["files"].as_array().cloned().unwrap_or_default();
        // 选择目标系统与架构对应的压缩包。
        let Some(f) = files
            .iter()
            .find(|f| f["os"] == os && f["arch"] == arch && f["kind"] == "archive")
        else {
            continue;
        };
        let Some(fname) = f["filename"].as_str() else {
            continue;
        };
        out.push(RemoteVersion {
            version: bare.to_string(),
            url: format!("https://go.dev/dl/{fname}"),
            sha256: f["sha256"].as_str().map(|s| s.to_string()),
            size_bytes: f["size"].as_u64(),
            entry: render_template(src.entry_template.as_deref(), bare, template),
            kind: archive_kind(fname).into(),
            prerelease: pre,
            note: None,
            released_at: None,
        });
    }
    Ok(limit_and_sort(out, src))
}

/// nginx：下载目录列出全部历史版本（255+）。
/// 目录不可用时回退到下载页，至少保留仍在发布的分支。
async fn fetch_nginx(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let client = http()?;
    let url = "https://nginx.org/download/";
    let html = match get_text(&client, url).await {
        Ok(html) if html.contains(".zip") => html,
        _ => get_text(&client, "https://nginx.org/en/download.html").await?,
    };

    let re = regex::Regex::new(r"nginx-(\d+\.\d+\.\d+)\.zip").unwrap();
    let ver_filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(&html) {
        let ver = cap[1].to_string();
        if !seen.insert(ver.clone()) {
            continue;
        }
        if ver_filter.as_ref().is_some_and(|re| !re.is_match(&ver)) {
            continue;
        }
        out.push(RemoteVersion {
            version: ver.clone(),
            url: format!("https://nginx.org/download/nginx-{ver}.zip"),
            sha256: None, // nginx 官网不提供校验值；下载后仅校验大小
            size_bytes: None,
            entry: render_template(src.entry_template.as_deref(), &ver, template),
            kind: "archive".into(),
            prerelease: false,
            note: Some(
                if ver
                    .split('.')
                    .nth(1)
                    .and_then(|n| n.parse::<u32>().ok())
                    .is_some_and(|n| n % 2 == 1)
                {
                    "Mainline"
                } else {
                    "Stable"
                }
                .into(),
            ),
            released_at: None,
        });
    }
    Ok(limit_and_sort(out, src))
}

/// Python：ftp 目录列出所有 3.x（含嵌入式 amd64 包）
async fn fetch_python(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let client = http()?;
    let url = "https://www.python.org/ftp/python/";
    let html = get_text(&client, url).await?;

    let re = regex::Regex::new(r">(3\.\d+\.\d+)/<").unwrap();
    let ver_filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());

    let mut seen = std::collections::HashSet::new();
    let mut out = Vec::new();
    for cap in re.captures_iter(&html) {
        let ver = cap[1].to_string();
        if !seen.insert(ver.clone()) {
            continue;
        }
        if ver_filter.as_ref().is_some_and(|re| !re.is_match(&ver)) {
            continue;
        }
        out.push(RemoteVersion {
            version: ver.clone(),
            url: format!("https://www.python.org/ftp/python/{ver}/python-{ver}-embed-amd64.zip"),
            sha256: None,
            size_bytes: None,
            entry: render_template(src.entry_template.as_deref(), &ver, template),
            kind: "archive".into(),
            prerelease: false,
            note: None,
            released_at: None,
        });
    }
    // FTP 目录还包括只有源码的安全维护版，以及尚未发布正式安装包的版本。
    // 必须确认目录里真实存在 Windows embed 包，不能根据版本号拼出不存在的 URL。
    let candidates = limit_and_sort(out, src);
    let verified = stream::iter(candidates)
        .map(|item| {
            let client = &client;
            async move {
                let directory = item.url.rsplit_once('/')?.0;
                let file = item.url.rsplit('/').next()?;
                let html = get_text(client, &format!("{directory}/")).await.ok()?;
                html.contains(&format!("\"{file}\"")).then_some(item)
            }
        })
        .buffer_unordered(6)
        .filter_map(|item| async move { item })
        .collect()
        .await;
    Ok(limit_and_sort(verified, src))
}

/* ================= 工具 ================= */

/// 入口路径模板：{version} 占位；未声明时沿用清单模板条目的 entry
fn render_template(tpl: Option<&str>, version: &str, template: &PackageManifestEntry) -> String {
    match tpl {
        Some(t) => t.replace("{version}", version),
        None => template
            .entry
            .replace(&template.version, version)
            .replace("{version}", version),
    }
}

fn archive_kind(url: &str) -> &'static str {
    // 只看 URL path，忽略查询参数和大小写；否则签名下载地址会被误判为 binary。
    let path = reqwest::Url::parse(url)
        .map(|parsed| parsed.path().to_ascii_lowercase())
        .unwrap_or_else(|_| url.to_ascii_lowercase());
    if path.ends_with(".7z") {
        "sevenzip"
    } else if path.ends_with(".tar.gz") || path.ends_with(".tgz") || path.ends_with(".gz") {
        "targz"
    } else if path.ends_with(".zip") {
        "archive"
    } else {
        "binary"
    }
}

/// 版本号降序（数值比较，避免 1.9 > 1.10 的字符串陷阱）+ 去重 + 截断
fn limit_and_sort(mut list: Vec<RemoteVersion>, src: &VersionSource) -> Vec<RemoteVersion> {
    let filter = src
        .version_filter
        .as_ref()
        .and_then(|p| regex::Regex::new(p).ok());
    list.retain(|v| {
        !v.version.is_empty()
            && (src.include_prerelease.unwrap_or(false)
                || !v.prerelease && !is_prerelease(&v.version))
            && filter.as_ref().is_none_or(|re| re.is_match(&v.version))
    });
    // 同版本的双胞 tag（如 memcached 的 1.6.8_mingw_libressl 与 1.6.8_mingw
    // 都规范化到 1.6.8）排序时让带 sha256 的那条胜出
    list.sort_by(|a, b| {
        cmp_version_desc(&a.version, &b.version)
            .then_with(|| b.sha256.is_some().cmp(&a.sha256.is_some()))
    });
    let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
    list.retain(|v| seen.insert(v.version.clone()));
    let max = src.max_versions.unwrap_or(60);
    list.truncate(max);
    list
}

/// 版本号降序比较：按数字段逐级比较，数字段相同则退化到字符串。
///
/// 两个必须处理的实际形态（清单里都真实存在）：
/// - `v1.19.1` 这类带前缀的 tag（qdrant/etcd 用）：前缀不是段，忽略掉，
///   否则 `v1.19.1` 会解析成 `[0,19,1]` 而排在 `1.19.0` 之下。
/// - `1.10.0-rc1` 这类预发布：同数字段时正式版应排在预发布之前。
pub fn cmp_version_desc(a: &str, b: &str) -> std::cmp::Ordering {
    let pa = version_parts(a);
    let pb = version_parts(b);
    for i in 0..pa.len().max(pb.len()) {
        let x = pa.get(i).copied().unwrap_or(0);
        let y = pb.get(i).copied().unwrap_or(0);
        if x != y {
            return y.cmp(&x);
        }
    }
    // 数字段完全相等：正式版优先于预发布版（降序 → 正式版排前面）
    match (is_prerelease(a), is_prerelease(b)) {
        (false, true) => std::cmp::Ordering::Less,
        (true, false) => std::cmp::Ordering::Greater,
        _ => natural_version_cmp(b.trim_start_matches(['v', 'V']), a.trim_start_matches(['v', 'V'])),
    }
}

/// 预发布判定：出现 rc / beta / alpha / dev / preview / snapshot 等标记
pub(crate) fn is_prerelease(v: &str) -> bool {
    prerelease_start(v).is_some()
}

fn prerelease_start(v: &str) -> Option<usize> {
    let low = v.split('+').next().unwrap_or(v).to_ascii_lowercase();
    [
        "rc", "beta", "alpha", "dev", "preview", "snapshot", "nightly",
    ]
    .iter()
    .filter_map(|m| low.find(m))
    .min()
}

// 同一主版本下按数字比较构建号、预发布序号（+10 > +9、rc.10 > rc.9）。
// 不转浮点数，避免长数字丢失精度；前后端使用相同的自然排序规则。
fn natural_version_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    static TOKENS: std::sync::LazyLock<regex::Regex> = std::sync::LazyLock::new(|| regex::Regex::new(r"[0-9]+|[^0-9]+").unwrap());
    let a = a.to_ascii_lowercase();
    let b = b.to_ascii_lowercase();
    let mut aa = TOKENS.find_iter(&a);
    let mut bb = TOKENS.find_iter(&b);
    loop {
        let (x, y) = match (aa.next(), bb.next()) {
            (Some(x), Some(y)) => (x.as_str(), y.as_str()),
            (x, y) => return x.is_some().cmp(&y.is_some()),
        };
        let order = if x.as_bytes()[0].is_ascii_digit() && y.as_bytes()[0].is_ascii_digit() {
            let x = x.trim_start_matches('0');
            let y = y.trim_start_matches('0');
            x.len().cmp(&y.len()).then_with(|| x.cmp(y))
        } else { x.cmp(y) };
        if !order.is_eq() { return order; }
    }
}

fn version_parts(v: &str) -> Vec<u64> {
    // 去掉 v/V 版本前缀（tag 常见写法 v1.19.1），否则首段会解析成 0
    let v = v
        .trim_start_matches(['v', 'V'])
        .split('+')
        .next()
        .unwrap_or(v);
    // rc.10 等后缀不属于主版本，不能使预发布排到同号正式版之前。
    let v = &v[..prerelease_start(v).unwrap_or(v.len())];
    v.split(['.', '-', '_', '+'])
        .map(|s| {
            // 保留日期版本和发行包编号中的数字段。
            let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn archive_kind_uses_url_path_and_keeps_7z_separate_from_tar() {
        assert_eq!(archive_kind("https://example.test/pkg.7z?download=1"), "sevenzip");
        assert_eq!(archive_kind("https://example.test/pkg.TAR.GZ?sig=abc"), "targz");
        assert_eq!(archive_kind("https://example.test/pkg.zip?sig=abc"), "archive");
    }

    #[test]
    fn github_asset_selection_is_stable_and_respects_architecture() {
        let mut template: PackageManifestEntry = serde_json::from_value(serde_json::json!({
            "id": "demo", "version": "1.0.0", "category": "tool", "displayName": "Demo",
            "description": "", "os": ["macos"], "arch": ["arm64"], "kind": "archive",
            "url": "https://example.test/demo.zip", "sizeBytes": 1, "entry": "demo"
        })).unwrap();
        let matcher = regex::Regex::new(r"^demo-.*\.zip$").unwrap();
        let first = serde_json::json!({"name":"demo-1.2.0-x64.zip","browser_download_url":"https://example.test/x64.zip","size":90,"digest":"sha256:bad"});
        let second = serde_json::json!({"name":"demo-1.2.0-arm64.zip","browser_download_url":"https://example.test/arm64.zip","size":10,"digest":"sha256:good"});
        let mut assets = vec![first, second];
        assert_eq!(pick_github_asset(&assets, &matcher, &template).unwrap()["name"], "demo-1.2.0-arm64.zip");
        assets.reverse();
        assert_eq!(pick_github_asset(&assets, &matcher, &template).unwrap()["name"], "demo-1.2.0-arm64.zip");
        template.arch = vec!["x64".into()];
        assert_eq!(pick_github_asset(&assets, &matcher, &template).unwrap()["name"], "demo-1.2.0-x64.zip");
    }

    #[test]
    fn apache_catalog_uses_latest_build_and_checksum_for_the_exact_archive() {
        let installer = crate::install::Installer { manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json")).unwrap() };
        let template = installer.template_for("apache").unwrap();
        let source = source_for(&template).unwrap();
        let html = r#"
            <a href="/download/VS18/binaries/httpd-2.4.68-260827-Win64-VS18.zip">old</a>
            <a href='https://www.apachelounge.com/download/VS18/binaries/httpd-2.4.68-260920-Win64-VS18.zip'>current</a>
            <a href="/download/VS18/binaries/httpd-2.4.68-260920-Win64-VS18.zip.txt">checksums</a>
            <a href="/download/VS18/binaries/httpd-2.4.68-260920-Win32-VS18.zip">32 bit</a>
            <a href="https://example.org/download/VS18/binaries/httpd-9.9.9-260920-Win64-VS18.zip">other origin</a>
            <a href="/download/VS17/binaries/httpd-9.9.9-260920-Win64-VS18.zip">mismatched compiler</a>
            <a href="/download/VS17/binaries/httpd-2.4.66-251206-Win64-VS17.zip">archive</a>
        "#;
        let releases = upstream::apache_releases(html, &source, &template);
        assert_eq!(releases.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["2.4.68", "2.4.66"]);
        let filename = "httpd-2.4.68-260920-Win64-VS18.zip";
        assert!(releases[0].url.ends_with(filename));
        let hash = "F6DCF17D08AA32721AE418CD818C157E4C521C9E889B758646FB64287F1D56E3";
        let checksums = format!("SHA1-Checksum for: {filename}:\r\n{}\r\n\r\nSHA256-Checksum for: {filename}:\r\n{hash}\r\n\r\nSHA512-Checksum for: {filename}:\r\n{}", "0".repeat(40), "0".repeat(128));
        assert_eq!(upstream::apache_checksum(&checksums, filename).unwrap(), hash.to_ascii_lowercase());
        for bad in [String::from("<html>file not found</html>"), checksums.replace("SHA256", "SHA224"),
            checksums.replace(filename, "httpd-2.4.68-260827-Win64-VS18.zip"), checksums.replace(hash, "not a hash"),
            checksums.replace(hash, &"a".repeat(65)), format!("{checksums}\n{checksums}")] {
            assert_eq!(upstream::apache_checksum(&bad, filename).unwrap_err().code, "APACHE_CHECKSUM_INVALID");
        }
    }

    #[tokio::test]
    #[ignore = "reads the official Node.js index; no package downloads or services"]
    async fn official_node_lts_catalog_refresh_persists_aliases_beyond_display_limit() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state.db")).unwrap();
        let mut template = crate::install::Installer::bundled().template_for("node").unwrap();
        let mut source = source_for(&template).unwrap(); source.max_versions = Some(1);
        template.version_source = Some(source);
        let catalog = catalog(&store, &template, true).await;
        assert!(catalog.online, "{:?}", catalog.error);
        assert_eq!(catalog.remote.len(), 1);
        let aliases: std::collections::BTreeMap<String, String> = serde_json::from_str(&store.get_setting_checked(NODE_LTS_KEY).unwrap().unwrap()).unwrap();
        assert!(aliases.len() >= 5);
        assert_eq!(node_lts_version(&store, "argon").unwrap(), "4.9.1");
        assert!(valid_node_release(&node_lts_version(&store, "*").unwrap()));
        // 独立核对 nvm 使用的官方 TSV 索引，避免只验证同一解析函数的结果。
        let tab = get_text(&http().unwrap(), "https://nodejs.org/dist/index.tab").await.unwrap();
        let columns: Vec<_> = tab.lines().next().unwrap().split('\t').collect();
        let lts_column = columns.iter().position(|name| *name == "lts").unwrap();
        let latest = tab.lines().skip(1).find_map(|line| {
            let cells: Vec<_> = line.split('\t').collect();
            (cells.get(lts_column).is_some_and(|lts| !lts.is_empty() && *lts != "-"))
                .then(|| cells[0].trim_start_matches('v').to_string())
        }).unwrap();
        assert_eq!(node_lts_version(&store, "*").unwrap(), latest);
    }

    #[test]
    fn node_lts_aliases_use_full_official_metadata_and_keep_last_valid_snapshot() {
        let temp = tempfile::tempdir().unwrap();
        let store = Store::open(temp.path().join("state.db")).unwrap();
        assert_eq!(node_lts_version(&store, "*").unwrap_err().code, "NODE_LTS_CATALOG_MISSING");
        let rows = serde_json::json!([
            {"version":"v22.9.0","lts":"Jod"}, {"version":"v22.10.0","lts":"Jod"},
            {"version":"v20.19.0","lts":"Iron"}, {"version":"v26.0.0","lts":false},
            {"version":"v30.0.0-rc.1","lts":"Future"}, {"version":"v32.0.0","lts":""},
            {"version":"v40.0.0","lts":"bad/name"}, {"version":"broken","lts":"Jod"}
        ]);
        cache_node_lts_aliases(&store, rows.as_array().unwrap()).unwrap();
        assert_eq!(node_lts_version(&store, "*").unwrap(), "22.10.0");
        assert_eq!(node_lts_version(&store, "JoD").unwrap(), "22.10.0");
        assert_eq!(node_lts_version(&store, "iron").unwrap(), "20.19.0");
        assert_eq!(node_lts_version(&store, "unknown").unwrap_err().code, "NODE_LTS_ALIAS_UNKNOWN");
        assert_eq!(cache_node_lts_aliases(&store, &[]).unwrap_err().code, "NODE_LTS_CATALOG_INVALID");
        clear_cache(&store);
        drop(store);
        let reopened = Store::open(temp.path().join("state.db")).unwrap();
        assert_eq!(node_lts_version(&reopened, "*").unwrap(), "22.10.0");
        reopened.set_setting(NODE_LTS_KEY, "broken").unwrap();
        assert_eq!(node_lts_version(&reopened, "*").unwrap_err().code, "NODE_LTS_CATALOG_MISSING");
    }

    fn sorted(list: &[&str]) -> Vec<String> {
        let mut v: Vec<String> = list.iter().map(|s| s.to_string()).collect();
        v.sort_by(|a, b| cmp_version_desc(a, b));
        v
    }

    #[test]
    fn version_order_is_numeric_not_lexicographic() {
        let v = sorted(&["1.9.0", "1.10.0", "1.2.0", "2.0.0", "1.10.0-rc1"]);
        // 2.0.0 最大；1.10.0 > 1.9.0（字符串比较会搞反）
        assert_eq!(v[0], "2.0.0");
        assert_eq!(v[1], "1.10.0");
        assert_eq!(v[2], "1.10.0-rc1");
        assert_eq!(v[3], "1.9.0");
        assert_eq!(v[4], "1.2.0");

        // Database Tools 官方索引不是按新旧排序，还包含 99.0.0 占位发行。
        let installer = crate::install::Installer {
            manifest: serde_json::from_str(include_str!("../../../manifest/packages.win.json"))
                .unwrap(),
        };
        let template = installer.template_for("mongodb-database-tools").unwrap();
        for (os, arch, upstream_arch) in [("windows", "x64", "x86_64"), ("macos", "arm64", "arm64"), ("macos", "x64", "x86_64")] {
            let mut template = template.clone();
            template.os = vec![os.into()]; template.arch = vec![arch.into()];
            let mut source = source_for(&template).unwrap();
            source.entry_template = Some(format!("mongodb-database-tools-{os}-{upstream_arch}-{{version}}/bin/mongodump{}", if os == "windows" { ".exe" } else { "" }));
            let row = |version: &str| {
                let files: Vec<_> = [("windows", "x86_64"), ("macos", "arm64"), ("macos", "x86_64"), ("ubuntu2404", "x86_64")].into_iter().map(|(platform, cpu)| serde_json::json!({
                    "name": platform, "arch": cpu,
                    "archive": { "url": format!("https://fastdl.mongodb.org/tools/db/mongodb-database-tools-{platform}-{cpu}-{version}.zip"), "sha256": "a".repeat(64) },
                    "package": { "url": "https://example.org/unwanted.msi" }
                })).collect();
                serde_json::json!({ "version": version, "downloads": files })
            };
            let mut broken = row("101.1.0");
            for file in broken["downloads"].as_array_mut().unwrap() { file["archive"]["sha256"] = serde_json::json!("bad hash"); }
            let mut development = row("101.2.0"); development["development_release"] = serde_json::json!(true);
            let mut missing_archive = row("101.3.0");
            for file in missing_archive["downloads"].as_array_mut().unwrap() { file.as_object_mut().unwrap().remove("archive"); }
            let data = serde_json::json!({ "versions": [row("99.0.0"), row("100.9.0"), broken, development, missing_archive, row("100.19.0"), row("100.19.0-rc1"), row("invalid"), row("100.19.0")] });
            let parsed = upstream::mongodb_tools_releases(&data, &source, &template);
            assert_eq!(parsed.iter().map(|r| r.version.as_str()).collect::<Vec<_>>(), ["100.19.0", "100.9.0"]);
            assert!(parsed.iter().all(|r| r.url.contains(&format!("-{os}-{upstream_arch}-")) && r.sha256.as_ref().unwrap().len() == 64));
            assert_eq!(parsed[0].entry, source.entry_template.as_ref().unwrap().replace("{version}", "100.19.0"));
            source.max_versions = Some(1);
            assert_eq!(upstream::mongodb_tools_releases(&data, &source, &template).len(), 1);
        }
    }

    /// 清单里 qdrant/etcd/mailpit 的版本串带 v 前缀，必须与无前缀版本可比较
    #[test]
    fn version_prefix_v_is_ignored() {
        let v = sorted(&["v1.19.1", "1.19.0", "v1.18.3"]);
        assert_eq!(
            v,
            vec!["v1.19.1", "1.19.0", "v1.18.3"],
            "v 前缀不能被当成第 0 段"
        );

        let v = sorted(&["v3.7.1", "v3.6.14", "v3.5.33"]);
        assert_eq!(v, vec!["v3.7.1", "v3.6.14", "v3.5.33"]);
    }

    /// 预发布必须排在对应正式版之后
    #[test]
    fn prerelease_sorts_below_release() {
        let v = sorted(&["2.0.0-beta1", "2.0.0", "2.0.0-rc2"]);
        assert_eq!(v[0], "2.0.0", "正式版应最大");
        for x in &v[1..] {
            assert!(is_prerelease(x), "{x} 应是预发布");
        }
        assert_eq!(sorted(&["1.2.3-rc.9", "1.2.3-rc.10", "1.2.3", "1.2.3-beta.2"]),
            ["1.2.3", "1.2.3-rc.10", "1.2.3-rc.9", "1.2.3-beta.2"]);
        assert_eq!(sorted(&["21.0.9+9", "21.0.9+10", "21.0.10+1"]),
            ["21.0.10+1", "21.0.9+10", "21.0.9+9"]);
        assert!(cmp_version_desc("v1.2.3", "1.2.3").is_eq());
        assert!(!is_prerelease("1.2.3+dev.build"));
        assert_eq!(sorted(&["4.0.7-9", "4.0.7-10"]), ["4.0.7-10", "4.0.7-9"]);
    }
}
