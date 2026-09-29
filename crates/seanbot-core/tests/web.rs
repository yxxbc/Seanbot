use seanbot_core::web::{AnySearch, WebBackend, WebError, WebQuery};
use serde_json::json;
use wiremock::{
    Mock, MockServer, ResponseTemplate,
    matchers::{body_json, header, method, path},
};

fn query(q: &str) -> WebQuery {
    WebQuery {
        query: q.into(),
        max_results: 3,
        language: Some("zh-CN".into()),
    }
}

#[tokio::test]
async fn search_parses_results_and_sends_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .and(header("authorization", "Bearer as_sk_test"))
        .and(header("x-anysearch-client", concat!("seanbot/", env!("CARGO_PKG_VERSION"))))
        .and(body_json(json!({"query": "ratatui", "max_results": 3, "language": "zh-CN"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "message": "success", "request_id": "r1",
            "data": {"results": [
                {"title": "v0.29.0", "url": "https://ratatui.rs/highlights/v029/", "snippet": "短摘要", "content": "完整内容"},
                {"title": "", "url": "https://github.com/ratatui/ratatui/releases", "snippet": "只有摘要", "content": ""}
            ]}
        })))
        .expect(1)
        .mount(&server)
        .await;
    let backend = AnySearch::new(server.uri(), Some("as_sk_test".into()));
    let results = backend.search(&query("ratatui")).await.unwrap();
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].title, "v0.29.0");
    assert_eq!(results[0].snippet, "完整内容");
    assert_eq!(results[1].title, "（无标题）");
    assert_eq!(results[1].snippet, "只有摘要");
}

#[tokio::test]
async fn anonymous_search_sends_no_authorization() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code": 0, "data": {"results": []}})),
        )
        .mount(&server)
        .await;
    for key in [None, Some("  ".to_string())] {
        let backend = AnySearch::new(server.uri(), key);
        assert!(backend.search(&query("x")).await.unwrap().is_empty());
    }
    for req in server.received_requests().await.unwrap() {
        assert!(req.headers.get("authorization").is_none());
    }
}

#[tokio::test]
async fn api_errors_carry_message_and_request_id() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/search"))
        .respond_with(ResponseTemplate::new(429).set_body_json(json!({
            "code": -1, "message": "Rate limited, retry after 30 seconds.", "request_id": "rid-9"
        })))
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .respond_with(
            ResponseTemplate::new(200).set_body_json(json!({"code": -1, "message": "Invalid URL"})),
        )
        .mount(&server)
        .await;
    let backend = AnySearch::new(server.uri(), None);
    let err = backend.search(&query("x")).await.unwrap_err();
    assert!(
        matches!(&err, WebError::Api(m) if m.contains("Rate limited") && m.contains("rid-9")),
        "{err}"
    );
    let err = backend.fetch("https://example.com").await.unwrap_err();
    assert_eq!(err, WebError::Api("Invalid URL".into()));
}

#[tokio::test]
async fn html_error_page_is_decode_error() {
    let server = MockServer::start().await;
    let html = format!("<html><body>{}</body></html>", "x".repeat(1000));
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(502).set_body_string(html))
        .mount(&server)
        .await;
    let err = AnySearch::new(server.uri(), None)
        .search(&query("x"))
        .await
        .unwrap_err();
    match err {
        WebError::Decode(m) => {
            assert!(m.contains("HTTP 502"), "{m}");
            assert!(
                m.chars().count() < 400,
                "错误信息应截断：{}",
                m.chars().count()
            );
        }
        other => panic!("意外错误：{other:?}"),
    }
}

#[tokio::test]
async fn fetch_parses_page() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/v1/extract"))
        .and(body_json(json!({"url": "https://example.com/a"})))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "code": 0, "data": {"title": "标题", "url": "https://example.com/a", "content": "正文"}
        })))
        .mount(&server)
        .await;
    let page = AnySearch::new(server.uri(), None)
        .fetch("https://example.com/a")
        .await
        .unwrap();
    assert_eq!(page.title.as_deref(), Some("标题"));
    assert_eq!(page.url, "https://example.com/a");
    assert_eq!(page.content, "正文");
}

#[tokio::test]
async fn unreachable_service_is_network_error() {
    let err = AnySearch::new("http://127.0.0.1:9", None)
        .search(&query("x"))
        .await
        .unwrap_err();
    assert!(matches!(err, WebError::Network(_)), "{err:?}");
}
