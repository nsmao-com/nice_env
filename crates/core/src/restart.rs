//! 有界的本机重启交接：只有指定子进程完成初始化并确认接管，父进程才退出。

use crate::error::{AppError, Result};
use serde::{Deserialize, Serialize};
use std::{
    io::{Read, Write},
    net::{SocketAddr, TcpListener, TcpStream},
    path::{Path, PathBuf},
    process::{Child, Command},
    sync::{Condvar, Mutex},
    time::{Duration, Instant},
};

const ENV: &str = "NSB_RESTART_HANDSHAKE";
const MAX_FRAME: usize = 64 * 1024;

/// 后台线程可先创建，交接提交之前不执行用户任务；失败时唤醒并结束等待者。
#[derive(Default)]
pub struct StartupGate {
    state: Mutex<u8>,
    changed: Condvar,
}
impl StartupGate {
    pub fn start(&self) {
        self.finish(1);
    }
    pub fn cancel(&self) {
        self.finish(2);
    }
    fn finish(&self, next: u8) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if *state == 0 {
            *state = next;
            self.changed.notify_all();
        }
    }
    pub fn wait(&self) -> bool {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        while *state == 0 {
            state = self.changed.wait(state).unwrap_or_else(|e| e.into_inner());
        }
        *state == 1
    }
    pub fn wait_timeout(&self, timeout: Duration) -> bool {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let (state, _) = self
            .changed
            .wait_timeout_while(state, timeout, |state| *state == 0)
            .unwrap_or_else(|e| e.into_inner());
        *state == 1
    }
}

#[derive(Serialize, Deserialize)]
struct Descriptor {
    address: SocketAddr,
    token: String,
    target: PathBuf,
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
enum Frame {
    Hello { token: String, pid: u32 },
    Initialize,
    Ready,
    Failed { error: AppError },
    Commit,
    Activated,
    Start,
}

fn protocol(message: &str) -> AppError {
    AppError::new("RESTART_HANDSHAKE", message)
}
fn write_frame(stream: &mut TcpStream, frame: &Frame) -> Result<()> {
    let mut bytes = serde_json::to_vec(frame)
        .map_err(|e| AppError::internal("编码启动交接消息", e.to_string()))?;
    if bytes.len() >= MAX_FRAME {
        return Err(protocol("启动交接消息超过大小限制"));
    }
    bytes.push(b'\n');
    stream
        .write_all(&bytes)
        .map_err(|e| AppError::io("发送启动交接消息", e))
}

fn read_frame(
    stream: &mut TcpStream,
    deadline: Instant,
    mut check_child: impl FnMut() -> Result<()>,
) -> Result<Frame> {
    let mut bytes = Vec::new();
    loop {
        check_child()?;
        if Instant::now() >= deadline {
            return Err(AppError::new(
                "RESTART_TIMEOUT",
                "新进程未在限定时间内完成启动交接",
            ));
        }
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) => return Err(protocol("新进程在完成启动交接前断开连接")),
            Ok(_) if byte[0] == b'\n' => {
                return serde_json::from_slice(&bytes)
                    .map_err(|e| AppError::internal("读取启动交接消息", e.to_string()))
            }
            Ok(_) => {
                bytes.push(byte[0]);
                if bytes.len() >= MAX_FRAME {
                    return Err(protocol("启动交接消息超过大小限制"));
                }
            }
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::Interrupted
                ) => {}
            Err(error) => return Err(AppError::io("接收启动交接消息", error)),
        }
    }
}

fn configure_stream(stream: &TcpStream) -> Result<()> {
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;
    stream.set_write_timeout(Some(Duration::from_secs(2)))?;
    Ok(())
}

fn child_alive(child: &mut Child) -> Result<()> {
    if let Some(status) = child
        .try_wait()
        .map_err(|e| AppError::io("检查新进程", e))?
    {
        return Err(AppError::new(
            "RESTART_CHILD_EXITED",
            format!("新进程在完成初始化前退出（{status}）"),
        ));
    }
    Ok(())
}

/// 只管理这次创建的子进程及其子树，不按名称或端口寻找其它应用进程。
pub fn launch_and_wait(command: &mut Command, target: &Path, timeout: Duration) -> Result<()> {
    let listener =
        TcpListener::bind(("127.0.0.1", 0)).map_err(|e| AppError::io("创建启动交接通道", e))?;
    listener.set_nonblocking(true)?;
    let descriptor = Descriptor {
        address: listener.local_addr()?,
        token: hex::encode(rand::random::<[u8; 32]>()),
        target: std::fs::canonicalize(target)?,
    };
    command.env(
        ENV,
        serde_json::to_string(&descriptor)
            .map_err(|e| AppError::internal("准备启动交接", e.to_string()))?,
    );
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        unsafe {
            command.pre_exec(|| platform::spawn_pre_exec());
        }
    }
    let mut group = platform::ProcessGroup::new_detached(true).map_err(AppError::from)?;
    let mut child = command
        .spawn()
        .map_err(|e| AppError::io("启动新的 NiceEnv 进程", e))?;
    let attached = group.attach(child.id()).map_err(AppError::from);
    let attached_ok = attached.is_ok();
    let deadline = Instant::now() + timeout;
    let result = attached.and_then(|_| {
        let mut stream = loop {
            if Instant::now() >= deadline {
                return Err(AppError::new("RESTART_TIMEOUT", "新进程未连接启动交接通道"));
            }
            match listener.accept() {
                Ok((mut stream, _)) => {
                    configure_stream(&stream)?;
                    let hello_deadline = deadline.min(Instant::now() + Duration::from_secs(2));
                    match read_frame(&mut stream, hello_deadline, || Ok(())) {
                        Ok(Frame::Hello { token, pid })
                            if token == descriptor.token && pid == child.id() =>
                        {
                            break stream
                        }
                        _ => continue,
                    }
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    child_alive(&mut child)?;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(error) => return Err(AppError::io("等待新进程连接", error)),
            }
        };
        // 子进程在此之前不创建 WebView 或其它子进程，避免 Windows Job 关联竞态。
        write_frame(&mut stream, &Frame::Initialize)?;
        match read_frame(&mut stream, deadline, || Ok(()))? {
            Frame::Ready => {}
            Frame::Failed { error } => return Err(error),
            _ => return Err(protocol("新进程返回了无效的初始化状态")),
        }
        child_alive(&mut child)?;
        write_frame(&mut stream, &Frame::Commit)?;
        match read_frame(&mut stream, deadline, || Ok(()))? {
            Frame::Activated => {
                child_alive(&mut child)?;
                // 此后不再进行可失败的父端步骤；子端收到 Start 才放行用户任务。
                write_frame(&mut stream, &Frame::Start)
            }
            Frame::Failed { error } => Err(error),
            _ => Err(protocol("新进程未确认启动交接")),
        }
    });
    if let Err(error) = result {
        // 先确认子进程已退出，再允许调用方回滚目录选择和 PATH。
        let group_result = group.terminate(true);
        let _ = child.kill();
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            match child.try_wait() {
                Ok(Some(_)) if attached_ok => {
                    #[cfg(windows)]
                    if group_result.is_err() {
                        break;
                    }
                    #[cfg(unix)]
                    if !platform::process_group_gone(child.id()).unwrap_or(false) {
                        if Instant::now() >= deadline {
                            break;
                        }
                        std::thread::sleep(Duration::from_millis(20));
                        continue;
                    }
                    // Unix 已独立确认整个组不再运行；退出竞态导致的信号错误不应阻止回滚。
                    return Err(error);
                }
                Ok(Some(_)) => break,
                _ if Instant::now() >= deadline => break,
                _ => std::thread::sleep(Duration::from_millis(20)),
            }
        }
        return Err(AppError::new(
            "RESTART_CHILD_CLEANUP_FAILED",
            "新进程启动失败，但未能确认其子进程全部退出；未恢复旧目录操作",
        )
        .with_hint("请先关闭本次新开的 NiceEnv 进程，再重新打开应用")
        .with_detail(format!(
            "{}；{}",
            error.message,
            group_result
                .err()
                .map(|e| e.to_string())
                .unwrap_or_default()
        )));
    }
    // 成功交接后父进程可能仍短暂存活；丢弃 Child 不会在 Unix 上回收子进程。
    std::thread::spawn(move || {
        let _ = child.wait();
    });
    Ok(())
}

pub struct RestartChild {
    stream: TcpStream,
    target: PathBuf,
}
impl RestartChild {
    pub fn from_env() -> Result<Option<Self>> {
        let Some(raw) = std::env::var_os(ENV) else {
            return Ok(None);
        };
        // 不能让接管后启动的用户服务继承父子交接凭据。
        std::env::remove_var(ENV);
        let descriptor: Descriptor = serde_json::from_str(&raw.to_string_lossy())
            .map_err(|e| AppError::internal("读取启动交接参数", e.to_string()))?;
        if !descriptor.address.ip().is_loopback()
            || descriptor.address.port() == 0
            || descriptor.token.len() != 64
            || !descriptor.token.bytes().all(|c| c.is_ascii_hexdigit())
            || !descriptor.target.is_absolute()
        {
            return Err(protocol("启动交接参数无效"));
        }
        let mut stream = TcpStream::connect_timeout(&descriptor.address, Duration::from_secs(3))
            .map_err(|e| AppError::io("连接原应用的启动交接通道", e))?;
        configure_stream(&stream)?;
        write_frame(
            &mut stream,
            &Frame::Hello {
                token: descriptor.token,
                pid: std::process::id(),
            },
        )?;
        if !matches!(
            read_frame(
                &mut stream,
                Instant::now() + Duration::from_secs(10),
                || Ok(())
            )?,
            Frame::Initialize
        ) {
            return Err(protocol("原应用未确认进程组已就绪，已停止新进程"));
        }
        Ok(Some(Self {
            stream,
            target: descriptor.target,
        }))
    }

    pub fn verify_path(&self, actual: &Path) -> Result<()> {
        if std::fs::canonicalize(actual)? != self.target {
            return Err(protocol("新进程打开的数据目录与迁移目标不一致"));
        }
        Ok(())
    }

    pub fn fail(&mut self, error: AppError) {
        let _ = write_frame(&mut self.stream, &Frame::Failed { error });
    }

    pub fn ready(mut self, activate_window: impl FnOnce() -> Result<()>) -> Result<()> {
        write_frame(&mut self.stream, &Frame::Ready)?;
        match read_frame(
            &mut self.stream,
            Instant::now() + Duration::from_secs(10),
            || Ok(()),
        )? {
            Frame::Commit => {}
            _ => return Err(protocol("原应用未确认启动交接，已停止新进程")),
        }
        if let Err(error) = activate_window() {
            self.fail(error.clone());
            return Err(error);
        }
        write_frame(&mut self.stream, &Frame::Activated)?;
        match read_frame(
            &mut self.stream,
            Instant::now() + Duration::from_secs(10),
            || Ok(()),
        )? {
            Frame::Start => Ok(()),
            _ => Err(protocol("原应用未完成启动交接，未启动后台任务")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn wait_gone(pid: u32) {
        let deadline = Instant::now() + Duration::from_secs(5);
        while platform::process_alive(pid) && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            !platform::process_alive(pid),
            "fixture process {pid} still running"
        );
    }

    fn probe(mode: &str) -> (tempfile::TempDir, Result<()>) {
        let temp = tempfile::tempdir().unwrap();
        let mut command = platform::command(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "restart::tests::restart_child_probe",
                "--nocapture",
            ])
            .env("NSB_RESTART_PROBE_MODE", mode)
            .env("NSB_RESTART_PROBE_ROOT", temp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());
        let timeout = if mode == "hang" {
            Duration::from_secs(2)
        } else {
            Duration::from_secs(8)
        };
        let result = launch_and_wait(&mut command, temp.path(), timeout);
        if result.is_ok() {
            std::fs::write(temp.path().join("parent-done"), "done").unwrap();
        }
        let pid: u32 = std::fs::read_to_string(temp.path().join("child-pid"))
            .unwrap()
            .parse()
            .unwrap();
        wait_gone(pid);
        if mode == "tree_fail" {
            let descendant = std::fs::read_to_string(temp.path().join("descendant-pid")).unwrap();
            wait_gone(descendant.parse().unwrap());
        }
        (temp, result)
    }

    #[test]
    fn restart_child_probe() {
        let Ok(mode) = std::env::var("NSB_RESTART_PROBE_MODE") else {
            return;
        };
        let root = PathBuf::from(std::env::var_os("NSB_RESTART_PROBE_ROOT").unwrap());
        if mode == "descendant" {
            std::fs::write(root.join("descendant-pid"), std::process::id().to_string()).unwrap();
            std::thread::sleep(Duration::from_secs(5));
            return;
        }
        std::fs::write(root.join("child-pid"), std::process::id().to_string()).unwrap();
        if mode == "spoof_then_ready" {
            let descriptor: Descriptor =
                serde_json::from_str(&std::env::var(ENV).unwrap()).unwrap();
            let mut impostor = TcpStream::connect(descriptor.address).unwrap();
            configure_stream(&impostor).unwrap();
            write_frame(
                &mut impostor,
                &Frame::Hello {
                    token: "0".repeat(64),
                    pid: std::process::id(),
                },
            )
            .unwrap();
        }
        let mut child = RestartChild::from_env().unwrap().unwrap();
        assert!(std::env::var_os(ENV).is_none());
        child.verify_path(&root).unwrap();
        match mode.as_str() {
            "fail" | "tree_fail" => {
                let _descendant = if mode == "tree_fail" {
                    let descendant = platform::command(std::env::current_exe().unwrap())
                        .args([
                            "--exact",
                            "restart::tests::restart_child_probe",
                            "--nocapture",
                        ])
                        .env("NSB_RESTART_PROBE_MODE", "descendant")
                        .env_remove(ENV)
                        .stdout(std::process::Stdio::null())
                        .stderr(std::process::Stdio::null())
                        .spawn()
                        .unwrap();
                    let deadline = Instant::now() + Duration::from_secs(2);
                    while !root.join("descendant-pid").exists() && Instant::now() < deadline {
                        std::thread::sleep(Duration::from_millis(20));
                    }
                    assert!(root.join("descendant-pid").exists());
                    Some(descendant)
                } else {
                    None
                };
                child.fail(AppError::new(
                    "FIXTURE_INIT_FAILED",
                    "temporary initialization failed",
                ));
                return;
            }
            "exit" => std::process::exit(17),
            "hang" => {
                std::thread::sleep(Duration::from_secs(5));
                return;
            }
            _ => {}
        }
        let state = crate::CoreState::init(Some(root.clone()), Arc::new(|_| {})).unwrap();
        child.verify_path(&state.paths.base).unwrap();
        if mode == "ready_late" {
            std::thread::sleep(Duration::from_millis(200));
        }
        let gate = Arc::new(StartupGate::default());
        let worker_gate = gate.clone();
        let worker_root = root.clone();
        let worker = std::thread::spawn(move || {
            if worker_gate.wait() {
                std::fs::write(worker_root.join("user-task"), "ran after commit").unwrap();
            }
        });
        let result = child.ready(|| {
            assert!(!root.join("user-task").exists());
            if mode == "window_fail" {
                return Err(AppError::new(
                    "FIXTURE_WINDOW_FAILED",
                    "temporary window failure",
                ));
            }
            std::fs::write(root.join("window-ready"), "shown").unwrap();
            Ok(())
        });
        if result.is_err() {
            gate.cancel();
            worker.join().unwrap();
            return;
        }
        gate.start();
        worker.join().unwrap();
        let deadline = Instant::now() + Duration::from_secs(5);
        while !root.join("parent-done").exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    #[test]
    fn restart_waits_for_ready_and_confirmation_and_rejects_wrong_token() {
        for mode in ["ready", "ready_late", "spoof_then_ready"] {
            let (temp, result) = probe(mode);
            result.unwrap();
            assert!(temp.path().join("window-ready").exists());
            assert!(temp.path().join("user-task").exists());
        }
    }

    #[test]
    fn restart_reports_initialization_failure_exit_and_timeout_after_cleanup() {
        for (mode, code) in [
            ("fail", "FIXTURE_INIT_FAILED"),
            ("tree_fail", "FIXTURE_INIT_FAILED"),
            ("window_fail", "FIXTURE_WINDOW_FAILED"),
            ("hang", "RESTART_TIMEOUT"),
        ] {
            let (temp, result) = probe(mode);
            assert_eq!(result.unwrap_err().code, code, "mode {mode}");
            assert!(!temp.path().join("user-task").exists());
        }
        let (_, result) = probe("exit");
        assert!(result.is_err());
    }

    #[test]
    fn restart_parent_disconnect_never_activates_child() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let stream = TcpStream::connect(address).unwrap();
            configure_stream(&stream).unwrap();
            RestartChild {
                stream,
                target: PathBuf::new(),
            }
            .ready(|| panic!("parent did not commit"))
        });
        let (mut stream, _) = listener.accept().unwrap();
        configure_stream(&stream).unwrap();
        assert!(matches!(
            read_frame(&mut stream, Instant::now() + Duration::from_secs(2), || Ok(
                ()
            ))
            .unwrap(),
            Frame::Ready
        ));
        drop(stream);
        assert!(worker.join().unwrap().is_err());
        let gate = Arc::new(StartupGate::default());
        let waiting = gate.clone();
        let task = std::thread::spawn(move || waiting.wait());
        gate.cancel();
        gate.start();
        assert!(!task.join().unwrap());
        let late = StartupGate::default();
        assert!(!late.wait_timeout(Duration::from_millis(5)));
        late.start();
        assert!(late.wait_timeout(Duration::from_millis(5)));

        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let address = listener.local_addr().unwrap();
        let worker = std::thread::spawn(move || {
            let stream = TcpStream::connect(address).unwrap();
            configure_stream(&stream).unwrap();
            // 窗口可以显示，但没有最终 Start 时不得启动用户任务。
            RestartChild {
                stream,
                target: PathBuf::new(),
            }
            .ready(|| Ok(()))
        });
        let (mut stream, _) = listener.accept().unwrap();
        configure_stream(&stream).unwrap();
        let deadline = Instant::now() + Duration::from_secs(2);
        assert!(matches!(
            read_frame(&mut stream, deadline, || Ok(())).unwrap(),
            Frame::Ready
        ));
        write_frame(&mut stream, &Frame::Commit).unwrap();
        assert!(matches!(
            read_frame(&mut stream, deadline, || Ok(())).unwrap(),
            Frame::Activated
        ));
        drop(stream);
        assert!(worker.join().unwrap().is_err());
    }
}
