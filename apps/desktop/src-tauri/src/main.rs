#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.iter().any(|a| a == "--smoke-test") {
        niceservbay_lib::smoke::run();
        return;
    }
    niceservbay_lib::run();
}
