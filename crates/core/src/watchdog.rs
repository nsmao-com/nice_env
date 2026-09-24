//! 服务看门狗：服务意外退出后自动拉起。
//!
//! FlyEnv / ServBay 都有「崩溃自动重启」。这里的关键是把「意外退出」和
//! 「用户主动停止」区分开——否则用户点停止，看门狗立刻又给拉起来，
//! 那个体验比不重启还糟。
//!
//! 设计：
//! - 只有**曾经成功 running 过**且**不是用户主动停止**的服务才纳入监控；
//! - 崩溃判定依据是 `snapshot()` 里已有的「pids 全死但状态仍为 Running」检测，
//!   所以看门狗不需要自己维护一套进程状态；
//! - 指数退避：连续崩溃时拉起的间隔递增（2s→4s→8s…最多 60s），
//!   避免一个永远起不来的服务把 CPU 打满；
//! - 上限次数：默认 5 次，超过后停止尝试并明确告知用户，
//!   而不是无限重试、日志里刷满同样的错误。
//! - 依赖顺序：MySQL 崩了不该去重启依赖它的站点进程，所以只重启服务本身。

use std::collections::HashMap;
use std::time::{Duration, Instant};

use parking_lot::Mutex;

use crate::model::ServiceState;

/// 一个服务的看门狗记录
#[derive(Debug, Clone)]
pub struct WatchEntry {
    /// 是否启用监控
    pub enabled: bool,
    /// 已连续尝试重启次数
    pub attempts: u32,
    /// 下一次允许重启的时间点（指数退避）
    pub next_attempt_at: Option<Instant>,
    /// 最后一次记录到的状态，用于检测「running → 不在跑」的跳变
    pub last_state: ServiceState,
    /// 用户主动请求停止——设上后看门狗不再干预，直到下次成功启动
    pub user_stopped: bool,
    /// 重启历史（时间戳 + 是否成功），供 UI 展示「最近自动重启过 N 次」
    pub restarts: Vec<(i64, bool)>,
}

impl Default for WatchEntry {
    fn default() -> Self {
        Self {
            enabled: false,
            attempts: 0,
            next_attempt_at: None,
            last_state: ServiceState::Stopped,
            user_stopped: false,
            restarts: Vec::new(),
        }
    }
}

/// 看门狗配置（存在 settings 里由用户控制）
#[derive(Debug, Clone, Copy)]
pub struct WatchdogConfig {
    /// 总开关
    pub enabled: bool,
    /// 单个服务的最大连续重启次数
    pub max_attempts: u32,
    /// 首次退避间隔（秒），之后翻倍
    pub base_delay_sec: u64,
    /// 退避上限（秒）
    pub max_delay_sec: u64,
    /// 轮询间隔（秒）
    pub interval_sec: u64,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            // 默认关闭：自动重启是「有副作用」的行为（会占端口、写日志），
            // 让用户显式打开，而不是装上就擅自重启进程。
            enabled: false,
            max_attempts: 5,
            base_delay_sec: 2,
            max_delay_sec: 60,
            interval_sec: 3,
        }
    }
}

/// 看门狗状态（不带 Instant，方便序列化给前端）
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchdogStatus {
    pub enabled: bool,
    pub max_attempts: u32,
    pub interval_sec: u64,
    /// 服务 id → 该服务的监控信息
    pub watched: Vec<WatchedService>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WatchedService {
    pub id: String,
    pub enabled: bool,
    pub attempts: u32,
    /// 已用尽重试次数，停止尝试
    pub exhausted: bool,
    pub restart_count: u32,
    /// 最后一次自动重启的 Unix 秒
    pub last_restart_at: Option<i64>,
}

pub struct Watchdog {
    entries: Mutex<HashMap<String, WatchEntry>>,
}

impl Watchdog {
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// 用户主动启动了服务 → 清空退避与「主动停止」标记
    pub fn note_started(&self, id: &str) {
        let mut m = self.entries.lock();
        let e = m.entry(id.to_string()).or_default();
        e.user_stopped = false;
        e.attempts = 0;
        e.next_attempt_at = None;
        e.last_state = ServiceState::Running;
        e.enabled = true;
    }

    /// 用户主动停止了服务 → 看门狗不得再拉起
    pub fn note_user_stopped(&self, id: &str) {
        let mut m = self.entries.lock();
        let e = m.entry(id.to_string()).or_default();
        e.user_stopped = true;
        e.last_state = ServiceState::Stopped;
        e.attempts = 0;
        e.next_attempt_at = None;
    }

    /// 记录一次重启尝试的结果
    pub fn note_restart(&self, id: &str, ok: bool, cfg: &WatchdogConfig) {
        let mut m = self.entries.lock();
        let e = m.entry(id.to_string()).or_default();
        e.restarts.push((chrono::Local::now().timestamp(), ok));
        // 只留最近 20 条，别让这个 vec 无限长
        if e.restarts.len() > 20 {
            let drop = e.restarts.len() - 20;
            e.restarts.drain(0..drop);
        }
        if ok {
            e.attempts = 0;
            e.next_attempt_at = None;
            e.last_state = ServiceState::Running;
        } else {
            e.attempts = e.attempts.saturating_add(1);
            // 指数退避，避免一个永远起不来的服务把机器拖垮
            let factor = 2u64.saturating_pow(e.attempts.min(6));
            let delay = (cfg.base_delay_sec.saturating_mul(factor)).min(cfg.max_delay_sec);
            e.next_attempt_at = Some(Instant::now() + Duration::from_secs(delay));
        }
    }

    /// 判断某个服务现在是否该被拉起。
    ///
    /// 返回 true 的条件（全部满足）：
    /// 1. 看门狗总开关开着，且该服务被监控；
    /// 2. 用户没有主动停过它；
    /// 3. 没超过最大尝试次数；
    /// 4. 退避时间已到。
    ///
    /// 注意：调用方负责判断「服务确实不在跑」——本函数不读进程状态，
    /// 这样它可以被单测覆盖，不需要真的起进程。
    pub fn should_restart(&self, id: &str, cfg: &WatchdogConfig) -> bool {
        if !cfg.enabled {
            return false;
        }
        let m = self.entries.lock();
        let Some(e) = m.get(id) else { return false };
        if !e.enabled || e.user_stopped {
            return false;
        }
        if e.attempts >= cfg.max_attempts {
            return false;
        }
        match e.next_attempt_at {
            Some(t) => Instant::now() >= t,
            None => true,
        }
    }

    /// 是否已用尽重试
    pub fn exhausted(&self, id: &str, cfg: &WatchdogConfig) -> bool {
        let m = self.entries.lock();
        m.get(id)
            .map(|e| e.enabled && !e.user_stopped && e.attempts >= cfg.max_attempts)
            .unwrap_or(false)
    }

    /// 前端展示用
    pub fn status(&self, cfg: &WatchdogConfig) -> WatchdogStatus {
        let m = self.entries.lock();
        let mut watched: Vec<WatchedService> = m
            .iter()
            .map(|(id, e)| WatchedService {
                id: id.clone(),
                enabled: e.enabled,
                attempts: e.attempts,
                exhausted: e.enabled && !e.user_stopped && e.attempts >= cfg.max_attempts,
                restart_count: e.restarts.len() as u32,
                last_restart_at: e.restarts.last().map(|(t, _)| *t),
            })
            .collect();
        watched.sort_by(|a, b| a.id.cmp(&b.id));
        WatchdogStatus {
            enabled: cfg.enabled,
            max_attempts: cfg.max_attempts,
            interval_sec: cfg.interval_sec,
            watched,
        }
    }

    /// 显式打开/关闭某个服务的监控
    pub fn set_enabled(&self, id: &str, on: bool) {
        let mut m = self.entries.lock();
        let e = m.entry(id.to_string()).or_default();
        e.enabled = on;
        if on {
            e.user_stopped = false;
            e.attempts = 0;
            e.next_attempt_at = None;
        }
    }

    /// 清空重试计数（UI 上的「重试」按钮）
    pub fn reset(&self, id: &str) {
        let mut m = self.entries.lock();
        if let Some(e) = m.get_mut(id) {
            e.attempts = 0;
            e.next_attempt_at = None;
        }
    }

    /// 服务被卸载/移除时清理
    pub fn forget(&self, id: &str) {
        self.entries.lock().remove(id);
    }
}

impl Default for Watchdog {
    fn default() -> Self {
        Self::new()
    }
}

/// 从设置里读看门狗配置
pub fn config_from_store(store: &crate::store::Store) -> WatchdogConfig {
    let d = WatchdogConfig::default();
    WatchdogConfig {
        enabled: store
            .get_setting("watchdogEnabled")
            .map(|v| v == "true")
            .unwrap_or(d.enabled),
        max_attempts: store
            .get_setting("watchdogMaxAttempts")
            .and_then(|v| v.parse().ok())
            .unwrap_or(d.max_attempts),
        base_delay_sec: d.base_delay_sec,
        max_delay_sec: d.max_delay_sec,
        interval_sec: store
            .get_setting("watchdogIntervalSec")
            .and_then(|v| v.parse().ok())
            .unwrap_or(d.interval_sec),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn on() -> WatchdogConfig {
        WatchdogConfig {
            enabled: true,
            ..Default::default()
        }
    }

    #[test]
    fn disabled_config_never_restarts() {
        let w = Watchdog::new();
        w.note_started("nginx");
        let cfg = WatchdogConfig {
            enabled: false,
            ..Default::default()
        };
        assert!(!w.should_restart("nginx", &cfg));
    }

    #[test]
    fn unknown_service_is_not_restarted() {
        let w = Watchdog::new();
        assert!(!w.should_restart("nothing", &on()));
    }

    #[test]
    fn started_service_becomes_watched() {
        let w = Watchdog::new();
        w.note_started("mysql");
        assert!(w.should_restart("mysql", &on()));
    }

    #[test]
    fn user_stopped_service_is_never_restarted() {
        // 这是最关键的一条：用户点停止后看门狗不能又给拉起来
        let w = Watchdog::new();
        w.note_started("redis");
        w.note_user_stopped("redis");
        assert!(!w.should_restart("redis", &on()));
        // 用户再次手动启动后，才恢复监控
        w.note_started("redis");
        assert!(w.should_restart("redis", &on()));
    }

    #[test]
    fn failed_restart_backs_off() {
        let w = Watchdog::new();
        w.note_started("nginx");
        let cfg = on();
        w.note_restart("nginx", false, &cfg);
        // 刚失败过，退避期内不该立刻再试
        assert!(!w.should_restart("nginx", &cfg), "首次失败后应退避");
    }

    #[test]
    fn successful_restart_clears_attempts_and_backoff() {
        let w = Watchdog::new();
        w.note_started("nginx");
        let cfg = on();
        w.note_restart("nginx", false, &cfg);
        w.note_restart("nginx", true, &cfg);
        assert!(w.should_restart("nginx", &cfg), "成功后应立刻可再次监控");
        let st = w.status(&cfg);
        let e = st.watched.iter().find(|x| x.id == "nginx").unwrap();
        assert_eq!(e.attempts, 0);
        assert_eq!(e.restart_count, 2);
    }

    #[test]
    fn gives_up_after_max_attempts() {
        let w = Watchdog::new();
        w.note_started("bad");
        let cfg = WatchdogConfig {
            max_attempts: 3,
            ..on()
        };
        for _ in 0..3 {
            w.note_restart("bad", false, &cfg);
        }
        assert!(!w.should_restart("bad", &cfg), "达上限后应停止尝试");
        assert!(w.exhausted("bad", &cfg));
    }

    #[test]
    fn reset_clears_exhaustion() {
        let w = Watchdog::new();
        w.note_started("bad");
        let cfg = WatchdogConfig {
            max_attempts: 2,
            ..on()
        };
        w.note_restart("bad", false, &cfg);
        w.note_restart("bad", false, &cfg);
        assert!(w.exhausted("bad", &cfg));
        w.reset("bad");
        assert!(!w.exhausted("bad", &cfg));
        assert!(w.should_restart("bad", &cfg));
    }

    #[test]
    fn backoff_is_capped() {
        let w = Watchdog::new();
        w.note_started("flap");
        let cfg = WatchdogConfig {
            base_delay_sec: 10,
            max_delay_sec: 30,
            max_attempts: 100,
            ..on()
        };
        // 连续失败很多次，delay 不应超过 max_delay_sec
        for _ in 0..10 {
            w.note_restart("flap", false, &cfg);
        }
        // 通过内部状态间接验证：记录下一次尝试时间不超过 max_delay
        let m = w.entries.lock();
        let e = m.get("flap").unwrap();
        let delta = e
            .next_attempt_at
            .map(|t| t.saturating_duration_since(Instant::now()))
            .unwrap_or_default();
        assert!(
            delta <= Duration::from_secs(cfg.max_delay_sec),
            "退避应被上限截断，实际 {delta:?}"
        );
    }

    #[test]
    fn restart_history_is_bounded() {
        let w = Watchdog::new();
        w.note_started("x");
        let cfg = on();
        for _ in 0..50 {
            w.note_restart("x", true, &cfg);
        }
        let st = w.status(&cfg);
        let e = st.watched.iter().find(|x| x.id == "x").unwrap();
        assert!(
            e.restart_count <= 20,
            "历史应被截断，实际 {}",
            e.restart_count
        );
    }

    #[test]
    fn set_enabled_false_disables_monitoring() {
        let w = Watchdog::new();
        w.note_started("nginx");
        w.set_enabled("nginx", false);
        assert!(!w.should_restart("nginx", &on()));
    }

    #[test]
    fn forget_removes_entry() {
        let w = Watchdog::new();
        w.note_started("gone");
        w.forget("gone");
        assert!(!w.should_restart("gone", &on()));
    }

    #[test]
    fn status_reports_last_restart_time() {
        let w = Watchdog::new();
        w.note_started("nginx");
        w.note_restart("nginx", true, &on());
        let st = w.status(&on());
        let e = st.watched.iter().find(|x| x.id == "nginx").unwrap();
        assert!(e.last_restart_at.is_some());
        assert!(!e.exhausted);
    }

    #[test]
    fn config_defaults_to_disabled() {
        // 自动重启有副作用，必须默认关
        assert!(!WatchdogConfig::default().enabled);
    }
}
