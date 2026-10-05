//! 远程版本目录验收：真实访问上游 API，验证「枚举完整版本历史」可用。
//! 用法：cargo run -p nsb-core --bin check_versions [id...]
//! 不带参数时测一组代表性源（github / nodejs / php / go / nginx / python）。

use futures_util::{stream, StreamExt};
use std::path::PathBuf;

fn selected_ids(args: &[String], manifest_arg: Option<usize>) -> Vec<String> {
    args.iter()
        .enumerate()
        .filter(|(index, value)| {
            !value.starts_with("--") && !manifest_arg.is_some_and(|m| *index == m + 1)
        })
        .map(|(_, value)| value.clone())
        .collect()
}

fn only_options(args: &[String], manifest_arg: Option<usize>) -> bool {
    args.iter().enumerate().all(|(index, value)| {
        value.starts_with("--") || manifest_arg.is_some_and(|m| index == m + 1)
    })
}

async fn probe_download(client: &reqwest::Client, url: &str) -> Result<u16, String> {
    let response = client.get(url).header("Range", "bytes=0-64")
        .send().await.map_err(|error| error.to_string())?;
    if response.status().is_success() {
        Ok(response.status().as_u16())
    } else {
        Err(format!("HTTP {}", response.status()))
    }
}

#[tokio::main]
async fn main() {
    let base = PathBuf::from(".versions-home");
    std::fs::create_dir_all(&base).expect("创建 .versions-home");
    // 只读发行源和独立缓存，不初始化服务管理、进程回收或自动备份。
    let store = nsb_core::store::Store::open(base.join("versions.sqlite")).expect("版本缓存");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let manifest_arg = args.iter().position(|s| s == "--manifest");
    let installer = if let Some(index) = manifest_arg {
        let file = args.get(index + 1).expect("--manifest 后需要清单路径");
        let raw = std::fs::read_to_string(file).expect("读取清单");
        nsb_core::install::Installer {
            manifest: nsb_core::install::parse_manifest_str(&raw).expect("清单格式"),
        }
    } else {
        nsb_core::install::Installer::bundled()
    };
    let json = args.iter().any(|s| s == "--json");
    let selected = selected_ids(&args, manifest_arg);
    let force = args.iter().any(|s| s == "--force");
    let ids: Vec<String> = if !selected.is_empty() {
        selected
    } else if args.iter().any(|s| s == "--all") || json {
        let mut ids: Vec<_> = installer
            .manifest
            .packages
            .iter()
            .map(|p| p.id.clone())
            .collect();
        ids.sort();
        ids.dedup();
        ids
    } else if args.is_empty() || only_options(&args, manifest_arg) {
        // 覆盖每一种版本源类型
        [
            "php",
            "node",
            "go",
            "nginx",
            "caddy",
            "mailpit",
            "minio",
            "memcached",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect()
    } else {
        // 只有套件 ID 才会进入 selected；保留分支防止未来新增参数时静默把选项当 ID。
        Vec::new()
    };
    if json {
        let catalogs: Vec<_> = stream::iter(&ids)
            .map(|id| {
                let template = installer.template_for(id).expect("套件模板");
                let store = &store;
                async move { nsb_core::versions::catalog(store, &template, force).await }
            })
            .buffer_unordered(6)
            .collect()
            .await;
        // 保持 stdout 纯 JSON，同时让脚本/CI 能从退出码发现在线源失败。
        // 静态清单和未声明版本源的目录没有 error，不应被误报为失败。
        let failed = catalogs
            .iter()
            .filter(|catalog| catalog.error.is_some())
            .count();
        println!(
            "{}",
            serde_json::to_string_pretty(&catalogs).expect("目录 JSON")
        );
        if failed > 0 {
            std::process::exit(1);
        }
        return;
    }

    let mut pass = 0usize;
    let mut fail = 0usize;
    for id in &ids {
        let started = std::time::Instant::now();
        let Some(template) = installer.template_for(id) else {
            eprintln!("未知套件 {id}");
            fail += 1;
            continue;
        };
        match Ok::<_, nsb_core::error::AppError>(
            nsb_core::versions::catalog(&store, &template, force).await,
        ) {
            Ok(cat) => {
                let n = cat.remote.len();
                if n == 0 {
                    if nsb_core::versions::source_for(&template).is_some_and(|s| s.kind != "static")
                    {
                        println!(
                            "[FAIL] {id:<14} {}",
                            cat.error.as_deref().unwrap_or("上游未返回版本")
                        );
                        fail += 1;
                    } else {
                        println!("[SKIP] {id:<14} 无远程版本（未声明版本源）");
                    }
                    continue;
                }
                if !cat.online {
                    println!(
                        "[FAIL] {id:<14} {}",
                        cat.error.as_deref().unwrap_or("未取得在线目录")
                    );
                    fail += 1;
                    continue;
                }
                let with_sha = cat.remote.iter().filter(|v| v.sha256.is_some()).count();
                let newest = &cat.remote[0];
                let oldest = &cat.remote[n - 1];
                println!(
                    "[PASS] {id:<14} {n:>3} 个版本（{with_sha} 带 sha256）· 最新 {} → 最早 {} · {:.1}s",
                    newest.version, oldest.version, started.elapsed().as_secs_f32()
                );
                // 抽查：最新版本必须能被「模板 + 远程版本」合成为可安装条目
                let synthesized =
                    nsb_core::install::Installer::entry_from_remote(&template, newest);
                if synthesized.url != newest.url || synthesized.version != newest.version {
                    println!("       ✗ 合成条目不正确");
                    fail += 1;
                    continue;
                }
                if newest.entry.is_empty() {
                    println!("       ✗ 入口路径为空");
                    fail += 1;
                    continue;
                }
                // 与安装流程一样，逐个候选地址尝试系统代理和直连；不读取完整安装包。
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(25))
                    .build()
                    .unwrap();
                let direct = reqwest::Client::builder()
                    .no_proxy()
                    .timeout(std::time::Duration::from_secs(25))
                    .build()
                    .unwrap();
                let candidates = installer.candidate_urls_for(&synthesized, &store);
                let mut reachable = None;
                let mut errors = Vec::new();
                for candidate in candidates {
                    match probe_download(&client, &candidate).await {
                        Ok(status) => {
                            reachable = Some((candidate, status, "系统网络"));
                            break;
                        }
                        Err(proxy_error) => match probe_download(&direct, &candidate).await {
                            Ok(status) => {
                                reachable = Some((candidate, status, "直连回退"));
                                break;
                            }
                            Err(direct_error) => errors.push(format!(
                                "{candidate}：系统网络失败（{proxy_error}）；直连失败（{direct_error}）"
                            )),
                        },
                    }
                }
                if let Some((url, status, transport)) = reachable {
                    let source = if url == newest.url { "上游" } else { "镜像回退" };
                    println!("       ✓ 最新版本下载地址可达（{source}，{transport}，HTTP {status}）");
                    pass += 1;
                } else {
                    println!("       ✗ 最新版本候选地址均不可达：{}", errors.join("；"));
                    fail += 1;
                }
            }
            Err(e) => {
                println!("[FAIL] {id:<14} {}", e.message);
                if let Some(h) = &e.hint {
                    println!("       hint: {h}");
                }
                fail += 1;
            }
        }
    }

    // 缓存验证：同一 id 第二次调用应命中缓存（明显更快，且不再打网络）
    if let Some(first) = ids.first() {
        let t = std::time::Instant::now();
        let template = installer.template_for(first).expect("套件模板");
        let cached = Ok::<_, nsb_core::error::AppError>(
            nsb_core::versions::catalog(&store, &template, false).await,
        );
        let ms = t.elapsed().as_millis();
        match cached {
            Ok(c) if !c.remote.is_empty() && ms < 400 => {
                println!(
                    "[PASS] 缓存命中        {first} 二次拉取 {ms}ms（{} 个版本）",
                    c.remote.len()
                );
                pass += 1;
            }
            Ok(c) => {
                println!(
                    "[FAIL] 缓存命中        {first} 二次拉取 {ms}ms（{} 个版本，期望 <400ms）",
                    c.remote.len()
                );
                fail += 1;
            }
            Err(e) => {
                println!("[FAIL] 缓存命中        {}", e.message);
                fail += 1;
            }
        }
    }

    println!("\n==== check_versions: {pass} pass / {fail} fail ====");
    if fail > 0 {
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::{only_options, selected_ids};

    #[test]
    fn flags_and_manifest_path_are_not_treated_as_package_ids() {
        let args = vec!["--manifest".into(), "manifest.json".into(), "--force".into()];
        assert!(selected_ids(&args, Some(0)).is_empty());
        assert!(only_options(&args, Some(0)));
    }

    #[test]
    fn explicit_package_ids_are_preserved() {
        let args = vec!["--force".into(), "nginx".into(), "php".into()];
        assert_eq!(selected_ids(&args, None), vec!["nginx", "php"]);
        assert!(!only_options(&args, None));
    }
}
