//! 计划任务：应用级 cron —— 任务定义存 SQLite，调度线程按 20s 心跳触发到点任务。
//! 应用退出即停（与 FlyEnv 的 cron 行为一致）；命令经系统 shell 执行，
//! 输出截断保存，避免长输出撑爆数据库。运行中的任务不会重复触发。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const RUNNING: &str = "running";
const MAX_RUN_SECS: u64 = 30 * 60;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CronJob {
    pub id: String,
    pub name: String,
    pub command: String,
    pub interval_min: i64,
    pub enabled: bool,
    pub created_at: i64,
    pub last_run_at: Option<i64>,
    pub last_exit: Option<String>,
    pub last_output: Option<String>,
}

/// 调度线程：应用启动时拉起，独立 SQLite 连接，每 20s 扫一遍到期任务
pub fn spawn_scheduler(paths: Paths) {
    std::thread::Builder::new()
        .name("cron-scheduler".into())
        .spawn(move || {
            let Ok(store) = Store::open(paths.db()) else {
                return;
            };
            loop {
                std::thread::sleep(Duration::from_secs(20));
                let Ok(jobs) = store.list_cron_jobs() else {
                    continue;
                };
                let now = crate::services::now_ms();
                for job in jobs {
                    if !job.enabled || job.last_exit.as_deref() == Some(RUNNING) {
                        continue;
                    }
                    let due = job
                        .last_run_at
                        .map_or(true, |t| now - t >= job.interval_min * 60_000);
                    if due {
                        spawn_run(&paths, &job.id);
                    }
                }
            }
        })
        .ok();
}

/// 到点任务在独立线程里跑，避免一条慢命令卡住整个调度器
fn spawn_run(paths: &Paths, id: &str) {
    let paths = paths.clone();
    let id = id.to_string();
    std::thread::Builder::new()
        .name(format!("cron-{id}"))
        .spawn(move || {
            if let Ok(store) = Store::open(paths.db()) {
                let _ = run_job(&store, &id, false);
            }
        })
        .ok();
}

/// 执行一次任务（调度触发或手动「立即运行」共用）
pub fn run_job(store: &Store, id: &str, manual: bool) -> Result<CronJob> {
    let job = store
        .get_cron_job(id)?
        .ok_or_else(|| AppError::new("CRON_NOT_FOUND", format!("计划任务 {id} 不存在")))?;
    if job.last_exit.as_deref() == Some(RUNNING) {
        if manual {
            return Err(AppError::new(
                "CRON_BUSY",
                "该计划任务正在运行，请等待当前任务结束",
            ));
        }
        return Ok(job); // 调度轮次跳过正在运行的任务
    }
    let now = crate::services::now_ms();
    store.mark_cron_run(id, now, RUNNING, None)?;

    let (exit, output) = run_shell(&job.command);
    store.mark_cron_run(id, now, &exit, Some(&output))?;
    Ok(store.get_cron_job(id)?.unwrap_or(job))
}

fn run_shell(command: &str) -> (String, String) {
    use std::io::Read;
    use std::process::Stdio;

    let spawn = {
        #[cfg(windows)]
        {
            platform::command("cmd")
                .args(["/C", command])
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        }
        #[cfg(not(windows))]
        {
            platform::command("sh")
                .arg("-c")
                .arg(command)
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()
        }
    };
    let mut child = match spawn {
        Ok(child) => child,
        Err(e) => return ("spawn failed".into(), truncate(&e.to_string(), 2000)),
    };

    let stdout = child.stdout.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    });
    let stderr = child.stderr.take().map(|mut pipe| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            let _ = pipe.read_to_end(&mut bytes);
            bytes
        })
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(MAX_RUN_SECS);
    let mut timed_out = false;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) if std::time::Instant::now() < deadline => {
                std::thread::sleep(Duration::from_millis(100));
            }
            Ok(None) => {
                timed_out = true;
                let _ = child.kill();
                break child.wait().ok();
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let out = stdout
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    let err = stderr
        .and_then(|thread| thread.join().ok())
        .unwrap_or_default();
    let text = format!(
        "stdout:\n{}\n\nstderr:\n{}",
        String::from_utf8_lossy(&out),
        String::from_utf8_lossy(&err)
    );
    if timed_out {
        return (
            "timeout".into(),
            truncate(
                &format!("{text}\n\n任务超过 {} 分钟，已终止", MAX_RUN_SECS / 60),
                8000,
            ),
        );
    }
    let Some(status) = status else {
        return ("wait failed".into(), truncate(&text, 8000));
    };
    (
        format!("exit {}", status.code().unwrap_or(-1)),
        truncate(&text, 8000),
    )
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let cut: String = s.chars().take(max).collect();
        format!("{cut}\n…")
    }
}

/// 生成短 id（新建任务时前端不传 id 则后端生成）
pub fn new_id() -> String {
    format!("cron-{}", crate::services::now_ms())
}
