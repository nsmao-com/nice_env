//! 非 GitHub 套件的官方发行索引。与版本缓存、安装模板共用同一条管线。
use super::*;
use serde_json::Value;

fn rows(value: &Value) -> &[Value] {
    value.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn release(
    src: &VersionSource,
    template: &PackageManifestEntry,
    version: &str,
    url: &str,
) -> RemoteVersion {
    RemoteVersion {
        version: version.into(),
        url: url.into(),
        sha256: None,
        size_bytes: None,
        entry: render_template(src.entry_template.as_deref(), version, template),
        kind: archive_kind(url).into(),
        prerelease: is_prerelease(version),
        note: None,
        released_at: None,
    }
}

fn hash(value: &Value) -> Option<String> {
    value
        .as_str()
        .filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
        .map(str::to_owned)
}

fn mac(template: &PackageManifestEntry) -> bool {
    template.os.iter().any(|os| os == "macos")
}

pub(super) fn apache_releases(html: &str, src: &VersionSource, template: &PackageManifestEntry) -> Vec<RemoteVersion> {
    // 只读取真正的 Win64 安装包链接，不能把签名、校验文件或第三方域名中的路径当作下载。
    let links = regex::Regex::new(r#"(?i)\bhref\s*=\s*["']((?:https://www\.apachelounge\.com)?/download/VS(\d+)/binaries/httpd-(\d+\.\d+\.\d+)-(\d{6})-Win64-VS(\d+)\.zip)["']"#).unwrap();
    let mut releases = Vec::new();
    for cap in links.captures_iter(html) {
        if cap[2] != cap[5] { continue; }
        let url = if cap[1].starts_with('/') { format!("https://www.apachelounge.com{}", &cap[1]) } else { cap[1].to_string() };
        releases.push((cap[4].to_string(), cap[2].parse::<u32>().unwrap_or(0), release(src, template, &cap[3], &url)));
    }
    // 上游会以不同日期重新构建同一版本；去重时必须保留最新构建。
    releases.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| b.1.cmp(&a.1)));
    limit_and_sort(releases.into_iter().map(|(_, _, item)| item).collect(), src)
}

pub(super) fn apache_checksum(text: &str, filename: &str) -> Result<String> {
    let marker = format!("SHA256-Checksum for: {filename}:");
    let lines: Vec<_> = text.lines().map(str::trim).filter(|line| !line.is_empty()).collect();
    let mut matches = lines.windows(2).filter(|pair| pair[0] == marker);
    if let Some(pair) = matches.next() {
        if matches.next().is_none() && pair[1].len() == 64 && pair[1].bytes().all(|c| c.is_ascii_hexdigit()) {
            return Ok(pair[1].to_ascii_lowercase());
        }
    }
    Err(AppError::new("APACHE_CHECKSUM_INVALID", "Apache 官方校验文件缺失或与安装包不匹配")
        .with_hint("请刷新版本列表后重试；不会跳过 SHA256 校验"))
}

pub(super) fn mongodb_tools_releases(data: &Value, src: &VersionSource, template: &PackageManifestEntry) -> Vec<RemoteVersion> {
    let target = if mac(template) { "macos" } else { "windows" };
    let arch = if template.arch.iter().any(|arch| arch == "arm64") { "arm64" } else { "x86_64" };
    let stable = regex::Regex::new(r"^\d+\.\d+\.\d+$").expect("固定版本表达式合法");
    let mut out = Vec::new();
    for row in rows(&data["versions"]) {
        let Some(version) = row["version"].as_str().filter(|v| stable.is_match(v)
            && v.split('.').next().and_then(|n| n.parse::<u64>().ok()).is_some_and(|n| n >= 100)) else { continue; };
        if row["development_release"] == true { continue; }
        for file in rows(&row["downloads"]) {
            if file["name"] != target || file["arch"] != arch { continue; }
            let archive = &file["archive"];
            let expected_url = format!("https://fastdl.mongodb.org/tools/db/mongodb-database-tools-{target}-{arch}-{version}.zip");
            let Some(url) = archive["url"].as_str().filter(|url| *url == expected_url) else { continue; };
            // 官方提供 SHA256；缺失或损坏的元数据不能退化成无校验安装。
            let Some(sha256) = hash(&archive["sha256"]) else { continue; };
            let mut item = release(src, template, version, url);
            item.sha256 = Some(sha256);
            out.push(item);
        }
    }
    limit_and_sort(out, src)
}

async fn mysql_html(client: &reqwest::Client, url: &str) -> Result<String> {
    let response = client
        .get(url)
        .send()
        .await
        .map_err(|e| AppError::download(url, e.to_string()))?;
    if response.status().as_u16() != 403 {
        return response
            .error_for_status()
            .map_err(|e| AppError::download(url, e.to_string()))?
            .text()
            .await
            .map_err(|e| AppError::download(url, e.to_string()));
    }
    // 官网 CDN 会拒绝部分 HTTP 客户端。使用系统自带 curl 重试同一个公开页面；
    // 保留证书验证，参数逐项传入，不经 shell 拼接或执行上游内容。
    let url = url.to_string();
    tokio::task::spawn_blocking(move || {
        let output = platform::command("curl")
            .args([
                "--fail",
                "--silent",
                "--show-error",
                "--location",
                "--max-time",
                "30",
                &url,
            ])
            .output()
            .map_err(|e| AppError::io("读取 MySQL 官方版本目录", e))?;
        if !output.status.success() {
            return Err(AppError::new(
                "VERSION_SOURCE_UNAVAILABLE",
                "MySQL 官网暂时无法访问，请稍后刷新",
            ));
        }
        String::from_utf8(output.stdout)
            .map_err(|e| AppError::internal("读取 MySQL 版本页面", e.to_string()))
    })
    .await
    .map_err(|e| AppError::internal("读取 MySQL 版本页面", e.to_string()))?
}

pub(super) async fn fetch(
    src: &VersionSource,
    template: &PackageManifestEntry,
) -> Result<Vec<RemoteVersion>> {
    let client = http()?;
    let mut out = Vec::new();
    match src.kind.as_str() {
        "memcached" => {
            // 维护中的 Windows 构建发布在 Git tags 中，仓库没有 Release assets。
            // 下载固定 commit 的整包以保留同目录 Cygwin DLL，使用通用 SSE2 入口。
            let data = get_json(&client, "https://api.github.com/repos/nono303/memcached/tags?per_page=100").await?;
            let numeric = regex::Regex::new(r"^\d+\.\d+\.\d+$").unwrap();
            for row in rows(&data) {
                let (Some(version), Some(commit)) = (row["name"].as_str(), row["commit"]["sha"].as_str()) else { continue };
                if !numeric.is_match(version) || cmp_version_desc(version, "1.6.24").is_gt()
                    || commit.len() != 40 || !commit.bytes().all(|b| b.is_ascii_hexdigit()) { continue; }
                let mut item = release(src, template, version,
                    &format!("https://github.com/nono303/memcached/archive/{commit}.zip"));
                item.entry = format!("memcached-{commit}/libevent-2.1/x64/memcached.exe");
                item.note = Some("Windows 社区构建 · Cygwin / SSE2".into());
                out.push(item);
            }
        }
        "composer" => {
            let data = get_json(&client, "https://getcomposer.org/versions").await?;
            for row in rows(&data["2"]) {
                if let (Some(v), Some(path)) = (row["version"].as_str(), row["path"].as_str()) {
                    out.push(release(
                        src,
                        template,
                        v,
                        &format!("https://getcomposer.org{path}"),
                    ));
                }
            }
        }
        "consul" => {
            // HashiCorp 的 GitHub Release 不附带 Windows 包，发行索引才是下载源。
            let data =
                get_json(&client, "https://releases.hashicorp.com/consul/index.json").await?;
            for (version, row) in data["versions"].as_object().into_iter().flatten() {
                if version.contains('+') {
                    continue;
                } // 企业版需要独立许可
                for file in rows(&row["builds"]) {
                    if file["os"] == "windows" && file["arch"] == "amd64" {
                        if let Some(url) = file["url"].as_str() {
                            out.push(release(src, template, version, url));
                        }
                    }
                }
            }
        }
        "gradle" => {
            let data = get_json(&client, "https://services.gradle.org/versions/all").await?;
            for row in rows(&data) {
                if row["snapshot"] == true || row["nightly"] == true || row["broken"] == true {
                    continue;
                }
                if let (Some(v), Some(url)) = (row["version"].as_str(), row["downloadUrl"].as_str())
                {
                    let mut r = release(src, template, v, url);
                    r.prerelease |= row["rcFor"].as_str().is_some_and(|s| !s.is_empty())
                        || row["milestoneFor"].as_str().is_some_and(|s| !s.is_empty());
                    r.sha256 = hash(&row["checksum"]);
                    out.push(r);
                }
            }
        }
        "zig" => {
            let data = get_json(&client, "https://ziglang.org/download/index.json").await?;
            for (v, row) in data.as_object().into_iter().flatten() {
                if v == "master" {
                    continue;
                }
                let file = &row["x86_64-windows"];
                if let Some(url) = file["tarball"].as_str() {
                    let mut r = release(src, template, v, url);
                    // 0.14 及以前的目录为 zig-windows-x86_64，不能固定使用新版目录名。
                    let root = url
                        .rsplit('/')
                        .next()
                        .unwrap_or("")
                        .trim_end_matches(".zip");
                    r.entry = format!("{root}/zig.exe");
                    r.sha256 = hash(&file["shasum"]);
                    r.size_bytes = file["size"].as_str().and_then(|s| s.parse().ok());
                    out.push(r);
                }
            }
        }
        "dotnet" => {
            let data = get_json(
                &client,
                "https://builds.dotnet.microsoft.com/dotnet/release-metadata/8.0/releases.json",
            )
            .await?;
            for row in rows(&data["releases"]) {
                for sdk in rows(&row["sdks"])
                    .iter()
                    .chain(std::iter::once(&row["sdk"]))
                {
                    let Some(v) = sdk["version"].as_str() else {
                        continue;
                    };
                    for f in rows(&sdk["files"]) {
                        if f["rid"] == "win-x64" {
                            if let Some(url) = f["url"].as_str().filter(|u| u.ends_with(".zip")) {
                                // 官方 hash 为 SHA512，不能误填进 SHA256 字段。
                                out.push(release(src, template, v, url));
                            }
                        }
                    }
                }
            }
        }
        "flutter" => {
            let data = get_json(&client, "https://storage.googleapis.com/flutter_infra_release/releases/releases_windows.json").await?;
            for row in rows(&data["releases"]) {
                if row["channel"] != "stable"
                    || row["dart_sdk_arch"].as_str().is_some_and(|s| s != "x64")
                {
                    continue;
                }
                if let (Some(v), Some(path)) = (row["version"].as_str(), row["archive"].as_str()) {
                    let mut r = release(
                        src,
                        template,
                        v,
                        &format!(
                            "https://storage.googleapis.com/flutter_infra_release/releases/{path}"
                        ),
                    );
                    r.sha256 = hash(&row["sha256"]);
                    r.released_at = row["release_date"].as_str().map(str::to_owned);
                    out.push(r);
                }
            }
        }
        "mongodb-tools" => {
            let data = get_json(&client, "https://downloads.mongodb.org/tools/db/full.json").await?;
            out = mongodb_tools_releases(&data, src, template);
        }
        "mongodb" => {
            // current.json 覆盖仍发布的各个分支；full.json 超过 50 MB，不用于首屏查询。
            let data = get_json(&client, "https://downloads.mongodb.org/current.json").await?;
            for row in rows(&data["versions"]) {
                let Some(v) = row["version"].as_str() else {
                    continue;
                };
                if row["development_release"] == true {
                    continue;
                }
                for file in rows(&row["downloads"]) {
                    let target = if mac(template) { "macos" } else { "windows" };
                    let arch = if mac(template) { "arm64" } else { "x86_64" };
                    if file["target"] != target || file["arch"] != arch || file["edition"] != "base"
                    {
                        continue;
                    }
                    if let Some(url) = file["archive"]["url"].as_str() {
                        let mut r = release(src, template, v, url);
                        r.sha256 = hash(&file["archive"]["sha256"]);
                        out.push(r);
                    }
                }
            }
        }
        "mariadb" => {
            let data = get_json(&client, "https://downloads.mariadb.org/rest-api/mariadb/").await?;
            let branches: Vec<_> = rows(&data["major_releases"])
                .iter()
                .filter(|r| r["release_status"] == "Stable")
                .filter_map(|r| r["release_id"].as_str())
                .take(8)
                .collect();
            let results = stream::iter(branches)
                .map(|v| {
                    let client = &client;
                    async move {
                        get_json(
                            client,
                            &format!("https://downloads.mariadb.org/rest-api/mariadb/{v}/"),
                        )
                        .await
                    }
                })
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await;
            for data in results.into_iter().flatten() {
                for (v, row) in data["releases"].as_object().into_iter().flatten() {
                    for f in rows(&row["files"]) {
                        if !f["file_name"]
                            .as_str()
                            .is_some_and(|s| s.ends_with("-winx64.zip"))
                        {
                            continue;
                        }
                        if let Some(filename) = f["file_name"].as_str() {
                            let mut r = release(src, template, v,
                                &format!("https://archive.mariadb.org/mariadb-{v}/winx64-packages/{filename}"));
                            r.sha256 = hash(&f["checksum"]["sha256sum"]);
                            out.push(r);
                        }
                    }
                }
            }
        }
        "phpmyadmin" => {
            let data = get_json(&client, "https://www.phpmyadmin.net/home_page/version.json").await?;
            for item in rows(&data["releases"]) {
                let Some(version) = item["version"].as_str().filter(|v| regex::Regex::new(r"^5\.2\.\d+$").unwrap().is_match(v)) else { continue; };
                let url = format!("https://files.phpmyadmin.net/phpMyAdmin/{version}/phpMyAdmin-{version}-all-languages.zip");
                let mut row = release(src, template, version, &url);
                let checksum = get_text(&client, &format!("{url}.sha256")).await?;
                row.sha256 = checksum.split_whitespace().next().filter(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit())).map(str::to_string);
                if row.sha256.is_none() { return Err(AppError::new("BAD_CHECKSUM", "phpMyAdmin 官方校验和无效")); }
                out.push(row);
            }
        }
        "mysql" => {
            let os = if mac(template) { "33" } else { "3" };
            let first = mysql_html(
                &client,
                &format!("https://dev.mysql.com/downloads/mysql/?os={os}"),
            )
            .await?;
            let branches = regex::Regex::new(r#"<option value="(\d+\.\d+)""#).unwrap();
            let pages = stream::iter(
                branches
                    .captures_iter(&first)
                    .map(|c| c[1].to_string())
                    .take(5),
            )
            .map(|branch| {
                let client = &client;
                async move {
                    mysql_html(
                        client,
                        &format!("https://dev.mysql.com/downloads/mysql/{branch}.html?os={os}"),
                    )
                    .await
                }
            })
            .buffer_unordered(3)
            .collect::<Vec<_>>()
            .await;
            let pattern = if mac(template) {
                r"mysql-(\d+\.\d+\.\d+)-macos\d+-arm64\.tar\.gz"
            } else {
                r"mysql-(\d+\.\d+\.\d+)-winx64\.zip"
            };
            let re = regex::Regex::new(pattern).unwrap();
            for html in std::iter::once(first).chain(pages.into_iter().flatten()) {
                for cap in re.captures_iter(&html) {
                    let v = &cap[1];
                    let branch = v.rsplit_once('.').map(|(b, _)| b).unwrap_or(v);
                    let file = &cap[0];
                    let mut r = release(
                        src,
                        template,
                        v,
                        &format!("https://cdn.mysql.com/Downloads/MySQL-{branch}/{file}"),
                    );
                    if mac(template) {
                        r.entry = format!("{}/bin/mysqld", file.trim_end_matches(".tar.gz"));
                    }
                    out.push(r);
                }
            }
            let file = if mac(template) { "mysql-8.2.0-macos13-arm64.tar.gz" } else { "mysql-8.2.0-winx64.zip" };
            let mut archived = release(src, template, "8.2.0", &format!("https://cdn.mysql.com/archives/mysql-8.2/{file}"));
            archived.note = Some("Oracle 官方历史归档；8.2 为已结束支持的 Innovation 分支".into());
            if mac(template) { archived.entry = "mysql-8.2.0-macos13-arm64/bin/mysqld".into(); }
            out.push(archived);
        }
        "postgresql" => {
            let html = get_text(
                &client,
                "https://www.enterprisedb.com/download-postgresql-binaries",
            )
            .await?;
            let version = regex::Regex::new(r"Version\s*(?:<!-- -->)?(\d+(?:\.\d+)+)").unwrap();
            let link = regex::Regex::new(r#"href="(https://sbp\.enterprisedb\.com/getfile\.jsp\?fileid=\d+)"[^>]*><img alt="Windows x86-64""#).unwrap();
            for block in html.split("Binaries from installer").skip(1) {
                if let (Some(v), Some(url)) = (version.captures(block), link.captures(block)) {
                    let mut r = release(src, template, &v[1], &url[1]);
                    r.kind = "archive".into();
                    out.push(r);
                }
            }
        }
        "apache" => {
            let html = get_text(&client, "https://www.apachelounge.com/download/").await?;
            out = apache_releases(&html, src, template);
            for item in &mut out {
                let checksums = get_text(&client, &format!("{}.txt", item.url)).await?;
                item.sha256 = Some(apache_checksum(&checksums, item.url.rsplit('/').next().unwrap_or(""))?);
            }
        }
        "neo4j" => {
            let html = get_text(&client, "https://neo4j.com/deployment-center/").await?;
            let re = regex::Regex::new(r"(neo4j-community-(\d+(?:\.\d+)+)-windows\.zip)").unwrap();
            for cap in re.captures_iter(&html) {
                out.push(release(
                    src,
                    template,
                    &cap[2],
                    &format!("https://dist.neo4j.org/{}", &cap[1]),
                ));
            }
        }
        "tomcat" => {
            let root = "https://downloads.apache.org/tomcat/";
            let html = get_text(&client, root).await?;
            let branch_re = regex::Regex::new(r#"href="(tomcat-\d+/)""#).unwrap();
            let version_re = regex::Regex::new(r#"href="v(\d+\.\d+\.\d+)/""#).unwrap();
            for branch in branch_re.captures_iter(&html) {
                let page = get_text(&client, &format!("{root}{}", &branch[1])).await?;
                for v in version_re.captures_iter(&page) {
                    out.push(release(
                        src,
                        template,
                        &v[1],
                        &format!(
                            "{root}{}v{}/bin/apache-tomcat-{}-windows-x64.zip",
                            &branch[1], &v[1], &v[1]
                        ),
                    ));
                }
            }
        }
        "elasticsearch" => {
            let data = get_json(&client, "https://artifacts-api.elastic.co/v1/versions").await?;
            for v in rows(&data["versions"]).iter().filter_map(Value::as_str) {
                out.push(release(src, template, v, &format!("https://artifacts.elastic.co/downloads/elasticsearch/elasticsearch-{v}-windows-x86_64.zip")));
            }
            // 构建索引也收录尚未正式发布的版本，仅保留公开下载站实际提供的包。
            out = stream::iter(limit_and_sort(out, src))
                .map(|r| {
                    let client = &client;
                    async move {
                        let response = client
                            .get(&r.url)
                            .header("Range", "bytes=0-0")
                            .send()
                            .await
                            .ok()?;
                        response.status().is_success().then_some(r)
                    }
                })
                .buffer_unordered(4)
                .filter_map(|r| async move { r })
                .collect()
                .await;
        }
        "rustup" => {
            let text = get_text(
                &client,
                "https://static.rust-lang.org/rustup/release-stable.toml",
            )
            .await?;
            let re = regex::Regex::new(r#"(?m)^version\s*=\s*['"]([\d.]+)['"]"#).unwrap();
            if let Some(cap) = re.captures(&text) {
                let mut r = release(src, template, &cap[1], "https://static.rust-lang.org/rustup/dist/x86_64-pc-windows-msvc/rustup-init.exe");
                r.note = Some("rustup installer".into());
                out.push(r);
            }
        }
        other => {
            return Err(AppError::new(
                "UNKNOWN_VERSION_SOURCE",
                format!("未知的版本源类型 {other}"),
            ))
        }
    }
    Ok(limit_and_sort(out, src))
}
