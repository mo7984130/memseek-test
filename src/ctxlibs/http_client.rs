use crate::error::ScenarioError;
use reqwest::{IntoUrl, Response};

#[derive(Debug, thiserror::Error)]
pub enum HttpError {
    #[error("HTTP request failed {0}")]
    Http(#[from] reqwest::Error),
    #[error("Parse url failed {0}")]
    UrlParse(#[from] url::ParseError),
}
impl ScenarioError for HttpError {
    fn kind(&self) -> &'static str {
        match self {
            Self::Http(_) => "http",
            Self::UrlParse(_) => "url_parse",
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

    pub async fn get(&self, url: &str) -> Result<Response, HttpError> {
        let resp = self.inner.get(self.base_url.join(url)?).send().await?;
        Ok(resp)
    }
}
