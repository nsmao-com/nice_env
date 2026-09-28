//! Interactive database workspace. Commands use private client options and stdin, never a shell.
use crate::{dbadmin::{self, MySqlClient}, AppError, Result};
use serde::{Deserialize, Serialize};
use std::{io::{Seek, Write}, process::Stdio, time::Duration};

#[derive(Debug, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Grid {
    pub columns: Vec<String>,
    pub rows: Vec<Vec<Option<String>>>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Request {
    pub database: String,
    pub action: String,
    pub table: Option<String>,
    pub offset: Option<u32>,
    pub sql: Option<String>,
    #[serde(default)]
    pub confirmed: bool,
    pub column: Option<String>,
    pub value: Option<String>,
    pub original: Option<Vec<Option<String>>>,
}

fn ident(value: &str) -> Result<String> {
    if value.is_empty() || value.len() > 256 || value.chars().any(char::is_control) { return Err(AppError::new("BAD_IDENTIFIER", "数据库标识无效")); }
    Ok(format!("`{}`", value.replace('`', "``")))
}
fn literal(value: Option<&str>) -> String {
    value.map(|v| format!("CONVERT(X'{}' USING utf8mb4)", hex::encode(v.as_bytes()))).unwrap_or_else(|| "NULL".into())
}

/// XML preserves NULL, embedded tabs/newlines, and duplicate column labels.
pub fn query(client: &MySqlClient, database: &str, sql: &str) -> Result<Vec<Grid>> {
    if sql.len() > 1024 * 1024 || sql.contains('\0') { return Err(AppError::new("BAD_SQL", "SQL 为空、过长或包含无效字符")); }
    let (_private, mut command) = dbadmin::client_command(&client.exe, "127.0.0.1", client.port, "root", &client.root_password)?;
    let mut input = tempfile::tempfile()?;
    input.write_all(sql.as_bytes())?; input.rewind()?;
    let mut output = tempfile::tempfile()?; let mut error = tempfile::tempfile()?;
    command.args(["--xml", "--batch", "--binary-mode", "--local-infile=0", "--default-character-set=utf8mb4", "--connect-timeout=5"])
        .arg(format!("--database={database}"))
        .stdin(Stdio::from(input)).stdout(output.try_clone()?).stderr(error.try_clone()?);
    let status = dbadmin::wait_client(&mut command, Duration::from_secs(30), || {})?;
    if !status.success() {
        let mut detail = dbadmin::read_output(&mut error, 16 * 1024)?;
        if !client.root_password.is_empty() { detail = detail.replace(&client.root_password, "***"); }
        return Err(AppError::new("SQL_FAILED", "SQL 执行失败；多语句中之前成功的写入可能已经生效").with_detail(detail));
    }
    if output.metadata()?.len() > 16 * 1024 * 1024 { return Err(AppError::new("SQL_RESULT_TOO_LARGE", "查询结果超过 16 MB，请使用 LIMIT；已执行的写入不会因此撤销")); }
    let xml = dbadmin::read_output(&mut output, 16 * 1024 * 1024)?;
    parse_results(&xml)
}

pub fn parse_results(xml: &str) -> Result<Vec<Grid>> {
    use quick_xml::events::Event;
    let mut reader = quick_xml::Reader::from_str(xml);
    let mut grids = Vec::new(); let mut grid = Grid::default();
    let mut row = Vec::new(); let mut names = Vec::new(); let mut cell: Option<String> = None; let mut in_field = false;
    loop {
        let event = reader.read_event().map_err(|e| AppError::new("SQL_RESULT_INVALID", format!("无法读取查询结果：{e}")))?;
        match event {
            Event::Start(ref e) if e.name().as_ref() == "resultset" => { grid = Grid::default(); }
            Event::Start(ref e) if e.name().as_ref() == "row" => { row.clear(); names.clear(); }
            Event::Start(ref e) | Event::Empty(ref e) if e.name().as_ref() == "field" => {
                cell = Some(String::new()); in_field = true;
                for attr in e.attributes() {
                    let attr = attr.map_err(|e| AppError::new("SQL_RESULT_INVALID", e.to_string()))?;
                    let value = attr.normalized_value(quick_xml::XmlVersion::Implicit1_0).map_err(|e| AppError::new("SQL_RESULT_INVALID", e.to_string()))?;
                    if attr.key.as_ref() == "name" { names.push(value.to_string()); }
                    if attr.key.as_ref() == "xsi:nil" && value == "true" { cell = None; }
                }
                if matches!(event, Event::Empty(_)) { row.push(cell.take()); in_field = false; }
            }
            Event::Text(e) if in_field => {
                let text = e.as_ref();
                if let Some(cell) = &mut cell { cell.push_str(&text); }
            }
            Event::GeneralRef(e) if in_field => {
                let name = e.as_ref();
                let escaped = format!("&{name};");
                let decoded = quick_xml::escape::unescape(&escaped).map_err(|e| AppError::new("SQL_RESULT_INVALID", e.to_string()))?;
                if let Some(cell) = &mut cell { cell.push_str(&decoded); }
            }
            Event::CData(e) if in_field => { if let Some(cell) = &mut cell { cell.push_str(&e.as_ref()); } }
            Event::End(ref e) if e.name().as_ref() == "field" => { row.push(cell.take()); in_field = false; }
            Event::End(ref e) if e.name().as_ref() == "row" => {
                if grid.columns.is_empty() { grid.columns = names.clone(); }
                grid.rows.push(std::mem::take(&mut row));
                if grid.rows.len() > 20000 { return Err(AppError::new("SQL_RESULT_TOO_LARGE", "单个结果集超过 20,000 行，请使用 LIMIT")); }
            }
            Event::End(ref e) if e.name().as_ref() == "resultset" => { grids.push(std::mem::take(&mut grid)); }
            Event::Eof => break,
            _ => {}
        }
    }
    Ok(grids)
}

pub fn execute(client: &MySqlClient, request: Request) -> Result<Vec<Grid>> {
    let db = ident(&request.database)?;
    // Discover databases and tables before using user-selected identifiers.
    if !client.list_databases()?.iter().any(|entry| entry.name == request.database) { return Err(AppError::new("DATABASE_NOT_FOUND", "所选数据库不存在")); }
    let tables = query(client, &request.database, "SHOW FULL TABLES;")?;
    if request.action == "tables" { return Ok(tables); }
    if request.action == "sql" {
        if !request.confirmed { return Err(AppError::new("SQL_CONFIRM_REQUIRED", "请确认目标数据库和完整 SQL 后执行")); }
        let sql = request.sql.as_deref().filter(|s| !s.trim().is_empty()).ok_or_else(|| AppError::new("BAD_SQL", "请输入 SQL"))?;
        return query(client, &request.database, sql);
    }
    let table = request.table.as_deref().ok_or_else(|| AppError::new("TABLE_REQUIRED", "请选择表"))?;
    let entry = tables.iter().flat_map(|g| &g.rows).find(|row| row.first().and_then(|v| v.as_deref()) == Some(table)).ok_or_else(|| AppError::new("TABLE_NOT_FOUND", "所选表不存在"))?;
    let target = format!("{db}.{}", ident(table)?);
    let schema = query(client, &request.database, &format!("SHOW FULL COLUMNS FROM {target}; SHOW INDEX FROM {target};"))?;
    if request.action == "schema" { return Ok(schema); }
    let columns = schema.first().ok_or_else(|| AppError::new("TABLE_SCHEMA_MISSING", "无法读取表结构"))?;
    let field_index = |name: &str| columns.columns.iter().position(|c| c == name).ok_or_else(|| AppError::new("TABLE_SCHEMA_INVALID", "表结构不完整"));
    let name_idx = field_index("Field")?; let key_idx = field_index("Key")?;
    let fields: Vec<&str> = columns.rows.iter().filter_map(|row| row.get(name_idx)?.as_deref()).collect();
    let primary: Vec<&str> = columns.rows.iter().filter(|row| row.get(key_idx).and_then(|v| v.as_deref()) == Some("PRI")).filter_map(|row| row.get(name_idx)?.as_deref()).collect();
    if request.action == "rows" {
        let offset = request.offset.unwrap_or(0).min(10_000_000);
        let order = if primary.is_empty() { String::new() } else { format!(" ORDER BY {}", primary.iter().map(|p| ident(p)).collect::<Result<Vec<_>>>()?.join(", ")) };
        return query(client, &request.database, &format!("SELECT * FROM {target}{order} LIMIT 101 OFFSET {offset};"));
    }
    if request.action != "update" || !request.confirmed { return Err(AppError::new("BAD_ACTION", "未知操作或尚未确认修改")); }
    if ["mysql", "sys", "information_schema", "performance_schema"].contains(&request.database.to_ascii_lowercase().as_str()) || primary.is_empty() || entry.get(1).and_then(|v| v.as_deref()) != Some("BASE TABLE") { return Err(AppError::new("TABLE_READ_ONLY", "系统表、视图或无主键表不支持网格编辑")); }
    let column = request.column.as_deref().ok_or_else(|| AppError::new("COLUMN_REQUIRED", "请选择字段"))?;
    let index = fields.iter().position(|name| *name == column).ok_or_else(|| AppError::new("COLUMN_NOT_FOUND", "字段不存在"))?;
    let type_idx = field_index("Type")?; let extra_idx = field_index("Extra")?;
    let editable = |row: &Vec<Option<String>>| {
        let typ = row.get(type_idx).and_then(|v| v.as_deref()).unwrap_or("").to_ascii_lowercase();
        let extra = row.get(extra_idx).and_then(|v| v.as_deref()).unwrap_or("").to_ascii_lowercase();
        !["blob", "tinyblob", "mediumblob", "longblob", "binary", "varbinary", "bit", "geometry", "geometrycollection", "point", "multipoint", "polygon", "multipolygon", "linestring", "multilinestring"].contains(&typ.split('(').next().unwrap_or("")) && !extra.contains("virtual generated") && !extra.contains("stored generated")
    };
    // Text grids cannot round-trip binary/spatial values safely.
    if columns.rows.iter().any(|row| !editable(row)) { return Err(AppError::new("TABLE_READ_ONLY", "含二进制、空间或生成字段的表请使用 SQL 编辑")); }
    let original = request.original.ok_or_else(|| AppError::new("ROW_REQUIRED", "缺少原始行"))?;
    if original.len() != fields.len() || original.iter().filter_map(|v| v.as_ref()).map(|v| v.len()).sum::<usize>() > 1024 * 1024 || request.value.as_ref().is_some_and(|v| v.len() > 1024 * 1024) { return Err(AppError::new("ROW_CHANGED", "行数据无效或过大，请重新加载")); }
    if original[index] == request.value { return Ok(Vec::new()); }
    let predicate = fields.iter().zip(&original).map(|(name, value)| Ok(format!("BINARY {} <=> BINARY {}", ident(name)?, literal(value.as_deref())))).collect::<Result<Vec<_>>>()?.join(" AND ");
    let result = query(client, &request.database, &format!("UPDATE {target} SET {} = {} WHERE {predicate} LIMIT 1; SELECT ROW_COUNT() AS affected;", ident(column)?, literal(request.value.as_deref())))?;
    if result.last().and_then(|g| g.rows.first()).and_then(|r| r.first()).and_then(|v| v.as_deref()) != Some("1") { return Err(AppError::new("ROW_CHANGED", "未修改任何行：数据可能已被其他连接修改，请刷新后重试")); }
    Ok(result)
}
