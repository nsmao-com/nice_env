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
        let mut entry = v.remove(pos);
        let _ = entry.child.lock().kill();
        let _ = entry.child.lock().wait();
        Ok(())
    } else {
        Err(AppError::new("TUNNEL_NOT_FOUND", "隧道不存在或已退出"))
    }
}

fn read_url(stream: impl std::io::Read, sink: Arc<Mutex<Option<String>>>) {
    for line in BufReader::new(stream).lines().flatten() {
        if sink.lock().is_some() {
            return;
        }
        if let Some(pos) = line.find("trycloudflare.com") {
            let head = &line[..pos];
            let start = head.rfind("https://").map(|p| p).unwrap_or(0);
            let candidate = line[start..]
                .split_whitespace()
                .next()
                .unwrap_or("")
                .to_string();
            if !candidate.is_empty() {
                *sink.lock() = Some(candidate);
            }
        }
    }
}
