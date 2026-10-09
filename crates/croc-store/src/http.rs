//! HTTP 客户端。
//!
//! 用 `reqwest`，配置按这个协议的需要收窄：
//!
//! - 不跟随重定向：取件凭证在 URL 的 fragment 里，跟着 3xx 跑到别的主机就等于把
//!   凭证交出去（Go 那边也是显式拒绝）；
//! - 连接超时与整体超时分开：一个 4 MiB 的分片在慢网络下要传一会儿，但连不上不该等那么久；
//! - 代理只在给了 `--socks5` / `--connect` 时挂上，且 socks5 走 `socks5h`——域名交给
//!   代理解析，本地解析会把 DNS 请求漏到网外。

use std::time::Duration;

use crate::Error;
use crate::proxy::Proxy;

/// 建连超时。
const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);

/// 单个请求的整体超时。
const REQUEST_TIMEOUT: Duration = Duration::from_secs(600);

/// 一次请求的响应。
#[derive(Debug, Clone)]
pub struct Response {
    /// 状态码。
    #[allow(dead_code, reason = "非 2xx 已经在上游变成错误了，这里留着是为了出错时能直接看")]
    pub status: u16,
    /// 响应头，名字统一小写。
    pub headers: Vec<(String, String)>,
    /// 响应体。
    pub body: Vec<u8>,
}

impl Response {
    /// 取一个响应头（名字大小写不敏感）。
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers.iter().find(|(key, _)| key == name).map(|(_, value)| value.as_str())
    }
}

/// 一次请求。
///
/// 拥有型的 builder：上传时请求要跟着任务跑进 `tokio::spawn`，借用型结构在那边
/// 躲不开生命周期。
#[derive(Debug, Clone, Default)]
pub struct Request {
    method: String,
    url: String,
    bearer: Option<String>,
    content_type: Option<String>,
    headers: Vec<(String, String)>,
    body: Option<Vec<u8>>,
}

impl Request {
    /// 新建一个请求。
    #[must_use]
    pub fn new(method: &str, url: &str) -> Self {
        Self { method: method.to_owned(), url: url.to_owned(), ..Self::default() }
    }

    /// 加 `Authorization: Bearer`。
    #[must_use]
    pub fn bearer(mut self, token: &str) -> Self {
        self.bearer = Some(token.to_owned());
        self
    }

    /// 带 JSON 请求体。
    #[must_use]
    pub fn json(mut self, body: &[u8]) -> Self {
        self.content_type = Some("application/json".to_owned());
        self.body = Some(body.to_vec());
        self
    }

    /// `Content-Type: application/octet-stream`。
    #[must_use]
    pub fn octet_stream(mut self) -> Self {
        self.content_type = Some("application/octet-stream".to_owned());
        self
    }

    /// 加一个请求头。
    #[must_use]
    pub fn header(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.to_owned(), value.to_owned()));
        self
    }

    /// 带请求体（不设 `Content-Type`）。
    #[must_use]
    pub fn body(mut self, body: &[u8]) -> Self {
        self.body = Some(body.to_vec());
        self
    }
}

/// HTTP 客户端。
#[derive(Debug, Clone)]
pub struct HttpClient {
    client: reqwest::Client,
}

impl HttpClient {
    /// 建一个客户端。`proxy` 为 `None` 时直连。
    ///
    /// # Errors
    ///
    /// TLS 后端初始化失败，或者代理地址不合法。
    pub fn new(proxy: Option<Proxy>) -> Result<Self, Error> {
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT);
        if let Some(proxy) = &proxy {
            let url = proxy.url();
            let configured =
                reqwest::Proxy::all(&url).map_err(|error| Error::Transport(format!("proxy {url}: {error}")))?;
            builder = builder.proxy(configured);
        }
        Ok(Self { client: builder.build().map_err(|error| Error::Transport(format!("http client: {error}")))? })
    }

    /// 发一次请求。非 2xx 会变成 [`Error::Service`]。
    ///
    /// # Errors
    ///
    /// 连不上、TLS 握手失败、报文不合法、或者服务端返回了非 2xx。
    pub async fn send(&self, request: Request) -> Result<Response, Error> {
        let method = reqwest::Method::from_bytes(request.method.as_bytes())
            .map_err(|error| Error::Transport(format!("bad method {}: {error}", request.method)))?;
        let mut builder = self.client.request(method, &request.url);
        if let Some(bearer) = &request.bearer {
            builder = builder.bearer_auth(bearer);
        }
        if let Some(content_type) = &request.content_type {
            builder = builder.header(reqwest::header::CONTENT_TYPE, content_type);
        }
        for (name, value) in &request.headers {
            builder = builder.header(name.as_str(), value.as_str());
        }
        if let Some(body) = request.body {
            builder = builder.body(body);
        }

        let response = builder.send().await.map_err(|error| Error::Transport(error.to_string()))?;
        let status = response.status().as_u16();
        let headers: Vec<(String, String)> = response
            .headers()
            .iter()
            .map(|(name, value)| (name.as_str().to_ascii_lowercase(), value.to_str().unwrap_or_default().to_owned()))
            .collect();
        let body = response.bytes().await.map_err(|error| Error::Transport(error.to_string()))?.to_vec();

        if (200..300).contains(&status) {
            return Ok(Response { status, headers, body });
        }
        // Go 那边只读 8 KiB 的错误正文；多余的部分对诊断没有帮助，却可能是几 MB 的噪音。
        let limit = body.len().min(8 * 1024);
        let message = String::from_utf8_lossy(body.get(..limit).unwrap_or_default()).trim().to_owned();
        Err(Error::Service {
            status,
            message,
            retry_after: headers.iter().find(|(name, _)| name == "retry-after").map(|(_, value)| value.clone()),
        })
    }
}
