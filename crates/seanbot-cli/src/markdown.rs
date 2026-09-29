//! Markdown 渲染（设计书 §6）：TUI 与 `-p` 共用同一套配色与渲染器。
//!
//! - 流式：`Streaming`（每轮一个实例）。`push` 返回**这次新冻结**的行——TUI 把它们写进
//!   终端滚动区，`tail` 给活动区域显示尚未冻结的尾部。
//! - 一次性：`render_lines`（回放历史）与 `render_ansi`（`-p` 直出）。
//! - `-p` 重定向到文件时不要用这里：调用方直接输出原始 Markdown。
//! - 渲染 panic 一律退回原始文本（`catch_unwind` 兜底），不让界面因为一段 Markdown 崩掉。

// TODO(阶段 2c)：render_lines 供 TUI 回放、render_ansi 供非流式输出，接线后删掉这个 allow
#![allow(dead_code)]

use clap::builder::styling::{AnsiColor, Color as Paint, RgbColor, Style as PaintStyle};
use std::fmt::Write as _;

use ratatui::{
    style::{Color, Modifier, Style},
    text::Line,
};
use theway_markdown::{
    MarkdownStyle, StreamingMarkdownRenderer, default_syntect, render_markdown,
    render_markdown_ratatui,
};

/// ANSI 转义起始符。
const ESC: char = '\x1b';

/// 终端太窄时表格至少要有的宽度，避免挤成一列。
const MIN_TABLE_WIDTH: usize = 20;

/// TUI 与流式渲染共用的样式，已按终端色彩能力降级。
///
/// TODO(阶段 2f)：换成品牌配色（模板/标题金 #E6B85C、行内代码奶油 #F4E9D8）。
/// 注意 @BT@MarkdownStyle@BT@ 用的是 anstyle（@BT@clap::builder::styling::Style@BT@），
/// 不是 ratatui 的 @BT@Style@BT@——改配色时要用 anstyle 的 API。
pub fn style() -> MarkdownStyle {
    brand_style().adapt()
}

/// 未按终端能力降级的品牌配色：标题金 #E6B85C、行内代码奶油 #F4E9D8、
/// 代码语言标签珊瑚 #D95F4B，链接蓝下划线、次要元素暗灰。
///
/// 注意 @BT@MarkdownStyle@BT@ 的字段是 anstyle（@BT@clap::builder::styling::Style@BT@），
/// 不是 ratatui 的 @BT@Style@BT@。
pub fn brand_style() -> MarkdownStyle {
    let gold = PaintStyle::new()
        .fg_color(Some(Paint::Rgb(RgbColor(0xE6, 0xB8, 0x5C))))
        .bold();
    let cream = PaintStyle::new().fg_color(Some(Paint::Rgb(RgbColor(0xF4, 0xE9, 0xD8))));
    let coral = PaintStyle::new().fg_color(Some(Paint::Rgb(RgbColor(0xD9, 0x5F, 0x4B))));
    let dim = PaintStyle::new().fg_color(Some(Paint::Ansi(AnsiColor::BrightBlack)));
    MarkdownStyle {
        heading_inner: [gold; 6],
        inline_code_inner: cream,
        code_language: coral,
        link_text: PaintStyle::new()
            .fg_color(Some(Paint::Ansi(AnsiColor::Blue)))
            .underline(),
        link_url: dim,
        rule: dim,
        blockquote_outer: dim,
        ..MarkdownStyle::default()
    }
}

/// 一轮响应的流式渲染器。
pub struct Streaming {
    inner: StreamingMarkdownRenderer,
    /// 已经交出去（写进滚动区）的行数
    frozen: usize,
}

// SAFETY: 内部的 syntect/onig 数据带裸指针，所以标准库不敢自动判定 Send。
// 实际上这个渲染器**只由持有它的线程使用**（CLI 的渲染循环，以后的 TUI 主线程），
// 从不跨线程共享；它引用的 default_syntect() 是 &'static 只读数据。
// 手动标记 Send 只是为了满足 Renderer<W>: Send 的约束。
unsafe impl Send for Streaming {}

impl Streaming {
    /// `width` 为终端宽度，用于限制表格宽度。
    pub fn new(width: usize) -> Self {
        let mut inner = StreamingMarkdownRenderer::new(style(), true);
        inner.set_max_table_width(Some(width.max(MIN_TABLE_WIDTH)));
        Self { inner, frozen: 0 }
    }

    /// 喂一段增量，返回这次新冻结的行。
    pub fn push(&mut self, chunk: &str) -> Vec<Line<'static>> {
        self.inner.push_and_render(chunk, Some(default_syntect()));
        self.take_frozen()
    }

    /// 本轮结束：冻结剩余内容并返回。
    pub fn finish(&mut self) -> Vec<Line<'static>> {
        self.inner.finish(Some(default_syntect()));
        self.take_frozen()
    }

    /// 还没冻结的尾部（活动区域显示用）。
    pub fn tail(&self) -> Vec<Line<'static>> {
        self.inner
            .view()
            .lines
            .get(self.frozen..)
            .unwrap_or_default()
            .to_vec()
    }

    fn take_frozen(&mut self) -> Vec<Line<'static>> {
        let view = self.inner.view();
        let frozen = self.inner.frozen_lines_count().min(view.lines.len());
        let out = view
            .lines
            .get(self.frozen..frozen)
            .unwrap_or_default()
            .to_vec();
        self.frozen = frozen;
        out
    }
}

/// 一次性渲染成 ratatui 行（回放历史、非流式场景）。
pub fn render_lines(text: &str) -> Vec<Line<'static>> {
    let owned = text.to_string();
    let input = owned.clone();
    let rendered = std::panic::catch_unwind(move || {
        render_markdown_ratatui(&input, style(), true, Some(default_syntect())).0
    });
    match rendered {
        Ok(lines) if !lines.is_empty() => lines,
        // 渲染失败或没渲染出行：退回原始文本，保证用户一定看得到内容
        _ => owned
            .lines()
            .map(|line| Line::from(line.to_string()))
            .collect(),
    }
}

/// 一次性渲染成 ANSI 字符串（`-p` 在终端里直出）。
pub fn render_ansi(text: &str) -> String {
    let owned = text.to_string();
    let input = owned.clone();
    let rendered = std::panic::catch_unwind(move || {
        render_markdown(&input, style(), true, Some(default_syntect())).0
    });
    match rendered {
        Ok(ansi) if !ansi.trim().is_empty() => ansi,
        _ => owned,
    }
}

/// 把 ratatui 行转成 ANSI（`-p` 流式输出时逐段用）。
pub fn lines_to_ansi(lines: &[Line<'static>]) -> String {
    let mut out = String::new();
    for line in lines {
        for span in &line.spans {
            let codes = ansi_codes(&span.style);
            if codes.is_empty() {
                out.push_str(&span.content);
            } else {
                let _ = write!(out, "{ESC}[{codes}m{}{ESC}[0m", span.content);
            }
        }
        out.push('\n');
    }
    out
}

/// ratatui 样式 → SGR 参数，只覆盖我们用得到的属性。
fn ansi_codes(style: &Style) -> String {
    let mut codes: Vec<String> = Vec::new();
    if let Some(color) = style.fg {
        codes.push(color_code(color, false));
    }
    if let Some(color) = style.bg {
        codes.push(color_code(color, true));
    }
    let modifiers = style.add_modifier;
    for (flag, code) in [
        (Modifier::BOLD, "1"),
        (Modifier::DIM, "2"),
        (Modifier::ITALIC, "3"),
        (Modifier::UNDERLINED, "4"),
        (Modifier::REVERSED, "7"),
        (Modifier::CROSSED_OUT, "9"),
    ] {
        if modifiers.contains(flag) {
            codes.push(code.to_string());
        }
    }
    codes.join(";")
}

fn color_code(color: Color, background: bool) -> String {
    let (bright, faint) = if background { (100, 40) } else { (90, 30) };
    match color {
        Color::Reset => {
            if background {
                "49".to_string()
            } else {
                "39".to_string()
            }
        }
        Color::Black => faint.to_string(),
        Color::Red => (faint + 1).to_string(),
        Color::Green => (faint + 2).to_string(),
        Color::Yellow => (faint + 3).to_string(),
        Color::Blue => (faint + 4).to_string(),
        Color::Magenta => (faint + 5).to_string(),
        Color::Cyan => (faint + 6).to_string(),
        Color::Gray => (faint + 7).to_string(),
        Color::DarkGray => bright.to_string(),
        Color::LightRed => (bright + 1).to_string(),
        Color::LightGreen => (bright + 2).to_string(),
        Color::LightYellow => (bright + 3).to_string(),
        Color::LightBlue => (bright + 4).to_string(),
        Color::LightMagenta => (bright + 5).to_string(),
        Color::LightCyan => (bright + 6).to_string(),
        Color::White => (bright + 7).to_string(),
        Color::Rgb(r, g, b) => format!("{};2;{r};{g};{b}", if background { 48 } else { 38 }),
        Color::Indexed(index) => format!("{};5;{index}", if background { 48 } else { 38 }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::text::Span;

    fn text_of(lines: &[Line<'static>]) -> String {
        lines
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn brand_palette_uses_the_project_colors() {
        let style = brand_style();
        assert_eq!(
            style.heading_inner[0].get_fg_color(),
            Some(Paint::Rgb(RgbColor(0xE6, 0xB8, 0x5C))),
            "标题应当是金色"
        );
        assert_eq!(
            style.inline_code_inner.get_fg_color(),
            Some(Paint::Rgb(RgbColor(0xF4, 0xE9, 0xD8))),
            "行内代码应当是奶油色"
        );
        // adapt() 会按终端能力降级，但不该 panic
        let _ = style.adapt();
    }

    #[test]
    fn renders_headings_lists_and_code() {
        let lines = render_lines(
            "# 标题\n\n- 一\n- 二\n\n**粗体** 与 \u{0060}代码\u{0060}\n\n\u{0060}\u{0060}\u{0060}rust\nfn main() {}\n\u{0060}\u{0060}\u{0060}",
        );
        let text = text_of(&lines);
        assert!(text.contains("标题"), "{text}");
        assert!(text.contains("一"), "{text}");
        assert!(text.contains("粗体"), "{text}");
        assert!(text.contains("fn main"), "{text}");
        // 标记是否隐藏取决于终端色彩能力（测试里是非 TTY），这里只验证确实渲染过
        assert!(text.contains('•'), "列表应当被渲染：{text}");
    }

    #[test]
    fn streaming_freezes_progressively_without_repeating() {
        let mut streaming = Streaming::new(80);
        let first = streaming.push("# 标题\n");
        let second = streaming.push("正文一段\n\n- 一\n");
        let rest = streaming.finish();
        let combined = format!("{}{}{}", text_of(&first), text_of(&second), text_of(&rest));
        assert_eq!(combined.matches("标题").count(), 1, "{combined}");
        assert!(combined.contains("正文一段"), "{combined}");
    }

    #[test]
    fn empty_input_is_harmless() {
        assert!(render_lines("").is_empty());
        assert!(render_ansi("").is_empty());
        assert!(lines_to_ansi(&[]).is_empty());
    }

    #[test]
    fn ansi_rendering_keeps_text() {
        // 非 TTY 下 adapt() 会降级成纯文本，这里只保证内容不丢
        let rendered = render_ansi("# 标题\n\n正文");
        assert!(rendered.contains("标题"), "{rendered}");
        assert!(rendered.contains("正文"), "{rendered}");
        // 转义序列只由 lines_to_ansi 保证（见下一个测试）：库的渲染结果在非 TTY 下不带颜色
    }

    #[test]
    fn lines_to_ansi_maps_colors_and_modifiers() {
        let lines = vec![Line::from(vec![
            Span::styled("粗", Style::default().add_modifier(Modifier::BOLD)),
            Span::styled("红", Style::default().fg(Color::Rgb(217, 95, 75))),
            Span::raw("普通"),
        ])];
        let ansi = lines_to_ansi(&lines);
        assert!(ansi.contains("1m粗"), "{ansi:?}");
        assert!(ansi.contains("38;2;217;95;75m红"), "{ansi:?}");
        assert!(ansi.ends_with("普通\n"), "{ansi:?}");
    }
}
