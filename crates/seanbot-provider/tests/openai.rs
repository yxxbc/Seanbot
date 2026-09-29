use std::time::Duration;

use futures::StreamExt;
use seanbot_provider::{
    ChatRequest, DEEPSEEK, FinishReason, Message, OpenAiCompat, Provider, ProviderError,
    StreamChunk, ToolCall,
};
use serde_json::{Value, json};
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_partial_json, header, method, path},
};

fn provider(server: &MockServer) -> OpenAiCompat {
    OpenAiCompat::new(&DEEPSEEK, "sk-test")
        .with_base_url(server.uri())
        .with_backoff_base(Duration::from_millis(1))
}

fn sse(events: &[Value]) -> String {
    let mut s = String::new();
    for e in events {
        s.push_str(&format!("data: {e}\n\n"));
    }
    s.push_str("data: [DONE]\n\n");
    s
}

fn sse_response(body: String) -> ResponseTemplate {
    ResponseTemplate::new(200)
        .insert_header("content-type", "text/event-stream")
        .set_body_string(body)
}

fn request() -> ChatRequest {
    ChatRequest {
        model: "deepseek-flash".into(),
        system: "sys".into(),
        messages: vec![Message::user("hi")],
        tools: vec![],
        max_tokens: None,
    }
}

async fn collect(
    p: &OpenAiCompat,
) -> Result<Vec<Result<StreamChunk, ProviderError>>, ProviderError> {
    Ok(p.stream(request()).await?.collect().await)
}

#[tokio::test]
async fn list_models_parses_and_authenticates() {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/models"))
        .and(header("authorization", "Bearer sk-test"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "object": "list",
            "data": [
                {"id": "deepseek-v4-pro", "object": "model", "owned_by": "deepseek"},
                {"id": "deepseek-flash", "object": "model", "owned_by": "deepseek", "context_window": 1000000}
            ]
        })))
        .expect(1)
        .mount(&server)
        .await;
    let models = provider(&server).list_models().await.unwrap();
    let ids: Vec<_> = models.iter().map(|m| m.id.as_str()).collect();
    assert_eq!(ids, ["deepseek-flash", "deepseek-v4-pro"]); // 按 id 排序
    assert_eq!(models[0].context_window, 1_000_000);
}

#[tokio::test]
async fn streams_text_reasoning_tool_calls_and_usage() {
    let server = MockServer::start().await;
    let body = sse(&[
        json!({"choices":[{"index":0,"delta":{"reasoning_content":"想"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"content":"好"},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\""}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":":\"a\"}"}}]},"finish_reason":null}]}),
        json!({"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":10,"completion_tokens":2,"prompt_cache_hit_tokens":8,"prompt_cache_miss_tokens":2}}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(body_partial_json(
            json!({"model":"deepseek-flash","stream":true,"stream_options":{"include_usage":true}}),
        ))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server)).await.unwrap();
    let chunks: Vec<StreamChunk> = chunks.into_iter().map(Result::unwrap).collect();
    assert_eq!(chunks[0], StreamChunk::ReasoningDelta("想".into()));
    assert_eq!(chunks[1], StreamChunk::TextDelta("好".into()));
    assert_eq!(
        chunks[2],
        StreamChunk::ToolCall(ToolCall {
            id: "c1".into(),
            name: "read".into(),
            arguments: r#"{"path":"a"}"#.into()
        })
    );
    match &chunks[3] {
        StreamChunk::Finished {
            reason,
            usage: Some(u),
        } => {
            assert_eq!(*reason, FinishReason::ToolCalls);
            assert_eq!(u.cache_hit_tokens, Some(8));
        }
        other => panic!("意外分片：{other:?}"),
    }
}

#[tokio::test]
async fn trace_records_wire_request_response_and_usage_without_auth() {
    let server = MockServer::start().await;
    let trace_dir = tempfile::tempdir().unwrap();
    let trace_path = trace_dir.path().join("nested/trace.jsonl");
    let body = sse(&[
        json!({"choices":[{"index":0,"delta":{"content":"answer"},"finish_reason":"stop"}]}),
        json!({"choices":[],"usage":{"prompt_tokens":12,"completion_tokens":3,"prompt_cache_hit_tokens":8,"prompt_cache_miss_tokens":4}}),
    ]);
    Mock::given(method("POST"))
        .and(path("/chat/completions"))
        .and(header("authorization", "Bearer sk-test"))
        .and(body_partial_json(json!({"model":"deepseek-flash","messages":[{"role":"system","content":"sys"},{"role":"user","content":"hi"}]})))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;

    let traced = provider(&server).with_trace(&trace_path).unwrap();
    collect(&traced).await.unwrap();

    let line = std::fs::read_to_string(&trace_path).unwrap();
    let record: Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(record["status"], "complete");
    assert_eq!(record["request"]["messages"][1]["content"], "hi");
    assert_eq!(record["response"]["text"], "answer");
    assert_eq!(record["usage"]["input_tokens"], 12);
    assert_eq!(record["usage"]["output_tokens"], 3);
    assert_eq!(record["usage"]["cache_hit_tokens"], 8);
    assert_eq!(
        record["request_metrics"]["context_chars_by_role"]["user"],
        2
    );
    assert!(!line.contains("sk-test"));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&trace_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

#[tokio::test]
async fn auth_error_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(401).set_body_string("bad key"))
        .expect(1)
        .mount(&server)
        .await;
    let err = collect(&provider(&server)).await.unwrap_err();
    assert!(matches!(err, ProviderError::Auth { status: 401, .. }));
}

#[tokio::test]
async fn bad_request_is_not_retried() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(400).set_body_string("context too long"))
        .expect(1)
        .mount(&server)
        .await;
    let err = collect(&provider(&server)).await.unwrap_err();
    assert!(matches!(err, ProviderError::BadRequest(m) if m.contains("context too long")));
}

#[tokio::test]
async fn server_errors_retry_then_succeed() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(503))
        .up_to_n_times(2)
        .expect(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_response(sse(&[
            json!({"choices":[{"index":0,"delta":{"content":"ok"},"finish_reason":"stop"}]}),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server)).await.unwrap();
    assert_eq!(chunks[0], Ok(StreamChunk::TextDelta("ok".into())));
}

#[tokio::test]
async fn gives_up_after_three_retries() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(500).set_body_string("down"))
        .expect(4) // 1 次原始请求 + 3 次重试
        .mount(&server)
        .await;
    let err = collect(&provider(&server)).await.unwrap_err();
    assert!(matches!(err, ProviderError::Server { status: 500, .. }));
}

#[tokio::test]
async fn rate_limit_honors_retry_after() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .expect(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .respond_with(sse_response(sse(&[
            json!({"choices":[{"index":0,"delta":{},"finish_reason":"stop"}]}),
        ])))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server)).await.unwrap();
    assert!(matches!(
        chunks.last(),
        Some(Ok(StreamChunk::Finished { .. }))
    ));
}

#[tokio::test]
async fn mid_stream_failure_is_not_retried() {
    let server = MockServer::start().await;
    // 没有 finish_reason 也没有 [DONE]：连接中途断开
    let body = format!(
        "data: {}\n\n",
        json!({"choices":[{"index":0,"delta":{"content":"半"},"finish_reason":null}]})
    );
    Mock::given(method("POST"))
        .respond_with(sse_response(body))
        .expect(1)
        .mount(&server)
        .await;
    let chunks = collect(&provider(&server)).await.unwrap();
    assert_eq!(chunks[0], Ok(StreamChunk::TextDelta("半".into())));
    assert!(matches!(chunks[1], Err(ProviderError::Network(_))));
}

#[tokio::test]
async fn stalled_response_hits_read_timeout() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(sse_response(sse(&[])).set_delay(Duration::from_millis(800)))
        .mount(&server)
        .await;
    let p = provider(&server).with_read_timeout(Duration::from_millis(100));
    let started = std::time::Instant::now();
    let err = collect(&p).await.unwrap_err();
    assert!(matches!(err, ProviderError::Network(_)), "{err:?}");
    // 4 次尝试 × 100ms，远小于服务端的 800ms 延迟
    assert!(
        started.elapsed() < Duration::from_millis(700),
        "{:?}",
        started.elapsed()
    );
}
