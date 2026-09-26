//! 日志导出：把当前（可能已过滤的）日志存成文件。
//!
//! 内容由前端传入，后端只负责落盘与命名。这样做是有意的：
//! 用户在界面上做了级别过滤 + 关键字搜索，导出必须与**他看到的**一致，
//! 否则会出现「我搜了 error，导出的却是全量」这种让人不信任的行为。
//!
//! 文件名带服务 id 与时间戳，多个服务的导出不会互相覆盖。若同名已存在
//! （同一秒内连续导出两次），自动加序号而不是静默覆盖。

use std::path::{Path, PathBuf};
use std::io::{Read, Write};

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

    // 在同一目录先写完，再原子发布；并发导出也不能覆盖已存在的文件。
    let mut temp = tempfile::NamedTempFile::new_in(&dir).map_err(|e| AppError::io("创建日志文件", e))?;
    temp.write_all(content.as_bytes()).map_err(|e| AppError::io("写入日志文件", e))?;
    temp.as_file().sync_all().map_err(|e| AppError::io("保存日志文件", e))?;
    let stem = name.trim_end_matches(".log");
    for i in 1..=999 {
        let target = dir.join(if i == 1 { name.clone() } else { format!("{stem}-{i}.log") });
        match temp.persist_noclobber(&target) {
            Ok(_) => return Ok(target.to_string_lossy().to_string()),
            Err(e) if e.error.kind() == std::io::ErrorKind::AlreadyExists => temp = e.file,
            Err(e) => return Err(AppError::io("保存日志文件", e.error)),
        }
    }
    Err(AppError::new("LOG_NAME_EXHAUSTED", "同名日志导出过多，请使用其他文件名或稍后重试"))
}

/// 完整日志以开始导出时的文件长度为准。先写临时文件再替换目标，失败保留旧文件。
/// 即使目标是源文件的硬链接，也只替换该链接，不通过链接截断源文件。
pub fn copy_log_file(source: &Path, dest: &Path) -> Result<u64> {
    let source_path = source.canonicalize().map_err(|e| {
        if e.kind() == std::io::ErrorKind::NotFound {
            AppError::new("LOG_EMPTY", "尚未生成日志文件").with_hint("请先启动服务或访问该站点")
        } else { AppError::io("读取日志路径失败", e) }
    })?;
    if dest.canonicalize().is_ok_and(|p| p == source_path) {
        return Err(AppError::new("SAME_LOG_FILE", "导出位置不能是原日志文件，请另选文件名"));
    }
    let source_file = std::fs::File::open(&source_path).map_err(|e| AppError::io("读取日志失败", e))?;
    let metadata = source_file.metadata().map_err(|e| AppError::io("读取日志属性失败", e))?;
    if !metadata.is_file() {
        return Err(AppError::new("BAD_LOG_FILE", "日志路径不是普通文件"));
    }
    let parent = dest.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent).map_err(|e| AppError::io("创建导出目录失败", e))?;
    let mut temp = tempfile::NamedTempFile::new_in(parent).map_err(|e| AppError::io("创建导出文件失败", e))?;
    let bytes = std::io::copy(&mut source_file.take(metadata.len()), &mut temp)
        .map_err(|e| AppError::io("复制日志失败", e))?;
    if bytes != metadata.len() {
        return Err(AppError::new("LOG_CHANGED", "导出期间日志已被轮转或清空，请重试"));
    }
    temp.as_file().sync_all().map_err(|e| AppError::io("保存日志失败", e))?;
    temp.persist(dest).map_err(|e| AppError::io("保存日志失败", e.error))?;
    Ok(bytes)
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

    #[test]
    fn concurrent_exports_never_overwrite_each_other() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_owned());
        let handles: Vec<_> = (0..12).map(|i| {
            let paths = paths.clone();
            std::thread::spawn(move || {
                let text = format!("log {i}");
                let file = write_log_file(&paths, "nginx", &text, Some("same")).unwrap();
                (file, text)
            })
        }).collect();
        let files: Vec<_> = handles.into_iter().map(|h| h.join().unwrap()).collect();
        for (path, text) in &files {
            assert_eq!(&std::fs::read_to_string(path).unwrap(), text);
        }
        let unique: std::collections::HashSet<_> = files.iter().map(|(p, _)| p).collect();
        assert_eq!(unique.len(), 12);
    }

    #[test]
    fn exhausted_names_return_error_and_preserve_last_export() {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_owned());
        let dir = export_dir(&paths);
        std::fs::create_dir_all(&dir).unwrap();
        for i in 1..=999 {
            let name = if i == 1 { "same.log".into() } else { format!("same-{i}.log") };
            std::fs::write(dir.join(name), "original").unwrap();
        }
        assert_eq!(write_log_file(&paths, "nginx", "new", Some("same")).unwrap_err().code, "LOG_NAME_EXHAUSTED");
        assert_eq!(std::fs::read_to_string(dir.join("same-999.log")).unwrap(), "original");
        assert_eq!(std::fs::read_dir(dir).unwrap().count(), 999);
    }

    #[test]
    fn full_log_export_preserves_source_and_existing_destination_on_failure() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.log");
        let dest = temp.path().join("copy.log");
        let text = b"old\r\nnew \xff\n";
        std::fs::write(&source, text).unwrap();
        assert_eq!(copy_log_file(&source, &source).unwrap_err().code, "SAME_LOG_FILE");
        std::fs::hard_link(&source, &dest).unwrap();
        assert_eq!(copy_log_file(&source, &dest).unwrap(), text.len() as u64);
        assert_eq!(std::fs::read(&source).unwrap(), text);
        assert_eq!(std::fs::read(&dest).unwrap(), text);
        std::fs::write(&dest, "keep").unwrap();
        assert_eq!(std::fs::read(&source).unwrap(), text, "导出后目标不能仍是源文件的硬链接");
        assert_eq!(copy_log_file(&temp.path().join("missing.log"), &dest).unwrap_err().code, "LOG_EMPTY");
        assert_eq!(std::fs::read_to_string(&dest).unwrap(), "keep");
        assert!(copy_log_file(&source, temp.path()).is_err());
        assert_eq!(std::fs::read(&source).unwrap(), text);
    }
}
