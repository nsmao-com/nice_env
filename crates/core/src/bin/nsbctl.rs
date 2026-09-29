//! nsbctl — NiceEnv 命令行工具。
//!
//! 与桌面 App 共用同一数据目录（%LOCALAPPDATA%/NiceEnv 或 NSB_HOME），
//! 能查状态、起停服务、列站点。CLI 启动的服务走「分离模式」：
//! CLI 退出服务不退；桌面 App 下次启动会自动收养它们（而不是清杀）。
//!
//! 用法：
//!   nsbctl status [--json]        服务状态总览
//!   nsbctl start <service>        启动服务（nginx / php@8.3.33 / mysql@8.0.46 / …）
//!   nsbctl stop <service>         停止服务
//!   nsbctl kill <service> --yes   强制停止已核实的服务（可能丢失未保存数据）
//!   nsbctl restart <service>      重启服务
//!   nsbctl start-all              启动常用栈（与托盘「启动常用栈」一致）
//!   nsbctl stop-all               停止全部服务
//!   nsbctl sites [--json]         站点列表（含访问 URL）
//!   nsbctl open <site-name>       用默认浏览器打开站点
//!   nsbctl packages [--json]      已安装套件
//!   nsbctl logs <service> [n]     最后 n 行日志（默认 50）
//!   nsbctl diagnose <port>        查端口占用者
//!
//! 退出码：0 成功；1 运行失败；2 用法错误。

use std::process::ExitCode;

#[cfg(windows)]
use std::os::windows::process::CommandExt;

enum Command<'a> {
    Help,
    Version,
    Status(bool),
    Sites(bool),
    Packages(bool),
    Service { action: &'a str, id: &'a str },
    Stack { start: bool, json: bool },
    Open(&'a str),
    Logs { id: &'a str, lines: usize },
    Diagnose(u16),
    Pin { version: &'a str, directory: &'a str },
}

/// 所有语法检查都在打开数据目录、接管服务进程之前完成。
fn parse_command(args: &[String]) -> Result<Command<'_>, String> {
    let Some(cmd) = args.first().map(String::as_str) else { return Err("请指定命令（nsbctl help 查看用法）".into()); };
    if !matches!(cmd, "help" | "--help" | "-h" | "version" | "--version" | "-V" | "status" | "sites" | "packages"
        | "start" | "stop" | "kill" | "restart" | "start-all" | "stop-all" | "open" | "logs" | "diagnose" | "pin") {
        return Err(format!("未知命令 {cmd}（nsbctl help 查看用法）"));
    }
    let rest = &args[1..];
    if rest.len() == 1 && matches!(rest[0].as_str(), "--help" | "-h") { return Ok(Command::Help); }
    let mut json = false;
    let mut yes = false;
    let mut pos = Vec::new();
    for arg in rest {
        match arg.as_str() {
            "--json" if !json && matches!(cmd, "status" | "sites" | "packages" | "start-all" | "stop-all") => json = true,
            "--yes" if !yes && cmd == "kill" => yes = true,
            flag if flag.starts_with('-') => return Err(format!("{cmd} 不支持参数 {flag}，或该参数已重复")),
            value => pos.push(value),
        }
    }
    let require = |count: usize| -> Result<(), String> {
        if pos.len() == count { Ok(()) } else { Err(format!("{cmd} 需要 {count} 个位置参数，收到 {} 个（nsbctl help 查看用法）", pos.len())) }
    };
    let service_id = |id: &str| -> Result<(), String> {
        if id.is_empty() || id.len() > 512 || id.chars().any(|c| c.is_whitespace() || c.is_control()) {
            Err("服务 id 不能为空、包含空白或控制字符，且长度不能超过 512".into())
        } else { Ok(()) }
    };
    match cmd {
        "help" | "--help" | "-h" => { require(0)?; Ok(Command::Help) },
        "version" | "--version" | "-V" => { require(0)?; Ok(Command::Version) },
        "status" => { require(0)?; Ok(Command::Status(json)) },
        "sites" => { require(0)?; Ok(Command::Sites(json)) },
        "packages" => { require(0)?; Ok(Command::Packages(json)) },
        "start-all" | "stop-all" => { require(0)?; Ok(Command::Stack { start: cmd == "start-all", json }) },
        "start" | "stop" | "restart" | "kill" => {
            require(1)?;
            service_id(pos[0])?;
            if cmd == "kill" && !yes {
                return Err("强制停止可能中断数据库写入；确认后使用 nsbctl kill <service> --yes".into());
            }
            Ok(Command::Service { action: cmd, id: pos[0] })
        },
        "open" => {
            require(1)?;
            if pos[0].trim().is_empty() || pos[0].chars().any(char::is_control) { return Err("请提供有效的站点名".into()); }
            Ok(Command::Open(pos[0]))
        },
        "logs" => {
            if !(1..=2).contains(&pos.len()) { return Err("用法：nsbctl logs <service> [n]".into()); }
            service_id(pos[0])?;
            let lines = match pos.get(1) {
                None => 50,
                Some(value) => value.parse::<usize>().ok().filter(|n| *n > 0).ok_or("日志行数必须是正整数")?,
            };
            Ok(Command::Logs { id: pos[0], lines })
        },
        "diagnose" => {
            require(1)?;
            let port = pos[0].parse::<u16>().ok().filter(|port| *port > 0).ok_or("端口必须是 1–65535 的整数")?;
            Ok(Command::Diagnose(port))
        },
        "pin" => {
            require(2)?;
            let version = pos[0].strip_prefix("php@").ok_or("目前仅支持 php@x.y.z 形式")?;
            let parts: Vec<_> = version.split('.').collect();
            if parts.len() != 3 || parts.iter().any(|part| part.is_empty() || !part.bytes().all(|b| b.is_ascii_digit())) {
                return Err("版本必须是 php@x.y.z 形式（示例：php@8.3.33）".into());
            }
            if pos[1].is_empty() { return Err("请提供项目目录".into()); }
            Ok(Command::Pin { version, directory: pos[1] })
        },
        _ => Err(format!("未知命令 {cmd}")),
    }
}

fn main() -> ExitCode {
    // 分离模式标记：spawn_tracked 据此选择不带 KILL_ON_JOB_CLOSE 的 Job
    std::env::set_var("NSB_CLI", "1");

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return ExitCode::from(2);
    }
    let command = match parse_command(&args) {
        Ok(command) => command,
        Err(message) => { eprintln!("nsbctl: {message}"); return ExitCode::from(2); }
    };
    match command {
        Command::Help => { usage(); return ExitCode::SUCCESS; },
        Command::Version => { println!("nsbctl {}", env!("CARGO_PKG_VERSION")); return ExitCode::SUCCESS; },
        Command::Diagnose(port) => return cmd_diagnose(port),
        Command::Pin { version, directory } => return cmd_pin(version, directory),
        _ => {},
    }

    let emit: nsb_core::EventSink = std::sync::Arc::new(|_| {});
    let state = match nsb_core::CoreState::init(None, emit) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "nsbctl: 初始化失败：{}{}",
                e.message, e.hint.map(|hint|format!("\n{hint}")).unwrap_or_default()
            );
            return ExitCode::FAILURE;
        }
    };

    let _activity = match nsb_core::paths::DataDirActivity::shared(&state.paths.base) {
        Ok(guard) => guard,
        Err(error) => { eprintln!("nsbctl: {}{}", error.message,error.hint.map(|hint|format!("\n{hint}")).unwrap_or_default()); return ExitCode::FAILURE; }
    };
    match command {
        Command::Status(json) => cmd_status(&state, json),
        Command::Sites(json) => cmd_sites(&state, json),
        Command::Packages(json) => cmd_packages(&state, json),
        Command::Service { action: "start", id } => one_service(&state, id, |s, id| s.start_service(id)),
        Command::Service { action: "stop", id } => one_service(&state, id, |s, id| s.stop_service(id)),
        Command::Service { action: "kill", id } => {
            one_service(&state, id, |state, id| {
                let preview = state.service_stop_preview(id)?;
                state.force_stop_service(id, &preview.revision)
            })
        },
        Command::Service { action: "restart", id } => one_service(&state, id, |s, id| s.restart_service(id)),
        Command::Stack { start, json } => stack(&state, start, json),
        Command::Open(name) => cmd_open(&state, name),
        Command::Logs { id, lines } => cmd_logs(&state, id, lines),
        _ => ExitCode::from(2),
    }
}

fn usage() {
    println!(
        "nsbctl — NiceEnv CLI\n\
         用法：nsbctl <命令>\n\
         \x20 help | --version\n\
         \x20 status [--json] | start|stop|restart <service> | start-all [--json] | stop-all [--json]\n\
         \x20 kill <service> --yes（强制停止，可能丢失未保存数据）\n\
         \x20 sites [--json] | open <site> | packages [--json] | logs <service> [n] | diagnose <port>\n\
         \x20 pin <php@x.y.z> <dir>（站点 PHP 版本，写入 .nsb.json）"
    );
}

fn one_service(
    state: &std::sync::Arc<nsb_core::CoreState>,
    id: &str,
    f: fn(&nsb_core::CoreState, &str) -> nsb_core::error::Result<()>,
) -> ExitCode {
    match f(state, id) {
        Ok(()) => {
            println!("ok {id}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("fail {id}: {}", e.message);
            if let Some(h) = &e.hint {
                eprintln!("  提示：{h}");
            }
            ExitCode::FAILURE
        }
    }
}

fn cmd_status(state: &std::sync::Arc<nsb_core::CoreState>, json: bool) -> ExitCode {
    let list = state.service_status_list();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&list).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    println!("{:<22} {:<9} {:>8}  {}", "SERVICE", "STATE", "PORT", "PIDS");
    for s in &list {
        println!(
            "{:<22} {:<9} {:>8}  {}",
            s.id,
            format!("{:?}", s.state).to_lowercase(),
            s.port.map(|p| p.to_string()).unwrap_or_else(|| "-".into()),
            s.pids
                .iter()
                .map(|p| p.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
    }
    ExitCode::SUCCESS
}

fn cmd_sites(state: &std::sync::Arc<nsb_core::CoreState>, json: bool) -> ExitCode {
    let sites = match nsb_core::sites::list_with_status(&state.paths, &state.store, &state.manager) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("nsbctl: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&sites).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    for s in &sites {
        println!("{:<18} {:<8} {}", s.name, s.status, s.access_url.as_deref().unwrap_or("（访问入口尚未就绪）"));
    }
    if sites.is_empty() {
        println!("（还没有站点）");
    }
    ExitCode::SUCCESS
}

fn cmd_packages(state: &std::sync::Arc<nsb_core::CoreState>, json: bool) -> ExitCode {
    let views = match state.list_packages() {
        Ok(v) => v,
        Err(e) => {
            eprintln!("nsbctl: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    let installed: Vec<_> = views.into_iter().filter(|v| v.install.is_some()).collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&installed).unwrap_or_default()
        );
        return ExitCode::SUCCESS;
    }
    for v in &installed {
        if let Some(i) = &v.install {
            println!("{:<14} {:<12} {}", v.manifest.id, i.version, i.install_path);
        }
    }
    if installed.is_empty() {
        println!("（还没有安装任何套件）");
    }
    ExitCode::SUCCESS
}

fn stack(state: &std::sync::Arc<nsb_core::CoreState>, start: bool, json: bool) -> ExitCode {
    // 与桌面批量操作一致：启动先库后 Web，停止反向，错误不能忽略。
    let mut ids: Vec<String> = state
        .service_status_list()
        .iter()
        .map(|s| s.id.clone())
        .collect();
    ids.sort();
    let result = if start { state.bulk_start(&ids) } else { state.stop_all_services() };
    let report = match result {
        Ok(report) => report,
        Err(error) => {
            eprintln!("{}: {}", error.code, error.message);
            return ExitCode::FAILURE;
        }
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report).unwrap());
    } else {
        for id in &report.succeeded { println!("{} {id}", if start { "started" } else { "stopped" }); }
        for id in &report.already { println!("already {id}"); }
        for item in &report.failed { eprintln!("fail {}: {}", item.service_id, item.error.message); }
    }
    if !report.failed.is_empty() {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_open(state: &std::sync::Arc<nsb_core::CoreState>, name: &str) -> ExitCode {
    let sites = match nsb_core::sites::list(&state.store) {
        Ok(sites) => sites,
        Err(error) => { eprintln!("nsbctl: 读取站点失败：{error}"); return ExitCode::FAILURE; }
    };
    let Some(site) = sites.iter().find(|s| s.name == name) else {
        eprintln!("nsbctl: 找不到站点 {name}");
        return ExitCode::FAILURE;
    };
    let url = match nsb_core::sites::access_url(&state.paths, &state.store, &state.manager, &site.id) {
        Ok(url) => url,
        Err(error) => {
            eprintln!("nsbctl: {}{}", error.message, error.hint.map(|hint| format!("\n{hint}")).unwrap_or_default());
            return ExitCode::FAILURE;
        }
    };
    #[cfg(windows)]
    let r = std::process::Command::new("cmd")
        .args(["/D", "/C", "start", "", &url])
        .creation_flags(0x0800_0000)
        .status();
    #[cfg(target_os = "macos")]
    let r = std::process::Command::new("open").arg(&url).status();
    #[cfg(not(any(windows, target_os = "macos")))]
    let r = std::process::Command::new("xdg-open").arg(&url).status();
    match r {
        Ok(status) if status.success() => {
            println!("opened {url}");
            ExitCode::SUCCESS
        }
        Ok(status) => {
            eprintln!("nsbctl: 浏览器启动失败（{status}），可手动访问 {url}");
            ExitCode::FAILURE
        }
        Err(e) => {
            eprintln!("nsbctl: 打开失败 {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_logs(
    state: &std::sync::Arc<nsb_core::CoreState>,
    id: &str,
    lines: usize,
) -> ExitCode {
    let logs = match state.tail_logs_checked(id, lines) {
        Ok(logs) => logs,
        Err(e) => {
            eprintln!("nsbctl: {}", e.message);
            return ExitCode::FAILURE;
        }
    };
    for l in logs {
        println!("{}", l.line);
    }
    ExitCode::SUCCESS
}

fn cmd_diagnose(p: u16) -> ExitCode {
    match nsb_core::ports::diagnose_port(p) {
        Ok(d) => {
            if d.in_use {
                println!(
                    "port {p}: 占用中 pid={} process={}",
                    d.pid.map(|x| x.to_string()).unwrap_or_else(|| "?".into()),
                    d.process_name.as_deref().unwrap_or("?")
                );
            } else {
                println!("port {p}: 空闲");
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nsbctl: {}", e.message);
            ExitCode::FAILURE
        }
    }
}

/// nsbctl pin <php@x.y.z> <dir> —— 把运行时版本锁定写入 <dir>/.nsb.json，
/// 此后在 App 里在该目录创建站点（PHP 版本选「跟随项目」）会自动使用该版本。
fn cmd_pin(ver: &str, dir: &str) -> ExitCode {
    let dir_path = std::path::Path::new(dir);
    if !dir_path.is_dir() {
        eprintln!("nsbctl: 目录不存在 {dir}");
        return ExitCode::FAILURE;
    }
    let file = dir_path.join(".nsb.json");
    // 保留文件中的其它配置；解析失败时不覆盖原文件，写入失败也不留下半份 JSON。
    let result = (|| -> nsb_core::error::Result<()> {
        use nsb_core::error::AppError;
        use std::io::Write;
        let original = match std::fs::read(&file) {
            Ok(bytes) => Some(bytes),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
            Err(error) => return Err(AppError::io("读取 .nsb.json", error)),
        };
        let mut config = match &original {
            Some(bytes) => serde_json::from_slice::<serde_json::Value>(bytes)
                .map_err(|error| AppError::new("BAD_PROJECT_PIN", format!(".nsb.json 格式不正确，原文件已保留：{error}")))?,
            None => serde_json::json!({}),
        };
        let object = config.as_object_mut().ok_or_else(|| AppError::new("BAD_PROJECT_PIN", ".nsb.json 必须是对象，原文件已保留"))?;
        object.insert("php".into(), serde_json::json!(ver));
        let mut pending = tempfile::NamedTempFile::new_in(dir_path)?;
        if original.is_some() { pending.as_file().set_permissions(std::fs::metadata(&file)?.permissions())?; }
        writeln!(pending, "{config:#}")?;
        pending.as_file().sync_all()?;
        if let Some(original) = original {
            if std::fs::read(&file)? != original {
                return Err(AppError::new("PROJECT_PIN_CHANGED", ".nsb.json 已被修改，请检查最新内容后重试"));
            }
            pending.persist(&file).map_err(|error| AppError::io("保存 .nsb.json", error.error))?;
        } else {
            pending.persist_noclobber(&file).map_err(|error| AppError::io("创建 .nsb.json", error.error))?;
        }
        Ok(())
    })();
    match result {
        Ok(()) => {
            println!("pinned php@{ver} → {}", file.display());
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nsbctl: 写入失败 {e}");
            ExitCode::FAILURE
        }
    }
}
