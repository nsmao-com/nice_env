//! 下载器：断点续传（HTTP Range）、sha256 校验、取消、进度事件。
//! 进度通过 EventSink 以 "download://progress" 推给前端；smoke 测试用打印 sink。

use crate::error::{AppError, Result};
use crate::paths::Paths;
use crate::model::DownloadProgress;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

pub type CancelMap = Arc<parking_lot::Mutex<HashMap<String, Arc<AtomicBool>>>>;

pub struct Downloader {
    pub cancels: CancelMap,
}

impl Downloader {
    pub fn new() -> Self {
        Self {
            cancels: Arc::new(parking_lot::Mutex::new(HashMap::new())),
        }
    }

    pub fn cancel(&self, task_id: &str) {
        if let Some(c) = self.cancels.lock().get(task_id) {
            c.store(true, Ordering::SeqCst);
        }
    }

    /// 下载到 {downloads}/{task_id}.pkg，完成后原子改名并校验 sha256。
    /// 已存在且校验通过则直接返回（幂等，冒烟测试预置缓存即可跳过下载）。
    /// 扩展名统一 `.pkg`：真实格式由清单 `kind` 决定（zip / tar.gz / gz / bin），
    /// 解压一律按清单分支处理，不依赖扩展名。
    pub async fn download(
        &self,
        task_id: &str,
        urls: &[String],
        expected_sha256: &str,
        expected_size: u64,
        paths: &Paths,
        emit: &dyn Fn(crate::Event),
    ) -> Result<PathBuf> {
        let final_path = paths.downloads().join(format!("{task_id}.pkg"));
        // 幂等：已下载且哈希匹配
        if final_path.exists() && expected_sha256 != "0" {
            if let Ok(h) = sha256_file(&final_path) {
                if h == expected_sha256.to_lowercase() {
                    emit(crate::Event::progress(task_id, 1, 1, 0, 0.0, "downloaded"));
                    return Ok(final_path);
                }
            }
        }

        let cancel = Arc::new(AtomicBool::new(false));
        self.cancels.lock().insert(task_id.to_string(), cancel.clone());

        let part_path = paths.downloads().join(format!("{task_id}.part"));
        let mut last_err: Option<String> = None;

        for url in urls {
            match self
                .download_one(task_id, url, &part_path, &final_path, expected_sha256, expected_size, &cancel, emit)
                .await
            {
                Ok(p) => {
                    self.cancels.lock().remove(task_id);
                    return Ok(p);
                }
                Err(e) => {
                    if e.code == "CANCELLED" || e.code == "CHECKSUM_MISMATCH" {
                        // 不可重试：用户取消 / 内容校验失败（换源也一样）
                        self.cancels.lock().remove(task_id);
                        return Err(e);
                    }
                    last_err = Some(format!("{url} → {e}"));
                    emit(crate::Event::progress(task_id, 0, expected_size, 0, 0.0, "downloading"));
                }
            }
        }
        self.cancels.lock().remove(task_id);
        Err(AppError::new(
            "DOWNLOAD_ALL_FAILED",
            format!("所有下载源均失败（{} 个）", urls.len()),
        )
        .with_hint("检查网络/代理；已下载部分会自动续传。可在设置里切换镜像源。")
        .with_detail(last_err.unwrap_or_default()))
    }

    async fn download_one(
        &self,
        task_id: &str,
        url: &str,
        part_path: &std::path::Path,
        final_path: &std::path::Path,
        expected_sha256: &str,
        expected_size: u64,
        cancel: &Arc<AtomicBool>,
        emit: &dyn Fn(crate::Event),
    ) -> Result<std::path::PathBuf> {
        let client = reqwest::Client::builder()
            // 总时长放宽：慢速但仍在推进的下载（弱网 + 加速前缀）不该被掐断；
            // 卡死的连接交给 read_timeout——20 秒没有任何数据就断开换下一个源，
            // 不然被墙的直连要干等满超时，前端看起来像「卡在 0 B」
            .timeout(Duration::from_secs(300))
            .connect_timeout(Duration::from_secs(15))
            .read_timeout(Duration::from_secs(20))
            .user_agent("NiceEnv/0.1 (+local dev env manager)")
            .build()
            .map_err(|e| AppError::internal("创建 HTTP 客户端", e.to_string()))?;

        // 断点续传：已有 .part 则从 offset 继续
        let mut offset: u64 = std::fs::metadata(part_path).map(|m| m.len()).unwrap_or(0);

        let mut req = client.get(url);
        if offset > 0 {
            req = req.header("Range", format!("bytes={offset}-"));
        }
        let resp = req.send().await.map_err(|e| AppError::download(url, e.to_string()))?;

        let status = resp.status();
        let resumable = status.as_u16() == 206;
        if status.as_u16() == 416 {
            // range 越界：本地 part 已完成，直接进入校验
            offset = 0;
            let _ = std::fs::remove_file(part_path);
        } else if !status.is_success() {
            return Err(AppError::download(url, format!("HTTP {status}")));
        }

        let total = if resumable {
            offset + resp.content_length().unwrap_or(0)
        } else {
            resp.content_length().unwrap_or(expected_size)
        };
        if !resumable && offset > 0 {
            // 服务器不支持 Range，重头下
            let _ = std::fs::remove_file(part_path);
            offset = 0;
        }

        use tokio::io::AsyncWriteExt;
        let mut file = tokio::io::BufWriter::new(
            tokio::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(part_path)
                .await
                .map_err(|e| AppError::io("打开下载缓存文件", e))?,
        );

        let mut stream = resp.bytes_stream();
        let mut received = offset;
        let mut hasher = Sha256::new();
        let start = Instant::now();
        let mut last_emit = Instant::now() - Duration::from_secs(1);
        let mut window_bytes: u64 = 0;
        let mut window_start = Instant::now();
        let mut speed_bps: u64 = 0;

        use futures_util::StreamExt;
        while let Some(chunk) = stream.next().await {
            if cancel.load(Ordering::SeqCst) {
                file.flush().await.ok();
                drop(file);
                return Err(AppError::new("CANCELLED", "下载已取消").with_hint("已下载的部分会保留，下次继续"));
            }
            let chunk = chunk.map_err(|e| AppError::download(url, e.to_string()))?;
            hasher.update(&chunk);
            file.write_all(&chunk)
                .await
                .map_err(|e| AppError::io("写入下载文件", e))?;
            received += chunk.len() as u64;
            window_bytes += chunk.len() as u64;

            if window_start.elapsed() >= Duration::from_secs(1) {
                speed_bps = (window_bytes as f64 / window_start.elapsed().as_secs_f64()) as u64;
                window_bytes = 0;
                window_start = Instant::now();
            }
            if last_emit.elapsed() >= Duration::from_millis(200) {
                last_emit = Instant::now();
                let eta = if speed_bps > 0 && total > received {
                    (total - received) as f64 / speed_bps as f64
                } else {
                    0.0
                };
                emit(crate::Event::DownloadProgress(DownloadProgress {
                    task_id: task_id.to_string(),
                    received,
                    total,
                    speed_bps,
                    eta_sec: eta,
                    state: "downloading".into(),
                    error: None,
                }));
            }
        }
        let _ = start;
        file.flush().await.map_err(|e| AppError::io("落盘", e))?;
        drop(file);

        emit(crate::Event::progress(task_id, received, total, speed_bps, 0.0, "verifying"));

        // 校验
        let actual = {
            let mut f = tokio::fs::File::open(part_path)
                .await
                .map_err(|e| AppError::io("读取下载文件", e))?;
            use tokio::io::AsyncReadExt;
            let mut buf = vec![0u8; 1024 * 512];
            let mut hasher = Sha256::new();
            loop {
                let n = f.read(&mut buf).await.map_err(|e| AppError::io("读取", e))?;
                if n == 0 {
                    break;
                }
                hasher.update(&buf[..n]);
            }
            hex::encode(hasher.finalize())
        };
        if expected_sha256 != "0" && actual != expected_sha256.to_lowercase() {
            let _ = std::fs::remove_file(part_path);
            return Err(AppError::new("CHECKSUM_MISMATCH", "文件校验失败（sha256 不匹配）")
                .with_hint("下载可能被中断或源损坏，已自动清理，请重试")
                .with_detail(format!("expect={expected_sha256} actual={actual}")));
        }

        // part → final
        if final_path.exists() {
            std::fs::remove_file(final_path).ok();
        }
        tokio::fs::rename(part_path, final_path)
            .await
            .map_err(|e| AppError::io("重命名下载文件", e))?;
        emit(crate::Event::progress(task_id, total, total, speed_bps, 0.0, "downloaded"));
        Ok(final_path.to_path_buf())
    }
}

use std::path::PathBuf;

pub fn sha256_file(path: &std::path::Path) -> Result<String> {
    use std::io::Read;
    let mut f = std::fs::File::open(path).map_err(|e| AppError::io("打开文件", e))?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1024 * 512];
    loop {
        let n = f.read(&mut buf).map_err(|e| AppError::io("读取文件", e))?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex::encode(hasher.finalize()))
}
