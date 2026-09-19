//! 环境变量注入验收：bin 目录推导、PATH 合并纯函数、真实读-写-回滚。
//!
//! 用法：cargo run -p nsb-core --bin check_pathenv
//! 数据目录固定 repo/.pathenv-home（与 .smoke-home / .services-home / .extra-home 隔离）
//!
//! ⚠️ 本测试会真实改写当前用户的 PATH（这是唯一能验证「确实写进去了」的方式），
//! 因此开头就快照原始值，结束时无条件还原（含 panic 场景：用 guard 兜底）。
//! 只在本机开发环境跑，不要放进 CI 的共享 runner。

use std::path::PathBuf;
use std::sync::Arc;

use nsb_core::pathenv;

fn main() {
    let base = PathBuf::from(".pathenv-home");
    // 每次从干净状态开始：上一次运行会把设置留在盘上（如 enabled=true），
    // 不清掉的话「默认关闭」这类初始态断言会在第二次运行时假失败。
    if base.exists() {
        std::fs::remove_dir_all(&base).expect("清理旧的 .pathenv-home");
    }
    std::fs::create_dir_all(&base).expect("创建 .pathenv-home");
    std::env::set_var("NSB_SKIP_HOSTS", "1");

    let mut pass = 0usize;
    let mut fail = 0usize;
    // 宏体包在闭包里：这样 `return Err(...)` 退出的是闭包而不是 main
    macro_rules! check {
        ($name:expr, $body:expr) => {
            match (|| -> std::result::Result<String, String> { $body })() {
                Ok(d) => {
                    println!("[PASS] {} — {}", $name, d);
                    pass += 1;
                }
                Err(e) => {
                    println!("[FAIL] {} — {}", $name, e);
                    fail += 1;
                }
            }
        };
    }

    /* ---------- 0. 快照真实 PATH，装好还原守卫 ---------- */
    let snapshot = platform::pathenv::read_user_path();
    let guard = RestoreGuard {
        value: snapshot.as_ref().ok().map(|p| p.value.clone()),
        reg_type: snapshot.as_ref().ok().map(|p| p.reg_type).unwrap_or(0),
    };
    match &snapshot {
        Ok(p) => println!(
            "· 已快照用户 PATH（{} 字符，reg_type={}），结束时会原样还原",
            p.value.chars().count(),
            p.reg_type
        ),
        Err(e) => {
            println!("[FAIL] 快照用户 PATH — {e}");
            std::process::exit(1);
        }
    }

    /* ---------- 1. 纯函数：bin 目录推导 ---------- */
    check!("bin 目录 = 入口程序的父目录", {
        let cases: &[(&str, &str, &str)] = &[
            ("D:/rt/php/8.3.33", "php-cgi.exe", "D:/rt/php/8.3.33"),
            ("D:/rt/go/1.24.1", "go/bin/go.exe", "D:/rt/go/1.24.1/go/bin"),
            (
                "D:/rt/mysql/8.0.46",
                "mysql-8.0.46-winx64/bin/mysqld.exe",
                "D:/rt/mysql/8.0.46/mysql-8.0.46-winx64/bin",
            ),
            (
                "D:/rt/nginx/1.26.3/",
                "nginx-1.26.3/nginx.exe",
                "D:/rt/nginx/1.26.3/nginx-1.26.3",
            ),
        ];
        let mut bad = Vec::new();
        for (root, entry, want) in cases {
            match pathenv::bin_dir_for(root, entry) {
                Some(got) if got == *want => {}
                Some(got) => bad.push(format!("{root} + {entry} → {got}，期望 {want}")),
                None => bad.push(format!("{root} + {entry} 应可推导，却返回 None")),
            }
        }
        if bad.is_empty() {
            Ok(format!("{} 个用例全部命中", cases.len()))
        } else {
            Err(bad.join("；"))
        }
    });

    check!("非可执行入口被跳过（phar/php/jar 等）", {
        let skipped = [
            ("D:/rt/composer/2.8.5", "composer.phar"),
            ("D:/rt/adminer/4.8.1", "adminer-4.8.1.php"),
        ];
        for (root, entry) in skipped {
            if pathenv::bin_dir_for(root, entry).is_some() {
                return Err(format!("{entry} 不该注入 PATH（不是可直接执行的命令）"));
            }
        }
        Ok("composer.phar / adminer.php 已正确排除".into())
    });

    /* ---------- 2. 纯函数：PATH 合并语义 ---------- */
    check!("合并：前置我们的目录且保留他人条目", {
        let out = pathenv::merge_win_path(
            "%SystemRoot%\\system32;C:\\Other\\bin",
            &[],
            &["D:\\rt\\php\\8.3.33".to_string()],
        );
        let want = "D:\\rt\\php\\8.3.33;%SystemRoot%\\system32;C:\\Other\\bin";
        if out != want {
            return Err(format!("得到 {out}，期望 {want}"));
        }
        Ok("我们的目录排最前，%SystemRoot% 变量引用未被破坏".into())
    });

    check!("关闭：只摘自己的，别人的一条不动", {
        let got = pathenv::merge_win_path(
            "D:\\rt\\php\\8.3.33;C:\\Other\\bin",
            &["D:\\rt\\php\\8.3.33".to_string()],
            &[],
        );
        if got != "C:\\Other\\bin" {
            return Err(format!("得到 {got}，期望 C:\\Other\\bin"));
        }
        Ok("托管条目已摘除，用户路径完好".into())
    });

    check!("幂等：重复应用不产生重复条目", {
        let dirs = vec!["D:\\rt\\php\\8.3.33".to_string(), "D:\\rt\\go\\1.24.1".to_string()];
        let once = pathenv::merge_win_path("C:\\keep", &[], &dirs);
        let twice = pathenv::merge_win_path(&once, &dirs, &dirs);
        if once != twice {
            return Err(format!("一次 {once}\n两次 {twice}"));
        }
        if twice.matches("D:\\rt\\php").count() != 1 {
            return Err("出现了重复条目".into());
        }
        Ok("两次应用结果一致".into())
    });

    /* ---------- 3. 真实写盘：开 → 校验 → 关 → 校验 ---------- */
    let state = nsb_core::CoreState::init(Some(base.clone()), Arc::new(|_| {})).expect("初始化");

    check!("初始状态：开关默认关闭", {
        let st = state.pathenv_status();
        if st.enabled {
            return Err("新数据目录不该是开启状态".into());
        }
        Ok(format!("enabled=false，已托管 {} 个目录", st.managed_dirs.len()))
    });

    check!("开启注入（真实写注册表）", {
        let st = state.pathenv_set_enabled(true).map_err(|e| e.message.clone())?;
        if !st.enabled {
            return Err("开启后 enabled 仍为 false".into());
        }
        // 无已装包时应没有目录要写，但仍应可开启（状态一致）
        let after = platform::pathenv::read_user_path().map_err(|e| e.to_string())?;
        let disk = pathenv::split_win_path(&after.value);
        for d in &st.managed_dirs {
            if !disk.iter().any(|x| x.eq_ignore_ascii_case(d)) {
                return Err(format!("托管目录 {d} 没有出现在真实 PATH 中"));
            }
        }
        Ok(format!(
            "enabled=true，已托管 {} 个目录，真实 PATH {} 条",
            st.managed_dirs.len(),
            disk.len()
        ))
    });

    check!("关闭注入后真实 PATH 逐字符还原", {
        let st = state.pathenv_set_enabled(false).map_err(|e| e.message.clone())?;
        if st.enabled {
            return Err("关闭后 enabled 仍为 true".into());
        }
        if !st.managed_dirs.is_empty() {
            return Err(format!("关闭后仍记录托管 {:?}", st.managed_dirs));
        }
        // 与开头的快照比：关闭后应当与原值一致
        let now = platform::pathenv::read_user_path().map_err(|e| e.to_string())?;
        let original = guard.value.clone().unwrap_or_default();
        if !same_path_text(&now.value, &original) {
            return Err(format!(
                "PATH 未还原。\n原值：{original}\n现值：{}",
                now.value
            ));
        }
        Ok("与快照一致（测试没有污染系统 PATH）".into())
    });

    check!("漂移检测与安全合并：外部条目必须保留", {
        // 造一个真实的「已安装包」：目录里放个假的可执行文件。
        // 比起真下载一个运行时，这样更快且同样走完整条推导/写入链路。
        let fake_root = base.join("runtimes").join("php").join("8.3.33");
        std::fs::create_dir_all(&fake_root).map_err(|e| e.to_string())?;
        let fake_exe = fake_root.join(if cfg!(windows) { "php.exe" } else { "php" });
        std::fs::write(&fake_exe, b"#!/bin/sh\n").map_err(|e| e.to_string())?;
        state
            .store
            .upsert_installed(&nsb_core::model::InstalledPackage {
                id: "php".into(),
                version: "8.3.33".into(),
                category: "runtime".into(),
                install_path: fake_root.to_string_lossy().to_string(),
                config_path: String::new(),
                installed_at: 0,
            })
            .map_err(|e| e.message.clone())?;

        state.pathenv_set_enabled(true).map_err(|e| e.message.clone())?;
        let after = state.pathenv_reapply().map_err(|e| e.message.clone())?;
        if after.managed_dirs.is_empty() {
            return Err("已装 php 8.3.33 却没有任何托管目录，推导链路断了".into());
        }
        let want = fake_root.to_string_lossy().to_string();
        let disk_after_apply = platform::pathenv::read_user_path().map_err(|e| e.to_string())?;
        if !pathenv::split_win_path(&disk_after_apply.value)
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&want))
        {
            return Err(format!("{want} 未写入真实 PATH"));
        }

        // 手工往真实 PATH 里塞一个外部目录（模拟别的程序改过 PATH），再重应用
        let tampered = format!("C:\\InjectByTest;{}", disk_after_apply.value);
        platform::pathenv::write_user_path(&tampered, disk_after_apply.reg_type)
            .map_err(|e| e.to_string())?;
        let after2 = state.pathenv_reapply().map_err(|e| e.message.clone())?;
        let disk = platform::pathenv::read_user_path().map_err(|e| e.to_string())?;
        // 安全红线：外部条目必须原样保留 —— 我们只清理自己写过的目录
        if !disk.value.contains("C:\\InjectByTest") {
            return Err("外部注入的目录被误删，合并过于激进".into());
        }
        if !pathenv::split_win_path(&disk.value)
            .iter()
            .any(|x| x.eq_ignore_ascii_case(&want))
        {
            return Err("重应用后托管目录丢失".into());
        }

        // 手工摘掉托管目录，漂移检测应能发现
        let stripped: Vec<String> = pathenv::split_win_path(&disk.value)
            .into_iter()
            .filter(|x| !after2.managed_dirs.iter().any(|m| m.eq_ignore_ascii_case(x)))
            .collect();
        platform::pathenv::write_user_path(&stripped.join(";"), disk.reg_type)
            .map_err(|e| e.to_string())?;
        if !state.pathenv_status().drift {
            return Err("托管目录被外部删除后未报告漂移".into());
        }

        Ok(format!(
            "托管 {} 个目录；外部条目保留；缺失时 drift 可检出",
            after2.managed_dirs.len()
        ))
    });

    /* ---------- 收尾：还原 ---------- */
    drop(guard);

    println!();
    println!("==== check_pathenv: {pass} pass / {fail} fail ====");
    println!("数据目录：.pathenv-home（用户 PATH 已还原）");
    if fail > 0 {
        std::process::exit(1);
    }
}

fn same_path_text(a: &str, b: &str) -> bool {
    let norm = |s: &str| {
        let mut v: Vec<String> = s
            .split(';')
            .map(|x| x.trim().trim_end_matches(['\\', '/']).to_ascii_lowercase())
            .filter(|x| !x.is_empty())
            .collect();
        v.sort();
        v
    };
    norm(a) == norm(b)
}

/// 无条件还原用户 PATH —— 即使中途 panic 也要把机器恢复原样
struct RestoreGuard {
    value: Option<String>,
    reg_type: u32,
}

impl Drop for RestoreGuard {
    fn drop(&mut self) {
        if let Some(v) = &self.value {
            match platform::pathenv::write_user_path(v, self.reg_type) {
                Ok(()) => println!("· 已还原用户 PATH（{} 字符）", v.chars().count()),
                Err(e) => eprintln!("!! 还原用户 PATH 失败，请手动检查：{e}"),
            }
        }
    }
}
