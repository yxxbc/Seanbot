//! OpenAI 兼容协议的厂商实现。

pub(crate) mod request;
pub(crate) mod sse;

use std::time::Duration;

use async_trait::async_trait;
use futures::StreamExt;
use serde_json::Value;

use crate::{
    ChatRequest, ChunkStream, ModelInfo, Provider, ProviderError, ProviderInfo,
    descriptor::{ProviderDescriptor, Quirks, model_info},
};

/// `Retry-After` 最长遵循 60 秒。
const MAX_RETRY_AFTER: Duration = Duration::from_secs(60);

pub struct OpenAiCompat {
    info: ProviderInfo,
    base_url: String,
    api_key: String,
    quirks: Quirks,
    http: reqwest::Client,
    backoff_base: Duration,
    max_retries: u32,
}

impl OpenAiCompat {
    pub fn new(desc: &ProviderDescriptor, api_key: impl Into<String>) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(30))
            .build()
            .expect("构建 HTTP 客户端失败");
        Self {
            info: ProviderInfo {
                id: desc.id.to_string(),
                display_name: desc.display_name.to_string(),
            },
            base_url: desc.base_url.trim_end_matches('/').to_string(),
            api_key: api_key.into(),
            quirks: desc.quirks,
            http,
            backoff_base: Duration::from_secs(1),
            max_retries: 3,
        }
    }

    pub fn with_base_url(mut self, url: impl Into<String>) -> Self {
        self.base_url = url.into().trim_end_matches('/').to_string();
        self
    }

    /// 退避基数（第 n 次重试等待 base × 2^n）。测试中设为毫秒级。
    pub fn with_backoff_base(mut self, base: Duration) -> Self {
        self.backoff_base = base;
        self
    }

    /// 发送请求；仅在拿到 2xx 响应之前（即尚未收到任何分片）对可重试错误重试。
    async fn send_with_retry<F>(&self, build: F) -> Result<reqwest::Response, ProviderError>
    where
        F: Fn() -> reqwest::RequestBuilder + Send + Sync,
    {
        let mut attempt: u32 = 0;
        loop {
            let err = match build().send().await {
                Ok(resp) if resp.status().is_success() => return Ok(resp),
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    let retry_after = retry_after(&resp);
                    let body = resp.text().await.unwrap_or_default();
                    ProviderError::from_status(status, body, retry_after)
                }
                Err(e) => ProviderError::Network(e.to_string()),
            };
            if !err.is_retryable() || attempt >= self.max_retries {
                return Err(err);
            }
            let delay = match &err {
                ProviderError::RateLimited {
                    retry_after: Some(d),
                } => (*d).min(MAX_RETRY_AFTER),
                _ => self.backoff_base * 2u32.pow(attempt),
            };
            tokio::time::sleep(delay).await;
            attempt += 1;
        }
    }
}

fn retry_after(resp: &reqwest::Response) -> Option<Duration> {
    resp.headers()
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

pub(crate) fn parse_models(v: &Value) -> Result<Vec<ModelInfo>, ProviderError> {
    let data = v
        .get("data")
        .and_then(Value::as_array)
        .ok_or_else(|| ProviderError::Decode("模型列表缺少 data 字段".into()))?;
    let mut models: Vec<ModelInfo> = data
        .iter()
        .filter_map(|m| {
            let id = m.get("id")?.as_str()?;
            Some(model_info(
                id,
                m.get("context_window").and_then(Value::as_u64),
            ))
        })
        .collect();
    models.sort_by(|a, b| a.id.cmp(&b.id));
    Ok(models)
}

#[async_trait]
impl Provider for OpenAiCompat {
    fn info(&self) -> &ProviderInfo {
        &self.info
    }

    async fn list_models(&self) -> Result<Vec<ModelInfo>, ProviderError> {
        let url = format!("{}/models", self.base_url);
        let resp = self
            .send_with_retry(|| self.http.get(&url).bearer_auth(&self.api_key))
            .await?;
        let v: Value = resp
            .json()
            .await
            .map_err(|e| ProviderError::Decode(e.to_string()))?;
        parse_models(&v)
    }

    async fn stream(&self, req: ChatRequest) -> Result<ChunkStream, ProviderError> {
        let body = request::build_body(&req, &self.quirks);
        let url = format!("{}/chat/completions", self.base_url);
        let resp = self
            .send_with_retry(|| self.http.post(&url).bearer_auth(&self.api_key).json(&body))
            .await?;
        let bytes = resp.bytes_stream().map(|r| {
            r.map(|b| b.to_vec())
                .map_err(|e| ProviderError::Network(e.to_string()))
        });
        Ok(sse::decode_stream(bytes, self.quirks.reasoning_field))
    }
}
