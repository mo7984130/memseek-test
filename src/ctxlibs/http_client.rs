use std::borrow::Cow;

use crate::error::ScenarioError;
use reqwest::{IntoUrl, Response, StatusCode};
use serde::ser::Error as _;

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP request failed: {0}")]
    Reqwest(#[from] reqwest::Error),
    #[error("JSON serialization failed: {0}")]
    Serde(#[from] serde_json::Error),
    #[error("Invalid URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("HTTP status error: {0}")]
    Status(reqwest::StatusCode),
}

impl HttpError {
    /// 构造状态码错误(供场景对非 2xx 响应使用)。
    pub fn status(code: StatusCode) -> Self {
        Self::Status(code)
    }
}

impl ScenarioError for HttpError {
    fn kind(&self) -> Cow<'static, str> {
        match self {
            Self::Reqwest(_) => "reqwest".into(),
            Self::Serde(_) => "serde".into(),
            Self::Url(_) => "url_parse".into(),
            Self::Status(code) => format!("http_status_{}", code.as_u16()).into(),
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

    /// POST 请求:状态码非 200 时返回错误。
    /// `body` 接受任意 `Serialize` 值(如 `json!({...})` 或 `&Struct`)。
    pub async fn post(
        &self,
        url: &str,
        body: impl serde::Serialize,
    ) -> Result<Response, HttpError> {
        let resp = self.post_raw(url, &body).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// POST 请求:原样返回响应(不校验状态码)。
    pub async fn post_raw(
        &self,
        url: &str,
        body: impl serde::Serialize,
    ) -> Result<Response, HttpError> {
        let resp = self
            .inner
            .post(self.base_url.join(url)?)
            .json(&body)
            .send()
            .await?;
        Ok(resp)
    }

    /// POST 请求:发送表单数据。
    pub async fn post_form(
        &self,
        url: &str,
        form: &[(String, String)],
    ) -> Result<Response, HttpError> {
        let resp = self
            .inner
            .post(self.base_url.join(url)?)
            .form(form)
            .send()
            .await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// PUT 请求:状态码非 200 时返回错误。
    /// `body` 接受任意 `Serialize` 值(如 `json!({...})` 或 `&Struct`)。
    pub async fn put(&self, url: &str, body: impl serde::Serialize) -> Result<Response, HttpError> {
        let resp = self.put_raw(url, &body).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// PUT 请求:原样返回响应(不校验状态码)。
    pub async fn put_raw(
        &self,
        url: &str,
        body: impl serde::Serialize,
    ) -> Result<Response, HttpError> {
        let resp = self
            .inner
            .put(self.base_url.join(url)?)
            .json(&body)
            .send()
            .await?;
        Ok(resp)
    }

    /// DELETE 请求:状态码非 200 时返回错误。
    pub async fn delete(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.delete_raw(url).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// DELETE 请求:原样返回响应(不校验状态码)。
    pub async fn delete_raw(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.inner.delete(self.base_url.join(url)?).send().await?;
        Ok(resp)
    }

    /// PATCH 请求:状态码非 200 时返回错误。
    /// `body` 接受任意 `Serialize` 值(如 `json!({...})` 或 `&Struct`)。
    pub async fn patch(
        &self,
        url: &str,
        body: impl serde::Serialize,
    ) -> Result<Response, HttpError> {
        let resp = self.patch_raw(url, &body).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// PATCH 请求:原样返回响应(不校验状态码)。
    pub async fn patch_raw(
        &self,
        url: &str,
        body: impl serde::Serialize,
    ) -> Result<Response, HttpError> {
        let resp = self
            .inner
            .patch(self.base_url.join(url)?)
            .json(&body)
            .send()
            .await?;
        Ok(resp)
    }

    /// OPTIONS 请求:状态码非 200 时返回错误。
    pub async fn options(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.options_raw(url).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// OPTIONS 请求:原样返回响应(不校验状态码)。
    pub async fn options_raw(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self
            .inner
            .request(reqwest::Method::OPTIONS, self.base_url.join(url)?)
            .send()
            .await?;
        Ok(resp)
    }

    /// HEAD 请求:状态码非 200 时返回错误。
    pub async fn head(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.head_raw(url).await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }

    /// HEAD 请求:原样返回响应(不校验状态码)。
    pub async fn head_raw(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.inner.head(self.base_url.join(url)?).send().await?;
        Ok(resp)
    }

    /// 创建一个自定义请求构建器
    pub fn request(&self, method: reqwest::Method, url: &str) -> RequestBuilder<'_> {
        RequestBuilder::new(self, method, url)
    }
}

/// 自定义请求构建器,支持更灵活的配置。
pub struct RequestBuilder<'a> {
    client: &'a Client,
    method: reqwest::Method,
    url: String,
    body: Option<reqwest::Body>,
    headers: reqwest::header::HeaderMap,
    query: Vec<(String, String)>,
}

impl<'a> RequestBuilder<'a> {
    pub fn new(client: &'a Client, method: reqwest::Method, url: &str) -> Self {
        Self {
            client,
            method,
            url: url.to_string(),
            body: None,
            headers: reqwest::header::HeaderMap::new(),
            query: Vec::new(),
        }
    }

    pub fn header(mut self, key: &str, value: &str) -> Self {
        if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes()) {
            if let Ok(header_value) = reqwest::header::HeaderValue::from_str(value) {
                self.headers.insert(header_name, header_value);
            }
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
        self.body = Some(body.into());
        self
    }

    /// 设置表单数据
    pub fn form<B: serde::Serialize>(mut self, form: &B) -> Result<Self, serde_json::Error> {
        let bytes = serde_urlencoded::to_string(form)
            .map_err(|e| serde_json::Error::custom(e.to_string()))?
            .into_bytes();
        self.body = Some(reqwest::Body::from(bytes));
        self.headers.insert(
            reqwest::header::CONTENT_TYPE,
            reqwest::header::HeaderValue::from_static("application/x-www-form-urlencoded"),
        );
        Ok(self)
    }

    pub async fn send(self) -> Result<Response, HttpError> {
        let mut req_builder = self
            .client
            .inner
            .request(self.method, self.client.base_url.join(&self.url)?);

        // 添加 headers
        for (key, value) in self.headers {
            if let Some(key) = key {
                req_builder = req_builder.header(key, value);
            }
        }

        // 添加 query 参数
        if !self.query.is_empty() {
            req_builder = req_builder.query(&self.query);
        }

        // 添加 body
        if let Some(body) = self.body {
            req_builder = req_builder.body(body);
        }

        let resp = req_builder.send().await?;
        Ok(resp)
    }

    /// 发送请求并自动检查状态码
    pub async fn send_checked(self) -> Result<Response, HttpError> {
        let resp = self.send().await?;
        if resp.status() != StatusCode::OK {
            return Err(HttpError::Status(resp.status()));
        }
        Ok(resp)
    }
}
