//! OpenAI 兼容协议的厂商实现。

pub(crate) mod request;
pub(crate) mod sse;

use std::{
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::Path,
    sync::{Arc, Mutex},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use async_trait::async_trait;
use futures::{StreamExt, stream};
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
    trace: Option<Arc<Mutex<File>>>,
}

struct TraceCapture {
    id: String,
    request: Value,
    file: Arc<Mutex<File>>,
    started: Instant,
    text: String,
    reasoning: String,
    tool_calls: Vec<Value>,
    usage: Option<Value>,
    finish_reason: Option<String>,
    error: Option<String>,
    finished: bool,
}

impl TraceCapture {
    fn new(request: Value, file: Arc<Mutex<File>>) -> Self {
        static NEXT_ID: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        Self {
            id: format!(
                "{timestamp}-{}",
                NEXT_ID.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
            ),
            request,
            file,
            started: Instant::now(),
            text: String::new(),
            reasoning: String::new(),
            tool_calls: Vec::new(),
            usage: None,
            finish_reason: None,
            error: None,
            finished: false,
        }
    }

    fn observe(&mut self, item: &Result<crate::StreamChunk, ProviderError>) {
        match item {
            Ok(crate::StreamChunk::TextDelta(text)) => self.text.push_str(text),
            Ok(crate::StreamChunk::ReasoningDelta(text)) => self.reasoning.push_str(text),
            Ok(crate::StreamChunk::ToolCall(call)) => self.tool_calls.push(serde_json::json!({
                "id": call.id,
                "name": call.name,
                "arguments": call.arguments,
            })),
            Ok(crate::StreamChunk::Finished { reason, usage }) => {
                self.finish_reason = Some(format!("{reason:?}"));
                self.usage = usage.map(|usage| {
                    serde_json::json!({
                        "input_tokens": usage.input_tokens,
                        "output_tokens": usage.output_tokens,
                        "cache_hit_tokens": usage.cache_hit_tokens,
                        "cache_miss_tokens": usage.cache_miss_tokens,
                    })
                });
            }
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    fn finish(&mut self, status: &str) {
        if self.finished {
            return;
        }
        self.finished = true;
        let messages = self.request["messages"].as_array();
        let mut context_chars = serde_json::Map::new();
        if let Some(messages) = messages {
            for message in messages {
                let role = message["role"].as_str().unwrap_or("unknown");
                let chars = message["content"]
                    .as_str()
                    .unwrap_or_default()
                    .chars()
                    .count();
                let entry = context_chars
                    .entry(role.to_string())
                    .or_insert(serde_json::json!(0));
                *entry = serde_json::json!(entry.as_u64().unwrap_or_default() + chars as u64);
            }
        }
        let request_bytes = serde_json::to_vec(&self.request).map_or(0, |bytes| bytes.len());
        let record = serde_json::json!({
            "id": self.id,
            "status": status,
            "elapsed_ms": self.started.elapsed().as_millis(),
            "request": self.request,
            "request_metrics": {
                "serialized_body_bytes": request_bytes,
                "message_count": messages.map_or(0, |items| items.len()),
                "tool_count": self.request["tools"].as_array().map_or(0, |items| items.len()),
                "context_chars_by_role": context_chars,
                "tool_schema_bytes": serde_json::to_vec(&self.request["tools"])
                    .map_or(0, |bytes| bytes.len()),
            },
            "response": {
                "text": self.text,
                "reasoning": self.reasoning,
                "tool_calls": self.tool_calls,
                "finish_reason": self.finish_reason,
                "error": self.error,
            },
            "response_metrics": {
                "text_chars": self.text.chars().count(),
                "reasoning_chars": self.reasoning.chars().count(),
                "tool_call_count": self.tool_calls.len(),
            },
            "usage": self.usage,
        });
        if let Ok(mut file) = self.file.lock() {
            let _ = serde_json::to_writer(&mut *file, &record);
            let _ = file.write_all(b"\n");
            let _ = file.flush();
        }
    }
}

impl Drop for TraceCapture {
    fn drop(&mut self) {
        self.finish("interrupted");
    }
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
            trace: None,
        }
    }

    pub fn with_trace(mut self, path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref();
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            fs::create_dir_all(parent)?;
        }
        let mut options = OpenOptions::new();
        options.create(true).append(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options.open(path)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
        }
        self.trace = Some(Arc::new(Mutex::new(file)));
        Ok(self)
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
        let mut capture = self
            .trace
            .as_ref()
            .map(|file| TraceCapture::new(body.clone(), Arc::clone(file)));
        let url = format!("{}/chat/completions", self.base_url);
        let response = self
            .send_with_retry(|| self.http.post(&url).bearer_auth(&self.api_key).json(&body))
            .await;
        let resp = match response {
            Ok(response) => response,
            Err(error) => {
                if let Some(capture) = &mut capture {
                    capture.error = Some(error.to_string());
                    capture.finish("request_failed");
                }
                return Err(error);
            }
        };
        let bytes = resp.bytes_stream().map(|r| {
            r.map(|b| b.to_vec())
                .map_err(|e| ProviderError::Network(e.to_string()))
        });
        let inner = sse::decode_stream(bytes, self.quirks.reasoning_field);
        let traced = stream::unfold((inner, capture), |(mut inner, mut capture)| async move {
            match inner.next().await {
                Some(item) => {
                    if let Some(capture) = &mut capture {
                        capture.observe(&item);
                    }
                    Some((item, (inner, capture)))
                }
                None => {
                    if let Some(mut capture) = capture.take() {
                        let status = if capture.error.is_some() {
                            "stream_failed"
                        } else {
                            "complete"
                        };
                        capture.finish(status);
                    }
                    None
                }
            }
        })
        .boxed();
        Ok(traced)
    }
}
