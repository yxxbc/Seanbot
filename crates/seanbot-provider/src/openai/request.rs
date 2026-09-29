//! ChatRequest → OpenAI 兼容的 JSON 请求体。
//! 序列化结果必须对相同输入逐字节一致（前缀缓存依赖于此）。

use serde_json::{Map, Value, json};

use crate::{ChatRequest, Role, descriptor::Quirks};

pub(crate) fn build_body(req: &ChatRequest, quirks: &Quirks) -> Value {
    let mut messages = Vec::with_capacity(req.messages.len() + 1);
    messages.push(json!({ "role": "system", "content": req.system }));
    for m in &req.messages {
        let msg = match m.role {
            Role::User => json!({ "role": "user", "content": m.content }),
            Role::Tool => json!({
                "role": "tool",
                "tool_call_id": m.tool_call_id.as_deref().unwrap_or_default(),
                "content": m.content,
            }),
            Role::Assistant => {
                let mut obj = Map::new();
                obj.insert("role".into(), json!("assistant"));
                let content = if m.content.is_empty() && !m.tool_calls.is_empty() {
                    Value::Null
                } else {
                    json!(m.content)
                };
                obj.insert("content".into(), content);
                if quirks.echo_reasoning {
                    if let Some(r) = &m.reasoning {
                        obj.insert(quirks.reasoning_field.into(), json!(r));
                    }
                }
                if !m.tool_calls.is_empty() {
                    let calls: Vec<Value> = m
                        .tool_calls
                        .iter()
                        .map(|c| {
                            json!({
                                "id": c.id,
                                "type": "function",
                                "function": { "name": c.name, "arguments": c.arguments },
                            })
                        })
                        .collect();
                    obj.insert("tool_calls".into(), Value::Array(calls));
                }
                Value::Object(obj)
            }
        };
        messages.push(msg);
    }

    let mut body = json!({
        "model": req.model,
        "messages": messages,
        "stream": true,
        "stream_options": { "include_usage": true },
    });
    if !req.tools.is_empty() {
        let tools: Vec<Value> = req
            .tools
            .iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t.name,
                        "description": t.description,
                        "parameters": t.parameters,
                    },
                })
            })
            .collect();
        body["tools"] = Value::Array(tools);
    }
    if let Some(max) = req.max_tokens {
        body["max_tokens"] = json!(max);
    }
    body
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Message, ToolCall, ToolSpec, descriptor::DEEPSEEK};

    fn sample() -> ChatRequest {
        ChatRequest {
            model: "deepseek-flash".into(),
            system: "你是助手".into(),
            messages: vec![
                Message::user("读一下 a.txt"),
                Message::assistant(
                    "",
                    Some("需要读文件".into()),
                    vec![ToolCall {
                        id: "c1".into(),
                        name: "read".into(),
                        arguments: r#"{"path":"a.txt"}"#.into(),
                    }],
                ),
                Message::tool("c1", "1\thello"),
                Message::assistant("内容是 hello", Some("读完了".into()), vec![]),
            ],
            tools: vec![
                ToolSpec {
                    name: "bash".into(),
                    description: "运行命令".into(),
                    parameters: json!({"type":"object"}),
                },
                ToolSpec {
                    name: "read".into(),
                    description: "读文件".into(),
                    parameters: json!({"type":"object"}),
                },
            ],
            max_tokens: None,
        }
    }

    #[test]
    fn builds_openai_body() {
        let body = build_body(&sample(), &DEEPSEEK.quirks);
        assert_eq!(body["model"], "deepseek-flash");
        assert_eq!(body["stream"], true);
        assert_eq!(body["stream_options"]["include_usage"], true);
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 5);
        assert_eq!(msgs[0], json!({"role":"system","content":"你是助手"}));
        assert_eq!(msgs[1], json!({"role":"user","content":"读一下 a.txt"}));
        assert_eq!(
            msgs[2],
            json!({
                "role":"assistant",
                "content": null,
                "reasoning_content":"需要读文件",
                "tool_calls":[{"id":"c1","type":"function","function":{"name":"read","arguments":"{\"path\":\"a.txt\"}"}}]
            })
        );
        assert_eq!(
            msgs[3],
            json!({"role":"tool","tool_call_id":"c1","content":"1\thello"})
        );
        assert_eq!(
            msgs[4],
            json!({"role":"assistant","content":"内容是 hello","reasoning_content":"读完了"})
        );
        assert!(body.get("max_tokens").is_none());
    }

    #[test]
    fn tools_keep_given_order() {
        let body = build_body(&sample(), &DEEPSEEK.quirks);
        let names: Vec<_> = body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["function"]["name"].as_str().unwrap())
            .collect();
        assert_eq!(names, ["bash", "read"]);
        assert_eq!(body["tools"][0]["type"], "function");
        assert_eq!(body["tools"][0]["function"]["description"], "运行命令");
        assert_eq!(
            body["tools"][0]["function"]["parameters"],
            json!({"type":"object"})
        );
    }

    #[test]
    fn omits_tools_when_empty_and_sets_max_tokens() {
        let mut req = sample();
        req.tools.clear();
        req.max_tokens = Some(100);
        let body = build_body(&req, &DEEPSEEK.quirks);
        assert!(body.get("tools").is_none());
        assert_eq!(body["max_tokens"], 100);
    }

    #[test]
    fn no_reasoning_echo_when_quirk_disabled() {
        let quirks = Quirks {
            reasoning_field: "reasoning_content",
            echo_reasoning: false,
        };
        let body = build_body(&sample(), &quirks);
        assert!(body["messages"][4].get("reasoning_content").is_none());
    }

    #[test]
    fn serialization_is_deterministic() {
        let a = build_body(&sample(), &DEEPSEEK.quirks).to_string();
        let b = build_body(&sample(), &DEEPSEEK.quirks).to_string();
        assert_eq!(a, b);
    }
}
