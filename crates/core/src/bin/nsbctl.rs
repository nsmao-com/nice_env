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

fn main() -> ExitCode {
    // 分离模式标记：spawn_tracked 据此选择不带 KILL_ON_JOB_CLOSE 的 Job
    std::env::set_var("NSB_CLI", "1");

    let args: Vec<String> = std::env::args().skip(1).collect();
    if args.is_empty() {
        usage();
        return ExitCode::from(2);
    }
    let (cmd, rest): (&str, &[String]) = (&args[0], &args[1..]);
    let json = rest.iter().any(|a| a == "--json");
    let pos: Vec<&str> = rest
        .iter()
        .filter(|a| !a.starts_with("--"))
        .map(String::as_str)
        .collect();

    let emit: nsb_core::EventSink = std::sync::Arc::new(|_| {});
    let state = match nsb_core::CoreState::init(None, emit) {
        Ok(s) => s,
        Err(e) => {
            eprintln!(
                "nsbctl: 初始化失败：{}（提示：数据目录不可写时可设 NSB_HOME）",
                e.message
            );
            return ExitCode::FAILURE;
        }
    };

    match cmd {
        "status" => cmd_status(&state, json),
        "sites" => cmd_sites(&state, json),
        "packages" => cmd_packages(&state, json),
        "start" => one_service(&state, pos.first().copied(), |s, id| s.start_service(id)),
        "stop" => one_service(&state, pos.first().copied(), |s, id| s.stop_service(id)),
        "restart" => one_service(&state, pos.first().copied(), |s, id| {
            s.stop_service(id).ok();
            s.start_service(id)
        }),
        "start-all" => stack(&state, true),
        "stop-all" => stack(&state, false),
        "open" => cmd_open(&state, pos.first().copied()),
        "logs" => cmd_logs(&state, pos.first().copied(), pos.get(1).copied()),
        "diagnose" => cmd_diagnose(pos.first().copied()),
        "pin" => cmd_pin(&pos),
        "help" | "--help" | "-h" => {
            usage();
            ExitCode::SUCCESS
        }
        other => {
            eprintln!("nsbctl: 未知命令 {other}（nsbctl help 查看用法）");
            ExitCode::from(2)
        }
    }
}

fn usage() {
    println!(
        "nsbctl — NiceEnv CLI\n\
         用法：nsbctl <命令>\n\
         \x20 status [--json] | start|stop|restart <service> | start-all | stop-all\n\
         \x20 sites [--json] | open <site> | packages [--json] | logs <service> [n] | diagnose <port>"
    );
}

fn one_service(
    state: &std::sync::Arc<nsb_core::CoreState>,
    id: Option<&str>,
    f: fn(&nsb_core::CoreState, &str) -> nsb_core::error::Result<()>,
) -> ExitCode {
    let Some(id) = id else {
        eprintln!("nsbctl: 缺少服务 id（示例：nginx、php@8.3.33、mysql@8.0.46）");
        return ExitCode::from(2);
    };
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
    let sites = match nsb_core::sites::list(&state.store) {
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
        let domain = s.domains.first().map(|d| d.as_str()).unwrap_or("localhost");
        let scheme = if s.https { "https" } else { "http" };
        println!("{:<18} {:<8} {}://{}", s.name, s.status, scheme, domain);
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

fn stack(state: &std::sync::Arc<nsb_core::CoreState>, start: bool) -> ExitCode {
    // 与托盘「启动常用栈」一致：nginx 优先、php 次之、数据库随后
    let mut ids: Vec<String> = state
        .service_status_list()
        .iter()
        .map(|s| s.id.clone())
        .collect();
    ids.sort_by_key(|id| match () {
        _ if id == "nginx" || id == "apache" => 0,
        _ if id.starts_with("php@") => 1,
        _ if id.starts_with("mysql@") => 2,
        _ if id == "postgresql" || id == "mongodb" => 3,
        _ => 4,
    });
    let mut fail = 0;
    for id in &ids {
        let r = if start {
            state.start_service(id)
        } else {
            state.stop_service(id)
        };
        match r {
            Ok(()) => println!("{} {id}", if start { "started" } else { "stopped" }),
            Err(e) => {
                // 停止时「本来就没在跑」不算失败
                if !start && e.code == "STOP_FAILED" {
                    println!("skip {id}");
                } else {
                    eprintln!("fail {id}: {}", e.message);
                    fail += 1;
                }
            }
        }
    }
    if fail > 0 {
        ExitCode::FAILURE
    } else {
        ExitCode::SUCCESS
    }
}

fn cmd_open(state: &std::sync::Arc<nsb_core::CoreState>, name: Option<&str>) -> ExitCode {
    let Some(name) = name else {
        eprintln!("nsbctl: 缺少站点名（nsbctl sites 查看）");
        return ExitCode::from(2);
    };
    let Ok(sites) = nsb_core::sites::list(&state.store) else {
        return ExitCode::FAILURE;
    };
    let Some(site) = sites.iter().find(|s| s.name == name) else {
        eprintln!("nsbctl: 找不到站点 {name}");
        return ExitCode::FAILURE;
    };
    let domain = site
        .domains
        .first()
        .map(|d| d.as_str())
        .unwrap_or("localhost");
    let url = format!("{}://{}", if site.https { "https" } else { "http" }, domain);
    #[cfg(windows)]
    let r = std::process::Command::new("cmd")
        .args(["/C", "start", "", &url])
        .creation_flags(0x0800_0000)
        .spawn();
    #[cfg(not(windows))]
    let r = std::process::Command::new("open").arg(&url).spawn();
    match r {
        Ok(_) => {
            println!("opened {url}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("nsbctl: 打开失败 {e}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_logs(
    state: &std::sync::Arc<nsb_core::CoreState>,
    id: Option<&str>,
    n: Option<&str>,
) -> ExitCode {
    let Some(id) = id else {
        eprintln!("nsbctl: 缺少服务 id");
        return ExitCode::from(2);
    };
    let lines: usize = n.and_then(|s| s.parse().ok()).unwrap_or(50);
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

fn cmd_diagnose(port: Option<&str>) -> ExitCode {
    let Some(p) = port.and_then(|s| s.parse::<u16>().ok()) else {
        eprintln!("nsbctl: 缺少或非法端口（0-65535）");
        return ExitCode::from(2);
    };
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
fn cmd_pin(args: &[&str]) -> ExitCode {
    let Some(key) = args.first() else {
        eprintln!("nsbctl: 用法 nsbctl pin <php@x.y.z> <dir>");
        return ExitCode::from(2);
    };
    let Some(dir) = args.get(1) else {
        eprintln!("nsbctl: 缺少目录参数");
        return ExitCode::from(2);
    };
    let Some(ver) = key.strip_prefix("php@") else {
        eprintln!("nsbctl: 目前仅支持 php@x.y.z 形式");
        return ExitCode::from(2);
    };
    if !ver.chars().all(|c| c.is_ascii_digit() || c == '.') {
        eprintln!("nsbctl: 非法版本串 {ver}");
        return ExitCode::from(2);
    }
    let dir_path = std::path::Path::new(dir);
    if !dir_path.is_dir() {
        eprintln!("nsbctl: 目录不存在 {dir}");
        return ExitCode::FAILURE;
    }
    let file = dir_path.join(".nsb.json");
    let body = format!(
        "{{ \"php\": \"{ver}\" }}
"
    );
    match std::fs::write(&file, body) {
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
