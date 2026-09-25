//! 平台差异层：Windows Job Object / 进程树管理、系统代理、hosts 写入、提权执行、
//! 运行时 bin 目录注入系统 PATH。
//! core 不直接依赖本 crate 的平台 API，统一走这里的封装。

use thiserror::Error;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

pub mod pathenv;

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

/* ================= 进程树管理 ================= */

/// 进程组句柄：Windows=Job Object(KILL_ON_JOB_CLOSE)，Unix=记录 pid 集合。
pub struct ProcessGroup {
    #[cfg(windows)]
    job: Option<windows_job::JobObject>,
    pids: Vec<u32>,
}

impl ProcessGroup {
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
                kill_tree(*pid, sig);
            }
        }
        self.pids.clear();
        Ok(())
    }
}

/// Unix：先终止进程组，组不存在时退回单进程。
#[cfg(not(windows))]
fn kill_tree(pid: u32, sig: libc::c_int) {
    let pid = pid as libc::pid_t;
    unsafe {
        // 负 pid = 整个进程组（子进程启动时 setpgid(0,0)，pgid == pid）
        if libc_kill_group(pid, sig) != 0 {
            let e = std::io::Error::last_os_error();
            // ESRCH：组不存在，可能未被置组，退回杀单个进程
            if e.raw_os_error() == Some(libc::ESRCH) {
                libc_kill(pid as u32, sig);
            }
        }
    }
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
    #[cfg(windows)]
    {
        windows_job::process_alive(pid)
    }
    #[cfg(not(windows))]
    {
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

pub fn hosts_path() -> std::path::PathBuf {
    if cfg!(windows) {
        let sysroot = std::env::var("SystemRoot").unwrap_or_else(|_| "C:\\Windows".into());
        std::path::PathBuf::from(sysroot).join("System32\\drivers\\etc\\hosts")
    } else {
        std::path::PathBuf::from("/etc/hosts")
    }
}

/// 读取 hosts 全文
pub fn read_hosts_file() -> Result<String> {
    std::fs::read_to_string(hosts_path()).map_err(io_err)
}

/// 以「标记块合并」方式写入托管条目；保留块外原有内容。
/// 无权限时返回 Err（由上层转人话提示 + 指引）。
pub fn apply_managed_hosts(entries: &[(String, String)]) -> Result<()> {
    let path = hosts_path();
    let original = std::fs::read_to_string(&path).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            PlatformError::Io(format!(
                "无权限写入 hosts（需要管理员/ root）。请以管理员身份运行本应用，或手动把域名指向 127.0.0.1。"
            ))
        } else {
            io_err(e)
        }
    })?;
    let out = merge_hosts_content(&original, entries);

    // 先完整写出临时文件（写不进去就不碰 hosts），再覆盖复制回 hosts。
    // 注意这不是原子替换：用 copy 而非 rename 是为了保留 hosts 原有的属主/ACL。
    let tmp = path.with_extension("hosts.tmp");
    std::fs::write(&tmp, out).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            PlatformError::Io("无权限写入 hosts（需要管理员权限）".into())
        } else {
            io_err(e)
        }
    })?;
    std::fs::copy(&tmp, &path).map_err(|e| {
        let _ = std::fs::remove_file(&tmp);
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            PlatformError::Io("无权限替换 hosts 文件（需要管理员权限）".into())
        } else {
            io_err(e)
        }
    })?;
    let _ = std::fs::remove_file(&tmp);
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
        let quoted_args: Vec<String> = args
            .iter()
            .map(|a| format!("\"{}\"", a.replace('"', "\\\"")))
            .collect();
        let cmd = format!(
            "Start-Process -FilePath '{}' -ArgumentList '{}' -Verb RunAs -Wait",
            program.replace('\'', "''"),
            quoted_args.join(",").replace('\'', "''")
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
                Err(std::io::Error::last_os_error())
            } else {
                Ok(())
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
            let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
            if h.is_null() {
                return false;
            }
            let mut code: u32 = 0;
            let ok = windows_sys::Win32::System::Threading::GetExitCodeProcess(h, &mut code);
            CloseHandle(h);
            ok != 0 && code == 259 /* STILL_ACTIVE */
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

/// 枚举已连接的网络接口名（Windows: netsh；macOS: networksetup）。
/// 用于让用户/调用方选择要把 DNS 指向本地解析器的网卡。
#[cfg(windows)]
pub fn connected_interfaces() -> Result<Vec<String>> {
    let out = std::process::Command::new("netsh")
        .args(["interface", "show", "interface"])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| PlatformError::Io(format!("netsh 失败：{e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    let mut names = Vec::new();
    for line in text.lines().skip(3) {
        // 列：Admin State State Type Interface Name
        let mut parts = line.split_whitespace();
        let _admin = parts.next();
        let state = parts.next().unwrap_or("");
        let _type_ = parts.next();
        parts.next(); // loopback 标记列
        if state.eq_ignore_ascii_case("connected") || state.eq_ignore_ascii_case("已连接") {
            let rest: Vec<&str> = line.splitn(4, ' ').collect();
            if let Some(name) = rest.last() {
                let name = name.trim().to_string();
                if !name.is_empty() {
                    names.push(name);
                }
            }
        }
    }
    Ok(names)
}

#[cfg(not(windows))]
pub fn connected_interfaces() -> Result<Vec<String>> {
    let out = std::process::Command::new("networksetup")
        .args(["-listallnetworkservices"])
        .output()
        .map_err(|e| PlatformError::Io(format!("networksetup 失败：{e}")))?;
    let text = String::from_utf8_lossy(&out.stdout);
    Ok(text
        .lines()
        .skip(1)
        .map(|l| l.trim().trim_start_matches('*').to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

/// 读取接口当前 DNS 服务器（原始文本，前端展示用）
#[cfg(windows)]
pub fn interface_dns_status(name: &str) -> Result<String> {
    let out = std::process::Command::new("netsh")
        .args(["interface", "ip", "show", "dns", "name=", name])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|e| PlatformError::Io(format!("netsh 失败：{e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

#[cfg(not(windows))]
pub fn interface_dns_status(name: &str) -> Result<String> {
    let out = std::process::Command::new("networksetup")
        .args(["-getdns", name])
        .output()
        .map_err(|e| PlatformError::Io(format!("networksetup 失败：{e}")))?;
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// 提权把接口 DNS 设为 127.0.0.1（本地解析器接管）。
/// Windows: netsh set dns（单个主 DNS 即可）；macOS: networksetup -setdnsservers。
pub fn set_dns_localhost_elevated(name: &str) -> Result<()> {
    #[cfg(windows)]
    {
        run_elevated(
            "netsh",
            &[
                "interface",
                "ip",
                "set",
                "dns",
                "name=".to_string().leak(),
                name,
                "source=static",
                "addr=127.0.0.1",
                "register=primary",
            ],
        )
    }
    #[cfg(not(windows))]
    {
        run_elevated("networksetup", &["-setdnsservers", name, "127.0.0.1"])
    }
}

/// 恢复为自动获取（DHCP 下发）的 DNS
pub fn restore_dns_elevated(name: &str) -> Result<()> {
    #[cfg(windows)]
    {
        run_elevated(
            "netsh",
            &[
                "interface",
                "ip",
                "set",
                "dns",
                "name=".to_string().leak(),
                name,
                "source=dhcp",
            ],
        )
    }
    #[cfg(not(windows))]
    {
        run_elevated("networksetup", &["-setdnsservers", name, "empty"])
    }
}
