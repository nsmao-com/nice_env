//! 计划任务：应用级 cron —— 任务定义存 SQLite，调度线程按 20s 心跳触发到点任务。
//! 应用退出即停（与 FlyEnv 的 cron 行为一致）；命令经系统 shell 执行，
//! 输出截断保存，避免长输出撑爆数据库。运行中的任务不会重复触发。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::time::Duration;

pub const RUNNING: &str = "running";

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
    if !manual && job.last_exit.as_deref() == Some(RUNNING) {
        return Ok(job); // 上一次还没跑完，跳过本轮
    }
    let now = crate::services::now_ms();
    store.mark_cron_run(id, now, RUNNING, None)?;

    let (exit, output) = run_shell(&job.command);
    store.mark_cron_run(id, now, &exit, Some(&output))?;
    Ok(store.get_cron_job(id)?.unwrap_or(job))
}

fn run_shell(command: &str) -> (String, String) {
    #[cfg(windows)]
    let out = std::process::Command::new("cmd").args(["/C", command]).output();
    #[cfg(not(windows))]
    let out = std::process::Command::new("sh").arg("-c").arg(command).output();
    match out {
        Ok(o) => {
            let text = format!(
                "stdout:\n{}\n\nstderr:\n{}",
                String::from_utf8_lossy(&o.stdout),
                String::from_utf8_lossy(&o.stderr)
            );
            (
                format!("exit {}", o.status.code().unwrap_or(-1)),
                truncate(&text, 8000),
            )
        }
        Err(e) => ("spawn failed".into(), truncate(&e.to_string(), 2000)),
    }
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
