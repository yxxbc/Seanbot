//! OpenAI 兼容 SSE 流解析：字节 → data 行 → StreamChunk。

use std::{collections::BTreeMap, collections::VecDeque, pin::Pin};

use futures::{Stream, StreamExt};
use serde_json::Value;

use crate::{ChunkStream, FinishReason, ProviderError, StreamChunk, ToolCall, Usage};

/// 把任意切分的字节流还原成 `data:` 负载。只在完整行上解码，UTF-8 多字节字符被切开也不受影响。
#[derive(Debug, Default)]
pub(crate) struct SseDecoder {
    buf: Vec<u8>,
}

#[derive(Debug, Default)]
struct PartialCall {
    id: String,
    name: String,
    arguments: String,
}

/// 把 data 负载组装成 StreamChunk；工具调用按 index 累积，在 finish_reason 出现时整体发出。
#[derive(Debug)]
pub(crate) struct Assembler {
    reasoning_field: &'static str,
    calls: BTreeMap<u64, PartialCall>,
    finish: Option<FinishReason>,
    usage: Option<Usage>,
}

impl SseDecoder {
    pub(crate) fn push(&mut self, bytes: &[u8]) -> Vec<String> {
        self.buf.extend_from_slice(bytes);
        let mut out = Vec::new();
        while let Some(pos) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=pos).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if let Some(data) = line.strip_prefix("data:") {
                out.push(data.trim_start().to_string());
            }
        }
        out
    }
}

impl Assembler {
    pub(crate) fn new(reasoning_field: &'static str) -> Self {
        Self {
            reasoning_field,
            calls: BTreeMap::new(),
            finish: None,
            usage: None,
        }
    }

    pub(crate) fn feed(&mut self, data: &str) -> Result<Vec<StreamChunk>, ProviderError> {
        let v: Value = serde_json::from_str(data)
            .map_err(|e| ProviderError::Decode(format!("{e}：{data}")))?;
        if let Some(err) = v.get("error").filter(|e| !e.is_null()) {
            return Err(ProviderError::Server {
                status: 200,
                body: err.to_string(),
            });
        }
        if let Some(u) = v.get("usage").filter(|u| !u.is_null()) {
            self.usage = Some(parse_usage(u));
        }
        let mut out = Vec::new();
        let Some(choice) = v.get("choices").and_then(|c| c.get(0)) else {
            return Ok(out);
        };
        let delta = &choice["delta"];
        if let Some(r) = delta.get(self.reasoning_field).and_then(Value::as_str) {
            if !r.is_empty() {
                out.push(StreamChunk::ReasoningDelta(r.to_string()));
            }
        }
        if let Some(t) = delta.get("content").and_then(Value::as_str) {
            if !t.is_empty() {
                out.push(StreamChunk::TextDelta(t.to_string()));
            }
        }
        if let Some(calls) = delta.get("tool_calls").and_then(Value::as_array) {
            for c in calls {
                let index = c.get("index").and_then(Value::as_u64).unwrap_or(0);
                let entry = self.calls.entry(index).or_default();
                if let Some(id) = c.get("id").and_then(Value::as_str) {
                    if !id.is_empty() {
                        entry.id = id.to_string();
                    }
                }
                if let Some(name) = c["function"].get("name").and_then(Value::as_str) {
                    entry.name.push_str(name);
                }
                if let Some(args) = c["function"].get("arguments").and_then(Value::as_str) {
                    entry.arguments.push_str(args);
                }
            }
        }
        if let Some(reason) = choice.get("finish_reason").and_then(Value::as_str) {
            self.finish = Some(FinishReason::parse(reason));
            out.extend(self.drain_calls());
        }
        Ok(out)
    }

    fn drain_calls(&mut self) -> Vec<StreamChunk> {
        std::mem::take(&mut self.calls)
            .into_values()
            .map(|c| {
                StreamChunk::ToolCall(ToolCall {
                    id: c.id,
                    name: c.name,
                    arguments: c.arguments,
                })
            })
            .collect()
    }

    /// 收到 `[DONE]`：发出残留的工具调用与 Finished。
    pub(crate) fn on_done(&mut self) -> Vec<StreamChunk> {
        let mut out = self.drain_calls();
        out.push(StreamChunk::Finished {
            reason: self.finish.take().unwrap_or(FinishReason::Stop),
            usage: self.usage.take(),
        });
        out
    }

    /// 字节流结束但没有 `[DONE]`：见过 finish_reason 视为正常结束，否则是连接中断。
    pub(crate) fn on_eof(&mut self) -> Result<Vec<StreamChunk>, ProviderError> {
        if self.finish.is_some() {
            Ok(self.on_done())
        } else {
            Err(ProviderError::Network("响应流意外结束".into()))
        }
    }
}

fn parse_usage(u: &Value) -> Usage {
    let field = |name: &str| {
        u.get(name).and_then(Value::as_u64).or_else(|| {
            u.get("prompt_tokens_details")
                .and_then(|d| d.get(name))
                .and_then(Value::as_u64)
        })
    };
    Usage {
        input_tokens: u.get("prompt_tokens").and_then(Value::as_u64).unwrap_or(0),
        output_tokens: u
            .get("completion_tokens")
            .and_then(Value::as_u64)
            .unwrap_or(0),
        cache_hit_tokens: field("prompt_cache_hit_tokens"),
        cache_miss_tokens: field("prompt_cache_miss_tokens"),
    }
}

struct DecodeState {
    bytes: Pin<Box<dyn Stream<Item = Result<Vec<u8>, ProviderError>> + Send>>,
    decoder: SseDecoder,
    asm: Assembler,
    pending: VecDeque<Result<StreamChunk, ProviderError>>,
    done: bool,
}

pub(crate) fn decode_stream<S>(bytes: S, reasoning_field: &'static str) -> ChunkStream
where
    S: Stream<Item = Result<Vec<u8>, ProviderError>> + Send + 'static,
{
    let state = DecodeState {
        bytes: Box::pin(bytes),
        decoder: SseDecoder::default(),
        asm: Assembler::new(reasoning_field),
        pending: VecDeque::new(),
        done: false,
    };
    futures::stream::unfold(state, |mut st| async move {
        loop {
            if let Some(item) = st.pending.pop_front() {
                return Some((item, st));
            }
            if st.done {
                return None;
            }
            match st.bytes.next().await {
                Some(Ok(buf)) => {
                    for data in st.decoder.push(&buf) {
                        if data == "[DONE]" {
                            st.pending.extend(st.asm.on_done().into_iter().map(Ok));
                            st.done = true;
                            break;
                        }
                        match st.asm.feed(&data) {
                            Ok(chunks) => st.pending.extend(chunks.into_iter().map(Ok)),
                            Err(e) => {
                                st.pending.push_back(Err(e));
                                st.done = true;
                                break;
                            }
                        }
                    }
                }
                Some(Err(e)) => {
                    st.pending.push_back(Err(e));
                    st.done = true;
                }
                None => {
                    match st.asm.on_eof() {
                        Ok(chunks) => st.pending.extend(chunks.into_iter().map(Ok)),
                        Err(e) => st.pending.push_back(Err(e)),
                    }
                    st.done = true;
                }
            }
        }
    })
    .boxed()
}
#[cfg(test)]
mod tests {
    use super::*;

    fn chunk(delta: Value, finish: Option<&str>) -> String {
        serde_json::json!({ "choices": [{ "index": 0, "delta": delta, "finish_reason": finish }] })
            .to_string()
    }

    #[test]
    fn decoder_handles_split_lines_crlf_and_comments() {
        let mut d = SseDecoder::default();
        assert!(d.push(b"data: {\"a\"").is_empty());
        assert_eq!(
            d.push(b":1}\r\n\r\n: keep-alive\n"),
            vec![r#"{"a":1}"#.to_string()]
        );
        assert_eq!(
            d.push(b"event: x\ndata:[DONE]\n"),
            vec!["[DONE]".to_string()]
        );
    }

    #[test]
    fn decoder_survives_utf8_split() {
        let mut d = SseDecoder::default();
        let bytes = "data: 你好\n".as_bytes();
        assert!(d.push(&bytes[..7]).is_empty()); // 切在“你”的中间
        assert_eq!(d.push(&bytes[7..]), vec!["你好".to_string()]);
    }

    #[test]
    fn text_and_reasoning_deltas() {
        let mut a = Assembler::new("reasoning_content");
        let out = a
            .feed(&chunk(serde_json::json!({"reasoning_content":"想"}), None))
            .unwrap();
        assert_eq!(out, vec![StreamChunk::ReasoningDelta("想".into())]);
        let out = a
            .feed(&chunk(serde_json::json!({"content":"答"}), None))
            .unwrap();
        assert_eq!(out, vec![StreamChunk::TextDelta("答".into())]);
        // 空字符串与 null 不产生分片
        let out = a
            .feed(&chunk(
                serde_json::json!({"content":"","reasoning_content":null}),
                None,
            ))
            .unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn assembles_fragmented_tool_calls_in_index_order() {
        let mut a = Assembler::new("reasoning_content");
        let frags = [
            serde_json::json!({"tool_calls":[{"index":0,"id":"c0","type":"function","function":{"name":"read","arguments":""}}]}),
            serde_json::json!({"tool_calls":[{"index":0,"function":{"arguments":"{\"pa"}}]}),
            serde_json::json!({"tool_calls":[{"index":1,"id":"c1","function":{"name":"bash","arguments":"{\"command\":\"ls\"}"}}]}),
            serde_json::json!({"tool_calls":[{"index":0,"function":{"arguments":"th\":\"a\"}"}}]}),
        ];
        for f in frags {
            assert!(a.feed(&chunk(f, None)).unwrap().is_empty());
        }
        let out = a
            .feed(&chunk(serde_json::json!({}), Some("tool_calls")))
            .unwrap();
        assert_eq!(
            out,
            vec![
                StreamChunk::ToolCall(ToolCall {
                    id: "c0".into(),
                    name: "read".into(),
                    arguments: r#"{"path":"a"}"#.into()
                }),
                StreamChunk::ToolCall(ToolCall {
                    id: "c1".into(),
                    name: "bash".into(),
                    arguments: r#"{"command":"ls"}"#.into()
                }),
            ]
        );
        assert_eq!(
            a.on_done(),
            vec![StreamChunk::Finished {
                reason: FinishReason::ToolCalls,
                usage: None
            }]
        );
    }

    #[test]
    fn invalid_arguments_pass_through_raw() {
        let mut a = Assembler::new("reasoning_content");
        a.feed(&chunk(serde_json::json!({"tool_calls":[{"index":0,"id":"c","function":{"name":"read","arguments":"{oops"}}]}), None)).unwrap();
        let out = a
            .feed(&chunk(serde_json::json!({}), Some("tool_calls")))
            .unwrap();
        assert!(matches!(&out[0], StreamChunk::ToolCall(c) if c.arguments == "{oops"));
    }

    #[test]
    fn usage_top_level_cache_fields() {
        let mut a = Assembler::new("reasoning_content");
        a.feed(&chunk(serde_json::json!({"content":"x"}), Some("stop")))
            .unwrap();
        a.feed(r#"{"choices":[],"usage":{"prompt_tokens":100,"completion_tokens":7,"prompt_cache_hit_tokens":90,"prompt_cache_miss_tokens":10}}"#).unwrap();
        assert_eq!(
            a.on_done(),
            vec![StreamChunk::Finished {
                reason: FinishReason::Stop,
                usage: Some(Usage {
                    input_tokens: 100,
                    output_tokens: 7,
                    cache_hit_tokens: Some(90),
                    cache_miss_tokens: Some(10)
                }),
            }]
        );
    }

    #[test]
    fn usage_nested_cache_fields() {
        let mut a = Assembler::new("reasoning_content");
        a.feed(&chunk(serde_json::json!({}), Some("stop"))).unwrap();
        a.feed(r#"{"choices":[],"usage":{"prompt_tokens":50,"completion_tokens":5,"prompt_tokens_details":{"prompt_cache_hit_tokens":40,"prompt_cache_miss_tokens":10}}}"#).unwrap();
        let out = a.on_done();
        assert!(
            matches!(&out[0], StreamChunk::Finished { usage: Some(u), .. } if u.cache_hit_tokens == Some(40) && u.cache_miss_tokens == Some(10))
        );
    }

    #[test]
    fn eof_without_finish_is_network_error() {
        let mut a = Assembler::new("reasoning_content");
        a.feed(&chunk(serde_json::json!({"content":"半"}), None))
            .unwrap();
        assert!(matches!(a.on_eof(), Err(ProviderError::Network(_))));
    }

    #[test]
    fn eof_after_finish_is_ok() {
        let mut a = Assembler::new("reasoning_content");
        a.feed(&chunk(serde_json::json!({}), Some("stop"))).unwrap();
        assert!(matches!(
            a.on_eof().unwrap().as_slice(),
            [StreamChunk::Finished { .. }]
        ));
    }

    #[test]
    fn invalid_json_is_decode_error() {
        let mut a = Assembler::new("reasoning_content");
        assert!(matches!(a.feed("{nope"), Err(ProviderError::Decode(_))));
    }

    #[test]
    fn error_object_in_stream() {
        let mut a = Assembler::new("reasoning_content");
        assert!(matches!(
            a.feed(r#"{"error":{"message":"boom"}}"#),
            Err(ProviderError::Server { .. })
        ));
    }

    #[tokio::test]
    async fn decode_stream_end_to_end() {
        let body = format!(
            "data: {}\n\ndata: {}\n\ndata: [DONE]\n\n",
            chunk(serde_json::json!({"content":"你好"}), None),
            chunk(serde_json::json!({}), Some("stop")),
        );
        let bytes = body.into_bytes();
        // 每 5 字节切一次，模拟网络分片
        let pieces: Vec<Result<Vec<u8>, ProviderError>> =
            bytes.chunks(5).map(|c| Ok(c.to_vec())).collect();
        let out: Vec<_> = decode_stream(futures::stream::iter(pieces), "reasoning_content")
            .collect()
            .await;
        assert_eq!(
            out,
            vec![
                Ok(StreamChunk::TextDelta("你好".into())),
                Ok(StreamChunk::Finished {
                    reason: FinishReason::Stop,
                    usage: None
                }),
            ]
        );
    }

    #[tokio::test]
    async fn decode_stream_propagates_transport_error() {
        let pieces: Vec<Result<Vec<u8>, ProviderError>> = vec![
            Ok(format!(
                "data: {}\n\n",
                chunk(serde_json::json!({"content":"a"}), None)
            )
            .into_bytes()),
            Err(ProviderError::Network("reset".into())),
        ];
        let out: Vec<_> = decode_stream(futures::stream::iter(pieces), "reasoning_content")
            .collect()
            .await;
        assert_eq!(
            out,
            vec![
                Ok(StreamChunk::TextDelta("a".into())),
                Err(ProviderError::Network("reset".into()))
            ]
        );
    }
}
