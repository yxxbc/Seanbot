use std::time::Duration;

use seanbot_provider::Usage;

/// 内核对 UI 暴露的事件流。
#[derive(Debug, Clone, PartialEq)]
pub enum AgentEvent {
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
