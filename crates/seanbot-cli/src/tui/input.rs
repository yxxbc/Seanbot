//! 输入框（设计书 §4.6）：多行编辑交给 tui-textarea，这里只叠一层"什么时候算提交"的规则。
//!
//! - ↑↓ 在输入框内按行移动（调用方在首/末行时才拿去翻历史）
//! - ←→ / Home / End / Backspace / Delete / Ctrl+A / Ctrl+E 都由 tui-textarea 处理
//! - **bracketed paste 整段插入，绝不触发提交**（多行粘贴不会误发消息）
//!
//! 提示符由 `App::draw` 画在左侧 gutter，所以占位符只放提示文案——否则空输入时会
//! 出现两个 `› `。

use crossterm::event::{Event, KeyCode};
use ratatui::style::{Color, Style};
use tui_textarea::{CursorMove, TextArea};

/// 提示符颜色（金色，与状态栏一致）。
pub(super) const PROMPT_COLOR: Color = Color::Rgb(0xE6, 0xB8, 0x5C);
/// 空输入时显示的提示文案。
pub(super) const PLACEHOLDER: &str = "输入消息，/ 查看命令";

/// 多行输入框。
#[derive(Debug, Clone)]
pub struct Input {
    area: TextArea<'static>,
}

impl Default for Input {
    fn default() -> Self {
        // 注意：不要给 TextArea 加 padding block——tui-textarea 用"内层高度"当可渲染高度，
        // 高度只剩 1 行时 padding 会把正文整行裁掉。
        let mut area = TextArea::default();
        area.set_placeholder_text(PLACEHOLDER);
        area.set_placeholder_style(Style::default().fg(PROMPT_COLOR));
        Self { area }
    }
}

impl Input {
    /// 渲染用：交给 ratatui 的部件。
    pub(super) fn area(&self) -> &TextArea<'static> {
        &self.area
    }

    /// 光标所在（行, 显示列），用于把终端光标摆到正确位置。
    ///
    /// tui-textarea 给的是**字符下标**，直接当终端列用会让中文输入的光标偏左
    /// （一个 CJK 字占两列），所以这里按显示宽度换算。
    pub(super) fn cursor(&self) -> (u16, u16) {
        let (row, col) = self.area.cursor();
        let used: usize = self
            .area
            .lines()
            .get(row)
            .map(|line| {
                line.chars()
                    .take(col)
                    .map(|c| unicode_width::UnicodeWidthChar::width(c).unwrap_or(0))
                    .sum()
            })
            .unwrap_or(0);
        (row as u16, used as u16)
    }

    pub fn text(&self) -> String {
        self.area.lines().join("\n")
    }

    pub fn is_empty(&self) -> bool {
        self.area.is_empty()
    }

    pub fn clear(&mut self) {
        // 用 default() 而不是 TextArea::default()：占位符与配色也要跟着回来
        *self = Self::default();
    }

    /// 取走内容并把输入框清空（提交时用）。
    pub fn take(&mut self) -> String {
        let text = self.text();
        self.clear();
        text
    }

    /// 直接塞一段文本（斜杠命令补全、欢迎语等）。
    pub fn insert_str(&mut self, s: &str) {
        self.area.insert_str(s);
    }

    /// 交给 tui-textarea 处理的一次编辑输入。
    pub(super) fn apply(&mut self, event: Event) -> bool {
        match event {
            // bracketed paste：整段插入，绝不触发提交
            Event::Paste(text) => {
                self.area.insert_str(text);
            }
            Event::Key(key) => match key.code {
                KeyCode::Char(c) => self.area.insert_char(c),
                KeyCode::Backspace => {
                    self.area.delete_char();
                }
                KeyCode::Delete => {
                    self.area.delete_next_char();
                }
                KeyCode::Left => self.area.move_cursor(CursorMove::Back),
                KeyCode::Right => self.area.move_cursor(CursorMove::Forward),
                KeyCode::Home => self.area.move_cursor(CursorMove::Head),
                KeyCode::End => self.area.move_cursor(CursorMove::End),
                _ => return false,
            },
            // 还支持鼠标点击定位光标（配置开了鼠标时）
            Event::Mouse(_) => {
                self.area.input(event);
            }
            _ => return false,
        }
        true
    }

    /// 行尾（逻辑行）移动，多行输入时给 ↑↓ 用。
    pub(super) fn move_line(&mut self, delta: isize) {
        self.area.move_cursor(if delta < 0 {
            CursorMove::Up
        } else {
            CursorMove::Down
        });
    }

    /// 输入框需要几行（内容有几行就几行，最多 `max` 行）。
    pub(super) fn height(&self, max: u16) -> u16 {
        (self.area.lines().len() as u16).clamp(1, max.max(1))
    }

    /// 移到首行行首（Ctrl+A 的语义）。
    pub(super) fn move_home(&mut self) {
        self.area.move_cursor(CursorMove::Top);
        self.area.move_cursor(CursorMove::Head);
    }

    /// 移到末行行尾（Ctrl+E 的语义）。
    pub(super) fn move_end(&mut self) {
        self.area.move_cursor(CursorMove::Bottom);
        self.area.move_cursor(CursorMove::End);
    }

    /// 光标是否在首行 / 末行（决定 ↑↓ 是"在输入框内移动"还是"翻历史"）。
    pub(super) fn on_first_line(&self) -> bool {
        self.area.cursor().0 == 0
    }

    pub(super) fn on_last_line(&self) -> bool {
        self.area.cursor().0 + 1 >= self.area.lines().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyEvent, KeyModifiers};

    fn press(code: KeyCode) -> Event {
        Event::Key(KeyEvent::new(code, KeyModifiers::NONE))
    }

    fn typed(text: &str) -> Input {
        let mut input = Input::default();
        for c in text.chars() {
            input.apply(press(KeyCode::Char(c)));
        }
        input
    }

    #[test]
    fn typing_and_backspace_edit_the_buffer() {
        let mut input = typed("你好");
        assert_eq!(input.text(), "你好");
        assert!(input.apply(press(KeyCode::Backspace)));
        assert_eq!(input.text(), "你");
    }

    /// 多行粘贴（bracketed paste）整段插入，换行不会被当成提交。
    #[test]
    fn paste_inserts_every_line_at_once() {
        let mut input = Input::default();
        let pasted = "第一行\n第二行\n第三行";
        assert!(input.apply(Event::Paste(pasted.into())));
        assert_eq!(input.text(), pasted);
        assert_eq!(input.height(10), 3, "粘贴的多行要让输入框长高");
    }

    #[test]
    fn height_grows_with_the_content_and_is_capped() {
        let mut input = typed("一");
        assert_eq!(input.height(5), 1);
        input.apply(Event::Paste("a\nb\nc\nd\ne\nf".into()));
        assert_eq!(input.height(5), 5, "最多长到上限");
        assert_eq!(input.height(2), 2);
    }

    #[test]
    fn arrows_home_end_and_delete_move_the_cursor() {
        let mut input = typed("abc");
        // 只有一行时首行与末行同时成立（↑↓ 都该去翻历史）
        assert!(input.on_first_line() && input.on_last_line());
        input.apply(press(KeyCode::Home));
        input.apply(press(KeyCode::Delete));
        assert_eq!(input.text(), "bc", "Delete 删掉光标后的字符");
        input.apply(press(KeyCode::End));
        input.apply(press(KeyCode::Left));
        input.apply(press(KeyCode::Backspace));
        assert_eq!(input.text(), "c");
    }

    /// 首/末行的判断决定 ↑↓ 是"在输入框内移动"还是"翻历史"。
    #[test]
    fn line_position_decides_whether_up_down_scroll_history() {
        let mut input = typed("上\n下");
        assert!(!input.on_first_line() && input.on_last_line());
        input.move_line(-1);
        assert!(input.on_first_line() && !input.on_last_line());
        // Ctrl+A / Ctrl+E 的语义：首行行首 / 末行行尾
        input.move_end();
        assert_eq!(input.cursor(), (1, 2), "「下」占两列");
        input.move_home();
        assert_eq!(input.cursor(), (0, 0));
    }

    /// 光标列按**显示宽度**算：中文不会把光标画偏（一个 CJK 字占两列）。
    #[test]
    fn cursor_column_counts_display_width() {
        let input = typed("中文ab");
        assert_eq!(input.cursor(), (0, 6), "两个汉字四列 + 两个字母两列");
        let mut moved = typed("中文ab");
        moved.apply(press(KeyCode::Home));
        assert_eq!(moved.cursor(), (0, 0));
    }

    #[test]
    fn take_empties_the_buffer_and_clear_restores_the_placeholder() {
        let mut input = typed("发出去");
        assert_eq!(input.take(), "发出去");
        assert!(input.is_empty());
        assert_eq!(input.height(5), 1);
        // 清空之后仍然是"带占位符的默认输入框"（否则提示文案会消失）
        assert_eq!(input.area.placeholder_text(), PLACEHOLDER);
    }

    #[test]
    fn unhandled_keys_are_left_to_the_caller() {
        let mut input = Input::default();
        assert!(
            !input.apply(press(KeyCode::Up)),
            "↑↓ 由调用方决定是翻历史还是在输入框内移动"
        );
        assert!(!input.apply(press(KeyCode::Tab)));
    }
}
