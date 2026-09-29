//! 与厂商无关的统一类型。

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    User,
    Assistant,
    Tool,
}

/// 一次完整的工具调用。`arguments` 保持模型给出的原始 JSON 文本，由内核解析。
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Message {
    pub role: Role,
    #[serde(default)]
    pub content: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tool_calls: Vec<ToolCall>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tool_call_id: Option<String>,
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            role: Role::User,
            content: content.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: None,
        }
    }

    pub fn assistant(
        content: impl Into<String>,
        reasoning: Option<String>,
        tool_calls: Vec<ToolCall>,
    ) -> Self {
        Self {
            role: Role::Assistant,
            content: content.into(),
            reasoning,
            tool_calls,
            tool_call_id: None,
        }
    }

    pub fn tool(call_id: impl Into<String>, content: impl Into<String>) -> Self {
        Self {
            role: Role::Tool,
            content: content.into(),
            reasoning: None,
            tool_calls: Vec::new(),
            tool_call_id: Some(call_id.into()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ToolSpec {
    pub name: String,
    pub description: String,
    /// JSON Schema
    pub parameters: Value,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ChatRequest {
    pub model: String,
    pub system: String,
    pub messages: Vec<Message>,
    pub tools: Vec<ToolSpec>,
    pub max_tokens: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Usage {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cache_hit_tokens: Option<u64>,
    pub cache_miss_tokens: Option<u64>,
}

impl Usage {
    /// 累加另一次调用的用量（一轮多步时汇总）。
    pub fn add(&mut self, other: &Usage) {
        self.input_tokens += other.input_tokens;
        self.output_tokens += other.output_tokens;
        self.cache_hit_tokens = add_opt(self.cache_hit_tokens, other.cache_hit_tokens);
        self.cache_miss_tokens = add_opt(self.cache_miss_tokens, other.cache_miss_tokens);
    }
}

fn add_opt(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        (a, b) => Some(a.unwrap_or(0) + b.unwrap_or(0)),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FinishReason {
    Stop,
    Length,
    ToolCalls,
    ContentFilter,
    Other(String),
}

impl FinishReason {
    pub fn parse(s: &str) -> Self {
        match s {
            "stop" => Self::Stop,
            "length" => Self::Length,
            "tool_calls" => Self::ToolCalls,
            "content_filter" => Self::ContentFilter,
            other => Self::Other(other.to_string()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum StreamChunk {
    ReasoningDelta(String),
    TextDelta(String),
    ToolCall(ToolCall),
    Finished {
        reason: FinishReason,
        usage: Option<Usage>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelInfo {
    pub id: String,
    pub context_window: u64,
    pub supports_tools: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderInfo {
    pub id: String,
    pub display_name: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usage_add_sums_all_fields() {
        let mut total = Usage::default();
        total.add(&Usage {
            input_tokens: 10,
            output_tokens: 1,
            cache_hit_tokens: Some(8),
            cache_miss_tokens: Some(2),
        });
        total.add(&Usage {
            input_tokens: 20,
            output_tokens: 2,
            cache_hit_tokens: Some(18),
            cache_miss_tokens: None,
        });
        assert_eq!(
            total,
            Usage {
                input_tokens: 30,
                output_tokens: 3,
                cache_hit_tokens: Some(26),
                cache_miss_tokens: Some(2)
            }
        );
    }

    #[test]
    fn usage_add_keeps_none_when_both_missing() {
        let mut total = Usage::default();
        total.add(&Usage {
            input_tokens: 1,
            output_tokens: 1,
            cache_hit_tokens: None,
            cache_miss_tokens: None,
        });
        assert_eq!(total.cache_hit_tokens, None);
    }

    #[test]
    fn finish_reason_parse() {
        assert_eq!(FinishReason::parse("tool_calls"), FinishReason::ToolCalls);
        assert_eq!(FinishReason::parse("stop"), FinishReason::Stop);
        assert_eq!(
            FinishReason::parse("insufficient_system_resource"),
            FinishReason::Other("insufficient_system_resource".into())
        );
    }

    #[test]
    fn message_constructors() {
        let t = Message::tool("call_1", "ok");
        assert_eq!(t.role, Role::Tool);
        assert_eq!(t.tool_call_id.as_deref(), Some("call_1"));
        let a = Message::assistant("hi", Some("think".into()), vec![]);
        assert_eq!(a.reasoning.as_deref(), Some("think"));
    }
    #[test]
    fn message_serde_roundtrip() {
        let m = Message::assistant(
            "好的\n第二行 \"引号\" 🎉",
            Some("想一想".into()),
            vec![ToolCall {
                id: "c1".into(),
                name: "read".into(),
                arguments: r#"{"path":"a"}"#.into(),
            }],
        );
        let json = serde_json::to_string(&m).unwrap();
        assert_eq!(serde_json::from_str::<Message>(&json).unwrap(), m);
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(v["role"], "assistant");
        assert!(v.get("tool_call_id").is_none());

        assert_eq!(
            serde_json::to_value(Message::user("hi")).unwrap(),
            serde_json::json!({"role": "user", "content": "hi"})
        );
        assert_eq!(
            serde_json::to_value(Message::tool("c1", "ok")).unwrap(),
            serde_json::json!({"role": "tool", "content": "ok", "tool_call_id": "c1"})
        );
        let back: Message =
            serde_json::from_value(serde_json::json!({"role": "user", "content": "x"})).unwrap();
        assert_eq!(back, Message::user("x"));
    }
}
