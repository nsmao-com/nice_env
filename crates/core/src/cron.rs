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

/// 一个任务的运行锁同时保护多个应用进程；文件存在不代表正在运行，只有操作系统锁有效。
fn execution_lock(store: &Store, id: &str) -> Result<Option<std::fs::File>> {
    if id.is_empty()
        || id.len() > 128
        || !id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return Err(AppError::new("CRON_BAD_JOB", "任务标识无效"));
    }
    let dir = store
        .path
        .parent()
        .ok_or_else(|| AppError::new("CRON_PATH", "计划任务目录无效"))?
        .join("cron-locks");
    std::fs::create_dir_all(&dir)?;
    let file = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(dir.join(format!("{id}.lock")))?;
    match file.try_lock() {
        Ok(()) => Ok(Some(file)),
        Err(std::fs::TryLockError::WouldBlock) => Ok(None),
        Err(std::fs::TryLockError::Error(e)) => Err(AppError::io("锁定计划任务", e)),
    }
}

pub fn validate_job(job: &CronJob) -> Result<()> {
    if job.id.is_empty()
        || job.id.len() > 128
        || !job
            .id
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || b"-_".contains(&c))
    {
        return Err(AppError::new("CRON_BAD_JOB", "任务标识无效"));
    }
    if job.name.trim().is_empty()
        || job.name.chars().count() > 128
        || job.name.chars().any(char::is_control)
    {
        return Err(AppError::new(
            "CRON_BAD_JOB",
            "任务名称需为 1–128 个字符，且不能包含换行",
        ));
    }
    if job.command.trim().is_empty() || job.command.len() > 8192 || job.command.contains('\0') {
        return Err(AppError::new(
            "CRON_BAD_JOB",
            "命令不能为空、包含空字符或超过 8192 字节",
        ));
    }
    if !(1..=525_600).contains(&job.interval_min) {
        return Err(AppError::new(
            "CRON_BAD_JOB",
            "执行周期必须为 1–525600 分钟的整数",
        ));
    }
    Ok(())
}

pub(crate) fn is_due(job: &CronJob, now: i64) -> bool {
    // 首次按创建时间等待完整周期；手动运行会重新计算下一次周期。
    now.saturating_sub(job.last_run_at.unwrap_or(job.created_at))
        >= job.interval_min.saturating_mul(60_000)
}

fn recover_interrupted(store: &Store) -> Result<()> {
    for job in store.list_cron_jobs()? {
        if job.last_exit.as_deref() == Some(RUNNING) {
            if let Some(_lock) = execution_lock(store, &job.id)? {
                store.recover_cron_run(&job.id, job.last_run_at)?;
            }
        }
    }
    Ok(())
}

/// 调度线程：持有独立 SQLite 连接，每 20 秒复核到期任务。
pub fn spawn_scheduler(paths: Paths) -> Result<()> {
    let store = Store::open(paths.db())?;
    recover_interrupted(&store)?;
    std::thread::Builder::new()
        .name("cron-scheduler".into())
        .spawn(move || loop {
            std::thread::sleep(Duration::from_secs(20));
            if SHUTTING_DOWN.load(std::sync::atomic::Ordering::Acquire) {
                break;
            }
            if recover_interrupted(&store).is_err() {
                continue;
            }
            let Ok(jobs) = store.list_cron_jobs() else {
                continue;
            };
            let now = crate::services::now_ms();
            for job in jobs {
                if job.enabled && job.last_exit.as_deref() != Some(RUNNING) && is_due(&job, now) {
                    let path = paths.db();
                    let _ = std::thread::Builder::new()
                        .name(format!("cron-{}", job.id))
                        .spawn(move || {
                            if let Ok(store) = Store::open(path) {
                                let _ = run_job(&store, &job.id, false);
                            }
                        });
                }
            }
        })
        .map_err(|e| AppError::io("启动计划任务调度器", e))?;
    Ok(())
}

use parking_lot::Mutex;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, OnceLock,
};
type RunKey = (std::path::PathBuf, String);
struct ActiveRun {
    cancelled: AtomicBool,
    group: Mutex<platform::ProcessGroup>,
}
static ACTIVE: OnceLock<Mutex<HashMap<RunKey, Arc<ActiveRun>>>> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
fn active() -> &'static Mutex<HashMap<RunKey, Arc<ActiveRun>>> {
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}
struct RunGuard(RunKey);
impl Drop for RunGuard {
    fn drop(&mut self) {
        active().lock().remove(&self.0);
    }
}

/// 停止请求由执行线程确认并写入最终结果；不能把“已请求”显示成“已停止”。
pub fn stop_job(store: &Store, id: &str) -> Result<()> {
    let key = (store.path.clone(), id.to_string());
    let run = active().lock().get(&key).cloned().ok_or_else(|| {
        AppError::new(
            "CRON_NOT_RUNNING",
            "任务已结束或由其它应用进程执行，请刷新列表",
        )
    })?;
    run.cancelled.store(true, Ordering::Release);
    Ok(())
}

/// 应用退出时关闭调度并终止本进程拥有的命令树（包括命令启动的子进程）。
pub fn shutdown() {
    SHUTTING_DOWN.store(true, Ordering::Release);
    for run in active().lock().values() {
        run.cancelled.store(true, Ordering::Release);
        let _ = run.group.lock().terminate(true);
    }
}

/// 手动和自动执行共用：先取得运行锁，再在数据库事务中复核状态和调度条件。
pub fn run_job(store: &Store, id: &str, manual: bool) -> Result<CronJob> {
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(AppError::new("CRON_SHUTDOWN", "应用正在退出，无法启动任务"));
    }
    let _lock = execution_lock(store, id)?
        .ok_or_else(|| AppError::new("CRON_BUSY", "任务正在运行，请等待当前任务结束"))?;
    let key = (store.path.clone(), id.to_string());
    let run = Arc::new(ActiveRun {
        cancelled: AtomicBool::new(false),
        group: Mutex::new(platform::ProcessGroup::new()?),
    });
    active().lock().insert(key.clone(), run.clone());
    let _registration = RunGuard(key);
    let now = crate::services::now_ms();
    let Some(job) = store.claim_cron_run(id, manual, now)? else {
        return store
            .get_cron_job(id)?
            .ok_or_else(|| AppError::new("CRON_NOT_FOUND", "计划任务不存在"));
    };
    let (exit, output) = run_shell(&job.command, &run, Duration::from_secs(MAX_RUN_SECS));
    store.finish_cron_run(id, now, &exit, &output)?;
    store
        .get_cron_job(id)?
        .ok_or_else(|| AppError::new("CRON_NOT_FOUND", "任务运行后记录不存在，请检查本机存储"))
}

/// 持续排空管道，但每路最多保存 32 KiB；超出部分丢弃并标明截断。
fn drain(mut pipe: impl std::io::Read + Send + 'static) -> std::sync::mpsc::Receiver<String> {
    let (send, recv) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let mut bytes = Vec::new();
        let mut buffer = [0; 4096];
        let mut clipped = false;
        let mut error = None;
        loop {
            match pipe.read(&mut buffer) {
                Ok(0) => break,
                Ok(count) => {
                    let keep = count.min((32 * 1024usize).saturating_sub(bytes.len()));
                    bytes.extend_from_slice(&buffer[..keep]);
                    clipped |= keep < count;
                }
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        }
        let mut text = platform::decode_command_output(&bytes);
        if clipped {
            text.push_str("\n…（输出超过 32 KiB，后续内容已省略）");
        }
        if let Some(e) = error {
            text.push_str(&format!("\n读取输出失败：{e}"));
        }
        let _ = send.send(text);
    });
    recv
}

fn run_shell(command: &str, run: &ActiveRun, timeout: Duration) -> (String, String) {
    use std::process::Stdio;
    #[cfg(windows)]
    let mut cmd = {
        use std::os::windows::process::CommandExt;
        let mut cmd = platform::command("cmd");
        // 用户输入的是完整 shell 命令；cmd 不使用 C argv 转义规则。
        cmd.args(["/D", "/S", "/C"])
            .raw_arg(format!("\"{command}\""));
        cmd
    };
    #[cfg(not(windows))]
    let mut cmd = {
        let mut cmd = platform::command("sh");
        cmd.args(["-c", command]);
        cmd
    };
    cmd.stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            cmd.pre_exec(platform::spawn_pre_exec);
        }
    }
    let mut child = {
        // 与退出清理互斥，防止退出检查后又启动新的孤儿进程。
        let mut group = run.group.lock();
        if run.cancelled.load(Ordering::Acquire) || SHUTTING_DOWN.load(Ordering::Acquire) {
            return ("cancelled".into(), "任务在启动前已停止".into());
        }
        let mut child = match cmd.spawn() {
            Ok(child) => child,
            Err(e) => return ("spawn failed".into(), e.to_string()),
        };
        if let Err(e) = group.attach(child.id()) {
            let _ = child.kill();
            let _ = child.wait();
            return ("spawn failed".into(), format!("无法管理命令进程树：{e}"));
        }
        child
    };
    let stdout = child.stdout.take().map(drain);
    let stderr = child.stderr.take().map(drain);
    let deadline = std::time::Instant::now() + timeout;
    let (mut exit, mut detail) = loop {
        if run.cancelled.load(Ordering::Acquire) {
            break ("cancelled".into(), "已停止命令及其子进程".into());
        }
        if std::time::Instant::now() >= deadline {
            break (
                "timeout".into(),
                "任务超过最长运行时间，已终止命令及其子进程".into(),
            );
        }
        match child.try_wait() {
            Ok(Some(status)) => {
                break (
                    format!("exit {}", status.code().unwrap_or(-1)),
                    String::new(),
                )
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(e) => break ("wait failed".into(), e.to_string()),
        }
    };
    // 即使 shell 先退出，也清理仍继承输出句柄的后台子进程，避免输出读取永久等待。
    if let Err(e) = run.group.lock().terminate(true) {
        exit = "stop failed".into();
        detail = format!("进程树清理失败：{e}");
    }
    let _ = child.kill();
    let _ = child.wait();
    let read = |receiver: Option<std::sync::mpsc::Receiver<String>>| {
        receiver
            .and_then(|r| r.recv_timeout(Duration::from_secs(2)).ok())
            .unwrap_or_else(|| "输出管道未关闭，未能完整读取".into())
    };
    let output = format!(
        "{detail}\nstdout:\n{}\n\nstderr:\n{}",
        truncate(&read(stdout), 4000),
        truncate(&read(stderr), 4000)
    );
    (exit, output)
}

fn truncate(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let cut: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{cut}\n…（输出已截断）")
    } else {
        cut
    }
}

pub fn new_id() -> String {
    format!(
        "cron-{}-{:016x}",
        crate::services::now_ms(),
        rand::random::<u64>()
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(command: &str) -> (tempfile::TempDir, Store, CronJob) {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("nsb.sqlite")).unwrap();
        let job = CronJob {
            id: new_id(),
            name: "Fixture".into(),
            command: command.into(),
            interval_min: 5,
            enabled: true,
            created_at: crate::services::now_ms() - 600_000,
            last_run_at: None,
            last_exit: None,
            last_output: None,
        };
        store.save_cron_job(&job, true).unwrap();
        (dir, store, job)
    }
    fn slow_command() -> &'static str {
        #[cfg(windows)]
        {
            "ping -n 30 127.0.0.1 >nul"
        }
        #[cfg(not(windows))]
        {
            "sleep 30"
        }
    }
    fn run_state() -> Arc<ActiveRun> {
        Arc::new(ActiveRun {
            cancelled: AtomicBool::new(false),
            group: Mutex::new(platform::ProcessGroup::new().unwrap()),
        })
    }

    #[test]
    fn cron_validation_and_due_time_do_not_silently_change_user_input() {
        let (_dir, _store, mut job) = fixture("echo fixture");
        job.created_at = 10_000;
        job.interval_min = 5;
        assert!(!is_due(&job, 309_999));
        assert!(is_due(&job, 310_000));
        job.last_run_at = Some(300_000);
        assert!(!is_due(&job, 310_000));
        assert!(is_due(&job, 600_000));
        for minutes in [0, -1, 525_601, i64::MAX] {
            job.interval_min = minutes;
            assert!(validate_job(&job).is_err());
        }
        job.interval_min = 5;
        job.command = "\0".into();
        assert!(validate_job(&job).is_err());
        job.command = "echo fixture".into();
        job.name.clear();
        assert!(validate_job(&job).is_err());
        job.name = "valid".into();
        job.id = "../outside".into();
        assert!(validate_job(&job).is_err());
    }

    #[test]
    fn cron_claim_rechecks_enabled_due_time_and_existing_state() {
        let (_dir, store, job) = fixture("echo fixture");
        store.set_cron_enabled(&job.id, false).unwrap();
        assert!(store
            .claim_cron_run(&job.id, false, crate::services::now_ms())
            .unwrap()
            .is_none());
        let now = crate::services::now_ms();
        let running = store.claim_cron_run(&job.id, true, now).unwrap().unwrap();
        assert_eq!(running.last_exit.as_deref(), Some(RUNNING));
        assert_eq!(
            store.claim_cron_run(&job.id, true, now).unwrap_err().code,
            "CRON_BUSY"
        );
        assert!(store.delete_cron_job(&job.id).is_err());
        assert!(store.save_cron_job(&job, false).is_err());
        store
            .finish_cron_run(&job.id, now, "exit 7", "failure output")
            .unwrap();
        let mut changed = job.clone();
        changed.command = "echo new".into();
        changed.enabled = true;
        store.save_cron_job(&changed, false).unwrap();
        let saved = store.get_cron_job(&job.id).unwrap().unwrap();
        assert!(!saved.enabled);
        assert_eq!(saved.last_output.as_deref(), Some("failure output"));
        store.set_cron_enabled(&job.id, true).unwrap();
        assert!(store
            .claim_cron_run(&job.id, false, now + 1)
            .unwrap()
            .is_none());
        store.delete_cron_job(&job.id).unwrap();
        assert!(store.delete_cron_job(&job.id).is_err());
        assert!(store.set_cron_enabled(&job.id, true).is_err());
        assert!(store.save_cron_job(&job, false).is_err());
    }

    #[test]
    fn cron_atomic_claim_across_sqlite_connections_only_has_one_winner() {
        let (_dir, store, job) = fixture("echo fixture");
        let other = Store::open(store.path.clone()).unwrap();
        let barrier = Arc::new(std::sync::Barrier::new(2));
        let peer = barrier.clone();
        let id = job.id.clone();
        let now = crate::services::now_ms();
        let thread = std::thread::spawn(move || {
            peer.wait();
            other.claim_cron_run(&id, true, now)
        });
        barrier.wait();
        let first = store.claim_cron_run(&job.id, true, now);
        let second = thread.join().unwrap();
        assert_ne!(first.is_ok(), second.is_ok());
        let error = if let Err(e) = first {
            e
        } else {
            second.unwrap_err()
        };
        assert_eq!(error.code, "CRON_BUSY");
    }

    #[test]
    fn cron_recovery_uses_os_lock_not_stale_running_text() {
        let (_dir, store, job) = fixture("echo fixture");
        let other = Store::open(store.path.clone()).unwrap();
        let lock = execution_lock(&store, &job.id).unwrap().unwrap();
        assert!(execution_lock(&other, &job.id).unwrap().is_none());
        store
            .claim_cron_run(&job.id, true, crate::services::now_ms())
            .unwrap();
        recover_interrupted(&other).unwrap();
        assert_eq!(
            store
                .get_cron_job(&job.id)
                .unwrap()
                .unwrap()
                .last_exit
                .as_deref(),
            Some(RUNNING)
        );
        drop(lock);
        recover_interrupted(&other).unwrap();
        let recovered = store.get_cron_job(&job.id).unwrap().unwrap();
        assert_eq!(recovered.last_exit.as_deref(), Some("interrupted"));
        assert!(!recovered.enabled);
    }

    #[cfg(windows)]
    #[test]
    fn cron_windows_shell_preserves_quoted_script_and_arguments() {
        let directory = tempfile::tempdir().unwrap();
        let folder = directory.path().join("folder with spaces");
        std::fs::create_dir_all(&folder).unwrap();
        let script = folder.join("fixture task.cmd");
        std::fs::write(
            &script,
            "@echo off\r\necho fixture argument: %~1\r\nexit /b 0\r\n",
        )
        .unwrap();
        let command = format!("\"{}\" \"hello world\"", script.display());
        let (exit, output) = run_shell(&command, &run_state(), Duration::from_secs(5));
        assert_eq!(exit, "exit 0", "{output}");
        assert!(output.contains("fixture argument: hello world"), "{output}");
    }

    #[test]
    fn cron_real_shell_records_success_and_nonzero_exit() {
        #[cfg(windows)]
        let command = "echo fixture-out & echo fixture-err 1>&2 & exit /b 7";
        #[cfg(not(windows))]
        let command = "echo fixture-out; echo fixture-err >&2; exit 7";
        let (_dir, store, mut job) = fixture(command);
        let result = run_job(&store, &job.id, false).unwrap();
        assert_eq!(result.last_exit.as_deref(), Some("exit 7"));
        let output = result.last_output.unwrap();
        assert!(output.contains("fixture-out"));
        assert!(output.contains("fixture-err"));
        job.command = "echo recovered 中文输出".into();
        store.save_cron_job(&job, false).unwrap();
        let result = run_job(&store, &job.id, true).unwrap();
        assert_eq!(result.last_exit.as_deref(), Some("exit 0"));
        assert!(result.last_output.unwrap().contains("recovered 中文输出"));
    }

    #[test]
    fn cron_cancel_stops_owned_process_and_unlocks_task() {
        let (_dir, store, job) = fixture(slow_command());
        let other = Store::open(store.path.clone()).unwrap();
        let id = job.id.clone();
        let worker = std::thread::spawn(move || run_job(&other, &id, true));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let pids = loop {
            if let Some(run) = active()
                .lock()
                .get(&(store.path.clone(), job.id.clone()))
                .cloned()
            {
                let pids = run.group.lock().pids().to_vec();
                if !pids.is_empty() {
                    break pids;
                }
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(
            run_job(&store, &job.id, true).unwrap_err().code,
            "CRON_BUSY"
        );
        stop_job(&store, &job.id).unwrap();
        let result = worker.join().unwrap().unwrap();
        assert_eq!(result.last_exit.as_deref(), Some("cancelled"));
        assert!(pids.iter().all(|pid| !platform::process_alive(*pid)));
        assert!(execution_lock(&store, &job.id).unwrap().is_some());
        assert!(stop_job(&store, &job.id).is_err());
    }

    #[test]
    fn cron_timeout_finishes_without_inherited_pipe_hang_and_output_is_bounded() {
        let started = std::time::Instant::now();
        let (exit, output) = run_shell(slow_command(), &run_state(), Duration::from_millis(200));
        assert_eq!(exit, "timeout");
        assert!(output.contains("最长运行时间"));
        assert!(started.elapsed() < Duration::from_secs(5));
        let captured = drain(std::io::Cursor::new(vec![b'x'; 2 * 1024 * 1024]))
            .recv_timeout(Duration::from_secs(2))
            .unwrap();
        assert!(captured.len() < 33 * 1024);
        assert!(captured.contains("省略"));
        assert!(truncate(&"中".repeat(9000), 4000).chars().count() < 4030);
    }
}
