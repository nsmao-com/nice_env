//! 平台差异层：Windows Job Object / 进程树管理、系统代理、hosts 写入、提权执行、
//! 运行时 bin 目录注入系统 PATH。
//! core 不直接依赖本 crate 的平台 API，统一走这里的封装。

use thiserror::Error;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub mod pathenv;

/// 与 Nginx include 一致的系统 glob：Unix 使用 glob(3)，Windows 使用 FindFirstFileW。
/// 只展开名称，不读取或修改匹配文件；无匹配不是错误。
pub fn config_glob(pattern: &str) -> std::io::Result<Vec<std::path::PathBuf>> {
    use std::io;
    #[cfg(unix)]
    {
        use std::{
            ffi::{CStr, CString, OsStr},
            os::unix::ffi::OsStrExt,
            path::PathBuf,
        };
        let pattern = CString::new(pattern)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "glob contains NUL"))?;
        struct Glob(libc::glob_t);
        impl Drop for Glob {
            fn drop(&mut self) {
                unsafe {
                    libc::globfree(&mut self.0);
                }
            }
        }
        // SAFETY: glob_t 由 glob 初始化；guard 在所有返回路径释放系统分配的名称。
        let mut result = Glob(unsafe { std::mem::zeroed() });
        let status = unsafe { libc::glob(pattern.as_ptr(), 0, None, &mut result.0) };
        if status == libc::GLOB_NOMATCH {
            return Ok(Vec::new());
        }
        if status != 0 {
            return Err(io::Error::other(format!("glob failed ({status})")));
        }
        let mut paths = Vec::new();
        for index in 0..result.0.gl_pathc {
            // SAFETY: glob 返回 gl_pathc 个以 NUL 结尾的文件名。
            let name = unsafe { CStr::from_ptr(*result.0.gl_pathv.add(index as usize)) };
            paths.push(PathBuf::from(OsStr::from_bytes(name.to_bytes())));
        }
        Ok(paths)
    }
    #[cfg(windows)]
    {
        use std::{ffi::OsString, os::windows::ffi::OsStringExt, path::Path};
        use windows_sys::Win32::{
            Foundation::{
                ERROR_FILE_NOT_FOUND, ERROR_NO_MORE_FILES, ERROR_PATH_NOT_FOUND, HANDLE,
                INVALID_HANDLE_VALUE,
            },
            Storage::FileSystem::{FindClose, FindFirstFileW, FindNextFileW, WIN32_FIND_DATAW},
        };
        if pattern.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "glob contains NUL",
            ));
        }
        let wide: Vec<u16> = pattern.encode_utf16().chain(Some(0)).collect();
        let mut data: WIN32_FIND_DATAW = unsafe { std::mem::zeroed() };
        let handle = unsafe { FindFirstFileW(wide.as_ptr(), &mut data) };
        if handle == INVALID_HANDLE_VALUE {
            let error = io::Error::last_os_error();
            return if matches!(
                error.raw_os_error().map(|code| code as u32),
                Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
            ) {
                Ok(Vec::new())
            } else {
                Err(error)
            };
        }
        struct Find(HANDLE);
        impl Drop for Find {
            fn drop(&mut self) {
                unsafe {
                    FindClose(self.0);
                }
            }
        }
        let handle = Find(handle);
        let parent = Path::new(pattern)
            .parent()
            .unwrap_or_else(|| Path::new("."));
        let mut paths = Vec::new();
        loop {
            let length = data
                .cFileName
                .iter()
                .position(|unit| *unit == 0)
                .unwrap_or(data.cFileName.len());
            paths.push(parent.join(OsString::from_wide(&data.cFileName[..length])));
            if unsafe { FindNextFileW(handle.0, &mut data) } == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == Some(ERROR_NO_MORE_FILES as i32) {
                    break;
                }
                return Err(error);
            }
        }
        Ok(paths)
    }
}

/// 本机控制通道凭据仅允许文件所有者与系统读取；写入凭据前调用。
pub fn restrict_file_to_owner(path: &std::path::Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600)).map_err(io_err)
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        use windows_sys::Win32::{Foundation::LocalFree, Security::{
            Authorization::{ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1},
            SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        }};
        let name: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
        // OW 是文件实际所有者，避免用户名解析、继承的 Everyone/Users 读取权限。
        let sddl: Vec<u16> = "D:P(A;;FA;;;SY)(A;;FA;;;OW)".encode_utf16().chain(Some(0)).collect();
        unsafe {
            let mut descriptor = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(sddl.as_ptr(), SDDL_REVISION_1, &mut descriptor, std::ptr::null_mut()) == 0 {
                return Err(io_err(std::io::Error::last_os_error()));
            }
            let result = SetFileSecurityW(name.as_ptr(), DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION, descriptor);
            let error = (result == 0).then(std::io::Error::last_os_error);
            LocalFree(descriptor);
            error.map_or(Ok(()), |error| Err(io_err(error)))
        }
    }
}

#[derive(Error, Debug)]
pub enum PlatformError {
    #[error("{0}")]
    Io(String),
    #[error("不支持的平台操作: {0}")]
    Unsupported(String),
    #[error("{0}")]
    Win(String),
}

pub type Result<T> = std::result::Result<T, PlatformError>;

fn io_err(e: std::io::Error) -> PlatformError {
    PlatformError::Io(e.to_string())
}

/* ================= 子进程 ================= */

/// Windows 下 CreateProcess 的 CREATE_NO_WINDOW
#[cfg(windows)]
pub const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// 创建一个不弹控制台窗口的子进程命令。
///
/// 桌面端是 GUI 进程，直接拉起控制台程序（php -m / nginx -t / mysqld --initialize /
/// certutil / netstat / tar …）时 Windows 会给子进程新开一个黑框，表现为界面上
/// 窗口频繁闪烁。所有后台探测、初始化、导入导出类调用都应走这里；
/// 真的需要给用户看到窗口的（打开终端等）才直接用 `std::process::Command`。
pub fn command<S: AsRef<std::ffi::OsStr>>(program: S) -> std::process::Command {
    #[allow(unused_mut)]
    let mut cmd = std::process::Command::new(program);
    #[cfg(windows)]
    cmd.creation_flags(CREATE_NO_WINDOW);
    cmd
}

/// 外部命令优先按 UTF-8 读取；Windows 传统 shell 输出使用系统 OEM 代码页。
pub fn decode_command_output(bytes: &[u8]) -> String {
    match std::str::from_utf8(bytes) {
        Ok(text) => return text.to_string(),
        // 截断发生在 UTF-8 尾字符中间时，保留其余有效文本。
        Err(e) if e.error_len().is_none() => return String::from_utf8_lossy(bytes).into_owned(),
        Err(_) => {}
    }
    #[cfg(windows)]
    if let Ok(length) = i32::try_from(bytes.len()) {
        use windows_sys::Win32::Globalization::{MultiByteToWideChar, CP_OEMCP};
        unsafe {
            let needed =
                MultiByteToWideChar(CP_OEMCP, 0, bytes.as_ptr(), length, std::ptr::null_mut(), 0);
            if needed > 0 {
                let mut units = vec![0u16; needed as usize];
                let written = MultiByteToWideChar(
                    CP_OEMCP,
                    0,
                    bytes.as_ptr(),
                    length,
                    units.as_mut_ptr(),
                    needed,
                );
                if written > 0 {
                    return String::from_utf16_lossy(&units[..written as usize]);
                }
            }
        }
    }
    String::from_utf8_lossy(bytes).into_owned()
}

/* ================= 进程树管理 ================= */

/// PID 会被复用，恢复与停机必须同时核对内核提供的创建标识。
/// 保留原生精度，不使用 sysinfo 按秒取整的 start_time。
pub fn process_start_marker(pid: u32) -> Option<String> {
    if pid == 0 || pid > i32::MAX as u32 {
        return None;
    }
    #[cfg(windows)]
    unsafe {
        use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
        use windows_sys::Win32::System::Threading::{
            GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
        };
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return None;
        }
        let mut created: FILETIME = std::mem::zeroed();
        let mut exited: FILETIME = std::mem::zeroed();
        let mut kernel: FILETIME = std::mem::zeroed();
        let mut user: FILETIME = std::mem::zeroed();
        let ok = GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user);
        CloseHandle(handle);
        return (ok != 0).then(|| {
            format!(
                "win:{}",
                ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64
            )
        });
    }
    #[cfg(target_os = "linux")]
    {
        let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        // comm 字段可含空格和括号；其后的第 20 项是 starttime（字段 22）。
        let tail = stat.get(stat.rfind(')')? + 1..)?;
        let ticks: u64 = tail.split_whitespace().nth(19)?.parse().ok()?;
        let boot = std::fs::read_to_string("/proc/sys/kernel/random/boot_id").ok()?;
        return Some(format!("linux:{}:{ticks}", boot.trim()));
    }
    #[cfg(target_os = "macos")]
    unsafe {
        let mut info: libc::proc_bsdinfo = std::mem::zeroed();
        let size = std::mem::size_of_val(&info) as libc::c_int;
        if libc::proc_pidinfo(
            pid as libc::c_int,
            libc::PROC_PIDTBSDINFO,
            1, // 包含尚未回收的 zombie，保留退出阶段的创建时间身份。
            &mut info as *mut _ as *mut libc::c_void,
            size,
        ) != size
        {
            return None;
        }
        return Some(format!(
            "mac:{}:{}",
            info.pbi_start_tvsec, info.pbi_start_tvusec
        ));
    }
    #[cfg(not(any(windows, target_os = "linux", target_os = "macos")))]
    None
}

/// 对历史 PID 的终止使用固定的进程对象，避免检查后 PID 被复用。
pub struct VerifiedProcess {
    #[cfg(windows)]
    handle: windows_sys::Win32::Foundation::HANDLE,
    #[cfg(target_os = "linux")]
    fd: std::os::fd::OwnedFd,
    #[cfg(not(any(windows, target_os = "linux")))]
    pid: u32,
    #[cfg(not(any(windows, target_os = "linux")))]
    started: String,
}

#[cfg(not(any(windows, target_os = "linux")))]
fn confirmed_process_matches(pid: u32, started: &str) -> Result<bool> {
    // macOS 的退出过渡期可能先从普通进程表消失、稍后才出现在 zombie 表。
    // 此间不能发送信号，也不能仅凭读取失败就当成退出；有界重试后仍未知则报错。
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(1);
    loop {
        match process_start_marker(pid) {
            Some(marker) => return Ok(marker == started && process_alive(pid)),
            None if !process_alive(pid) => return Ok(false),
            None => {}
        }
        if std::time::Instant::now() >= deadline {
            return Err(PlatformError::Io(format!(
                "无法核实进程 {pid} 的身份，未发送信号"
            )));
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
}

impl VerifiedProcess {
    /// None 表示原进程已退出或 PID 已复用；无法读取身份则返回错误。
    pub fn open(pid: u32, started: &str) -> Result<Option<Self>> {
        if pid <= 4 || pid > i32::MAX as u32 || pid == std::process::id() || started.is_empty() {
            return Err(PlatformError::Io(
                "不能结束系统进程、当前进程或身份不明的进程".into(),
            ));
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::Foundation::{
                CloseHandle, ERROR_INVALID_PARAMETER, FILETIME, WAIT_OBJECT_0,
            };
            use windows_sys::Win32::System::Threading::{
                GetProcessTimes, OpenProcess, WaitForSingleObject,
                PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SYNCHRONIZE, PROCESS_TERMINATE,
            };
            let handle = OpenProcess(
                PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_TERMINATE | PROCESS_SYNCHRONIZE,
                0,
                pid,
            );
            if handle.is_null() {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) {
                    Ok(None)
                } else {
                    Err(io_err(error))
                };
            }
            let mut created: FILETIME = std::mem::zeroed();
            let mut exited: FILETIME = std::mem::zeroed();
            let mut kernel: FILETIME = std::mem::zeroed();
            let mut user: FILETIME = std::mem::zeroed();
            if GetProcessTimes(handle, &mut created, &mut exited, &mut kernel, &mut user) == 0 {
                let error = std::io::Error::last_os_error();
                CloseHandle(handle);
                return Err(io_err(error));
            }
            let marker = format!(
                "win:{}",
                ((created.dwHighDateTime as u64) << 32) | created.dwLowDateTime as u64
            );
            // 未退出进程的 exit time 未定义，必须等待进程对象判断存活。
            if marker != started || WaitForSingleObject(handle, 0) == WAIT_OBJECT_0 {
                CloseHandle(handle);
                return Ok(None);
            }
            return Ok(Some(Self { handle }));
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::FromRawFd;
            let raw = unsafe { libc::syscall(libc::SYS_pidfd_open, pid, 0) };
            if raw < 0 {
                let error = std::io::Error::last_os_error();
                return if error.raw_os_error() == Some(libc::ESRCH) {
                    Ok(None)
                } else {
                    Err(io_err(error))
                };
            }
            let fd = unsafe { std::os::fd::OwnedFd::from_raw_fd(raw as i32) };
            return match process_start_marker(pid) {
                Some(marker) if marker == started => Ok(Some(Self { fd })),
                Some(_) => Ok(None),
                None if !process_alive(pid) => Ok(None),
                None => Err(PlatformError::Io(format!("无法核实进程 {pid} 的身份"))),
            };
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            Ok(confirmed_process_matches(pid, started)?.then(|| Self {
                pid,
                started: started.to_string(),
            }))
        }
    }

    /// MongoDB 的正常停机：Windows 使用上游监听的命名事件，Unix 只发送 SIGTERM。
    /// 这里只请求正常退出；调用者等待结束，失败或超时不能自动升级为强杀。
    pub fn request_mongodb_shutdown(&self) -> Result<()> {
        if self.has_exited()? {
            return Ok(());
        }
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::Foundation::CloseHandle;
            use windows_sys::Win32::System::Threading::{
                GetProcessId, OpenEventW, SetEvent, EVENT_MODIFY_STATE,
            };
            // MongoDB signal_win32.cpp / eventProcessingThread 使用 Global\Mongo_<pid>。
            // 持有已核实的进程句柄，避免退出后 PID 被复用；不创建不存在的事件。
            let pid = GetProcessId(self.handle);
            if pid == 0 {
                return Err(io_err(std::io::Error::last_os_error()));
            }
            let name: Vec<u16> = format!("Global\\Mongo_{pid}")
                .encode_utf16()
                .chain(Some(0))
                .collect();
            let event = OpenEventW(EVENT_MODIFY_STATE, 0, name.as_ptr());
            if event.is_null() {
                let error = std::io::Error::last_os_error();
                return if self.has_exited()? {
                    Ok(())
                } else {
                    Err(io_err(error))
                };
            }
            let result = (|| {
                if self.has_exited()? {
                    return Ok(());
                }
                if SetEvent(event) == 0 {
                    return Err(io_err(std::io::Error::last_os_error()));
                }
                Ok(())
            })();
            CloseHandle(event);
            return result;
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    libc::SIGTERM,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(io_err(error));
                }
            }
            Ok(())
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            if !confirmed_process_matches(self.pid, &self.started)? {
                return Ok(());
            }
            if unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGTERM) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(io_err(error));
                }
            }
            Ok(())
        }
    }

    pub fn terminate(&self) -> Result<()> {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
            use windows_sys::Win32::System::Threading::{TerminateProcess, WaitForSingleObject};
            if WaitForSingleObject(self.handle, 0) == WAIT_OBJECT_0 {
                return Ok(());
            }
            if TerminateProcess(self.handle, 1) == 0 {
                let error = std::io::Error::last_os_error();
                // 父进程退出可能已带动此子进程进入终止阶段，此时会返回 ACCESS_DENIED。
                if WaitForSingleObject(self.handle, 5000) != WAIT_OBJECT_0 {
                    return Err(io_err(error));
                }
            }
            if WaitForSingleObject(self.handle, 5000) != WAIT_OBJECT_0 {
                return Err(PlatformError::Io("等待已确认进程退出超时".into()));
            }
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            if unsafe {
                libc::syscall(
                    libc::SYS_pidfd_send_signal,
                    self.fd.as_raw_fd(),
                    libc::SIGKILL,
                    std::ptr::null::<libc::siginfo_t>(),
                    0,
                )
            } != 0
            {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(io_err(error));
                }
            }
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        {
            if !confirmed_process_matches(self.pid, &self.started)? {
                return Ok(());
            }
            if unsafe { libc::kill(self.pid as libc::pid_t, libc::SIGKILL) } != 0 {
                let error = std::io::Error::last_os_error();
                if error.raw_os_error() != Some(libc::ESRCH) {
                    return Err(io_err(error));
                }
            }
        }
        Ok(())
    }

    pub fn has_exited(&self) -> Result<bool> {
        #[cfg(windows)]
        unsafe {
            use windows_sys::Win32::Foundation::{WAIT_OBJECT_0, WAIT_TIMEOUT};
            return match windows_sys::Win32::System::Threading::WaitForSingleObject(self.handle, 0)
            {
                WAIT_OBJECT_0 => Ok(true),
                WAIT_TIMEOUT => Ok(false),
                _ => Err(io_err(std::io::Error::last_os_error())),
            };
        }
        #[cfg(target_os = "linux")]
        {
            use std::os::fd::AsRawFd;
            let mut descriptor = libc::pollfd {
                fd: self.fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            if unsafe { libc::poll(&mut descriptor, 1, 0) } < 0 {
                return Err(io_err(std::io::Error::last_os_error()));
            }
            return Ok(descriptor.revents & libc::POLLIN != 0);
        }
        #[cfg(not(any(windows, target_os = "linux")))]
        match process_start_marker(self.pid) {
            Some(marker) => Ok(marker != self.started || !process_alive(self.pid)),
            None if !process_alive(self.pid) => Ok(true),
            // macOS 在终止到进入 zombie 之间可能暂时读不到身份；由调用方的有界等待重试。
            // 无法核实时保守地视作尚未退出，不能把它当成停机成功。
            None => Ok(false),
        }
    }
}

#[cfg(windows)]
impl Drop for VerifiedProcess {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

/// 进程组句柄：Windows=Job Object(KILL_ON_JOB_CLOSE)，Unix=记录 pid 集合。
pub struct ProcessGroup {
    #[cfg(windows)]
    job: Option<windows_job::JobObject>,
    pids: Vec<u32>,
}

impl ProcessGroup {
    /// 恢复已确认属于本应用的 pid；没有 Job 句柄时按 pid 终止进程树。
    pub fn from_pids(pids: Vec<u32>) -> Self {
        Self {
            #[cfg(windows)]
            job: None,
            pids,
        }
    }

    /// 创建进程组（尚未包含任何进程）。
    /// `detached=true`：Windows 上用不带 KILL_ON_JOB_CLOSE 的 Job——
    /// 进程树仍可被整体终止（stop 正常工作），但创建者退出时服务继续存活
    /// （nsbctl CLI 启动的服务不随 CLI 退出而被杀）。
    pub fn new_detached(detached: bool) -> Result<Self> {
        #[cfg(windows)]
        {
            let job = if detached {
                windows_job::JobObject::create_detached()
            } else {
                windows_job::JobObject::create_kill_on_close()
            }
            .map_err(|e| PlatformError::Win(format!("CreateJobObject 失败: {e}")))?;
            Ok(Self {
                job: Some(job),
                pids: Vec::new(),
            })
        }
        #[cfg(not(windows))]
        {
            let _ = detached;
            Ok(Self { pids: Vec::new() })
        }
    }

    /// 常规创建（随创建者退出而终止整组）
    pub fn new() -> Result<Self> {
        Self::new_detached(false)
    }

    /// 把已启动的子进程加入组（Windows 下必须尽快调用，防孤儿）
    pub fn attach(&mut self, pid: u32) -> Result<()> {
        #[cfg(windows)]
        {
            if let Some(job) = &self.job {
                windows_job::assign_process(job, pid).map_err(|e| {
                    PlatformError::Win(format!("AssignProcessToJobObject({pid}) 失败: {e}"))
                })?;
            }
        }
        let _ = pid;
        self.pids.push(pid);
        Ok(())
    }

    pub fn pids(&self) -> &[u32] {
        &self.pids
    }

    /// 本会话持有的 Job 对象不受 PID 复用影响；其它组必须剔除身份已变化的根进程。
    pub fn retain_roots(&mut self, mut keep: impl FnMut(u32) -> bool) {
        self.pids.retain(|pid| keep(*pid));
    }

    #[cfg(windows)]
    pub fn has_job(&self) -> bool { self.job.is_some() }

    /// 终止整组。force=true 直接 SIGKILL；force=false 先发 SIGTERM。
    /// Unix 下子进程在启动时被置为独立进程组（见 `spawn_pre_exec`），
    /// 因此负 pid 信号能连同其 fork 出的孙子进程一起收走。
    pub fn terminate(&mut self, force: bool) -> Result<()> {
        #[cfg(windows)]
        {
            let _ = force;
            if let Some(job) = &self.job {
                windows_job::terminate_job(job)
                    .map_err(|e| PlatformError::Win(format!("TerminateJobObject 失败: {e}")))?;
            } else {
                // 无 Job 句柄（收养的孤儿/历史组）：按 pid 逐个强杀（含子树）
                for pid in &self.pids {
                    let _ = std::process::Command::new("taskkill")
                        .args(["/PID", &pid.to_string(), "/T", "/F"])
                        .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
                        .output();
                }
            }
        }
        #[cfg(not(windows))]
        {
            let sig = if force { libc::SIGKILL } else { libc::SIGTERM };
            for pid in &self.pids {
                kill_tree(*pid, sig)?;
            }
        }
        self.pids.clear();
        Ok(())
    }
}

/// 确认本次创建的 Unix 进程组已经消失；权限错误不当作已清理。
#[cfg(unix)]
pub fn process_group_gone(pid: u32) -> Result<bool> {
    if pid == 0 || pid > i32::MAX as u32 {
        return Err(PlatformError::Io("无效的进程组 ID".into()));
    }
    let probe = unsafe { libc::kill(-(pid as libc::pid_t), 0) };
    let error = std::io::Error::last_os_error();
    // macOS killpg 会跳过 zombie；组仍存在但只剩 zombie 时返回 EPERM。
    // EPERM 本身不能证明清理成功，仍须完整枚举并核实每个成员均已退出。
    if probe == 0 || (cfg!(target_os = "macos") && error.raw_os_error() == Some(libc::EPERM)) {
        #[cfg(target_os = "linux")]
        {
            // kill(0) 也匹配已退出、等待父进程回收的僵尸；它们已不再运行或占用监听端口。
            // 逐项读取失败时保守地保留进程组，不能把无权读取当成清理成功。
            for entry in std::fs::read_dir("/proc").map_err(io_err)? {
                let entry = entry.map_err(io_err)?;
                if entry.file_name().to_string_lossy().parse::<u32>().is_err() {
                    continue;
                }
                let stat = match std::fs::read_to_string(entry.path().join("stat")) {
                    Ok(stat) => stat,
                    Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                    Err(error) => return Err(io_err(error)),
                };
                let Some(tail) = stat.rfind(')').and_then(|end| stat.get(end + 1..)) else {
                    return Ok(false);
                };
                let mut fields = tail.split_whitespace();
                let state = fields.next();
                // /proc/<pid>/stat：state 后依次是 ppid、pgrp；只关注 pgrp == 目标组。
                let group = fields.nth(1).and_then(|value| value.parse::<u32>().ok());
                if group.is_none() {
                    return Ok(false);
                }
                if group == Some(pid) && !matches!(state, Some("Z" | "X" | "x")) {
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        #[cfg(target_os = "macos")]
        {
            // libproc 同时列出普通进程和 zombie；返回值是 PID 个数，0 也可能表示错误。
            // 完整枚举后检查存活状态，避免把只剩 zombie 的组误判成迁移清理失败。
            let mut members = vec![0 as libc::pid_t; 64];
            loop {
                let count = unsafe {
                    *libc::__error() = 0;
                    libc::proc_listpgrppids(
                        pid as libc::pid_t,
                        members.as_mut_ptr().cast(),
                        (members.len() * std::mem::size_of::<libc::pid_t>()) as libc::c_int,
                    )
                };
                let error = std::io::Error::last_os_error();
                if count < 0 || (count == 0 && error.raw_os_error() != Some(0)) {
                    return Err(io_err(error));
                }
                let count = count as usize;
                if count < members.len() {
                    return Ok(members[..count]
                        .iter()
                        .all(|member| *member > 0 && !process_alive(*member as u32)));
                }
                if members.len() >= 65536 {
                    return Ok(false);
                }
                members.resize(members.len() * 2, 0);
            }
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Ok(false);
    }
    if error.raw_os_error() == Some(libc::ESRCH) {
        Ok(true)
    } else {
        Err(io_err(error))
    }
}

/// Unix：先终止进程组，组不存在时退回单进程。
#[cfg(not(windows))]
fn kill_tree(pid: u32, sig: libc::c_int) -> Result<()> {
    if pid == 0 || pid > i32::MAX as u32 { return Err(PlatformError::Io("无效的进程组 ID".into())); }
    let pid = pid as libc::pid_t;
    unsafe {
        // 负 pid = 整个进程组（子进程启动时 setpgid(0,0)，pgid == pid）
        if libc_kill_group(pid, sig) != 0 {
            let e = std::io::Error::last_os_error();
            // ESRCH：组不存在，可能未被置组，退回杀单个进程
            if e.raw_os_error() == Some(libc::ESRCH) {
                if libc_kill(pid as u32, sig) != 0 {
                    let error = std::io::Error::last_os_error();
                    if error.raw_os_error() != Some(libc::ESRCH) { return Err(io_err(error)); }
                }
            } else {
                return Err(io_err(e));
            }
        }
    }
    Ok(())
}

#[cfg(not(windows))]
unsafe fn libc_kill(pid: u32, sig: i32) -> i32 {
    libc::kill(pid as libc::pid_t, sig)
}

#[cfg(not(windows))]
unsafe fn libc_kill_group(pid: libc::pid_t, sig: libc::c_int) -> i32 {
    libc::kill(-pid, sig)
}

/// Unix：让子进程脱离父进程组（成为新组组长），使整棵子树可被一次性收走。
#[cfg(not(windows))]
pub fn spawn_pre_exec() -> std::io::Result<()> {
    unsafe {
        if libc::setpgid(0, 0) != 0 {
            return Err(std::io::Error::last_os_error());
        }
    }
    Ok(())
}

/// 进程是否仍存活
pub fn process_alive(pid: u32) -> bool {
    if pid == 0 || pid > i32::MAX as u32 { return false; }
    #[cfg(windows)]
    {
        windows_job::process_alive(pid)
    }
    #[cfg(not(windows))]
    {
        #[cfg(target_os = "linux")]
        if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
            if let Some(state) = stat
                .rfind(')')
                .and_then(|end| stat.get(end + 1..))
                .and_then(|tail| tail.split_whitespace().next())
            {
                if matches!(state, "Z" | "X" | "x") {
                    return false;
                }
            }
        }
        #[cfg(target_os = "macos")]
        unsafe {
            // arg=1 才包含等待父进程回收的 zombie；kill(pid, 0) 对它仍返回成功。
            // SHORTBSDINFO 不要求同一用户，避免把权限不足当成存活状态。
            let mut info: libc::proc_bsdshortinfo = std::mem::zeroed();
            let size = std::mem::size_of_val(&info) as libc::c_int;
            if libc::proc_pidinfo(
                pid as libc::c_int,
                libc::PROC_PIDT_SHORTBSDINFO,
                1,
                &mut info as *mut _ as *mut libc::c_void,
                size,
            ) == size
                && info.pbsi_status == libc::SZOMB
            {
                return false;
            }
        }
        // macOS 无 /proc：kill(pid, 0) 探测（0=存活 ; -1 且 ESRCH=不存在，EPERM=存在但无权限）
        unsafe {
            if libc::kill(pid as libc::pid_t, 0) == 0 {
                return true;
            }
        }
        std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
    }
}

/* ================= hosts ================= */

pub const HOSTS_BEGIN: &str = "# BEGIN NiceEnv (managed)";
pub const HOSTS_END: &str = "# END NiceEnv (managed)";
/// 旧版产品名（NiceServBay）写入的托管标记：合并时同样识别并清掉，
/// 否则老用户 hosts 文件里的旧块不会被替换，会残留成重复托管块
pub const HOSTS_BEGIN_LEGACY: &str = "# BEGIN NiceServBay (managed)";
pub const HOSTS_END_LEGACY: &str = "# END NiceServBay (managed)";

pub fn hosts_path() -> Result<std::path::PathBuf> {
    #[cfg(windows)]
    {
        // 提权进程不能信任调用方可修改的 SystemRoot 环境变量。
        let mut buffer = vec![0u16; 32768];
        let length = unsafe { windows_sys::Win32::System::SystemInformation::GetSystemDirectoryW(buffer.as_mut_ptr(), buffer.len() as u32) } as usize;
        if length == 0 || length >= buffer.len() { return Err(PlatformError::Win("无法定位 Windows 系统目录，未修改 hosts".into())); }
        use std::os::windows::ffi::OsStringExt;
        Ok(std::path::PathBuf::from(std::ffi::OsString::from_wide(&buffer[..length])).join("drivers\\etc\\hosts"))
    }
    #[cfg(not(windows))]
    { Ok(std::path::PathBuf::from("/etc/hosts")) }
}

/// 读取 hosts 全文
pub fn read_hosts_file() -> Result<String> {
    std::fs::read_to_string(hosts_path()?).map_err(io_err)
}

/// 以「标记块合并」方式写入托管条目；保留块外原有内容。
/// Windows 桌面进程可注册专用 helper；仅权限不足时申请 UAC，主进程不提权。
pub fn apply_managed_hosts(entries: &[(String, String)]) -> Result<()> {
    validate_hosts_entries(entries)?;
    let path = hosts_path()?;
    let original = std::fs::read_to_string(&path).map_err(io_err)?;
    let out = merge_hosts_content(&original, entries);
    if hosts_unchanged(&original, &out, entries) { return Ok(()); }
    match write_hosts_snapshot(&path, &original, &out) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied && HOSTS_HELPER.get().is_some() => {
            apply_hosts_elevated(HostsRequest { entries: Some(entries.to_vec()), content: None, expected: original })
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(PlatformError::Io("写入 hosts 需要管理员权限，请通过桌面应用重试并确认系统授权。".into())),
        Err(error) => Err(io_err(error)),
    }
}

/// 全文编辑固定的系统 hosts；沿用快照检查、恢复副本、ACL 和专用 UAC 流程。
pub fn write_hosts_file(expected: &str, content: &str) -> Result<()> {
    validate_hosts_content(content)?;
    if read_hosts_file()? != expected {
        return Err(PlatformError::Io("hosts 已被其它程序修改，请重新读取后再保存；草稿未写入".into()));
    }
    if content == expected { return Ok(()); }
    match write_hosts_snapshot(&hosts_path()?, expected, content) {
        Ok(()) => Ok(()),
        #[cfg(windows)]
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied && HOSTS_HELPER.get().is_some() => {
            apply_hosts_elevated(HostsRequest { entries: None, content: Some(content.into()), expected: expected.into() })
        }
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => Err(PlatformError::Io("保存 hosts 需要管理员权限，请通过桌面应用重试并确认系统授权。".into())),
        Err(error) => Err(io_err(error)),
    }
}

pub fn validate_hosts_content(content: &str) -> Result<()> {
    if content.len() > 1024 * 1024 || content.contains('\0') {
        return Err(PlatformError::Io("hosts 文件超过 1 MiB 或包含无效字符，未写入".into()));
    }
    for (index, line) in content.trim_start_matches('\u{feff}').lines().enumerate() {
        let line = line.split('#').next().unwrap_or_default().trim();
        if line.is_empty() { continue; }
        let mut parts = line.split_whitespace();
        let ip = parts.next().unwrap_or_default();
        let domains: Vec<_> = parts.collect();
        if domains.is_empty() || domains.iter().any(|domain| validate_hosts_entries(&[(ip.into(), domain.trim_end_matches('.').to_ascii_lowercase())]).is_err()) {
            return Err(PlatformError::Io(format!("hosts 第 {} 行格式不正确：请填写 IP 地址和至少一个主机名，注释以 # 开头", index + 1)));
        }
    }
    Ok(())
}

fn validate_hosts_entries(entries: &[(String, String)]) -> Result<()> {
    for (ip, domain) in entries {
        if ip.parse::<std::net::IpAddr>().is_err() || domain.is_empty() || domain.len() > 253
            || domain.parse::<std::net::IpAddr>().is_ok()
            || domain.split('.').any(|part| part.is_empty() || part.len() > 63 || part.starts_with('-') || part.ends_with('-')
                || !part.bytes().all(|ch| ch.is_ascii_alphanumeric() || ch == b'-')) {
            return Err(PlatformError::Io("hosts 条目格式无效，未修改系统文件".into()));
        }
    }
    Ok(())
}

fn hosts_unchanged(original: &str, out: &str, entries: &[(String, String)]) -> bool {
    original.replace("\r\n", "\n") == out
        || (entries.is_empty() && !original.lines().any(|line| is_hosts_begin(line.trim()) || is_hosts_end(line.trim())))
}

#[cfg(test)]
mod hosts_file_checks {
    use super::*;

    #[test]
    fn full_file_preserves_original_text_and_rejects_stale_writes() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("hosts");
        let original = "\u{feff}# local mappings\r\n127.0.0.1 localhost local-alias # keep\r\n\r\n::1 localhost\r\n";
        let updated = original.replace("local-alias", "renamed-alias");
        validate_hosts_content(&updated).unwrap();
        std::fs::write(&path, original).unwrap();
        write_hosts_snapshot(&path, original, &updated).unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), updated);
        assert!(write_hosts_snapshot(&path, original, "").is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), updated);
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
        write_hosts_snapshot(&path, &updated, "# no mappings\n").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "# no mappings\n");
        assert!(validate_hosts_content("127.0.0.1 ok.test\nnot-an-ip bad.test").unwrap_err().to_string().contains("第 2 行"));
        assert!(validate_hosts_content(&"#".repeat(1024 * 1024 + 1)).is_err());
    }

    #[cfg(windows)]
    #[test]
    fn elevation_request_accepts_only_one_valid_hosts_edit() {
        let mut request = HostsRequest { entries: None, content: Some("127.0.0.1 localhost # comment\r\n".into()), expected: "# before\n".into() };
        assert_eq!(request.output().unwrap(), request.content.clone().unwrap());
        request.entries = Some(vec![("::1".into(), "local.test".into())]);
        assert!(request.output().is_err());
        request.content = None;
        assert_eq!(request.output().unwrap(), merge_hosts_content(&request.expected, request.entries.as_ref().unwrap()));
        request.entries = None;
        assert!(request.output().is_err());
        request.content = Some("not an IP".into());
        assert!(request.output().is_err());
    }
}

/// 锁定原文件后重核快照并保存恢复副本；写原文件以保留其 ACL，不替换文件身份。
fn write_hosts_snapshot(path: &std::path::Path, expected: &str, out: &str) -> std::io::Result<()> {
    use std::io::{Read, Seek, Write};
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true);
    #[cfg(windows)]
    {
        use std::os::windows::fs::OpenOptionsExt;
        options.share_mode(1).custom_flags(0x0020_0000); // FILE_SHARE_READ / OPEN_REPARSE_POINT
    }
    let mut file = options.open(path)?;
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if file.metadata()?.file_attributes() & 0x400 != 0 {
            return Err(std::io::Error::other("hosts 是重解析点，未修改其指向的文件"));
        }
    }
    let mut current = String::new();
    file.read_to_string(&mut current)?;
    if current != expected { return Err(std::io::Error::other("hosts 在授权期间发生变化，请刷新后重试；未覆盖其它程序的修改")); }
    let mut backup = tempfile::Builder::new().prefix("niceenv-hosts-").suffix(".bak").tempfile_in(path.parent().ok_or_else(|| std::io::Error::other("hosts 路径无效"))?)?;
    backup.write_all(current.as_bytes())?;
    backup.as_file().sync_all()?;
    let (backup_file, backup_path) = backup.keep().map_err(|error| error.error)?;
    drop(backup_file);
    let mut write = |bytes: &[u8]| -> std::io::Result<()> {
        file.rewind()?;
        file.write_all(bytes)?;
        file.set_len(bytes.len() as u64)?;
        file.sync_all()
    };
    if let Err(error) = write(out.as_bytes()) {
        let recovery = write(current.as_bytes());
        return Err(std::io::Error::other(format!("hosts 写入失败：{error}；恢复{}；原始副本：{}", if recovery.is_ok() { "成功" } else { "失败，请从副本恢复" }, backup_path.display())));
    }
    // 写入已成功；恢复副本清理失败不应把成功操作误报为失败。
    let _ = std::fs::remove_file(backup_path);
    Ok(())
}

#[cfg(windows)]
pub const HOSTS_HELPER_ARG: &str = "--niceenv-apply-hosts";
#[cfg(windows)]
static HOSTS_HELPER: std::sync::OnceLock<std::path::PathBuf> = std::sync::OnceLock::new();

#[cfg(windows)]
pub fn enable_hosts_elevation(executable: std::path::PathBuf) {
    let _ = HOSTS_HELPER.set(executable);
}

#[cfg(windows)]
#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HostsRequest {
    entries: Option<Vec<(String, String)>>,
    content: Option<String>,
    expected: String,
}

#[cfg(windows)]
impl HostsRequest {
    fn output(&self) -> Result<String> {
        match (&self.entries, &self.content) {
            (Some(entries), None) => {
                validate_hosts_entries(entries)?;
                Ok(merge_hosts_content(&self.expected, entries))
            }
            (None, Some(content)) => {
                validate_hosts_content(content)?;
                Ok(content.clone())
            }
            _ => Err(PlatformError::Io("hosts 请求必须且只能指定一种编辑方式".into())),
        }
    }
}

/// 专用提权入口：目标固定为系统 hosts，映射和全文均校验，不启动 UI 或服务。
#[cfg(windows)]
pub fn run_hosts_elevation_helper(request_path: &std::path::Path) -> Result<()> {
    use std::io::Read;
    let mut request = String::new();
    std::fs::File::open(request_path).map_err(io_err)?.take(8 * 1024 * 1024 + 1).read_to_string(&mut request).map_err(io_err)?;
    if request.len() > 8 * 1024 * 1024 { return Err(PlatformError::Io("hosts 请求超过大小限制".into())); }
    let request: HostsRequest = serde_json::from_str(&request).map_err(|_| PlatformError::Io("hosts 请求格式无效".into()))?;
    let out = request.output()?;
    write_hosts_snapshot(&hosts_path()?, &request.expected, &out).map_err(io_err)
}

#[cfg(windows)]
fn apply_hosts_elevated(request: HostsRequest) -> Result<()> {
    use std::io::Write;
    let executable = HOSTS_HELPER.get().ok_or_else(|| PlatformError::Io("未配置 hosts 授权程序".into()))?;
    let mut request_file = tempfile::Builder::new().prefix("niceenv-hosts-request-").suffix(".json").tempfile().map_err(io_err)?;
    let out = request.output()?;
    let request = serde_json::to_vec(&request)
        .map_err(|_| PlatformError::Io("无法生成 hosts 请求".into()))?;
    if request.len() > 8 * 1024 * 1024 { return Err(PlatformError::Io("hosts 请求超过大小限制".into())); }
    request_file.write_all(&request).map_err(io_err)?;
    request_file.as_file().sync_all().map_err(io_err)?;
    run_elevated(&executable.to_string_lossy(), &[HOSTS_HELPER_ARG, &request_file.path().to_string_lossy()])
        .map_err(|error| PlatformError::Win(format!("hosts 管理员授权未完成或写入失败，请确认 Windows 授权后重试；若已授权，请检查 hosts 是否被占用或修改。{error}")))?;
    let actual = read_hosts_file()?;
    if actual != out {
        return Err(PlatformError::Io("授权程序已退出，但 hosts 内容未通过核对，请重试".into()));
    }
    Ok(())
}

fn is_hosts_begin(t: &str) -> bool {
    t == HOSTS_BEGIN || t == HOSTS_BEGIN_LEGACY
}

fn is_hosts_end(t: &str) -> bool {
    t == HOSTS_END || t == HOSTS_END_LEGACY
}

/// 逐行标出「这一行是开始标记，且它之后的下一个标记是结束标记」——
/// 只有这样的开始标记才真正开启一个托管块。结束标记被手工删掉时，
/// 孤立的开始标记后面的内容（哪怕后面还跟着一个完整托管块）都不算托管内容。
pub fn hosts_block_starts(lines: &[&str]) -> Vec<bool> {
    let mut out = vec![false; lines.len()];
    let mut next_marker_is_end: Option<bool> = None;
    for i in (0..lines.len()).rev() {
        let t = lines[i].trim();
        if is_hosts_begin(t) {
            out[i] = next_marker_is_end == Some(true);
            next_marker_is_end = Some(false);
        } else if is_hosts_end(t) {
            next_marker_is_end = Some(true);
        }
    }
    out
}

/// 纯函数：把托管标记块合并进 hosts 文本（可单测）
///
/// 只有「开始标记后面确实还有结束标记」才当作托管块整段替换；
/// 结束标记被手工删掉时只丢弃孤立的开始标记行，其后的行原样保留——
/// 宁可残留几条旧托管条目，也不能把用户自己的 hosts 条目一并删掉。
pub fn merge_hosts_content(original: &str, entries: &[(String, String)]) -> String {
    let lines: Vec<&str> = original.lines().collect();
    let starts = hosts_block_starts(&lines);
    let mut out = String::new();
    let mut in_block = false;
    let mut block_written = false;
    for (i, line) in lines.iter().enumerate() {
        let t = line.trim();
        if is_hosts_begin(t) {
            in_block = starts[i];
            if !block_written {
                out.push_str(&render_block(entries));
                block_written = true;
            }
            continue;
        }
        if is_hosts_end(t) {
            in_block = false;
            continue;
        }
        if !in_block {
            out.push_str(line);
            out.push('\n');
        }
    }
    if !block_written {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
        out.push_str(&render_block(entries));
    }
    out
}

fn render_block(entries: &[(String, String)]) -> String {
    let mut s = String::from(HOSTS_BEGIN);
    s.push('\n');
    for (ip, domain) in entries {
        s.push_str(&format!("{ip:<15} {domain}\n"));
    }
    s.push_str(HOSTS_END);
    s.push('\n');
    s
}

/* ================= 系统代理 ================= */

#[derive(serde::Serialize, serde::Deserialize, Clone, Debug)]
pub struct SystemProxyState {
    pub enabled: bool,
    pub server: String,
}

/// 读取系统代理（Windows: 注册表；macOS: networksetup）
pub fn get_system_proxy() -> Result<SystemProxyState> {
    #[cfg(windows)]
    {
        sysproxy_win::get().map_err(|e| PlatformError::Win(e))
    }
    #[cfg(not(windows))]
    {
        // networksetup -getwebproxy Wi-Fi → 输出 "Enabled: Yes\nServer: 127.0.0.1\nPort: 8080"
        let service = mac_network_service()?;
        let out = std::process::Command::new("networksetup")
            .args(["-getwebproxy", &service])
            .output()
            .map_err(|e| PlatformError::Io(format!("执行 networksetup 失败：{e}")))?;
        if !out.status.success() {
            return Err(PlatformError::Io(format!(
                "networksetup 读取代理失败：{}",
                String::from_utf8_lossy(&out.stderr)
            )));
        }
        let text = String::from_utf8_lossy(&out.stdout).to_string();
        let enabled = text.lines().any(|l| l.trim() == "Enabled: Yes");
        let server = text
            .lines()
            .find(|l| l.starts_with("Server:"))
            .and_then(|l| l.split(':').nth(1))
            .map(|s| s.trim().to_string())
            .and_then(|host| {
                text.lines()
                    .find(|l| l.starts_with("Port:"))
                    .and_then(|l| l.split(':').nth(1))
                    .map(|p| format!("{}:{}", host, p.trim()))
            })
            .unwrap_or_default();
        Ok(SystemProxyState { enabled, server })
    }
}

/// 设置系统代理；返回开启前的旧值供恢复。
pub fn set_system_proxy(enable: bool, server: &str) -> Result<SystemProxyState> {
    #[cfg(windows)]
    {
        sysproxy_win::set(enable, server).map_err(|e| PlatformError::Win(e))
    }
    #[cfg(not(windows))]
    {
        let old = get_system_proxy()?;
        let service = mac_network_service()?;
        let (host, port) = match server.split_once(':') {
            Some((h, p)) => (h, p),
            None => ("127.0.0.1", "7890"),
        };
        let run = |args: &[&str]| -> Result<()> {
            let out = std::process::Command::new("networksetup")
                .args(args)
                .output()
                .map_err(|e| PlatformError::Io(format!("networksetup 失败：{e}")))?;
            if !out.status.success() {
                return Err(PlatformError::Io(format!(
                    "networksetup {} 失败：{}",
                    args.first().unwrap_or(&""),
                    String::from_utf8_lossy(&out.stderr)
                )));
            }
            Ok(())
        };
        if enable {
            run(&["-setwebproxy", &service, host, port])?;
            run(&["-setsecurewebproxy", &service, host, port])?;
        } else {
            run(&["-setwebproxystate", &service, "off"])?;
            run(&["-setsecurewebproxystate", &service, "off"])?;
        }
        Ok(old)
    }
}

/// macOS：取第一个已连接的网络服务（Wi-Fi 优先）
#[cfg(not(windows))]
fn mac_network_service() -> Result<String> {
    let out = std::process::Command::new("networksetup")
        .args(["-listallnetworkservices"])
        .output()
        .map_err(|e| PlatformError::Io(format!("networksetup 失败：{e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    for line in text.lines().skip(1) {
        let name = line.trim().trim_start_matches('*');
        if name.eq_ignore_ascii_case("Wi-Fi") {
            return Ok(name.to_string());
        }
    }
    // 兜底：第一个非 VPN 服务
    for line in text.lines().skip(1) {
        let name = line.trim().trim_start_matches('*');
        if !name.is_empty()
            && !name.eq_ignore_ascii_case("VPN")
            && !name.contains("Thunderbolt")
            && !name.contains("Bluetooth")
        {
            return Ok(name.to_string());
        }
    }
    Err(PlatformError::Io("找不到可用的网络服务".into()))
}

/* ================= 提权执行（预留） ================= */

/// 以管理员运行命令（Windows runas / macOS osascript）。用于信任 CA、写 hosts 失败兜底。
pub fn run_elevated(program: &str, args: &[&str]) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // ShellExecuteW runas（0x0802 请求管理员）。
        // 参数各自用引号包住，避免含空格路径被拆成多个参数
        let quoted_args = windows_argument_line(args);
        let cmd = format!(
            "$ErrorActionPreference='Stop'; try {{ $child = Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -WindowStyle Hidden -Wait -PassThru; if ($child.ExitCode -ne 0) {{ throw ('提权程序退出码：' + $child.ExitCode) }} }} catch {{ [Console]::Error.WriteLine($_.Exception.Message); exit 1 }}",
            program.replace('\'', "''"),
            quoted_args.replace('\'', "''")
        );
        let out = std::process::Command::new("powershell")
            .args(["-NoProfile", "-Command", &cmd])
            .creation_flags(0x0800_0000) // CREATE_NO_WINDOW
            .output()
            .map_err(io_err)?;
        // UAC 被取消 / 命令失败时 PowerShell 返回非 0，必须让调用方知道
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(PlatformError::Win(if msg.is_empty() {
                "提权被取消或执行失败（UAC）".into()
            } else {
                format!("提权执行失败：{msg}")
            }));
        }
        Ok(())
    }
    #[cfg(not(windows))]
    {
        // 逐参数做 shell 引号转义：路径里带空格/引号时不能简单 join
        let quoted: Vec<String> = std::iter::once(program.to_string())
            .chain(args.iter().map(|a| a.to_string()))
            .map(|a| shell_quote(&a))
            .collect();
        let inner = quoted.join(" ");
        // 内层还有一层 AppleScript 字符串，需转义反斜杠与双引号
        let escaped = inner.replace('\\', "\\\\").replace('"', "\\\"");
        let script = format!("do shell script \"{escaped}\" with administrator privileges");
        let out = std::process::Command::new("osascript")
            .args(["-e", &script])
            .output()
            .map_err(io_err)?;
        // 用户取消授权时 osascript 返回非 0；要让上层知道失败，不能静默当成功
        if !out.status.success() {
            let msg = String::from_utf8_lossy(&out.stderr).trim().to_string();
            return Err(PlatformError::Io(if msg.is_empty() {
                "提权被取消或执行失败".into()
            } else {
                format!("提权执行失败：{msg}")
            }));
        }
        Ok(())
    }
}

#[cfg(any(windows, test))]
fn windows_argument_line(args: &[&str]) -> String {
    args.iter().map(|arg| {
        let mut out = String::from("\"");
        let mut slashes = 0;
        for ch in arg.chars() {
            if ch == '\\' { slashes += 1; continue; }
            out.push_str(&"\\".repeat(if ch == '"' { slashes * 2 + 1 } else { slashes }));
            out.push(ch);
            slashes = 0;
        }
        out.push_str(&"\\".repeat(slashes * 2));
        out.push('"');
        out
    }).collect::<Vec<_>>().join(" ")
}

/// POSIX 单引号转义：'`it`s`' → '\'' 包裹，可安全嵌入 shell 命令
#[cfg(not(windows))]
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', "'\\''"))
}

/* ================= windows 实现 ================= */

#[cfg(windows)]
mod windows_job {
    use windows_sys::Win32::Foundation::{CloseHandle, HANDLE};
    use windows_sys::Win32::System::JobObjects::{
        AssignProcessToJobObject, CreateJobObjectW, JobObjectExtendedLimitInformation,
        JobObjectBasicAccountingInformation, QueryInformationJobObject, JOBOBJECT_BASIC_ACCOUNTING_INFORMATION,
        SetInformationJobObject, TerminateJobObject, JOBOBJECT_EXTENDED_LIMIT_INFORMATION,
        JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
    };
    use windows_sys::Win32::System::Threading::{
        OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_SET_QUOTA, PROCESS_TERMINATE,
    };

    pub struct JobObject(HANDLE);

    unsafe impl Send for JobObject {}

    impl JobObject {
        /// 不带 KILL_ON_JOB_CLOSE 的普通 Job：进程树仍可被整体终止，
        /// 但创建者（如 CLI）退出、句柄关闭时**不会**连带杀掉服务。
        pub fn create_detached() -> std::io::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                Ok(JobObject(job))
            }
        }

        pub fn create_kill_on_close() -> std::io::Result<Self> {
            unsafe {
                let job = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                if job.is_null() {
                    return Err(std::io::Error::last_os_error());
                }
                let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
                let ok = SetInformationJobObject(
                    job,
                    JobObjectExtendedLimitInformation,
                    &info as *const _ as *const core::ffi::c_void,
                    std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
                );
                if ok == 0 {
                    let e = std::io::Error::last_os_error();
                    CloseHandle(job);
                    return Err(e);
                }
                Ok(JobObject(job))
            }
        }
    }

    pub fn assign_process(job: &JobObject, pid: u32) -> std::io::Result<()> {
        unsafe {
            let proc = OpenProcess(PROCESS_SET_QUOTA | PROCESS_TERMINATE, 0, pid);
            if proc.is_null() {
                return Err(std::io::Error::last_os_error());
            }
            let ok = AssignProcessToJobObject(job.0, proc);
            let err = if ok == 0 {
                Some(std::io::Error::last_os_error())
            } else {
                None
            };
            CloseHandle(proc);
            match err {
                Some(e) => Err(e),
                None => Ok(()),
            }
        }
    }

    pub fn terminate_job(job: &JobObject) -> std::io::Result<()> {
        unsafe {
            if TerminateJobObject(job.0, 0) == 0 {
                return Err(std::io::Error::last_os_error());
            }
            // TerminateJobObject 像 TerminateProcess 一样发起异步退出；
            // 必须等整个 Job 清空，不能只等 cmd 等父进程，否则子进程仍可能占端口。
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
            loop {
                let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
                if QueryInformationJobObject(job.0, JobObjectBasicAccountingInformation,
                    &mut info as *mut _ as *mut core::ffi::c_void,
                    std::mem::size_of_val(&info) as u32, std::ptr::null_mut()) == 0 {
                    return Err(std::io::Error::last_os_error());
                }
                if info.ActiveProcesses == 0 { return Ok(()); }
                if std::time::Instant::now() >= deadline {
                    return Err(std::io::Error::new(std::io::ErrorKind::TimedOut,
                        format!("等待服务进程组退出超时，仍有 {} 个进程", info.ActiveProcesses)));
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        }
    }

    impl Drop for JobObject {
        fn drop(&mut self) {
            unsafe {
                CloseHandle(self.0);
            }
        }
    }

    pub fn process_alive(pid: u32) -> bool {
        unsafe {
            use windows_sys::Win32::Foundation::{ERROR_INVALID_PARAMETER, WAIT_OBJECT_0};
            use windows_sys::Win32::System::Threading::{PROCESS_SYNCHRONIZE, WaitForSingleObject};
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | PROCESS_SYNCHRONIZE, 0, pid);
            if h.is_null() {
                // 拒绝访问或其它读取错误不能冒充已退出。
                return std::io::Error::last_os_error().raw_os_error() != Some(ERROR_INVALID_PARAMETER as i32);
            }
            let exited = WaitForSingleObject(h, 0) == WAIT_OBJECT_0;
            CloseHandle(h);
            !exited
        }
    }
}

#[cfg(windows)]
mod sysproxy_win {
    use super::SystemProxyState;
    use windows_sys::Win32::Foundation::ERROR_SUCCESS;
    use windows_sys::Win32::Networking::WinInet::{
        InternetSetOptionW, INTERNET_OPTION_REFRESH, INTERNET_OPTION_SETTINGS_CHANGED,
    };
    use windows_sys::Win32::System::Registry::{
        RegCloseKey, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER,
        KEY_READ, KEY_SET_VALUE, REG_DWORD, REG_SZ,
    };

    const SUBKEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Internet Settings";

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn wide_to_string(buf: &[u8]) -> String {
        let units: Vec<u16> = buf
            .chunks_exact(2)
            .take_while(|c| !(c[0] == 0 && c[1] == 0))
            .map(|c| u16::from_le_bytes([c[0], c[1]]))
            .collect();
        String::from_utf16_lossy(&units)
    }

    pub fn get() -> std::result::Result<SystemProxyState, String> {
        unsafe {
            let subkey = wide(SUBKEY);
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(HKEY_CURRENT_USER, subkey.as_ptr(), 0, KEY_READ, &mut hkey)
                != ERROR_SUCCESS
            {
                return Err("打开注册表失败".into());
            }
            let value = wide("ProxyEnable");
            let mut data: u32 = 0;
            let mut size: u32 = 4;
            let t = RegQueryValueExW(
                hkey,
                value.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                &mut data as *mut u32 as *mut u8,
                &mut size,
            );
            let enabled = t == ERROR_SUCCESS && data != 0;

            let mut server = String::new();
            let value = wide("ProxyServer");
            let mut buf = vec![0u8; 1024];
            let mut size: u32 = buf.len() as u32;
            let t = RegQueryValueExW(
                hkey,
                value.as_ptr(),
                std::ptr::null_mut(),
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                &mut size,
            );
            if t == ERROR_SUCCESS {
                server = wide_to_string(&buf[..size as usize]);
            }
            RegCloseKey(hkey);
            Ok(SystemProxyState { enabled, server })
        }
    }

    pub fn set(enable: bool, server: &str) -> std::result::Result<SystemProxyState, String> {
        unsafe {
            let old = get()?;
            let subkey = wide(SUBKEY);
            let mut hkey: HKEY = std::ptr::null_mut();
            if RegOpenKeyExW(
                HKEY_CURRENT_USER,
                subkey.as_ptr(),
                0,
                KEY_SET_VALUE,
                &mut hkey,
            ) != ERROR_SUCCESS
            {
                return Err("打开注册表失败（写入系统代理）".into());
            }
            // ProxyEnable
            let name = wide("ProxyEnable");
            let data: u32 = if enable { 1 } else { 0 };
            let r1 = RegSetValueExW(
                hkey,
                name.as_ptr(),
                0,
                REG_DWORD,
                (&data as *const u32) as *const u8,
                4,
            );
            // ProxyServer
            let mut r2 = ERROR_SUCCESS;
            if enable {
                let name = wide("ProxyServer");
                let ws = wide(server);
                r2 = RegSetValueExW(
                    hkey,
                    name.as_ptr(),
                    0,
                    REG_SZ,
                    ws.as_ptr() as *const u8,
                    (ws.len() * 2) as u32,
                );
            }
            RegCloseKey(hkey);
            if r1 != ERROR_SUCCESS || r2 != ERROR_SUCCESS {
                return Err("写入注册表失败".into());
            }
            // 通知 WinINet 刷新
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_SETTINGS_CHANGED,
                std::ptr::null(),
                0,
            );
            InternetSetOptionW(
                std::ptr::null_mut(),
                INTERNET_OPTION_REFRESH,
                std::ptr::null(),
                0,
            );
            Ok(old)
        }
    }
}

/* ================= 系统 DNS 接管（本地域名解析配套） ================= */

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct DnsConfiguration {
    pub interface_id: String,
    pub automatic: bool,
    pub servers: Vec<String>,
}

impl DnsConfiguration {
    pub fn is_local(&self) -> bool {
        !self.automatic && self.servers == ["127.0.0.1"]
    }

    pub fn matches(&self, other: &Self) -> bool {
        self.interface_id == other.interface_id && self.automatic == other.automatic
            && (self.automatic || self.servers == other.servers)
    }
}

#[cfg(windows)]
fn powershell_json(script: &str) -> Result<serde_json::Value> {
    let script = format!("$ErrorActionPreference='Stop'; [Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); {script}");
    let out = command("powershell").args(["-NoProfile", "-NonInteractive", "-Command", &script]).output().map_err(io_err)?;
    if !out.status.success() {
        return Err(PlatformError::Io(format!("读取网络配置失败：{}", String::from_utf8_lossy(&out.stderr).trim())));
    }
    serde_json::from_slice(&out.stdout).map_err(|e| PlatformError::Io(format!("网络配置返回格式无效：{e}")))
}

#[cfg(windows)]
pub fn connected_interfaces() -> Result<Vec<String>> {
    let value = powershell_json("ConvertTo-Json -Compress -InputObject @(Get-NetAdapter -IncludeHidden | Where-Object { $_.Status -eq 'Up' } | Select-Object -ExpandProperty Name | Sort-Object -Unique)")?;
    serde_json::from_value(value).map_err(|e| PlatformError::Io(format!("网络接口格式无效：{e}")))
}

#[cfg(not(windows))]
pub fn connected_interfaces() -> Result<Vec<String>> {
    let out = command("networksetup").arg("-listallnetworkservices").output().map_err(io_err)?;
    if !out.status.success() { return Err(PlatformError::Io(String::from_utf8_lossy(&out.stderr).into())); }
    Ok(String::from_utf8_lossy(&out.stdout).lines().skip(1).map(str::trim)
        .filter(|name| !name.is_empty() && !name.starts_with('*')).map(str::to_owned).collect())
}

/// Windows 只接管 IPv4，保留接口原有 IPv6 DNS；macOS networksetup 读取完整静态服务器列表。
pub fn dns_configuration(name: &str) -> Result<DnsConfiguration> {
    if name.trim().is_empty() || name.chars().any(char::is_control) {
        return Err(PlatformError::Io("网络接口名称无效".into()));
    }
    #[cfg(windows)]
    {
        let escaped = name.replace('\'', "''");
        let value = powershell_json(&format!(
            "$adapter = @(Get-NetAdapter -IncludeHidden | Where-Object {{ $_.Name -eq '{escaped}' }}); if ($adapter.Count -ne 1) {{ throw '网络接口不存在或名称不唯一' }};              $id = ([guid]$adapter[0].InterfaceGuid).ToString('B');              $registry = Get-ItemProperty -LiteralPath ('HKLM:\\SYSTEM\\CurrentControlSet\\Services\\Tcpip\\Parameters\\Interfaces\\' + $id);              $static = [string]$registry.NameServer;              $servers = @((Get-DnsClientServerAddress -InterfaceIndex $adapter[0].ifIndex -AddressFamily IPv4).ServerAddresses);              @{{ interfaceId=$id; automatic=[string]::IsNullOrWhiteSpace($static); servers=$servers }} | ConvertTo-Json -Compress"
        ))?;
        serde_json::from_value(value).map_err(|e| PlatformError::Io(format!("DNS 配置格式无效：{e}")))
    }
    #[cfg(not(windows))]
    {
        let out = command("networksetup").args(["-getdnsservers", name]).output().map_err(io_err)?;
        if !out.status.success() { return Err(PlatformError::Io(String::from_utf8_lossy(&out.stderr).into())); }
        let text = String::from_utf8_lossy(&out.stdout);
        let automatic = text.trim().starts_with("There aren't any DNS Servers set on ");
        let servers = if automatic { Vec::new() } else {
            text.lines().map(|line| line.trim().parse::<std::net::IpAddr>()
                .map(|ip| ip.to_string()).map_err(|_| PlatformError::Io("无法识别当前 DNS 配置，未修改网络设置".into())))
                .collect::<Result<Vec<_>>>()?
        };
        if !automatic && servers.is_empty() { return Err(PlatformError::Io("DNS 配置为空，无法确定自动获取状态".into())); }
        Ok(DnsConfiguration { interface_id: name.into(), automatic, servers })
    }
}

pub fn set_dns_configuration_elevated(name: &str, config: &DnsConfiguration) -> Result<()> {
    let current = dns_configuration(name)?;
    if current.interface_id != config.interface_id {
        return Err(PlatformError::Io("网络接口已变化，不能把旧配置写入其它接口".into()));
    }
    if !config.automatic && config.servers.is_empty() {
        return Err(PlatformError::Io("静态 DNS 至少需要一个服务器地址".into()));
    }
    for server in &config.servers {
        let ip = server.parse::<std::net::IpAddr>().map_err(|_| PlatformError::Io("DNS 服务器地址无效".into()))?;
        if cfg!(windows) && !ip.is_ipv4() { return Err(PlatformError::Io("此接口操作仅支持 IPv4 DNS，IPv6 配置保持不变".into())); }
    }
    #[cfg(windows)]
    {
        let name_arg = format!("name={name}").replace('\'', "''");
        let script = if config.automatic {
            format!("& netsh interface ipv4 set dnsservers '{name_arg}' source=dhcp; exit $LASTEXITCODE")
        } else {
            let mut script = format!("& netsh interface ipv4 set dnsservers '{name_arg}' source=static address={} validate=no; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}; ", config.servers[0]);
            for (index, server) in config.servers.iter().enumerate().skip(1) {
                script.push_str(&format!("& netsh interface ipv4 add dnsservers '{name_arg}' address={server} index={} validate=no; if ($LASTEXITCODE -ne 0) {{ exit $LASTEXITCODE }}; ", index + 1));
            }
            script.push_str("exit 0");
            script
        };
        run_elevated("powershell", &["-NoProfile", "-NonInteractive", "-Command", &script])?;
    }
    #[cfg(not(windows))]
    {
        let mut args = vec!["-setdnsservers", name];
        if config.automatic { args.push("empty"); } else { args.extend(config.servers.iter().map(String::as_str)); }
        run_elevated("networksetup", &args)?;
    }
    let after = dns_configuration(name)?;
    if !config.matches(&after) {
        return Err(PlatformError::Io("设置命令已退出，但 DNS 配置未达到目标状态；请刷新核对后重试".into()));
    }
    Ok(())
}

#[cfg(test)]
mod process_tests {
    use super::*;

    struct ChildGuard(std::process::Child);
    impl Drop for ChildGuard {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    #[cfg(windows)]
    #[test]
    fn mongodb_event_requests_never_force_terminate_and_reject_stale_identity() {
        use windows_sys::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
        use windows_sys::Win32::System::Threading::{CreateEventW, WaitForSingleObject};
        struct EventGuard(HANDLE);
        impl Drop for EventGuard {
            fn drop(&mut self) {
                unsafe {
                    CloseHandle(self.0);
                }
            }
        }
        let mut child = ChildGuard(
            command("powershell.exe")
                .args([
                    "-NoProfile",
                    "-NonInteractive",
                    "-Command",
                    "Start-Sleep -Seconds 30",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let pid = child.0.id();
        let marker = process_start_marker(pid).unwrap();
        assert!(VerifiedProcess::open(pid, "win:1").unwrap().is_none());
        let process = VerifiedProcess::open(pid, &marker).unwrap().unwrap();
        assert!(process.request_mongodb_shutdown().is_err());
        assert!(child.0.try_wait().unwrap().is_none());
        let name: Vec<u16> = format!("Global\\Mongo_{pid}")
            .encode_utf16()
            .chain(Some(0))
            .collect();
        let event = EventGuard(unsafe { CreateEventW(std::ptr::null(), 1, 0, name.as_ptr()) });
        assert!(!event.0.is_null());
        process.request_mongodb_shutdown().unwrap();
        assert_eq!(unsafe { WaitForSingleObject(event.0, 0) }, WAIT_OBJECT_0);
        // 普通程序不会监听此事件；请求正常停机不能直接结束它。
        assert!(child.0.try_wait().unwrap().is_none());
        drop(event);
        assert!(process.request_mongodb_shutdown().is_err());
        child.0.kill().unwrap();
        child.0.wait().unwrap();
        assert!(process.has_exited().unwrap());
        process.request_mongodb_shutdown().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn repeated_termination_handles_the_exit_transition_without_reaping() {
        use std::os::unix::process::CommandExt;
        for _ in 0..16 {
            let mut cmd = command("sleep");
            cmd.arg("5");
            unsafe {
                cmd.pre_exec(spawn_pre_exec);
            }
            let mut child = ChildGuard(cmd.spawn().unwrap());
            let pid = child.0.id();
            assert!(!process_group_gone(pid).unwrap());
            let process = VerifiedProcess::open(pid, &process_start_marker(pid).unwrap())
                .unwrap()
                .unwrap();
            process.terminate().unwrap();
            process.terminate().unwrap();
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
            while !process.has_exited().unwrap() && std::time::Instant::now() < deadline {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            assert!(process.has_exited().unwrap());
            process.terminate().unwrap();
            assert!(process_group_gone(pid).unwrap());
            child.0.wait().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn mongodb_shutdown_uses_term_and_allows_handler_to_exit_cleanly() {
        use std::io::BufRead;
        let mut child = ChildGuard(
            command("sh")
                .args([
                    "-c",
                    "trap 'exit 0' TERM; echo ready; while :; do sleep 1; done",
                ])
                .stdin(std::process::Stdio::null())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let mut ready = String::new();
        std::io::BufReader::new(child.0.stdout.take().unwrap())
            .read_line(&mut ready)
            .unwrap();
        assert_eq!(ready.trim(), "ready");
        let pid = child.0.id();
        let process = VerifiedProcess::open(pid, &process_start_marker(pid).unwrap())
            .unwrap()
            .unwrap();
        process.request_mongodb_shutdown().unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while !process.has_exited().unwrap() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        assert!(process.has_exited().unwrap());
        assert!(!process_alive(pid));
        assert_eq!(child.0.wait().unwrap().code(), Some(0));
    }
}

#[cfg(test)]
mod dns_tests {
    use super::*;

    #[test]
    fn elevated_arguments_preserve_spaces_quotes_and_trailing_slashes() {
        assert_eq!(windows_argument_line(&["interface", "name=Wi-Fi 2", "a\"b", "C:\\dir\\", ""]),
            "\"interface\" \"name=Wi-Fi 2\" \"a\\\"b\" \"C:\\dir\\\\\" \"\"");
    }

    #[test]
    fn restored_dns_compares_static_order_but_not_dhcp_assigned_addresses() {
        let mut original = DnsConfiguration { interface_id: "adapter".into(), automatic: false, servers: vec!["9.9.9.9".into(), "1.1.1.1".into()] };
        let mut current = original.clone();
        current.servers.reverse();
        assert!(!original.matches(&current));
        original.automatic = true;
        current.automatic = true;
        assert!(original.matches(&current));
        current.interface_id = "replacement".into();
        assert!(!original.matches(&current));
        assert!(!current.is_local());
    }
}
