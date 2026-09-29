//! Ctrl+O 转录视图（设计书 §4.9）：全屏查看本会话的消息，工具调用可展开看完整结果。
//!
//! 进入时切到备用屏幕并按需捕获鼠标，退出时全部还原；行内界面在返回后重绘。

use std::{collections::HashSet, io, time::Duration};

use crossterm::{
    event::{
        DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEventKind,
        KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
    },
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen},
};
use futures::StreamExt;
use ratatui::{
    Terminal,
    backend::CrosstermBackend,
    layout::{Constraint, Layout},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Paragraph, Wrap},
};

use crate::markdown;

/// 转录里的一条记录。
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Entry {
    User(String),
    Assistant(String),
    Tool {
        name: String,
        title: String,
        ok: bool,
        summary: String,
        /// 模型看到的完整工具结果
        content: String,
    },
}

impl Entry {
    /// 折叠时的一行标题。
    pub fn label(&self) -> String {
        match self {
            Self::User(text) => format!("› {}", first_line(text)),
            Self::Assistant(text) => format!("✻ {}", first_line(text)),
            Self::Tool {
                ok,
                name,
                title,
                summary,
                ..
            } => {
                let mark = if *ok { "✓" } else { "✗" };
                let what = if title.trim().is_empty() { name } else { title };
                format!("{mark} {what} · {summary}")
            }
        }
    }

    /// 展开后追加的正文行（用户/助手消息本身就是正文）。
    pub fn body(&self) -> Option<&str> {
        match self {
            Self::User(text) | Self::Assistant(text) => Some(text),
            Self::Tool { content, .. } => Some(content),
        }
    }
}

/// 转录视图的状态：选中项、展开集合、滚动位置。
#[derive(Debug, Default)]
pub struct View {
    pub selected: usize,
    expanded: HashSet<usize>,
    /// 顶部行号（PgUp/PgDn 改它）
    pub offset: usize,
}

impl View {
    pub fn is_expanded(&self, index: usize) -> bool {
        self.expanded.contains(&index)
    }

    /// 上下移动（循环）。
    pub fn move_selection(&mut self, delta: isize, len: usize) {
        if len == 0 {
            return;
        }
        let len = len as isize;
        self.selected = (self.selected as isize + delta).rem_euclid(len) as usize;
    }

    /// 选中项跳到某条目。
    pub fn select(&mut self, index: usize, len: usize) {
        if index < len {
            self.selected = index;
        }
    }

    /// 展开/折叠某条目；返回展开后的状态。
    pub fn toggle(&mut self, index: usize) -> bool {
        if self.expanded.contains(&index) {
            self.expanded.remove(&index);
            false
        } else {
            self.expanded.insert(index);
            true
        }
    }

    /// 生成要显示的行，以及"第几行属于哪个条目"的映射（鼠标点击用）。
    pub fn rows(
        &mut self,
        entries: &[Entry],
        width: u16,
        height: usize,
    ) -> (Vec<Line<'static>>, Vec<Option<usize>>) {
        let mut lines: Vec<Line<'static>> = Vec::new();
        let mut map: Vec<Option<usize>> = Vec::new();
        for (index, entry) in entries.iter().enumerate() {
            let selected = index == self.selected;
            let marker = if selected { "▸ " } else { "  " };
            let base = if selected {
                Style::default()
                    .fg(Color::Rgb(0xE6, 0xB8, 0x5C))
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default()
            };
            lines.push(Line::from(Span::styled(
                format!("{marker}{}", entry.label()),
                base,
            )));
            map.push(Some(index));

            if self.is_expanded(index) {
                // 展开：正文按 Markdown 渲染（工具结果原样显示，模型看到的就是它）
                let body = entry.body().unwrap_or_default();
                let rendered = if body.trim().is_empty() {
                    vec![Line::from("(空)")]
                } else {
                    markdown::render_lines(body)
                };
                for line in rendered {
                    lines.push(Line::from(Span::styled(
                        format!("    {line}"),
                        Style::default().fg(Color::Rgb(0xF4, 0xE9, 0xD8)),
                    )));
                    map.push(Some(index));
                }
            }
            let _ = width;
        }

        // 让选中项始终在窗口里
        let selected_start = map
            .iter()
            .position(|entry| *entry == Some(self.selected))
            .unwrap_or(0);
        if selected_start < self.offset {
            self.offset = selected_start;
        } else if selected_start >= self.offset + height {
            self.offset = selected_start.saturating_sub(height / 2);
        }
        let top = self
            .offset
            .min(lines.len().saturating_sub(1))
            .min(lines.len().saturating_sub(height));
        self.offset = top;
        let visible: Vec<Line<'static>> = lines.iter().skip(top).take(height).cloned().collect();
        let visible_map: Vec<Option<usize>> = map.iter().skip(top).take(height).copied().collect();
        (visible, visible_map)
    }
}

fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    let clipped: String = line.chars().take(80).collect();
    if line.chars().count() > 80 {
        format!("{clipped}…")
    } else {
        clipped
    }
}

/// 在备用屏幕里显示转录，直到用户按 Esc/q。
pub async fn show(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut crate::tui::App,
    events: &mut EventStream,
    mouse: bool,
) -> io::Result<()> {
    let mut out = io::stdout();
    execute!(out, EnterAlternateScreen)?;
    if mouse {
        let _ = execute!(out, EnableMouseCapture);
    }
    let result = run(terminal, app, events).await;
    if mouse {
        let _ = execute!(out, DisableMouseCapture);
    }
    execute!(out, LeaveAlternateScreen)?;
    terminal.clear()?;
    result
}

async fn run(
    terminal: &mut Terminal<CrosstermBackend<io::Stdout>>,
    app: &mut crate::tui::App,
    events: &mut EventStream,
) -> io::Result<()> {
    let mut ticker = tokio::time::interval(Duration::from_millis(200));
    loop {
        let area = terminal.size()?;
        let height = area.height.saturating_sub(3) as usize;
        let entries = app.transcript().to_vec();
        let view = app.transcript_view_mut();
        // 每帧重建"行 → 条目"映射，鼠标点击据此命中
        let (lines, last_map) = view.rows(&entries, area.width, height);
        terminal.draw(|frame| {
            let rows = Layout::vertical([
                Constraint::Length(1),
                Constraint::Min(1),
                Constraint::Length(1),
            ])
            .split(frame.area());
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    format!(
                        " 转录（{} 条）· ↑↓/j/k 移动 · Enter 展开收起 · Esc/q 返回 ",
                        entries.len()
                    ),
                    Style::default()
                        .fg(Color::Rgb(0xE6, 0xB8, 0x5C))
                        .add_modifier(Modifier::BOLD),
                ))),
                rows[0],
            );
            let block = Block::default()
                .borders(Borders::TOP)
                .border_style(Style::default().fg(Color::DarkGray));
            frame.render_widget(
                Paragraph::new(lines)
                    .wrap(Wrap { trim: false })
                    .block(block),
                rows[1],
            );
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    " 选中项用 Enter 或鼠标点击展开；工具调用展开后是模型看到的完整结果",
                    Style::default().fg(Color::DarkGray),
                ))),
                rows[2],
            );
        })?;

        tokio::select! {
            maybe = events.next() => match maybe {
                Some(Ok(event)) => {
                    if handle_event(event, app, &last_map, height) {
                        return Ok(());
                    }
                }
                Some(Err(_)) | None => return Ok(()),
            },
            _ = ticker.tick() => {}
        }
    }
}

/// 处理一个事件；返回 true 表示退出转录视图。
fn handle_event(
    event: Event,
    app: &mut crate::tui::App,
    map: &[Option<usize>],
    height: usize,
) -> bool {
    let len = app.transcript().len();
    match event {
        Event::Key(key) => {
            if key.kind != KeyEventKind::Press {
                return false;
            }
            let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => return true,
                KeyCode::Char('c') if ctrl => return true,
                KeyCode::Up | KeyCode::Char('k') => {
                    app.transcript_view_mut().move_selection(-1, len)
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    app.transcript_view_mut().move_selection(1, len)
                }
                KeyCode::PageUp => {
                    let view = app.transcript_view_mut();
                    view.offset = view.offset.saturating_sub(height);
                }
                KeyCode::PageDown => {
                    let view = app.transcript_view_mut();
                    view.offset += height;
                }
                KeyCode::Enter => {
                    let index = app.transcript_view().selected;
                    app.transcript_view_mut().toggle(index);
                }
                _ => {}
            }
            false
        }
        Event::Mouse(mouse) => {
            handle_mouse(mouse, app, map);
            false
        }
        _ => false,
    }
}

/// 鼠标：点击某行展开/折叠，滚轮移动选中项。
fn handle_mouse(mouse: MouseEvent, app: &mut crate::tui::App, map: &[Option<usize>]) {
    match mouse.kind {
        MouseEventKind::Down(MouseButton::Left) => {
            // 减去标题行后落到内容区
            let row = mouse.row.saturating_sub(2) as usize;
            if let Some(Some(index)) = map.get(row) {
                let index = *index;
                let len = app.transcript().len();
                app.transcript_view_mut().select(index, len);
                app.transcript_view_mut().toggle(index);
            }
        }
        MouseEventKind::ScrollDown => {
            let len = app.transcript().len();
            app.transcript_view_mut().move_selection(1, len);
        }
        MouseEventKind::ScrollUp => {
            let len = app.transcript().len();
            app.transcript_view_mut().move_selection(-1, len);
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entries() -> Vec<Entry> {
        vec![
            Entry::User("看看仓库".into()),
            Entry::Assistant("好的\n我去看看".into()),
            Entry::Tool {
                name: "bash".into(),
                title: "Bash(cargo test)".into(),
                ok: true,
                summary: "测试通过".into(),
                content: "running 3 tests\nall ok".into(),
            },
        ]
    }

    #[test]
    fn labels_are_single_line_and_marked() {
        let entries = entries();
        assert!(entries[0].label().starts_with("› 看看仓库"));
        assert!(entries[1].label().starts_with("✻ 好的"), "只取首行");
        assert!(entries[2].label().contains("✓ Bash(cargo test)"));
        assert!(entries[2].label().contains("测试通过"));
    }

    #[test]
    fn selection_wraps_and_expansion_toggles() {
        let mut view = View::default();
        view.move_selection(-1, 3);
        assert_eq!(view.selected, 2, "向上应当循环到末尾");
        view.move_selection(1, 3);
        assert_eq!(view.selected, 0);
        assert!(!view.is_expanded(0));
        assert!(view.toggle(0));
        assert!(view.is_expanded(0));
        assert!(!view.toggle(0), "再点一次是折叠");
    }

    #[test]
    fn collapsed_entries_are_one_line_and_expanded_show_the_body() {
        let entries = entries();
        let mut view = View::default();
        let (lines, map) = view.rows(&entries, 80, 10);
        assert_eq!(lines.len(), 3, "折叠时每条一行：{lines:?}");
        assert_eq!(map, vec![Some(0), Some(1), Some(2)]);

        view.toggle(2);
        let (lines, map) = view.rows(&entries, 80, 20);
        assert!(lines.len() > 3, "展开后条目占多行：{lines:?}");
        assert!(
            lines.iter().any(|line| line.to_string().contains("all ok")),
            "展开要显示完整结果：{lines:?}"
        );
        assert_eq!(
            map.iter().filter(|entry| **entry == Some(2)).count(),
            lines.len() - 2
        );
    }

    #[test]
    fn mouse_mapping_points_at_the_clicked_entry() {
        let entries = entries();
        let mut view = View::default();
        let (_, map) = view.rows(&entries, 80, 10);
        assert_eq!(map.get(1), Some(&Some(1)));
        assert_eq!(map.get(2), Some(&Some(2)));
    }
}
