//! 联网搜索与网页读取的后端。默认使用 AnySearch（支持匿名访问）。

use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use serde_json::{Value, json};

use crate::config::{Config, WebConfig};

pub const ANYSEARCH_DEFAULT_BASE: &str = "https://api.anysearch.com";
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
const ERROR_BODY_CHARS: usize = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebQuery {
    pub query: String,
    pub max_results: u8,
    pub language: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebResult {
    pub title: String,
    pub url: String,
    pub snippet: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WebPage {
    pub title: Option<String>,
    pub url: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WebError {
    #[error("联网请求失败：{0}")]
    Network(String),
    #[error("搜索服务返回错误：{0}")]
    Api(String),
    #[error("搜索服务响应无法解析：{0}")]
    Decode(String),
    #[error("{0}")]
    Unsupported(String),
}

#[async_trait]
pub trait WebBackend: Send + Sync {
    async fn search(&self, query: &WebQuery) -> Result<Vec<WebResult>, WebError>;
    async fn fetch(&self, url: &str) -> Result<WebPage, WebError>;
}

/// 解析 base 与 key：非空的环境变量优先，其次配置文件，最后默认值 / 匿名。
pub fn resolve_web_settings(
    cfg: &WebConfig,
    env_key: Option<String>,
    env_base: Option<String>,
) -> (String, Option<String>) {
    let non_empty = |v: Option<String>| v.filter(|s| !s.trim().is_empty());
    let base = non_empty(env_base)
        .or_else(|| non_empty(cfg.base_url.clone()))
        .unwrap_or_else(|| ANYSEARCH_DEFAULT_BASE.to_string());
    let key = non_empty(env_key).or_else(|| non_empty(cfg.api_key.clone()));
    (base, key)
}

pub fn backend_from_config(cfg: &Config) -> Arc<dyn WebBackend> {
    let web = &cfg.tools.web;
    match web.provider.as_str() {
        "" | "anysearch" => {
            let (base, key) = resolve_web_settings(
                web,
                std::env::var("ANYSEARCH_API_KEY").ok(),
                std::env::var("ANYSEARCH_API_BASE_URL").ok(),
            );
            Arc::new(AnySearch::new(base, key))
        }
        other => Arc::new(Unsupported(other.to_string())),
    }
}

/// 配置了不支持的联网服务：调用时返回明确的错误。
struct Unsupported(String);

#[async_trait]
impl WebBackend for Unsupported {
    async fn search(&self, _query: &WebQuery) -> Result<Vec<WebResult>, WebError> {
        Err(self.error())
    }
    async fn fetch(&self, _url: &str) -> Result<WebPage, WebError> {
        Err(self.error())
    }
}

impl Unsupported {
    fn error(&self) -> WebError {
        WebError::Unsupported(format!(
            "不支持的联网服务：{}，目前仅支持 anysearch",
            self.0
        ))
    }
}

impl AnySearch {
    pub fn new(base: impl Into<String>, api_key: Option<String>) -> Self {
        let http = reqwest::Client::builder()
            .timeout(REQUEST_TIMEOUT)
            .build()
            .expect("构建 HTTP 客户端失败");
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
            api_key: api_key.filter(|k| !k.trim().is_empty()),
        }
    }

    async fn post(&self, path: &str, body: Value) -> Result<Value, WebError> {
        let mut req = self
            .http
            .post(format!("{}{path}", self.base))
            .header(
                "X-Anysearch-Client",
                concat!("seanbot/", env!("CARGO_PKG_VERSION")),
            )
            .json(&body);
        if let Some(key) = &self.api_key {
            req = req.bearer_auth(key);
        }
        let resp = req
            .send()
            .await
            .map_err(|e| WebError::Network(e.to_string()))?;
        let status = resp.status().as_u16();
        let text = resp
            .text()
            .await
            .map_err(|e| WebError::Network(e.to_string()))?;
        parse_envelope(status, &text)
    }
}

/// 解析 AnySearch 的响应信封 `{code, message, request_id, data}`，返回 `data`。
fn parse_envelope(status: u16, text: &str) -> Result<Value, WebError> {
    let body: Value = serde_json::from_str(text).map_err(|_| {
        WebError::Decode(format!(
            "HTTP {status}：{}",
            clip(text.trim(), ERROR_BODY_CHARS)
        ))
    })?;
    let code = body.get("code").and_then(Value::as_i64).unwrap_or(0);
    if status >= 400 || code != 0 {
        let message = body
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("未知错误");
        let request_id = body
            .get("request_id")
            .and_then(Value::as_str)
            .filter(|r| !r.is_empty())
            .map(|r| format!("（request_id: {r}）"))
            .unwrap_or_default();
        return Err(WebError::Api(format!("{message}{request_id}")));
    }
    Ok(body.get("data").cloned().unwrap_or(Value::Null))
}

fn str_field<'a>(v: &'a Value, key: &str) -> Option<&'a str> {
    v.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.trim().is_empty())
}

fn clip(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let mut out: String = s.chars().take(max).collect();
    out.push('…');
    out
}

#[async_trait]
impl WebBackend for AnySearch {
    async fn search(&self, query: &WebQuery) -> Result<Vec<WebResult>, WebError> {
        let mut body = json!({"query": query.query, "max_results": query.max_results});
        if let Some(lang) = &query.language {
            body["language"] = json!(lang);
        }
        let data = self.post("/v1/search", body).await?;
        let results = data
            .get("results")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        Ok(results
            .iter()
            .map(|r| WebResult {
                title: str_field(r, "title").unwrap_or("（无标题）").to_string(),
                url: str_field(r, "url").unwrap_or_default().to_string(),
                snippet: str_field(r, "content")
                    .or_else(|| str_field(r, "snippet"))
                    .unwrap_or_default()
                    .to_string(),
            })
            .collect())
    }

    async fn fetch(&self, url: &str) -> Result<WebPage, WebError> {
        let data = self.post("/v1/extract", json!({"url": url})).await?;
        Ok(WebPage {
            title: str_field(&data, "title").map(String::from),
            url: str_field(&data, "url").unwrap_or(url).to_string(),
            content: data
                .get("content")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string(),
        })
    }
}

pub struct AnySearch {
    http: reqwest::Client,
    base: String,
    api_key: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn web(key: Option<&str>, base: Option<&str>) -> WebConfig {
        WebConfig {
            provider: "anysearch".into(),
            api_key: key.map(String::from),
            base_url: base.map(String::from),
            ..WebConfig::default()
        }
    }

    #[test]
    fn settings_precedence() {
        assert_eq!(
            resolve_web_settings(&web(None, None), None, None),
            (ANYSEARCH_DEFAULT_BASE.to_string(), None)
        );
        assert_eq!(
            resolve_web_settings(&web(Some("cfg"), Some("http://cfg")), None, None),
            ("http://cfg".to_string(), Some("cfg".to_string()))
        );
        assert_eq!(
            resolve_web_settings(
                &web(Some("cfg"), Some("http://cfg")),
                Some("env".into()),
                Some("http://env".into())
            ),
            ("http://env".to_string(), Some("env".to_string()))
        );
        assert_eq!(
            resolve_web_settings(&web(Some(" "), None), Some("".into()), Some(" ".into())),
            (ANYSEARCH_DEFAULT_BASE.to_string(), None)
        );
    }

    #[tokio::test]
    async fn unsupported_provider_reports_error() {
        let mut cfg = Config::default();
        cfg.tools.web.provider = "bing".into();
        let backend = backend_from_config(&cfg);
        let q = WebQuery {
            query: "x".into(),
            max_results: 1,
            language: None,
        };
        assert!(
            matches!(backend.search(&q).await, Err(WebError::Unsupported(m)) if m.contains("bing"))
        );
    }
}
