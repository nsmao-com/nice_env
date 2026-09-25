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
        "apache" | "neo4j" => {
            let (page, pattern, prefix) = match src.kind.as_str() {
                "apache" => (
                    "https://www.apachelounge.com/download/",
                    r"(/download/VS\d+/binaries/httpd-(\d+\.\d+\.\d+)-\d+-Win64-VS\d+\.zip)",
                    "https://www.apachelounge.com",
                ),
                _ => (
                    "https://neo4j.com/deployment-center/",
                    r"(neo4j-community-(\d+(?:\.\d+)+)-windows\.zip)",
                    "https://dist.neo4j.org/",
                ),
            };
            let html = get_text(&client, page).await?;
            let re = regex::Regex::new(pattern).unwrap();
            for cap in re.captures_iter(&html) {
                out.push(release(
                    src,
                    template,
                    &cap[2],
                    &format!("{prefix}{}", &cap[1]),
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
