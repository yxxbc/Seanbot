use std::time::Duration;

use seanbot_provider::{Message, Usage};

/// 内核对 UI 暴露的事件流。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
    /// 内核向历史追加了一条消息（UI 据此持久化会话）。
    MessageAppended(Message),
    /// 内核撤回了历史中的最后一条消息（本轮首步取消/失败时未被回应的用户消息）。
    MessageRetracted,
    ThinkingStarted,
    ReasoningDelta(String),
    TextDelta(String),
    ToolStarted {
        call_id: String,
        name: String,
        title: String,
    },
    ToolFinished {
        call_id: String,
        ok: bool,
        summary: String,
        preview: Vec<String>,
        elapsed: Duration,
    },
    TurnFinished {
        usage: Option<Usage>,
        steps: u32,
    },
    Cancelled,
    Error(String),
}

#[derive(Debug, Clone, PartialEq)]
pub struct TurnSummary {
    pub usage: Option<Usage>,
    pub steps: u32,
    pub cancelled: bool,
}
