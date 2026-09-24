//! 日志导出：把当前（可能已过滤的）日志存成文件。
//!
//! 内容由前端传入，后端只负责落盘与命名。这样做是有意的：
//! 用户在界面上做了级别过滤 + 关键字搜索，导出必须与**他看到的**一致，
//! 否则会出现「我搜了 error，导出的却是全量」这种让人不信任的行为。
//!
//! 文件名带服务 id 与时间戳，多个服务的导出不会互相覆盖。若同名已存在
//! （同一秒内连续导出两次），自动加序号而不是静默覆盖。

use std::path::{Path, PathBuf};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// 日志导出目录：{base}/logs/export/
pub fn export_dir(paths: &Paths) -> PathBuf {
    paths.logs().join("export")
}

/// 清洗成单一文件名片段（不允许路径分隔符）。
///
/// 除了替换非法字符，还要处理两个让人困惑的产物：
/// - 前导点会生成**隐藏文件**，用户在资源管理器里看不到自己刚导出的日志；
/// - `../../x` 清洗后会变成 `.._.._x`，虽然它只是一个普通文件名（不构成穿越，
///   因为 `/` 已被换成 `_`），但看起来像路径穿越，容易被误当成漏洞。
///   所以首尾的点与下划线一律去掉，中间连续的点也收敛成一个。
pub fn sanitize_component(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' || c == '.' {
                c
            } else {
                '_'
            }
        })
        .collect();
    let trimmed = cleaned.trim_matches(|c| c == '_' || c == '.').to_string();
    let collapsed = trimmed.replace("..", ".");
    if collapsed.is_empty() {
        "log".to_string()
    } else {
        collapsed
    }
}

/// 生成默认文件名：{service}-{YYYYmmdd-HHMMSS}.log
pub fn default_name(service_id: &str) -> String {
    format!(
        "{}-{}.log",
        sanitize_component(service_id),
        chrono::Local::now().format("%Y%m%d-%H%M%S")
    )
}

/// 写入日志文件，返回最终路径。
///
/// `suggested_name` 由前端给（用户可能自己改了名）；为空则用默认名。
pub fn write_log_file(
    paths: &Paths,
    service_id: &str,
    content: &str,
    suggested_name: Option<&str>,
) -> Result<String> {
    if content.trim().is_empty() {
        return Err(AppError::new("EMPTY_LOG", "没有可导出的日志内容")
            .with_hint("当前过滤条件下没有日志行；清空关键字或把级别切到「全部」再试"));
    }
    let dir = export_dir(paths);
    std::fs::create_dir_all(&dir).map_err(|e| AppError::io("创建日志导出目录", e))?;

    let name = match suggested_name.filter(|n| !n.trim().is_empty()) {
        Some(n) => {
            // 用户给的名字也要清洗，并强制 .log 后缀（避免被当成可执行文件）
            let base = sanitize_component(n.trim_end_matches(".log"));
            format!("{base}.log")
        }
        None => default_name(service_id),
    };

    // 同名不覆盖：加 -2 / -3 序号
    let mut target = dir.join(&name);
    if target.exists() {
        let stem = name.trim_end_matches(".log").to_string();
        for i in 2..1000 {
            target = dir.join(format!("{stem}-{i}.log"));
            if !target.exists() {
                break;
            }
        }
    }

    std::fs::write(&target, content).map_err(|e| AppError::io("写入日志文件", e))?;
    Ok(target.to_string_lossy().to_string())
}

/// 列出已导出的日志（按时间倒序，供界面显示「最近导出」）
pub fn list_exports(paths: &Paths) -> Vec<(String, u64, i64)> {
    let dir = export_dir(paths);
    let mut out = Vec::new();
    let rd = match std::fs::read_dir(&dir) {
        Ok(r) => r,
        Err(_) => return out,
    };
    for e in rd.flatten() {
        let p = e.path();
        if p.extension().and_then(|s| s.to_str()) != Some("log") {
            continue;
        }
        let meta = match e.metadata() {
            Ok(m) => m,
            Err(_) => continue,
        };
        let ts = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0);
        out.push((
            p.file_name()
                .map(|s| s.to_string_lossy().to_string())
                .unwrap_or_default(),
            meta.len(),
            ts,
        ));
    }
    out.sort_by(|a, b| b.2.cmp(&a.2));
    out
}

/// 删除一个导出文件（只允许删导出目录内的，防路径穿越）
pub fn delete_export(paths: &Paths, name: &str) -> Result<()> {
    if name.contains('/') || name.contains('\\') || name.contains("..") {
        return Err(AppError::new("FORBIDDEN", "非法的文件名"));
    }
    let target = export_dir(paths).join(name);
    if !target.is_file() {
        return Err(AppError::new("FILE_NOT_FOUND", "文件不存在"));
    }
    std::fs::remove_file(&target).map_err(|e| AppError::io("删除日志文件", e))
}

/// 判断路径是否落在导出目录内（测试与调用方自检用）
pub fn is_in_export_dir(paths: &Paths, p: &Path) -> bool {
    match (export_dir(paths).canonicalize(), p.canonicalize()) {
        (Ok(d), Ok(t)) => t.starts_with(d),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_path_separators() {
        assert_eq!(sanitize_component("php@8.3"), "php_8.3");
        assert_eq!(sanitize_component("../../etc/passwd"), "etc_passwd");
        assert_eq!(sanitize_component("nginx"), "nginx");
        assert_eq!(sanitize_component("a b c"), "a_b_c");
    }

    #[test]
    fn sanitize_falls_back_when_all_illegal() {
        assert_eq!(sanitize_component("///"), "log");
        assert_eq!(sanitize_component(""), "log");
        assert_eq!(sanitize_component("中文"), "log");
    }

    #[test]
    fn default_name_has_service_and_timestamp() {
        let n = default_name("php@8.3.33");
        assert!(n.starts_with("php_8.3.33-"), "{n}");
        assert!(n.ends_with(".log"), "{n}");
    }

    #[test]
    fn write_creates_file_with_content() {
        let t = std::env::temp_dir().join(format!("nsb-logexp-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let p = write_log_file(&paths, "nginx", "[error] boom\n", None).unwrap();
        let s = std::fs::read_to_string(&p).unwrap();
        assert_eq!(s, "[error] boom\n");
        assert!(p.ends_with(".log"));
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn write_rejects_empty_content() {
        let t = std::env::temp_dir().join(format!("nsb-logexp2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let r = write_log_file(&paths, "nginx", "   \n  ", None);
        assert!(r.is_err());
        assert_eq!(r.unwrap_err().code, "EMPTY_LOG");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn write_does_not_overwrite_same_name() {
        let t = std::env::temp_dir().join(format!("nsb-logexp3-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let a = write_log_file(&paths, "nginx", "first\n", Some("my log")).unwrap();
        let b = write_log_file(&paths, "nginx", "second\n", Some("my log")).unwrap();
        assert_ne!(a, b, "同名不该覆盖");
        assert_eq!(std::fs::read_to_string(&a).unwrap(), "first\n");
        assert_eq!(std::fs::read_to_string(&b).unwrap(), "second\n");
        assert!(b.contains("-2.log"), "{b}");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn custom_name_is_sanitized_and_forced_log_suffix() {
        let t = std::env::temp_dir().join(format!("nsb-logexp4-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        // 用户可能填一个带路径或奇怪后缀的名字
        let p = write_log_file(&paths, "nginx", "x\n", Some("../../evil.exe")).unwrap();
        assert!(p.ends_with(".log"), "必须强制 .log 后缀：{p}");
        assert!(!p.contains(".."), "不能穿越路径：{p}");
        assert!(
            is_in_export_dir(&paths, Path::new(&p)),
            "必须落在导出目录内"
        );
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn list_exports_sorted_newest_first() {
        let t = std::env::temp_dir().join(format!("nsb-logexp5-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        write_log_file(&paths, "a", "1\n", Some("one")).unwrap();
        std::thread::sleep(std::time::Duration::from_millis(1100));
        write_log_file(&paths, "b", "2\n", Some("two")).unwrap();
        let list = list_exports(&paths);
        assert_eq!(list.len(), 2);
        assert!(list[0].0.starts_with("two"), "{list:?}");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn list_exports_on_missing_dir_is_empty() {
        let paths = Paths::new(std::env::temp_dir().join("nsb-logexp-none"));
        assert!(list_exports(&paths).is_empty());
    }

    #[test]
    fn delete_rejects_path_traversal() {
        let t = std::env::temp_dir().join(format!("nsb-logexp6-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(t.join("logs").join("export")).unwrap();
        let paths = Paths::new(t.clone());
        for bad in ["../../x.log", "..\\x.log", "a/b.log"] {
            assert!(delete_export(&paths, bad).is_err(), "{bad} 应被拒绝");
        }
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn delete_removes_existing_export() {
        let t = std::env::temp_dir().join(format!("nsb-logexp7-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let p = write_log_file(&paths, "nginx", "x\n", Some("todelete")).unwrap();
        assert!(Path::new(&p).is_file());
        delete_export(&paths, "todelete.log").unwrap();
        assert!(!Path::new(&p).exists());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn unicode_service_id_does_not_break_name() {
        let n = default_name("服务");
        assert!(n.ends_with(".log"));
        assert!(!n.contains(' '));
        assert!(n.starts_with("log-"), "{n}");
    }
}
