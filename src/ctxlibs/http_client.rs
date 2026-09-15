use std::borrow::Cow;
use std::ops::Deref;
use std::time::Duration;

use crate::error::ScenarioError;
use reqwest::{IntoUrl, Method, Response, StatusCode, Url};
use serde::ser::Error as _;

/// 重新导出 reqwest 的 multipart 类型(`Form` / `Part`),
/// 便于构造 multipart/form-data 表单,且与 crate 内部 reqwest 版本一致。
pub use reqwest::multipart;

/// 请求/响应内容捕获开关。
///
/// 值为 `0` 时表示不捕获该侧内容;默认两侧各截断到 512 字符,见 [`CaptureOptions::default`]。
#[derive(Debug, Clone, Copy)]
pub struct CaptureOptions {
    /// 请求体快照最大字符数,`0` 表示不捕获。
    pub max_request_body: usize,
    /// 响应体快照最大字符数,`0` 表示不捕获(此时也不会读取响应体)。
    pub max_response_body: usize,
}

impl Default for CaptureOptions {
    fn default() -> Self {
        Self {
            max_request_body: 512,
            max_response_body: 512,
        }
    }
}

impl CaptureOptions {
    /// 完全不捕获请求/响应内容(高 QPS 场景降低开销)。
    pub const fn disabled() -> Self {
        Self {
            max_request_body: 0,
            max_response_body: 0,
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP request failed: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("JSON serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Invalid URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("HTTP {method} {url} -> {status}")]
    Status {
        status: StatusCode,
        method: Method,
        url: String,
        /// 请求体快照(开启捕获时,截断后的文本)。
        request_body: Option<String>,
        /// 响应体快照;读取失败时为 `Err(原因)`。开关关闭时为 `None`。
        response_body: Option<Result<String, String>>,
    },
}

impl HttpError {
    /// 构造带请求上下文的 HTTP 状态码错误(方法 + 完整 URL + 状态码)。
    ///
    /// 不带内容快照(业务侧自行构造错误时使用);`Client` 发起请求
    /// 遇到非 2xx 会自动捕获请求/响应内容,无需手动调用。
    pub fn status(status: StatusCode, method: Method, url: &Url) -> Self {
        Self::Status {
            status,
            method,
            url: url.to_string(),
            request_body: None,
            response_body: None,
        }
    }

    /// 从 `reqwest::Response` 构造:读取响应体快照(开关开启时),
    /// 读取失败会以 `Err(原因)` 形式保留在 `response_body` 中,不静默丢弃。
    async fn from_response(
        resp: Response,
        method: Method,
        url: Url,
        request_body: Option<String>,
        capture: &CaptureOptions,
    ) -> Self {
        let status = resp.status();
        let response_body = if capture.max_response_body == 0 {
            None
        } else {
            match resp.text().await {
                Ok(text) => Some(Ok(truncate_log(&text, capture.max_response_body))),
                Err(err) => Some(Err(err.to_string())),
            }
        };
        Self::Status {
            status,
            method,
            url: url.to_string(),
            request_body,
            response_body,
        }
    }
}

impl ScenarioError for HttpError {
    fn kind(&self) -> Cow<'static, str> {
        match self {
            Self::Reqwest(_) => "reqwest".into(),
            Self::Serde(_) => "serde".into(),
            Self::Url(_) => "url_parse".into(),
            Self::Status { status, .. } => format!("http_status_{}", status.as_u16()).into(),
        }
    }
}

/// 直接使用 `reqwest::Client` 时,`reqwest::Error` 也能参与报告错误分类
/// (本地 trait 可为外部类型实现,孤儿规则允许)。
///
/// 状态码错误按 `http_status_{code}` 归类,传输层错误按原因细分,
/// 与 [`HttpError`] 的分类保持一致。
impl ScenarioError for reqwest::Error {
    fn kind(&self) -> Cow<'static, str> {
        if let Some(status) = self.status() {
            return format!("http_status_{}", status.as_u16()).into();
        }
        if self.is_timeout() {
            return "timeout".into();
        }
        if self.is_connect() {
            return "connect".into();
        }
        if self.is_body() || self.is_decode() {
            return "decode".into();
        }
        if self.is_redirect() {
            return "redirect".into();
        }
        if self.is_request() || self.is_builder() {
            return "request".into();
        }
        "reqwest".into()
    }
}

/// 截断快照到最多 `max` 个字符,超出部分标注原文长度。
fn truncate_log(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let head: String = s.chars().take(max).collect();
        format!("{head}...(total {} chars)", s.chars().count())
    }
}

/// 相对路径解析: 去掉前导 `/` 做相对拼接, 保留 base 的路径前缀(如 `/api`)。
/// 传入完整 URL 时原样返回(没有前导 `/`)。
pub fn resolve(base: &Url, path: &str) -> Result<Url, HttpError> {
    Ok(base.join(path.trim_start_matches('/'))?)
}

/// 校验响应状态码:非 200 时读取响应体快照并返回 `HttpError::Status`。
///
/// 直接使用 `reqwest::Client` 时,用它替代 `error_for_status()`
/// (后者会丢弃响应体,排障时看不到失败内容)。
pub async fn check_status(
    resp: Response,
    method: Method,
    capture: &CaptureOptions,
) -> Result<Response, HttpError> {
    if resp.status() == StatusCode::OK {
        return Ok(resp);
    }
    let url = resp.url().clone();
    Err(HttpError::from_response(resp, method, url, None, capture).await)
}

/// HTTP 客户端薄封装:统一 `base_url` 相对路径解析与失败响应内容快照。
///
/// reqwest 的完整能力(超时/连接池/TLS/代理/重定向等)不在此重复包装:
/// 通过 [`Self::from_reqwest`] 传入自定义 `reqwest::Client`,或经 `Deref`
/// 直接调用其方法。注意 `Deref` 暴露的方法是 reqwest 原生的,发起请求时
/// 需要完整 URL(可先用 [`resolve`] 或 [`Self::resolve`] 拼接)。
pub struct Client {
    pub base_url: reqwest::Url,
    inner: reqwest::Client,
    capture: CaptureOptions,
}

/// 保证 base 路径以 `/` 结尾,使相对拼接保留 path 前缀(如 `/api`)。
fn normalize_base(mut base_url: Url) -> Url {
    if !base_url.path().ends_with('/') {
        let path = format!("{}/", base_url.path());
        base_url.set_path(&path);
    }
    base_url
}

/// 默认请求超时(30s)。
///
/// reqwest 默认**不设超时**,服务端挂起时请求会无限期等待,拖死整个压测进程。
/// `Client::new` 会应用该兕底值;需要自定义超时(上传大文件等)时用
/// [`Client::from_reqwest`] 自行构建。
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(30);

impl Client {
    /// 使用默认配置:统一 `base_url` 相对路径解析 + 30s 请求超时
    /// (见 [`DEFAULT_TIMEOUT`]);需要自定义超时/连接池/TLS 时用 [`Self::from_reqwest`]。
    pub fn new(base_url: impl IntoUrl) -> Result<Self, HttpError> {
        let inner = reqwest::Client::builder()
            .timeout(DEFAULT_TIMEOUT)
            .build()?;
        Self::from_reqwest(inner, base_url)
    }

    /// 用自定义的 `reqwest::Client` 构造, 底层能力(超时、连接池、
    /// keep-alive、TLS、代理等)由调用方透过 `reqwest::Client::builder()` 配置。
    pub fn from_reqwest(inner: reqwest::Client, base_url: impl IntoUrl) -> Result<Self, HttpError> {
        Ok(Self {
            base_url: normalize_base(base_url.into_url()?),
            inner,
            capture: CaptureOptions::default(),
        })
    }

    /// 链式设置请求/响应内容捕获(默认开启,截断 512 字符;`0` 关闭)。
    pub fn with_capture(mut self, capture: CaptureOptions) -> Self {
        self.capture = capture;
        self
    }

    /// 底层 `reqwest::Client` 访问器,用于绕过本封装使用其完整 API。
    /// 注意其方法需要完整 URL;相对路径请先用 [`Self::resolve`] 拼接。
    pub fn inner(&self) -> &reqwest::Client {
        &self.inner
    }

    /// 解析请求路径: 去掉前导 `/` 做相对拼接, 保留 base 的路径前缀(如 `/api`)。
    /// 传入完整 URL 时原样返回。
    pub fn resolve(&self, path: &str) -> Result<Url, HttpError> {
        resolve(&self.base_url, path)
    }

    /// 唯一请求入口: 链式配置后以 [`RequestBuilder::send`] /
    /// [`RequestBuilder::send_checked`] 发送。相对路径自动拼接 `base_url`。
    pub fn request(&self, method: reqwest::Method, url: &str) -> RequestBuilder<'_> {
        RequestBuilder::new(self, method, url)
    }

    /// 请求体快照(开关开启时),否则 `None`。
    fn capture_body(&self, bytes: &[u8]) -> Option<String> {
        if self.capture.max_request_body == 0 {
            None
        } else {
            Some(truncate_log(
                &String::from_utf8_lossy(bytes),
                self.capture.max_request_body,
            ))
        }
    }
}

impl Deref for Client {
    type Target = reqwest::Client;

    fn deref(&self) -> &Self::Target {
        &self.inner
    }
}

/// 自定义请求构建器,支持更灵活的配置。
///
/// 与 `reqwest::RequestBuilder` 的区别:相对路径自动拼接 `base_url`,
/// `send_checked` 会把非 200 响应转成带内容快照的 [`HttpError::Status`]。
pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: reqwest::Method,
    url: String,
    body: Option<reqwest::Body>,
    /// multipart 表单;与 `body` 互斥(设置其一即清空另一)
    multipart: Option<reqwest::multipart::Form>,
    headers: reqwest::header::HeaderMap,
    query: Vec<(String, String)>,
    body_snapshot: Option<String>,
}

impl<'a> RequestBuilder<'a> {
    pub fn new(client: &'a Client, method: reqwest::Method, url: &str) -> Self {
        Self {
            client,
            method,
            url: url.to_string(),
            body: None,
            multipart: None,
            headers: reqwest::header::HeaderMap::new(),
            query: Vec::new(),
            body_snapshot: None,
        }
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
            && let Ok(header_value) = reqwest::header::HeaderValue::from_str(value)
        {
            self.headers.insert(header_name, header_value);
        }
        self
    }

    pub fn query(mut self, key: &str, value: &str) -> Self {
        self.query.push((key.to_string(), value.to_string()));
        self
    }

    pub fn query_vec(mut self, params: &[(String, String)]) -> Self {
        self.query.extend(params.iter().cloned());
        self
    }

    /// 设置 JSON body
    pub fn json<B: serde::Serialize>(mut self, body: &B) -> Result<Self, serde_json::Error> {
        let bytes = serde_json::to_vec(body)?;
        self.multipart = None;
        self.body_snapshot = self.client.capture_body(&bytes);
        self.body = Some(reqwest::Body::from(bytes));
        self.headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/json"),
        );
        Ok(self)
    }

    /// 设置 JSON body (失败时 panic)
    pub fn json_unwrap<B: serde::Serialize>(self, body: &B) -> Self {
        self.json(body).unwrap()
    }

    pub fn body(mut self, body: impl Into<reqwest::Body>) -> Self {
        self.multipart = None;
        self.body = Some(body.into());
        self
    }

    /// 设置表单数据
    pub fn form<B: serde::Serialize>(mut self, form: &B) -> Result<Self, serde_json::Error> {
        let encoded = serde_urlencoded::to_string(form)
            .map_err(|e| serde_json::Error::custom(e.to_string()))?;
        self.multipart = None;
        self.body_snapshot = self.client.capture_body(encoded.as_bytes());
        self.body = Some(reqwest::Body::from(encoded.into_bytes()));
        self.headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        Ok(self)
    }

    /// 设置 multipart/form-data 表单(文件上传等)。
    ///
    /// 与 [`Self::json`]/[`Self::form`]/[`Self::body`] 互斥,后设置者生效。
    /// 二进制内容不做文本快照,错误上下文中的 `request_body` 为 `None`。
    pub fn multipart(mut self, form: reqwest::multipart::Form) -> Self {
        self.body = None;
        self.body_snapshot = None;
        self.multipart = Some(form);
        self
    }

    /// 统一的发送实现:解析完整 URL 并发起请求。
    async fn send_impl(
        client: &Client,
        method: reqwest::Method,
        full_url: Url,
        headers: reqwest::header::HeaderMap,
        query: Vec<(String, String)>,
        body: Option<reqwest::Body>,
        multipart: Option<reqwest::multipart::Form>,
    ) -> Result<Response, HttpError> {
        let mut req_builder = client.inner.request(method, full_url);

        // 添加 headers
        for (key, value) in headers {
            if let Some(key) = key {
                req_builder = req_builder.header(key, value);
            }
        }

        // 添加 query 参数
        if !query.is_empty() {
            req_builder = req_builder.query(&query);
        }

        // 添加 body;multipart 与普通 body 互斥,优先 multipart
        if let Some(form) = multipart {
            req_builder = req_builder.multipart(form);
        } else if let Some(body) = body {
            req_builder = req_builder.body(body);
        }

        req_builder.send().await.map_err(HttpError::from)
    }

    /// 发送请求,原样返回响应(不校验状态码)。
    pub async fn send(self) -> Result<Response, HttpError> {
        let RequestBuilder {
            client,
            method,
            url: path,
            body,
            multipart,
            headers,
            query,
            body_snapshot: _,
        } = self;
        let full_url = client.resolve(&path)?;
        Self::send_impl(client, method, full_url, headers, query, body, multipart).await
    }

    /// 发送请求并自动检查状态码:非 200 时自动捕获请求/响应内容快照
    /// 并返回 [`HttpError::Status`],便于压测报告按状态码分类统计。
    pub async fn send_checked(self) -> Result<Response, HttpError> {
        let RequestBuilder {
            client,
            method,
            url: path,
            body,
            multipart,
            headers,
            query,
            body_snapshot,
        } = self;
        let full_url = client.resolve(&path)?;
        let resp = Self::send_impl(
            client,
            method.clone(),
            full_url.clone(),
            headers,
            query,
            body,
            multipart,
        )
        .await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::from_response(
                resp,
                method,
                full_url,
                body_snapshot,
                &client.capture,
            )
            .await);
        }
        Ok(resp)
    }
}

#[cfg(test)]
mod tests {
    use super::{Client, resolve};

    fn base_url(base: &str) -> reqwest::Url {
        Client::new(base).unwrap().base_url
    }

    fn resolve_url(base: &str, path: &str) -> String {
        resolve(&base_url(base), path).unwrap().to_string()
    }

    #[test]
    fn keep_base_path_prefix() {
        // 带路径前缀的 base(如 /api): 前导 / 的请求路径应保留前缀
        assert_eq!(
            resolve_url("https://memory-seek.driftcloud.cn/api", "/auth/login"),
            "https://memory-seek.driftcloud.cn/api/auth/login"
        );
    }

    #[test]
    fn no_prefix_unchanged() {
        assert_eq!(
            resolve_url("http://127.0.0.1:7985", "/auth/login"),
            "http://127.0.0.1:7985/auth/login"
        );
    }

    #[test]
    fn query_preserved() {
        assert_eq!(
            resolve_url(
                "http://localhost:8025",
                "/api/v2/messages?limit=100&order=desc"
            ),
            "http://localhost:8025/api/v2/messages?limit=100&order=desc"
        );
    }

    #[test]
    fn absolute_url_passthrough() {
        assert_eq!(
            resolve_url("http://127.0.0.1:7985", "https://other.example/x"),
            "https://other.example/x"
        );
    }
}
