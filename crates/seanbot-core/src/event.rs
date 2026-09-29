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

/// UI → 内核的事件：用户对一次运行中的回合能做的全部动作。
///
/// 与 [`AgentEvent`] 成对，构成两个方向都走事件/通道的边界：UI 不直接调用工具、
/// 也不直接改内核状态，只把这几件事投进通道。
///
/// 取消的落点：发出 `UserEvent::Cancel` 后，真正的取消由运行时持有的
/// `CancellationToken` 完成；UI 消失或投递失败时，运行中的回合同样按取消收尾。
#[derive(Debug, Clone, PartialEq)]
pub enum UserEvent {
    /// 提交一条用户输入，开始一轮。
    Submit(String),
    /// 取消正在跑的回合。
    Cancel,
    /// 对一次权限询问的答复。
    ///
    /// 用 tool 回指被询问的那次调用，避免确认框还没点、内核已经换了工具时答错。
    PermissionDecision {
        tool: String,
        allow: bool,
        reason: Option<String>,
    },
}

impl UserEvent {
    /// 拒绝某次工具调用（可以附一句给模型看的原因）。
    pub fn deny(tool: impl Into<String>, reason: Option<String>) -> Self {
        Self::PermissionDecision {
            tool: tool.into(),
            allow: false,
            reason,
        }
    }

    /// 这一条是不是放行（提交也算放行）。
    pub fn allows(&self) -> bool {
        matches!(
            self,
            Self::PermissionDecision { allow: true, .. } | Self::Submit(_)
        )
    }

    /// 这条事件是否针对某个工具调用（把答复和询问对上）。
    pub fn matches_tool(&self, tool: &str) -> bool {
        matches!(self, Self::PermissionDecision { tool: t, .. } if t == tool)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deny_helper_is_a_denial_for_that_tool() {
        let event = UserEvent::deny("bash", Some("别动生产".into()));
        assert!(!event.allows());
        assert!(event.matches_tool("bash"));
        assert!(!event.matches_tool("edit"));
    }

    #[test]
    fn submit_is_not_tool_scoped() {
        let event = UserEvent::Submit("你好".into());
        assert!(event.allows());
        assert!(!event.matches_tool("bash"));
    }

    #[test]
    fn permission_decision_round_trips() {
        let allow = UserEvent::PermissionDecision {
            tool: "edit".into(),
            allow: true,
            reason: None,
        };
        assert!(allow.allows());
        assert!(allow.matches_tool("edit"));
        assert_ne!(allow, UserEvent::deny("edit", None));
    }
}
