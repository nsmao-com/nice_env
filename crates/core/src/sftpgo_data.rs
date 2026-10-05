//! SFTPGo 本地账号库中的物理目录。只处理迁移副本，不改密码、权限或远程存储前缀。

use crate::error::{AppError, Result};
use crate::paths::{portable_path_text, DataPathRebase};
use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Provider {
    pub path: PathBuf,
    pub driver: String,
    pub prefix: String,
    pub external: bool,
    pub executable: Option<PathBuf>,
}

fn invalid(message: &str) -> AppError {
    AppError::new("DATA_DIR_SFTPGO_STATE", message)
        .with_hint("未切换数据目录，原账号库和文件均已保留。请检查 SFTPGo 账号库配置后重试。")
}

pub(crate) fn provider(
    content: &str,
    json: bool,
    environment: &HashMap<String, String>,
    directory: &Path,
    cwd: Option<&Path>,
) -> Result<Option<Provider>> {
    let config = crate::configpaths::sftpgo_config(content, json)?;
    let value = |field: &str, default: &str| {
        environment
            .get(&format!(
                "SFTPGO_DATA_PROVIDER__{}",
                field.to_ascii_uppercase()
            ))
            .cloned()
            .unwrap_or_else(|| {
                config["data_provider"][field]
                    .as_str()
                    .unwrap_or(default)
                    .to_string()
            })
    };
    let driver = value("driver", "sqlite");
    if !matches!(driver.as_str(), "sqlite" | "bolt") {
        // 远程库和 memory provider 的账号目录无法从本地文件副本核对；
        // 跳过后继续切换会留下旧 home_dir，甚至让上传文件被当成配置改写。
        return Err(AppError::new(
            "DATA_DIR_SFTPGO_PROVIDER",
            "当前 SFTPGo 账号库类型不支持自动迁移用户目录，未切换数据目录",
        )
        .with_hint("请继续使用原数据目录。如需迁移，请先通过 SFTPGo 备份账号，改用本地 SQLite 或 Bolt 账号库并确认账号可用后重试。原账号库、配置和文件均已保留。"));
    }
    let name = value("name", "sftpgo.db");
    let connection = value("connection_string", "");
    if name.is_empty() && (driver == "bolt" || connection.is_empty()) {
        return Err(invalid("SFTPGo 账号库文件名不能为空"));
    }
    let path = if driver == "sqlite" {
        let connection = if connection.is_empty() {
            crate::configpaths::sftpgo_sqlite_dsn(directory, &name)
        } else {
            connection
        };
        let Some(path) = crate::configpaths::sqlite_connection_path(&connection)? else {
            return Ok(None);
        };
        if path.is_absolute() {
            path
        } else {
            cwd.ok_or_else(|| invalid("无法确认 SFTPGo 相对数据库地址的工作目录"))?
                .join(path)
        }
    } else {
        // Bolt 按上游规则始终使用 name，connection_string 仅用于 SQL provider。
        directory.join(name)
    };
    Ok(Some(Provider {
        path,
        driver,
        prefix: value("sql_tables_prefix", ""),
        external: false,
        executable: None,
    }))
}

fn sql_name(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn check_provider(provider: &Provider) -> Result<bool> {
    if !provider.path.try_exists()? {
        return Ok(false);
    }
    if std::fs::metadata(&provider.path)?.len() == 0 {
        return Ok(false);
    }
    if provider.external {
        return Err(invalid("SFTPGo 账号库位于数据目录之外，无法安全迁移其中的用户目录")
            .with_hint("请先将该账号库纳入当前数据目录并确认 SFTPGo 可以正常使用，再重新迁移。原文件未修改。")
            .with_detail(portable_path_text(&provider.path)));
    }
    Ok(true)
}

struct Update {
    table: String,
    column: &'static str,
    id: i64,
    before: rusqlite::types::Value,
    after: rusqlite::types::Value,
}

#[derive(Default)]
struct AccountState {
    updates: Vec<Update>,
    directories: Vec<PathBuf>,
}

fn account_state(
    conn: &rusqlite::Connection,
    prefix: &str,
    rebase: &DataPathRebase,
) -> Result<AccountState> {
    let check: String = conn.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(invalid("SFTPGo 账号库完整性校验失败，未切换数据目录"));
    }
    let tables: Vec<String> = conn
        .prepare("SELECT name FROM sqlite_master WHERE type='table'")?
        .query_map([], |row| row.get(0))?
        .collect::<std::result::Result<_, _>>()?;
    let mut state = AccountState::default();
    for (table, column, object) in [
        ("users", "home_dir", false),
        ("folders", "path", false),
        ("groups", "user_settings", true),
    ] {
        let name = format!("{prefix}{table}");
        if !tables.contains(&name) {
            if table == "groups" {
                continue;
            } // 旧版 SFTPGo 尚无用户组。
            return Err(invalid("SFTPGo 账号库缺少预期的数据表，未迁移账号目录"));
        }
        let table = sql_name(&name);
        let columns: Vec<String> = conn
            .prepare(&format!("PRAGMA table_info({table})"))?
            .query_map([], |row| row.get(1))?
            .collect::<std::result::Result<_, _>>()?;
        if !columns.iter().any(|name| name == "id") || !columns.iter().any(|name| name == column) {
            return Err(invalid("SFTPGo 账号库字段与当前迁移规则不匹配"));
        }
        let rows: Vec<(i64, rusqlite::types::Value)> = conn
            .prepare(&format!("SELECT id,{} FROM {table}", sql_name(column)))?
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .collect::<std::result::Result<_, _>>()?;
        for (id, before) in rows {
            use rusqlite::types::Value;
            let text = match &before {
                Value::Null => continue,
                Value::Text(text) => text.as_str(),
                Value::Blob(bytes) => std::str::from_utf8(bytes)
                    .map_err(|_| invalid("SFTPGo 账号目录不是有效 UTF-8"))?,
                _ => return Err(invalid("SFTPGo 账号目录字段类型无效")),
            };
            let after = if object && !text.is_empty() {
                let mut settings: serde_json::Value = serde_json::from_str(text)
                    .map_err(|_| invalid("SFTPGo 用户组设置不是有效 JSON，未迁移账号目录"))?;
                if settings.is_null() {
                    continue;
                }
                if !settings.is_object() {
                    return Err(invalid("SFTPGo 用户组设置类型无效"));
                }
                let Some(home) = settings.get_mut("home_dir") else {
                    continue;
                };
                if home.is_null() {
                    continue;
                }
                let Some(value) = home.as_str() else {
                    return Err(invalid("SFTPGo 用户组目录字段类型无效"));
                };
                // 用户组的 home_dir 模板由 SFTPGo 在登录时展开，保护模板前的父目录。
                let mut directory = PathBuf::from(value);
                while directory.to_string_lossy().contains("%username%")
                    || directory.to_string_lossy().contains("%role%")
                {
                    if !directory.pop() {
                        break;
                    }
                }
                if directory.is_absolute() {
                    state.directories.push(directory);
                }
                let migrated = rebase.path(value);
                if migrated == value {
                    continue;
                }
                *home = migrated.into();
                serde_json::to_string(&settings)
                    .map_err(|_| invalid("无法保存 SFTPGo 用户组目录"))?
            } else if object {
                continue;
            } else {
                let directory = PathBuf::from(text);
                if directory.is_absolute() {
                    state.directories.push(directory);
                }
                rebase.path(text)
            };
            if after != text {
                let after = if matches!(before, Value::Blob(_)) {
                    Value::Blob(after.into_bytes())
                } else {
                    Value::Text(after)
                };
                state.updates.push(Update {
                    table: table.clone(),
                    column,
                    id,
                    before,
                    after,
                });
            }
        }
    }
    Ok(state)
}

/// 账号目录内即使包含 .ini、.conf、sftpgo.json，也属于用户内容，不应改写。
pub(crate) fn data_directories(
    provider: &Provider,
    rebase: &DataPathRebase,
) -> Result<Vec<PathBuf>> {
    if !check_provider(provider)? {
        return Ok(Vec::new());
    }
    if provider.driver == "bolt" {
        return Ok(crate::sftpgo_bolt::plan(&provider.path, rebase)?.directories);
    }
    let conn = rusqlite::Connection::open_with_flags(
        &provider.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    Ok(account_state(&conn, &provider.prefix, rebase)?.directories)
}

pub(crate) fn rebase_provider(provider: &Provider, rebase: &DataPathRebase) -> Result<bool> {
    if !check_provider(provider)? {
        return Ok(false);
    }
    if provider.driver == "bolt" {
        return crate::sftpgo_bolt::migrate(&provider.path, provider.executable.as_deref(), rebase);
    }
    let mut conn = rusqlite::Connection::open_with_flags(
        &provider.path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )?;
    conn.busy_timeout(std::time::Duration::from_secs(2))?;
    let transaction = conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
    let state = account_state(&transaction, &provider.prefix, rebase)?;
    if state.updates.is_empty() {
        return Ok(false);
    }
    for Update {
        table,
        column,
        id,
        before,
        after,
    } in state.updates
    {
        let column = sql_name(column);
        if transaction.execute(
            &format!("UPDATE {table} SET {column}=?1 WHERE id=?2 AND {column}=?3"),
            rusqlite::params![after, id, before],
        )? != 1
        {
            return Err(invalid("迁移过程中 SFTPGo 账号目录发生变化，已取消迁移"));
        }
    }
    let check: String = transaction.query_row("PRAGMA quick_check", [], |row| row.get(0))?;
    if check != "ok" {
        return Err(invalid("迁移副本的 SFTPGo 账号库完整性校验失败"));
    }
    transaction.commit()?;
    Ok(true)
}
