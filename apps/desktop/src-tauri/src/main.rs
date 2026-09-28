#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    #[cfg(windows)]
    {
        if args.get(1).is_some_and(|arg| arg == platform::HOSTS_HELPER_ARG) {
            let result = if args.len() == 3 {
                platform::run_hosts_elevation_helper(std::path::Path::new(&args[2]))
            } else {
                Err(platform::PlatformError::Io("hosts 授权参数无效".into()))
            };
            std::process::exit(if result.is_ok() { 0 } else { 1 });
        }
    }
    if args.iter().any(|a| a == "--smoke-test") {
        niceservbay_lib::smoke::run();
        return;
    }
    #[cfg(windows)]
    if let Ok(executable) = std::env::current_exe() {
        platform::enable_hosts_elevation(executable);
    }
    niceservbay_lib::run();
}
