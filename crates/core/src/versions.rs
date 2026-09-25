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

static FETCH_SLOTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(6);

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
            "github",
            "jefyt/memcached-windows",
            "",
            r"memcached-.*-win64.*\.zip$",
            "memcached-{version}-win64-mingw/bin/memcached.exe",
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
            "tporadowski/redis",
            "v",
            r"^Redis-x64-[\d.]+\.zip$",
            "redis-server.exe",
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
        // memcached 的 tag 形如 `1.6.8_mingw_libressl`；安装路径只认 `1.6.8`
        version_strip: if id == "memcached" {
            Some(r"^(\d+(?:\.\d+)+).*$".to_string())
        } else {
            None
        },
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
    let cache_key = format!(
        "versionCatalog:v2:{}:{:x}",
        template.id,
        Sha256::digest(signature)
    );
    if !force {
        if let Some(hit) = read_cache(store, &cache_key) {
            return hit;
        }
    }

    let _permit = FETCH_SLOTS.acquire().await;
    // 排队期间其它调用可能已经填入缓存。
    if !force {
        if let Some(hit) = read_cache(store, &cache_key) {
            return hit;
        }
    }
    match fetch(&src, template).await {
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

async fn fetch(src: &VersionSource, template: &PackageManifestEntry) -> Result<VersionCatalog> {
    for pattern in [&src.asset_match, &src.version_filter, &src.version_strip]
        .into_iter()
        .flatten()
    {
        regex::Regex::new(pattern)
            .map_err(|e| AppError::new("BAD_VERSION_SOURCE", format!("版本源规则无效：{e}")))?;
    }
    let list = match src.kind.as_str() {
        "github" => fetch_github(src, template).await?,
        "nodejs" => fetch_nodejs(src, template).await?,
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

fn http() -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(45))
        .connect_timeout(std::time::Duration::from_secs(15))
        .user_agent("NiceEnv/0.1 (+local dev env manager)")
        .build()
        .map_err(|e| AppError::internal("创建 HTTP 客户端", e.to_string()))
}

/// GitHub Releases：一次请求拿到该 repo 最近 100 个 release（含 asset digest）
async fn fetch_github(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let repo = src.repo.as_deref().ok_or_else(|| {
        AppError::new("BAD_VERSION_SOURCE", "github 版本源缺少 repo（owner/name）")
    })?;
    let client = http()?;
    let url = format!("https://api.github.com/repos/{repo}/releases?per_page=100");
    let resp = client
        .get(&url)
        .header("X-GitHub-Api-Version", "2022-11-28")
        .send()
        .await
        .map_err(|e| AppError::download(&url, e.to_string()))?;

    if resp.status().as_u16() == 403 {
        return Err(AppError::new(
            "GITHUB_RATE_LIMIT",
            "GitHub 接口请求过于频繁（匿名限流 60 次/小时）",
        )
        .with_hint("稍后再试；已内置常用版本可直接安装"));
    }
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
            Some(re) => assets
                .iter()
                .find(|a| a["name"].as_str().map(|n| re.is_match(n)).unwrap_or(false)),
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

/// Node.js：官方 dist 索引；校验文件延迟到安装选中的版本时读取。
async fn fetch_nodejs(
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
    for row in rows {
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

    // 校验值在选择安装版本时读取，避免版本列表多等 12 次网络请求。
    Ok(out)
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
    if url.ends_with(".tar.gz")
        || url.ends_with(".tgz")
        || url.ends_with(".gz")
        || url.ends_with(".7z")
    {
        "targz"
    } else if url.ends_with(".zip") {
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
        _ => b.cmp(a),
    }
}

/// 预发布判定：出现 rc / beta / alpha / dev / preview / snapshot 等标记
fn is_prerelease(v: &str) -> bool {
    let low = v.to_ascii_lowercase();
    [
        "rc", "beta", "alpha", "dev", "preview", "snapshot", "nightly",
    ]
    .iter()
    .any(|m| low.contains(m))
}

fn version_parts(v: &str) -> Vec<u64> {
    // 去掉 v/V 版本前缀（tag 常见写法 v1.19.1），否则首段会解析成 0
    let v = v
        .trim_start_matches(['v', 'V'])
        .split('+')
        .next()
        .unwrap_or(v);
    v.split(['.', '-', '_', '+'])
        .map(|s| {
            // 取段内前导数字：1.2.3rc1 → 1,2,3(,1)
            let digits: String = s.chars().take_while(|c| c.is_ascii_digit()).collect();
            digits.parse().unwrap_or(0)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

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
    }
}
