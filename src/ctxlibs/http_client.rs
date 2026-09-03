use crate::error::ScenarioError;
use reqwest::{IntoUrl, Response, StatusCode};

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP request failed {0}")]
    Http(#[from] reqwest::Error),
    #[error("Parse url failed {0}")]
    UrlParse(#[from] url::ParseError),
    #[error("Unexpected status code {0}")]
    Status(StatusCode),
}

impl HttpError {
    /// 构造状态码错误(供场景对非 2xx 响应使用)。
    pub fn status(code: StatusCode) -> Self {
        Self::Status(code)
    }
}

impl ScenarioError for HttpError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Http(_) => "http",
            Self::UrlParse(_) => "url_parse",
            Self::Status(code) => match code.as_u16() {
                400..=499 => "http_status_4xx",
                500..=599 => "http_status_5xx",
                _ => "http_status_other",
            },
        }
    }
}

pub struct Client {
    pub base_url: reqwest::Url,
    inner: reqwest::Client,
}

impl Client {
    pub fn new(base_url: impl IntoUrl) -> Result<Self, HttpError> {
        Ok(Self {
            base_url: base_url.into_url()?,
            inner: reqwest::Client::new(),
        })
    }

    /// GET 请求:状态码非 200 时直接返回 `HttpError::Status`(压测默认行为)。
    /// 需要读取非 200 响应体时,用 [`Self::get_raw`]。
    pub async fn get(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.get_raw(url).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// GET 请求:原样返回响应(不校验状态码)。
    pub async fn get_raw(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.inner.get(self.base_url.join(url)?).send().await?;
        Ok(resp)
    }
}
