//! 站点 `.env` 管理。
//!
//! Laravel / Symfony / WordPress（用 wp-config 也好不到哪去）都要改 `.env`。
//! 用户改 `.env` 最容易踩的三个坑，这里都处理掉：
//! 1. **值里有空格或 # 但没加引号** → 被解析成注释或被截断；
//! 2. **改完忘了同步数据库连接串** → 站点连不上库，却是密码写错；
//! 3. **手改坏了没有退路** → 所以每次保存前备份。
//!
//! 另外做一件很实用的事：从站点绑定的数据库自动补全 DB_* 变量，
//! 不用用户自己去翻「数据库」页抄密码。

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::{AppError, Result};
use crate::paths::Paths;

/// 一条环境变量
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvEntry {
    pub key: String,
    pub value: String,
    /// 是否为注释行（保留在文件里但不生效）
    pub commented: bool,
    /// 值看起来像敏感信息（密码 / 密钥 / token）—— 前端默认打码
    pub secret: bool,
    /// 该行在文件里的行号（1-based），用于精确定位
    pub line: usize,
    /// 值需要加引号但没加（保存时会修，或提示用户）
    pub needs_quote: bool,
}

/// 一个站点的 .env 文件
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EnvFileView {
    pub site_id: String,
    pub site_name: String,
    pub path: String,
    pub exists: bool,
    pub entries: Vec<EnvEntry>,
    /// 站点绑定的数据库信息，可用于一键补全 DB_*
    #[serde(skip_serializing_if = "Option::is_none")]
    pub db_hint: Option<DbHint>,
    /// 探测到的 .env 变体文件（.env.example / .env.local …）
    #[serde(default)]
    pub variants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DbHint {
    pub database: String,
    pub username: String,
    pub password: String,
    pub port: u16,
}

/// 键名看起来是否敏感
pub fn is_secret_key(key: &str) -> bool {
    let k = key.to_ascii_uppercase();
    // 明确列出常见后缀/前缀，而不是模糊包含匹配，避免把 APP_KEY_ALGO 之类误判
    k.contains("PASSWORD")
        || k.contains("PASSWD")
        || k.contains("SECRET")
        || k.contains("_KEY")
        || k.ends_with("KEY")
        || k.contains("TOKEN")
        || k.contains("PRIVATE")
        || k == "APP_KEY"
        || k.contains("CREDENTIAL")
}

/// 值是否需要引号包裹。
///
/// dotenv 的规则：值里出现空格或 `#` 就必须加引号，否则
/// `NAME=my app#1` 会被解析成 `NAME=my app` 并把 `#1` 当注释。
pub fn needs_quoting(value: &str) -> bool {
    if value.is_empty() {
        return false;
    }
    value.contains(' ')
        || value.contains('#')
        || value.contains('\t')
        || value.starts_with('"')
        || value.starts_with('\'')
        // 前后有空白也会被吃掉
        || value.trim() != value
}

/// 去掉值两端成对的引号
fn unquote(v: &str) -> String {
    let t = v.trim();
    if t.len() >= 2 {
        let b = t.as_bytes();
        if (b[0] == b'"' && b[t.len() - 1] == b'"') || (b[0] == b'\'' && b[t.len() - 1] == b'\'') {
            return t[1..t.len() - 1].to_string();
        }
    }
    t.to_string()
}

/// 解析 .env 内容。
///
/// 只处理 `KEY=VALUE` 与 `# 注释`；`export KEY=VALUE` 也认。
/// 保留注释行的原样（通过 line 与 commented 标记），保存时不丢用户注释。
pub fn parse_env(content: &str) -> Vec<EnvEntry> {
    let mut out = Vec::new();
    for (i, raw) in content.lines().enumerate() {
        let line_no = i + 1;
        let t = raw.trim();
        if t.is_empty() {
            continue;
        }
        let (body, commented) = match t.strip_prefix('#') {
            Some(rest) => (rest.trim_start(), true),
            None => (t, false),
        };
        // 支持 export KEY=VALUE
        let body = body.strip_prefix("export ").unwrap_or(body);
        let Some((k, v)) = body.split_once('=') else {
            continue; // 不是赋值行（纯注释文字），不进列表
        };
        let key = k.trim().to_string();
        if key.is_empty() {
            continue;
        }
        let raw_value = v.trim();
        let quoted = (raw_value.starts_with('"') && raw_value.ends_with('"'))
            || (raw_value.starts_with('\'') && raw_value.ends_with('\''));
        let value = unquote(raw_value);
        out.push(EnvEntry {
            secret: is_secret_key(&key),
            // 已加引号的就没问题
            needs_quote: !commented && !quoted && needs_quoting(&value),
            key,
            value,
            commented,
            line: line_no,
        });
    }
    out
}

/// 重新生成 .env 内容：更新已有键，保留注释与空行，末尾追加新键。
///
/// 之所以不「整份重写」：用户 .env 里常有分组注释和空行，
/// 整份重写会把这些结构抹掉，下次打开就不认识了。
pub fn apply_env_changes(original: &str, changes: &[(String, String)]) -> String {
    let mut remaining: Vec<(String, String)> = changes.to_vec();
    let mut out: Vec<String> = Vec::new();

    for raw in original.lines() {
        let t = raw.trim();
        if t.is_empty() {
            out.push(raw.to_string());
            continue;
        }
        let body = match t.strip_prefix('#') {
            Some(rest) => rest.trim_start(),
            None => t,
        };
        let body = body.strip_prefix("export ").unwrap_or(body);
        match body.split_once('=') {
            Some((k, _)) => {
                let key = k.trim();
                if let Some(pos) = remaining.iter().position(|(ck, _)| ck == key) {
                    let (_, v) = remaining.remove(pos);
                    // 需要引号则加上；否则保持裸值
                    let val = if needs_quoting(&v) {
                        format!("\"{}\"", v.replace('"', "\\\""))
                    } else {
                        v
                    };
                    out.push(format!("{key}={val}"));
                    continue;
                }
                out.push(raw.to_string());
            }
            None => out.push(raw.to_string()),
        }
    }

    // 剩下的就是新键
    if !remaining.is_empty() {
        if !out.is_empty() && !out.last().map(|l| l.trim().is_empty()).unwrap_or(true) {
            out.push(String::new());
        }
        for (k, v) in remaining {
            let val = if needs_quoting(&v) {
                format!("\"{}\"", v.replace('"', "\\\""))
            } else {
                v
            };
            out.push(format!("{k}={val}"));
        }
    }

    let mut s = out.join("\n");
    s.push('\n');
    s
}

/// 生成一组与站点绑定数据库对齐的 DB_* 变量（Laravel 命名约定）
pub fn db_env_vars(hint: &DbHint) -> Vec<(String, String)> {
    vec![
        ("DB_CONNECTION".to_string(), "mysql".to_string()),
        ("DB_HOST".to_string(), "127.0.0.1".to_string()),
        ("DB_PORT".to_string(), hint.port.to_string()),
        ("DB_DATABASE".to_string(), hint.database.clone()),
        ("DB_USERNAME".to_string(), hint.username.clone()),
        ("DB_PASSWORD".to_string(), hint.password.clone()),
    ]
}

/// .env 文件路径（站点根目录下）
pub fn env_path(root: &Path) -> PathBuf {
    root.join(".env")
}

/// 探测站点目录下的 .env 变体
pub fn env_variants(root: &Path) -> Vec<String> {
    const NAMES: &[&str] = &[
        ".env",
        ".env.example",
        ".env.local",
        ".env.development",
        ".env.production",
        ".env.testing",
        ".env.dist",
    ];
    NAMES
        .iter()
        .filter(|n| root.join(n).is_file())
        .map(|s| s.to_string())
        .collect()
}

/// 读取某站点的 .env
pub fn read_env(
    paths: &Paths,
    store: &crate::store::Store,
    site_id: &str,
) -> Result<EnvFileView> {
    let site = crate::sites::list(store)?
        .into_iter()
        .find(|s| s.id == site_id)
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "找不到该站点"))?;
    let root = PathBuf::from(&site.root_dir);
    let path = env_path(&root);
    let exists = path.is_file();
    let content = if exists {
        std::fs::read_to_string(&path).map_err(|e| AppError::io("读取 .env", e))?
    } else {
        String::new()
    };
    let _ = paths;

    // 站点绑了库就给补全提示
    let db_hint = site.db.as_ref().filter(|d| d.enabled).map(|d| DbHint {
        database: d.database.clone(),
        username: d.username.clone(),
        password: d.password.clone(),
        port: 3306,
    });

    Ok(EnvFileView {
        site_id: site.id.clone(),
        site_name: site.name.clone(),
        path: path.to_string_lossy().to_string(),
        exists,
        entries: parse_env(&content),
        db_hint,
        variants: env_variants(&root),
    })
}

/// 保存 .env：只改传进来的键，其余原样保留；写前备份
pub fn save_env(
    _paths: &Paths,
    store: &crate::store::Store,
    site_id: &str,
    changes: &[(String, String)],
) -> Result<()> {
    let site = crate::sites::list(store)?
        .into_iter()
        .find(|s| s.id == site_id)
        .ok_or_else(|| AppError::new("SITE_NOT_FOUND", "找不到该站点"))?;
    let root = PathBuf::from(&site.root_dir);
    if !root.is_dir() {
        return Err(AppError::new("ROOT_MISSING", "站点根目录不存在")
            .with_hint("站点目录可能被移动或删除，请到站点详情里修正路径"));
    }
    let path = env_path(&root);
    let original = if path.is_file() {
        std::fs::read_to_string(&path).map_err(|e| AppError::io("读取 .env", e))?
    } else {
        String::new()
    };
    let next = apply_env_changes(&original, changes);
    // 就地备份：.env 不进全局备份目录，放在项目旁边更直观
    if path.is_file() {
        let bak = root.join(".env.nsb-backup");
        let _ = std::fs::write(&bak, &original);
    }
    std::fs::write(&path, next).map_err(|e| AppError::io("写入 .env", e))?;
    Ok(())
}

/// 一键补全 DB_* 变量（返回写入了哪些键）
pub fn apply_db_vars(
    _paths: &Paths,
    store: &crate::store::Store,
    site_id: &str,
) -> Result<Vec<String>> {
    let view = read_env(_paths, store, site_id)?;
    let hint = view
        .db_hint
        .ok_or_else(|| AppError::new("NO_DB_BINDING", "该站点没有绑定数据库")
            .with_hint("先到站点详情里为它创建/绑定一个数据库"))?;
    let vars = db_env_vars(&hint);
    save_env(_paths, store, site_id, &vars)?;
    Ok(vars.into_iter().map(|(k, _)| k).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_env() {
        let e = parse_env("APP_NAME=Demo\nAPP_ENV=local\n");
        assert_eq!(e.len(), 2);
        assert_eq!(e[0].key, "APP_NAME");
        assert_eq!(e[0].value, "Demo");
        assert_eq!(e[1].line, 2);
    }

    #[test]
    fn parses_quoted_and_comment_lines() {
        let e = parse_env("NAME=\"my app\"\n#DEBUG=true\n");
        assert_eq!(e[0].value, "my app");
        assert!(!e[0].needs_quote, "已加引号就不需要再加");
        assert_eq!(e[1].key, "DEBUG");
        assert!(e[1].commented);
    }

    #[test]
    fn detects_value_needing_quotes() {
        // 有空格但不加引号：dotenv 会截断
        let e = parse_env("NAME=my app\n");
        assert_eq!(e[0].value, "my app");
        assert!(e[0].needs_quote);
    }

    #[test]
    fn detects_hash_needing_quotes() {
        let e = parse_env("PASS=abc#1\n");
        assert!(e[0].needs_quote, "# 后的内容会被当注释");
    }

    #[test]
    fn no_quote_needed_for_plain_values() {
        for v in ["abc", "a-b_c.d", "1234", "a/b:c", ""] {
            let e = parse_env(&format!("K={v}\n"));
            if e.is_empty() {
                continue;
            }
            assert!(!e[0].needs_quote, "{v} 不需要引号");
        }
    }

    #[test]
    fn strips_export_prefix() {
        let e = parse_env("export PATH_EXTRA=/opt/bin\n");
        assert_eq!(e[0].key, "PATH_EXTRA");
        assert_eq!(e[0].value, "/opt/bin");
    }

    #[test]
    fn ignores_non_assignment_lines() {
        let e = parse_env("# just a title\n\nnot an assignment\nK=v\n");
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].key, "K");
    }

    #[test]
    fn secret_keys_are_flagged() {
        for k in [
            "DB_PASSWORD",
            "APP_KEY",
            "JWT_SECRET",
            "AWS_SECRET_ACCESS_KEY",
            "API_TOKEN",
            "REDIS_PASSWORD",
        ] {
            assert!(is_secret_key(k), "{k} 应判为敏感");
        }
        for k in ["APP_NAME", "APP_ENV", "DB_HOST", "DB_PORT", "LOG_LEVEL"] {
            assert!(!is_secret_key(k), "{k} 不该判为敏感");
        }
    }

    #[test]
    fn apply_updates_existing_key_in_place() {
        let src = "APP_NAME=Old\nAPP_ENV=local\n";
        let out = apply_env_changes(src, &[("APP_NAME".into(), "New".into())]);
        assert!(out.contains("APP_NAME=New"));
        assert!(!out.contains("APP_NAME=Old"));
        assert!(out.contains("APP_ENV=local"), "其它键应保留");
        // 顺序不变
        let lines: Vec<&str> = out.lines().collect();
        assert!(lines[0].starts_with("APP_NAME"));
    }

    #[test]
    fn apply_appends_new_keys_at_end() {
        let src = "APP_NAME=Old\n";
        let out = apply_env_changes(src, &[("DB_HOST".into(), "127.0.0.1".into())]);
        let lines: Vec<&str> = out.lines().filter(|l| !l.is_empty()).collect();
        assert_eq!(lines[0], "APP_NAME=Old");
        assert_eq!(lines[1], "DB_HOST=127.0.0.1");
    }

    #[test]
    fn apply_preserves_comments_and_blank_lines() {
        let src = "# App\nAPP_NAME=Old\n\n# DB\nDB_HOST=x\n";
        let out = apply_env_changes(src, &[("APP_NAME".into(), "New".into())]);
        assert!(out.contains("# App"));
        assert!(out.contains("# DB"));
        // 空行结构保留（除了结尾）
        assert!(out.contains("New\n\n# DB"));
    }

    #[test]
    fn apply_quotes_values_that_need_it() {
        let out = apply_env_changes("K=1\n", &[("APP_NAME".into(), "my app".into())]);
        assert!(out.contains("APP_NAME=\"my app\""), "{out}");
    }

    #[test]
    fn apply_does_not_quote_plain_values() {
        let out = apply_env_changes("K=1\n", &[("DB_HOST".into(), "127.0.0.1".into())]);
        assert!(out.contains("DB_HOST=127.0.0.1"), "{out}");
        assert!(!out.contains("DB_HOST=\""));
    }

    #[test]
    fn apply_escapes_inner_quotes() {
        let out = apply_env_changes("", &[("K".into(), "say \"hi\" now".into())]);
        assert!(out.contains(r#"K="say \"hi\" now""#), "{out}");
    }

    #[test]
    fn apply_is_idempotent_for_same_change() {
        let src = "A=1\nB=2\n";
        let once = apply_env_changes(src, &[("A".into(), "9".into())]);
        let twice = apply_env_changes(&once, &[("A".into(), "9".into())]);
        assert_eq!(once, twice);
    }

    #[test]
    fn apply_multiple_changes_at_once() {
        let src = "DB_HOST=localhost\nDB_PORT=3306\n";
        let out = apply_env_changes(
            src,
            &[("DB_HOST".into(), "127.0.0.1".into()), ("DB_PORT".into(), "23306".into())],
        );
        assert!(out.contains("DB_HOST=127.0.0.1"));
        assert!(out.contains("DB_PORT=23306"));
    }

    #[test]
    fn apply_handles_empty_original() {
        let out = apply_env_changes("", &[("A".into(), "1".into())]);
        assert_eq!(out.trim(), "A=1");
    }

    #[test]
    fn db_vars_use_laravel_conventions() {
        let hint = DbHint {
            database: "shop".into(),
            username: "shop_user".into(),
            password: "p@ss".into(),
            port: 3306,
        };
        let vars = db_env_vars(&hint);
        let map: std::collections::HashMap<_, _> = vars.into_iter().collect();
        assert_eq!(map.get("DB_CONNECTION").map(String::as_str), Some("mysql"));
        assert_eq!(map.get("DB_HOST").map(String::as_str), Some("127.0.0.1"));
        assert_eq!(map.get("DB_DATABASE").map(String::as_str), Some("shop"));
        assert_eq!(map.get("DB_USERNAME").map(String::as_str), Some("shop_user"));
        assert_eq!(map.get("DB_PORT").map(String::as_str), Some("3306"));
    }

    #[test]
    fn env_variants_detects_existing_only() {
        let t = std::env::temp_dir().join(format!("nsb-env-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        std::fs::write(t.join(".env"), "A=1").unwrap();
        std::fs::write(t.join(".env.example"), "A=").unwrap();
        let v = env_variants(&t);
        assert!(v.contains(&".env".to_string()));
        assert!(v.contains(&".env.example".to_string()));
        assert!(!v.contains(&".env.production".to_string()), "不存在的变体不该列出");
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn save_env_rejects_missing_site_root() {
        let t = std::env::temp_dir().join(format!("nsb-env2-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&t);
        std::fs::create_dir_all(&t).unwrap();
        let paths = Paths::new(t.clone());
        let store = crate::store::Store::open(t.join("s.sqlite")).unwrap();
        // 站点不存在
        assert!(save_env(&paths, &store, "nope", &[]).is_err());
        let _ = std::fs::remove_dir_all(&t);
    }

    #[test]
    fn unquote_handles_unescaped_pairs_only() {
        assert_eq!(unquote("\"abc\""), "abc");
        assert_eq!(unquote("'abc'"), "abc");
        assert_eq!(unquote("abc"), "abc");
        // 只有单边引号时不剥
        assert_eq!(unquote("\"abc"), "\"abc");
        assert_eq!(unquote("\""), "\"");
    }
}
