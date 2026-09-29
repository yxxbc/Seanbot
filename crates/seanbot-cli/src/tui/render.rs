//! 绘制层（设计书 §4.4）：把 `AppState` 投影成一帧画面——活动区、输入行、状态栏、浮窗。
//!
//! 这里只读状态、只画像素：真正的冻结（把完成的行写进终端 scrollback）在主循环里，
//! 见 `mod.rs` 的 `freeze()`。所有内容都在 `App` 上，所以绘制是"状态的一个投影"。

use ratatui::{
    Frame,
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
};

use super::{App, input, popup};
use crate::format;

/// 提示符宽度（`\u{203a} ` 占两列）。
pub(super) const PROMPT_WIDTH: u16 = 2;

impl App {
    pub(super) fn status_line(&self) -> Line<'static> {
        // 与 TUI 之前完全一致：跑工具时是「⠋ 标签 用时」，空闲/思考时才是环环的迷你转圈
        let left = match &self.running_tool {
            Some((label, since)) => format!(
                "{} {label} {}",
                crate::render::FRAMES[self.frame % crate::render::FRAMES.len()],
                format::secs(since.elapsed())
            ),
            None if self.running => "思考中…".to_string(),
            None => String::new(),
        };
        let hint = self.hint.clone().unwrap_or_default();
        let mut spans: Vec<Span<'static>> = Vec::new();
        if self.running_tool.is_none() {
            spans.push(self.mascot.mini());
        }
        spans.push(Span::styled(
            left,
            Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C)),
        ));
        spans.push(Span::raw(format!("{} · {hint}", self.left)));
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            self.mode.clone(),
            Style::default().fg(Color::Rgb(0xD9, 0x5F, 0x4B)),
        ));
        Line::from(spans)
    }

    pub(super) fn draw(&mut self, frame: &mut Frame) {
        let area = frame.area();
        // 这一帧真正用于渲染的宽度：折行（`viewport_lines`）与冻结补位都用它，
        // 否则"账本宽度"和"屏幕宽度"会不一致，行数就算错了。
        self.width = area.width.max(1);
        // 输入框行数是动态的（多行输入会长高），状态栏永远占最后一行
        let status_row = area.bottom().saturating_sub(1).max(area.y);
        let input_max = status_row.saturating_sub(area.y);

        // 输入框高度：内容有几行就几行（上限见 input_max）
        let input_height = self.input.height(input_max.max(1));
        let input = Rect {
            x: area.x,
            y: status_row.saturating_sub(input_height).max(area.y),
            width: area.width,
            height: input_height.min(input_max.max(1)),
        };
        let active = Rect {
            y: area.y,
            height: input.y.saturating_sub(area.y),
            ..area
        };
        // 冻结策略要用"这一帧真正能画多少行"，不能用屏幕高度减去固定 chrome：
        // 输入框多行时 chrome 会变高，否则活动区顶部的行会既不冻结也不绘制。
        self.active_height = active.height.max(1);

        // 活动区只画"还没冻结"的部分（冻结的已经在终端 scrollback 里了）。
        // 行内视口的高度随内容收缩，窗口下方多出来的屏幕行必须清掉，
        // 否则上一帧的旧内容会留在屏幕上、看起来像历史重复。
        // 注意：行已经在 `viewport_lines` 里折成屏幕行并补位到整宽，这里**不能再折行**
        // （否则一条逻辑行占两个屏幕行，窗口把最后一行挤出屏幕）。
        let window = self.viewport_window(active.height as usize);
        let used = window.len() as u16;
        if used > 0 {
            frame.render_widget(
                Paragraph::new(window),
                Rect {
                    height: used.min(active.height),
                    ..active
                },
            );
        }
        let spare = active.height.saturating_sub(used);
        if spare > 0 {
            frame.render_widget(
                Clear,
                Rect {
                    y: active.y + used.min(active.height),
                    height: spare,
                    ..active
                },
            );
        }

        // 输入框：提示符单独占左侧两列（多行文本不会被提示符挤错位），
        // 正文交给 tui-textarea 渲染（行数超出时它会自己滚动）
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "› ",
                    Style::default().fg(input::PROMPT_COLOR)
                ));
                input.height.saturating_sub(1) as usize
            ]),
            Rect {
                width: PROMPT_WIDTH.min(input.width),
                ..input
            },
        );
        frame.render_widget(
            self.input.area(),
            Rect {
                x: input.x + PROMPT_WIDTH.min(input.width),
                width: input.width.saturating_sub(PROMPT_WIDTH),
                ..input
            },
        );
        // 提示符画在 gutter 上（输入框有内容时 placeholder 不会再显示）
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    "› ",
                    Style::default().fg(input::PROMPT_COLOR)
                ));
                input.height.max(1) as usize
            ]),
            Rect {
                width: PROMPT_WIDTH.min(input.width),
                ..input
            },
        );
        frame.render_widget(
            Paragraph::new(self.status_line()),
            Rect {
                y: status_row,
                height: 1,
                ..area
            },
        );

        // 确认框：金色边框，盖在活动区底部
        if let Some(confirm) = &self.confirm {
            let height = confirm.height().min(active.height);
            if height >= 3 {
                let area = Rect {
                    x: active.x,
                    y: active.bottom().saturating_sub(height),
                    width: active.width,
                    height,
                };
                let mut lines: Vec<Line<'static>> = confirm
                    .ask
                    .as_ref()
                    .map(|ask| ask.preview.clone())
                    .unwrap_or_default()
                    .into_iter()
                    .map(|text| Line::from(Span::raw(text)))
                    .collect();
                lines.push(Line::from(""));
                match &confirm.reason {
                    Some(reason) => lines.push(Line::from(vec![
                        Span::raw("拒绝原因："),
                        Span::styled(
                            reason.text().to_string(),
                            Style::default().fg(Color::Rgb(0xF4, 0xE9, 0xD8)),
                        ),
                    ])),
                    None => {
                        for (index, option) in confirm.options().iter().enumerate() {
                            let selected = index == confirm.selected;
                            let marker = if selected { "▸ " } else { "  " };
                            let style = if selected {
                                Style::default()
                                    .fg(Color::Rgb(0xE6, 0xB8, 0x5C))
                                    .add_modifier(Modifier::BOLD)
                            } else {
                                Style::default()
                            };
                            lines.push(Line::from(Span::styled(
                                format!("{marker}{}. {option}", index + 1),
                                style,
                            )));
                        }
                    }
                }
                let gold = Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C));
                let block = Block::default()
                    .borders(Borders::ALL)
                    .border_style(gold)
                    .title(" 需要确认 ");
                let inner = block.inner(area);
                frame.render_widget(Clear, area);
                frame.render_widget(block, area);
                frame.render_widget(Paragraph::new(lines), inner);
            }
        }

        // 二级列表同样贴在输入行上方，第一行是标题
        if let Some(picker) = &self.picker {
            let height = (picker.items.len() as u16 + 1).min(active.height);
            if height > 1 {
                let area = Rect {
                    x: active.x,
                    y: active.bottom().saturating_sub(height),
                    width: active.width,
                    height,
                };
                let mut lines: Vec<Line<'static>> = vec![Line::from(Span::styled(
                    format!(" {} （↑↓ 选择 · Enter 确认 · Esc 取消）", picker.title),
                    Style::default().fg(Color::Rgb(0xE6, 0xB8, 0x5C)),
                ))];
                for (index, item) in picker.items.iter().enumerate() {
                    lines.push(popup::picker_line(item, index == picker.selected));
                }
                frame.render_widget(Clear, area);
                frame.render_widget(Paragraph::new(lines), area);
            }
        }

        // 浮窗贴在输入行上方
        if let Some(popup) = &self.popup {
            let height = (popup.matches.len() as u16).min(active.height);
            if height > 0 {
                let area = Rect {
                    x: active.x,
                    y: active.bottom().saturating_sub(height),
                    width: active.width,
                    height,
                };
                let lines: Vec<Line<'static>> = popup
                    .matches
                    .iter()
                    .enumerate()
                    .map(|(index, item)| popup::popup_line(item, index == popup.selected))
                    .collect();
                frame.render_widget(Clear, area);
                frame.render_widget(Paragraph::new(lines), area);
            }
        }

        // 光标：输入框内的 (行, 列) + 提示符宽度
        let (crow, ccol) = self.input.cursor();
        let x = input.x + PROMPT_WIDTH.min(input.width) + ccol;
        let y = input.y + crow;
        frame.set_cursor_position((
            x.min(input.right().saturating_sub(1)),
            y.min(input.bottom().saturating_sub(1)),
        ));
    }
}

/// 状态栏右侧的模式文案。
pub(super) fn mode_label(mode: seanbot_core::PermissionMode) -> String {
    match mode {
        seanbot_core::PermissionMode::Yolo => "⚡ YOLO".to_string(),
        seanbot_core::PermissionMode::Confirm => "确认模式".to_string(),
    }
}
