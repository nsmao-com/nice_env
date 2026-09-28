//! MongoDB 官方 archive v0.1 的流式检查与按库提取；不执行归档中的数据或脚本。
//! 格式依据：https://github.com/mongodb/mongo-tools/blob/master/common/archive/spec.md
use crate::{AppError, Result};
use serde::Serialize;
use std::{collections::BTreeMap, io::{BufRead, BufReader, Read, Write}, time::{Duration, Instant}};

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveDatabase { pub name: String, pub collections: usize }
#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct ArchiveInfo { pub version: String, pub tools_version: String, pub compression: String, pub databases: Vec<ArchiveDatabase> }
fn invalid(message: &str) -> AppError { AppError::new("MONGO_ARCHIVE_INVALID", message) }

enum Frame { End, Terminator, Document(Vec<u8>) }
fn frame(input: &mut impl Read) -> Result<Frame> {
    let mut prefix = [0u8;4];
    if input.read(&mut prefix[..1])? == 0 { return Ok(Frame::End); }
    input.read_exact(&mut prefix[1..]).map_err(|_| invalid("MongoDB 归档被截断"))?;
    let length = u32::from_le_bytes(prefix);
    if length == u32::MAX { return Ok(Frame::Terminator); }
    if !(5..=16*1024*1024).contains(&length) { return Err(invalid("MongoDB 归档 BSON 长度无效或超过 16 MiB")); }
    let mut data = vec![0u8;length as usize]; data[..4].copy_from_slice(&prefix);
    input.read_exact(&mut data[4..]).map_err(|_| invalid("MongoDB 归档文档不完整"))?;
    if data.last() != Some(&0) { return Err(invalid("MongoDB 归档文档结束标记无效")); }
    Ok(Frame::Document(data))
}
enum Atom { Text(String), Number(u64), Bool(bool) }
fn fields(bytes: &[u8]) -> Result<BTreeMap<String, Atom>> {
    fn take<'a>(bytes: &mut &'a [u8], n: usize) -> Result<&'a [u8]> {
        if bytes.len()<n { return Err(invalid("MongoDB 归档元信息不完整")); }
        let (value, rest)=bytes.split_at(n); *bytes=rest; Ok(value)
    }
    let mut data=&bytes[4..bytes.len()-1]; let mut result=BTreeMap::new();
    while !data.is_empty() {
        if result.len()>=32 { return Err(invalid("MongoDB 归档元信息字段过多")); }
        let kind=take(&mut data,1)?[0];
        let end=data.iter().position(|b|*b==0).filter(|n|*n<=128).ok_or_else(||invalid("MongoDB 归档字段名无效"))?;
        let name=std::str::from_utf8(take(&mut data,end)?).map_err(|_|invalid("MongoDB 归档字段名编码无效"))?.to_string(); take(&mut data,1)?;
        let atom=match kind {
            2 => { let n=u32::from_le_bytes(take(&mut data,4)?.try_into().unwrap()) as usize;
                if n==0 { return Err(invalid("MongoDB 归档字符串长度无效")); }
                let value=take(&mut data,n)?;
                if value[n-1]!=0 { return Err(invalid("MongoDB 归档字符串结束标记无效")); }
                Atom::Text(std::str::from_utf8(&value[..n-1]).map_err(|_|invalid("MongoDB 归档字符串编码无效"))?.into()) },
            8 => match take(&mut data,1)?[0] { 0=>Atom::Bool(false),1=>Atom::Bool(true),_=>return Err(invalid("MongoDB 归档布尔值无效")) },
            16 => Atom::Number(i32::from_le_bytes(take(&mut data,4)?.try_into().unwrap()) as i64 as u64),
            18 => Atom::Number(u64::from_le_bytes(take(&mut data,8)?.try_into().unwrap())),
            _=>return Err(invalid("MongoDB 归档元信息包含不支持的字段类型")),
        };
        if result.insert(name,atom).is_some() { return Err(invalid("MongoDB 归档包含重复字段")); }
    }
    Ok(result)
}
fn text<'a>(data: &'a BTreeMap<String,Atom>, key: &str) -> Result<&'a str> {
    match data.get(key) { Some(Atom::Text(value))=>Ok(value),_=>Err(invalid("MongoDB 归档缺少必要字符串字段")) }
}
fn numeric(data: &BTreeMap<String,Atom>, key: &str) -> Result<u64> {
    match data.get(key) { Some(Atom::Number(value))=>Ok(*value),_=>Err(invalid("MongoDB 归档缺少必要数字字段")) }
}
fn version(value: &str) -> Result<Vec<u32>> {
    let core=value.split('-').next().unwrap_or_default();
    let numbers=core.split('.').map(|n|n.parse::<u32>()).collect::<std::result::Result<Vec<_>,_>>().map_err(|_|invalid("MongoDB 归档版本无法识别"))?;
    if numbers.len()!=3 || value.len()>128 || value.chars().any(char::is_control) { return Err(invalid("MongoDB 归档版本无效")); }
    Ok(numbers)
}
fn crc64(mut crc: u64, bytes: &[u8]) -> u64 {
    static TABLE: once_cell::sync::Lazy<[u64;256]>=once_cell::sync::Lazy::new(||{
        let mut table=[0;256]; for (i,value) in table.iter_mut().enumerate() { let mut n=i as u64;
            for _ in 0..8 { n=if n&1!=0 { (n>>1)^0xc96c5795d7870f42 } else { n>>1 }; } *value=n;
        } table
    });
    crc=!crc; for byte in bytes { crc=TABLE[((crc as u8)^byte) as usize]^(crc>>8); } !crc
}

/// 内存有界：每次仅持有一个 BSON 文档及集合索引，校验所有库的 CRC 和 EOF。
/// selected 存在时，只输出所选库的元信息与原始 BSON，其他库的数据不会进入导入副本。
pub(crate) fn inspect(input: impl Read, selected: Option<&str>, mut output: impl Write) -> Result<ArchiveInfo> {
    let mut buffered=BufReader::new(input);
    let gzip=buffered.fill_buf()?.starts_with(&[0x1f,0x8b]);
    let mut reader: Box<dyn Read+'_>=if gzip { Box::new(flate2::read::MultiGzDecoder::new(buffered)) } else { Box::new(buffered) };
    let started=Instant::now(); let mut magic=[0u8;4]; reader.read_exact(&mut magic).map_err(|_|invalid("请选择完整的 mongodump archive 文件"))?;
    if u32::from_le_bytes(magic)!=0x8199e26d { return Err(invalid("文件不是 MongoDB archive，不能导入单独 BSON、JSON 或目录备份")); }
    let Frame::Document(header)=frame(&mut reader)? else { return Err(invalid("MongoDB 归档头缺失")); };
    let head=fields(&header)?;
    if text(&head,"version")?!="0.1" { return Err(invalid("暂不支持此 MongoDB archive 格式版本")); }
    let server=text(&head,"server_version")?.to_string(); let tools=text(&head,"tool_version")?.to_string();
    let server_numbers=version(&server)?; version(&tools)?;
    if !(1..=1024).contains(&numeric(&head,"concurrent_collections")?) { return Err(invalid("MongoDB 归档并发参数超出支持范围")); }
    if selected.is_some() { output.write_all(&magic)?; output.write_all(&header)?; }
    let mut namespaces: BTreeMap<(String,String),(u64,bool)>=BTreeMap::new(); let mut databases: BTreeMap<String,usize>=BTreeMap::new();
    let mut metadata_bytes=0usize;
    loop {
        let bytes=match frame(&mut reader)? { Frame::Document(bytes)=>bytes,Frame::Terminator=>break,Frame::End=>return Err(invalid("MongoDB 归档元信息未结束")) };
        metadata_bytes+=bytes.len();
        if metadata_bytes>64*1024*1024 || namespaces.len()>=100_000 { return Err(invalid("MongoDB 归档集合元信息超出支持范围")); }
        let info=fields(&bytes)?; let db=text(&info,"db")?; let collection=text(&info,"collection")?;
        if db.is_empty() || db.len()>63 || db.contains(['\0','.']) || collection.is_empty() || collection.len()>512 || collection.contains('\0') { return Err(invalid("MongoDB 归档库名或集合名无效")); }
        let kind=match info.get("type") { Some(Atom::Text(value))=>value.as_str(),None=>"",_=>return Err(invalid("MongoDB 归档集合类型无效")) };
        let physical=if kind=="timeseries" && server_numbers.as_slice()<[8,3,0].as_slice() { format!("system.buckets.{collection}") } else { collection.into() };
        if namespaces.insert((db.into(),physical),(0,false)).is_some() { return Err(invalid("MongoDB 归档包含重复集合")); }
        *databases.entry(db.into()).or_default()+=1;
        if selected==Some(db) { output.write_all(&bytes)?; }
    }
    if databases.is_empty() { return Err(invalid("MongoDB 归档没有数据库或集合")); }
    if selected.is_some_and(|name|!databases.contains_key(name)) { return Err(invalid("所选数据库不在归档中")); }
    if selected.is_some() { output.write_all(&u32::MAX.to_le_bytes())?; }
    loop {
        if started.elapsed()>Duration::from_secs(1800) { return Err(invalid("归档校验超过 30 分钟，请检查文件或磁盘")); }
        let bytes=match frame(&mut reader)? { Frame::End=>break,Frame::Document(bytes)=>bytes,Frame::Terminator=>return Err(invalid("MongoDB 归档集合段缺少头部")) };
        let info=fields(&bytes)?; let db=text(&info,"db")?; let collection=text(&info,"collection")?;
        let eof=match info.get("EOF") { Some(Atom::Bool(value))=>*value,_=>return Err(invalid("MongoDB 归档集合缺少 EOF 标记")) };
        let expected=numeric(&info,"CRC")?;
        let entry=namespaces.get_mut(&(db.into(),collection.into())).ok_or_else(||invalid("MongoDB 归档数据属于未声明的集合"))?;
        if entry.1 { return Err(invalid("MongoDB 归档包含已结束集合的数据")); }
        let emit=selected==Some(db); if emit { output.write_all(&bytes)?; }
        loop {
            if started.elapsed()>Duration::from_secs(1800) { return Err(invalid("归档校验超过 30 分钟，请检查文件或磁盘")); }
            match frame(&mut reader)? {
                Frame::Document(doc) if !eof => { entry.0=crc64(entry.0,&doc); if emit { output.write_all(&doc)?; } }
                Frame::Terminator=> { if emit { output.write_all(&u32::MAX.to_le_bytes())?; } break; }
                _=>return Err(invalid("MongoDB 归档集合段被截断或 EOF 后仍有数据")),
            }
        }
        if eof { if entry.0!=expected { return Err(invalid("MongoDB 归档集合 CRC 校验失败")); } entry.1=true; }
        else if expected!=0 { return Err(invalid("MongoDB 归档非 EOF 段包含校验值")); }
    }
    if namespaces.values().any(|(_,closed)|!*closed) { return Err(invalid("MongoDB 归档缺少集合结束标记")); }
    Ok(ArchiveInfo { version:server,tools_version:tools,compression:if gzip {"gzip"} else {"none"}.into(),databases:databases.into_iter().map(|(name,collections)|ArchiveDatabase{name,collections}).collect() })
}
