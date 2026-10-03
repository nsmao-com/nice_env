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
    spawn_scheduler_when_ready(paths,None)
}

pub fn spawn_scheduler_when_ready(paths: Paths, gate: Option<std::sync::Arc<crate::restart::StartupGate>>) -> Result<()> {
    let _activity = crate::paths::DataDirActivity::shared(&paths.base)?;
    let store = Store::open(paths.db())?;
    recover_interrupted(&store)?;
    std::thread::Builder::new()
        .name("cron-scheduler".into())
        .spawn(move || {
            if gate.is_some_and(|gate| !gate.wait()) { return; }
            loop {
                std::thread::sleep(Duration::from_secs(20));
                if SHUTTING_DOWN.load(std::sync::atomic::Ordering::Acquire) {
                    continue;
                }
                let Ok(_activity) = crate::paths::DataDirActivity::shared(&paths.base) else { continue; };
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
                                let Some(base) = path.parent() else { return; };
                                let Ok(_activity) = crate::paths::DataDirActivity::shared(base) else { return; };
                                if let Ok(store) = Store::open(path) {
                                    let _ = run_job(&store, &job.id, false);
                                }
                            });
                    }
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
    pid: std::sync::atomic::AtomicU32,
    tree_owned: AtomicBool,
    finished: AtomicBool,
    cleanup_failed: AtomicBool,
    record_error: Mutex<Option<AppError>>,
}
static ACTIVE: OnceLock<Mutex<HashMap<RunKey, Arc<ActiveRun>>>> = OnceLock::new();
static SHUTTING_DOWN: AtomicBool = AtomicBool::new(false);
fn active() -> &'static Mutex<HashMap<RunKey, Arc<ActiveRun>>> {
    ACTIVE.get_or_init(|| Mutex::new(HashMap::new()))
}
fn forget_run(key: &RunKey, run: &Arc<ActiveRun>) {
    let mut runs = active().lock();
    if runs.get(key).is_some_and(|current| Arc::ptr_eq(current, run)) { runs.remove(key); }
}
struct RunGuard(RunKey);
impl Drop for RunGuard {
    fn drop(&mut self) {
        let mut runs = active().lock();
        if let Some(run) = runs.get(&self.0) {
            if std::thread::panicking() {
                run.cleanup_failed.store(true, Ordering::Release);
                *run.record_error.lock() = Some(AppError::new("CRON_WORKER_FAILED", "计划任务执行线程异常结束"));
            }
            run.finished.store(true, Ordering::Release);
            // 清理失败时保留组句柄和 PID，后续停止/退出仍能重试。
            if !run.cleanup_failed.load(Ordering::Acquire) { runs.remove(&self.0); }
        }
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
    if run.finished.load(Ordering::Acquire) {
        retry_cleanup(&run)?;
        forget_run(&key, &run);
    }
    Ok(())
}

/// 应用退出时关闭调度并终止本进程拥有的命令树（包括命令启动的子进程）。
pub fn shutdown() {
    let _ = shutdown_checked(Duration::from_secs(12));
}

pub fn resume_after_shutdown() {
    SHUTTING_DOWN.store(false, Ordering::Release);
}

pub fn shutdown_checked(timeout: Duration) -> Result<()> {
    let runs = {
        let runs = active().lock();
        SHUTTING_DOWN.store(true, Ordering::Release);
        runs.iter().map(|(key, run)| (key.clone(), run.clone())).collect::<Vec<_>>()
    };
    for (_, run) in &runs { run.cancelled.store(true, Ordering::Release); }
    let deadline = std::time::Instant::now() + timeout;
    while runs.iter().any(|(_, run)| !run.finished.load(Ordering::Acquire)) && std::time::Instant::now() < deadline {
        std::thread::sleep(Duration::from_millis(20));
    }
    let mut errors = Vec::new();
    for (key, run) in runs {
        if !run.finished.load(Ordering::Acquire) {
            errors.push(format!("任务 {} 仍在结束或保存结果，请稍后重试", key.1));
            continue;
        }
        if run.cleanup_failed.load(Ordering::Acquire) {
            if std::time::Instant::now() >= deadline {
                errors.push(format!("任务 {} 的清理尚未确认，请再次停止后重试", key.1));
                continue;
            }
            match retry_cleanup(&run) {
                Ok(()) => { forget_run(&key, &run); },
                Err(error) => errors.push(format!("任务 {}：{}", key.1, error.message)),
            }
        }
        if let Some(error) = run.record_error.lock().as_ref() {
            errors.push(format!("任务 {} 结果保存失败：{}", key.1, error.message));
        }
    }
    if errors.is_empty() { Ok(()) } else {
        Err(AppError::new("CRON_SHUTDOWN_FAILED", errors.join("；")))
    }
}

fn verify_process_gone(pid: u32) -> Result<()> {
    if pid == 0 { return Ok(()); }
    if platform::process_alive(pid) { return Err(AppError::new("CRON_STOP_FAILED", "计划任务进程仍未退出")); }
    #[cfg(unix)]
    {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !platform::process_group_gone(pid).map_err(AppError::from)? {
            if std::time::Instant::now() >= deadline {
                return Err(AppError::new("CRON_STOP_FAILED", "计划任务子进程组仍未退出"));
            }
            std::thread::sleep(Duration::from_millis(20));
        }
    }
    Ok(())
}

fn retry_cleanup(run: &ActiveRun) -> Result<()> {
    let pid = run.pid.load(Ordering::Acquire);
    if pid != 0 && !run.tree_owned.load(Ordering::Acquire) {
        return Err(AppError::new("CRON_STOP_FAILED", "命令未能加入受管进程组，无法确认其子进程已全部退出；请检查系统进程状态"));
    }
    let mut group = run.group.try_lock_for(Duration::from_secs(2))
        .ok_or_else(|| AppError::new("CRON_BUSY", "计划任务进程组正在清理，请稍后重试"))?;
    #[cfg(unix)]
    if pid != 0 && group.pids().is_empty() && !platform::process_group_gone(pid).map_err(AppError::from)? {
        *group = platform::ProcessGroup::from_pids(vec![pid]);
    }
    group.terminate(true).map_err(AppError::from)?;
    drop(group);
    verify_process_gone(pid)?;
    run.cleanup_failed.store(false, Ordering::Release);
    Ok(())
}

/// 手动和自动执行共用：先取得运行锁，再在数据库事务中复核状态和调度条件。
pub fn run_job(store: &Store, id: &str, manual: bool) -> Result<CronJob> {
    let base = store.path.parent().ok_or_else(|| AppError::new("CRON_PATH", "计划任务目录无效"))?;
    let _activity = crate::paths::DataDirActivity::shared(base)?;
    if SHUTTING_DOWN.load(Ordering::Acquire) {
        return Err(AppError::new("CRON_SHUTDOWN", "应用正在退出，无法启动任务"));
    }
    let _lock = execution_lock(store, id)?
        .ok_or_else(|| AppError::new("CRON_BUSY", "任务正在运行，请等待当前任务结束"))?;
    let key = (store.path.clone(), id.to_string());
    let run = Arc::new(ActiveRun {
        cancelled: AtomicBool::new(false),
        group: Mutex::new(platform::ProcessGroup::new()?),
        pid: std::sync::atomic::AtomicU32::new(0),
        tree_owned: AtomicBool::new(false),
        finished: AtomicBool::new(false),
        cleanup_failed: AtomicBool::new(false),
        record_error: Mutex::new(None),
    });
    {
        let mut runs = active().lock();
        if SHUTTING_DOWN.load(Ordering::Acquire) { return Err(AppError::new("CRON_SHUTDOWN", "应用正在退出，无法启动任务")); }
        if runs.contains_key(&key) { return Err(AppError::new("CRON_BUSY", "任务仍在运行或清理，请先停止后重试")); }
        runs.insert(key.clone(), run.clone());
    }
    let _registration = RunGuard(key);
    let now = crate::services::now_ms();
    let Some(job) = store.claim_cron_run(id, manual, now)? else {
        return store
            .get_cron_job(id)?
            .ok_or_else(|| AppError::new("CRON_NOT_FOUND", "计划任务不存在"));
    };
    let (exit, output) = run_shell(&job.command, &run, Duration::from_secs(MAX_RUN_SECS));
    store.finish_cron_run(id, now, &exit, &output).map_err(|error| {
        *run.record_error.lock() = Some(error.clone());
        error
    })?;
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
        // cmd 会缓存启动代码页，必须先设置 UTF-8 再启动实际执行命令的 cmd。
        // 延迟展开只发生在外层，避免用户命令的 %、!、^ 被两层 shell 重复解释。
        cmd.env("NSB_CRON_COMMAND", command)
            .args(["/D", "/V:ON", "/S", "/C"])
            .raw_arg("\"chcp 65001 >nul && cmd /D /V:OFF /S /C \"!NSB_CRON_COMMAND!\"\"");
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
        run.pid.store(child.id(), Ordering::Release);
        if let Err(e) = group.attach(child.id()) {
            run.cleanup_failed.store(true, Ordering::Release);
            let _ = child.kill();
            let deadline = std::time::Instant::now() + Duration::from_secs(3);
            while matches!(child.try_wait(), Ok(None)) && std::time::Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(20));
            }
            return ("spawn failed".into(), format!("无法管理命令进程树：{e}"));
        }
        run.tree_owned.store(true, Ordering::Release);
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
        run.cleanup_failed.store(true, Ordering::Release);
        exit = "stop failed".into();
        detail = format!("进程树清理失败：{e}");
    }
    let _ = child.kill();
    let deadline = std::time::Instant::now() + Duration::from_secs(3);
    loop {
        match child.try_wait() {
            Ok(Some(_)) => break,
            Ok(None) if std::time::Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            _ => { run.cleanup_failed.store(true, Ordering::Release); break; },
        }
    }
    if let Err(error) = verify_process_gone(child.id()) {
        run.cleanup_failed.store(true, Ordering::Release);
        detail = format!("{detail}\n{}", error.message);
    }
    if run.cleanup_failed.load(Ordering::Acquire) { exit = "stop failed".into(); }
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
            pid: std::sync::atomic::AtomicU32::new(0),
            tree_owned: AtomicBool::new(false),
            finished: AtomicBool::new(false),
            cleanup_failed: AtomicBool::new(false),
            record_error: Mutex::new(None),
        })
    }

    #[test]
    fn cron_shutdown_waits_and_failed_transition_restores_operations() {
        let output = platform::command(std::env::current_exe().unwrap())
            .args(["--exact", "cron::tests::cron_shutdown_probe", "--nocapture"])
            .env("NSB_CRON_SHUTDOWN_PROBE", "1").output().unwrap();
        assert!(output.status.success(), "{}\n{}", String::from_utf8_lossy(&output.stdout), String::from_utf8_lossy(&output.stderr));
    }

    #[test]
    fn cron_shutdown_probe() {
        if std::env::var_os("NSB_CRON_SHUTDOWN_PROBE").is_none() { return; }
        let (_dir, store, job) = fixture(slow_command());
        let other = Store::open(store.path.clone()).unwrap();
        let id = job.id.clone();
        let worker = std::thread::spawn(move || run_job(&other, &id, true));
        let deadline = std::time::Instant::now() + Duration::from_secs(5);
        let run = loop {
            let run = active().lock().get(&(store.path.clone(), job.id.clone())).cloned();
            if let Some(run) = run {
                if run.pid.load(Ordering::Acquire) != 0 { break run; }
            }
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(Duration::from_millis(10));
        };
        // 真实命令仍在运行；暂持组锁使执行线程无法清理，超时不能报成功。
        let held = run.group.lock();
        let pid = run.pid.load(Ordering::Acquire);
        assert_eq!(shutdown_checked(Duration::from_millis(30)).unwrap_err().code, "CRON_SHUTDOWN_FAILED");
        assert!(platform::process_alive(pid));
        assert_eq!(run_job(&store, &job.id, true).unwrap_err().code, "CRON_SHUTDOWN");
        drop(held);
        resume_after_shutdown();
        let state = crate::CoreState::init(Some(store.path.parent().unwrap().to_path_buf()), Arc::new(|_| {})).unwrap();
        let guard = crate::AuxiliaryShutdown::prepare().unwrap();
        assert!(!platform::process_alive(pid));
        assert_eq!(worker.join().unwrap().unwrap().last_exit.as_deref(), Some("cancelled"));
        assert_eq!(store.get_cron_job(&job.id).unwrap().unwrap().last_exit.as_deref(), Some("cancelled"));
        assert_eq!(state.start_service("fixture-missing").unwrap_err().code, "APP_BUSY");
        assert!(state.watchdog_tick().is_empty());
        assert!(crate::AuxiliaryShutdown::prepare().is_err());
        // 模拟下一步启动安装器失败或用户取消迁移，释放准备状态后可正常运行新任务。
        drop(guard);
        let failed = (|| -> Result<()> {
            let _guard = crate::AuxiliaryShutdown::prepare()?;
            state.with_stopped_services(|| Err(AppError::new("FIXTURE_LAUNCH_FAILED", "fixture: installer did not start")))
        })();
        assert_eq!(failed.unwrap_err().code, "FIXTURE_LAUNCH_FAILED");
        // 已完成但结果保存失败必须向组合清理流程返回错误，并撤销所有关闭标记。
        let failed_run = run_state();
        failed_run.finished.store(true, Ordering::Release);
        *failed_run.record_error.lock() = Some(AppError::new("FIXTURE_RECORD_FAILED", "fixture: result could not be saved"));
        let failed_key = (store.path.clone(), "failed-result".to_string());
        active().lock().insert(failed_key.clone(), failed_run);
        assert!(matches!(crate::AuxiliaryShutdown::prepare(), Err(error) if error.code=="AUXILIARY_STOP_FAILED"));
        active().lock().remove(&failed_key);
        assert!(!SHUTTING_DOWN.load(Ordering::Acquire));
        let (_next, store, job) = fixture("echo shutdown resumed");
        assert_eq!(run_job(&store, &job.id, true).unwrap().last_exit.as_deref(), Some("exit 0"));
        let mut guard = crate::AuxiliaryShutdown::prepare().unwrap();
        guard.commit();
        drop(guard);
        assert_eq!(run_job(&store, &job.id, true).unwrap_err().code, "CRON_SHUTDOWN");
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
        job.command = "echo recovered 中文输出 🦀".into();
        store.save_cron_job(&job, false).unwrap();
        let result = run_job(&store, &job.id, true).unwrap();
        assert_eq!(result.last_exit.as_deref(), Some("exit 0"));
        let output = result.last_output.unwrap();
        assert!(output.contains("recovered 中文输出 🦀"), "{output}");
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
