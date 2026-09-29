//! 浮窗、二级列表与确认框（设计书 §4.7 / §4.8）：只放**状态**与**纯渲染行**，
//! 按键逻辑留在 `App`（它才知道优先级：确认框 > 二级列表 > 斜杠浮窗）。
//!
//! 三者的共同点：都是"盖在活动区上的一小块"，内容由 `AppState` 投影出来，
//! 谁都不碰内核——确认框只发 `UserEvent::PermissionDecision`。

use std::sync::{Arc, Mutex};

use ratatui::{
    style::{Color, Modifier, Style},
    text::{Line, Span},
};

use super::permission::{AskView, Rules};
use super::slash;
use crate::tui::Input;

/// 确认框状态（设计书 §4.8）。
pub(super) struct ConfirmState {
    /// 待确认的请求（不含回传通道：答复走 `UserEvent::PermissionDecision`）；
    /// 拿走它就等于关闭确认框
    pub(super) ask: Option<AskView>,
    pub(super) selected: usize,
    /// 正在输入拒绝原因（None 表示还在选选项）
    pub(super) reason: Option<Input>,
    pub(super) rules: Arc<Mutex<Rules>>,
}

impl ConfirmState {
    /// 可用选项；改动工作目录之外的 edit 不提供"本会话不再询问"。
    pub(super) fn options(&self) -> Vec<&'static str> {
        let mut options = vec!["允许"];
        if self.ask.as_ref().is_some_and(|ask| ask.rememberable) {
            options.push("允许，本会话不再询问");
        }
        options.push("拒绝，并告诉环环原因");
        options
    }

    /// 确认框需要几行：预览 + 空行 + 选项，再加上下边框。
    pub(super) fn height(&self) -> u16 {
        let preview = self.ask.as_ref().map(|ask| ask.preview.len()).unwrap_or(0) as u16;
        preview + self.options().len() as u16 + 4
    }
}

/// 二级列表选出来的东西要干什么。
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PickKind {
    Model,
    Session,
}

/// 二级列表里的一个候选项。
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct PickerItem {
    pub(super) label: String,
    pub(super) detail: String,
    /// 选中后交给调用方的值（模型 id 或会话文件路径）
    pub(super) value: String,
}

/// 二级列表状态（/model、/resume）。
#[derive(Debug)]
pub(super) struct Picker {
    pub(super) title: String,
    pub(super) kind: PickKind,
    pub(super) items: Vec<PickerItem>,
    pub(super) selected: usize,
}

/// 斜杠命令浮窗状态。
#[derive(Debug, Default)]
pub(super) struct Popup {
    pub(super) matches: Vec<slash::Match>,
    pub(super) selected: usize,
}

/// 二级列表的一行：选中标记 + 标签 + 次要说明。
pub(super) fn picker_line(item: &PickerItem, selected: bool) -> Line<'static> {
    let marker = if selected { "▸ " } else { "  " };
    Line::from(vec![
        Span::styled(marker, Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C))),
        Span::raw(item.label.clone()),
        Span::styled(
            format!("  {}", item.detail),
            Style::default().fg(Color::DarkGray),
        ),
    ])
}

/// 浮窗的一行：命中字符高亮 + 说明。
pub(super) fn popup_line(item: &slash::Match, selected: bool) -> Line<'static> {
    let command = slash::COMMANDS[item.index];
    let marker = if selected { "▸ " } else { "  " };
    let mut spans = vec![Span::styled(
        marker,
        Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C)),
    )];
    for (text, hit) in slash::highlight(command.name, &item.positions) {
        let style = if hit {
            Style::default()
                .fg(Color::Rgb(0xE6, 0xB8, 0x5C))
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };
        spans.push(Span::styled(text, style));
    }
    spans.push(Span::raw(format!("  {}", command.description)));
    Line::from(spans)
}

#[cfg(test)]
mod tests {
    use super::*;
    use seanbot_core::{PermissionRequest, Risk};

    fn ask(rememberable: bool) -> AskView {
        AskView {
            tool: "bash".into(),
            request: PermissionRequest {
                tool: "bash".into(),
                title: "Bash 想要执行".into(),
                risk: Risk::Mutating,
                args: serde_json::json!({"command": "cargo test"}),
            },
            rememberable,
            preview: vec!["$ cargo test".into()],
        }
    }

    fn confirm(rememberable: bool) -> ConfirmState {
        ConfirmState {
            ask: Some(ask(rememberable)),
            selected: 0,
            reason: None,
            rules: Arc::new(Mutex::new(Rules::default())),
        }
    }

    /// 工作目录之外的改动不给"不再询问"，选项就只剩两个。
    #[test]
    fn remember_option_depends_on_the_request() {
        assert_eq!(confirm(true).options().len(), 3);
        let outside = confirm(false).options();
        assert_eq!(outside.len(), 2, "{outside:?}");
        assert!(outside[1].contains("拒绝"));
    }

    /// 确认框高度要跟着预览行数与选项数走（它盖在活动区上，不能超出）。
    #[test]
    fn height_follows_preview_and_options() {
        let mut state = confirm(true);
        assert_eq!(
            state.height(),
            1 + 3 + 4,
            "1 行预览 + 3 个选项 + 边框与空行"
        );
        state.ask.as_mut().unwrap().preview.push("第二行".into());
        assert_eq!(state.height(), 2 + 3 + 4);
    }

    #[test]
    fn picker_line_marks_the_selection() {
        let item = PickerItem {
            label: "deepseek-chat".into(),
            detail: "上下文 64k".into(),
            value: "deepseek-chat".into(),
        };
        let selected = picker_line(&item, true).to_string();
        assert!(selected.starts_with("▸ deepseek-chat"), "{selected:?}");
        assert!(selected.contains("上下文 64k"), "{selected:?}");
        assert!(picker_line(&item, false).to_string().starts_with("  "));
    }

    #[test]
    fn popup_line_highlights_the_matched_characters() {
        let matched = slash::filter("/mo").into_iter().next().expect("有匹配项");
        let line = popup_line(&matched, true);
        let text = line.to_string();
        assert!(text.contains("/model"), "{text:?}");
        assert!(text.starts_with("▸ "), "{text:?}");
        assert!(
            line.spans
                .iter()
                .any(|span| span.style.add_modifier.contains(Modifier::BOLD)),
            "命中的字符要高亮：{text:?}"
        );
    }
}
