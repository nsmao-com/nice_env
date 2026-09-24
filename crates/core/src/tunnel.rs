//! 快速隧道：cloudflared 临时隧道（无需账号/域名），把本机端口暴露到公网。
//! 启动后从子进程输出里抓 `*.trycloudflare.com` 公网地址；应用退出随进程结束。

use crate::error::{AppError, Result};
use crate::model::TunnelInfo;
use parking_lot::Mutex;
use std::io::{BufRead, BufReader};
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};

struct TunnelEntry {
    id: String,
    port: u16,
    started_at: i64,
    url: Arc<Mutex<Option<String>>>,
    child: Mutex<std::process::Child>,
}

static TUNNELS: OnceLock<Mutex<Vec<TunnelEntry>>> = OnceLock::new();
static SEQ: AtomicU64 = AtomicU64::new(1);

fn map() -> &'static Mutex<Vec<TunnelEntry>> {
    TUNNELS.get_or_init(|| Mutex::new(Vec::new()))
}

fn alive(t: &TunnelEntry) -> bool {
    matches!(t.child.lock().try_wait(), Ok(None))
}

/// 启动一条临时隧道。exe 由调用方解析（已安装的 cloudflared 主程序）。
pub fn start(exe: &Path, port: u16) -> Result<TunnelInfo> {
    {
        let mut v = map().lock();
        v.retain(|t| alive(t));
        if let Some(t) = v.iter().find(|t| t.port == port) {
            let url = t.url.lock().clone();
            return Ok(TunnelInfo {
                id: t.id.clone(),
                port: t.port,
                url,
                started_at: t.started_at,
                alive: true,
            });
        }
    }

    let mut child = std::process::Command::new(exe)
        .args([
            "tunnel",
            "--no-autoupdate",
            "--url",
            &format!("http://127.0.0.1:{port}"),
        ])
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .map_err(|e| AppError::io("启动 cloudflared", e))?;
    let started_at = crate::services::now_ms();
    let id = format!("tunnel-{}", SEQ.fetch_add(1, Ordering::Relaxed));

    let url = Arc::new(Mutex::new(None));
    // cloudflared 把公网地址打在 stderr，stdout 也一并读，谁先抓到算谁的
    let so = child.stdout.take();
    let se = child.stderr.take();
    let sink_out = url.clone();
    std::thread::Builder::new()
        .name(format!("tunnel-out-{id}"))
        .spawn(move || {
            if let Some(s) = so {
                read_url(s, sink_out);
            }
        })
        .ok();
    let sink_err = url.clone();
    std::thread::Builder::new()
        .name(format!("tunnel-err-{id}"))
        .spawn(move || {
            if let Some(s) = se {
                read_url(s, sink_err);
            }
        })
        .ok();

    let info = TunnelInfo {
        id: id.clone(),
        port,
        url: None,
        started_at,
        alive: true,
    };
    map().lock().push(TunnelEntry {
        id,
        port,
        started_at,
        url,
        child: Mutex::new(child),
    });
    Ok(info)
}

/// 活跃隧道列表（已退出的顺带清理）
pub fn list() -> Vec<TunnelInfo> {
    let mut v = map().lock();
    v.retain(|t| alive(t));
    v.iter()
        .map(|t| TunnelInfo {
            id: t.id.clone(),
            port: t.port,
            url: t.url.lock().clone(),
            started_at: t.started_at,
            alive: true,
        })
        .collect()
}

pub fn stop(id: &str) -> Result<()> {
    let mut v = map().lock();
    v.retain(|t| alive(t));
    if let Some(pos) = v.iter().position(|t| t.id == id) {
        let entry = v.remove(pos);
        let _ = entry.child.lock().kill();
        let _ = entry.child.lock().wait();
        Ok(())
    } else {
        Err(AppError::new("TUNNEL_NOT_FOUND", "隧道不存在或已退出"))
    }
}

/// 持续读取 cloudflared 的一路输出，抓到公网地址后写入 sink。
///
/// 抓到地址后**不能停止读取**：cloudflared 会一直打日志，没人读的话管道缓冲区
/// 写满后它会卡在写日志上，隧道随之假死。所以一直读到 EOF（进程退出）为止。
/// 按字节读再有损转 UTF-8：遇到非 UTF-8 输出也不会中断（`lines()` 会在那一行报错）。
fn read_url(stream: impl std::io::Read, sink: Arc<Mutex<Option<String>>>) {
    let mut reader = BufReader::new(stream);
    let mut buf = Vec::new();
    loop {
        buf.clear();
        match reader.read_until(b'\n', &mut buf) {
            Ok(0) | Err(_) => return,
            Ok(_) => {}
        }
        let mut url = sink.lock();
        if url.is_none() {
            *url = extract_tunnel_url(&String::from_utf8_lossy(&buf));
        }
    }
}

/// 从一行 cloudflared 输出里提取快速隧道公网地址（`https://<随机名>.trycloudflare.com`）。
///
/// cloudflared 在给出地址之前会先打一行
/// `... INF Requesting new quick Tunnel on trycloudflare.com...`，
/// 失败时还会打出 `https://api.trycloudflare.com/tunnel` 这类 API 地址，二者都不是隧道地址。
fn extract_tunnel_url(line: &str) -> Option<String> {
    const SUFFIX: &str = ".trycloudflare.com";
    let mut rest = line;
    while let Some(pos) = rest.find("https://") {
        let after = &rest[pos + "https://".len()..];
        let host: String = after
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '.')
            .collect();
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        if let Some(sub) = host.strip_suffix(SUFFIX) {
            if !sub.is_empty() && sub != "api" && !sub.contains('.') {
                return Some(format!("https://{host}"));
            }
        }
        rest = after;
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ignores_request_line_before_url() {
        // 旧实现会把这一行行首的时间戳当成公网地址
        let l = "2026-09-24T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com...";
        assert_eq!(extract_tunnel_url(l), None);
    }

    #[test]
    fn ignores_api_endpoint_in_errors() {
        let l = r#"2026-09-24T10:00:00Z ERR failed to request quick Tunnel: Post "https://api.trycloudflare.com/tunnel": dial tcp: i/o timeout"#;
        assert_eq!(extract_tunnel_url(l), None);
    }

    #[test]
    fn extracts_url_from_banner_box() {
        let l = "2026-09-24T10:00:01Z INF |  https://Brave-Otter-Sample-42.trycloudflare.com                                     |";
        assert_eq!(
            extract_tunnel_url(l).as_deref(),
            Some("https://brave-otter-sample-42.trycloudflare.com")
        );
    }

    #[test]
    fn read_url_takes_real_url_and_drains_the_rest() {
        let out = "2026-09-24T10:00:00Z INF Thank you for trying Cloudflare Tunnel.\n\
2026-09-24T10:00:00Z INF Requesting new quick Tunnel on trycloudflare.com...\n\
2026-09-24T10:00:01Z INF |  https://first-url.trycloudflare.com  |\n\
2026-09-24T10:00:02Z INF |  https://second-url.trycloudflare.com  |\n";
        let mut bytes = out.as_bytes().to_vec();
        // 夹一段非 UTF-8 字节，读取不能因此中断
        bytes.extend_from_slice(b"\xff\xfe garbage\n");
        bytes.extend_from_slice(b"2026-09-24T10:00:03Z INF Registered tunnel connection\n");
        let cursor = std::io::Cursor::new(bytes);
        let sink = Arc::new(Mutex::new(None));
        read_url(cursor, sink.clone());
        assert_eq!(
            sink.lock().as_deref(),
            Some("https://first-url.trycloudflare.com")
        );
    }
}
