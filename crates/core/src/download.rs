//! 下载器：校验过的 HTTP Range 续传、可取消的网络等待与安装任务互斥。
use crate::error::{AppError, Result};
use crate::paths::Paths;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Notify;

struct TaskControl {
    // 0 = 可取消，1 = 已取消，2 = 正在提交安装结果（不可中断）。
    phase: AtomicU8,
    notify: Notify,
}

type TaskMap = Arc<parking_lot::Mutex<HashMap<String, Arc<TaskControl>>>>;

/// 从解析版本到提交安装记录都持有；提前返回或 future 被丢弃也会释放任务。
pub(crate) struct DownloadTask {
    id: String,
    control: Arc<TaskControl>,
    tasks: TaskMap,
}

impl DownloadTask {
    pub(crate) fn id(&self) -> &str {
        &self.id
    }

    pub(crate) fn check_cancelled(&self) -> Result<()> {
        if self.control.phase.load(Ordering::SeqCst) == 1 {
            Err(AppError::new("CANCELLED", "安装已取消")
                .with_hint("已下载的有效部分会保留，下次可以继续"))
        } else {
            Ok(())
        }
    }

    pub(crate) async fn cancelled(&self) {
        loop {
            let notified = self.control.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if self.control.phase.load(Ordering::SeqCst) == 1 {
                return;
            }
            notified.await;
        }
    }

    /// 原子划定提交边界，避免「已提示取消」后仍发布安装。
    pub(crate) fn begin_commit(&self) -> Result<()> {
        match self
            .control
            .phase
            .compare_exchange(0, 2, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) | Err(2) => Ok(()),
            Err(_) => self.check_cancelled(),
        }
    }
}

impl Drop for DownloadTask {
    fn drop(&mut self) {
        let mut tasks = self.tasks.lock();
        if tasks
            .get(&self.id)
            .is_some_and(|c| Arc::ptr_eq(c, &self.control))
        {
            tasks.remove(&self.id);
        }
    }
}

pub struct Downloader {
    tasks: TaskMap,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct ResumeInfo {
    url: String,
    validator: Option<String>,
}

impl Downloader {
    pub fn new() -> Self {
        Self {
            tasks: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    pub(crate) fn begin_task(&self, task_id: &str) -> Result<DownloadTask> {
        // taskId 会成为下载文件名，不能允许路径、盘符或 Windows ADS。
        if task_id.is_empty()
            || task_id == "."
            || task_id == ".."
            || task_id.contains(['/', '\\', ':', '\0', '<', '>', '"', '|', '?', '*'])
            || task_id.ends_with(['.', ' '])
        {
            return Err(AppError::new("INVALID_PACKAGE_KEY", "非法的安装任务标识"));
        }
        let mut tasks = self.tasks.lock();
        if tasks.contains_key(task_id) {
            return Err(
                AppError::new("PACKAGE_BUSY", format!("{task_id} 正在安装或卸载"))
                    .with_hint("等待当前操作完成后重试"),
            );
        }
        let control = Arc::new(TaskControl {
            phase: AtomicU8::new(0),
            notify: Notify::new(),
        });
        tasks.insert(task_id.to_string(), control.clone());
        Ok(DownloadTask {
            id: task_id.into(),
            control,
            tasks: self.tasks.clone(),
        })
    }

    /// 数据目录迁移前检查是否仍有安装/卸载任务在读写下载缓存或运行时目录。
    pub fn has_tasks(&self) -> bool {
        !self.tasks.lock().is_empty()
    }

    /// true 表示已接收取消；提交阶段或已结束的任务返回 false。
    pub fn cancel(&self, task_id: &str) -> bool {
        let tasks = self.tasks.lock();
        let control = tasks.get(task_id).or_else(|| {
            // 不带版本的调用也能取消唯一对应的版本任务。
            let prefix = format!("{task_id}@");
            let mut matches = tasks.iter().filter(|(id, _)| id.starts_with(&prefix));
            let first = matches.next().map(|(_, value)| value);
            if matches.next().is_none() {
                first
            } else {
                None
            }
        });
        let Some(control) = control else { return false };
        match control
            .phase
            .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
        {
            Ok(_) | Err(1) => {
                control.notify.notify_waiters();
                true
            }
            Err(_) => false,
        }
    }

    /// 下载到缓存；独立调用与 Installer 使用同一任务互斥和取消机制。
    pub async fn download(
        &self,
        task_id: &str,
        urls: &[String],
        expected_sha256: &str,
        expected_size: u64,
        paths: &Paths,
        emit: &dyn Fn(crate::Event),
    ) -> Result<PathBuf> {
        let task = self.begin_task(task_id)?;
        self.download_with_task(&task, urls, expected_sha256, expected_size, paths, emit)
            .await
    }

    pub(crate) async fn download_with_task(
        &self,
        task: &DownloadTask,
        urls: &[String],
        expected_sha256: &str,
        expected_size: u64,
        paths: &Paths,
        emit: &dyn Fn(crate::Event),
    ) -> Result<PathBuf> {
        task.check_cancelled()?;
        std::fs::create_dir_all(paths.downloads())?;
        let final_path = paths.downloads().join(format!("{}.pkg", task.id()));
        if final_path.is_file() && expected_sha256 != "0" {
            if hash_file_checked(&final_path, task).await? == expected_sha256.to_lowercase() {
                let size = std::fs::metadata(&final_path)?.len();
                task.check_cancelled()?;
                emit(crate::Event::progress(
                    task.id(),
                    size,
                    size,
                    0,
                    0.0,
                    "downloaded",
                ));
                return Ok(final_path);
            }
        }

        let part_path = paths.downloads().join(format!("{}.part", task.id()));
        let mut last_err = None;
        for url in urls {
            task.check_cancelled()?;
            match self
                .download_one(
                    task,
                    url,
                    &part_path,
                    &final_path,
                    expected_sha256,
                    expected_size,
                    emit,
                )
                .await
            {
                Ok(path) => return Ok(path),
                Err(err) if err.code == "CANCELLED" => return Err(err),
                Err(err) => {
                    // 校验失败也可换源，但下一次必须从干净的 part 开始。
                    emit(crate::Event::progress(
                        task.id(),
                        0,
                        expected_size,
                        0,
                        0.0,
                        "downloading",
                    ));
                    last_err = Some(err);
                }
            }
        }
        let err =
            last_err.unwrap_or_else(|| AppError::new("DOWNLOAD_ALL_FAILED", "没有可用的下载源"));
        Err(err.with_hint("检查网络、代理或下载镜像后重试；有效的下载片段会保留"))
    }

    async fn download_one(
        &self,
        task: &DownloadTask,
        url: &str,
        part_path: &Path,
        final_path: &Path,
        expected_sha256: &str,
        expected_size: u64,
        emit: &dyn Fn(crate::Event),
    ) -> Result<PathBuf> {
        let client = reqwest::Client::builder()
            // 不设总时长：大包只要持续传输就继续；取消可打断连接和读等待。
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(20))
            .gzip(false)
            .user_agent(concat!("NiceEnv/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| AppError::internal("创建 HTTP 客户端", e.to_string()))?;
        let meta_path = part_path.with_extension("part.json");
        let resume = std::fs::read(&meta_path)
            .ok()
            .and_then(|raw| serde_json::from_slice::<ResumeInfo>(&raw).ok());
        let mut offset = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);
        // 无哈希时只能用同源的强 ETag / Last-Modified 做 If-Range，避免拼接不同文件。
        let same_source = resume.as_ref().filter(|info| info.url == url);
        if offset > 0
            && expected_sha256 == "0"
            && same_source
                .and_then(|info| info.validator.as_ref())
                .is_none()
        {
            discard_partial(part_path)?;
            offset = 0;
        }

        let mut retry_full = false;
        let resp = loop {
            task.check_cancelled()?;
            let mut req = client
                .get(url)
                .header(reqwest::header::ACCEPT_ENCODING, "identity");
            if offset > 0 {
                req = req.header(reqwest::header::RANGE, format!("bytes={offset}-"));
                if let Some(validator) = same_source.and_then(|info| info.validator.as_ref()) {
                    req = req.header(reqwest::header::IF_RANGE, validator);
                }
            }
            let resp = tokio::select! {
                biased;
                _ = task.cancelled() => return Err(AppError::new("CANCELLED", "安装已取消")),
                result = req.send() => result.map_err(|e| AppError::download(url, e.to_string()))?,
            };
            if resp.status().as_u16() == 416 && offset > 0 && !retry_full {
                // 416 的响应体不是安装包；清掉失效片段并重新请求完整文件。
                discard_partial(part_path)?;
                offset = 0;
                retry_full = true;
                continue;
            }
            if resp.status().as_u16() == 206 && offset > 0 {
                if let Some(validator) = same_source.and_then(|info| info.validator.as_ref()) {
                    let header = if validator.starts_with('"') {
                        reqwest::header::ETAG
                    } else {
                        reqwest::header::LAST_MODIFIED
                    };
                    if resp
                        .headers()
                        .get(header)
                        .is_some_and(|value| value.to_str().ok() != Some(validator.as_str()))
                    {
                        // 某些镜像忽略 If-Range；不能把新文件的片段追加到旧文件。
                        discard_partial(part_path)?;
                        offset = 0;
                        retry_full = true;
                        continue;
                    }
                }
            }
            break resp;
        };
        let status = resp.status().as_u16();
        if status != 200 && status != 206 {
            return Err(AppError::download(url, format!("HTTP {}", resp.status())));
        }
        let length = resp.content_length();
        let total = if status == 206 {
            let range = resp
                .headers()
                .get(reqwest::header::CONTENT_RANGE)
                .and_then(|h| h.to_str().ok())
                .and_then(parse_content_range);
            match range {
                Some((start, end, total))
                    if start == offset
                        && end + 1 == total
                        && length.is_none_or(|len| len == end - start + 1) =>
                {
                    Some(total)
                }
                _ => {
                    discard_partial(part_path)?;
                    return Err(AppError::new(
                        "INVALID_DOWNLOAD_RANGE",
                        "下载源返回的续传范围不正确",
                    )
                    .with_detail(format!(
                        "requested offset={offset}; Content-Range={:?}",
                        resp.headers().get(reqwest::header::CONTENT_RANGE)
                    )));
                }
            }
        } else {
            offset = 0;
            length
        };
        let validator = resp
            .headers()
            .get(reqwest::header::ETAG)
            .and_then(|v| v.to_str().ok())
            .filter(|v| !v.starts_with("W/"))
            .or_else(|| {
                resp.headers()
                    .get(reqwest::header::LAST_MODIFIED)
                    .and_then(|v| v.to_str().ok())
            })
            .map(str::to_owned);

        use tokio::io::AsyncWriteExt;
        let mut options = tokio::fs::OpenOptions::new();
        options.create(true).write(true);
        if offset > 0 {
            options.append(true);
        } else {
            options.truncate(true);
        }
        let mut file = options
            .open(part_path)
            .await
            .map_err(|e| AppError::io("打开下载缓存", e))?;
        let info = ResumeInfo {
            url: url.into(),
            validator,
        };
        std::fs::write(
            &meta_path,
            serde_json::to_vec(&info)
                .map_err(|e| AppError::internal("保存续传信息", e.to_string()))?,
        )?;
        let mut received = offset;
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let start = Instant::now();
        let mut speed = 0;
        use futures_util::StreamExt;
        let mut stream = resp.bytes_stream();
        loop {
            let chunk = tokio::select! {
                biased;
                _ = task.cancelled() => { file.flush().await?; return Err(AppError::new("CANCELLED", "安装已取消")); }
                result = stream.next() => result,
            };
            let Some(chunk) = chunk else { break };
            let chunk = chunk.map_err(|e| AppError::download(url, e.to_string()))?;
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::io("写入下载缓存", e))?;
            received += chunk.len() as u64;
            if last_emit.elapsed() >= Duration::from_millis(200) {
                last_emit = Instant::now();
                speed =
                    ((received - offset) as f64 / start.elapsed().as_secs_f64().max(0.001)) as u64;
                let progress_total = total.unwrap_or(expected_size);
                let eta = if speed > 0 {
                    progress_total.saturating_sub(received) as f64 / speed as f64
                } else {
                    0.0
                };
                emit(crate::Event::progress(
                    task.id(),
                    received,
                    progress_total,
                    speed,
                    eta,
                    "downloading",
                ));
            }
        }
        file.flush()
            .await
            .map_err(|e| AppError::io("下载落盘", e))?;
        drop(file);
        task.check_cancelled()?;
        if received == 0 || total.is_some_and(|total| total != received) {
            return Err(AppError::new("DOWNLOAD_INCOMPLETE", "下载文件不完整")
                .with_detail(format!("received={received}, expected={total:?}")));
        }
        emit(crate::Event::progress(
            task.id(),
            received,
            received,
            speed,
            0.0,
            "verifying",
        ));
        if expected_sha256 != "0" {
            let actual = hash_file_checked(part_path, task).await?;
            if actual != expected_sha256.to_lowercase() {
                discard_partial(part_path)?;
                return Err(
                    AppError::new("CHECKSUM_MISMATCH", "文件校验失败（sha256 不匹配）")
                        .with_detail(format!("expect={expected_sha256} actual={actual}")),
                );
            }
        }
        task.check_cancelled()?;
        if final_path.exists() {
            tokio::fs::remove_file(final_path)
                .await
                .map_err(|e| AppError::io("替换下载缓存", e))?;
        }
        tokio::fs::rename(part_path, final_path)
            .await
            .map_err(|e| AppError::io("保存下载文件", e))?;
        let _ = std::fs::remove_file(meta_path);
        emit(crate::Event::progress(
            task.id(),
            received,
            received,
            speed,
            0.0,
            "downloaded",
        ));
        Ok(final_path.to_path_buf())
    }
}

fn discard_partial(path: &Path) -> Result<()> {
    for target in [path.to_path_buf(), path.with_extension("part.json")] {
        match std::fs::remove_file(&target) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => return Err(AppError::io("清理失效下载片段", err)),
        }
    }
    Ok(())
}

fn parse_content_range(value: &str) -> Option<(u64, u64, u64)> {
    let (range, total) = value.strip_prefix("bytes ")?.split_once('/')?;
    let (start, end) = range.split_once('-')?;
    let (start, end, total) = (
        start.parse::<u64>().ok()?,
        end.parse::<u64>().ok()?,
        total.parse::<u64>().ok()?,
    );
    (start <= end && end < total).then_some((start, end, total))
}

async fn hash_file_checked(path: &Path, task: &DownloadTask) -> Result<String> {
    use tokio::io::AsyncReadExt;
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(|e| AppError::io("读取下载文件", e))?;
    let mut hash = Sha256::new();
    let mut buffer = vec![0; 512 * 1024];
    loop {
        task.check_cancelled()?;
        let n = file
            .read(&mut buffer)
            .await
            .map_err(|e| AppError::io("校验下载文件", e))?;
        if n == 0 {
            break;
        }
        hash.update(&buffer[..n]);
    }
    task.check_cancelled()?;
    Ok(hex::encode(hash.finalize()))
}

pub fn sha256_file(path: &Path) -> Result<String> {
    use std::io::Read;
    let mut file = std::fs::File::open(path).map_err(|e| AppError::io("打开文件", e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 512 * 1024];
    loop {
        let n = file
            .read(&mut buf)
            .map_err(|e| AppError::io("读取文件", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn fixture() -> (tempfile::TempDir, Paths, Downloader) {
        let temp = tempfile::tempdir().unwrap();
        let paths = Paths::new(temp.path().to_path_buf());
        paths.ensure_dirs().unwrap();
        (temp, paths, Downloader::new())
    }

    async fn server(replies: Vec<String>) -> (String, tokio::task::JoinHandle<Vec<String>>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/fixture", listener.local_addr().unwrap());
        let handle = tokio::spawn(async move {
            let mut requests = Vec::new();
            for reply in replies {
                let (mut stream, _) = listener.accept().await.unwrap();
                let mut data = Vec::new();
                loop {
                    let mut byte = [0];
                    if stream.read(&mut byte).await.unwrap() == 0 {
                        break;
                    }
                    data.push(byte[0]);
                    if data.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                requests.push(String::from_utf8(data).unwrap());
                stream.write_all(reply.as_bytes()).await.unwrap();
            }
            requests
        });
        (url, handle)
    }

    #[tokio::test]
    async fn range_416_restarts_without_accepting_error_body() {
        let (_temp, paths, downloader) = fixture();
        std::fs::write(paths.downloads().join("fixture.part"), b"old fragment").unwrap();
        let (url, server) = server(vec![
            "HTTP/1.1 416 Range Not Satisfiable\r\nContent-Length: 5\r\nConnection: close\r\n\r\nerror".into(),
            "HTTP/1.1 200 OK\r\nContent-Length: 7\r\nConnection: close\r\n\r\npayload".into(),
        ]).await;
        let sha = hex::encode(Sha256::digest(b"payload"));
        let path = downloader
            .download("fixture", &[url], &sha, 7, &paths, &|_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"payload");
        let requests = server.await.unwrap();
        assert!(requests[0].to_lowercase().contains("range: bytes=12-"));
        assert!(!requests[1].to_lowercase().contains("range:"));
    }

    #[tokio::test]
    async fn invalid_range_is_rejected_and_bad_mirror_can_fall_back() {
        let (_temp, paths, downloader) = fixture();
        std::fs::write(paths.downloads().join("range.part"), b"abc").unwrap();
        let (url, response) = server(vec![
            "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 0-2/3\r\nContent-Length: 3\r\nConnection: close\r\n\r\nxyz".into()
        ]).await;
        let err = downloader
            .download("range", &[url], "hash", 6, &paths, &|_| {})
            .await
            .unwrap_err();
        assert_eq!(err.code, "INVALID_DOWNLOAD_RANGE");
        assert!(!paths.downloads().join("range.pkg").exists());
        response.await.unwrap();

        let (bad, first) = server(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 3\r\nConnection: close\r\n\r\nbad".into(),
        ])
        .await;
        let (good, second) = server(vec![
            "HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ngood".into(),
        ])
        .await;
        let sha = hex::encode(Sha256::digest(b"good"));
        let path = downloader
            .download("mirror", &[bad, good], &sha, 4, &paths, &|_| {})
            .await
            .unwrap();
        assert_eq!(std::fs::read(path).unwrap(), b"good");
        first.await.unwrap();
        second.await.unwrap();
    }

    #[tokio::test]
    async fn unhashed_resume_requires_same_validator_and_restarts_if_it_changes() {
        let (_temp, paths, downloader) = fixture();
        for (key, changed) in [("resume", false), ("changed", true)] {
            let mut replies = vec![format!(
                "HTTP/1.1 206 Partial Content\r\nContent-Range: bytes 3-5/6\r\nContent-Length: 3\r\nETag: \"{}\"\r\nConnection: close\r\n\r\ndef",
                if changed { "new" } else { "old" }
            )];
            if changed {
                replies.push("HTTP/1.1 200 OK\r\nContent-Length: 6\r\nETag: \"new\"\r\nConnection: close\r\n\r\nuvwxyz".into());
            }
            let (url, response) = server(replies).await;
            std::fs::write(paths.downloads().join(format!("{key}.part")), b"abc").unwrap();
            std::fs::write(
                paths.downloads().join(format!("{key}.part.json")),
                serde_json::to_vec(&ResumeInfo {
                    url: url.clone(),
                    validator: Some("\"old\"".into()),
                })
                .unwrap(),
            )
            .unwrap();
            let path = downloader
                .download(key, &[url], "0", 6, &paths, &|_| {})
                .await
                .unwrap();
            assert_eq!(
                std::fs::read(path).unwrap(),
                if changed { b"uvwxyz" } else { b"abcdef" }
            );
            let requests = response.await.unwrap();
            assert!(requests[0].to_lowercase().contains("if-range: \"old\""));
            assert!(requests[0].to_lowercase().contains("range: bytes=3-"));
            if changed {
                assert!(!requests[1].to_lowercase().contains("range:"));
            }
        }
    }

    #[tokio::test]
    async fn cancellation_interrupts_waiting_headers_and_task_guard_releases() {
        let (_temp, paths, downloader) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/stall", listener.local_addr().unwrap());
        let urls = [url];
        let download = downloader.download("cancel", &urls, "0", 0, &paths, &|_| {});
        let cancel = async {
            let (_stream, _) = listener.accept().await.unwrap();
            assert!(downloader.cancel("cancel"));
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(download, cancel)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        let task = downloader.begin_task("cancel").unwrap();
        assert_eq!(
            downloader.begin_task("cancel").err().unwrap().code,
            "PACKAGE_BUSY"
        );
        task.begin_commit().unwrap();
        assert!(!downloader.cancel("cancel"));
        drop(task);
        assert!(downloader.begin_task("cancel").is_ok());
    }

    #[tokio::test]
    async fn cancelled_stalled_body_keeps_only_received_bytes() {
        let (_temp, paths, downloader) = fixture();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let urls = [format!("http://{}/body", listener.local_addr().unwrap())];
        let download = downloader.download("body", &urls, "0", 100, &paths, &|_| {});
        let cancel = async {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut buf = [0; 2048];
            stream.read(&mut buf).await.unwrap();
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\nETag: \"one\"\r\n\r\ndata")
                .await
                .unwrap();
            while std::fs::metadata(paths.downloads().join("body.part"))
                .map(|m| m.len())
                .unwrap_or(0)
                < 4
            {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert!(downloader.cancel("body"));
            tokio::time::sleep(Duration::from_millis(200)).await;
        };
        let (result, _) = tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(download, cancel)
        })
        .await
        .unwrap();
        assert_eq!(result.unwrap_err().code, "CANCELLED");
        assert_eq!(
            std::fs::read(paths.downloads().join("body.part")).unwrap(),
            b"data"
        );
        assert!(!paths.downloads().join("body.pkg").exists());
    }
}
