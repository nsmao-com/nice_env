//! 托盘：自绘角标图标 + 仿搜狗风格的托盘面板（独立 webview 小窗）。
//!
//! 原生右键菜单（Win32 风格）观感差，改为**自绘面板**：
//! - 点击托盘图标（左键）弹出面板窗口：卡片式布局，含状态抬头、快捷操作宫格、
//!   服务/站点/服务栈快捷行、应用内导航与退出入口，交互与主窗口一致；
//! - 面板是无边框 + 透明 + 置顶的小窗，贴着托盘图标上方弹出，失焦自动隐藏；
//! - 服务状态变化后 `refresh()` 重建角标/tooltip，并推送 `tray://state`
//!   让面板实时重渲染（在主窗口里启停服务，面板打开时也跟着变）。
//!
//! 面板 UI 在 `apps/web/public/tray_panel.html`（自包含 HTML/CSS/JS，
//! dev 走 Next dev server、打包走 Tauri 资产协议，两端同路径）。
//! 数据经 `tray_panel_state` 命令拉取，动作直接复用主窗口的既有命令
//! （start_service / start_stack / open_in_browser …），新增命令只补托盘特有行为。

use nsb_core::model::ServiceState;
use nsb_core::CoreState;
use std::sync::Arc;
use tauri::{
    image::Image,
    tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent},
    AppHandle, Emitter, Manager, Runtime, State,
};

pub const TRAY_ID: &str = "main";
/// 托盘面板窗口 label（与 capabilities 里的窗口名对应）
pub const PANEL_LABEL: &str = "tray-panel";
/// 面板逻辑宽度（逻辑像素；高度由前端按内容自适应上报）
pub const PANEL_W: f64 = 384.0;

/// 最近一次托盘图标的屏幕矩形（物理像素：x, y, w, h），用于把面板弹到图标旁边
static LAST_TRAY_RECT: std::sync::Mutex<(f64, f64, f64, f64)> = std::sync::Mutex::new((0.0, 0.0, 0.0, 0.0));
/// 面板因失焦被隐藏的时间点：Windows 上点托盘会先让面板失焦（触发 hide），
/// 紧接着才收到 Click 事件。若刚因失焦隐藏过，就认定这次点击是「想收起」，
/// 不再重新弹出——否则面板会永远关不上。
static PANEL_BLURRED_AT: std::sync::Mutex<Option<std::time::Instant>> = std::sync::Mutex::new(None);

/* ================= 服务元数据 ================= */

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

/// 服务 id → 面板显示名（面板行高有限，压短一些）
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
        _ => fallback.to_string(),
    }
}

/// ServiceState → 前端字符串（面板按此渲染状态点与可点性）
fn state_tag(s: &ServiceState) -> &'static str {
    match s {
        ServiceState::Running => "running",
        ServiceState::Starting => "starting",
        ServiceState::Stopping => "stopping",
        ServiceState::Error => "error",
        _ => "stopped",
    }
}

/* ================= 托盘图标（角标 + tooltip） ================= */

/// 托盘图标：在应用图标右下角画一个「运行中服务数」的角标。
/// 手工拼 RGBA 像素（无第三方依赖），3x 超采样后混合让边缘平滑。
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
    // 底色跟随状态（数量多 = 绿 = 健康，少 = 蓝 = 信息）
    let (br, bg, bb) = if running >= 5 {
        (74, 222, 128) // green
    } else {
        (96, 165, 250) // blue
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

fn tooltip_text(state: &Arc<CoreState>) -> String {
    let services = state.service_status_list();
    let running = services.iter().filter(|s| s.state == ServiceState::Running).count();
    if running == 0 {
        "NiceEnv — 本地开发环境（全部已停止）".into()
    } else {
        format!("NiceEnv — {running}/{} 个服务运行中", services.len())
    }
}

/* ================= 面板数据 ================= */

/// 面板渲染所需的全部状态（一次拉齐，前端零业务逻辑）
pub fn panel_state_json<R: Runtime>(app: &AppHandle<R>, state: &Arc<CoreState>) -> serde_json::Value {
    let services = state.service_status_list();
    let running = services.iter().filter(|s| s.state == ServiceState::Running).count();
    let active = services
        .iter()
        .filter(|s| matches!(s.state, ServiceState::Running | ServiceState::Starting))
        .count();

    let mut sorted = services.clone();
    sorted.sort_by_key(|s| (group_of(&s.id), s.id.clone()));
    let services_json: Vec<serde_json::Value> = sorted
        .iter()
        .map(|s| {
            serde_json::json!({
                "id": s.id,
                "label": short_label(&s.id, &s.label),
                "state": state_tag(&s.state),
                "group": group_of(&s.id),
                "port": s.port,
            })
        })
        .collect();

    let stacks_json: Vec<serde_json::Value> = state
        .list_stacks()
        .unwrap_or_default()
        .into_iter()
        .take(4)
        .map(|st| {
            let (r, t) = nsb_core::stacks::status_of(&state.manager, &st);
            serde_json::json!({ "id": st.id, "name": st.name, "running": r, "total": t })
        })
        .collect();

    let ports = nsb_core::services::PortsProfile::from_settings(&state.store);
    let sites_json: Vec<serde_json::Value> = nsb_core::sites::list(&state.store)
        .unwrap_or_default()
        .into_iter()
        .take(6)
        .map(|s| {
            let domain = s.domains.first().cloned().unwrap_or_else(|| "localhost".into());
            let scheme = if s.https { "https" } else { "http" };
            let port = if s.https { ports.https } else { ports.http };
            let std_port = (s.https && port == 443) || (!s.https && port == 80);
            let url = if std_port {
                format!("{scheme}://{domain}")
            } else {
                format!("{scheme}://{domain}:{port}")
            };
            serde_json::json!({ "id": s.id, "name": s.name, "url": url, "https": s.https })
        })
        .collect();

    serde_json::json!({
        "version": app.package_info().version.to_string(),
        "appearance": state.store.get_setting("appearance").unwrap_or_else(|| "light".into()),
        "running": running,
        "active": active,
        "total": services.len(),
        "dataDir": state.paths.base.to_string_lossy(),
        "groupTitles": GROUP_TITLES,
        "services": services_json,
        "stacks": stacks_json,
        "sites": sites_json,
    })
}

/// 把最新状态推给面板（打开中才会真正收到；隐藏时收不到也无妨，
/// 下次弹出前会再推一次）
fn push_state<R: Runtime>(app: &AppHandle<R>, state: &Arc<CoreState>) {
    if app.get_webview_window(PANEL_LABEL).is_some() {
        let _ = app.emit("tray://state", panel_state_json(app, state));
    }
}

/* ================= 面板窗口 ================= */

/// 创建托盘面板小窗：无边框 + 透明 + 置顶 + 不进任务栏，初始隐藏。
/// 提前建好（webview 预加载），弹出时零白屏。
fn build_panel<R: Runtime>(app: &AppHandle<R>) -> tauri::Result<()> {
    let panel = tauri::WebviewWindowBuilder::new(
        app,
        PANEL_LABEL,
        tauri::WebviewUrl::App("tray_panel.html".into()),
    )
    .title("NiceEnv")
    .inner_size(PANEL_W, 620.0)
    .visible(false)
    .decorations(false)
    .transparent(true)
    .resizable(false)
    .skip_taskbar(true)
    .always_on_top(true)
    // Windows 上关掉系统投影：投影是矩形包着圆角，会露怯
    .shadow(false)
    .focused(false)
    .build()?;

    let handle = app.clone();
    panel.on_window_event(move |e| {
        if let tauri::WindowEvent::Focused(false) = e {
            *PANEL_BLURRED_AT.lock().unwrap_or_else(|p| p.into_inner()) = Some(std::time::Instant::now());
            if let Some(w) = handle.get_webview_window(PANEL_LABEL) {
                let _ = w.hide();
            }
        }
    });
    Ok(())
}

/// 把面板摆到托盘图标旁边：右缘对齐图标右缘、底缘悬在图标上方一点，
/// 再夹回图标所在显示器的可视范围（任务栏在上方/侧方时也不出屏）。
fn position_panel<R: Runtime>(app: &AppHandle<R>) {
    let Some(panel) = app.get_webview_window(PANEL_LABEL) else {
        return;
    };
    let Ok(size) = panel.outer_size() else { return };
    let (tx, ty, tw, _) = *LAST_TRAY_RECT.lock().unwrap_or_else(|p| p.into_inner());

    let mut x = tx + tw - size.width as f64;
    let mut y = ty - size.height as f64 - 6.0;

    // 以图标中心点找到所在显示器（多显示器 + 任务栏不在底部时兜住）
    if let Ok(monitors) = panel.available_monitors() {
        let cx = tx + tw / 2.0;
        let cy = ty + 8.0;
        let mon = monitors.iter().find(|m| {
            let (mx, my) = (m.position().x as f64, m.position().y as f64);
            let (mw, mh) = (m.size().width as f64, m.size().height as f64);
            cx >= mx && cx < mx + mw && cy >= my && cy < my + mh
        });
        if let Some(m) = mon {
            let margin = 8.0;
            let min_x = m.position().x as f64 + margin;
            let max_x = (m.position().x + m.size().width as i32) as f64 - size.width as f64 - margin;
            let min_y = m.position().y as f64 + margin;
            x = x.clamp(min_x, max_x.max(min_x));
            y = y.max(min_y);
        }
    }

    let _ = panel.set_position(tauri::Position::Physical(tauri::PhysicalPosition::new(x as i32, y as i32)));
}

/// 托盘点击 → 弹出 / 收起面板。（tx/ty/tw/th = 托盘图标矩形，物理像素）
fn toggle_panel<R: Runtime>(app: &AppHandle<R>, tx: f64, ty: f64, tw: f64, th: f64) {
    *LAST_TRAY_RECT.lock().unwrap_or_else(|p| p.into_inner()) = (tx, ty, tw, th);
    let Some(panel) = app.get_webview_window(PANEL_LABEL) else {
        return;
    };

    // 刚因失焦藏起来过 → 这次点击视为「收起」（见 PANEL_BLURRED_AT 注释）
    let blurred_recently = match *PANEL_BLURRED_AT.lock().unwrap_or_else(|p| p.into_inner()) {
        Some(t) => t.elapsed().as_millis() < 350,
        None => false,
    };
    let visible = panel.is_visible().unwrap_or(false);
    if visible || blurred_recently {
        if visible {
            let _ = panel.hide();
        }
        return;
    }

    let state = app.state::<Arc<CoreState>>().inner().clone();
    push_state(app, &state);
    position_panel(app);
    let _ = panel.show();
    let _ = panel.set_focus();
}

/* ================= 托盘装配 ================= */

/// 托盘图标 + 面板窗口（setup 阶段调用一次）
pub fn build<R: Runtime>(app: &AppHandle<R>, state: Arc<CoreState>) -> tauri::Result<()> {
    build_panel(app)?;

    TrayIconBuilder::with_id(TRAY_ID)
        .icon(badge_icon(&base_icon(app), running_count(&state)))
        .tooltip(tooltip_text(&state))
        .on_tray_icon_event(move |tray, event| match event {
            // 左键：进主窗口；右键：弹出/收起自绘面板（原生菜单已弃用）
            TrayIconEvent::Click {
                button,
                button_state: MouseButtonState::Up,
                rect,
                ..
            } => {
                let app = tray.app_handle();
                // 事件坐标可能是物理或逻辑，统一换算成物理像素
                let (tx, ty) = match rect.position {
                    tauri::Position::Physical(p) => (p.x as f64, p.y as f64),
                    tauri::Position::Logical(p) => (p.x, p.y),
                };
                let (tw, th) = match rect.size {
                    tauri::Size::Physical(s) => (s.width as f64, s.height as f64),
                    tauri::Size::Logical(s) => (s.width, s.height),
                };
                match button {
                    MouseButton::Left => show_main(app),
                    MouseButton::Right => toggle_panel(app, tx, ty, tw, th),
                    _ => {}
                }
            }
            _ => {}
        })
        .build(app)?;

    Ok(())
}

fn show_main<R: Runtime>(app: &AppHandle<R>) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.show();
        let _ = w.unminimize();
        let _ = w.set_focus();
    }
}

/// 重建图标角标 + 推送面板状态（服务状态变化后调用：启停、栈启动、端口清理）
pub fn refresh<R: Runtime>(app: &AppHandle<R>) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };
    let state = app.state::<Arc<CoreState>>().inner().clone();
    let _ = tray.set_icon(Some(badge_icon(&base_icon(app), running_count(&state))));
    let _ = tray.set_tooltip(Some(tooltip_text(&state)));
    push_state(app, &state);
}

/* ================= 面板专用命令 ================= */

/// 面板初始数据（HTML 加载后 invoke 一次；此后靠 tray://state 增量推送）
#[tauri::command]
pub fn tray_panel_state(app: tauri::AppHandle, state: State<'_, Arc<CoreState>>) -> serde_json::Value {
    panel_state_json(&app, state.inner())
}

/// 一键停止全部服务（后台执行，完成后 tray://state 自动刷新面板与角标）
#[tauri::command]
pub fn tray_stop_all(app: tauri::AppHandle, state: State<'_, Arc<CoreState>>) -> bool {
    let st = state.inner().clone();
    let app2 = app.clone();
    std::thread::spawn(move || {
        nsb_core::ops::stop_all(&st.store, &st.paths, &st.manager);
        let path = st.paths.data().join("run").join("pids.json");
        let _ = std::fs::remove_file(path);
        refresh(&app2);
    });
    true
}

/// 打开主窗口；path → 路由跳转，action → 让前端开「检查更新 / 关于」弹窗。
/// 同时收起面板（主窗口抢焦点后面板也该消失）。
#[tauri::command]
pub fn tray_open_main<R: Runtime>(app: tauri::AppHandle<R>, path: Option<String>, action: Option<String>) -> bool {
    show_main(&app);
    match action.as_deref() {
        Some("check-update") => {
            let _ = app.emit("tray://check-update", serde_json::json!({}));
        }
        Some("about") => {
            let _ = app.emit("tray://about", serde_json::json!({}));
        }
        _ => {}
    }
    if let Some(p) = path {
        let _ = app.emit("tray://navigate", serde_json::json!({ "path": p }));
    }
    if let Some(panel) = app.get_webview_window(PANEL_LABEL) {
        let _ = panel.hide();
    }
    true
}

/// 面板按内容自适应高度后上报（Rust 侧改窗口尺寸并保持贴着托盘）
#[tauri::command]
pub fn tray_panel_resize(app: tauri::AppHandle, height: f64) -> bool {
    let Some(panel) = app.get_webview_window(PANEL_LABEL) else {
        return false;
    };
    let h = height.clamp(320.0, 680.0);
    let _ = panel.set_size(tauri::LogicalSize::new(PANEL_W, h));
    position_panel(&app);
    true
}

/// 面板主动收起（Esc 键）
#[tauri::command]
pub fn tray_panel_hide(app: tauri::AppHandle) -> bool {
    if let Some(panel) = app.get_webview_window(PANEL_LABEL) {
        let _ = panel.hide();
    }
    true
}
