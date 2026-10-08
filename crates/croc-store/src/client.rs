//! 与暂存服务对话的 HTTP 客户端。
//!
//! 九个端点，全部挂在 `<origin>/api/v1/store/transfers` 下面：
//!
//! | 方法 | 路径 | 认证 | 作用 |
//! |------|------|------|------|
//! | POST | `` | 无 | 建传输，拿上传令牌 |
//! | PUT | `/<id>/manifest` | 上传令牌 | 上传加密后的清单 |
//! | PUT | `/<id>/chunks/<n>` | 上传令牌 | 上传一个分片 |
//! | POST | `/<id>/complete` | 上传令牌 | 收尾，返回过期时间 |
//! | GET | `/<id>/manifest` | 取件能力 | 取清单 |
//! | GET | `/<id>/chunks/<n>` | 取件能力 | 取一个分片 |
//! | POST | `/<id>/claim` | 取件能力 | 领取下载名额 |
//! | POST | `/<id>/commit` | 取件能力 | 确认下载完成 |
//! | DELETE | `/<id>` | 上传令牌 | 撤销 |
//!
//! 两把凭据的分工：**上传令牌**只能写与删，**取件能力**只能读。两者都从主密钥派生，
//! 服务端手里的只是取件能力的 SHA-256。

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};

use serde::{Deserialize, Serialize};
use sha2::Digest as _;
use time::{Duration, OffsetDateTime};
use tokio::io::{AsyncReadExt as _, AsyncSeekExt as _, AsyncWriteExt as _};

use crate::http::{HttpClient, Request, Response};
use crate::manifest::{self, ChunkRef, Manifest, ManifestFile};
use crate::proxy::Proxy;
use crate::share::Share;
use crate::{
    CHUNK_SIZE, Error, KEY_SIZE, MAX_FILES, MAX_MANIFEST_BYTES, PROTOCOL, TRANSFER_ID_LEN, VERSION, base64url, crypto,
};

/// 一个文件用几个连接。Go 的 `storedTransferWorkers` 同值。
const WORKERS: usize = 4;

/// 每多少个分片把断点续传状态落一次盘。Go 的 `checkpointChunks` 同值。
const CHECKPOINTS: u32 = 8;

/// 一个分片最多传几次（只对网络错误与 5xx 重试）。
const UPLOAD_ATTEMPTS: u32 = 3;

/// 上传的默认有效期。
const DEFAULT_EXPIRATION: Duration = Duration::hours(24);

/// 密文比明文多出来的字节：一个 nonce 加一个认证标签。
const SEAL_OVERHEAD: i64 = 28;

/// 一次上传的选项。
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct UploadOptions {
    /// 允许的下载次数。`0` 表示用默认值 1。
    pub downloads: i64,
    /// 有效期。`Duration::ZERO` 表示用默认值 24 小时。
    pub expiration: Duration,
}

impl UploadOptions {
    /// 填默认值并校验。服务端也会校验，但在这里拦下来能省一次往返。
    fn normalized(self) -> Result<Self, Error> {
        let downloads = if self.downloads == 0 { 1 } else { self.downloads };
        if downloads < 1 {
            return Err(Error::Format("stored-transfer downloads must be positive".to_owned()));
        }
        let expiration = if self.expiration.is_zero() { DEFAULT_EXPIRATION } else { self.expiration };
        if expiration < Duration::MINUTE {
            return Err(Error::Format("stored-transfer expiration must be at least one minute".to_owned()));
        }
        if expiration.subsec_nanoseconds() != 0 {
            return Err(Error::Format("stored-transfer expiration must use whole seconds".to_owned()));
        }
        Ok(Self { downloads, expiration })
    }
}

/// 一次上传的结果。
#[derive(Debug, Clone)]
pub struct UploadResult {
    /// 给收件方的取件凭证。
    pub share: Share,
    /// 只有发送方持有的上传令牌，用来撤销。
    pub upload_token: String,
    /// 服务端给的过期时间。
    pub expires_at: OffsetDateTime,
    /// 服务端实际允许的下载次数。
    pub downloads: i64,
}

/// 进度。
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Progress {
    /// 第几个文件（从 0 开始）。
    pub file_index: usize,
    /// 一共几个文件。
    pub file_count: usize,
    /// 文件名。
    pub file_name: String,
    /// 这个文件已处理多少字节。
    pub file_bytes: i64,
    /// 这个文件一共多少字节。
    pub file_size: i64,
    /// 整次传输已处理多少字节。
    pub total_bytes: i64,
    /// 整次传输一共多少字节。
    pub total_size: i64,
}

/// 状态与进度的接收方。
pub trait Reporter: Send + Sync {
    /// 一句人能读的状态。
    fn status(&self, _message: &str) {}
    /// 字节进度。
    fn progress(&self, _progress: Progress) {}
}

/// 什么都不报的接收方。
#[derive(Debug, Clone, Copy, Default)]
pub struct Silent;

impl Reporter for Silent {}

/// store 客户端。
#[derive(Clone)]
pub struct Client {
    http: HttpClient,
}

impl Client {
    /// 建一个客户端。传 `None` 就是直连。
    ///
    /// # Errors
    ///
    /// TLS 后端初始化失败，或者代理地址不合法。
    pub fn new(proxy: Option<Proxy>) -> Result<Self, Error> {
        Ok(Self { http: HttpClient::new(proxy)? })
    }

    /// 上传一组普通文件。
    ///
    /// 中途失败会把已经建好的传输**撤销掉**，不在服务端留一份谁也不认识的密文。
    ///
    /// # Errors
    ///
    /// 文件读不了、服务端拒绝、网络中断。
    pub async fn upload(
        &self,
        origin: &str,
        paths: &[PathBuf],
        options: UploadOptions,
        reporter: &Arc<dyn Reporter>,
    ) -> Result<UploadResult, Error> {
        let options = options.normalized()?;
        let prepared = prepare(paths, reporter).await?;
        let master = random_key()?;
        let created = self.create(origin, &master, &prepared, options, reporter).await?;
        match self.finish(&prepared, &created, reporter).await {
            Ok(expires_at) => Ok(UploadResult {
                share: created.share,
                upload_token: created.upload_token,
                expires_at,
                downloads: created.downloads,
            }),
            Err(error) => {
                // 撤销失败不覆盖原始错误——真正要报的是上传为什么没成。
                let _ = self.revoke(&created.share, &created.upload_token).await;
                Err(error)
            }
        }
    }

    /// 取清单（不领取下载名额），顺带拿到服务端给的过期时间。
    ///
    /// # Errors
    ///
    /// 凭证不合法、服务端拒绝、解密或校验失败。
    pub async fn inspect(&self, share: &Share) -> Result<(Manifest, Option<OffsetDateTime>), Error> {
        share.browser_url()?;
        let response = self.get_manifest(share).await?;
        let plaintext = crypto::open_manifest(&share.master_key, &share.id, &response.body, MAX_MANIFEST_BYTES)?;
        let parsed = manifest::decode(&plaintext, MAX_MANIFEST_BYTES)?;
        let expires = response.header("x-croc-expires-at").and_then(parse_timestamp);
        Ok((parsed, expires))
    }

    /// 下载并落盘，最后确认下载完成。
    ///
    /// 支持断点续传：状态文件放在输出目录里（`.croc-store-<id>.json`），重跑会跳过已经
    /// 下完的分片。每个文件先写成 `.part`、校验 SHA-256、再改名——落盘的要么是完整文件，
    /// 要么什么都没有。
    ///
    /// # Errors
    ///
    /// 清单不合法、服务端拒绝、校验失败、磁盘写不了。
    pub async fn receive(
        &self,
        share: &Share,
        manifest: &Manifest,
        output: &Path,
        reporter: &Arc<dyn Reporter>,
    ) -> Result<(), Error> {
        share.browser_url()?;
        manifest::validate_manifest(manifest, MAX_MANIFEST_BYTES)?;
        std::fs::create_dir_all(output)?;

        let mut session = Session::load(share, manifest, output);
        if session.state.claim_token.is_empty() {
            session.state.claim_token = self.claim(share).await?;
            session.save()?;
        }

        for (index, file) in manifest.files.iter().enumerate() {
            if session.state.renamed.contains_key(&file.name) {
                session.transferred += file.size;
                continue;
            }
            self.receive_file(&mut session, file, index, reporter).await?;
        }

        session.state.verified = true;
        session.save()?;
        reporter.status("Committing verified download…");
        let remaining = self.commit_with_retry(&mut session).await?;
        match remaining {
            0 => reporter.status("Verified download committed; stored ciphertext removed"),
            1 => reporter.status("Verified download committed; one download remains"),
            count => reporter.status(&format!("Verified download committed; {count} downloads remain")),
        }
        let _ = std::fs::remove_file(session.state_file());
        Ok(())
    }

    /// 撤销一次传输。
    ///
    /// # Errors
    ///
    /// 凭证不合法或服务端拒绝。
    pub async fn revoke(&self, share: &Share, upload_token: &str) -> Result<(), Error> {
        share.browser_url()?;
        self.http
            .send(Request::new("DELETE", &api_url(&share.origin, &format!("/{}", share.id))).bearer(upload_token))
            .await?;
        Ok(())
    }

    // ---------------------------------------------------------------- 内部

    async fn create(
        &self,
        origin: &str,
        master: &[u8],
        prepared: &Prepared,
        options: UploadOptions,
        reporter: &Arc<dyn Reporter>,
    ) -> Result<Created, Error> {
        // 先确认来源合法，免得收完文件才发现 URL 写错了。
        Share {
            origin: origin.to_owned(),
            id: base64url::encode(&[0u8; TRANSFER_ID_LEN]),
            master_key: vec![0u8; KEY_SIZE],
        }
        .browser_url()?;

        let redeem = crypto::redeem_capability(master)?;
        let request = CreateRequest {
            protocol: PROTOCOL.to_owned(),
            manifest_bytes: i64::try_from(prepared.manifest_json.len()).unwrap_or(i64::MAX) + SEAL_OVERHEAD,
            chunk_bytes: prepared.chunk_bytes.clone(),
            redeem_verifier: base64url::encode(&crypto::capability_verifier(&redeem)),
            files: prepared.files.len(),
            plaintext_bytes: prepared.total_bytes,
            // 只有与默认值不同才发出去：这两个字段是"请求"，服务端有权拒绝。
            downloads: (options.downloads != 1).then_some(options.downloads),
            expires_seconds: (options.expiration != DEFAULT_EXPIRATION).then_some(options.expiration.whole_seconds()),
        };
        reporter.status("Reserving encrypted temporary storage…");
        let body = serde_json::to_vec(&request)?;
        let response = self.http.send(Request::new("POST", &api_url(origin, "")).json(&body)).await?;
        let created: CreateResponse = decode(&response.body)?;
        let accepted = match response.header("x-croc-downloads").map(str::trim).filter(|value| !value.is_empty()) {
            Some(value) => value
                .parse::<i64>()
                .map_err(|_| Error::Format("storage service returned an invalid download count".to_owned()))?,
            None => 1,
        };
        if accepted < 1 {
            return Err(Error::Format("storage service returned an invalid download count".to_owned()));
        }
        if created.chunk_size != CHUNK_SIZE {
            return Err(Error::Format(format!(
                "storage service returned unsupported chunk size {}",
                created.chunk_size
            )));
        }
        if accepted != options.downloads {
            return Err(Error::Format(format!(
                "storage service created {accepted} downloads instead of {}",
                options.downloads
            )));
        }
        let share = Share { origin: origin.to_owned(), id: created.id, master_key: master.to_vec() };
        if let Err(error) = share.browser_url() {
            return Err(Error::Format(format!("storage service returned an invalid transfer id: {error}")));
        }
        if !valid_capability(&created.upload_token) {
            return Err(Error::Format("storage service returned an invalid upload capability".to_owned()));
        }
        Ok(Created { share, upload_token: created.upload_token, downloads: accepted })
    }

    /// 上传清单与全部分片。
    async fn finish(
        &self,
        prepared: &Prepared,
        created: &Created,
        reporter: &Arc<dyn Reporter>,
    ) -> Result<OffsetDateTime, Error> {
        let encrypted = crypto::seal_manifest(&created.share.master_key, &created.share.id, &prepared.manifest_json)?;
        reporter.status("Uploading encrypted manifest…");
        self.put(&created.share, &created.upload_token, "/manifest", &encrypted).await?;

        let shared = Arc::new(Shared {
            http: self.http.clone(),
            share: created.share.clone(),
            token: created.upload_token.clone(),
            references: manifest::chunk_refs(&prepared.manifest),
            total_size: prepared.total_bytes,
            file_count: prepared.files.len(),
            sent: AtomicI64::new(0),
            reporter: Arc::clone(reporter),
        });

        let mut set = tokio::task::JoinSet::new();
        for (file_index, file) in prepared.files.iter().enumerate() {
            if file.chunk_count == 0 {
                continue;
            }
            let file = Arc::new(file.clone());
            let next = Arc::new(AtomicUsize::new(0));
            let per_file_sent = Arc::new(AtomicI64::new(0));
            let count = usize::try_from(file.chunk_count).unwrap_or(usize::MAX);
            for _ in 0..WORKERS.min(count) {
                let shared = Arc::clone(&shared);
                let file = Arc::clone(&file);
                let next = Arc::clone(&next);
                let per_file_sent = Arc::clone(&per_file_sent);
                set.spawn(async move { upload_worker(shared, file, file_index, next, per_file_sent).await });
            }
        }
        while let Some(joined) = set.join_next().await {
            joined.map_err(|error| Error::Transport(format!("upload task failed: {error}")))??;
        }
        self.complete(&created.share, &created.upload_token, reporter).await
    }

    async fn complete(
        &self,
        share: &Share,
        token: &str,
        reporter: &Arc<dyn Reporter>,
    ) -> Result<OffsetDateTime, Error> {
        reporter.status("Finalizing encrypted transfer…");
        let response = self
            .http
            .send(Request::new("POST", &api_url(&share.origin, &format!("/{}/complete", share.id))).bearer(token))
            .await?;
        let parsed: CompleteResponse = decode(&response.body)?;
        parsed.expires_at.ok_or_else(|| Error::Format("storage service returned an invalid expiration time".to_owned()))
    }

    /// 上传一份密文，网络错误与 5xx 重试。
    async fn put(&self, share: &Share, token: &str, suffix: &str, payload: &[u8]) -> Result<(), Error> {
        let url = api_url(&share.origin, &format!("/{}{suffix}", share.id));
        let checksum = crypto::encode_sha256(payload);
        let mut last = None;
        for attempt in 0..UPLOAD_ATTEMPTS {
            match self
                .http
                .send(
                    Request::new("PUT", &url)
                        .bearer(token)
                        .octet_stream()
                        .header("X-Croc-SHA256", &checksum)
                        .body(payload),
                )
                .await
            {
                Ok(_) => return Ok(()),
                Err(error) => {
                    if retryable(&error) {
                        last = Some(error);
                        backoff(attempt).await;
                    } else {
                        return Err(error);
                    }
                }
            }
        }
        Err(last.unwrap_or_else(|| Error::Transport("upload failed".to_owned())))
    }

    /// 取清单。这一条用**取件能力**——领取下载名额之前也要能看清单。
    async fn get_manifest(&self, share: &Share) -> Result<Response, Error> {
        let token = base64url::encode(&crypto::redeem_capability(&share.master_key)?);
        let url = api_url(&share.origin, &format!("/{}/manifest", share.id));
        self.http.send(Request::new("GET", &url).bearer(&token)).await
    }

    /// 取一个分片。这一条用**名额令牌**：取件能力只能拿清单，拿不动密文。
    async fn get_chunk(&self, share: &Share, claim_token: &str, index: i64) -> Result<Response, Error> {
        let url = api_url(&share.origin, &format!("/{}/chunks/{index}", share.id));
        self.http.send(Request::new("GET", &url).bearer(claim_token)).await
    }

    /// 领取一个下载名额。
    async fn claim(&self, share: &Share) -> Result<String, Error> {
        let token = base64url::encode(&crypto::redeem_capability(&share.master_key)?);
        let response = self
            .http
            .send(Request::new("POST", &api_url(&share.origin, &format!("/{}/claim", share.id))).bearer(&token))
            .await?;
        let claimed: ClaimResponse = decode(&response.body)?;
        if !valid_capability(&claimed.claim_token) {
            return Err(Error::Format("storage service returned an invalid claim capability".to_owned()));
        }
        Ok(claimed.claim_token)
    }

    /// 确认下载完成，返回还剩几次。
    async fn commit(&self, share: &Share, claim_token: &str) -> Result<i64, Error> {
        let response = self
            .http
            .send(Request::new("POST", &api_url(&share.origin, &format!("/{}/commit", share.id))).bearer(claim_token))
            .await?;
        // 老服务端没有这个头：它一定把唯一一次下载用掉了。
        response.header("x-croc-downloads-remaining").map(str::trim).filter(|value| !value.is_empty()).map_or(
            Ok(0),
            |value| {
                value.parse::<i64>().map_err(|_| {
                    Error::Format("storage service returned an invalid remaining-download count".to_owned())
                })
            },
        )
    }

    /// 取一个分片；名额过期就续一个再来。
    ///
    /// 断点续传时手里的名额可能早就过期了，服务端会回 404 / 410——这不是错误，
    /// 只是要重新领一个。
    async fn chunk_with_retry(&self, session: &mut Session, index: i64) -> Result<Vec<u8>, Error> {
        match self.get_chunk(&session.share, &session.state.claim_token, index).await {
            Ok(response) => return Ok(response.body),
            Err(error) if expired_claim(&error) => {}
            Err(error) => return Err(error),
        }
        session.state.claim_token = self.claim(&session.share).await?;
        session.save()?;
        Ok(self.get_chunk(&session.share, &session.state.claim_token, index).await?.body)
    }

    async fn commit_with_retry(&self, session: &mut Session) -> Result<i64, Error> {
        let token = session.state.claim_token.clone();
        match self.commit(&session.share, &token).await {
            Ok(remaining) => Ok(remaining),
            Err(error) if expired_claim(&error) => {
                session.state.claim_token = self.claim(&session.share).await?;
                session.save()?;
                self.commit(&session.share, &session.state.claim_token).await
            }
            Err(error) => Err(error),
        }
    }

    async fn receive_file(
        &self,
        session: &mut Session,
        file: &ManifestFile,
        index: usize,
        reporter: &Arc<dyn Reporter>,
    ) -> Result<(), Error> {
        let part = session.output.join(part_name(&file.name, &session.share.id));
        // 不截断：续传时文件里可能已经有下好的分片。
        let mut handle =
            tokio::fs::OpenOptions::new().create(true).truncate(false).read(true).write(true).open(&part).await?;
        handle.set_len(u64::try_from(file.size).unwrap_or(u64::MAX)).await?;

        let file_index = i64::try_from(index).unwrap_or(i64::MAX);
        let pending: Vec<ChunkRef> =
            session.references.iter().filter(|reference| reference.file_index == file_index).copied().collect();

        // 已经下完的（续传）也要算进进度里。
        let mut done = pending
            .iter()
            .filter(|reference| session.state.completed.contains(&reference.object_index))
            .map(|reference| reference.plain_size)
            .sum::<i64>();
        session.transferred += done;

        reporter.status(&format!("Downloading {}", file.name));
        for reference in pending {
            if session.state.completed.contains(&reference.object_index) {
                continue;
            }
            let ciphertext = self.chunk_with_retry(session, reference.object_index).await?;
            let plaintext = crypto::open_chunk(&session.share.master_key, &session.share.id, reference, &ciphertext)?;
            let offset = u64::try_from(reference.file_chunk * CHUNK_SIZE).unwrap_or(u64::MAX);
            handle.seek(std::io::SeekFrom::Start(offset)).await?;
            handle.write_all(&plaintext).await?;
            session.state.completed.insert(reference.object_index);
            session.checkpoints += 1;
            done += reference.plain_size;
            session.transferred += reference.plain_size;
            if session.checkpoints >= CHECKPOINTS {
                session.save()?;
                session.checkpoints = 0;
            }
            reporter.progress(Progress {
                file_index: index,
                file_count: session.file_count,
                file_name: file.name.clone(),
                file_bytes: done,
                file_size: file.size,
                total_bytes: session.transferred,
                total_size: session.total,
            });
        }
        handle.sync_all().await?;
        drop(handle);

        install_verified(file, &part, &session.output, reporter).await?;
        session.state.renamed.insert(file.name.clone(), true);
        session.save()
    }
}

/// 上传任务之间共享的东西。
struct Shared {
    http: HttpClient,
    share: Share,
    token: String,
    references: Vec<ChunkRef>,
    total_size: i64,
    file_count: usize,
    sent: AtomicI64,
    reporter: Arc<dyn Reporter>,
}

/// 从一个文件里取分片、加密、上传，直到取完。
async fn upload_worker(
    shared: Arc<Shared>,
    file: Arc<PreparedFile>,
    file_index: usize,
    next: Arc<AtomicUsize>,
    file_sent: Arc<AtomicI64>,
) -> Result<(), Error> {
    let mut handle = tokio::fs::File::open(&file.path).await?;
    let count = usize::try_from(file.chunk_count).unwrap_or(usize::MAX);
    let base = usize::try_from(file.first_chunk).unwrap_or(usize::MAX);
    loop {
        let sequence = next.fetch_add(1, Ordering::SeqCst);
        if sequence >= count {
            return Ok(());
        }
        let Some(reference) = shared.references.get(base.wrapping_add(sequence)).copied() else {
            return Err(Error::Format("stored-transfer chunk map is out of range".to_owned()));
        };
        let offset = u64::try_from(i64::try_from(sequence).unwrap_or(i64::MAX) * CHUNK_SIZE).unwrap_or(u64::MAX);
        let mut plaintext = vec![0u8; usize::try_from(reference.plain_size).unwrap_or(0)];
        handle.seek(std::io::SeekFrom::Start(offset)).await?;
        handle.read_exact(&mut plaintext).await?;
        let ciphertext = crypto::seal_chunk(&shared.share.master_key, &shared.share.id, reference, &plaintext)?;
        shared.reporter.status(&format!("Uploading {}", file.name));
        put_chunk(&shared, reference.object_index, &ciphertext).await?;

        let sent = file_sent.fetch_add(reference.plain_size, Ordering::SeqCst) + reference.plain_size;
        let total = shared.sent.fetch_add(reference.plain_size, Ordering::SeqCst) + reference.plain_size;
        shared.reporter.progress(Progress {
            file_index,
            file_count: shared.file_count,
            file_name: file.name.clone(),
            file_bytes: sent,
            file_size: file.size,
            total_bytes: total,
            total_size: shared.total_size,
        });
    }
}

/// 上传一个分片，网络错误与 5xx 重试。
async fn put_chunk(shared: &Shared, object_index: i64, ciphertext: &[u8]) -> Result<(), Error> {
    let url = api_url(&shared.share.origin, &format!("/{}/chunks/{object_index}", shared.share.id));
    let checksum = crypto::encode_sha256(ciphertext);
    let mut last = None;
    for attempt in 0..UPLOAD_ATTEMPTS {
        match shared
            .http
            .send(
                Request::new("PUT", &url)
                    .bearer(&shared.token)
                    .octet_stream()
                    .header("X-Croc-SHA256", &checksum)
                    .body(ciphertext),
            )
            .await
        {
            Ok(_) => return Ok(()),
            Err(error) => {
                if retryable(&error) {
                    last = Some(error);
                    backoff(attempt).await;
                } else {
                    return Err(error);
                }
            }
        }
    }
    Err(last.unwrap_or_else(|| Error::Transport("chunk upload failed".to_owned())))
}

/// 值得重试吗：网络错误、超时、以及 5xx。4xx 是"这个请求本身不对"。
fn retryable(error: &Error) -> bool {
    match error {
        Error::Service { status, .. } => *status >= 500,
        Error::Transport(_) => true,
        _ => false,
    }
}

/// 名额没了。
fn expired_claim(error: &Error) -> bool {
    matches!(error, Error::Service { status, .. } if *status == 404 || *status == 410)
}

/// 退避等待。Go 那边是 `(attempt+1) * 250ms`。
async fn backoff(attempt: u32) {
    let millis = u64::from(attempt + 1) * 250;
    tokio::time::sleep(std::time::Duration::from_millis(millis)).await;
}

/// 准备上传：算哈希、排分片。
struct Prepared {
    files: Vec<PreparedFile>,
    manifest: Manifest,
    manifest_json: Vec<u8>,
    chunk_bytes: Vec<i64>,
    total_bytes: i64,
}

/// 一个待上传的文件。
#[derive(Debug, Clone)]
struct PreparedFile {
    path: PathBuf,
    name: String,
    size: i64,
    first_chunk: i64,
    chunk_count: i64,
}

async fn prepare(paths: &[PathBuf], reporter: &Arc<dyn Reporter>) -> Result<Prepared, Error> {
    if paths.is_empty() || paths.len() > MAX_FILES {
        return Err(Error::Format(format!("stored transfer must contain between 1 and {MAX_FILES} files")));
    }
    let mut files = Vec::with_capacity(paths.len());
    let mut manifest_files = Vec::with_capacity(paths.len());
    let mut names: Vec<String> = Vec::with_capacity(paths.len());
    let mut total_bytes = 0i64;
    let mut next_chunk = 0i64;

    for (index, path) in paths.iter().enumerate() {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(Error::Format(format!("stored mode supports regular files only: {}", path.display())));
        }
        let Some(name) = path.file_name().and_then(|value| value.to_str()).map(ToOwned::to_owned) else {
            return Err(Error::Format(format!("unsupported file name: {}", path.display())));
        };
        if names.contains(&name) {
            return Err(Error::Format(format!("duplicate stored-transfer filename: {name}")));
        }
        names.push(name.clone());

        let size = i64::try_from(metadata.len()).unwrap_or(i64::MAX);
        total_bytes += size;
        reporter.status(&format!("Hashing {}/{}: {name}", index + 1, paths.len()));
        let digest = hash_file(path).await?;
        let chunk_count = (size + CHUNK_SIZE - 1) / CHUNK_SIZE;

        manifest_files.push(ManifestFile {
            name: name.clone(),
            size,
            modified: metadata.modified().ok().and_then(system_time_to_offset).unwrap_or(OffsetDateTime::UNIX_EPOCH),
            sha256: base64url::encode(&digest),
            first_chunk: next_chunk,
            chunk_count,
        });
        files.push(PreparedFile { path: path.clone(), name, size, first_chunk: next_chunk, chunk_count });
        next_chunk += chunk_count;
    }

    let document = Manifest { version: VERSION, chunk_size: CHUNK_SIZE, files: manifest_files };
    manifest::validate_manifest(&document, MAX_MANIFEST_BYTES)?;
    let manifest_json = serde_json::to_vec(&document)?;

    let mut chunk_bytes = Vec::new();
    for file in &files {
        let mut remaining = file.size;
        for _ in 0..file.chunk_count {
            let plain = remaining.min(CHUNK_SIZE);
            chunk_bytes.push(plain + SEAL_OVERHEAD);
            remaining -= plain;
        }
    }
    Ok(Prepared { files, manifest: document, manifest_json, chunk_bytes, total_bytes })
}

/// 一次下载的会话状态。
struct Session {
    share: Share,
    output: PathBuf,
    references: Vec<ChunkRef>,
    file_count: usize,
    total: i64,
    transferred: i64,
    checkpoints: u32,
    state: DownloadState,
}

impl Session {
    fn load(share: &Share, manifest: &Manifest, output: &Path) -> Self {
        let manifest_hash = crypto::encode_sha256(&serde_json::to_vec(manifest).unwrap_or_default());
        let fresh = DownloadState {
            version: VERSION,
            id: share.id.clone(),
            manifest_hash,
            claim_token: String::new(),
            completed: BTreeSet::new(),
            renamed: BTreeMap::new(),
            verified: false,
        };
        // 只有同一份传输、同一份清单的状态才能拿来续传——否则会把上一次的"已完成"
        // 当成这一次的，跳到一半才发现分片对不上。
        let state = std::fs::read(state_file(share, output))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<DownloadState>(&bytes).ok())
            .filter(|loaded| loaded.id == fresh.id && loaded.manifest_hash == fresh.manifest_hash)
            .unwrap_or(fresh);
        Self {
            share: share.clone(),
            output: output.to_path_buf(),
            references: manifest::chunk_refs(manifest),
            file_count: manifest.files.len(),
            total: manifest.files.iter().map(|file| file.size).sum(),
            transferred: 0,
            checkpoints: 0,
            state,
        }
    }

    fn state_file(&self) -> PathBuf {
        state_file(&self.share, &self.output)
    }

    fn save(&self) -> Result<(), Error> {
        std::fs::write(self.state_file(), serde_json::to_vec_pretty(&self.state)?)?;
        Ok(())
    }
}

/// 断点续传的状态。字段名与 Go 的一致，两种实现可以互相续传。
#[derive(Debug, Clone, Serialize, Deserialize)]
struct DownloadState {
    version: u32,
    id: String,
    #[serde(rename = "manifestHash")]
    manifest_hash: String,
    #[serde(rename = "claimToken")]
    claim_token: String,
    completed: BTreeSet<i64>,
    renamed: BTreeMap<String, bool>,
    verified: bool,
}

/// 校验哈希、设时间、改名。
async fn install_verified(
    file: &ManifestFile,
    part: &Path,
    output: &Path,
    reporter: &Arc<dyn Reporter>,
) -> Result<(), Error> {
    reporter.status(&format!("Verifying {}", file.name));
    let digest = hash_file(part).await?;
    if base64url::encode(&digest) != file.sha256 {
        return Err(Error::Format(format!("stored-transfer hash verification failed for {}", file.name)));
    }
    if let Ok(handle) = std::fs::File::open(part)
        && let Some(modified) = offset_to_system_time(file.modified)
    {
        let _ = handle.set_modified(modified);
    }
    let target = output.join(&file.name);
    // Windows 上 rename 不覆盖已存在的文件。
    let _ = std::fs::remove_file(&target);
    std::fs::rename(part, &target)?;
    Ok(())
}

/// 分片的临时文件名。与 Go 的 `.<名字>.croc-<id>.part` 同形。
fn part_name(name: &str, id: &str) -> String {
    format!(".{name}.croc-{id}.part")
}

fn state_file(share: &Share, output: &Path) -> PathBuf {
    output.join(format!(".croc-store-{}.json", share.id))
}

fn api_url(origin: &str, suffix: &str) -> String {
    format!("{}/api/v1/store/transfers{suffix}", origin.trim_end_matches('/'))
}

fn valid_capability(value: &str) -> bool {
    base64url::decode(value).is_ok_and(|capability| capability.len() == 32 && base64url::encode(&capability) == value)
}

fn random_key() -> Result<Vec<u8>, Error> {
    let mut key = vec![0u8; KEY_SIZE];
    getrandom::fill(&mut key).map_err(|error| Error::Random(error.to_string()))?;
    Ok(key)
}

async fn hash_file(path: &Path) -> Result<Vec<u8>, Error> {
    let mut handle = tokio::fs::File::open(path).await?;
    let mut hasher = sha2::Sha256::new();
    let mut buffer = vec![0u8; 256 * 1024];
    loop {
        let read = handle.read(&mut buffer).await?;
        if read == 0 {
            break;
        }
        hasher.update(buffer.get(..read).unwrap_or_default());
    }
    Ok(hasher.finalize().to_vec())
}

/// 解析一个响应体：拒绝未知字段，拒绝尾随数据。
fn decode<T: serde::de::DeserializeOwned>(body: &[u8]) -> Result<T, Error> {
    let mut deserializer = serde_json::Deserializer::from_slice(body);
    let value = T::deserialize(&mut deserializer)?;
    deserializer.end()?;
    Ok(value)
}

fn parse_timestamp(value: &str) -> Option<OffsetDateTime> {
    OffsetDateTime::parse(value.trim(), &time::format_description::well_known::Rfc3339).ok()
}

fn system_time_to_offset(value: std::time::SystemTime) -> Option<OffsetDateTime> {
    let duration = value.duration_since(std::time::UNIX_EPOCH).ok()?;
    let seconds = i64::try_from(duration.as_secs()).ok()?;
    OffsetDateTime::from_unix_timestamp(seconds)
        .ok()?
        .checked_add(Duration::nanoseconds(i64::from(duration.subsec_nanos())))
}

fn offset_to_system_time(value: OffsetDateTime) -> Option<std::time::SystemTime> {
    let seconds = u64::try_from(value.unix_timestamp()).ok()?;
    Some(std::time::UNIX_EPOCH + std::time::Duration::new(seconds, value.nanosecond()))
}

// ---------------------------------------------------------------- 线路类型

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct CreateRequest {
    protocol: String,
    manifest_bytes: i64,
    chunk_bytes: Vec<i64>,
    redeem_verifier: String,
    files: usize,
    plaintext_bytes: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloads: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    expires_seconds: Option<i64>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CreateResponse {
    id: String,
    upload_token: String,
    // 服务端一定给，本地用不上。
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[allow(dead_code, reason = "只是为了让未知字段检查通过")]
    upload_expires_at: Option<OffsetDateTime>,
    chunk_size: i64,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct CompleteResponse {
    #[serde(default, with = "time::serde::rfc3339::option")]
    expires_at: Option<OffsetDateTime>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct ClaimResponse {
    claim_token: String,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[allow(dead_code, reason = "只是为了让未知字段检查通过")]
    claim_expires_at: Option<OffsetDateTime>,
    #[serde(default, with = "time::serde::rfc3339::option")]
    #[allow(dead_code, reason = "只是为了让未知字段检查通过")]
    expires_at: Option<OffsetDateTime>,
}

/// 上传途中在服务端那边建好的传输。
struct Created {
    share: Share,
    upload_token: String,
    downloads: i64,
}
