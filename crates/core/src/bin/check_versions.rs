//! 远程版本目录验收：真实访问上游 API，验证「枚举完整版本历史」可用。
//! 用法：cargo run -p nsb-core --bin check_versions [id...]
//! 不带参数时测一组代表性源（github / nodejs / php / go / nginx / python）。

use std::path::PathBuf;
use std::sync::Arc;

#[tokio::main]
async fn main() {
    let base = PathBuf::from(".versions-home");
    std::fs::create_dir_all(&base).expect("创建 .versions-home");
    std::env::set_var("NSB_SKIP_HOSTS", "1");
    let state = nsb_core::CoreState::init(Some(base), Arc::new(|_| {})).expect("初始化");

    let args: Vec<String> = std::env::args().skip(1).collect();
    let ids: Vec<String> = if args.is_empty() {
        // 覆盖每一种版本源类型
        ["php", "node", "go", "nginx", "caddy", "mailpit", "minio", "memcached"]
            .iter().map(|s| s.to_string()).collect()
    } else {
        args
    };

    let mut pass = 0usize;
    let mut fail = 0usize;
    for id in &ids {
        let started = std::time::Instant::now();
        match state.version_catalog(id, false).await {
            Ok(cat) => {
                let n = cat.remote.len();
                if n == 0 {
                    // 未声明版本源（static）不算失败
                    println!("[SKIP] {id:<14} 无远程版本（未声明版本源）");
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
                let template = state.installer.template_for(id).expect("模板");
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
                // 抽查第一个版本的下载地址可达（HEAD 或 Range GET 拿 2xx/3xx）
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(25))
                    .build()
                    .unwrap();
                let probe = client
                    .get(&newest.url)
                    .header("Range", "bytes=0-64")
                    .send()
                    .await;
                match probe {
                    Ok(r) if r.status().is_success() || r.status().is_redirection() => {
                        println!("       ✓ 最新版本下载地址可达（HTTP {}）", r.status().as_u16());
                        pass += 1;
                    }
                    Ok(r) => {
                        println!("       ✗ 最新版本地址异常：HTTP {}", r.status());
                        fail += 1;
                    }
                    Err(e) => {
                        println!("       ✗ 最新版本地址不可达：{e}");
                        fail += 1;
                    }
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
        let cached = state.version_catalog(first, false).await;
        let ms = t.elapsed().as_millis();
        match cached {
            Ok(c) if !c.remote.is_empty() && ms < 400 => {
                println!("[PASS] 缓存命中        {first} 二次拉取 {ms}ms（{} 个版本）", c.remote.len());
                pass += 1;
            }
            Ok(c) => {
                println!("[FAIL] 缓存命中        {first} 二次拉取 {ms}ms（{} 个版本，期望 <400ms）", c.remote.len());
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
