//! 托盘：状态感知的右键菜单 + 自绘图标。
//!
//! 菜单结构（自上而下，组与组之间用分隔线切开）：
//! 1. **状态抬头**：应用名 + 版本、运行中服务汇总（`enabled=false` 的只读项）；
//! 2. **一键操作**：`▶ 启动服务栈` 子菜单（预设栈 + `管理服务栈…`）、`■ 停止全部服务`；
//! 3. **服务**：按类别分组——Web 服务器 / 语言运行时 / 数据库 / 缓存·队列 / 工具·代理，
//!    每组以一条**禁用的组标题**开头、组间加分隔线；条目带状态符号
//!    （● 运行中 / ○ 已停止 / ⚠ 异常），勾选态 = 运行中，点一下 = 启停该服务；
//!    只列「已安装 + 有启停语义」的服务，避免菜单被空壳塞满。
//! 4. **站点**：最近 8 个站点的浏览器快捷入口 + `管理站点…`；
//! 5. **导航**：总览 / 套件·服务 / 服务栈 / 站点 / 日志 / 设置（通知前端路由跳转）；
//! 6. **应用**：打开主窗口 / 检查更新 / 打开数据目录 / 关于 / 退出。
//!
//! 其它设计要点：
//! - **图标跟随状态**：运行中的服务数量直接画在托盘图标上（小角标），
//!   不用打开窗口就知道环境起没起来。
//! - **菜单按需重建**：服务状态变化后调用 `refresh()`，重建菜单项文本/勾选态。
//!   重建是廉价的（几十项），只在窗口事件与启停操作后触发，不做轮询。

use nsb_core::model::ServiceState;
use nsb_core::CoreState;
use std::sync::Arc;
use tauri::{
    image::Image,
    menu::{CheckMenuItem, IsMenuItem, Menu, MenuItem, PredefinedMenuItem, Submenu},
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime,
};

pub const TRAY_ID: &str = "main";

/// 服务分组标题（下标与 `group_of` 的返回值一一对应）
const GROUP_TITLES: [&str; 5] = ["Web 服务器", "语言运行时", "数据库", "缓存 / 队列", "工具 / 代理"];

/// 服务分组顺序：Web 服务器 → 语言运行时 → 数据库 → 缓存/队列 → 工具/代理（含长尾服务）
fn group_of(id: &str) -> u8 {
    let base = id.split('@').next().unwrap_or(id);
    match base {
        "nginx" | "apache" => 0,
        "php" | "node" | "python" | "go" => 1,
        s if s.starts_with("java") || s.starts_with("temurin") || s.contains("jdk") || s.contains("jre") => 1,
        "mysql" | "postgresql" | "mongodb" => 2,
        "redis" | "memcached" => 3,
        // mihomo 与清单里的长尾服务（caddy / minio / meilisearch …）统一收在最后一组
        _ => 4,
    }
}

/// 服务状态符号：一眼看出在跑 / 停了 / 出错了（勾选态另由 CheckMenuItem 表达）
fn status_glyph(state: &ServiceState) -> &'static str {
    match state {
        ServiceState::Running => "●",
        ServiceState::Error => "⚠",
        _ => "○",
    }
}

/// 服务 id → 托盘显示名（托盘空间有限，压短一些）
fn short_label(id: &str, fallback: &str) -> String {
    if let Some(ver) = id.strip_prefix("php@") {
        return format!("PHP {ver}");
    }
    if let Some(ver) = id.strip_prefix("mysql@") {
        return format!("MySQL {ver}");
    }
    match id {
        "nginx" => "Nginx".into(),
        "apache" => "Apache".into(),
        "redis" => "Redis".into(),
        "mihomo" => "mihomo (Clash)".into(),
        "postgresql" => "PostgreSQL".into(),
        "mongodb" => "MongoDB".into(),
        other => fallback.to_string().replace(other, other),
    }
}

/// 托盘图标：在应用图标右下角画一个「运行中服务数」的角标。
/// 手工拼 RGBA 像素（无第三方依赖），4x 超采样后降采样让边缘平滑。
fn badge_icon(base: &Image<'static>, running: usize) -> Image<'static> {
    if running == 0 {
        return base.clone();
    }
    let w = base.width() as usize;
    let h = base.height() as usize;
    let mut px = base.rgba().to_vec();
    if px.len() < w * h * 4 {
        return base.clone();
    }

    // 角标几何：右下角圆形，直径约 42%
    let d = (w.min(h) as f32 * 0.42).max(8.0);
    let cx = w as f32 - d / 2.0 - w as f32 * 0.03;
    let cy = h as f32 - d / 2.0 - h as f32 * 0.03;
    let r_out = d / 2.0;
    let r_in = r_out - (d * 0.11).max(1.0);
    // 底色跟随主题色相（青绿 = 健康），数量越多越偏暖
    let (br, bg, bb) = if running >= 5 {
        (74, 222, 128) // running
    } else {
        (96, 165, 250) // info
    };

    let ss = 3; // 每个像素采 3x3 个样本
    for y in 0..h {
        for x in 0..w {
            let mut hits = 0.0f32;
            for sy in 0..ss {
                for sx in 0..ss {
                    let px_ = x as f32 + (sx as f32 + 0.5) / ss as f32;
                    let py_ = y as f32 + (sy as f32 + 0.5) / ss as f32;
                    let dist = ((px_ - cx).powi(2) + (py_ - cy).powi(2)).sqrt();
                    if dist <= r_in {
                        hits += 1.0;
                    } else if dist <= r_out {
                        // 外圈做 1px 过渡，抗锯齿
                        hits += (r_out - dist) / (r_out - r_in);
                    }
                }
            }
            let cov = hits / (ss * ss) as f32;
            if cov <= 0.0 {
                continue;
            }
            let i = (y * w + x) * 4;
            if i + 3 >= px.len() {
                continue;
            }
            let a = cov.clamp(0.0, 1.0);
            // 覆盖式混色：角标压在原图标之上
            let (sr, sg, sb, sa) = (px[i] as f32 / 255.0, px[i + 1] as f32 / 255.0, px[i + 2] as f32 / 255.0, px[i + 3] as f32 / 255.0);
            let nr = (br as f32 / 255.0) * a + sr * (1.0 - a);
            let ng = (bg as f32 / 255.0) * a + sg * (1.0 - a);
            let nb = (bb as f32 / 255.0) * a + sb * (1.0 - a);
            let na = (a + sa * (1.0 - a)).clamp(0.0, 1.0);
            px[i] = (nr * 255.0) as u8;
            px[i + 1] = (ng * 255.0) as u8;
            px[i + 2] = (nb * 255.0) as u8;
            px[i + 3] = (na * 255.0) as u8;
        }
    }
    Image::new_owned(px, w as u32, h as u32)
}

/// 当前运行中的服务数（角标与 tooltip 用）
fn running_count(state: &CoreState) -> usize {
    state
        .service_status_list()
        .into_iter()
        .filter(|s| s.state == ServiceState::Running)
        .count()
}

/// 应用图标作为托盘图标基底（拷成 owned，便于加角标）
fn base_icon<R: Runtime>(app: &AppHandle<R>) -> Image<'static> {
    match app.default_window_icon() {
        Some(img) => Image::new_owned(img.rgba().to_vec(), img.width(), img.height()),
        // 打包路径异常时的兜底：1x1 透明像素
        None => Image::new_owned(vec![0, 0, 0, 0], 1, 1),
    }
}

/// 用系统文件管理器打开目录（「打开数据目录」用）。
/// 实现留在本模块内，与 lib.rs 的 open_target 同策略，不跨文件依赖。
fn open_dir(path: &std::path::Path) {
    let target = path.to_string_lossy().to_string();
    #[cfg(windows)]
    {
        let _ = std::process::Command::new("explorer").arg(&target).spawn();
    }
    #[cfg(not(windows))]
    {
        let _ = std::process::Command::new("open").arg(&target).spawn();
    }
}

/// 菜单构造：每次刷新都重建整棵树（便宜、无状态残留）
fn build_menu<R: Runtime>(app: &AppHandle<R>, state: &Arc<CoreState>) -> tauri::Result<Menu<R>> {
    let services = state.service_status_list();
    let running = services.iter().filter(|s| s.state == ServiceState::Running).count();

    /* ---- 抬头：应用名 + 版本 + 运行汇总（禁用项，只看不点） ---- */
    let pkg = app.package_info();
    let header_app = MenuItem::with_id(
        app,
        "tray:header:app",
        format!("{} v{}", pkg.name, pkg.version),
        false,
        None::<&str>,
    )?;
    let header_status = MenuItem::with_id(
        app,
        "tray:header:status",
        if services.is_empty() {
            "尚未安装任何服务".to_string()
        } else if running == 0 {
            format!("○ 全部已停止（共 {} 个服务）", services.len())
        } else {
            format!("● {running} / {} 个服务运行中", services.len())
        },
        false,
        None::<&str>,
    )?;

    /* ---- 第一组：一键操作（启动某个服务栈 / 停止全部） ---- */
    let mut stacks: Vec<nsb_core::model::Stack> = state.list_stacks().unwrap_or_default();
    stacks.truncate(6); // 托盘菜单别太长，完整列表在应用里
    let mut stack_items: Vec<Box<dyn IsMenuItem<R>>> = Vec::new();
    for s in &stacks {
        let (r, t) = nsb_core::stacks::status_of(&state.manager, s);
        let label = if t > 0 {
            format!("▶ {}（{r}/{t} 运行中）", s.name)
        } else {
            format!("▶ {}", s.name)
        };
        stack_items.push(Box::new(MenuItem::with_id(
            app,
            format!("stack:start:{}", s.id),
            label,
            t > 0, // 全部运行中就没必要再点
            None::<&str>,
        )?));
    }
    if !stack_items.is_empty() {
        stack_items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    stack_items.push(Box::new(MenuItem::with_id(
        app,
        "go:/stacks",
        "管理服务栈…",
        true,
        None::<&str>,
    )?));
    let stack_refs: Vec<&dyn IsMenuItem<R>> = stack_items.iter().map(|i| i.as_ref()).collect();
    let stack_menu = Submenu::with_items(app, "▶ 启动服务栈", true, &stack_refs)?;
    let stop_all = MenuItem::with_id(
        app,
        "services:stop_all",
        "■ 停止全部服务",
        running > 0,
        None::<&str>,
    )?;

    /* ---- 第二组：服务逐个启停（类别分组 + 勾选 = 运行中） ---- */
    let mut sorted = services.clone();
    sorted.sort_by_key(|s| (group_of(&s.id), s.id.clone()));
    let mut service_items: Vec<Box<dyn IsMenuItem<R>>> = Vec::new();
    let mut last_group: Option<u8> = None;
    for s in &sorted {
        let g = group_of(&s.id);
        if last_group != Some(g) {
            if last_group.is_some() {
                service_items.push(Box::new(PredefinedMenuItem::separator(app)?));
            }
            // 分组标题：禁用项，只当分隔用的「小标题」
            service_items.push(Box::new(MenuItem::with_id(
                app,
                format!("services:group:{g}"),
                GROUP_TITLES.get(g as usize).copied().unwrap_or("其它"),
                false,
                None::<&str>,
            )?));
            last_group = Some(g);
        }
        let name = short_label(&s.id, &s.label);
        let label = match s.port {
            Some(p) => format!("{} {name}  :{p}", status_glyph(&s.state)),
            None => format!("{} {name}", status_glyph(&s.state)),
        };
        service_items.push(Box::new(CheckMenuItem::with_id(
            app,
            format!("service:{}", s.id),
            label,
            true,
            s.state == ServiceState::Running,
            None::<&str>,
        )?));
    }
    let service_refs: Vec<&dyn IsMenuItem<R>> = service_items.iter().map(|i| i.as_ref()).collect();
    let services_menu = Submenu::with_items(
        app,
        if sorted.is_empty() {
            "服务（尚未安装任何服务）".to_string()
        } else {
            format!("服务（{} 个）", sorted.len())
        },
        !sorted.is_empty(),
        &service_refs,
    )?;

    /* ---- 第三组：站点快捷入口 ---- */
    let mut site_items: Vec<Box<dyn IsMenuItem<R>>> = Vec::new();
    let sites = nsb_core::sites::list(&state.store).unwrap_or_default();
    let ports = nsb_core::services::PortsProfile::from_settings(&state.store);
    for s in sites.iter().take(8) {
        let domain = s.domains.first().cloned().unwrap_or_else(|| "localhost".into());
        let scheme = if s.https { "https" } else { "http" };
        let port = if s.https { ports.https } else { ports.http };
        let std_port = (s.https && port == 443) || (!s.https && port == 80);
        let url = if std_port {
            format!("{scheme}://{domain}")
        } else {
            format!("{scheme}://{domain}:{port}")
        };
        site_items.push(Box::new(MenuItem::with_id(
            app,
            format!("site:{url}"),
            format!("🌐 {}", s.name),
            true,
            None::<&str>,
        )?));
    }
    if !site_items.is_empty() {
        site_items.push(Box::new(PredefinedMenuItem::separator(app)?));
    }
    site_items.push(Box::new(MenuItem::with_id(
        app,
        "go:/sites",
        "管理站点…",
        true,
        None::<&str>,
    )?));
    let site_refs: Vec<&dyn IsMenuItem<R>> = site_items.iter().map(|i| i.as_ref()).collect();
    let sites_menu = Submenu::with_items(app, format!("站点（{} 个）", sites.len()), true, &site_refs)?;

    /* ---- 第四组：应用内导航 ---- */
    let nav = |label: &str, id: &str| -> tauri::Result<MenuItem<R>> {
        MenuItem::with_id(app, id, label, true, None::<&str>)
    };
    let nav_dash = nav("总览", "go:/")?;
    let nav_pkgs = nav("套件 / 服务", "go:/packages")?;
    let nav_stacks = nav("服务栈", "go:/stacks")?;
    let nav_sites = nav("站点", "go:/sites")?;
    let nav_logs = nav("日志", "go:/logs")?;
    let nav_settings = nav("设置", "go:/settings")?;

    /* ---- 第五组：应用（窗口 / 更新 / 数据目录 / 关于 / 退出） ---- */
    let show = MenuItem::with_id(app, "app:show", "打开主窗口", true, None::<&str>)?;
    let check_update = MenuItem::with_id(app, "app:check_update", "检查更新…", true, None::<&str>)?;
    let open_data = MenuItem::with_id(app, "app:open_data_dir", "打开数据目录", true, None::<&str>)?;
    let about = MenuItem::with_id(app, "app:about", "关于 NiceServBay", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "app:quit", "退出 NiceServBay", true, None::<&str>)?;

    /* ---- 组装 ---- */
    Menu::with_items(
        app,
        &[
            &header_app,
            &header_status,
            &PredefinedMenuItem::separator(app)?,
            &stack_menu,
            &stop_all,
            &PredefinedMenuItem::separator(app)?,
            &services_menu,
            &sites_menu,
            &PredefinedMenuItem::separator(app)?,
            &nav_dash,
            &nav_pkgs,
            &nav_stacks,
            &nav_sites,
            &nav_logs,
            &nav_settings,
            &PredefinedMenuItem::separator(app)?,
            &show,
            &check_update,
            &open_data,
            &about,
            &PredefinedMenuItem::separator(app)?,
            &quit,
        ],
    )
}

/// 应用托盘图标 + 菜单（setup 阶段调用一次）
pub fn build<R: Runtime>(app: &AppHandle<R>, state: Arc<CoreState>) -> tauri::Result<()> {
    let menu = build_menu(app, &state)?;
    let icon = badge_icon(&base_icon(app), running_count(&state));

    let tray_state = state.clone();
    let app_handle = app.clone();
    TrayIconBuilder::with_id(TRAY_ID)
        .icon(icon)
        .menu(&menu)
        .show_menu_on_left_click(false)
        .tooltip(tooltip_text(&state))
        .on_tray_icon_event(move |_tray, event| match event {
            // 左键点图标：把窗口叫到前台（比右键菜单快一步）
            TrayIconEvent::Click {
                button: MouseButton::Left,
                button_state: MouseButtonState::Up,
                ..
            } => show_main(&app_handle),
            TrayIconEvent::DoubleClick { .. } => show_main(&app_handle),
            _ => {}
        })
        .on_menu_event(move |app, event| {
            let id = event.id().as_ref().to_string();
            handle_menu_event(app, &tray_state, &id);
        })
        .build(app)?;
    Ok(())
}

fn tooltip_text(state: &Arc<CoreState>) -> String {
    let services = state.service_status_list();
    let running = services.iter().filter(|s| s.state == ServiceState::Running).count();
    if running == 0 {
        "NiceServBay — 本地开发环境（全部已停止）".into()
    } else {
        format!("NiceServBay — {running}/{} 个服务运行中", services.len())
    }
}

/// 菜单事件分发：id 前缀决定动作（`service:` / `stack:start:` / `site:` / `go:` / `app:`）
fn handle_menu_event<R: Runtime>(app: &AppHandle<R>, state: &Arc<CoreState>, id: &str) {
    // 服务启停：勾选态取反
    if let Some(sid) = id.strip_prefix("service:") {
        let sid = sid.to_string();
        let st = state.clone();
        let app2 = app.clone();
        std::thread::spawn(move || {
            let running = st
                .manager
                .snapshot(&sid)
                .map(|s| s.state == ServiceState::Running || s.state == ServiceState::Starting)
                .unwrap_or(false);
            let r = if running { st.stop_service(&sid) } else { st.start_service(&sid) };
            let _ = r;
            refresh(&app2);
        });
        return;
    }

    // 一键启动某个栈
    if let Some(rest) = id.strip_prefix("stack:start:") {
        let st = state.clone();
        let stack_id = rest.to_string();
        let app2 = app.clone();
        std::thread::spawn(move || {
            let _ = st.start_stack(&stack_id);
            refresh(&app2);
        });
        return;
    }

    // 打开站点 URL
    if let Some(url) = id.strip_prefix("site:") {
        let url = url.to_string();
        // 用系统默认浏览器打开：Windows 走 cmd start，macOS 走 open
        #[cfg(windows)]
        {
            let _ = std::process::Command::new("cmd")
                .args(["/c", "start", "", &url])
                .spawn();
        }
        #[cfg(not(windows))]
        {
            let _ = std::process::Command::new("open").arg(&url).spawn();
        }
        return;
    }

    // 跳转应用内页面：显示窗口 + 通知前端路由
    if let Some(path) = id.strip_prefix("go:") {
        let path = path.to_string();
        show_main(app);
        let _ = app.emit("tray://navigate", serde_json::json!({ "path": path }));
        return;
    }

    match id {
        "app:show" => show_main(app),
        // 「检查更新」「关于」都由前端弹窗呈现：先把窗口叫出来，再让前端开弹窗
        "app:check_update" => {
            show_main(app);
            let _ = app.emit("tray://check-update", serde_json::json!({}));
        }
        "app:about" => {
            show_main(app);
            let _ = app.emit("tray://about", serde_json::json!({}));
        }
        "app:open_data_dir" => open_dir(&state.paths.base),
        "services:stop_all" => {
            let st = state.clone();
            let app2 = app.clone();
            std::thread::spawn(move || {
                nsb_core::ops::stop_all(&st.store, &st.paths, &st.manager);
                let path = st.paths.data().join("run").join("pids.json");
                let _ = std::fs::remove_file(path);
                refresh(&app2);
            });
        }
        "app:quit" => {
            crate::stop_all_and_clear_pidfile(state);
            app.exit(0);
        }
        _ => {}
    }
}

fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 重建菜单与图标（服务状态变化后调用：启停、栈启动、端口清理）
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let state = app.state::<Arc<CoreState>>().inner().clone();
    if let Ok(menu) = build_menu(app, &state) {
        let _ = tray.set_menu(Some(menu));
    }
    let _ = tray.set_icon(Some(badge_icon(&base_icon(app), running_count(&state))));
    let _ = tray.set_tooltip(Some(tooltip_text(&state)));
}
